// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex
// OpenAI-Codex_Responsible: Source_Code
// OpenAI-Codex_index_review
// OpenAI-Codex_lspdfr_role_core

//! The install pipeline state machine: Analyzing (done by the caller via
//! `mod_analyzer::classify`) → ConflictCheck → Backing_Up → Writing → Verifying →
//! Success/RollingBack.
//!
//! Filesystem operations are not transactional, so failures are undone via an
//! in-memory, append-only rollback action log ([`InstallTransaction`]) consumed only
//! on failure. DB rows for `installed_mod`/`installed_mod_file`/`install_event` are
//! committed in a single `rusqlite` transaction only on Success, so a failed install
//! never leaves partial installed-mod rows. Manual DLC intent is deliberately
//! persisted separately before file changes and remains reviewable on failure.
//!
//! Every write in this pipeline goes through `protected_files::check_write` first; a
//! violation aborts the entire transaction with no partial effect — this cannot be
//! bypassed by any `InstallOptions` flag.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::conflict::{self, ConflictReport};
use crate::dlc_manual::{self, ManualDlcOperation, ManualDlcOperationToken};
use crate::error::{CoreError, CoreResult};
#[cfg(test)]
use crate::mod_analyzer::dlc;
use crate::mod_analyzer::{ModFormat, ModPlan};
use crate::protected_files;
use crate::util::hash_file;

#[derive(Debug, Clone)]
pub enum InstallAction {
    /// A file existed before and was backed up prior to being overwritten.
    FileOverwritten { target: PathBuf, backup: PathBuf },
    /// A file did not exist before; nothing to restore on rollback except deletion.
    FileWritten { target: PathBuf },
}

#[derive(Debug, Default)]
pub struct InstallTransaction {
    actions: Vec<InstallAction>,
}

impl InstallTransaction {
    pub fn record(&mut self, action: InstallAction) {
        self.actions.push(action);
    }

    /// Undoes every recorded action in reverse order. Consumes `self` — a rolled-back
    /// transaction cannot be reused.
    pub fn rollback(self) -> CoreResult<()> {
        let mut failures = Vec::new();
        let mut failed_targets = std::collections::HashSet::new();
        let actions: Vec<_> = self
            .actions
            .into_iter()
            .map(|action| {
                let target = match &action {
                    InstallAction::FileWritten { target }
                    | InstallAction::FileOverwritten { target, .. } => target,
                };
                (rollback_target_key(target), action)
            })
            .collect();
        for (target_key, action) in actions.into_iter().rev() {
            if failed_targets.contains(&target_key) {
                failures.push(format!(
                    "skipped earlier rollback action for {} after a failure",
                    target_key.display()
                ));
                continue;
            }
            let (description, result) = match action {
                InstallAction::FileWritten { target } => {
                    let description = format!("remove {}", target.display());
                    let result = (|| -> CoreResult<()> {
                        if target.try_exists()? {
                            std::fs::remove_file(&target)?;
                        }
                        Ok(())
                    })();
                    (description, result)
                }
                InstallAction::FileOverwritten { target, backup } => {
                    let description =
                        format!("restore {} from {}", target.display(), backup.display());
                    let result = (|| -> CoreResult<()> {
                        let backup_file = std::fs::File::open(&backup)?;
                        if !backup_file.metadata()?.is_file() {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "backup is not a regular file",
                            )
                            .into());
                        }
                        // OpenAI-Codex: A byte-range lock can permit metadata but
                        // reject reads. Detect it before removing the current target.
                        // A later copy can still fail; this is not atomic replacement.
                        hash_file(&backup)?;
                        drop(backup_file);
                        if target.try_exists()? {
                            std::fs::remove_file(&target)?;
                        }
                        crate::util::move_file(&backup, &target)
                    })();
                    (description, result)
                }
            };
            if let Err(err) = result {
                failed_targets.insert(target_key);
                failures.push(format!("{description}: {err:?}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(CoreError::InstallRollbackFailures { failures })
        }
    }
}

fn rollback_target_key(path: &Path) -> PathBuf {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    #[cfg(windows)]
    {
        PathBuf::from(resolved.to_string_lossy().to_lowercase())
    }
    #[cfg(not(windows))]
    {
        resolved
    }
}

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub auto_backup: bool,
    /// Allows review and overwrite of existing files with no identified active owner.
    /// Protected files and files identified as owned by another active mod cannot be overridden.
    pub override_foreign_conflicts: bool,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            auto_backup: true,
            override_foreign_conflicts: false,
        }
    }
}

/// OpenAI-Codex_ui_report_review: Valid page metadata for a new installation.
/// OpenAI-Codex_v1_readiness: Reports whether category selection is available.
// OpenAI-Codex_v1_readiness: Preserve page routing while category selection is paused.
pub fn classification_enabled() -> bool {
    false
}

/// OpenAI-Codex_v1_readiness: Page metadata for a new installation.
#[derive(Debug, Clone, Copy)]
pub struct PageInstallMetadata {
    mode: &'static str,
    category: Option<&'static str>,
}

impl PageInstallMetadata {
    pub fn new(mode: &str, category: Option<&str>) -> Option<Self> {
        if category.is_some() && !classification_enabled() {
            return None;
        }
        let (mode, category) = match (mode, category) {
            ("legacy-sp", None) => ("legacy-sp", None),
            // OpenAI-Codex_v1_readiness: Keep page routing while classification is paused.
            ("legacy-lspdfr", None) => ("legacy-lspdfr", None),
            ("legacy-lspdfr", Some("framework")) => ("legacy-lspdfr", Some("framework")),
            ("legacy-lspdfr", Some("eup-peds")) => ("legacy-lspdfr", Some("eup-peds")),
            ("legacy-lspdfr", Some("vehicles")) => ("legacy-lspdfr", Some("vehicles")),
            ("legacy-lspdfr", Some("callouts")) => ("legacy-lspdfr", Some("callouts")),
            ("legacy-lspdfr", Some("other")) => ("legacy-lspdfr", Some("other")),
            _ => return None,
        };
        Some(Self { mode, category })
    }
}

fn verify_page_install_metadata(
    conn: &Connection,
    installed_mod_id: i64,
    metadata: PageInstallMetadata,
) -> CoreResult<()> {
    let mut statement = conn.prepare(
        "SELECT 1 FROM installed_mod \
         WHERE id = ?1 AND mode = ?2 AND mode_inferred = 0 AND category IS ?3 LIMIT 1",
    )?;
    let query_result: rusqlite::Result<()> = match statement.query(rusqlite::params![
        installed_mod_id,
        metadata.mode,
        metadata.category
    ]) {
        Ok(mut rows) => {
            let mut matched = false;
            loop {
                match rows.next() {
                    Ok(Some(_)) => matched = true,
                    Ok(None) if matched => break Ok(()),
                    Ok(None) => break Err(rusqlite::Error::QueryReturnedNoRows),
                    Err(error) => break Err(error),
                }
            }
        }
        Err(error) => Err(error),
    };
    let finalize_result = statement.finalize();
    // OpenAI-Codex_ui_report_review: Keep the observed query error if finalization also fails.
    query_result.and(finalize_result).map_err(CoreError::from)
}

#[derive(Debug, Clone, serde::Serialize)]
pub enum InstallOutcome {
    Success {
        installed_mod_id: i64,
        files_written: usize,
    },
    /// Refused: at least one existing file has no identified active owner and
    /// `InstallOptions::override_foreign_conflicts` was not set. Direct installs
    /// do not write files before this result. Reinstall performs a preflight, but
    /// a later conflict can arise after uninstall if the filesystem or DB changes.
    RequiresOverride(ConflictReport),
    /// Refused, unconditionally: at least one target is on the `protected_files`
    /// list. No option can bypass this; see the reinstall caveat above.
    ProtectedFileBlocked(Vec<PathBuf>),
}

impl InstallOutcome {
    /// OpenAI-Codex: Do not let callers treat a refused operation as successful.
    pub fn require_success(self) -> CoreResult<Self> {
        match &self {
            Self::Success { .. } => Ok(self),
            Self::RequiresOverride(report) => match report.foreign_conflicts.first() {
                Some(conflict) => match conflict.owner_mod_id {
                    Some(owner_mod_id) => Err(CoreError::ForeignConflict {
                        path: conflict.path.clone(),
                        owner_mod_id,
                    }),
                    None => Err(CoreError::UntrackedFileConflict {
                        path: conflict.path.clone(),
                    }),
                },
                None => Err(CoreError::UnsupportedFormat {
                    reason: "the installation requires conflict confirmation".to_string(),
                }),
            },
            Self::ProtectedFileBlocked(paths) => match paths.first() {
                Some(path) => Err(CoreError::ProtectedFileViolation { path: path.clone() }),
                None => Err(CoreError::UnsupportedFormat {
                    reason: "the installation targets a protected file".to_string(),
                }),
            },
        }
    }
}

fn source_type_str(format: &ModFormat) -> CoreResult<&'static str> {
    Ok(match format {
        ModFormat::Asi => "asi",
        ModFormat::NativeDll | ModFormat::ManagedDll => "dll",
        ModFormat::MenyooXml => "menyoo_xml",
        ModFormat::FolderReplacer | ModFormat::AddOnPack { .. } => "folder",
        ModFormat::Zip => "zip",
        ModFormat::SevenZip => "sevenzip",
        ModFormat::Rar => "rar",
        ModFormat::Unsupported(reason) => {
            return Err(CoreError::UnsupportedFormat {
                reason: reason.clone(),
            })
        }
    })
}

// OpenAI-Codex: Limit backup writes near known LML reservations.
fn ensure_lml_backup_paths_allowed(
    conn: &Connection,
    game_root: &Path,
    backup_root: &Path,
    plan: &ModPlan,
    auto_backup: bool,
) -> CoreResult<()> {
    if !auto_backup || plan.files.is_empty() {
        return Ok(());
    }
    let mut paths = vec![backup_root.to_path_buf()];
    paths.extend(
        plan.files
            .iter()
            .enumerate()
            .map(|(index, _)| backup_root.join(format!("{index}.bak"))),
    );
    crate::lml_store::ensure_generic_targets_allowed(conn, game_root, &paths)
}

// OpenAI-Codex: Refuse backup slots that are already occupied at preflight.
fn refuse_existing_backup_slots(
    backup_root: &Path,
    plan: &ModPlan,
    auto_backup: bool,
) -> CoreResult<()> {
    if !auto_backup {
        return Ok(());
    }
    for index in 0..plan.files.len() {
        let path = backup_root.join(format!("{index}.bak"));
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                return Err(CoreError::UnsupportedFormat {
                    reason: format!("backup destination already exists: {}", path.display()),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

// OpenAI-Codex: Compare backup destinations using the existing folded path rules.
fn backup_prefix_hit(set: &std::collections::BTreeSet<String>, key: &str) -> bool {
    if set.contains(key) {
        return true;
    }
    if key
        .match_indices('/')
        .any(|(index, _)| set.contains(&key[..index]))
    {
        return true;
    }
    let prefix = format!("{key}/");
    set.range(prefix.clone()..)
        .next()
        .is_some_and(|candidate| candidate.starts_with(&prefix))
}

#[derive(Default)]
struct BackupSlotIndex {
    paths: Vec<PathBuf>,
    keys: Vec<String>,
    lower: std::collections::BTreeSet<String>,
    upper: std::collections::BTreeSet<String>,
}

impl BackupSlotIndex {
    fn insert(&mut self, path: PathBuf) -> CoreResult<()> {
        let key = crate::lml_store::backup_guard_identity(&path)?;
        self.lower.insert(key.to_lowercase());
        self.upper.insert(key.to_uppercase());
        self.paths.push(path);
        self.keys.push(key);
        Ok(())
    }

    fn overlap(&self, path: &Path) -> CoreResult<Option<&Path>> {
        let key = crate::lml_store::backup_guard_identity(path)?;
        if !backup_prefix_hit(&self.lower, &key.to_lowercase())
            && !backup_prefix_hit(&self.upper, &key.to_uppercase())
        {
            return Ok(None);
        }
        let index = self
            .keys
            .iter()
            .position(|slot| crate::lml_store::backup_guard_overlap(slot, &key))
            .ok_or_else(|| CoreError::UnsupportedFormat {
                reason: "backup path comparison was inconsistent".to_string(),
            })?;
        Ok(Some(self.paths[index].as_path()))
    }
}

// OpenAI-Codex: Protect same-database paths before backup slots are created.
fn refuse_backup_path_overlaps(
    conn: &Connection,
    backup_root: &Path,
    plan: &ModPlan,
    auto_backup: bool,
) -> CoreResult<()> {
    if !auto_backup || plan.files.is_empty() {
        return Ok(());
    }
    let mut slots = BackupSlotIndex::default();
    for index in 0..plan.files.len() {
        slots.insert(backup_root.join(format!("{index}.bak")))?;
    }
    for file in &plan.files {
        if let Some(slot) = slots.overlap(&file.target)? {
            return Err(CoreError::UnsupportedFormat {
                reason: format!(
                    "backup destination overlaps planned target: {}",
                    slot.display()
                ),
            });
        }
    }

    let stored = {
        let mut statement = conn
            .prepare("SELECT id, target_path, backup_path FROM installed_mod_file ORDER BY id")?;
        let mut rows = statement.query([])?;
        let mut stored = Vec::new();
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let target: String = row.get(1).map_err(|_| CoreError::UnsupportedFormat {
                reason: format!("cannot read recorded target path for file {id}"),
            })?;
            let backup: Option<String> = row.get(2).map_err(|_| CoreError::UnsupportedFormat {
                reason: format!("cannot read recorded backup path for file {id}"),
            })?;
            stored.push((id, target, backup));
        }
        stored
    };
    for (id, target, backup) in stored {
        let target_hit =
            slots
                .overlap(Path::new(&target))
                .map_err(|error| CoreError::UnsupportedFormat {
                    reason: format!("cannot verify recorded target path for file {id}: {error}"),
                })?;
        if let Some(slot) = target_hit {
            return Err(CoreError::UnsupportedFormat {
                reason: format!(
                    "backup destination overlaps a recorded target: {}",
                    slot.display()
                ),
            });
        }
        if let Some(backup) = backup {
            let backup_hit = slots.overlap(Path::new(&backup)).map_err(|error| {
                CoreError::UnsupportedFormat {
                    reason: format!("cannot verify recorded backup path for file {id}: {error}"),
                }
            })?;
            if let Some(slot) = backup_hit {
                return Err(CoreError::UnsupportedFormat {
                    reason: format!(
                        "backup destination is already referenced: {}",
                        slot.display()
                    ),
                });
            }
        }
    }
    Ok(())
}

// OpenAI-Codex: A planned target must not overwrite a recorded backup.
fn refuse_targets_overlapping_recorded_backups(
    conn: &Connection,
    plan: &ModPlan,
) -> CoreResult<()> {
    if plan.files.is_empty() {
        return Ok(());
    }
    let mut targets = BackupSlotIndex::default();
    for file in &plan.files {
        targets.insert(file.target.clone())?;
    }
    let backups = {
        let mut statement = conn.prepare(
            "SELECT id, backup_path FROM installed_mod_file \
             WHERE backup_path IS NOT NULL ORDER BY id",
        )?;
        let mut rows = statement.query([])?;
        let mut backups = Vec::new();
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let backup: String = row.get(1).map_err(|_| CoreError::UnsupportedFormat {
                reason: format!("cannot read recorded backup path for file {id}"),
            })?;
            backups.push((id, backup));
        }
        backups
    };
    for (id, backup) in backups {
        let hit =
            targets
                .overlap(Path::new(&backup))
                .map_err(|error| CoreError::UnsupportedFormat {
                    reason: format!("cannot verify recorded backup path for file {id}: {error}"),
                })?;
        if let Some(target) = hit {
            return Err(CoreError::UnsupportedFormat {
                reason: format!(
                    "planned target overlaps a recorded backup: {}",
                    target.display()
                ),
            });
        }
    }
    Ok(())
}

// OpenAI-Codex: Resolve ordinary relative sources without following links.
fn source_guard_absolute(path: &Path) -> CoreResult<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(CoreError::UnsupportedFormat {
            reason: "source path is empty".to_string(),
        });
    }
    let absolute_input = path.is_absolute();
    let mut absolute = if absolute_input {
        PathBuf::new()
    } else {
        std::env::current_dir()?
    };
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => absolute.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::Prefix(_) | std::path::Component::RootDir if absolute_input => {
                absolute.push(component.as_os_str());
            }
            _ => {
                return Err(CoreError::UnsupportedFormat {
                    reason: format!("source path is not safely resolvable: {}", path.display()),
                });
            }
        }
    }
    if !absolute.is_absolute() {
        return Err(CoreError::UnsupportedFormat {
            reason: format!("source path is not absolute: {}", path.display()),
        });
    }
    Ok(absolute)
}

// OpenAI-Codex: Compare all sources before an earlier write can change them.
fn refuse_source_path_overlaps(
    plan: &ModPlan,
    source_path: &Path,
    backup_root: &Path,
    auto_backup: bool,
) -> CoreResult<()> {
    if plan.files.is_empty() {
        return Ok(());
    }
    let mut targets = BackupSlotIndex::default();
    let mut sources = Vec::with_capacity(plan.files.len());
    for file in &plan.files {
        targets.insert(file.target.clone())?;
        sources.push(source_guard_absolute(&file.source)?);
    }
    for source in &sources {
        if let Some(target) = targets.overlap(source)? {
            return Err(CoreError::UnsupportedFormat {
                reason: format!(
                    "planned source overlaps a planned target: {}",
                    target.display()
                ),
            });
        }
    }
    let original = source_guard_absolute(source_path)?;
    if let Some(target) = targets.overlap(&original)? {
        return Err(CoreError::UnsupportedFormat {
            reason: format!(
                "planned target overlaps the original package: {}",
                target.display()
            ),
        });
    }
    if auto_backup {
        let mut slots = BackupSlotIndex::default();
        for index in 0..plan.files.len() {
            slots.insert(backup_root.join(format!("{index}.bak")))?;
        }
        for source in &sources {
            if let Some(slot) = slots.overlap(source)? {
                return Err(CoreError::UnsupportedFormat {
                    reason: format!(
                        "planned source overlaps backup destination: {}",
                        slot.display()
                    ),
                });
            }
        }
        if let Some(slot) = slots.overlap(&original)? {
            return Err(CoreError::UnsupportedFormat {
                reason: format!(
                    "original package overlaps backup destination: {}",
                    slot.display()
                ),
            });
        }
    }
    Ok(())
}

// OpenAI-Codex: Preserve new sources from the old active mod's removal.
fn refuse_reinstall_sources_overlapping_old_paths(
    conn: &Connection,
    mod_id: i64,
    status: &str,
    sources: &[PathBuf],
) -> CoreResult<()> {
    if status == "disabled" || status == "uninstalled" || sources.is_empty() {
        return Ok(());
    }
    let recorded = {
        let mut statement = conn.prepare(
            "SELECT target_path, backup_path FROM installed_mod_file \
             WHERE installed_mod_id = ?1 ORDER BY id",
        )?;
        let rows = statement
            .query_map([mod_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut old_targets = BackupSlotIndex::default();
    let mut old_backups = BackupSlotIndex::default();
    for (target, backup) in recorded {
        old_targets.insert(PathBuf::from(target))?;
        if let Some(backup) = backup {
            old_backups.insert(PathBuf::from(backup))?;
        }
    }
    for source in sources {
        let source = source_guard_absolute(source)?;
        if let Some(target) = old_targets.overlap(&source)? {
            return Err(CoreError::UnsupportedFormat {
                reason: format!(
                    "reinstall source overlaps an old target: {}",
                    target.display()
                ),
            });
        }
        if let Some(backup) = old_backups.overlap(&source)? {
            return Err(CoreError::UnsupportedFormat {
                reason: format!(
                    "reinstall source overlaps an old backup: {}",
                    backup.display()
                ),
            });
        }
    }
    Ok(())
}

// OpenAI-Codex: A planned install cannot assign overlapping file targets.
fn refuse_overlapping_planned_targets(plan: &ModPlan) -> CoreResult<()> {
    let mut seen = BackupSlotIndex::default();
    for file in &plan.files {
        if seen.overlap(&file.target)?.is_some() {
            return Err(CoreError::UnsupportedFormat {
                reason: format!("planned install targets overlap: {}", file.target.display()),
            });
        }
        seen.insert(file.target.clone())?;
    }
    Ok(())
}

// OpenAI-Codex_v1_readiness: Check planned targets before any install writes.
fn plan_requires_backup(plan: &ModPlan) -> CoreResult<bool> {
    for file in &plan.files {
        if file.target.try_exists()? {
            return Ok(true);
        }
    }
    Ok(false)
}

// OpenAI-Codex_v1_readiness: Consider files that uninstall will remove or restore.
fn reinstall_requires_backup(
    conn: &Connection,
    mod_id: i64,
    status: &str,
    plan: &ModPlan,
) -> CoreResult<bool> {
    let mut statement = conn.prepare(
        "SELECT target_path, backup_path FROM installed_mod_file WHERE installed_mod_id = ?1",
    )?;
    let recorded: Vec<(String, Option<String>)> = statement
        .query_map([mod_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(statement);
    for file in &plan.files {
        if status != "disabled" && status != "uninstalled" {
            let mut old = recorded
                .iter()
                .filter(|(target, _)| Path::new(target) == file.target.as_path());
            if let Some((_, backup)) = old.next() {
                if old.next().is_none() {
                    if backup.is_some() {
                        return Ok(true);
                    }
                    continue;
                }
            }
        }
        if file.target.try_exists()? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Runs the full install pipeline for `plan` (already produced by
/// `mod_analyzer::classify`). `backup_root` is where overwritten originals are copied
/// before being replaced — callers should point this at a per-install-attempt
/// subdirectory they control (tests use a temp dir; the CLI/app will use an app-data
/// backups folder).
pub fn install(
    conn: &mut Connection,
    name: &str,
    plan: &ModPlan,
    game_root: &Path,
    backup_root: &Path,
    options: InstallOptions,
    source_path: &Path,
) -> CoreResult<InstallOutcome> {
    install_with_expected_restores(
        conn,
        name,
        plan,
        game_root,
        backup_root,
        options,
        source_path,
        &std::collections::HashMap::new(),
        None,
        None,
        None,
    )
}

/// OpenAI-Codex_ui_report_review: Record validated page metadata in the install transaction.
/// The caller must classify the plan for the selected page and game root.
#[allow(clippy::too_many_arguments)]
pub fn install_with_page_metadata(
    conn: &mut Connection,
    name: &str,
    plan: &ModPlan,
    game_root: &Path,
    backup_root: &Path,
    options: InstallOptions,
    source_path: &Path,
    page_metadata: PageInstallMetadata,
) -> CoreResult<InstallOutcome> {
    install_with_expected_restores(
        conn,
        name,
        plan,
        game_root,
        backup_root,
        options,
        source_path,
        &std::collections::HashMap::new(),
        None,
        None,
        Some(page_metadata),
    )
}

#[allow(clippy::too_many_arguments)]
fn install_with_expected_restores(
    conn: &mut Connection,
    name: &str,
    plan: &ModPlan,
    game_root: &Path,
    backup_root: &Path,
    options: InstallOptions,
    source_path: &Path,
    expected_restores: &std::collections::HashMap<PathBuf, String>,
    manual_operation: Option<&ManualDlcOperationToken>,
    preserved_mode: Option<&str>,
    page_metadata: Option<PageInstallMetadata>,
) -> CoreResult<InstallOutcome> {
    // OpenAI-Codex: Shared protection for callers that already hold a plan.
    crate::providers::ensure_game_root_available(game_root)?;
    let source_type = source_type_str(&plan.format)?;
    // OpenAI-Codex; OpenAI-Codex_Responsible: Source_Code.
    if plan.files.is_empty() {
        return Err(CoreError::UnsupportedFormat {
            reason: "the install plan contains no files".to_string(),
        });
    }

    // --- ConflictCheck ---
    let targets: Vec<PathBuf> = plan.files.iter().map(|f| f.target.clone()).collect();
    // OpenAI-Codex_lml_generic_guards; OpenAI-Codex: Also protect caller-built plans.
    crate::lml_store::ensure_generic_targets_allowed(conn, game_root, &targets)?;
    // OpenAI-Codex: Reject duplicate or nested targets before any writes.
    refuse_overlapping_planned_targets(plan)?;
    // OpenAI-Codex: Include the new backup destinations in the LML guard.
    ensure_lml_backup_paths_allowed(conn, game_root, backup_root, plan, options.auto_backup)?;
    // OpenAI-Codex: Check all slots before recording intent or writing targets.
    refuse_existing_backup_slots(backup_root, plan, options.auto_backup)?;
    // OpenAI-Codex: Refuse collisions with targets and recorded backup paths.
    refuse_backup_path_overlaps(conn, backup_root, plan, options.auto_backup)?;
    // OpenAI-Codex: Recorded backups are protected even without auto-backup.
    refuse_targets_overlapping_recorded_backups(conn, plan)?;
    // OpenAI-Codex: Protect sources and the original package before writes.
    refuse_source_path_overlaps(plan, source_path, backup_root, options.auto_backup)?;
    let mut report = conflict::analyze(conn, &targets)?;
    retain_unexpected_conflicts(&mut report, expected_restores)?;

    if report.has_protected_hits() {
        let paths = report
            .protected_hits
            .iter()
            .map(|h| h.path.clone())
            .collect();
        return Ok(InstallOutcome::ProtectedFileBlocked(paths));
    }
    // OpenAI-Codex_v1_removal_review: Reject observed active owners before overwrite.
    refuse_active_owner_conflicts(&report)?;
    if report.requires_explicit_override() && !options.override_foreign_conflicts {
        return Ok(InstallOutcome::RequiresOverride(report));
    }

    // OpenAI-Codex_v1_readiness: Refuse only when the current plan needs backup.
    if options.auto_backup && plan_requires_backup(plan)? {
        crate::full_backup::ensure_backup_available()?;
    }
    // OpenAI-Codex: Persist manual registration intent before the first file write.
    let owned_operation;
    let manual_operation = match manual_operation {
        Some(operation) => operation,
        None => {
            let packs = dlc_manual::pack_names_for_paths(game_root, &targets)?;
            owned_operation = dlc_manual::begin_tasks(
                conn,
                game_root,
                &packs,
                ManualDlcOperation::Install,
                None,
            )?;
            &owned_operation
        }
    };

    // --- Backing_Up + Writing ---
    let mut tx_log = InstallTransaction::default();
    let result = perform_writes(plan, backup_root, options.auto_backup, &mut tx_log);

    let mut file_hashes = match result {
        Ok(hashes) => hashes,
        Err(err) => {
            return Err(rollback_failed_install(conn, tx_log, err));
        }
    };

    // --- Verifying ---
    // (file existence was already confirmed while writing; re-check here as the
    // dedicated Verifying step so a future change to `perform_writes` can't silently
    // skip it)
    for planned in &plan.files {
        if !planned.target.exists() {
            let err = CoreError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "expected file missing after write: {}",
                    planned.target.display()
                ),
            ));
            return Err(rollback_failed_install(conn, tx_log, err));
        }
    }

    // --- Success: commit DB rows in one transaction ---
    let db_result = (|| -> CoreResult<i64> {
        let db_tx = conn.transaction()?;
        let installed_mod_id = if page_metadata.is_some() {
            // OpenAI-Codex_Source_Code: Retain the original NULL-mode parent insert.
            let mut statement = db_tx.prepare(
                "INSERT INTO installed_mod (name, source_type, install_path, status, source_path, mode) \
                 VALUES (?1, ?2, ?3, 'active', ?4, ?5)",
            )?;
            let insert_result = statement.insert(rusqlite::params![
                name,
                source_type,
                game_root.to_string_lossy(),
                source_path.to_string_lossy(),
                None::<&str>,
            ]);
            let finalize_result = statement.finalize();
            match (insert_result, finalize_result) {
                (Err(primary), _) => return Err(primary.into()),
                (Ok(_), Err(finalize_error)) => return Err(finalize_error.into()),
                (Ok(id), Ok(())) => id,
            }
        } else {
            db_tx.execute(
                "INSERT INTO installed_mod (name, source_type, install_path, status, source_path, mode) \
                 VALUES (?1, ?2, ?3, 'active', ?4, ?5)",
                rusqlite::params![
                    name,
                    source_type,
                    game_root.to_string_lossy(),
                    source_path.to_string_lossy(),
                    preserved_mode
                ],
            )?;
            db_tx.last_insert_rowid()
        };

        for planned in &plan.files {
            let hash = file_hashes.remove(&planned.target).unwrap_or_default();
            let backup_path = tx_log_backup_path_for(&tx_log, &planned.target);
            db_tx.execute(
                "INSERT INTO installed_mod_file (installed_mod_id, target_path, backup_path, file_hash) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    installed_mod_id,
                    planned.target.to_string_lossy(),
                    backup_path.map(|p| p.to_string_lossy().into_owned()),
                    hash,
                ],
            )?;
        }
        db_tx.execute(
            "INSERT INTO install_event (installed_mod_id, event_type, success) VALUES (?1, 'install', 1)",
            [installed_mod_id],
        )?;
        dlc_manual::settle_tasks(&db_tx, manual_operation)?;
        if let Some(metadata) = page_metadata {
            // OpenAI-Codex_Source_Code: Check the actual metadata UPDATE count before commit.
            let mut statement = db_tx.prepare(
                "UPDATE installed_mod SET mode = ?1, mode_inferred = 0, category = ?3 WHERE id = ?2",
            )?;
            let update_result = statement
                .execute(rusqlite::params![
                    metadata.mode,
                    installed_mod_id,
                    metadata.category
                ])
                .and_then(|count| {
                    if count == 1 {
                        Ok(())
                    } else {
                        Err(rusqlite::Error::StatementChangedRows(count))
                    }
                });
            let finalize_result = statement.finalize();
            update_result.and(finalize_result)?;
            verify_page_install_metadata(&db_tx, installed_mod_id, metadata)?;
        }
        db_tx.commit()?;
        Ok(installed_mod_id)
    })();

    let installed_mod_id = match db_result {
        Ok(installed_mod_id) => installed_mod_id,
        Err(err) => {
            let rollback_result = tx_log.rollback();
            let _ = record_failed_event(conn, None, &err);
            if let Err(rollback_err) = rollback_result {
                let mut stderr = std::io::stderr().lock();
                let _ = std::io::Write::write_fmt(
                    &mut stderr,
                    format_args!(
                        "install database operation failed ({err:?}); filesystem rollback also failed ({rollback_err:?})\n"
                    ),
                );
                return Err(CoreError::InstallDatabaseRollbackFailed {
                    database_error: Box::new(err),
                    rollback_error: Box::new(rollback_err),
                });
            }
            return Err(err);
        }
    };

    Ok(InstallOutcome::Success {
        installed_mod_id,
        files_written: plan.files.len(),
    })
}

/// OpenAI-Codex: A reinstall may recognize only its own unchanged restored originals.
fn retain_unexpected_conflicts(
    report: &mut ConflictReport,
    expected_restores: &std::collections::HashMap<PathBuf, String>,
) -> CoreResult<()> {
    let conflicts = std::mem::take(&mut report.foreign_conflicts);
    for conflict in conflicts {
        if conflict.owner_mod_id.is_none() {
            if let Some(expected_hash) = expected_restores.get(&rollback_target_key(&conflict.path))
            {
                if !std::fs::symlink_metadata(&conflict.path)?
                    .file_type()
                    .is_file()
                {
                    return Err(CoreError::UnsupportedFormat {
                        reason: format!(
                            "restored target is not a regular file: {}",
                            conflict.path.display()
                        ),
                    });
                }
                if &hash_file(&conflict.path)? == expected_hash {
                    continue;
                }
            }
        }
        report.foreign_conflicts.push(conflict);
    }
    Ok(())
}

// OpenAI-Codex_v1_removal_review: A known active owner cannot be overridden.
fn refuse_active_owner_conflicts(report: &ConflictReport) -> CoreResult<()> {
    for conflict in &report.foreign_conflicts {
        if let Some(owner_mod_id) = conflict.owner_mod_id {
            return Err(CoreError::ForeignConflict {
                path: conflict.path.clone(),
                owner_mod_id,
            });
        }
    }
    Ok(())
}

/// Reinstalls `mod_id` from `new_source_path` — uninstalling its current files first
/// (if not already uninstalled), then running the normal install pipeline against the
/// new source. This is the backing implementation for the AI Action Schema's
/// `ReinstallMod` (see `ai_assistant::action_schema` module docs for why this needed a
/// schema fix first: `Action::ReinstallMod` originally had no source-path field, and
/// this crate has no mechanism to look one up on its own — `installed_mod.source_path`
/// (added alongside this function) only remembers a *previous* install's source, and
/// only if that file/folder is still there; it cannot discover a *different* version's
/// package location, since this project never downloads mods on the user's behalf.
///
/// Not transactional across the uninstall+install boundary: if `install` fails after
/// `uninstall` already succeeded, `mod_id` is left uninstalled (recoverable from the
/// recycle bin) rather than silently reinstalling the old version — a real interruption
/// here should surface as a real, visible failure, not be hidden by an automatic revert
/// this project has no way to guarantee is actually safe.
#[allow(clippy::too_many_arguments)]
pub fn reinstall(
    conn: &mut Connection,
    mod_id: i64,
    new_source_path: &Path,
    version_label: &str,
    provider: &dyn crate::providers::ModeProvider,
    game_root: &Path,
    backup_root: &Path,
    recycle_bin_root: &Path,
    options: InstallOptions,
) -> CoreResult<InstallOutcome> {
    reinstall_impl(
        conn,
        mod_id,
        new_source_path,
        version_label,
        provider,
        game_root,
        backup_root,
        recycle_bin_root,
        options,
        None,
    )
}

/// OpenAI-Codex: Preserves disabled payloads in the app-owned recycle bin.
#[allow(clippy::too_many_arguments)]
pub fn reinstall_with_staging(
    conn: &mut Connection,
    mod_id: i64,
    new_source_path: &Path,
    version_label: &str,
    provider: &dyn crate::providers::ModeProvider,
    game_root: &Path,
    backup_root: &Path,
    recycle_bin_root: &Path,
    options: InstallOptions,
    staging_root: &Path,
) -> CoreResult<InstallOutcome> {
    reinstall_impl(
        conn,
        mod_id,
        new_source_path,
        version_label,
        provider,
        game_root,
        backup_root,
        recycle_bin_root,
        options,
        Some(staging_root),
    )
}

// OpenAI-Codex_lspdfr_role_core: DB destinations, not current file existence,
// preserve the user's standalone LSPDFR role across all reinstall callers.
fn preserve_lspdfr_reinstall_role(
    conn: &Connection,
    mod_id: i64,
    source: &Path,
    game_root: &Path,
    plan: &ModPlan,
) -> CoreResult<Option<&'static str>> {
    let (mode, source_type, old_root): (Option<String>, String, String) = conn.query_row(
        "SELECT mode, source_type, install_path FROM installed_mod WHERE id = ?1",
        [mod_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    // Historical managed and native DLLs both use `dll`; archives retain their
    // archive source type, even if they contain only one managed DLL.
    if mode.as_deref() != Some("legacy-lspdfr") || source_type != "dll" {
        return Ok(None);
    }
    let refused = || {
        CoreError::UnsupportedFormat {
        reason: "cannot safely preserve the recorded LSPDFR DLL destination; choose a standalone managed DLL and retain its recorded Plugins or Plugins/LSPDFR role".to_string(),
    }
    };
    let root = lspdfr_role_path_key(game_root).ok_or_else(refused)?;
    if lspdfr_role_path_key(Path::new(&old_root)).as_ref() != Some(&root) {
        return Err(refused());
    }
    let mut statement =
        conn.prepare("SELECT target_path FROM installed_mod_file WHERE installed_mod_id = ?1")?;
    let targets: Vec<String> = statement
        .query_map([mod_id], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    if targets.len() != 1 {
        return Err(refused());
    }
    let old_target = lspdfr_role_path_key(Path::new(&targets[0])).ok_or_else(refused)?;
    let prefix = format!("{root}/");
    let relative = old_target
        .strip_prefix(prefix.as_str())
        .ok_or_else(refused)?;
    let (folder, basename) = relative.rsplit_once('/').unwrap_or(("", relative));
    if !lspdfr_role_dll_basename(basename) {
        return Err(refused());
    }
    // A single root DLL is the existing native-DLL layout, outside role routing.
    if folder.is_empty() {
        return Ok(None);
    }
    if !matches!(folder, "Plugins" | "Plugins/LSPDFR") {
        return Err(refused());
    }
    if plan.format != ModFormat::ManagedDll || plan.files.len() != 1 {
        return Err(refused());
    }
    let metadata = std::fs::symlink_metadata(source)?;
    if !metadata.file_type().is_file() {
        return Err(refused());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(refused());
        }
    }
    let source_name = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(refused)?;
    if !lspdfr_role_dll_basename(source_name) || plan.files[0].source != source {
        return Err(refused());
    }
    let expected = game_root.join(folder).join(source_name);
    if lspdfr_role_path_key(&plan.files[0].target) != lspdfr_role_path_key(&expected) {
        return Err(refused());
    }
    Ok(Some("legacy-lspdfr"))
}

fn lspdfr_role_path_key(path: &Path) -> Option<String> {
    if !path.is_absolute() {
        return None;
    }
    let path = path.to_str()?.replace('\\', "/");
    if path.split('/').any(|part| part == "." || part == "..") {
        return None;
    }
    let path = path.trim_end_matches('/').to_string();
    Some(path)
}

fn lspdfr_role_dll_basename(name: &str) -> bool {
    let Some(stem) = name.get(..name.len().saturating_sub(4)) else {
        return false;
    };
    !stem.is_empty()
        && name.to_ascii_lowercase().ends_with(".dll")
        && !stem.ends_with(['.', ' '])
        && !name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        && !matches!(
            stem.split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase()
                .as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        )
}

#[allow(clippy::too_many_arguments)]
fn reinstall_impl(
    conn: &mut Connection,
    mod_id: i64,
    new_source_path: &Path,
    version_label: &str,
    provider: &dyn crate::providers::ModeProvider,
    game_root: &Path,
    backup_root: &Path,
    recycle_bin_root: &Path,
    options: InstallOptions,
    staging_root: Option<&Path>,
) -> CoreResult<InstallOutcome> {
    // OpenAI-Codex: Check both workspaces before uninstalling the previous version.
    crate::providers::ensure_installed_mod_available(conn, mod_id)?;
    crate::providers::ensure_game_root_available(game_root)?;
    let (old_name, status): (String, String) = conn
        .query_row(
            "SELECT name, status FROM installed_mod WHERE id = ?1",
            [mod_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| {
            if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                CoreError::UnsupportedFormat {
                    reason: format!("no installed mod with id {mod_id}"),
                }
            } else {
                e.into()
            }
        })?;

    if status == "disabled" {
        let staging_root = staging_root.ok_or_else(|| CoreError::UnsupportedFormat {
            reason: "disabled reinstall requires an explicit staging root".to_string(),
        })?;
        crate::state::disabled_payloads(conn, mod_id, staging_root)?;
        let source = std::fs::canonicalize(new_source_path)?;
        if source.starts_with(std::fs::canonicalize(staging_root)?) {
            return Err(CoreError::UnsupportedFormat {
                reason: "choose an original package outside managed staging for reinstall"
                    .to_string(),
            });
        }
    }
    // OpenAI-Codex: Check snapshot paths before recording reinstall intent.
    if status != "uninstalled" {
        crate::uninstall::preflight_recycle_snapshot_paths(conn, mod_id, game_root)?;
    }

    // OpenAI-Codex: Check the original package before classification and uninstall.
    refuse_reinstall_sources_overlapping_old_paths(
        conn,
        mod_id,
        &status,
        &[new_source_path.to_path_buf()],
    )?;

    // OpenAI-Codex: Check the backup root before classifying or removing the old mod.
    if options.auto_backup {
        crate::lml_store::ensure_generic_targets_allowed(
            conn,
            game_root,
            &[backup_root.to_path_buf()],
        )?;
    }

    // OpenAI-Codex: Validate the new package and unrelated conflicts before removal.
    // Keep this plan alive: archive source paths belong to its staging directory.
    let plan = crate::mod_analyzer::classify(new_source_path, provider)?;
    source_type_str(&plan.format)?;
    // OpenAI-Codex: Check the new plan before removing the old mod.
    refuse_overlapping_planned_targets(&plan)?;
    // OpenAI-Codex: Check all prospective backup slots before uninstall.
    ensure_lml_backup_paths_allowed(conn, game_root, backup_root, &plan, options.auto_backup)?;
    // OpenAI-Codex: Check all slots before the old mod is removed.
    refuse_existing_backup_slots(backup_root, &plan, options.auto_backup)?;
    // OpenAI-Codex: Check recorded paths before removing the old mod.
    refuse_backup_path_overlaps(conn, backup_root, &plan, options.auto_backup)?;
    // OpenAI-Codex: Protect recorded backups before uninstalling the old mod.
    refuse_targets_overlapping_recorded_backups(conn, &plan)?;
    // OpenAI-Codex: Check source aliases before removing the old mod.
    refuse_source_path_overlaps(&plan, new_source_path, backup_root, options.auto_backup)?;
    // OpenAI-Codex: Check classified sources before uninstalling the old mod.
    let reinstall_sources: Vec<PathBuf> =
        plan.files.iter().map(|file| file.source.clone()).collect();
    refuse_reinstall_sources_overlapping_old_paths(conn, mod_id, &status, &reinstall_sources)?;
    let preserved_mode =
        preserve_lspdfr_reinstall_role(conn, mod_id, new_source_path, game_root, &plan)?;
    if plan.files.is_empty() {
        return Err(CoreError::UnsupportedFormat {
            reason: "the new package contains no installable files".to_string(),
        });
    }
    for file in &plan.files {
        if !std::fs::metadata(&file.source)?.is_file() {
            return Err(CoreError::UnsupportedFormat {
                reason: format!(
                    "install source is not a regular file: {}",
                    file.source.display()
                ),
            });
        }
        match std::fs::metadata(&file.target) {
            Ok(metadata) if !metadata.is_file() => {
                return Err(CoreError::UnsupportedFormat {
                    reason: format!(
                        "install target is not a regular file: {}",
                        file.target.display()
                    ),
                });
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(error.into());
            }
            _ => {}
        }
    }
    let targets: Vec<PathBuf> = plan.files.iter().map(|file| file.target.clone()).collect();
    // OpenAI-Codex_lml_generic_guards; OpenAI-Codex: Refuse new targets before old uninstall.
    crate::lml_store::ensure_generic_targets_allowed(conn, game_root, &targets)?;
    let mut report = conflict::analyze(conn, &targets)?;
    if report.has_protected_hits() {
        return Ok(InstallOutcome::ProtectedFileBlocked(
            report
                .protected_hits
                .iter()
                .map(|hit| hit.path.clone())
                .collect(),
        ));
    }
    report
        .foreign_conflicts
        .retain(|conflict| conflict.owner_mod_id != Some(mod_id));
    if report
        .self_update
        .as_ref()
        .is_some_and(|suggestion| suggestion.existing_mod_id == mod_id)
    {
        report.self_update = None;
    }
    // OpenAI-Codex_v1_removal_review: Reject observed active owners before overwrite.
    refuse_active_owner_conflicts(&report)?;
    if report.requires_explicit_override() && !options.override_foreign_conflicts {
        return Ok(InstallOutcome::RequiresOverride(report));
    }

    let mut expected_restores = std::collections::HashMap::new();
    if status != "uninstalled" {
        let mut statement = conn.prepare(
            "SELECT target_path, backup_path FROM installed_mod_file WHERE installed_mod_id = ?1",
        )?;
        let old_targets: Vec<(String, Option<String>)> = statement
            .query_map([mod_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        drop(statement);
        // OpenAI-Codex_v1_removal_review: Preserve the old write set for owner preflight.
        let old_owner_targets: Vec<PathBuf> = old_targets
            .iter()
            .map(|(target, _)| PathBuf::from(target))
            .collect();
        let mut old_paths = std::collections::HashSet::new();
        let planned_keys: std::collections::HashSet<PathBuf> = targets
            .iter()
            .map(|path| rollback_target_key(path))
            .collect();
        for (target, backup) in old_targets {
            // OpenAI-Codex: Uninstall touches every old target, including files
            // absent from the update, so validate all recorded backups first.
            if let Some(backup) = backup.filter(|_| status != "disabled") {
                if !std::fs::symlink_metadata(&backup)?.file_type().is_file() {
                    return Err(CoreError::UnsupportedFormat {
                        reason: format!("backup is not a regular file: {backup}"),
                    });
                }
                let key = rollback_target_key(Path::new(&target));
                if planned_keys.contains(&key) {
                    expected_restores.insert(key, hash_file(Path::new(&backup))?);
                }
            }
            match std::fs::canonicalize(&target) {
                Ok(path) => {
                    old_paths.insert(rollback_target_key(&path));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        for file in &plan.files {
            let source = std::fs::canonicalize(&file.source)?;
            if old_paths.contains(&rollback_target_key(&source)) {
                return Err(CoreError::UnsupportedFormat {
                    reason: format!("the new package uses a currently deployed file as its source: {}; choose the original package instead", file.source.display()),
                });
            }
        }
        if status != "disabled" {
            // OpenAI-Codex_v1_removal_review: Check old active targets before intent.
            let mut old_owner_report = conflict::analyze(conn, &old_owner_targets)?;
            old_owner_report
                .foreign_conflicts
                .retain(|conflict| conflict.owner_mod_id != Some(mod_id));
            refuse_active_owner_conflicts(&old_owner_report)?;
        }
    }

    // OpenAI-Codex_v1_readiness: Refuse required backup before uninstall intent.
    if options.auto_backup && reinstall_requires_backup(conn, mod_id, &status, &plan)? {
        crate::full_backup::ensure_backup_available()?;
    }
    // OpenAI-Codex: One compound intent spans both versions, including partial failure.
    let mut packs = dlc_manual::pack_names_for_paths(game_root, &targets)?;
    packs.extend(dlc_manual::pack_names_for_mod(conn, mod_id, game_root)?);
    let manual_operation = dlc_manual::begin_tasks(
        conn,
        game_root,
        &packs,
        ManualDlcOperation::Reinstall,
        Some(mod_id),
    )?;
    if status != "uninstalled" {
        crate::uninstall::uninstall_for_reinstall(
            conn,
            mod_id,
            game_root,
            recycle_bin_root,
            staging_root,
            &manual_operation,
        )?;
    }

    let new_name = format!("{old_name} ({version_label})");
    install_with_expected_restores(
        conn,
        &new_name,
        &plan,
        game_root,
        backup_root,
        options,
        new_source_path,
        &expected_restores,
        Some(&manual_operation),
        preserved_mode,
        None,
    )
}

fn tx_log_backup_path_for(tx_log: &InstallTransaction, target: &Path) -> Option<PathBuf> {
    tx_log.actions.iter().find_map(|action| match action {
        InstallAction::FileOverwritten { target: t, backup } if t == target => Some(backup.clone()),
        _ => None,
    })
}

fn perform_writes(
    plan: &ModPlan,
    backup_root: &Path,
    auto_backup: bool,
    tx_log: &mut InstallTransaction,
) -> CoreResult<std::collections::HashMap<PathBuf, String>> {
    let mut hashes = std::collections::HashMap::new();

    for (index, planned) in plan.files.iter().enumerate() {
        protected_files::check_write(&planned.target)?;

        if let Some(parent) = planned.target.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if planned.target.exists() {
            if auto_backup {
                // OpenAI-Codex_v1_readiness: Stop if a target appeared after preflight.
                crate::full_backup::ensure_backup_available()?;
                std::fs::create_dir_all(backup_root)?;
                let backup_path = backup_root.join(format!("{index}.bak"));
                std::fs::copy(&planned.target, &backup_path)?;
                tx_log.record(InstallAction::FileOverwritten {
                    target: planned.target.clone(),
                    backup: backup_path,
                });
            }
            // If auto_backup is off, the user has explicitly opted out of the
            // safety net (see the MVP spec's default-on warning requirement) — the
            // pre-existing file is overwritten with no recorded original to restore.
        } else {
            tx_log.record(InstallAction::FileWritten {
                target: planned.target.clone(),
            });
        }

        std::fs::copy(&planned.source, &planned.target)?;
        let hash = hash_file(&planned.target)?;
        hashes.insert(planned.target.clone(), hash);
    }

    Ok(hashes)
}

fn rollback_failed_install(
    conn: &Connection,
    tx_log: InstallTransaction,
    err: CoreError,
) -> CoreError {
    let rollback_result = tx_log.rollback();
    let _ = record_failed_event(conn, None, &err);
    match rollback_result {
        Ok(()) => err,
        Err(rollback_error) => CoreError::InstallRollbackFailed {
            install_error: Box::new(err),
            rollback_error: Box::new(rollback_error),
        },
    }
}

fn record_failed_event(
    conn: &Connection,
    installed_mod_id: Option<i64>,
    err: &CoreError,
) -> CoreResult<()> {
    conn.execute(
        "INSERT INTO install_event (installed_mod_id, event_type, success, error_message) \
         VALUES (?1, 'install', 0, ?2)",
        rusqlite::params![installed_mod_id, err.to_string()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ModeProvider;

    // OpenAI-Codex_lml_generic_guards; OpenAI-Codex: Additional source-only coverage.
    #[test]
    fn recycle_restore_refuses_tracked_lml_before_reading_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        let snapshot = dir.path().join("snapshot");
        let backup = dir.path().join("backup");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&snapshot).unwrap();
        std::fs::write(snapshot.join("payload"), b"preserved").unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        conn.execute("INSERT INTO installed_mod(name,source_type,install_path,status,mode) VALUES('LML snapshot','folder',?1,'uninstalled','legacy-sp')", [root.to_str().unwrap()]).unwrap();
        let id = conn.last_insert_rowid();
        conn.execute("INSERT INTO installed_mod_file(installed_mod_id,target_path,file_hash) VALUES(?1,?2,?3)", rusqlite::params![id, root.join("lml/pkg/payload").to_str().unwrap(), "a".repeat(64)]).unwrap();
        // Invalid metadata proves refusal precedes snapshot parsing and filesystem restoration.
        conn.execute("INSERT INTO recycle_bin_entry(original_installed_mod_id,mod_package_snapshot_path,expires_at,snapshot_metadata) VALUES(?1,?2,'2999-01-01','invalid JSON')", rusqlite::params![id, snapshot.to_str().unwrap()]).unwrap();
        let entry = conn.last_insert_rowid();
        let error = crate::recycle_bin::restore(&mut conn, entry, &root, &backup).unwrap_err();
        assert!(error.to_string().contains("LML metadata"));
        assert_eq!(
            std::fs::read(snapshot.join("payload")).unwrap(),
            b"preserved"
        );
        assert!(!backup.exists());
        assert!(!root.join("lml").exists());
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM recycle_bin_entry WHERE id=?1",
                [entry],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("SELECT status FROM installed_mod WHERE id=?1", [id], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "uninstalled"
        );
    }

    #[test]
    fn handmade_lspdfr_plan_blocks_lml_and_preserves_normal_plugin_install() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir(&root).unwrap();
        let provider = crate::providers::LegacyLspdfrProvider::new(root.clone());
        let source = write_source(dir.path(), "plugin.dll", b"synthetic plugin payload");
        let backup = dir.path().join("backup");
        let mut conn = crate::db::open_in_memory().unwrap();
        let mut plan = ModPlan::new(
            ModFormat::FolderReplacer,
            vec![crate::mod_analyzer::PlannedFile {
                source: source.clone(),
                target: provider.game_root().join("lml/pkg/plugin.dll"),
            }],
        );
        assert!(install(
            &mut conn,
            "Blocked",
            &plan,
            &root,
            &backup,
            InstallOptions::default(),
            &source
        )
        .is_err());
        assert!(!backup.exists());
        assert!(!root.join("lml").exists());
        let target = provider.game_root().join("Plugins/LSPDFR/plugin.dll");
        plan.files[0].target = target.clone();
        assert!(matches!(
            install(
                &mut conn,
                "Normal",
                &plan,
                &root,
                &backup,
                InstallOptions::default(),
                &source
            )
            .unwrap(),
            InstallOutcome::Success { .. }
        ));
        assert_eq!(std::fs::read(target).unwrap(), b"synthetic plugin payload");
    }

    // OpenAI-Codex_lml_generic_guards; OpenAI-Codex: Caller-built plans cannot bypass LML guards.
    #[test]
    fn handmade_lml_target_refuses_before_backups_or_database_changes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir(&root).unwrap();
        let provider = crate::providers::LegacySpProvider::new(root.clone());
        let source = write_source(dir.path(), "mod.asi", b"payload");
        let mut plan = crate::mod_analyzer::classify(&source, &provider).unwrap();
        plan.files[0].target = root.join("lml/pkg/mod.asi");
        let mut conn = crate::db::open_in_memory().unwrap();
        let backup = dir.path().join("backup");
        assert!(install(
            &mut conn,
            "Blocked",
            &plan,
            &root,
            &backup,
            InstallOptions::default(),
            &source
        )
        .is_err());
        assert!(!backup.exists());
        assert!(!root.join("lml").exists());
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM installed_mod", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn tracked_lml_paths_refuse_state_uninstall_and_reinstall_before_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir(&root).unwrap();
        let provider = crate::providers::LegacySpProvider::new(root.clone());
        let source = write_source(dir.path(), "mod.asi", b"payload");
        let plan = crate::mod_analyzer::classify(&source, &provider).unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let backup = dir.path().join("backup");
        let outcome = install(
            &mut conn,
            "Normal",
            &plan,
            &root,
            &backup,
            InstallOptions::default(),
            &source,
        )
        .unwrap();
        let InstallOutcome::Success {
            installed_mod_id: id,
            ..
        } = outcome
        else {
            panic!("expected install")
        };
        let target = root.join("lml/pkg/payload.asi");
        conn.execute(
            "UPDATE installed_mod_file SET target_path=?1 WHERE installed_mod_id=?2",
            rusqlite::params![target.to_str().unwrap(), id],
        )
        .unwrap();
        let stage = dir.path().join("stage");
        let recycle = dir.path().join("recycle");
        assert!(crate::state::disable(&conn, id, &stage).is_err());
        assert!(matches!(
            crate::state::disable(&conn, id, &stage),
            Err(CoreError::ModDisablingUnavailable)
        ));
        assert!(matches!(
            crate::lml_store::ensure_generic_mod_allowed(&conn, id),
            Err(CoreError::UnsupportedFormat { reason })
                if reason == "LML metadata: generic target overlaps an LML reservation"
        ));
        assert!(crate::state::enable(&conn, id, &stage).is_err());
        assert!(crate::uninstall::uninstall(&mut conn, id, &root, &recycle).is_err());
        assert!(reinstall(
            &mut conn,
            id,
            &source,
            "v2",
            &provider,
            &root,
            &backup,
            &recycle,
            InstallOptions::default()
        )
        .is_err());
        let source2 = write_source(dir.path(), "normal.asi", b"normal");
        let plan2 = crate::mod_analyzer::classify(&source2, &provider).unwrap();
        let normal = install(
            &mut conn,
            "Normal2",
            &plan2,
            &root,
            &backup,
            InstallOptions::default(),
            &source2,
        )
        .unwrap();
        let InstallOutcome::Success {
            installed_mod_id: normal_id,
            ..
        } = normal
        else {
            panic!("expected install")
        };
        let old_profile = crate::profile::create(&conn, "Old").unwrap();
        let new_profile = crate::profile::create(&conn, "New").unwrap();
        crate::profile::add_mod(&conn, old_profile, normal_id).unwrap();
        crate::profile::add_mod(&conn, new_profile, id).unwrap();
        // OpenAI-Codex_disable_ai_audit: Confirm the LML guard stops this switch.
        let switch_result = crate::profile::switch(&conn, new_profile, &stage);
        assert!(switch_result.is_err());
        assert!(matches!(
            switch_result,
            Err(CoreError::UnsupportedFormat { reason })
                if reason == "LML metadata: generic target overlaps an LML reservation"
        ));
        assert_eq!(std::fs::read(root.join("normal.asi")).unwrap(), b"normal");
        assert_eq!(
            conn.query_row(
                "SELECT status FROM installed_mod WHERE id=?1",
                [normal_id],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "active"
        );
        assert!(!stage.exists());
        assert!(!recycle.exists());
        assert_eq!(std::fs::read(root.join("mod.asi")).unwrap(), b"payload");
        assert_eq!(
            conn.query_row("SELECT status FROM installed_mod WHERE id=?1", [id], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "active"
        );
    }

    // OpenAI-Codex: Disabled reinstall preserves recoverable staging on refusal.
    #[test]
    fn disabled_reinstall_requires_context_and_preserves_staging_on_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir(&root).unwrap();
        let provider = crate::providers::LegacySpProvider::new(root.clone());
        let source = write_source(dir.path(), "mod.asi", b"old payload");
        let plan = crate::mod_analyzer::classify(&source, &provider).unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let backups = dir.path().join("backups");
        let recycle = dir.path().join("recycle");
        let staging = dir.path().join("staging");
        let outcome = install(
            &mut conn,
            "Old",
            &plan,
            &root,
            &backups,
            InstallOptions::default(),
            &source,
        )
        .unwrap();
        let InstallOutcome::Success {
            installed_mod_id: id,
            ..
        } = outcome
        else {
            panic!("expected install success")
        };
        let fixture_stage = staging.join(id.to_string());
        std::fs::create_dir_all(&fixture_stage).unwrap();
        std::fs::rename(root.join("mod.asi"), fixture_stage.join("0")).unwrap();
        crate::uninstall::seed_disabled_fixture_from_staging(&conn, id, &staging);
        std::fs::write(root.join("mod.asi"), b"current original").unwrap();
        assert!(reinstall(
            &mut conn,
            id,
            &source,
            "v2",
            &provider,
            &root,
            &backups,
            &recycle,
            InstallOptions::default()
        )
        .is_err());
        let refusal = reinstall_with_staging(
            &mut conn,
            id,
            &source,
            "v2",
            &provider,
            &root,
            &backups,
            &recycle,
            InstallOptions::default(),
            &staging,
        )
        .unwrap();
        assert!(matches!(refusal, InstallOutcome::RequiresOverride(_)));
        assert_eq!(
            std::fs::read(root.join("mod.asi")).unwrap(),
            b"current original"
        );
        assert_eq!(
            std::fs::read(staging.join(id.to_string()).join("0")).unwrap(),
            b"old payload"
        );
        assert!(!recycle.exists());
        let status: String = conn
            .query_row(
                "SELECT status FROM installed_mod WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "disabled");
        let result = reinstall_with_staging(
            &mut conn,
            id,
            &source,
            "v2",
            &provider,
            &root,
            &backups,
            &recycle,
            InstallOptions {
                override_foreign_conflicts: true,
                ..Default::default()
            },
            &staging,
        )
        .unwrap();
        assert!(matches!(result, InstallOutcome::Success { .. }));
        assert_eq!(std::fs::read(root.join("mod.asi")).unwrap(), b"old payload");
        assert_eq!(crate::recycle_bin::list(&conn).unwrap().len(), 1);
    }
    use crate::mod_analyzer::PlannedFile;

    #[test]
    fn refused_install_outcomes_do_not_become_success_for_command_callers() {
        let conflict = conflict::ForeignConflict {
            owner_mod_id: Some(42),
            owner_name: "Existing".to_string(),
            path: PathBuf::from("shared.asi"),
        };
        let report = conflict::ConflictReport {
            foreign_conflicts: vec![conflict],
            ..Default::default()
        };
        assert!(matches!(
            InstallOutcome::RequiresOverride(report).require_success(),
            Err(CoreError::ForeignConflict {
                owner_mod_id: 42,
                ..
            })
        ));
        let untracked = conflict::ConflictReport {
            foreign_conflicts: vec![conflict::ForeignConflict {
                owner_mod_id: None,
                owner_name: "Unknown".to_string(),
                path: PathBuf::from("manual.asi"),
            }],
            ..Default::default()
        };
        assert!(matches!(
            InstallOutcome::RequiresOverride(untracked).require_success(),
            Err(CoreError::UntrackedFileConflict { .. })
        ));
        assert!(matches!(
            InstallOutcome::ProtectedFileBlocked(vec![PathBuf::from("GTA5.exe")]).require_success(),
            Err(CoreError::ProtectedFileViolation { .. })
        ));
        assert!(InstallOutcome::Success {
            installed_mod_id: 1,
            files_written: 1
        }
        .require_success()
        .is_ok());
    }

    fn write_source(dir: &Path, name: &str, contents: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn untracked_existing_file_is_preserved_until_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir(&game_root).unwrap();
        let target = write_source(&game_root, "existing.asi", b"manual mod");
        let source = write_source(dir.path(), "new.asi", b"new mod");
        let plan = ModPlan::new(
            ModFormat::Asi,
            vec![PlannedFile {
                source: source.clone(),
                target: target.clone(),
            }],
        );
        let mut conn = crate::db::open_in_memory().unwrap();
        let backup = dir.path().join("backups");
        let outcome = install(
            &mut conn,
            "New",
            &plan,
            &game_root,
            &backup,
            InstallOptions::default(),
            &source,
        )
        .unwrap();
        let InstallOutcome::RequiresOverride(report) = outcome else {
            panic!("expected confirmation");
        };
        assert_eq!(report.foreign_conflicts[0].owner_mod_id, None);
        assert_eq!(std::fs::read(&target).unwrap(), b"manual mod");
        assert!(!backup.exists());
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM installed_mod", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn changed_restored_content_cannot_use_the_reinstall_exemption() {
        let dir = tempfile::tempdir().unwrap();
        let target = write_source(dir.path(), "target.asi", b"expected original");
        let mut expected = std::collections::HashMap::new();
        expected.insert(rollback_target_key(&target), hash_file(&target).unwrap());
        std::fs::write(&target, b"changed externally").unwrap();
        let mut report = conflict::analyze(
            &crate::db::open_in_memory().unwrap(),
            std::slice::from_ref(&target),
        )
        .unwrap();
        retain_unexpected_conflicts(&mut report, &expected).unwrap();
        assert!(report.requires_explicit_override());
        assert_eq!(std::fs::read(target).unwrap(), b"changed externally");
    }

    #[test]
    fn known_owner_cannot_use_a_matching_restored_content_exemption() {
        let conn = crate::db::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let target = write_source(dir.path(), "target.asi", b"matching content");
        conn.execute("INSERT INTO installed_mod (name, source_type, install_path, status) VALUES ('Other', 'asi', '', 'active')", []).unwrap();
        let id = conn.last_insert_rowid();
        conn.execute("INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) VALUES (?1, ?2, 'hash')", rusqlite::params![id, target.to_string_lossy().as_ref()]).unwrap();
        let mut expected = std::collections::HashMap::new();
        expected.insert(rollback_target_key(&target), hash_file(&target).unwrap());
        let mut report = conflict::analyze(&conn, &[target]).unwrap();
        retain_unexpected_conflicts(&mut report, &expected).unwrap();
        assert!(report.requires_explicit_override());
        assert_eq!(report.foreign_conflicts[0].owner_mod_id, Some(id));
    }

    #[test]
    fn reinstall_recognizes_only_its_own_restored_original() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir(&game_root).unwrap();
        write_source(&game_root, "mod.asi", b"original game file");
        let old_source = write_source(dir.path(), "mod.asi", b"old mod");
        let provider = crate::providers::LegacySpProvider::new(game_root.clone());
        let plan = crate::mod_analyzer::classify(&old_source, &provider).unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let first = install(
            &mut conn,
            "Old",
            &plan,
            &game_root,
            &dir.path().join("old-backups"),
            InstallOptions {
                override_foreign_conflicts: true,
                ..Default::default()
            },
            &old_source,
        )
        .unwrap();
        let InstallOutcome::Success {
            installed_mod_id, ..
        } = first
        else {
            panic!("expected installation");
        };
        let update_dir = dir.path().join("update");
        std::fs::create_dir(&update_dir).unwrap();
        let source = write_source(&update_dir, "mod.asi", b"new mod");
        let new_backup = dir.path().join("new-backups");
        let recycle = dir.path().join("recycle");
        let outcome = reinstall(
            &mut conn,
            installed_mod_id,
            &source,
            "v2",
            &provider,
            &game_root,
            &new_backup,
            &recycle,
            InstallOptions::default(),
        )
        .unwrap();
        // OpenAI-Codex_v1_removal_review: The original version was recycled.
        let InstallOutcome::Success {
            installed_mod_id: new_id,
            ..
        } = outcome
        else {
            panic!("expected successful reinstall");
        };
        let old_status: String = conn
            .query_row(
                "SELECT status FROM installed_mod WHERE id = ?1",
                [installed_mod_id],
                |row| row.get(0),
            )
            .unwrap();
        let new_status: String = conn
            .query_row(
                "SELECT status FROM installed_mod WHERE id = ?1",
                [new_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_status, "uninstalled");
        assert_eq!(new_status, "active");
        let entries = crate::recycle_bin::list(&conn).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].original_installed_mod_id, Some(installed_mod_id));
        let snapshot: String = conn
            .query_row(
                "SELECT mod_package_snapshot_path FROM recycle_bin_entry WHERE id = ?1",
                [entries[0].id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            std::fs::read(Path::new(&snapshot).join("mod.asi")).unwrap(),
            b"old mod"
        );
        assert_eq!(
            std::fs::read(game_root.join("mod.asi")).unwrap(),
            b"new mod"
        );
        assert_eq!(
            std::fs::read(new_backup.join("0.bak")).unwrap(),
            b"original game file"
        );
    }

    #[test]
    fn missing_recorded_backup_blocks_reinstall_before_removing_unrelated_old_target() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir(&game_root).unwrap();
        let target = write_source(&game_root, "old.asi", b"old mod");
        let source = write_source(dir.path(), "new.asi", b"new mod");
        let provider = crate::providers::LegacySpProvider::new(game_root.clone());
        let mut conn = crate::db::open_in_memory().unwrap();
        conn.execute("INSERT INTO installed_mod (name, source_type, install_path, status) VALUES ('Old', 'asi', ?1, 'active')", [game_root.to_string_lossy().as_ref()]).unwrap();
        let id = conn.last_insert_rowid();
        conn.execute("INSERT INTO installed_mod_file (installed_mod_id, target_path, backup_path, file_hash) VALUES (?1, ?2, ?3, 'hash')", rusqlite::params![id, target.to_string_lossy().as_ref(), dir.path().join("missing.bak").to_string_lossy().as_ref()]).unwrap();
        let recycle = dir.path().join("recycle");
        let result = reinstall(
            &mut conn,
            id,
            &source,
            "v2",
            &provider,
            &game_root,
            &dir.path().join("backups"),
            &recycle,
            InstallOptions::default(),
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"old mod");
        assert!(!recycle.exists());
    }

    #[test]
    fn rollback_regression_continues_after_multiple_failures() {
        let dir = tempfile::tempdir().unwrap();
        let removable = write_source(dir.path(), "removable.asi", b"new");
        let blocked = dir.path().join("blocked-directory");
        std::fs::create_dir(&blocked).unwrap();
        let missing_backup = dir.path().join("missing-backup.asi");
        let mut tx = InstallTransaction::default();
        tx.record(InstallAction::FileWritten {
            target: removable.clone(),
        });
        tx.record(InstallAction::FileOverwritten {
            target: dir.path().join("missing-target.asi"),
            backup: missing_backup,
        });
        tx.record(InstallAction::FileWritten {
            target: blocked.clone(),
        });

        let err = tx.rollback().unwrap_err();
        assert!(!removable.exists(), "later rollback actions must still run");
        let details = format!("{err:?}");
        assert!(details.contains("blocked-directory"), "{details}");
        assert!(details.contains("missing-backup.asi"), "{details}");
    }

    #[test]
    fn rollback_regression_preserves_target_when_backup_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let target = write_source(dir.path(), "existing.asi", b"current");
        let mut tx = InstallTransaction::default();
        tx.record(InstallAction::FileOverwritten {
            target: target.clone(),
            backup: dir.path().join("missing-backup.asi"),
        });

        assert!(tx.rollback().is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"current");
    }

    #[cfg(windows)]
    #[test]
    fn atomic_regression_restoring_backup_preserves_a_target_on_failure() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = tempfile::tempdir().unwrap();
        let target = write_source(dir.path(), "existing.asi", b"current");
        let backup = write_source(dir.path(), "backup.asi", b"original");
        let locked_file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1 | 0x4)
            .open(&target)
            .unwrap();
        let mut tx = InstallTransaction::default();
        tx.record(InstallAction::FileOverwritten {
            target: target.clone(),
            backup: backup.clone(),
        });

        let result = tx.rollback();
        drop(locked_file);
        if result.is_err() {
            assert_eq!(std::fs::read(&target).unwrap(), b"current");
            assert_eq!(std::fs::read(&backup).unwrap(), b"original");
        } else {
            assert_eq!(std::fs::read(&target).unwrap(), b"original");
        }
    }

    #[cfg(windows)]
    #[test]
    fn atomic_regression_locked_backup_does_not_destroy_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = write_source(dir.path(), "existing.asi", b"current");
        let backup = write_source(dir.path(), "backup.asi", b"original");
        let locked_backup = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&backup)
            .unwrap();
        locked_backup.try_lock().unwrap();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let unlocker = std::thread::spawn(move || {
            let signaled = release_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .is_ok();
            locked_backup.unlock().unwrap();
            signaled
        });
        let mut tx = InstallTransaction::default();
        tx.record(InstallAction::FileOverwritten {
            target: target.clone(),
            backup: backup.clone(),
        });

        let result = tx.rollback();
        let _ = release_tx.send(());
        assert!(unlocker.join().unwrap(), "backup lock fixture timed out");
        assert!(result.is_err(), "locked backup must fail to copy");
        assert_eq!(std::fs::read(&backup).unwrap(), b"original");
        assert_eq!(std::fs::read(&target).unwrap(), b"current");
    }

    #[test]
    fn rollback_regression_skips_earlier_actions_for_failed_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = write_source(dir.path(), "duplicate.asi", b"current");
        let unrelated = write_source(dir.path(), "unrelated.asi", b"new");
        let mut tx = InstallTransaction::default();
        tx.record(InstallAction::FileWritten {
            target: unrelated.clone(),
        });
        tx.record(InstallAction::FileWritten {
            target: target.clone(),
        });
        tx.record(InstallAction::FileOverwritten {
            target: target.clone(),
            backup: dir.path().join("missing-backup.asi"),
        });

        let err = tx.rollback().unwrap_err();
        assert_eq!(std::fs::read(&target).unwrap(), b"current");
        assert!(
            !unrelated.exists(),
            "unrelated rollback actions must continue"
        );
        assert!(format!("{err:?}").contains("duplicate.asi"));
    }

    #[cfg(windows)]
    #[test]
    fn rollback_regression_recognizes_case_aliases_for_failed_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = write_source(dir.path(), "duplicate.asi", b"current");
        let mut tx = InstallTransaction::default();
        tx.record(InstallAction::FileWritten {
            target: target.clone(),
        });
        tx.record(InstallAction::FileOverwritten {
            target: dir.path().join("DUPLICATE.ASI"),
            backup: dir.path().join("missing-backup.asi"),
        });

        assert!(tx.rollback().is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"current");
    }

    #[test]
    fn rollback_regression_leaves_unreadable_dlclist_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let dlclist_path = dir.path().join("dlclist.xml");
        std::fs::write(&dlclist_path, [0xff]).unwrap();
        let target = write_source(dir.path(), "new-pack.rpf", b"payload");
        let mut tx = InstallTransaction::default();
        tx.record(InstallAction::FileWritten {
            target: target.clone(),
        });

        tx.rollback().unwrap();
        assert!(!target.exists());
        assert_eq!(std::fs::read(dlclist_path).unwrap(), [0xff]);
    }

    #[test]
    fn rollback_regression_preserves_preexisting_dlclist_entry() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        let dlclist_path = dlc::dlclist_path(&game_root);
        std::fs::create_dir_all(dlclist_path.parent().unwrap()).unwrap();
        std::fs::write(
            &dlclist_path,
            "<Paths>\n<Item>dlcpacks:\\MyAddonCar\\</Item>\n</Paths>\n",
        )
        .unwrap();
        let source = write_source(dir.path(), "dlc.rpf", b"payload");
        let target = game_root.join("mods/update/x64/dlcpacks/MyAddonCar/dlc.rpf");
        let plan = ModPlan::new(
            ModFormat::AddOnPack {
                pack_name: "MyAddonCar".to_string(),
            },
            vec![PlannedFile {
                source: source.clone(),
                target: target.clone(),
            }],
        );
        let mut conn = crate::db::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TRIGGER force_installed_mod_file_failure \
             BEFORE INSERT ON installed_mod_file \
             BEGIN SELECT RAISE(ABORT, 'forced'); END;",
        )
        .unwrap();

        assert!(install(
            &mut conn,
            "My Addon Car",
            &plan,
            &game_root,
            &dir.path().join("backups"),
            InstallOptions::default(),
            &source,
        )
        .is_err());
        assert!(!target.exists(), "new mod file must be rolled back");
        assert!(dlc::has_entry(&dlclist_path, "MyAddonCar").unwrap());
        let tasks = dlc_manual::list_tasks(&conn, Some(&game_root)).unwrap();
        assert_eq!(tasks.len(), 1);
        assert!(
            tasks[0].unresolved,
            "DB failure must retain the pre-write intent"
        );
    }

    #[test]
    fn rollback_regression_preserves_other_entries_in_single_line_dlclist() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        let dlclist_path = dlc::dlclist_path(&game_root);
        std::fs::create_dir_all(dlclist_path.parent().unwrap()).unwrap();
        std::fs::write(
            &dlclist_path,
            "<Paths><Item>dlcpacks:\\Existing\\</Item></Paths>",
        )
        .unwrap();
        let source = write_source(dir.path(), "dlc.rpf", b"payload");
        let plan = ModPlan::new(
            ModFormat::AddOnPack {
                pack_name: "MyAddonCar".to_string(),
            },
            vec![PlannedFile {
                source: source.clone(),
                target: game_root.join("mods/update/x64/dlcpacks/MyAddonCar/dlc.rpf"),
            }],
        );
        let mut conn = crate::db::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TRIGGER force_installed_mod_file_failure \
             BEFORE INSERT ON installed_mod_file \
             BEGIN SELECT RAISE(ABORT, 'forced'); END;",
        )
        .unwrap();

        assert!(install(
            &mut conn,
            "My Addon Car",
            &plan,
            &game_root,
            &dir.path().join("backups"),
            InstallOptions::default(),
            &source,
        )
        .is_err());
        let contents = std::fs::read_to_string(&dlclist_path).unwrap();
        assert!(contents.contains("<Paths>"));
        assert!(contents.contains("</Paths>"));
        assert!(dlc::has_entry(&dlclist_path, "Existing").unwrap());
        assert!(!dlc::has_entry(&dlclist_path, "MyAddonCar").unwrap());
        let tasks = dlc_manual::list_tasks(&conn, Some(&game_root)).unwrap();
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].unresolved);
    }

    #[cfg(windows)]
    #[test]
    fn rollback_regression_preserves_original_write_error() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let target = write_source(&game_root, "existing.asi", b"original");
        let source = write_source(dir.path(), "replacement.asi", b"replacement");
        let _locked_target = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1 | 0x2)
            .open(&target)
            .unwrap();
        let plan = ModPlan::new(
            ModFormat::Asi,
            vec![
                PlannedFile {
                    source: source.clone(),
                    target,
                },
                PlannedFile {
                    source: dir.path().join("missing-source.asi"),
                    target: game_root.join("new.asi"),
                },
            ],
        );
        let mut conn = crate::db::open_in_memory().unwrap();
        let err = install(
            &mut conn,
            "Write Failure Test",
            &plan,
            &game_root,
            &dir.path().join("backups"),
            InstallOptions {
                override_foreign_conflicts: true,
                ..Default::default()
            },
            &source,
        )
        .unwrap_err();

        let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(&err);
        let mut original_error_found = false;
        while let Some(error) = cause {
            if let Some(CoreError::Io(io_error)) = error.downcast_ref::<CoreError>() {
                if io_error.kind() == std::io::ErrorKind::NotFound {
                    original_error_found = true;
                    break;
                }
            }
            cause = error.source();
        }
        assert!(
            original_error_found,
            "original write error must remain accessible: {err:?}"
        );
        assert!(err.to_string().contains("rollback"));
        let failed_event_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM install_event WHERE success = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(failed_event_count, 1);
    }

    #[test]
    fn rollback_removes_newly_written_files() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("new_file.asi");
        std::fs::write(&target, b"payload").unwrap();

        let mut tx = InstallTransaction::default();
        tx.record(InstallAction::FileWritten {
            target: target.clone(),
        });
        tx.rollback().unwrap();

        assert!(!target.exists());
    }

    #[test]
    fn rollback_restores_overwritten_files_from_backup() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("existing.asi");
        let backup = dir.path().join("existing.asi.bak");
        std::fs::write(&backup, b"original").unwrap();
        std::fs::write(&target, b"new content that overwrote the original").unwrap();

        let mut tx = InstallTransaction::default();
        tx.record(InstallAction::FileOverwritten {
            target: target.clone(),
            backup: backup.clone(),
        });
        tx.rollback().unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"original");
        assert!(
            !backup.exists(),
            "backup file should be moved back, not left in place"
        );
    }

    #[test]
    fn database_write_failure_rolls_back_new_and_overwritten_files() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();

        let existing_target = game_root.join("existing.asi");
        std::fs::write(&existing_target, b"original").unwrap();
        let existing_source = write_source(dir.path(), "replacement.asi", b"replacement");
        let new_source = write_source(dir.path(), "new.asi", b"new payload");
        let new_target = game_root.join("new.asi");
        let plan = ModPlan::new(
            ModFormat::Asi,
            vec![
                PlannedFile {
                    source: existing_source.clone(),
                    target: existing_target.clone(),
                },
                PlannedFile {
                    source: new_source,
                    target: new_target.clone(),
                },
            ],
        );

        let mut conn = crate::db::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TRIGGER force_installed_mod_file_failure \
             BEFORE INSERT ON installed_mod_file \
             BEGIN SELECT RAISE(ABORT, 'forced'); END;",
        )
        .unwrap();

        let err = install(
            &mut conn,
            "Rollback Test",
            &plan,
            &game_root,
            &dir.path().join("backups"),
            InstallOptions {
                override_foreign_conflicts: true,
                ..Default::default()
            },
            &existing_source,
        )
        .unwrap_err();

        assert!(matches!(err, CoreError::Database(_)));
        assert!(!new_target.exists(), "new file should be removed");
        assert_eq!(
            std::fs::read(&existing_target).unwrap(),
            b"original",
            "overwritten file should be restored from its backup"
        );
        let installed_mod_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM installed_mod", [], |row| row.get(0))
            .unwrap();
        let installed_mod_file_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM installed_mod_file", [], |row| {
                row.get(0)
            })
            .unwrap();
        let failed_event_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM install_event WHERE success = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(installed_mod_count, 0);
        assert_eq!(installed_mod_file_count, 0);
        assert_eq!(failed_event_count, 1);
    }

    #[cfg(windows)]
    #[test]
    fn database_failure_reports_a_failed_filesystem_rollback() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let target = game_root.join("existing.asi");
        let backup_root = dir.path().join("backups");
        std::fs::write(&target, b"original").unwrap();
        let source = write_source(dir.path(), "replacement.asi", b"replacement");

        // Allow reads and writes, but prevent deletion while the install runs.
        let _locked_target = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1 | 0x2)
            .open(&target)
            .unwrap();
        let plan = ModPlan::new(
            ModFormat::Asi,
            vec![PlannedFile {
                source: source.clone(),
                target: target.clone(),
            }],
        );

        let mut conn = crate::db::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TRIGGER force_installed_mod_file_failure \
             BEFORE INSERT ON installed_mod_file \
             BEGIN SELECT RAISE(ABORT, 'forced database failure'); END;",
        )
        .unwrap();
        let err = install(
            &mut conn,
            "Rollback Failure Test",
            &plan,
            &game_root,
            &backup_root,
            InstallOptions {
                override_foreign_conflicts: true,
                ..Default::default()
            },
            &source,
        )
        .unwrap_err();

        let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(&err);
        let mut original_database_error_found = false;
        while let Some(error) = cause {
            if let Some(CoreError::Database(database_error)) = error.downcast_ref::<CoreError>() {
                assert!(database_error
                    .to_string()
                    .contains("forced database failure"));
                original_database_error_found = true;
                break;
            }
            cause = error.source();
        }
        assert!(
            original_database_error_found,
            "the original database error must remain accessible: {err:?}"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"replacement");
        assert_eq!(
            std::fs::read(backup_root.join("0.bak")).unwrap(),
            b"original"
        );
        let installed_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM installed_mod", [], |row| row.get(0))
            .unwrap();
        assert_eq!(installed_count, 0);
        let failed_event_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM install_event WHERE success = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(failed_event_count, 1);

        let message = err.to_string();
        assert!(
            message.contains("filesystem rollback") && message.contains("forced database failure"),
            "the caller must receive both errors: {message}"
        );
        assert!(
            message.contains("code: 32"),
            "the caller must receive the Windows rollback error details: {message}"
        );
    }

    // OpenAI-Codex; OpenAI-Codex_Responsible: Source_Code.
    #[test]
    fn empty_install_plans_preserve_state_and_error_priorities() {
        let temp = tempfile::tempdir().unwrap();
        let game_root = temp.path().join("game");
        let enhanced_root = temp.path().join("enhanced");
        let empty_package = temp.path().join("empty-package");
        let backup_root = temp.path().join("backups");
        std::fs::create_dir(&game_root).unwrap();
        std::fs::create_dir(&enhanced_root).unwrap();
        std::fs::create_dir(&empty_package).unwrap();
        std::fs::write(enhanced_root.join("GTA5_Enhanced.exe"), b"").unwrap();
        let existing = write_source(&game_root, "existing.asi", b"preserve");
        let mut conn = crate::db::open_in_memory().unwrap();
        let game_text = game_root.to_str().unwrap();
        conn.execute(
            "INSERT INTO installed_mod(name, source_type, install_path, status, mode) \
             VALUES ('Existing', 'asi', ?1, 'active', 'legacy-sp')",
            [game_text],
        )
        .unwrap();
        let existing_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO installed_mod_file(installed_mod_id, target_path, file_hash) \
             VALUES (?1, ?2, ?3)",
            rusqlite::params![
                existing_id,
                existing.to_str().unwrap(),
                hash_file(&existing).unwrap()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO installed_mod(name, source_type, install_path, status, mode) \
             VALUES ('Existing zero-file', 'folder', ?1, 'active', 'legacy-sp')",
            [game_text],
        )
        .unwrap();

        let provider = crate::providers::LegacySpProvider::new(game_root.clone());
        let classified = crate::mod_analyzer::classify(&empty_package, &provider).unwrap();
        assert!(matches!(&classified.format, ModFormat::FolderReplacer));
        assert!(classified.files.is_empty());
        crate::lml_store::ensure_generic_targets_allowed(&conn, &game_root, &[]).unwrap();
        let report = conflict::analyze(&conn, &[]).unwrap();
        assert!(!report.has_protected_hits());
        assert!(!report.requires_explicit_override());
        assert!(dlc_manual::pack_names_for_paths(&game_root, &[])
            .unwrap()
            .is_empty());

        let tree_before = snapshot_backup_tree(temp.path());
        let db_before = snapshot_backup_database(&conn);
        let changes_before: i64 = conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        let cases = [
            (
                "classified",
                classified,
                game_root.as_path(),
                InstallOptions::default(),
                "the install plan contains no files",
            ),
            (
                "caller-built",
                ModPlan::new(ModFormat::Zip, Vec::new()),
                game_root.as_path(),
                InstallOptions {
                    auto_backup: false,
                    override_foreign_conflicts: true,
                },
                "the install plan contains no files",
            ),
            (
                "unsupported",
                ModPlan::new(
                    ModFormat::Unsupported("preserved unsupported reason".to_string()),
                    Vec::new(),
                ),
                game_root.as_path(),
                InstallOptions::default(),
                "preserved unsupported reason",
            ),
            (
                "enhanced",
                ModPlan::new(ModFormat::FolderReplacer, Vec::new()),
                enhanced_root.as_path(),
                InstallOptions::default(),
                "GTA V Enhanced support is under development",
            ),
        ];
        for (case, plan, root, options, expected) in cases {
            let result = install(
                &mut conn,
                "Empty",
                &plan,
                root,
                &backup_root,
                options,
                &empty_package,
            );
            assert!(
                matches!(&result, Err(CoreError::UnsupportedFormat { reason })
                    if reason == expected),
                "{case}: {result:?}"
            );
            assert_eq!(snapshot_backup_tree(temp.path()), tree_before, "{case}");
            assert_eq!(snapshot_backup_database(&conn), db_before, "{case}");
            let changes_after: i64 = conn
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .unwrap();
            assert_eq!(changes_after, changes_before, "{case}");
            assert!(conn.is_autocommit());
            assert!(!backup_root.exists());
        }
    }

    #[test]
    fn successful_install_writes_file_and_commits_db_rows() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let source = write_source(dir.path(), "cool_mod.asi", b"payload");
        let backup_root = dir.path().join("backups");

        let plan = ModPlan::new(
            ModFormat::Asi,
            vec![PlannedFile {
                source: source.clone(),
                target: game_root.join("cool_mod.asi"),
            }],
        );

        let mut conn = crate::db::open_in_memory().unwrap();
        let outcome = install(
            &mut conn,
            "Cool Mod",
            &plan,
            &game_root,
            &backup_root,
            InstallOptions::default(),
            &source,
        )
        .unwrap();

        match outcome {
            InstallOutcome::Success {
                installed_mod_id,
                files_written,
            } => {
                assert_eq!(files_written, 1);
                let count: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM installed_mod_file WHERE installed_mod_id = ?1",
                        [installed_mod_id],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(count, 1);
            }
            other => panic!("expected Success, got {other:?}"),
        }
        assert!(game_root.join("cool_mod.asi").exists());
    }

    #[test]
    fn successful_install_records_the_source_path() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let source = write_source(dir.path(), "cool_mod.asi", b"payload");
        let backup_root = dir.path().join("backups");

        let plan = ModPlan::new(
            ModFormat::Asi,
            vec![PlannedFile {
                source: source.clone(),
                target: game_root.join("cool_mod.asi"),
            }],
        );

        let mut conn = crate::db::open_in_memory().unwrap();
        let outcome = install(
            &mut conn,
            "Cool Mod",
            &plan,
            &game_root,
            &backup_root,
            InstallOptions::default(),
            &source,
        )
        .unwrap();

        let InstallOutcome::Success {
            installed_mod_id, ..
        } = outcome
        else {
            panic!("expected Success");
        };
        let stored: String = conn
            .query_row(
                "SELECT source_path FROM installed_mod WHERE id = ?1",
                [installed_mod_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, source.to_string_lossy());
    }

    #[test]
    fn reinstall_uninstalls_the_old_version_then_installs_the_new_one() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let backup_root = dir.path().join("backups");
        let recycle_root = dir.path().join("recycle");
        let provider = crate::providers::LegacySpProvider::new(game_root.clone());

        let old_source = write_source(dir.path(), "cool_mod.asi", b"v1 payload");
        let plan = crate::mod_analyzer::classify(&old_source, &provider).unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let outcome = install(
            &mut conn,
            "Cool Mod",
            &plan,
            &game_root,
            &backup_root,
            InstallOptions::default(),
            &old_source,
        )
        .unwrap();
        let InstallOutcome::Success {
            installed_mod_id: old_id,
            ..
        } = outcome
        else {
            panic!("expected Success");
        };

        let new_source = write_source(dir.path(), "cool_mod_v2.asi", b"v2 payload");
        let outcome = reinstall(
            &mut conn,
            old_id,
            &new_source,
            "v2",
            &provider,
            &game_root,
            &backup_root,
            &recycle_root,
            InstallOptions::default(),
        )
        .unwrap();

        let InstallOutcome::Success {
            installed_mod_id: new_id,
            ..
        } = outcome
        else {
            panic!("expected Success, got {outcome:?}");
        };
        assert_ne!(old_id, new_id, "reinstall should create a fresh mod row");

        let old_status: String = conn
            .query_row(
                "SELECT status FROM installed_mod WHERE id = ?1",
                [old_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_status, "uninstalled");

        let (new_name, new_status): (String, String) = conn
            .query_row(
                "SELECT name, status FROM installed_mod WHERE id = ?1",
                [new_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(new_name, "Cool Mod (v2)");
        assert_eq!(new_status, "active");
        assert_eq!(
            std::fs::read(game_root.join("cool_mod_v2.asi")).unwrap(),
            b"v2 payload"
        );
    }

    struct CalloutReinstallProvider(crate::providers::LegacyLspdfrProvider);

    impl ModeProvider for CalloutReinstallProvider {
        fn game_root(&self) -> &Path {
            self.0.game_root()
        }
        fn resolve_asi_target(&self, name: &std::ffi::OsStr) -> PathBuf {
            self.0.resolve_asi_target(name)
        }
        fn resolve_native_dll_target(&self, name: &std::ffi::OsStr) -> PathBuf {
            self.0.resolve_native_dll_target(name)
        }
        fn resolve_managed_dll_target(&self, name: &std::ffi::OsStr) -> PathBuf {
            self.0.game_root().join("Plugins/LSPDFR").join(name)
        }
        fn resolve_menyoo_target(
            &self,
            category: crate::mod_analyzer::MenyooCategory,
            name: &std::ffi::OsStr,
        ) -> PathBuf {
            self.0.resolve_menyoo_target(category, name)
        }
        fn resolve_folder_replacer_target(&self, relative: &Path) -> PathBuf {
            self.0.resolve_folder_replacer_target(relative)
        }
        fn lspdfr_package_game_root(&self) -> Option<&Path> {
            self.0.lspdfr_package_game_root()
        }
        fn resolve_add_on_pack_target(&self, name: &str, relative: &Path) -> PathBuf {
            self.0.resolve_add_on_pack_target(name, relative)
        }
    }

    // OpenAI-Codex_lspdfr_role_core: Catch role loss before uninstall, even when
    // the original source or deployed payload no longer exists.
    #[test]
    fn lspdfr_reinstall_rejects_role_loss_before_changing_the_old_record() {
        for missing_target in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let game = dir.path().join("game");
            let old_target = game.join("Plugins/LSPDFR/old.dll");
            std::fs::create_dir_all(old_target.parent().unwrap()).unwrap();
            if !missing_target {
                std::fs::write(&old_target, b"old payload").unwrap();
            }
            let mut conn = crate::db::open_in_memory().unwrap();
            conn.execute(
                "INSERT INTO installed_mod (name, source_type, install_path, status, mode) VALUES ('Old callout', 'dll', ?1, 'active', 'legacy-lspdfr')",
                [game.to_string_lossy().as_ref()],
            ).unwrap();
            let id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) VALUES (?1, ?2, 'old')",
                rusqlite::params![id, old_target.to_string_lossy()],
            ).unwrap();
            let mut pe = vec![0u8; 0x180];
            pe[0..2].copy_from_slice(b"MZ");
            pe[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
            pe[0x40..0x44].copy_from_slice(b"PE\0\0");
            pe[0x58..0x5a].copy_from_slice(&0x20bu16.to_le_bytes());
            pe[0x138..0x13c].copy_from_slice(&1u32.to_le_bytes());
            pe[0x13c..0x140].copy_from_slice(&1u32.to_le_bytes());
            let source = write_source(dir.path(), "new.dll", &pe);
            let provider = crate::providers::LegacyLspdfrProvider::new(game.clone());
            let recycle = dir.path().join("recycle");
            let backup = dir.path().join("backup");
            let result = reinstall(
                &mut conn,
                id,
                &source,
                "v2",
                &provider,
                &game,
                &backup,
                &recycle,
                InstallOptions::default(),
            );
            assert!(matches!(result, Err(CoreError::UnsupportedFormat { .. })));
            let status: String = conn
                .query_row(
                    "SELECT status FROM installed_mod WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(status, "active");
            assert!(!game.join("Plugins/new.dll").exists());
            assert!(!backup.exists());
            assert!(!recycle.exists());
            if !missing_target {
                assert_eq!(std::fs::read(&old_target).unwrap(), b"old payload");
                let matching = CalloutReinstallProvider(provider);
                let outcome = reinstall(
                    &mut conn,
                    id,
                    &source,
                    "v2",
                    &matching,
                    &game,
                    &backup,
                    &recycle,
                    InstallOptions::default(),
                )
                .unwrap();
                let InstallOutcome::Success {
                    installed_mod_id: new_id,
                    ..
                } = outcome
                else {
                    panic!("expected a matching-role reinstall");
                };
                let new_mode: Option<String> = conn
                    .query_row(
                        "SELECT mode FROM installed_mod WHERE id = ?1",
                        [new_id],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(new_mode.as_deref(), Some("legacy-lspdfr"));
                assert_eq!(
                    std::fs::read(game.join("Plugins/LSPDFR/new.dll")).unwrap(),
                    pe
                );
                assert!(!game.join("Plugins/new.dll").exists());
                let ordinary = crate::providers::LegacyLspdfrProvider::new(game.clone());
                let second = reinstall(
                    &mut conn,
                    new_id,
                    &source,
                    "v3",
                    &ordinary,
                    &game,
                    &backup,
                    &recycle,
                    InstallOptions::default(),
                );
                assert!(matches!(second, Err(CoreError::UnsupportedFormat { .. })));
                assert_eq!(
                    std::fs::read(game.join("Plugins/LSPDFR/new.dll")).unwrap(),
                    pe
                );
                assert!(!game.join("Plugins/new.dll").exists());
                let status: String = conn
                    .query_row(
                        "SELECT status FROM installed_mod WHERE id = ?1",
                        [new_id],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(status, "active");
            }
        }
    }

    #[test]
    fn lspdfr_role_guard_accepts_matching_folder_and_preserves_other_formats() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path().join("game");
        let source = write_source(dir.path(), "new.dll", b"new");
        let conn = crate::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status, mode) VALUES ('Old', 'dll', ?1, 'active', 'legacy-lspdfr')",
            [game.to_string_lossy().as_ref()],
        ).unwrap();
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) VALUES (?1, ?2, 'old')",
            rusqlite::params![id, game.join("Plugins/LSPDFR/old.dll").to_string_lossy()],
        ).unwrap();
        let plan = ModPlan::new(
            ModFormat::ManagedDll,
            vec![PlannedFile {
                source: source.clone(),
                target: game.join("Plugins/LSPDFR/new.dll"),
            }],
        );
        assert!(preserve_lspdfr_reinstall_role(&conn, id, &source, &game, &plan).is_ok());
        #[cfg(unix)]
        {
            let linked_source = dir.path().join("linked.dll");
            std::os::unix::fs::symlink(&source, &linked_source).unwrap();
            let linked_plan = ModPlan::new(
                ModFormat::ManagedDll,
                vec![PlannedFile {
                    source: linked_source.clone(),
                    target: game.join("Plugins/LSPDFR/linked.dll"),
                }],
            );
            assert!(
                preserve_lspdfr_reinstall_role(&conn, id, &linked_source, &game, &linked_plan)
                    .is_err()
            );
        }
        let native_plan = ModPlan::new(ModFormat::NativeDll, plan.files.clone());
        assert!(preserve_lspdfr_reinstall_role(&conn, id, &source, &game, &native_plan).is_err());
        let folder = dir.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        assert!(preserve_lspdfr_reinstall_role(&conn, id, &folder, &game, &plan).is_err());
        let multi = ModPlan::new(ModFormat::ManagedDll, vec![plan.files[0].clone(); 2]);
        assert!(preserve_lspdfr_reinstall_role(&conn, id, &source, &game, &multi).is_err());
        let ordinary_plan = ModPlan::new(
            ModFormat::ManagedDll,
            vec![PlannedFile {
                source: source.clone(),
                target: game.join("Plugins/new.dll"),
            }],
        );
        for malformed in [
            game.join("Plugins/extra/old.dll"),
            game.join("Plugins/LSPDFR/../old.dll"),
            game.join("Plugins/LSPDFR/old.txt"),
            dir.path().join("other/Plugins/LSPDFR/old.dll"),
        ] {
            conn.execute(
                "UPDATE installed_mod_file SET target_path = ?1 WHERE installed_mod_id = ?2",
                rusqlite::params![malformed.to_string_lossy(), id],
            )
            .unwrap();
            assert!(
                preserve_lspdfr_reinstall_role(&conn, id, &source, &game, &ordinary_plan).is_err()
            );
        }
        conn.execute(
            "UPDATE installed_mod_file SET target_path = ?1 WHERE installed_mod_id = ?2",
            rusqlite::params![game.join("Plugins/old.dll").to_string_lossy(), id],
        )
        .unwrap();
        assert!(preserve_lspdfr_reinstall_role(&conn, id, &source, &game, &plan).is_err());
        conn.execute(
            "UPDATE installed_mod SET mode = 'legacy-sp' WHERE id = ?1",
            [id],
        )
        .unwrap();
        assert!(preserve_lspdfr_reinstall_role(&conn, id, &source, &game, &plan).is_ok());
        conn.execute("UPDATE installed_mod SET mode = NULL WHERE id = ?1", [id])
            .unwrap();
        assert!(preserve_lspdfr_reinstall_role(&conn, id, &source, &game, &plan).is_ok());
        conn.execute(
            "UPDATE installed_mod SET mode = 'legacy-lspdfr', source_type = 'zip' WHERE id = ?1",
            [id],
        )
        .unwrap();
        assert!(preserve_lspdfr_reinstall_role(&conn, id, &source, &game, &plan).is_ok());
        conn.execute(
            "UPDATE installed_mod SET source_type = 'dll' WHERE id = ?1",
            [id],
        )
        .unwrap();
        conn.execute(
            "UPDATE installed_mod_file SET target_path = ?1 WHERE installed_mod_id = ?2",
            rusqlite::params![game.join("old.dll").to_string_lossy(), id],
        )
        .unwrap();
        assert!(preserve_lspdfr_reinstall_role(&conn, id, &source, &game, &ordinary_plan).is_ok());
        conn.execute("INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) VALUES (?1, ?2, 'old')",
            rusqlite::params![id, game.join("Plugins/other.dll").to_string_lossy()]).unwrap();
        assert!(preserve_lspdfr_reinstall_role(&conn, id, &source, &game, &ordinary_plan).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn lspdfr_role_paths_do_not_merge_differently_cased_roots() {
        assert_ne!(
            lspdfr_role_path_key(Path::new(r"C:\Game\Plugins\old.dll")),
            lspdfr_role_path_key(Path::new(r"C:\game\Plugins\old.dll")),
        );
    }

    #[test]
    fn reinstall_unknown_mod_id_errors() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let provider = crate::providers::LegacySpProvider::new(game_root.clone());
        let source = write_source(dir.path(), "mod.asi", b"payload");
        let mut conn = crate::db::open_in_memory().unwrap();

        let err = reinstall(
            &mut conn,
            999,
            &source,
            "v2",
            &provider,
            &game_root,
            &dir.path().join("backups"),
            &dir.path().join("recycle"),
            InstallOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(err, CoreError::UnsupportedFormat { .. }));
    }

    #[test]
    fn reinstall_invalid_package_preserves_old_files_and_active_record() {
        for new_name in [
            "missing.asi",
            "unsupported.oiv",
            "deployed.asi",
            "directory.asi",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let game_root = dir.path().join("game");
            let provider = crate::providers::LegacySpProvider::new(game_root.clone());
            let source = write_source(dir.path(), "original.asi", b"original");
            let plan = crate::mod_analyzer::classify(&source, &provider).unwrap();
            let mut conn = crate::db::open_in_memory().unwrap();
            let backup = dir.path().join("backups");
            let outcome = install(
                &mut conn,
                "Original",
                &plan,
                &game_root,
                &backup,
                InstallOptions::default(),
                &source,
            )
            .unwrap();
            let InstallOutcome::Success {
                installed_mod_id, ..
            } = outcome
            else {
                panic!("expected initial installation");
            };
            let recycle = dir.path().join("recycle");
            let new_source = if new_name == "deployed.asi" {
                game_root.join("original.asi")
            } else {
                if new_name == "directory.asi" {
                    write_source(dir.path(), new_name, b"new");
                    std::fs::create_dir(game_root.join(new_name)).unwrap();
                }
                dir.path().join(new_name)
            };
            let result = reinstall(
                &mut conn,
                installed_mod_id,
                &new_source,
                "v2",
                &provider,
                &game_root,
                &backup,
                &recycle,
                InstallOptions::default(),
            );
            assert!(result.is_err(), "{new_name}");
            assert_eq!(
                std::fs::read(game_root.join("original.asi")).unwrap(),
                b"original"
            );
            let status: String = conn
                .query_row(
                    "SELECT status FROM installed_mod WHERE id = ?1",
                    [installed_mod_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(status, "active");
            assert!(!recycle.exists());
        }
    }

    // OpenAI-Codex_v1_removal_review: Check old target ownership before DLC intent.
    #[test]
    fn reinstall_refuses_other_owner_of_old_target_before_dlc_intent() {
        for new_pack in ["Shared", "Other"] {
            for foreign_present in [false, true] {
                for allow_override in [false, true] {
                    let dir = tempfile::tempdir().unwrap();
                    let root = dir.path().join("game");
                    std::fs::create_dir(&root).unwrap();
                    let provider = crate::providers::LegacySpProvider::new(root.clone());
                    let old_source = dir.path().join("v1/Shared");
                    std::fs::create_dir_all(&old_source).unwrap();
                    std::fs::write(old_source.join("dlc.rpf"), b"old payload").unwrap();
                    let mut conn = crate::db::open_in_memory().unwrap();
                    let old_plan = crate::mod_analyzer::classify(&old_source, &provider).unwrap();
                    let InstallOutcome::Success {
                        installed_mod_id: old_id,
                        ..
                    } = install(
                        &mut conn,
                        "Old",
                        &old_plan,
                        &root,
                        &dir.path().join("old-backups"),
                        InstallOptions::default(),
                        &old_source,
                    )
                    .unwrap()
                    else {
                        panic!("expected initial installation");
                    };
                    let old_target = root.join("mods/update/x64/dlcpacks/Shared/dlc.rpf");
                    assert_eq!(std::fs::read(&old_target).unwrap(), b"old payload");
                    let foreign_id = if foreign_present {
                        std::fs::write(&old_target, b"foreign payload").unwrap();
                        conn.execute(
                            "INSERT INTO installed_mod (name, source_type, install_path, status) VALUES ('Other', 'folder', ?1, 'active')",
                            [root.to_string_lossy().as_ref()],
                        )
                        .unwrap();
                        let owner = conn.last_insert_rowid();
                        conn.execute(
                            "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) VALUES (?1, ?2, ?3)",
                            rusqlite::params![
                                owner,
                                old_target.to_string_lossy().as_ref(),
                                hash_file(&old_target).unwrap(),
                            ],
                        )
                        .unwrap();
                        Some(owner)
                    } else {
                        None
                    };
                    let new_source = dir.path().join("v2").join(new_pack);
                    std::fs::create_dir_all(&new_source).unwrap();
                    std::fs::write(new_source.join("dlc.rpf"), b"new payload").unwrap();
                    let plan = crate::mod_analyzer::classify(&new_source, &provider).unwrap();
                    let new_target = root
                        .join("mods/update/x64/dlcpacks")
                        .join(new_pack)
                        .join("dlc.rpf");
                    assert_eq!(plan.files.len(), 1);
                    assert_eq!(plan.files[0].target, new_target);
                    assert_eq!(
                        dlc_manual::pack_names_for_paths(&root, std::slice::from_ref(&old_target))
                            .unwrap(),
                        vec!["Shared".to_string()]
                    );
                    assert_eq!(
                        dlc_manual::pack_names_for_paths(&root, std::slice::from_ref(&new_target))
                            .unwrap(),
                        vec![new_pack.to_string()]
                    );
                    let old_report =
                        conflict::analyze(&conn, std::slice::from_ref(&old_target)).unwrap();
                    assert!(old_report
                        .foreign_conflicts
                        .iter()
                        .any(|c| c.owner_mod_id == Some(old_id)));
                    if let Some(id) = foreign_id {
                        assert!(old_report
                            .foreign_conflicts
                            .iter()
                            .any(|c| c.owner_mod_id == Some(id)));
                    }
                    let targets: Vec<PathBuf> =
                        plan.files.iter().map(|file| file.target.clone()).collect();
                    let report = conflict::analyze(&conn, &targets).unwrap();
                    assert!(!report.has_protected_hits());
                    if new_pack == "Other" {
                        assert!(report.foreign_conflicts.is_empty());
                    }
                    let backup = dir.path().join("new-backups");
                    let recycle = dir.path().join("recycle");
                    assert!(
                        crate::providers::ensure_installed_mod_available(&conn, old_id).is_ok()
                    );
                    assert!(crate::providers::ensure_game_root_available(&root).is_ok());
                    assert!(crate::uninstall::preflight_recycle_snapshot_paths(
                        &conn, old_id, &root
                    )
                    .is_ok());
                    assert!(refuse_reinstall_sources_overlapping_old_paths(
                        &conn,
                        old_id,
                        "active",
                        std::slice::from_ref(&new_source)
                    )
                    .is_ok());
                    assert!(crate::lml_store::ensure_generic_targets_allowed(
                        &conn,
                        &root,
                        std::slice::from_ref(&backup)
                    )
                    .is_ok());
                    assert!(source_type_str(&plan.format).is_ok());
                    assert!(refuse_overlapping_planned_targets(&plan).is_ok());
                    assert!(
                        ensure_lml_backup_paths_allowed(&conn, &root, &backup, &plan, true).is_ok()
                    );
                    assert!(refuse_existing_backup_slots(&backup, &plan, true).is_ok());
                    assert!(refuse_backup_path_overlaps(&conn, &backup, &plan, true).is_ok());
                    assert!(refuse_targets_overlapping_recorded_backups(&conn, &plan).is_ok());
                    assert!(refuse_source_path_overlaps(&plan, &new_source, &backup, true).is_ok());
                    let sources: Vec<PathBuf> =
                        plan.files.iter().map(|file| file.source.clone()).collect();
                    assert!(refuse_reinstall_sources_overlapping_old_paths(
                        &conn, old_id, "active", &sources
                    )
                    .is_ok());
                    assert!(preserve_lspdfr_reinstall_role(
                        &conn,
                        old_id,
                        &new_source,
                        &root,
                        &plan
                    )
                    .unwrap()
                    .is_none());
                    assert!(crate::lml_store::ensure_generic_targets_allowed(
                        &conn, &root, &targets
                    )
                    .is_ok());
                    for file in &plan.files {
                        assert!(std::fs::metadata(&file.source).unwrap().is_file());
                        match std::fs::metadata(&file.target) {
                            Ok(metadata) => assert!(metadata.is_file()),
                            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
                        }
                    }
                    let intents_before: i64 = conn.query_row(
                        "SELECT COUNT(*) FROM manual_dlc_event WHERE operation = 'reinstall' AND phase = 'intent'",
                        [],
                        |row| row.get(0),
                    ).unwrap();
                    assert_eq!(intents_before, 0);
                    let tree_before = snapshot_backup_tree(dir.path());
                    let database_before = snapshot_backup_database(&conn);
                    let outcome = reinstall(
                        &mut conn,
                        old_id,
                        &new_source,
                        "v2",
                        &provider,
                        &root,
                        &backup,
                        &recycle,
                        InstallOptions {
                            override_foreign_conflicts: allow_override,
                            ..Default::default()
                        },
                    );
                    match (foreign_id, outcome) {
                        (
                            Some(expected_owner),
                            Err(CoreError::ForeignConflict { path, owner_mod_id }),
                        ) => {
                            assert_eq!(owner_mod_id, expected_owner);
                            assert_eq!(path, old_target);
                            assert_eq!(snapshot_backup_tree(dir.path()), tree_before);
                            assert_eq!(snapshot_backup_database(&conn), database_before);
                            assert!(!recycle.exists());
                            assert!(!backup.exists());
                            let intents_after: i64 = conn.query_row(
                                "SELECT COUNT(*) FROM manual_dlc_event WHERE operation = 'reinstall' AND phase = 'intent'",
                                [],
                                |row| row.get(0),
                            ).unwrap();
                            assert_eq!(intents_after, 0);
                            if new_pack == "Other" && !allow_override {
                                // OpenAI-Codex_v1_removal_review: The new-target prompt precedes the old-owner refusal.
                                std::fs::create_dir_all(new_target.parent().unwrap()).unwrap();
                                std::fs::write(&new_target, b"untracked target").unwrap();

                                let rechecked_plan =
                                    crate::mod_analyzer::classify(&new_source, &provider).unwrap();
                                assert_eq!(rechecked_plan.files.len(), 1);
                                assert_eq!(rechecked_plan.files[0].target, new_target);
                                assert!(std::fs::metadata(&rechecked_plan.files[0].source)
                                    .unwrap()
                                    .is_file());
                                assert!(std::fs::metadata(&new_target).unwrap().is_file());
                                assert!(source_type_str(&rechecked_plan.format).is_ok());
                                assert!(refuse_overlapping_planned_targets(&rechecked_plan).is_ok());
                                assert!(ensure_lml_backup_paths_allowed(
                                    &conn,
                                    &root,
                                    &backup,
                                    &rechecked_plan,
                                    true
                                )
                                .is_ok());
                                assert!(refuse_existing_backup_slots(
                                    &backup,
                                    &rechecked_plan,
                                    true
                                )
                                .is_ok());
                                assert!(refuse_backup_path_overlaps(
                                    &conn,
                                    &backup,
                                    &rechecked_plan,
                                    true
                                )
                                .is_ok());
                                assert!(refuse_targets_overlapping_recorded_backups(
                                    &conn,
                                    &rechecked_plan
                                )
                                .is_ok());
                                assert!(refuse_source_path_overlaps(
                                    &rechecked_plan,
                                    &new_source,
                                    &backup,
                                    true
                                )
                                .is_ok());
                                assert!(crate::lml_store::ensure_generic_targets_allowed(
                                    &conn,
                                    &root,
                                    std::slice::from_ref(&new_target)
                                )
                                .is_ok());
                                let new_report =
                                    conflict::analyze(&conn, std::slice::from_ref(&new_target))
                                        .unwrap();
                                assert!(!new_report.has_protected_hits());
                                assert_eq!(new_report.foreign_conflicts.len(), 1);
                                assert_eq!(new_report.foreign_conflicts[0].owner_mod_id, None);
                                assert_eq!(new_report.foreign_conflicts[0].path, new_target);
                                let old_report =
                                    conflict::analyze(&conn, std::slice::from_ref(&old_target))
                                        .unwrap();
                                assert!(old_report
                                    .foreign_conflicts
                                    .iter()
                                    .any(|item| item.owner_mod_id == Some(old_id)));
                                assert!(old_report
                                    .foreign_conflicts
                                    .iter()
                                    .any(|item| item.owner_mod_id == Some(expected_owner)));

                                let retry_tree_before = snapshot_backup_tree(dir.path());
                                let retry_database_before = snapshot_backup_database(&conn);
                                for confirmed in [false, true] {
                                    let retry = reinstall(
                                        &mut conn,
                                        old_id,
                                        &new_source,
                                        "v2",
                                        &provider,
                                        &root,
                                        &backup,
                                        &recycle,
                                        InstallOptions {
                                            override_foreign_conflicts: confirmed,
                                            ..Default::default()
                                        },
                                    );
                                    match (confirmed, retry) {
                                        (false, Ok(InstallOutcome::RequiresOverride(report))) => {
                                            assert_eq!(report.foreign_conflicts.len(), 1);
                                            assert_eq!(
                                                report.foreign_conflicts[0].owner_mod_id,
                                                None
                                            );
                                            assert_eq!(
                                                report.foreign_conflicts[0].path,
                                                new_target
                                            );
                                        }
                                        (
                                            true,
                                            Err(CoreError::ForeignConflict { path, owner_mod_id }),
                                        ) => {
                                            assert_eq!(owner_mod_id, expected_owner);
                                            assert_eq!(path, old_target);
                                        }
                                        (_, unexpected) => {
                                            panic!("unexpected confirmation result: {unexpected:?}")
                                        }
                                    }
                                    let status: String = conn
                                        .query_row(
                                            "SELECT status FROM installed_mod WHERE id = ?1",
                                            [old_id],
                                            |row| row.get(0),
                                        )
                                        .unwrap();
                                    let retry_intents: i64 = conn.query_row(
                                        "SELECT COUNT(*) FROM manual_dlc_event WHERE operation = 'reinstall' AND phase = 'intent'",
                                        [],
                                        |row| row.get(0),
                                    ).unwrap();
                                    assert_eq!(status, "active");
                                    assert_eq!(retry_intents, 0);
                                    assert_eq!(
                                        std::fs::read(&old_target).unwrap(),
                                        b"foreign payload"
                                    );
                                    assert_eq!(
                                        std::fs::read(&new_target).unwrap(),
                                        b"untracked target"
                                    );
                                    assert!(!recycle.exists());
                                    assert!(!backup.exists());
                                    assert_eq!(snapshot_backup_tree(dir.path()), retry_tree_before);
                                    assert_eq!(
                                        snapshot_backup_database(&conn),
                                        retry_database_before
                                    );
                                }
                            }
                        }
                        (
                            None,
                            Ok(InstallOutcome::Success {
                                installed_mod_id, ..
                            }),
                        ) => {
                            assert_eq!(std::fs::read(&new_target).unwrap(), b"new payload");
                            let old_status: String = conn
                                .query_row(
                                    "SELECT status FROM installed_mod WHERE id = ?1",
                                    [old_id],
                                    |row| row.get(0),
                                )
                                .unwrap();
                            let new_status: String = conn
                                .query_row(
                                    "SELECT status FROM installed_mod WHERE id = ?1",
                                    [installed_mod_id],
                                    |row| row.get(0),
                                )
                                .unwrap();
                            assert_eq!(old_status, "uninstalled");
                            assert_eq!(new_status, "active");
                            let entries = crate::recycle_bin::list(&conn).unwrap();
                            assert_eq!(entries.len(), 1);
                            assert_eq!(entries[0].original_installed_mod_id, Some(old_id));
                            let intents_after: i64 = conn.query_row(
                                "SELECT COUNT(*) FROM manual_dlc_event WHERE operation = 'reinstall' AND phase = 'intent'",
                                [],
                                |row| row.get(0),
                            ).unwrap();
                            assert!(intents_after > 0);
                        }
                        (_, unexpected) => panic!("unexpected reinstall result: {unexpected:?}"),
                    }
                }
            }
        }
    }

    #[test]
    fn reinstall_checks_existing_conflicts_before_removing_the_old_version() {
        for (tracked, allow_override) in
            [(true, false), (true, true), (false, false), (false, true)]
        {
            let dir = tempfile::tempdir().unwrap();
            let game_root = dir.path().join("game");
            let provider = crate::providers::LegacySpProvider::new(game_root.clone());
            let mut conn = crate::db::open_in_memory().unwrap();
            let backup = dir.path().join("backups");
            let mut old_id = 0;
            for name in ["old.asi", "shared.asi"] {
                if name == "shared.asi" && !tracked {
                    std::fs::write(game_root.join(name), b"shared.asi").unwrap();
                    continue;
                }
                let source = write_source(dir.path(), name, name.as_bytes());
                let plan = crate::mod_analyzer::classify(&source, &provider).unwrap();
                let outcome = install(
                    &mut conn,
                    name,
                    &plan,
                    &game_root,
                    &backup,
                    InstallOptions::default(),
                    &source,
                )
                .unwrap();
                if name == "old.asi" {
                    let InstallOutcome::Success {
                        installed_mod_id, ..
                    } = outcome
                    else {
                        panic!("expected initial installation");
                    };
                    old_id = installed_mod_id;
                }
            }
            std::fs::create_dir_all(dir.path().join("new")).unwrap();
            let source = write_source(&dir.path().join("new"), "shared.asi", b"new content");
            let recycle = dir.path().join("recycle");
            // OpenAI-Codex_v1_removal_review: Compare refusal with the full fixture.
            let tree_before = snapshot_backup_tree(dir.path());
            let database_before = snapshot_backup_database(&conn);
            let outcome = reinstall(
                &mut conn,
                old_id,
                &source,
                "v2",
                &provider,
                &game_root,
                &backup,
                &recycle,
                InstallOptions {
                    override_foreign_conflicts: allow_override,
                    ..Default::default()
                },
            );
            if tracked {
                let expected_owner: i64 = conn
                    .query_row(
                        "SELECT id FROM installed_mod WHERE name = 'shared.asi' AND status = 'active'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert!(matches!(
                    &outcome,
                    Err(CoreError::ForeignConflict { path, owner_mod_id })
                        if path == &game_root.join("shared.asi") && *owner_mod_id == expected_owner
                ));
            } else if !allow_override {
                assert!(matches!(outcome, Ok(InstallOutcome::RequiresOverride(_))));
            } else {
                let InstallOutcome::Success {
                    installed_mod_id: new_id,
                    ..
                } = outcome.unwrap()
                else {
                    panic!("expected unowned overwrite to succeed");
                };
                assert!(!game_root.join("old.asi").exists());
                assert_eq!(
                    std::fs::read(game_root.join("shared.asi")).unwrap(),
                    b"new content"
                );
                assert_eq!(std::fs::read(backup.join("0.bak")).unwrap(), b"shared.asi");
                let old_status: String = conn
                    .query_row(
                        "SELECT status FROM installed_mod WHERE id = ?1",
                        [old_id],
                        |row| row.get(0),
                    )
                    .unwrap();
                let new_status: String = conn
                    .query_row(
                        "SELECT status FROM installed_mod WHERE id = ?1",
                        [new_id],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(old_status, "uninstalled");
                assert_eq!(new_status, "active");
                let entries = crate::recycle_bin::list(&conn).unwrap();
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].original_installed_mod_id, Some(old_id));
            }
            if tracked || !allow_override {
                assert_eq!(snapshot_backup_tree(dir.path()), tree_before);
                assert_eq!(snapshot_backup_database(&conn), database_before);
            }
            if tracked || !allow_override {
                assert_eq!(
                    std::fs::read(game_root.join("old.asi")).unwrap(),
                    b"old.asi"
                );
                assert_eq!(
                    std::fs::read(game_root.join("shared.asi")).unwrap(),
                    b"shared.asi"
                );
                assert!(!recycle.exists());
            }
        }
    }

    #[test]
    fn protected_file_target_blocks_with_no_writes() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let source = write_source(dir.path(), "GTA5.exe", b"malicious");

        let plan = ModPlan::new(
            ModFormat::Asi, // format doesn't matter; target name is what's checked
            vec![PlannedFile {
                source: source.clone(),
                target: game_root.join("GTA5.exe"),
            }],
        );

        let mut conn = crate::db::open_in_memory().unwrap();
        let outcome = install(
            &mut conn,
            "Malicious Mod",
            &plan,
            &game_root,
            &dir.path().join("backups"),
            InstallOptions::default(),
            &source,
        )
        .unwrap();

        assert!(matches!(outcome, InstallOutcome::ProtectedFileBlocked(_)));
        assert!(!game_root.join("GTA5.exe").exists());
    }

    // OpenAI-Codex_v1_removal_review: Reject a managed owner after an unowned target.
    #[test]
    fn mixed_conflicts_refuse_every_target_with_or_without_override() {
        for owner_present in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let game_root = dir.path().join("game");
            std::fs::create_dir(&game_root).unwrap();
            let unowned = write_source(&game_root, "unowned.asi", b"user original");
            let owned = game_root.join("owned.asi");
            if owner_present {
                std::fs::write(&owned, b"managed payload").unwrap();
            }
            let recorded = if cfg!(windows) {
                game_root.join("OWNED.ASI")
            } else {
                owned.clone()
            };
            let source_a = write_source(dir.path(), "source_a.asi", b"new A");
            let source_b = write_source(dir.path(), "source_b.asi", b"new B");
            let mut conn = crate::db::open_in_memory().unwrap();
            conn.execute(
                "INSERT INTO installed_mod (name, source_type, install_path, status) VALUES ('Owner', 'asi', ?1, 'active')",
                [game_root.to_string_lossy().as_ref()],
            )
            .unwrap();
            let owner = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) VALUES (?1, ?2, 'hash')",
                rusqlite::params![owner, recorded.to_string_lossy().as_ref()],
            )
            .unwrap();
            let report = conflict::analyze(&conn, &[unowned.clone(), owned.clone()]).unwrap();
            assert_eq!(report.foreign_conflicts.len(), 2);
            assert_eq!(report.foreign_conflicts[0].owner_mod_id, None);
            assert_eq!(report.foreign_conflicts[1].owner_mod_id, Some(owner));
            let plan = ModPlan::new(
                ModFormat::Asi,
                vec![
                    PlannedFile {
                        source: source_a.clone(),
                        target: unowned.clone(),
                    },
                    PlannedFile {
                        source: source_b,
                        target: owned,
                    },
                ],
            );
            let backup = dir.path().join("backups");
            let tree_before = snapshot_backup_tree(dir.path());
            let database_before = snapshot_backup_database(&conn);
            for allow_override in [false, true] {
                let error = install(
                    &mut conn,
                    "New mod",
                    &plan,
                    &game_root,
                    &backup,
                    InstallOptions {
                        override_foreign_conflicts: allow_override,
                        ..Default::default()
                    },
                    &source_a,
                )
                .unwrap_err();
                assert!(matches!(
                    error,
                    CoreError::ForeignConflict { ref path, owner_mod_id }
                        if path == &recorded && owner_mod_id == owner
                ));
                assert_eq!(snapshot_backup_tree(dir.path()), tree_before);
                assert_eq!(snapshot_backup_database(&conn), database_before);
            }
        }
    }

    #[test]
    fn managed_conflict_refuses_both_override_choices() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let existing_target = game_root.join("shared.dll");
        std::fs::write(&existing_target, b"already there").unwrap();

        let mut conn = crate::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status) \
             VALUES ('OtherMod', 'dll', '', 'active')",
            [],
        )
        .unwrap();
        let other_mod_id = conn.last_insert_rowid();
        // OpenAI-Codex: A low-overlap conflict also requires explicit confirmation.
        for file in ["shared.dll", "other_a.dll", "other_b.dll"] {
            conn.execute(
                "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) \
                 VALUES (?1, ?2, 'hash')",
                rusqlite::params![other_mod_id, game_root.join(file).to_string_lossy()],
            )
            .unwrap();
        }

        let source = write_source(dir.path(), "shared.dll", b"new version");
        let plan = ModPlan::new(
            ModFormat::NativeDll,
            vec![PlannedFile {
                source: source.clone(),
                target: existing_target.clone(),
            }],
        );

        // OpenAI-Codex_v1_removal_review: An active owner blocks both override choices.
        for allow_override in [false, true] {
            let outcome = install(
                &mut conn,
                "NewMod",
                &plan,
                &game_root,
                &dir.path().join("backups"),
                InstallOptions {
                    override_foreign_conflicts: allow_override,
                    ..Default::default()
                },
                &source,
            )
            .unwrap_err();

            assert!(matches!(
                &outcome,
                CoreError::ForeignConflict { path, owner_mod_id }
                    if path == &existing_target && *owner_mod_id == other_mod_id
            ));
            assert_eq!(std::fs::read(&existing_target).unwrap(), b"already there");
            assert!(!dir.path().join("backups").exists());
        }
    }

    #[test]
    fn untracked_conflict_with_override_proceeds_and_backs_up() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let existing_target = game_root.join("shared.dll");
        std::fs::write(&existing_target, b"already there").unwrap();

        let mut conn = crate::db::open_in_memory().unwrap();
        // OpenAI-Codex_v1_removal_review: The original has no active owner.

        let source = write_source(dir.path(), "shared.dll", b"new version");
        let plan = ModPlan::new(
            ModFormat::NativeDll,
            vec![PlannedFile {
                source: source.clone(),
                target: existing_target.clone(),
            }],
        );

        let outcome = install(
            &mut conn,
            "NewMod",
            &plan,
            &game_root,
            &dir.path().join("backups"),
            InstallOptions {
                auto_backup: true,
                override_foreign_conflicts: true,
            },
            &source,
        )
        .unwrap();

        assert!(matches!(outcome, InstallOutcome::Success { .. }));
        assert_eq!(std::fs::read(&existing_target).unwrap(), b"new version");

        let backup_path: Option<String> = conn
            .query_row(
                "SELECT backup_path FROM installed_mod_file WHERE target_path = ?1 \
                 ORDER BY id DESC LIMIT 1",
                [existing_target.to_string_lossy()],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            backup_path.is_some(),
            "overwritten file should have a recorded backup"
        );
        // OpenAI-Codex_v1_removal_review: Preserve and restore the original bytes.
        assert_eq!(
            std::fs::read(backup_path.expect("untracked original backup")).unwrap(),
            b"already there"
        );
        let mod_id: i64 = conn
            .query_row(
                "SELECT id FROM installed_mod WHERE status = 'active'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        crate::uninstall::uninstall(&mut conn, mod_id, &game_root, &dir.path().join("recycle"))
            .unwrap();
        assert_eq!(std::fs::read(&existing_target).unwrap(), b"already there");
        let entries = crate::recycle_bin::list(&conn).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].original_installed_mod_id, Some(mod_id));
    }

    // OpenAI-Codex: One operation spans both versions without settling the uninstall.
    fn assert_manual_dlc_reinstall(new_pack: &str, fail_new_install: bool) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir_all(&root).unwrap();
        let provider = crate::providers::LegacySpProvider::new(root.clone());
        let old_source = dir.path().join("v1/OldCar");
        std::fs::create_dir_all(&old_source).unwrap();
        std::fs::write(old_source.join("dlc.rpf"), b"v1 pack").unwrap();
        let new_source = dir.path().join("v2").join(new_pack);
        std::fs::create_dir_all(&new_source).unwrap();
        std::fs::write(new_source.join("dlc.rpf"), b"v2 pack").unwrap();
        let plan = crate::mod_analyzer::classify(&old_source, &provider).unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let InstallOutcome::Success {
            installed_mod_id: old_id,
            ..
        } = install(
            &mut conn,
            "Old pack",
            &plan,
            &root,
            &dir.path().join("backups"),
            InstallOptions::default(),
            &old_source,
        )
        .unwrap()
        else {
            panic!("initial pack must install");
        };
        if fail_new_install {
            conn.execute_batch(
                "CREATE TRIGGER reject_updated_pack BEFORE INSERT ON installed_mod \
                BEGIN SELECT RAISE(ABORT, 'new pack DB failure'); END;",
            )
            .unwrap();
        }
        let result = reinstall(
            &mut conn,
            old_id,
            &new_source,
            "v2",
            &provider,
            &root,
            &dir.path().join("backups"),
            &dir.path().join("recycle"),
            InstallOptions::default(),
        );
        if fail_new_install {
            assert!(result.is_err());
        } else {
            assert!(matches!(result.unwrap(), InstallOutcome::Success { .. }));
            assert_eq!(
                std::fs::read(
                    root.join("mods/update/x64/dlcpacks")
                        .join(new_pack)
                        .join("dlc.rpf")
                )
                .unwrap(),
                b"v2 pack"
            );
        }
        let tasks = dlc_manual::list_tasks(&conn, Some(&root)).unwrap();
        let expected_packs = if new_pack == "OldCar" { 1 } else { 2 };
        assert_eq!(tasks.len(), expected_packs);
        assert!(tasks.iter().all(|task| task.unresolved == fail_new_install));
        let (operations, intents, settled): (i64, i64, i64) = conn.query_row(
            "SELECT COUNT(DISTINCT operation_id), COALESCE(SUM(phase = 'intent'), 0), \
             COALESCE(SUM(phase = 'settled'), 0) FROM manual_dlc_event WHERE operation = 'reinstall'",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        assert_eq!(operations, 1);
        assert_eq!(intents, expected_packs as i64);
        assert_eq!(
            settled,
            if fail_new_install {
                0
            } else {
                expected_packs as i64
            }
        );
        let old_status: String = conn
            .query_row(
                "SELECT status FROM installed_mod WHERE id = ?1",
                [old_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_status, "uninstalled");
        assert!(!dlc::dlclist_path(&root).exists());
    }

    #[test]
    fn manual_dlc_same_pack_reinstall_is_one_task() {
        assert_manual_dlc_reinstall("OldCar", false);
    }

    #[test]
    fn manual_dlc_renamed_pack_reinstall_keeps_both_tasks() {
        assert_manual_dlc_reinstall("NewCar", false);
    }

    #[test]
    fn manual_dlc_failed_reinstall_keeps_the_whole_intent_unsettled() {
        assert_manual_dlc_reinstall("NewCar", true);
    }

    // OpenAI-Codex: These source-first cases pin the approved manual-registration contract.
    fn assert_manual_dlc_install_preserves_xml(with_xml: bool) {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let xml = dlc::dlclist_path(&game_root);
        let original = b"<SMandatoryPacksData><Paths></Paths></SMandatoryPacksData>";
        if with_xml {
            std::fs::create_dir_all(xml.parent().unwrap()).unwrap();
            std::fs::write(&xml, original).unwrap();
        }
        let source = write_source(dir.path(), "dlc.rpf", b"synthetic pack");
        let target = game_root.join("mods/update/x64/dlcpacks/ManualCar/dlc.rpf");
        let plan = ModPlan::new(
            ModFormat::AddOnPack {
                pack_name: "ManualCar".into(),
            },
            vec![PlannedFile {
                source: source.clone(),
                target: target.clone(),
            }],
        );
        let db_path = dir.path().join("manual-install.sqlite3");
        let mut conn = crate::db::open(&db_path).unwrap();
        let result = install(
            &mut conn,
            "Manual car",
            &plan,
            &game_root,
            &dir.path().join("backups"),
            InstallOptions::default(),
            &source,
        )
        .unwrap();
        assert!(matches!(result, InstallOutcome::Success { .. }));
        assert_eq!(std::fs::read(&target).unwrap(), b"synthetic pack");
        if with_xml {
            assert_eq!(std::fs::read(&xml).unwrap(), original);
        } else {
            assert!(!xml.exists(), "manual registration must not fabricate XML");
        }
        drop(conn);
        let conn = crate::db::open(&db_path).unwrap();
        let (intents, settled): (i64, i64) = conn
            .query_row(
                "SELECT COALESCE(SUM(phase = 'intent'), 0), COALESCE(SUM(phase = 'settled'), 0) \
             FROM manual_dlc_event WHERE pack_display = 'ManualCar'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((intents, settled), (1, 1));
    }

    #[test]
    fn manual_dlc_install_does_not_require_xml_and_persists_work() {
        assert_manual_dlc_install_preserves_xml(false);
    }

    #[test]
    fn manual_dlc_install_preserves_existing_xml_bytes() {
        assert_manual_dlc_install_preserves_xml(true);
    }

    #[test]
    fn manual_dlc_intent_database_failure_prevents_payload_writes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir_all(&root).unwrap();
        let source = write_source(dir.path(), "dlc.rpf", b"pack");
        let target = root.join("mods/update/x64/dlcpacks/ManualCar/dlc.rpf");
        let plan = ModPlan::new(
            ModFormat::AddOnPack {
                pack_name: "ManualCar".into(),
            },
            vec![PlannedFile {
                source: source.clone(),
                target: target.clone(),
            }],
        );
        let mut conn = crate::db::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TRIGGER reject_manual_intent BEFORE INSERT ON manual_dlc_event \
            BEGIN SELECT RAISE(ABORT, 'intent DB failure'); END;",
        )
        .unwrap();
        let backups = dir.path().join("backups");
        assert!(install(
            &mut conn,
            "Manual car",
            &plan,
            &root,
            &backups,
            InstallOptions::default(),
            &source
        )
        .is_err());
        assert!(!target.exists());
        assert!(!backups.exists());
        let mods: i64 = conn
            .query_row("SELECT COUNT(*) FROM installed_mod", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mods, 0);
    }

    #[test]
    fn add_on_pack_install_defers_dlclist_registration() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let dlclist_path = dlc::dlclist_path(&game_root);
        std::fs::create_dir_all(dlclist_path.parent().unwrap()).unwrap();
        std::fs::write(
            &dlclist_path,
            "<SMandatoryPacksData>\n  <Paths>\n  </Paths>\n</SMandatoryPacksData>\n",
        )
        .unwrap();

        let source = write_source(dir.path(), "dlc.rpf", b"payload");
        let plan = ModPlan::new(
            ModFormat::AddOnPack {
                pack_name: "MyAddonCar".to_string(),
            },
            vec![PlannedFile {
                source: source.clone(),
                target: game_root.join("mods/update/x64/dlcpacks/MyAddonCar/dlc.rpf"),
            }],
        );

        let mut conn = crate::db::open_in_memory().unwrap();
        let outcome = install(
            &mut conn,
            "My Addon Car",
            &plan,
            &game_root,
            &dir.path().join("backups"),
            InstallOptions::default(),
            &source,
        )
        .unwrap();

        assert!(matches!(outcome, InstallOutcome::Success { .. }));
        assert!(!dlc::has_entry(&dlclist_path, "MyAddonCar").unwrap());
        let tasks = dlc_manual::list_tasks(&conn, Some(&game_root)).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].recommendation, "check_present");
        assert!(!tasks[0].unresolved);
    }

    // OpenAI-Codex: Capture the complete synthetic fixture around backup refusal.
    fn snapshot_backup_tree(root: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
        let mut pending = vec![root.to_path_buf()];
        let mut snapshot = Vec::new();
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let kind = entry.file_type().unwrap();
                if kind.is_dir() {
                    snapshot.push((path.clone(), None));
                    pending.push(path);
                } else {
                    assert!(kind.is_file());
                    snapshot.push((path.clone(), Some(std::fs::read(path).unwrap())));
                }
            }
        }
        snapshot.sort_by(|a, b| a.0.cmp(&b.0));
        snapshot
    }

    fn snapshot_backup_database(
        conn: &Connection,
    ) -> Vec<(String, Vec<Vec<rusqlite::types::Value>>)> {
        let mut statement = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='table' \
                 AND (name NOT LIKE 'sqlite_%' OR name='sqlite_sequence') ORDER BY name",
            )
            .unwrap();
        let tables = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        drop(statement);
        tables
            .into_iter()
            .map(|table| {
                let sql = format!(
                    "SELECT rowid, * FROM \"{}\" ORDER BY rowid",
                    table.replace('"', "\"\"")
                );
                let mut statement = conn.prepare(&sql).unwrap();
                let count = statement.column_count();
                let rows = statement
                    .query_map([], |row| {
                        (0..count)
                            .map(|index| row.get::<_, rusqlite::types::Value>(index))
                            .collect::<rusqlite::Result<Vec<_>>>()
                    })
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap();
                (table, rows)
            })
            .collect()
    }

    // OpenAI-Codex: The writer must not replace an occupied backup after preflight.
    #[test]
    fn backup_writer_refuses_an_occupied_slot_without_changing_files() {
        let temp = tempfile::tempdir().unwrap();
        let game_root = temp.path().join("game");
        let backup_root = temp.path().join("backups");
        std::fs::create_dir(&game_root).unwrap();
        std::fs::create_dir(&backup_root).unwrap();
        let source = write_source(temp.path(), "new.asi", b"new payload");
        let target = write_source(&game_root, "existing.asi", b"original target");
        let occupied = write_source(&backup_root, "0.bak", b"older backup");
        let plan = ModPlan::new(
            ModFormat::Asi,
            vec![PlannedFile {
                source: source.clone(),
                target: target.clone(),
            }],
        );
        let tree_before = snapshot_backup_tree(temp.path());
        let mut actions = InstallTransaction::default();
        let result = perform_writes(&plan, &backup_root, true, &mut actions);
        assert!(matches!(
            result,
            Err(CoreError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists
        ));
        assert_eq!(std::fs::read(&occupied).unwrap(), b"older backup");
        assert_eq!(std::fs::read(&target).unwrap(), b"original target");
        assert_eq!(std::fs::read(&source).unwrap(), b"new payload");
        assert!(actions.actions.is_empty());
        assert_eq!(snapshot_backup_tree(temp.path()), tree_before);

        let safe_backup = temp.path().join("safe_backups");
        let mut safe_actions = InstallTransaction::default();
        let hashes = perform_writes(&plan, &safe_backup, true, &mut safe_actions).unwrap();
        assert_eq!(hashes.get(&target), Some(&hash_file(&target).unwrap()));
        assert_eq!(std::fs::read(&target).unwrap(), b"new payload");
        assert_eq!(
            std::fs::read(safe_backup.join("0.bak")).unwrap(),
            b"original target"
        );
        assert!(matches!(
            safe_actions.actions.as_slice(),
            [InstallAction::FileOverwritten { .. }]
        ));
    }

    // OpenAI-Codex: A later occupied backup slot must block the whole install.
    #[test]
    fn install_preserves_existing_backup_before_any_target_write() {
        let temp = tempfile::tempdir().unwrap();
        let game_root = temp.path().join("game");
        let backup_root = temp.path().join("backups");
        std::fs::create_dir(&game_root).unwrap();
        std::fs::create_dir(&backup_root).unwrap();
        let source_a = write_source(temp.path(), "source_a.asi", b"new A");
        let source_b = write_source(temp.path(), "source_b.asi", b"new B");
        let target_a = write_source(&game_root, "a.asi", b"original A");
        let target_b = write_source(&game_root, "b.asi", b"original B");
        let occupied_backup = write_source(&backup_root, "1.bak", b"older backup");
        assert!(!backup_root.join("0.bak").exists());
        let plan = ModPlan::new(
            ModFormat::Asi,
            vec![
                PlannedFile {
                    source: source_a.clone(),
                    target: target_a.clone(),
                },
                PlannedFile {
                    source: source_b.clone(),
                    target: target_b.clone(),
                },
            ],
        );
        let mut conn = crate::db::open_in_memory().unwrap();
        let report = conflict::analyze(&conn, &[target_a.clone(), target_b.clone()]).unwrap();
        assert!(!report.has_protected_hits());
        assert_eq!(report.foreign_conflicts.len(), 2);
        assert!(report
            .foreign_conflicts
            .iter()
            .all(|item| item.owner_mod_id.is_none()));
        let tree_before = snapshot_backup_tree(temp.path());
        let database_before = snapshot_backup_database(&conn);
        let changes_before: i64 = conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        let result = install(
            &mut conn,
            "Two files",
            &plan,
            &game_root,
            &backup_root,
            InstallOptions {
                auto_backup: true,
                override_foreign_conflicts: true,
            },
            &source_a,
        );
        let expected_error = format!(
            "backup destination already exists: {}",
            occupied_backup.display()
        );
        assert!(matches!(
            &result,
            Err(CoreError::UnsupportedFormat { reason }) if reason == &expected_error
        ));
        assert_eq!(snapshot_backup_tree(temp.path()), tree_before);
        assert_eq!(snapshot_backup_database(&conn), database_before);
        let changes_after: i64 = conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        assert_eq!(changes_after, changes_before);
        assert!(matches!(
            result,
            Err(CoreError::UnsupportedFormat { reason }) if reason.contains("backup")
        ));
        assert_eq!(std::fs::read(&target_a).unwrap(), b"original A");
        assert_eq!(std::fs::read(&target_b).unwrap(), b"original B");
        assert_eq!(std::fs::read(&source_a).unwrap(), b"new A");
        assert_eq!(std::fs::read(&source_b).unwrap(), b"new B");
        assert_eq!(std::fs::read(&occupied_backup).unwrap(), b"older backup");
        assert!(!backup_root.join("0.bak").exists());
        let installed_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM installed_mod", [], |row| row.get(0))
            .unwrap();
        let event_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM install_event", [], |row| row.get(0))
            .unwrap();
        assert_eq!(installed_count, 0);
        assert_eq!(event_count, 0);
        // OpenAI-Codex: An unoccupied backup directory remains usable.
        let safe_backup = temp.path().join("safe_backups");
        std::fs::create_dir(&safe_backup).unwrap();
        let unrelated = write_source(&safe_backup, "unrelated.bak", b"unrelated");
        let control = install(
            &mut conn,
            "Safe two files",
            &plan,
            &game_root,
            &safe_backup,
            InstallOptions {
                auto_backup: true,
                override_foreign_conflicts: true,
            },
            &source_a,
        )
        .unwrap();
        assert!(matches!(control, InstallOutcome::Success { .. }));
        assert_eq!(std::fs::read(&target_a).unwrap(), b"new A");
        assert_eq!(std::fs::read(&target_b).unwrap(), b"new B");
        assert_eq!(
            std::fs::read(safe_backup.join("0.bak")).unwrap(),
            b"original A"
        );
        assert_eq!(
            std::fs::read(safe_backup.join("1.bak")).unwrap(),
            b"original B"
        );
        assert_eq!(std::fs::read(unrelated).unwrap(), b"unrelated");
    }

    // OpenAI-Codex: Earlier targets cannot become later backup destinations.
    #[test]
    fn install_refuses_a_planned_target_that_is_another_backup_slot() {
        let temp = tempfile::tempdir().unwrap();
        let game_root = temp.path().join("game");
        let backup_root = game_root.join("backups");
        std::fs::create_dir(&game_root).unwrap();
        std::fs::create_dir(&backup_root).unwrap();
        let target_a = backup_root.join("1.bak");
        let target_b = write_source(&game_root, "b.asi", b"original B");
        let source_a = write_source(temp.path(), "source_a.asi", b"new A");
        let source_b = write_source(temp.path(), "source_b.asi", b"new B");
        assert!(!target_a.exists());
        let plan = ModPlan::new(
            ModFormat::Asi,
            vec![
                PlannedFile {
                    source: source_a.clone(),
                    target: target_a.clone(),
                },
                PlannedFile {
                    source: source_b.clone(),
                    target: target_b.clone(),
                },
            ],
        );
        let mut conn = crate::db::open_in_memory().unwrap();
        assert!(crate::lml_store::ensure_generic_targets_allowed(
            &conn,
            &game_root,
            &[target_a.clone(), target_b.clone(), backup_root.clone()],
        )
        .is_ok());
        let report = conflict::analyze(&conn, &[target_a.clone(), target_b.clone()]).unwrap();
        assert!(!report.has_protected_hits());
        assert_eq!(report.foreign_conflicts.len(), 1);
        assert_eq!(report.foreign_conflicts[0].owner_mod_id, None);
        let tree_before = snapshot_backup_tree(temp.path());
        let database_before = snapshot_backup_database(&conn);
        let changes_before: i64 = conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        let result = install(
            &mut conn,
            "Overlapping target",
            &plan,
            &game_root,
            &backup_root,
            InstallOptions {
                auto_backup: true,
                override_foreign_conflicts: true,
            },
            &source_a,
        );
        let expected_error = format!(
            "backup destination overlaps planned target: {}",
            target_a.display()
        );
        assert!(matches!(
            result,
            Err(CoreError::UnsupportedFormat { reason }) if reason == expected_error
        ));
        assert_eq!(snapshot_backup_tree(temp.path()), tree_before);
        assert_eq!(snapshot_backup_database(&conn), database_before);
        let changes_after: i64 = conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        assert_eq!(changes_after, changes_before);

        let safe_backup = game_root.join("separate_backups");
        let control = install(
            &mut conn,
            "Separate target",
            &plan,
            &game_root,
            &safe_backup,
            InstallOptions {
                auto_backup: true,
                override_foreign_conflicts: true,
            },
            &source_a,
        )
        .unwrap();
        assert!(matches!(control, InstallOutcome::Success { .. }));
        assert_eq!(std::fs::read(&target_a).unwrap(), b"new A");
        assert_eq!(std::fs::read(&target_b).unwrap(), b"new B");
        assert_eq!(
            std::fs::read(safe_backup.join("1.bak")).unwrap(),
            b"original B"
        );
    }

    // OpenAI-Codex: A missing target can still belong to another mod.
    #[test]
    fn install_preserves_missing_targets_recorded_by_other_mods() {
        use sha2::{Digest, Sha256};

        for owner_disabled in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let game_root = temp.path().join("game");
            let backup_root = game_root.join("backups");
            let staging_root = temp.path().join("staging");
            std::fs::create_dir(&game_root).unwrap();
            std::fs::create_dir(&backup_root).unwrap();
            let owner_target = backup_root.join("0.bak");
            if owner_disabled {
                std::fs::write(&owner_target, b"owner payload").unwrap();
            }
            let source = write_source(temp.path(), "new.asi", b"new payload");
            let target = write_source(&game_root, "candidate.asi", b"existing candidate");
            let mut conn = crate::db::open_in_memory().unwrap();
            conn.execute(
                "INSERT INTO installed_mod (name, source_type, install_path, status, mode) \
                 VALUES ('Owner', 'asi', ?1, 'active', 'legacy-sp')",
                [game_root.to_str().unwrap()],
            )
            .unwrap();
            let owner_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) \
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    owner_id,
                    owner_target.to_str().unwrap(),
                    format!("{:x}", Sha256::digest(b"owner payload"))
                ],
            )
            .unwrap();
            if owner_disabled {
                let fixture_stage = staging_root.join(owner_id.to_string());
                std::fs::create_dir_all(&fixture_stage).unwrap();
                std::fs::rename(&owner_target, fixture_stage.join("0")).unwrap();
                crate::uninstall::seed_disabled_fixture_from_staging(
                    &conn,
                    owner_id,
                    &staging_root,
                );
                assert_eq!(
                    crate::state::disabled_payloads(&conn, owner_id, &staging_root)
                        .unwrap()
                        .len(),
                    1
                );
            }
            let expected_status = if owner_disabled { "disabled" } else { "active" };
            let owner_status: String = conn
                .query_row(
                    "SELECT status FROM installed_mod WHERE id = ?1",
                    [owner_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(owner_status, expected_status);
            assert!(!owner_target.exists());
            assert!(crate::providers::ensure_installed_mod_available(&conn, owner_id).is_ok());
            assert!(crate::lml_store::ensure_generic_targets_allowed(
                &conn,
                &game_root,
                &[backup_root.clone(), owner_target.clone()],
            )
            .is_ok());
            let report = conflict::analyze(&conn, std::slice::from_ref(&target)).unwrap();
            assert!(!report.has_protected_hits());
            assert_eq!(report.foreign_conflicts.len(), 1);
            assert_eq!(report.foreign_conflicts[0].owner_mod_id, None);
            let plan = ModPlan::new(
                ModFormat::Asi,
                vec![PlannedFile {
                    source: source.clone(),
                    target: target.clone(),
                }],
            );
            let tree_before = snapshot_backup_tree(temp.path());
            let database_before = snapshot_backup_database(&conn);
            let changes_before: i64 = conn
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .unwrap();
            let result = install(
                &mut conn,
                "Candidate",
                &plan,
                &game_root,
                &backup_root,
                InstallOptions {
                    auto_backup: true,
                    override_foreign_conflicts: true,
                },
                &source,
            );
            let expected_error = format!(
                "backup destination overlaps a recorded target: {}",
                owner_target.display()
            );
            assert!(matches!(
                result,
                Err(CoreError::UnsupportedFormat { reason }) if reason == expected_error
            ));
            assert!(!owner_target.exists());
            assert_eq!(snapshot_backup_tree(temp.path()), tree_before);
            assert_eq!(snapshot_backup_database(&conn), database_before);
            let changes_after: i64 = conn
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .unwrap();
            assert_eq!(changes_after, changes_before);

            let safe_backup = temp.path().join("safe_backup");
            let control = install(
                &mut conn,
                "Safe candidate",
                &plan,
                &game_root,
                &safe_backup,
                InstallOptions {
                    auto_backup: true,
                    override_foreign_conflicts: true,
                },
                &source,
            )
            .unwrap();
            assert!(matches!(control, InstallOutcome::Success { .. }));
            assert_eq!(std::fs::read(&target).unwrap(), b"new payload");
            assert_eq!(
                std::fs::read(safe_backup.join("0.bak")).unwrap(),
                b"existing candidate"
            );
            assert!(!owner_target.exists());
            let owner_status_after: String = conn
                .query_row(
                    "SELECT status FROM installed_mod WHERE id = ?1",
                    [owner_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(owner_status_after, expected_status);
            if owner_disabled {
                assert_eq!(
                    crate::state::disabled_payloads(&conn, owner_id, &staging_root)
                        .unwrap()
                        .len(),
                    1
                );
            }
        }
    }

    // OpenAI-Codex: Installed backup files are not ordinary override targets.
    #[test]
    fn install_refuses_targets_owned_as_other_mods_backups() {
        for auto_backup in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let game_root = temp.path().join("game");
            let old_backups = game_root.join("old_backups");
            let new_backups = temp.path().join("new_backups");
            std::fs::create_dir(&game_root).unwrap();
            std::fs::create_dir(&old_backups).unwrap();
            let owner_target = write_source(&game_root, "owner.asi", b"owner payload");
            let recorded_backup = write_source(&old_backups, "original.bak", b"owner original");
            let source = write_source(temp.path(), "new.asi", b"new payload");
            let mut conn = crate::db::open_in_memory().unwrap();
            conn.execute(
                "INSERT INTO installed_mod (name, source_type, install_path, status, mode) \
                 VALUES ('Owner', 'asi', ?1, 'active', 'legacy-sp')",
                [game_root.to_str().unwrap()],
            )
            .unwrap();
            let owner_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO installed_mod_file \
                 (installed_mod_id, target_path, backup_path, file_hash) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    owner_id,
                    owner_target.to_str().unwrap(),
                    recorded_backup.to_str().unwrap(),
                    hash_file(&owner_target).unwrap()
                ],
            )
            .unwrap();
            assert!(crate::providers::ensure_installed_mod_available(&conn, owner_id).is_ok());
            let report = conflict::analyze(&conn, std::slice::from_ref(&recorded_backup)).unwrap();
            assert!(!report.has_protected_hits());
            assert_eq!(report.foreign_conflicts.len(), 1);
            assert_eq!(report.foreign_conflicts[0].owner_mod_id, None);
            let plan = ModPlan::new(
                ModFormat::Asi,
                vec![PlannedFile {
                    source: source.clone(),
                    target: recorded_backup.clone(),
                }],
            );
            let tree_before = snapshot_backup_tree(temp.path());
            let database_before = snapshot_backup_database(&conn);
            let changes_before: i64 = conn
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .unwrap();
            let result = install(
                &mut conn,
                "Attempt to replace backup",
                &plan,
                &game_root,
                &new_backups,
                InstallOptions {
                    auto_backup,
                    override_foreign_conflicts: true,
                },
                &source,
            );
            let expected_error = format!(
                "planned target overlaps a recorded backup: {}",
                recorded_backup.display()
            );
            assert!(matches!(
                result,
                Err(CoreError::UnsupportedFormat { reason }) if reason == expected_error
            ));
            assert_eq!(snapshot_backup_tree(temp.path()), tree_before);
            assert_eq!(snapshot_backup_database(&conn), database_before);
            let changes_after: i64 = conn
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .unwrap();
            assert_eq!(changes_after, changes_before);

            let safe_target = write_source(&game_root, "safe.asi", b"safe original");
            let safe_plan = ModPlan::new(
                ModFormat::Asi,
                vec![PlannedFile {
                    source: source.clone(),
                    target: safe_target.clone(),
                }],
            );
            let control = install(
                &mut conn,
                "Safe candidate",
                &safe_plan,
                &game_root,
                &new_backups,
                InstallOptions {
                    auto_backup,
                    override_foreign_conflicts: true,
                },
                &source,
            )
            .unwrap();
            assert!(matches!(control, InstallOutcome::Success { .. }));
            assert_eq!(std::fs::read(&safe_target).unwrap(), b"new payload");
            assert_eq!(std::fs::read(&recorded_backup).unwrap(), b"owner original");
            assert_eq!(std::fs::read(&owner_target).unwrap(), b"owner payload");
            if auto_backup {
                assert_eq!(
                    std::fs::read(new_backups.join("0.bak")).unwrap(),
                    b"safe original"
                );
            } else {
                assert!(!new_backups.exists());
            }
        }
    }

    // OpenAI-Codex: Resolve ordinary relative paths without accepting traversal.
    #[test]
    fn source_guard_resolves_relative_paths_and_refuses_ambiguous_forms() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            source_guard_absolute(Path::new("./example.asi")).unwrap(),
            cwd.join("example.asi")
        );
        assert!(source_guard_absolute(Path::new("../example.asi")).is_err());
        assert!(source_guard_absolute(Path::new("")).is_err());
        #[cfg(windows)]
        {
            let absolute = Path::new(r"C:\folder\example.asi");
            assert_eq!(source_guard_absolute(absolute).unwrap(), absolute);
            assert!(source_guard_absolute(Path::new(r"C:example.asi")).is_err());
            assert!(source_guard_absolute(Path::new(r"\example.asi")).is_err());
            let unc = source_guard_absolute(Path::new(r"\\server\share\example.asi")).unwrap();
            assert!(crate::lml_store::backup_guard_identity(&unc).is_err());
        }
    }

    // OpenAI-Codex: A folder package cannot contain its own install target.
    #[test]
    fn install_refuses_a_target_inside_the_original_package_folder() {
        for auto_backup in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let game_root = temp.path().join("game");
            let package = temp.path().join("package");
            let backup_root = temp.path().join("backups");
            std::fs::create_dir(&game_root).unwrap();
            std::fs::create_dir(&package).unwrap();
            let payload = write_source(&package, "payload.asi", b"package payload");
            let package_target = package.join("nested_target.asi");
            let plan = ModPlan::new(
                ModFormat::Asi,
                vec![PlannedFile {
                    source: payload.clone(),
                    target: package_target.clone(),
                }],
            );
            let mut conn = crate::db::open_in_memory().unwrap();
            let tree_before = snapshot_backup_tree(temp.path());
            let database_before = snapshot_backup_database(&conn);
            let result = install(
                &mut conn,
                "Folder target",
                &plan,
                &game_root,
                &backup_root,
                InstallOptions {
                    auto_backup,
                    override_foreign_conflicts: true,
                },
                &package,
            );
            let expected_error = format!(
                "planned target overlaps the original package: {}",
                package_target.display()
            );
            assert!(matches!(
                result,
                Err(CoreError::UnsupportedFormat { reason }) if reason == expected_error
            ));
            assert_eq!(snapshot_backup_tree(temp.path()), tree_before);
            assert_eq!(snapshot_backup_database(&conn), database_before);
            let safe_target = game_root.join("safe.asi");
            let safe_plan = ModPlan::new(
                ModFormat::Asi,
                vec![PlannedFile {
                    source: payload.clone(),
                    target: safe_target.clone(),
                }],
            );
            let control = install(
                &mut conn,
                "Separate folder target",
                &safe_plan,
                &game_root,
                &backup_root,
                InstallOptions {
                    auto_backup,
                    override_foreign_conflicts: true,
                },
                &package,
            )
            .unwrap();
            assert!(matches!(control, InstallOutcome::Success { .. }));
            assert_eq!(std::fs::read(&safe_target).unwrap(), b"package payload");
            assert_eq!(std::fs::read(&payload).unwrap(), b"package payload");
        }
    }

    // OpenAI-Codex: Reinstall must not consume its own original package.
    #[test]
    fn reinstall_refuses_a_source_that_is_the_old_mod_backup() {
        let temp = tempfile::tempdir().unwrap();
        let game_root = temp.path().join("game");
        let backup_root = temp.path().join("new_backups");
        let recycle_root = temp.path().join("recycle");
        std::fs::create_dir(&game_root).unwrap();
        let old_target = write_source(&game_root, "old.asi", b"old mod");
        let old_backup = write_source(temp.path(), "new_package.asi", b"old original");
        let safe_source = write_source(temp.path(), "safe.asi", b"new mod");
        let provider = crate::providers::LegacySpProvider::new(game_root.clone());
        let mut conn = crate::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status, mode) \
             VALUES ('Old', 'asi', ?1, 'active', 'legacy-sp')",
            [game_root.to_str().unwrap()],
        )
        .unwrap();
        let mod_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO installed_mod_file \
             (installed_mod_id, target_path, backup_path, file_hash) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                mod_id,
                old_target.to_str().unwrap(),
                old_backup.to_str().unwrap(),
                hash_file(&old_target).unwrap()
            ],
        )
        .unwrap();
        assert!(crate::providers::ensure_installed_mod_available(&conn, mod_id).is_ok());
        let tree_before = snapshot_backup_tree(temp.path());
        let database_before = snapshot_backup_database(&conn);
        let changes_before: i64 = conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        let result = reinstall(
            &mut conn,
            mod_id,
            &old_backup,
            "v2",
            &provider,
            &game_root,
            &backup_root,
            &recycle_root,
            InstallOptions::default(),
        );
        let expected_error = format!(
            "reinstall source overlaps an old backup: {}",
            old_backup.display()
        );
        assert!(matches!(
            result,
            Err(CoreError::UnsupportedFormat { reason }) if reason == expected_error
        ));
        assert_eq!(snapshot_backup_tree(temp.path()), tree_before);
        assert_eq!(snapshot_backup_database(&conn), database_before);
        let changes_after: i64 = conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        assert_eq!(changes_after, changes_before);

        let control = reinstall(
            &mut conn,
            mod_id,
            &safe_source,
            "v2",
            &provider,
            &game_root,
            &backup_root,
            &recycle_root,
            InstallOptions::default(),
        )
        .unwrap();
        assert!(matches!(control, InstallOutcome::Success { .. }));
        assert_eq!(std::fs::read(&safe_source).unwrap(), b"new mod");
        assert_eq!(
            std::fs::read(game_root.join("safe.asi")).unwrap(),
            b"new mod"
        );
    }

    // OpenAI-Codex: An old deployed archive cannot also be the new package.
    #[test]
    fn reinstall_refuses_an_archive_that_is_the_old_mod_target() {
        let temp = tempfile::tempdir().unwrap();
        let game_root = temp.path().join("game");
        let backup_root = temp.path().join("new_backups");
        let recycle_root = temp.path().join("recycle");
        std::fs::create_dir(&game_root).unwrap();
        let archive = game_root.join("package.zip");
        let file = std::fs::File::create(&archive).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file("payload.asi", zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut writer, b"archive payload").unwrap();
        writer.finish().unwrap();
        let safe_source = write_source(temp.path(), "safe.asi", b"new mod");
        let provider = crate::providers::LegacySpProvider::new(game_root.clone());
        let mut conn = crate::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status, mode) \
             VALUES ('Old archive', 'zip', ?1, 'active', 'legacy-sp')",
            [game_root.to_str().unwrap()],
        )
        .unwrap();
        let mod_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) \
             VALUES (?1, ?2, ?3)",
            rusqlite::params![
                mod_id,
                archive.to_str().unwrap(),
                hash_file(&archive).unwrap()
            ],
        )
        .unwrap();
        assert!(crate::providers::ensure_installed_mod_available(&conn, mod_id).is_ok());
        let tree_before = snapshot_backup_tree(temp.path());
        let database_before = snapshot_backup_database(&conn);
        let changes_before: i64 = conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        let result = reinstall(
            &mut conn,
            mod_id,
            &archive,
            "v2",
            &provider,
            &game_root,
            &backup_root,
            &recycle_root,
            InstallOptions::default(),
        );
        let expected_error = format!(
            "reinstall source overlaps an old target: {}",
            archive.display()
        );
        assert!(matches!(
            result,
            Err(CoreError::UnsupportedFormat { reason }) if reason == expected_error
        ));
        assert_eq!(snapshot_backup_tree(temp.path()), tree_before);
        assert_eq!(snapshot_backup_database(&conn), database_before);
        let changes_after: i64 = conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        assert_eq!(changes_after, changes_before);

        let control = reinstall(
            &mut conn,
            mod_id,
            &safe_source,
            "v2",
            &provider,
            &game_root,
            &backup_root,
            &recycle_root,
            InstallOptions::default(),
        )
        .unwrap();
        assert!(matches!(control, InstallOutcome::Success { .. }));
        assert_eq!(std::fs::read(&safe_source).unwrap(), b"new mod");
        assert_eq!(
            std::fs::read(game_root.join("safe.asi")).unwrap(),
            b"new mod"
        );
    }

    // OpenAI-Codex: A source folder may contain the old mod's numbered backup.
    #[test]
    fn reinstall_refuses_a_source_folder_containing_the_old_backup() {
        let temp = tempfile::tempdir().unwrap();
        let game_root = temp.path().join("game");
        let package = temp.path().join("package");
        let backup_root = temp.path().join("new_backups");
        let recycle_root = temp.path().join("recycle");
        std::fs::create_dir(&game_root).unwrap();
        std::fs::create_dir(&package).unwrap();
        let old_target = write_source(&game_root, "old.asi", b"old mod");
        let old_backup = write_source(&package, "0.bak", b"old original");
        let package_payload = write_source(&package, "payload.asi", b"new mod");
        let safe_source = write_source(temp.path(), "safe.asi", b"safe mod");
        let provider = crate::providers::LegacySpProvider::new(game_root.clone());
        let mut conn = crate::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status, mode) \
             VALUES ('Old', 'asi', ?1, 'active', 'legacy-sp')",
            [game_root.to_str().unwrap()],
        )
        .unwrap();
        let mod_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO installed_mod_file \
             (installed_mod_id, target_path, backup_path, file_hash) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                mod_id,
                old_target.to_str().unwrap(),
                old_backup.to_str().unwrap(),
                hash_file(&old_target).unwrap()
            ],
        )
        .unwrap();
        assert!(crate::providers::ensure_installed_mod_available(&conn, mod_id).is_ok());
        let tree_before = snapshot_backup_tree(temp.path());
        let database_before = snapshot_backup_database(&conn);
        let changes_before: i64 = conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        let result = reinstall(
            &mut conn,
            mod_id,
            &package,
            "v2",
            &provider,
            &game_root,
            &backup_root,
            &recycle_root,
            InstallOptions::default(),
        );
        let expected_error = format!(
            "reinstall source overlaps an old backup: {}",
            old_backup.display()
        );
        assert!(matches!(
            result,
            Err(CoreError::UnsupportedFormat { reason }) if reason == expected_error
        ));
        assert_eq!(snapshot_backup_tree(temp.path()), tree_before);
        assert_eq!(snapshot_backup_database(&conn), database_before);
        let changes_after: i64 = conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        assert_eq!(changes_after, changes_before);
        let control = reinstall(
            &mut conn,
            mod_id,
            &safe_source,
            "v2",
            &provider,
            &game_root,
            &backup_root,
            &recycle_root,
            InstallOptions::default(),
        )
        .unwrap();
        assert!(matches!(control, InstallOutcome::Success { .. }));
        assert_eq!(std::fs::read(&package_payload).unwrap(), b"new mod");
        assert_eq!(
            std::fs::read(game_root.join("safe.asi")).unwrap(),
            b"safe mod"
        );
    }

    // OpenAI-Codex: Disabled removal does not consume the recorded old backup.
    #[test]
    fn disabled_reinstall_can_use_the_old_backup_as_its_source() {
        let temp = tempfile::tempdir().unwrap();
        let game_root = temp.path().join("game");
        let backup_root = temp.path().join("new_backups");
        let recycle_root = temp.path().join("recycle");
        let staging_root = temp.path().join("staging");
        std::fs::create_dir(&game_root).unwrap();
        let old_target = write_source(&game_root, "old.asi", b"old mod");
        let old_backup = write_source(temp.path(), "new_package.asi", b"old original");
        let provider = crate::providers::LegacySpProvider::new(game_root.clone());
        let mut conn = crate::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status, mode) \
             VALUES ('Old', 'asi', ?1, 'active', 'legacy-sp')",
            [game_root.to_str().unwrap()],
        )
        .unwrap();
        let mod_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO installed_mod_file \
             (installed_mod_id, target_path, backup_path, file_hash) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                mod_id,
                old_target.to_str().unwrap(),
                old_backup.to_str().unwrap(),
                hash_file(&old_target).unwrap()
            ],
        )
        .unwrap();
        let fixture_stage = staging_root.join(mod_id.to_string());
        std::fs::create_dir_all(&fixture_stage).unwrap();
        std::fs::rename(&old_target, fixture_stage.join("0")).unwrap();
        std::fs::write(&old_target, b"old original").unwrap();
        crate::uninstall::seed_disabled_fixture_from_staging(&conn, mod_id, &staging_root);
        assert_eq!(
            crate::state::disabled_payloads(&conn, mod_id, &staging_root)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(std::fs::read(&old_target).unwrap(), b"old original");
        let manifest_before =
            std::fs::read(staging_root.join(mod_id.to_string()).join("manifest.json")).unwrap();
        let result = reinstall_with_staging(
            &mut conn,
            mod_id,
            &old_backup,
            "v2",
            &provider,
            &game_root,
            &backup_root,
            &recycle_root,
            InstallOptions::default(),
            &staging_root,
        )
        .unwrap();
        assert!(matches!(result, InstallOutcome::Success { .. }));
        assert_eq!(std::fs::read(&old_backup).unwrap(), b"old original");
        assert_eq!(std::fs::read(&old_target).unwrap(), b"old original");
        assert_eq!(
            std::fs::read(game_root.join("new_package.asi")).unwrap(),
            b"old original"
        );
        assert_eq!(
            std::fs::read(staging_root.join(mod_id.to_string()).join("0")).unwrap(),
            b"old mod"
        );
        assert_eq!(
            std::fs::read(staging_root.join(mod_id.to_string()).join("manifest.json")).unwrap(),
            manifest_before
        );
        let old_status: String = conn
            .query_row(
                "SELECT status FROM installed_mod WHERE id = ?1",
                [mod_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_status, "uninstalled");
        let snapshot_dir: String = conn
            .query_row(
                "SELECT mod_package_snapshot_path FROM recycle_bin_entry WHERE original_installed_mod_id = ?1",
                [mod_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            std::fs::read(Path::new(&snapshot_dir).join("old.asi")).unwrap(),
            b"old mod"
        );
    }

    // OpenAI-Codex_lml_guard_audit: Reject active snapshot collisions before DLC intent.
    #[test]
    fn reinstall_checks_active_snapshot_paths_before_manual_dlc_intent() {
        for overlapping in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let game_root = temp.path().join("game");
            let pack_root = game_root.join("mods/update/x64/dlcpacks/CollisionPack");
            let backup_root = temp.path().join("backups");
            let recycle_root = temp.path().join("recycle");
            std::fs::create_dir_all(&pack_root).unwrap();
            let source = write_source(temp.path(), "new.asi", b"new payload");
            let first = write_source(&pack_root, "dlc.rpf", b"old first");
            let second = if overlapping {
                first.clone()
            } else {
                write_source(&pack_root, "extra.rpf", b"old second")
            };

            let mut conn = crate::db::open_in_memory().unwrap();
            conn.execute(
                "INSERT INTO installed_mod (name, source_type, install_path, status, mode) \
                 VALUES ('Active DLC', 'folder', ?1, 'active', 'legacy-sp')",
                [game_root.to_str().unwrap()],
            )
            .unwrap();
            let mod_id = conn.last_insert_rowid();
            for target in [&first, &second] {
                conn.execute(
                    "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) \
                     VALUES (?1, ?2, ?3)",
                    rusqlite::params![mod_id, target.to_str().unwrap(), hash_file(target).unwrap()],
                )
                .unwrap();
            }
            let file_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM installed_mod_file WHERE installed_mod_id = ?1",
                    [mod_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(file_count, 2);
            assert!(crate::providers::ensure_installed_mod_available(&conn, mod_id).is_ok());
            assert_eq!(
                crate::dlc_manual::pack_names_for_mod(&conn, mod_id, &game_root).unwrap(),
                vec!["CollisionPack".to_string()]
            );
            let tree_before = snapshot_backup_tree(temp.path());
            let database_before = snapshot_backup_database(&conn);
            let changes_before: i64 = conn
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .unwrap();
            let intents_before: i64 = conn
                .query_row("SELECT COUNT(*) FROM manual_dlc_event", [], |row| {
                    row.get(0)
                })
                .unwrap();

            let provider = crate::providers::LegacySpProvider::new(game_root.clone());
            let result = reinstall(
                &mut conn,
                mod_id,
                &source,
                "v2",
                &provider,
                &game_root,
                &backup_root,
                &recycle_root,
                InstallOptions::default(),
            );
            if overlapping {
                let relative = second.strip_prefix(&game_root).unwrap();
                let expected_error =
                    format!("recycle snapshot paths overlap: {}", relative.display());
                assert!(matches!(
                    result,
                    Err(CoreError::UnsupportedFormat { reason }) if reason == expected_error
                ));
                assert_eq!(snapshot_backup_tree(temp.path()), tree_before);
                assert_eq!(snapshot_backup_database(&conn), database_before);
                let changes_after: i64 = conn
                    .query_row("SELECT total_changes()", [], |row| row.get(0))
                    .unwrap();
                let intents_after: i64 = conn
                    .query_row("SELECT COUNT(*) FROM manual_dlc_event", [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                assert_eq!(changes_after, changes_before);
                assert_eq!(intents_after, intents_before);
                let status: String = conn
                    .query_row(
                        "SELECT status FROM installed_mod WHERE id = ?1",
                        [mod_id],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(status, "active");
                assert_eq!(std::fs::read(&first).unwrap(), b"old first");
                assert!(!game_root.join("new.asi").exists());
                assert!(!recycle_root.exists());
            } else {
                let new_mod_id = match result {
                    Ok(InstallOutcome::Success {
                        installed_mod_id,
                        files_written,
                    }) => {
                        assert_eq!(files_written, 1);
                        installed_mod_id
                    }
                    other => panic!("expected a successful disjoint reinstall: {other:?}"),
                };
                let old_status: String = conn
                    .query_row(
                        "SELECT status FROM installed_mod WHERE id = ?1",
                        [mod_id],
                        |row| row.get(0),
                    )
                    .unwrap();
                let new_status: String = conn
                    .query_row(
                        "SELECT status FROM installed_mod WHERE id = ?1",
                        [new_mod_id],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(old_status, "uninstalled");
                assert_eq!(new_status, "active");
                let phases = {
                    let mut statement = conn
                        .prepare(
                            "SELECT phase FROM manual_dlc_event \
                             WHERE operation = 'reinstall' AND installed_mod_id = ?1 \
                             AND pack_display = 'CollisionPack' ORDER BY id",
                        )
                        .unwrap();
                    let phases = statement
                        .query_map([mod_id], |row| row.get::<_, String>(0))
                        .unwrap()
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .unwrap();
                    phases
                };
                assert_eq!(phases, vec!["intent".to_string(), "settled".to_string()]);
                let snapshot: String = conn
                    .query_row(
                        "SELECT mod_package_snapshot_path FROM recycle_bin_entry \
                         WHERE original_installed_mod_id = ?1",
                        [mod_id],
                        |row| row.get(0),
                    )
                    .unwrap();
                let snapshot = PathBuf::from(snapshot);
                assert_eq!(
                    std::fs::read(snapshot.join(first.strip_prefix(&game_root).unwrap())).unwrap(),
                    b"old first"
                );
                assert_eq!(
                    std::fs::read(snapshot.join(second.strip_prefix(&game_root).unwrap())).unwrap(),
                    b"old second"
                );
                assert!(!first.exists());
                assert!(!second.exists());
                assert_eq!(
                    std::fs::read(game_root.join("new.asi")).unwrap(),
                    b"new payload"
                );
            }
        }
    }

    // OpenAI-Codex: Reinstall must reject snapshot collisions before DLC intent.
    #[test]
    fn reinstall_checks_disabled_snapshot_paths_before_manual_dlc_intent() {
        for overlapping in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let game_root = temp.path().join("game");
            let pack_root = game_root.join("mods/update/x64/dlcpacks/CollisionPack");
            let staging_root = temp.path().join("staging");
            let backup_root = temp.path().join("backups");
            let recycle_root = temp.path().join("recycle");
            std::fs::create_dir_all(&pack_root).unwrap();
            let source = write_source(temp.path(), "new.asi", b"new payload");
            let mut conn = crate::db::open_in_memory().unwrap();
            conn.execute(
                "INSERT INTO installed_mod (name, source_type, install_path, status, mode) \
                 VALUES ('Old DLC', 'folder', ?1, 'disabled', 'legacy-sp')",
                [game_root.to_str().unwrap()],
            )
            .unwrap();
            let mod_id = conn.last_insert_rowid();
            let stage = staging_root.join(mod_id.to_string());
            std::fs::create_dir_all(&stage).unwrap();
            // OpenAI-Codex_lml_guard_audit: This fixture uses legacy staging without a manifest.
            assert!(!stage.join("manifest.json").exists());
            let first = pack_root.join("dlc.rpf");
            let second = if overlapping {
                first.clone()
            } else {
                pack_root.join("extra.rpf")
            };
            for (index, (target, payload)) in [
                (&first, b"first staged".as_slice()),
                (&second, b"second staged".as_slice()),
            ]
            .into_iter()
            .enumerate()
            {
                let staged = stage.join(index.to_string());
                std::fs::write(&staged, payload).unwrap();
                conn.execute(
                    "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) \
                     VALUES (?1, ?2, ?3)",
                    rusqlite::params![
                        mod_id,
                        target.to_str().unwrap(),
                        hash_file(&staged).unwrap()
                    ],
                )
                .unwrap();
            }
            assert_eq!(
                crate::state::disabled_payloads(&conn, mod_id, &staging_root)
                    .unwrap()
                    .len(),
                2
            );
            assert_eq!(
                crate::dlc_manual::pack_names_for_mod(&conn, mod_id, &game_root).unwrap(),
                vec!["CollisionPack".to_string()]
            );
            assert!(crate::providers::ensure_installed_mod_available(&conn, mod_id).is_ok());
            let tree_before = snapshot_backup_tree(temp.path());
            let database_before = snapshot_backup_database(&conn);
            let changes_before: i64 = conn
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .unwrap();
            let intents_before: i64 = conn
                .query_row("SELECT COUNT(*) FROM manual_dlc_event", [], |row| {
                    row.get(0)
                })
                .unwrap();
            let result = reinstall_with_staging(
                &mut conn,
                mod_id,
                &source,
                "v2",
                &crate::providers::LegacySpProvider::new(game_root.clone()),
                &game_root,
                &backup_root,
                &recycle_root,
                InstallOptions::default(),
                &staging_root,
            );
            if overlapping {
                let relative = second.strip_prefix(&game_root).unwrap();
                let expected_error =
                    format!("recycle snapshot paths overlap: {}", relative.display());
                assert!(matches!(
                    result,
                    Err(CoreError::UnsupportedFormat { reason }) if reason == expected_error
                ));
                assert_eq!(snapshot_backup_tree(temp.path()), tree_before);
                assert_eq!(snapshot_backup_database(&conn), database_before);
                let changes_after: i64 = conn
                    .query_row("SELECT total_changes()", [], |row| row.get(0))
                    .unwrap();
                let intents_after: i64 = conn
                    .query_row("SELECT COUNT(*) FROM manual_dlc_event", [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                assert_eq!(changes_after, changes_before);
                assert_eq!(intents_after, intents_before);
            } else {
                // OpenAI-Codex_lml_guard_audit: Verify the complete disjoint legacy outcome.
                let new_mod_id = match &result {
                    Ok(InstallOutcome::Success {
                        installed_mod_id, ..
                    }) => *installed_mod_id,
                    other => panic!("expected a successful legacy reinstall: {other:?}"),
                };
                let old_status: String = conn
                    .query_row(
                        "SELECT status FROM installed_mod WHERE id = ?1",
                        [mod_id],
                        |row| row.get(0),
                    )
                    .unwrap();
                let new_status: String = conn
                    .query_row(
                        "SELECT status FROM installed_mod WHERE id = ?1",
                        [new_mod_id],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(old_status, "uninstalled");
                assert_eq!(new_status, "active");
                let phases = {
                    let mut statement = conn
                        .prepare(
                            "SELECT operation_id, phase FROM manual_dlc_event \
                             WHERE operation = 'reinstall' AND installed_mod_id = ?1 \
                             AND pack_display = 'CollisionPack' ORDER BY id",
                        )
                        .unwrap();
                    let phases = statement
                        .query_map([mod_id], |row| {
                            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                        })
                        .unwrap()
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .unwrap();
                    phases
                };
                assert_eq!(phases.len(), 2);
                assert_eq!(phases[0].0, phases[1].0);
                assert_eq!(phases[0].1, "intent");
                assert_eq!(phases[1].1, "settled");
                let snapshot: String = conn
                    .query_row(
                        "SELECT mod_package_snapshot_path FROM recycle_bin_entry \
                         WHERE original_installed_mod_id = ?1",
                        [mod_id],
                        |row| row.get(0),
                    )
                    .unwrap();
                let snapshot = PathBuf::from(snapshot);
                assert_eq!(
                    std::fs::read(snapshot.join(first.strip_prefix(&game_root).unwrap())).unwrap(),
                    b"first staged"
                );
                assert_eq!(
                    std::fs::read(snapshot.join(second.strip_prefix(&game_root).unwrap())).unwrap(),
                    b"second staged"
                );
                assert!(!first.exists());
                assert!(!second.exists());
                assert!(!stage.join("manifest.json").exists());
                assert!(matches!(result, Ok(InstallOutcome::Success { .. })));
                let intents_after: i64 = conn
                    .query_row("SELECT COUNT(*) FROM manual_dlc_event", [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                assert!(intents_after > intents_before);
                assert_eq!(
                    std::fs::read(game_root.join("new.asi")).unwrap(),
                    b"new payload"
                );
                assert_eq!(std::fs::read(stage.join("0")).unwrap(), b"first staged");
                assert_eq!(std::fs::read(stage.join("1")).unwrap(), b"second staged");
            }
        }
    }

    // OpenAI-Codex_lml_guard_audit: Preserve real disabled manifests before DLC intent.
    #[test]
    fn reinstall_checks_disabled_manifest_snapshot_before_manual_dlc_intent() {
        for overlapping in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let game_root = temp.path().join("game");
            let pack_root = game_root.join("mods/update/x64/dlcpacks/CollisionPack");
            let staging_root = temp.path().join("staging");
            let backup_root = temp.path().join("backups");
            let recycle_root = temp.path().join("recycle");
            std::fs::create_dir_all(&pack_root).unwrap();
            let source = write_source(temp.path(), "new.asi", b"new payload");
            let first = write_source(&pack_root, "dlc.rpf", b"first payload");
            let second = write_source(&pack_root, "extra.rpf", b"second payload");
            let mut conn = crate::db::open_in_memory().unwrap();
            conn.execute(
                "INSERT INTO installed_mod (name, source_type, install_path, status, mode) \
                 VALUES ('Manifest DLC', 'folder', ?1, 'active', 'legacy-sp')",
                [game_root.to_str().unwrap()],
            )
            .unwrap();
            let mod_id = conn.last_insert_rowid();
            let mut file_ids = Vec::new();
            for target in [&first, &second] {
                conn.execute(
                    "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) \
                     VALUES (?1, ?2, ?3)",
                    rusqlite::params![mod_id, target.to_str().unwrap(), hash_file(target).unwrap()],
                )
                .unwrap();
                file_ids.push(conn.last_insert_rowid());
            }
            assert_ne!(file_ids[0], file_ids[1]);
            let fixture_stage = staging_root.join(mod_id.to_string());
            std::fs::create_dir_all(&fixture_stage).unwrap();
            std::fs::rename(&first, fixture_stage.join("0")).unwrap();
            std::fs::rename(&second, fixture_stage.join("1")).unwrap();
            crate::uninstall::seed_disabled_fixture_from_staging(&conn, mod_id, &staging_root);
            let stage = staging_root.join(mod_id.to_string());
            let manifest_path = stage.join("manifest.json");
            let manifest_before = std::fs::read(&manifest_path).unwrap();
            assert_eq!(std::fs::read(stage.join("0")).unwrap(), b"first payload");
            assert_eq!(std::fs::read(stage.join("1")).unwrap(), b"second payload");
            assert!(!first.exists());
            assert!(!second.exists());

            if overlapping {
                let affected = conn
                    .execute(
                        "UPDATE installed_mod_file SET target_path = ?1 \
                         WHERE id = ?2 AND installed_mod_id = ?3",
                        rusqlite::params![first.to_str().unwrap(), file_ids[1], mod_id],
                    )
                    .unwrap();
                assert_eq!(affected, 1);
            }
            let payloads = crate::state::disabled_payloads(&conn, mod_id, &staging_root).unwrap();
            assert_eq!(
                payloads.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
                file_ids
            );
            let status: String = conn
                .query_row(
                    "SELECT status FROM installed_mod WHERE id = ?1",
                    [mod_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(status, "disabled");
            assert!(crate::providers::ensure_installed_mod_available(&conn, mod_id).is_ok());
            assert_eq!(
                crate::dlc_manual::pack_names_for_mod(&conn, mod_id, &game_root).unwrap(),
                vec!["CollisionPack".to_string()]
            );
            let tree_before = snapshot_backup_tree(temp.path());
            let database_before = snapshot_backup_database(&conn);
            let changes_before: i64 = conn
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .unwrap();
            let events_before: i64 = conn
                .query_row("SELECT COUNT(*) FROM manual_dlc_event", [], |row| {
                    row.get(0)
                })
                .unwrap();
            let provider = crate::providers::LegacySpProvider::new(game_root.clone());
            let result = reinstall_with_staging(
                &mut conn,
                mod_id,
                &source,
                "v2",
                &provider,
                &game_root,
                &backup_root,
                &recycle_root,
                InstallOptions::default(),
                &staging_root,
            );
            if overlapping {
                let relative = first.strip_prefix(&game_root).unwrap();
                let expected_error =
                    format!("recycle snapshot paths overlap: {}", relative.display());
                assert!(matches!(
                    result,
                    Err(CoreError::UnsupportedFormat { reason }) if reason == expected_error
                ));
                assert_eq!(snapshot_backup_tree(temp.path()), tree_before);
                assert_eq!(snapshot_backup_database(&conn), database_before);
                let changes_after: i64 = conn
                    .query_row("SELECT total_changes()", [], |row| row.get(0))
                    .unwrap();
                let events_after: i64 = conn
                    .query_row("SELECT COUNT(*) FROM manual_dlc_event", [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                assert_eq!(changes_after, changes_before);
                assert_eq!(events_after, events_before);
                let status: String = conn
                    .query_row(
                        "SELECT status FROM installed_mod WHERE id = ?1",
                        [mod_id],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(status, "disabled");
                assert_eq!(std::fs::read(&manifest_path).unwrap(), manifest_before);
                assert!(!recycle_root.exists());
                assert!(!game_root.join("new.asi").exists());
            } else {
                let new_mod_id = match result {
                    Ok(InstallOutcome::Success {
                        installed_mod_id,
                        files_written,
                    }) => {
                        assert_eq!(files_written, 1);
                        installed_mod_id
                    }
                    other => panic!("expected a successful manifest reinstall: {other:?}"),
                };
                let old_status: String = conn
                    .query_row(
                        "SELECT status FROM installed_mod WHERE id = ?1",
                        [mod_id],
                        |row| row.get(0),
                    )
                    .unwrap();
                let new_status: String = conn
                    .query_row(
                        "SELECT status FROM installed_mod WHERE id = ?1",
                        [new_mod_id],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(old_status, "uninstalled");
                assert_eq!(new_status, "active");
                let phases = {
                    let mut statement = conn
                        .prepare(
                            "SELECT operation_id, phase FROM manual_dlc_event \
                             WHERE operation = 'reinstall' AND installed_mod_id = ?1 \
                             AND pack_display = 'CollisionPack' ORDER BY id",
                        )
                        .unwrap();
                    let phases = statement
                        .query_map([mod_id], |row| {
                            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                        })
                        .unwrap()
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .unwrap();
                    phases
                };
                assert_eq!(phases.len(), 2);
                assert_eq!(phases[0].0, phases[1].0);
                assert_eq!(phases[0].1, "intent");
                assert_eq!(phases[1].1, "settled");
                let snapshot: String = conn
                    .query_row(
                        "SELECT mod_package_snapshot_path FROM recycle_bin_entry \
                         WHERE original_installed_mod_id = ?1",
                        [mod_id],
                        |row| row.get(0),
                    )
                    .unwrap();
                let snapshot = PathBuf::from(snapshot);
                assert_eq!(
                    std::fs::read(snapshot.join(first.strip_prefix(&game_root).unwrap())).unwrap(),
                    b"first payload"
                );
                assert_eq!(
                    std::fs::read(snapshot.join(second.strip_prefix(&game_root).unwrap())).unwrap(),
                    b"second payload"
                );
                assert_eq!(std::fs::read(&manifest_path).unwrap(), manifest_before);
                assert_eq!(std::fs::read(stage.join("0")).unwrap(), b"first payload");
                assert_eq!(std::fs::read(stage.join("1")).unwrap(), b"second payload");
                assert!(!first.exists());
                assert!(!second.exists());
                assert_eq!(
                    std::fs::read(game_root.join("new.asi")).unwrap(),
                    b"new payload"
                );
            }
        }
    }

    // OpenAI-Codex_lml_guard_audit: An uninstalled mod has no old snapshot to create.
    #[test]
    fn reinstall_of_uninstalled_mod_skips_old_snapshot_preflight() {
        fn old_rows(
            conn: &rusqlite::Connection,
            mod_id: i64,
        ) -> (
            Vec<rusqlite::types::Value>,
            Vec<Vec<rusqlite::types::Value>>,
        ) {
            let mut mod_statement = conn
                .prepare("SELECT * FROM installed_mod WHERE id = ?1")
                .unwrap();
            let mod_columns = mod_statement.column_count();
            let old_mod = mod_statement
                .query_row([mod_id], |row| {
                    (0..mod_columns)
                        .map(|index| row.get::<_, rusqlite::types::Value>(index))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap();
            drop(mod_statement);
            let mut file_statement = conn
                .prepare("SELECT * FROM installed_mod_file WHERE installed_mod_id = ?1 ORDER BY id")
                .unwrap();
            let file_columns = file_statement.column_count();
            let old_files = file_statement
                .query_map([mod_id], |row| {
                    (0..file_columns)
                        .map(|index| row.get::<_, rusqlite::types::Value>(index))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            (old_mod, old_files)
        }

        let temp = tempfile::tempdir().unwrap();
        let game_root = temp.path().join("game");
        let pack_root = game_root.join("mods/update/x64/dlcpacks/CollisionPack");
        let backup_root = temp.path().join("backups");
        let recycle_root = temp.path().join("recycle");
        std::fs::create_dir_all(&pack_root).unwrap();
        let old_target = pack_root.join("dlc.rpf");
        let unrelated = write_source(&pack_root, "unrelated.rpf", b"unrelated data");
        let source = write_source(temp.path(), "new.asi", b"new payload");
        assert!(!old_target.exists());
        let mut conn = crate::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status, mode) \
             VALUES ('Removed DLC', 'folder', ?1, 'uninstalled', 'legacy-sp')",
            [game_root.to_str().unwrap()],
        )
        .unwrap();
        let mod_id = conn.last_insert_rowid();
        for hash in ["first legacy hash", "second legacy hash"] {
            conn.execute(
                "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) \
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![mod_id, old_target.to_str().unwrap(), hash],
            )
            .unwrap();
        }
        let before = old_rows(&conn, mod_id);
        assert_eq!(before.1.len(), 2);
        assert!(crate::providers::ensure_installed_mod_available(&conn, mod_id).is_ok());
        assert_eq!(
            crate::dlc_manual::pack_names_for_mod(&conn, mod_id, &game_root).unwrap(),
            vec!["CollisionPack".to_string()]
        );
        let relative = old_target.strip_prefix(&game_root).unwrap();
        let expected_error = format!("recycle snapshot paths overlap: {}", relative.display());
        let preflight =
            crate::uninstall::preflight_recycle_snapshot_paths(&conn, mod_id, &game_root);
        assert!(matches!(
            preflight,
            Err(CoreError::UnsupportedFormat { reason }) if reason == expected_error
        ));
        let recycle_before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM recycle_bin_entry WHERE original_installed_mod_id = ?1",
                [mod_id],
                |row| row.get(0),
            )
            .unwrap();

        let provider = crate::providers::LegacySpProvider::new(game_root.clone());
        let result = reinstall(
            &mut conn,
            mod_id,
            &source,
            "v2",
            &provider,
            &game_root,
            &backup_root,
            &recycle_root,
            InstallOptions::default(),
        );
        let new_mod_id = match result {
            Ok(InstallOutcome::Success {
                installed_mod_id,
                files_written,
            }) => {
                assert_eq!(files_written, 1);
                installed_mod_id
            }
            other => panic!("expected an uninstalled mod to reinstall: {other:?}"),
        };
        assert_ne!(new_mod_id, mod_id);
        let status: String = conn
            .query_row(
                "SELECT status FROM installed_mod WHERE id = ?1",
                [new_mod_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "active");
        assert_eq!(old_rows(&conn, mod_id), before);
        assert!(!old_target.exists());
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"unrelated data");
        assert_eq!(std::fs::read(&source).unwrap(), b"new payload");
        assert_eq!(
            std::fs::read(game_root.join("new.asi")).unwrap(),
            b"new payload"
        );
        let recycle_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM recycle_bin_entry WHERE original_installed_mod_id = ?1",
                [mod_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(recycle_after, recycle_before);
        assert!(!recycle_root.exists());
        // OpenAI-Codex: Characterize historical reminders, not pack removal.
        let phases = {
            let mut statement = conn
                .prepare(
                    "SELECT operation_id, phase FROM manual_dlc_event \
                     WHERE operation = 'reinstall' AND installed_mod_id = ?1 \
                     AND pack_display = 'CollisionPack' ORDER BY id",
                )
                .unwrap();
            let phases = statement
                .query_map([mod_id], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            phases
        };
        assert_eq!(phases.len(), 2);
        assert_eq!(phases[0].0, phases[1].0);
        assert_eq!(phases[0].1, "intent");
        assert_eq!(phases[1].1, "settled");
    }

    // OpenAI-Codex: A missing later source cannot be produced by a backup slot.
    #[test]
    fn install_refuses_a_source_that_is_a_later_backup_slot() {
        let temp = tempfile::tempdir().unwrap();
        let game_root = temp.path().join("game");
        let backup_root = temp.path().join("backups");
        std::fs::create_dir(&game_root).unwrap();
        let source_a = write_source(temp.path(), "source_a.asi", b"new A");
        let safe_source_b = write_source(temp.path(), "source_b.asi", b"new B");
        let target_a = write_source(&game_root, "a.asi", b"original A");
        let target_b = write_source(&game_root, "b.asi", b"original B");
        let missing_source = backup_root.join("0.bak");
        assert!(!missing_source.exists());
        let plan = ModPlan::new(
            ModFormat::Asi,
            vec![
                PlannedFile {
                    source: source_a.clone(),
                    target: target_a.clone(),
                },
                PlannedFile {
                    source: missing_source.clone(),
                    target: target_b.clone(),
                },
            ],
        );
        let mut conn = crate::db::open_in_memory().unwrap();
        let tree_before = snapshot_backup_tree(temp.path());
        let database_before = snapshot_backup_database(&conn);
        let result = install(
            &mut conn,
            "Backup as later source",
            &plan,
            &game_root,
            &backup_root,
            InstallOptions {
                auto_backup: true,
                override_foreign_conflicts: true,
            },
            &source_a,
        );
        let expected_error = format!(
            "planned source overlaps backup destination: {}",
            missing_source.display()
        );
        assert!(matches!(
            result,
            Err(CoreError::UnsupportedFormat { reason }) if reason == expected_error
        ));
        assert_eq!(snapshot_backup_tree(temp.path()), tree_before);
        assert_eq!(snapshot_backup_database(&conn), database_before);
        let safe_plan = ModPlan::new(
            ModFormat::Asi,
            vec![
                PlannedFile {
                    source: source_a.clone(),
                    target: target_a.clone(),
                },
                PlannedFile {
                    source: safe_source_b.clone(),
                    target: target_b.clone(),
                },
            ],
        );
        let control = install(
            &mut conn,
            "Separate later source",
            &safe_plan,
            &game_root,
            &backup_root,
            InstallOptions {
                auto_backup: true,
                override_foreign_conflicts: true,
            },
            &source_a,
        )
        .unwrap();
        assert!(matches!(control, InstallOutcome::Success { .. }));
        assert_eq!(std::fs::read(&target_a).unwrap(), b"new A");
        assert_eq!(std::fs::read(&target_b).unwrap(), b"new B");
        assert_eq!(
            std::fs::read(backup_root.join("0.bak")).unwrap(),
            b"original A"
        );
    }

    // OpenAI-Codex: One planned target must not mutate a later planned source.
    #[test]
    fn install_refuses_a_planned_source_that_is_another_target() {
        for auto_backup in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let game_root = temp.path().join("game");
            let backup_root = temp.path().join("backups");
            std::fs::create_dir(&game_root).unwrap();
            let source_a = write_source(temp.path(), "source_a.asi", b"new A");
            let source_b = write_source(temp.path(), "source_b.asi", b"new B");
            let target_a = write_source(&game_root, "a.asi", b"original A");
            let target_b = write_source(&game_root, "b.asi", b"original B");
            let plan = ModPlan::new(
                ModFormat::Asi,
                vec![
                    PlannedFile {
                        source: source_a.clone(),
                        target: target_a.clone(),
                    },
                    PlannedFile {
                        source: target_a.clone(),
                        target: target_b.clone(),
                    },
                ],
            );
            let mut conn = crate::db::open_in_memory().unwrap();
            let tree_before = snapshot_backup_tree(temp.path());
            let database_before = snapshot_backup_database(&conn);
            let changes_before: i64 = conn
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .unwrap();
            let result = install(
                &mut conn,
                "Overlapping sources",
                &plan,
                &game_root,
                &backup_root,
                InstallOptions {
                    auto_backup,
                    override_foreign_conflicts: true,
                },
                &source_a,
            );
            let expected_error = format!(
                "planned source overlaps a planned target: {}",
                target_a.display()
            );
            assert!(matches!(
                result,
                Err(CoreError::UnsupportedFormat { reason }) if reason == expected_error
            ));
            assert_eq!(snapshot_backup_tree(temp.path()), tree_before);
            assert_eq!(snapshot_backup_database(&conn), database_before);
            let changes_after: i64 = conn
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .unwrap();
            assert_eq!(changes_after, changes_before);

            let safe_plan = ModPlan::new(
                ModFormat::Asi,
                vec![
                    PlannedFile {
                        source: source_a.clone(),
                        target: target_a.clone(),
                    },
                    PlannedFile {
                        source: source_b.clone(),
                        target: target_b.clone(),
                    },
                ],
            );
            let control = install(
                &mut conn,
                "Separate sources",
                &safe_plan,
                &game_root,
                &backup_root,
                InstallOptions {
                    auto_backup,
                    override_foreign_conflicts: true,
                },
                &source_a,
            )
            .unwrap();
            assert!(matches!(control, InstallOutcome::Success { .. }));
            assert_eq!(std::fs::read(&target_a).unwrap(), b"new A");
            assert_eq!(std::fs::read(&target_b).unwrap(), b"new B");
            assert_eq!(std::fs::read(&source_a).unwrap(), b"new A");
            assert_eq!(std::fs::read(&source_b).unwrap(), b"new B");
        }
    }

    // OpenAI-Codex: Keep the caller's original archive even when payloads are extracted.
    #[test]
    fn install_refuses_a_target_that_is_the_original_package() {
        for auto_backup in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let game_root = temp.path().join("game");
            let backup_root = temp.path().join("backups");
            std::fs::create_dir(&game_root).unwrap();
            let archive = write_source(&game_root, "package.zip", b"original archive");
            let payload = write_source(temp.path(), "extracted.asi", b"extracted payload");
            let plan = ModPlan::new(
                ModFormat::Asi,
                vec![PlannedFile {
                    source: payload.clone(),
                    target: archive.clone(),
                }],
            );
            let mut conn = crate::db::open_in_memory().unwrap();
            let tree_before = snapshot_backup_tree(temp.path());
            let database_before = snapshot_backup_database(&conn);
            let changes_before: i64 = conn
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .unwrap();
            let result = install(
                &mut conn,
                "Original package target",
                &plan,
                &game_root,
                &backup_root,
                InstallOptions {
                    auto_backup,
                    override_foreign_conflicts: true,
                },
                &archive,
            );
            let expected_error = format!(
                "planned target overlaps the original package: {}",
                archive.display()
            );
            assert!(matches!(
                result,
                Err(CoreError::UnsupportedFormat { reason }) if reason == expected_error
            ));
            assert_eq!(snapshot_backup_tree(temp.path()), tree_before);
            assert_eq!(snapshot_backup_database(&conn), database_before);
            let changes_after: i64 = conn
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .unwrap();
            assert_eq!(changes_after, changes_before);

            let safe_target = game_root.join("safe.asi");
            let safe_plan = ModPlan::new(
                ModFormat::Asi,
                vec![PlannedFile {
                    source: payload.clone(),
                    target: safe_target.clone(),
                }],
            );
            let control = install(
                &mut conn,
                "Separate package",
                &safe_plan,
                &game_root,
                &backup_root,
                InstallOptions {
                    auto_backup,
                    override_foreign_conflicts: true,
                },
                &archive,
            )
            .unwrap();
            assert!(matches!(control, InstallOutcome::Success { .. }));
            assert_eq!(std::fs::read(&safe_target).unwrap(), b"extracted payload");
            assert_eq!(std::fs::read(&archive).unwrap(), b"original archive");
        }
    }

    // OpenAI-Codex: An install must not create records with overlapping targets.
    #[test]
    fn install_refuses_overlapping_planned_targets() {
        for (first, second) in [
            ("shared.asi", "shared.asi"),
            ("Plugins/Callout.dll", "plugins/callout.dll"),
            ("parent", "parent/child.asi"),
            ("parent/child.asi", "parent"),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let game_root = temp.path().join("game");
            let backup_root = temp.path().join("backups");
            std::fs::create_dir(&game_root).unwrap();
            let source_a = write_source(temp.path(), "source_a.asi", b"new A");
            let source_b = write_source(temp.path(), "source_b.asi", b"new B");
            let target_a = game_root.join(first);
            let target_b = game_root.join(second);
            let plan = ModPlan::new(
                ModFormat::Asi,
                vec![
                    PlannedFile {
                        source: source_a.clone(),
                        target: target_a,
                    },
                    PlannedFile {
                        source: source_b.clone(),
                        target: target_b.clone(),
                    },
                ],
            );
            let mut conn = crate::db::open_in_memory().unwrap();
            let tree_before = snapshot_backup_tree(temp.path());
            let database_before = snapshot_backup_database(&conn);
            let changes_before: i64 = conn
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .unwrap();
            let result = install(
                &mut conn,
                "Overlapping targets",
                &plan,
                &game_root,
                &backup_root,
                InstallOptions {
                    auto_backup: true,
                    override_foreign_conflicts: true,
                },
                &source_a,
            );
            let expected_error = format!("planned install targets overlap: {}", target_b.display());
            assert!(
                matches!(result, Err(CoreError::UnsupportedFormat { reason }) if reason == expected_error),
                "first={first}, second={second}"
            );
            assert_eq!(snapshot_backup_tree(temp.path()), tree_before);
            assert_eq!(snapshot_backup_database(&conn), database_before);
            let changes_after: i64 = conn
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .unwrap();
            assert_eq!(changes_after, changes_before);

            let safe_a = game_root.join("parent");
            let safe_b = game_root.join("parent2/child.asi");
            let safe_plan = ModPlan::new(
                ModFormat::Asi,
                vec![
                    PlannedFile {
                        source: source_a.clone(),
                        target: safe_a.clone(),
                    },
                    PlannedFile {
                        source: source_b.clone(),
                        target: safe_b.clone(),
                    },
                ],
            );
            let control = install(
                &mut conn,
                "Distinct targets",
                &safe_plan,
                &game_root,
                &backup_root,
                InstallOptions {
                    auto_backup: true,
                    override_foreign_conflicts: true,
                },
                &source_a,
            )
            .unwrap();
            assert!(matches!(control, InstallOutcome::Success { .. }));
            assert_eq!(std::fs::read(&safe_a).unwrap(), b"new A");
            assert_eq!(std::fs::read(&safe_b).unwrap(), b"new B");
        }
    }

    // OpenAI-Codex: Missing backups may still be referenced by another mod.
    #[test]
    fn install_preserves_missing_backup_paths_referenced_by_other_mods() {
        for owner_disabled in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let game_root = temp.path().join("game");
            let backup_root = temp.path().join("backups");
            let staging_root = temp.path().join("staging");
            std::fs::create_dir(&game_root).unwrap();
            std::fs::create_dir(&backup_root).unwrap();
            let missing_backup = backup_root.join("0.bak");
            let owner_target = write_source(&game_root, "owner.asi", b"owner payload");
            let source = write_source(temp.path(), "new.asi", b"new payload");
            let target = write_source(&game_root, "candidate.asi", b"existing candidate");
            let mut conn = crate::db::open_in_memory().unwrap();
            conn.execute(
                "INSERT INTO installed_mod (name, source_type, install_path, status, mode) \
                 VALUES ('Owner', 'asi', ?1, 'active', 'legacy-sp')",
                [game_root.to_str().unwrap()],
            )
            .unwrap();
            let owner_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO installed_mod_file \
                 (installed_mod_id, target_path, backup_path, file_hash) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    owner_id,
                    owner_target.to_str().unwrap(),
                    missing_backup.to_str().unwrap(),
                    hash_file(&owner_target).unwrap()
                ],
            )
            .unwrap();
            if owner_disabled {
                let fixture_stage = staging_root.join(owner_id.to_string());
                std::fs::create_dir_all(&fixture_stage).unwrap();
                std::fs::rename(&owner_target, fixture_stage.join("0")).unwrap();
                crate::uninstall::seed_disabled_fixture_from_staging(
                    &conn,
                    owner_id,
                    &staging_root,
                );
                assert_eq!(
                    crate::state::disabled_payloads(&conn, owner_id, &staging_root)
                        .unwrap()
                        .len(),
                    1
                );
            }
            let expected_status = if owner_disabled { "disabled" } else { "active" };
            let owner_status: String = conn
                .query_row(
                    "SELECT status FROM installed_mod WHERE id = ?1",
                    [owner_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(owner_status, expected_status);
            assert!(!missing_backup.exists());
            assert!(crate::providers::ensure_installed_mod_available(&conn, owner_id).is_ok());
            assert!(crate::lml_store::ensure_generic_targets_allowed(
                &conn,
                &game_root,
                &[backup_root.clone(), missing_backup.clone()],
            )
            .is_ok());
            let report = conflict::analyze(&conn, std::slice::from_ref(&target)).unwrap();
            assert!(!report.has_protected_hits());
            assert_eq!(report.foreign_conflicts.len(), 1);
            assert_eq!(report.foreign_conflicts[0].owner_mod_id, None);
            let plan = ModPlan::new(
                ModFormat::Asi,
                vec![PlannedFile {
                    source: source.clone(),
                    target: target.clone(),
                }],
            );
            let tree_before = snapshot_backup_tree(temp.path());
            let database_before = snapshot_backup_database(&conn);
            let changes_before: i64 = conn
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .unwrap();
            let result = install(
                &mut conn,
                "Candidate",
                &plan,
                &game_root,
                &backup_root,
                InstallOptions {
                    auto_backup: true,
                    override_foreign_conflicts: true,
                },
                &source,
            );
            let expected_error = format!(
                "backup destination is already referenced: {}",
                missing_backup.display()
            );
            assert!(matches!(
                result,
                Err(CoreError::UnsupportedFormat { reason }) if reason == expected_error
            ));
            assert!(!missing_backup.exists());
            assert_eq!(snapshot_backup_tree(temp.path()), tree_before);
            assert_eq!(snapshot_backup_database(&conn), database_before);
            let changes_after: i64 = conn
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .unwrap();
            assert_eq!(changes_after, changes_before);

            let safe_backup = temp.path().join("safe_backup");
            let control = install(
                &mut conn,
                "Safe candidate",
                &plan,
                &game_root,
                &safe_backup,
                InstallOptions {
                    auto_backup: true,
                    override_foreign_conflicts: true,
                },
                &source,
            )
            .unwrap();
            assert!(matches!(control, InstallOutcome::Success { .. }));
            assert_eq!(std::fs::read(&target).unwrap(), b"new payload");
            assert_eq!(
                std::fs::read(safe_backup.join("0.bak")).unwrap(),
                b"existing candidate"
            );
            assert!(!missing_backup.exists());
            let owner_status_after: String = conn
                .query_row(
                    "SELECT status FROM installed_mod WHERE id = ?1",
                    [owner_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(owner_status_after, expected_status);
            if owner_disabled {
                assert_eq!(
                    crate::state::disabled_payloads(&conn, owner_id, &staging_root)
                        .unwrap()
                        .len(),
                    1
                );
            } else {
                assert_eq!(std::fs::read(&owner_target).unwrap(), b"owner payload");
            }
        }
    }

    // OpenAI-Codex_requirements_inventory: protect another game's reserved staging.
    #[test]
    fn install_refuses_backup_inside_another_game_lml_stage_before_writes() {
        use crate::lml_store::{
            begin_operation, load_operation, record_phase, NewOperation, OperationFile, Phase,
        };
        use sha2::{Digest, Sha256};

        type DbRow = Vec<rusqlite::types::Value>;
        fn snapshot_database(conn: &Connection) -> Vec<(String, Vec<DbRow>)> {
            let mut tables_statement = conn
                .prepare(
                    "SELECT name FROM sqlite_master WHERE type='table' \
                     AND (name NOT LIKE 'sqlite_%' OR name='sqlite_sequence') ORDER BY name",
                )
                .unwrap();
            let tables = tables_statement
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            drop(tables_statement);
            let mut snapshot = Vec::new();
            for table in tables {
                let escaped = table.replace('"', "\"\"");
                let sql = format!("SELECT rowid, * FROM \"{escaped}\" ORDER BY rowid");
                let mut statement = conn.prepare(&sql).unwrap();
                let count = statement.column_count();
                let rows = statement
                    .query_map([], |row| {
                        (0..count)
                            .map(|index| row.get::<_, rusqlite::types::Value>(index))
                            .collect::<rusqlite::Result<Vec<_>>>()
                    })
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap();
                snapshot.push((table, rows));
            }
            snapshot
        }

        // Store canonical local paths without the Windows extended-drive prefix.
        let canonical_text = |path: &Path| {
            let canonical = std::fs::canonicalize(path).unwrap();
            let text = canonical.to_str().unwrap();
            text.strip_prefix(r"\\?\").unwrap_or(text).to_string()
        };
        let temp_a = tempfile::tempdir().unwrap();
        let temp_b = tempfile::tempdir().unwrap();
        let root_a_dir = temp_a.path().join("game_a");
        let root_b_dir = temp_b.path().join("game_b");
        let stage_b_dir = temp_b.path().join("stage_b");
        std::fs::create_dir(&root_a_dir).unwrap();
        std::fs::create_dir(&root_b_dir).unwrap();
        std::fs::create_dir(&stage_b_dir).unwrap();
        let root_a = PathBuf::from(canonical_text(&root_a_dir));
        let root_b = PathBuf::from(canonical_text(&root_b_dir));
        let stage_b = PathBuf::from(canonical_text(&stage_b_dir));
        assert_ne!(root_a, root_b);

        let manifest = b"<EasyInstall><Name>Example</Name></EasyInstall>".to_vec();
        let manifest_hash = format!("{:x}", Sha256::digest(&manifest));
        let sentinel = stage_b.join("0.bak");
        std::fs::write(stage_b.join("install.xml"), &manifest).unwrap();
        std::fs::write(&sentinel, b"reserved B sentinel").unwrap();
        let operation = NewOperation {
            mode: "legacy-sp".to_string(),
            root_key: root_b.to_str().unwrap().to_string(),
            root_display: root_b.to_str().unwrap().to_string(),
            target: "PackageB".to_string(),
            staging_path: stage_b.to_str().unwrap().to_string(),
            manifest: manifest.clone(),
            manifest_sha256: manifest_hash.clone(),
            unresolved_coverage: true,
            files: vec![
                OperationFile {
                    relative_path: "install.xml".to_string(),
                    size: manifest.len() as u64,
                    sha256: manifest_hash,
                },
                OperationFile {
                    relative_path: "0.bak".to_string(),
                    size: b"reserved B sentinel".len() as u64,
                    sha256: hash_file(&sentinel).unwrap(),
                },
            ],
        };
        let mut conn = crate::db::open_in_memory().unwrap();
        let operation_id = begin_operation(&mut conn, &operation).unwrap();
        record_phase(&mut conn, operation_id, Phase::Prepared, Phase::Staged, "").unwrap();

        let source = write_source(temp_a.path(), "incoming.asi", b"incoming A");
        let target = root_a.join("existing.asi");
        std::fs::write(&target, b"original A").unwrap();
        assert!(crate::lml_store::ensure_generic_targets_allowed(
            &conn,
            &root_a,
            std::slice::from_ref(&target),
        )
        .is_ok());
        assert!(crate::lml_store::ensure_generic_targets_allowed(
            &conn,
            &root_a,
            std::slice::from_ref(&stage_b),
        )
        .is_err());
        let report = conflict::analyze(&conn, std::slice::from_ref(&target)).unwrap();
        assert!(!report.has_protected_hits());
        assert!(report
            .foreign_conflicts
            .iter()
            .any(|item| item.owner_mod_id.is_none()));
        let options = InstallOptions {
            auto_backup: true,
            override_foreign_conflicts: true,
        };

        // A non-overlapping backup remains allowed with the same B reservation.
        let control_target = root_a.join("control.asi");
        std::fs::write(&control_target, b"control original").unwrap();
        let control_plan = ModPlan::new(
            ModFormat::Asi,
            vec![PlannedFile {
                source: source.clone(),
                target: control_target.clone(),
            }],
        );
        let control_backup = temp_a.path().join("control_backup");
        let control = install(
            &mut conn,
            "Control",
            &control_plan,
            &root_a,
            &control_backup,
            options.clone(),
            &source,
        )
        .unwrap();
        assert!(matches!(control, InstallOutcome::Success { .. }));
        assert_eq!(
            std::fs::read(control_backup.join("0.bak")).unwrap(),
            b"control original"
        );
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"reserved B sentinel");

        // OpenAI-Codex: Compare every final entry in the reserved fixture.
        let snapshot_stage = |directory: &Path| {
            let mut entries = std::fs::read_dir(directory)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    assert!(entry.file_type().unwrap().is_file());
                    (entry.file_name(), std::fs::read(entry.path()).unwrap())
                })
                .collect::<Vec<_>>();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            entries
        };
        let stage_before = snapshot_stage(&stage_b);

        // OpenAI-Codex: Refuse the same backup before removing an old mod.
        let control_id = match &control {
            InstallOutcome::Success {
                installed_mod_id, ..
            } => *installed_mod_id,
            _ => panic!("the control installation must succeed"),
        };
        assert!(crate::providers::ensure_installed_mod_available(&conn, control_id).is_ok());
        let provider = crate::providers::LegacySpProvider::new(root_a.clone());
        let reinstall_target = root_a.join("incoming.asi");
        std::fs::write(&reinstall_target, b"original at new target").unwrap();
        let classified = crate::mod_analyzer::classify(&source, &provider).unwrap();
        assert_eq!(classified.files.len(), 1);
        assert_eq!(classified.files[0].target, reinstall_target);
        let reinstall_db_before = snapshot_database(&conn);
        let recycle_root = temp_a.path().join("reinstall_recycle");
        let reinstall_result = reinstall(
            &mut conn,
            control_id,
            &source,
            "v2",
            &provider,
            &root_a,
            &stage_b,
            &recycle_root,
            options.clone(),
        );
        assert!(matches!(
            &reinstall_result,
            Err(CoreError::UnsupportedFormat { reason })
                if reason == "LML metadata: generic target overlaps an LML reservation"
        ));
        assert_eq!(std::fs::read(&control_target).unwrap(), b"incoming A");
        assert_eq!(
            std::fs::read(control_backup.join("0.bak")).unwrap(),
            b"control original"
        );
        assert_eq!(std::fs::read(&source).unwrap(), b"incoming A");

        assert_eq!(
            std::fs::read(&reinstall_target).unwrap(),
            b"original at new target"
        );
        assert!(!recycle_root.exists());
        assert_eq!(snapshot_stage(&stage_b), stage_before);
        assert_eq!(snapshot_database(&conn), reinstall_db_before);

        let reservation_before = load_operation(&conn, operation_id).unwrap().unwrap();
        let database_before = snapshot_database(&conn);
        let plan = ModPlan::new(
            ModFormat::Asi,
            vec![PlannedFile {
                source: source.clone(),
                target: target.clone(),
            }],
        );
        let result = install(
            &mut conn,
            "Overlapping backup",
            &plan,
            &root_a,
            &stage_b,
            options,
            &source,
        );
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"reserved B sentinel");
        assert_eq!(std::fs::read(&target).unwrap(), b"original A");
        assert_eq!(std::fs::read(&source).unwrap(), b"incoming A");
        assert_eq!(
            std::fs::read(stage_b.join("install.xml")).unwrap(),
            manifest
        );
        assert_eq!(
            load_operation(&conn, operation_id).unwrap().unwrap(),
            reservation_before
        );
        assert_eq!(snapshot_database(&conn), database_before);
        // OpenAI-Codex: An unrelated error must not satisfy this regression.
        assert_eq!(snapshot_stage(&stage_b), stage_before);
        assert!(matches!(
            &result,
            Err(CoreError::UnsupportedFormat { reason })
                if reason == "LML metadata: generic target overlaps an LML reservation"
        ));

        assert!(
            matches!(
                result,
                Err(_)
                    | Ok(InstallOutcome::RequiresOverride(_))
                    | Ok(InstallOutcome::ProtectedFileBlocked(_))
            ),
            "a backup overlapping another LML reservation must be refused"
        );
        // OpenAI-Codex: A second active-owner context still allows a safe backup.
        let later_target = root_a.join("later_control.asi");
        std::fs::write(&later_target, b"later original").unwrap();
        let later_plan = ModPlan::new(
            ModFormat::Asi,
            vec![PlannedFile {
                source: source.clone(),
                target: later_target.clone(),
            }],
        );
        let later_backup = temp_a.path().join("later_control_backup");
        let later = install(
            &mut conn,
            "Later control",
            &later_plan,
            &root_a,
            &later_backup,
            InstallOptions {
                auto_backup: true,
                override_foreign_conflicts: true,
            },
            &source,
        )
        .unwrap();
        assert!(matches!(later, InstallOutcome::Success { .. }));
        assert_eq!(std::fs::read(&later_target).unwrap(), b"incoming A");
        assert_eq!(
            std::fs::read(later_backup.join("0.bak")).unwrap(),
            b"later original"
        );
        assert_eq!(snapshot_stage(&stage_b), stage_before);
    }
}
