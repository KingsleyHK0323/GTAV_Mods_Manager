// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex
// OpenAI-Codex_Responsible: Source_Code

//! The install pipeline state machine: Analyzing (done by the caller via
//! `mod_analyzer::classify`) → ConflictCheck → Backing_Up → Writing → Verifying →
//! Success/RollingBack.
//!
//! Filesystem operations are not transactional, so failures are undone via an
//! in-memory, append-only rollback action log ([`InstallTransaction`]) consumed only
//! on failure. DB rows for `installed_mod`/`installed_mod_file`/`install_event` are
//! committed in a single `rusqlite` transaction only on Success, so a failed install
//! never leaves partial DB rows.
//!
//! Every write in this pipeline goes through `protected_files::check_write` first; a
//! violation aborts the entire transaction with no partial effect — this cannot be
//! bypassed by any `InstallOptions` flag.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::conflict::{self, ConflictReport};
use crate::error::{CoreError, CoreResult};
use crate::mod_analyzer::{dlc, ModFormat, ModPlan};
use crate::protected_files;
use crate::util::hash_file;

#[derive(Debug, Clone)]
pub enum InstallAction {
    /// A file existed before and was backed up prior to being overwritten.
    FileOverwritten { target: PathBuf, backup: PathBuf },
    /// A file did not exist before; nothing to restore on rollback except deletion.
    FileWritten { target: PathBuf },
    /// A `dlclist.xml` entry was added; rollback removes it again.
    DlclistEntryAdded {
        dlclist_path: PathBuf,
        pack_name: String,
    },
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
                    InstallAction::DlclistEntryAdded { dlclist_path, .. } => dlclist_path,
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
                InstallAction::DlclistEntryAdded {
                    dlclist_path,
                    pack_name,
                } => {
                    let description = format!(
                        "remove DLC entry {pack_name} from {}",
                        dlclist_path.display()
                    );
                    let result = dlc::remove_entry(&dlclist_path, &pack_name);
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
    /// Must be `true` for `install` to proceed when `ConflictReport::requires_explicit_override`
    /// is true. Has no effect on protected-file hits, which are never overridable.
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

#[derive(Debug, Clone, serde::Serialize)]
pub enum InstallOutcome {
    Success {
        installed_mod_id: i64,
        files_written: usize,
    },
    /// Refused: at least one target collides with a *different* mod's file, and
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
) -> CoreResult<InstallOutcome> {
    // OpenAI-Codex: Shared protection for callers that already hold a plan.
    crate::providers::ensure_game_root_available(game_root)?;
    let source_type = source_type_str(&plan.format)?;

    // --- ConflictCheck ---
    let targets: Vec<PathBuf> = plan.files.iter().map(|f| f.target.clone()).collect();
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
    if report.requires_explicit_override() && !options.override_foreign_conflicts {
        return Ok(InstallOutcome::RequiresOverride(report));
    }

    // --- Backing_Up + Writing ---
    let mut tx_log = InstallTransaction::default();
    let result = perform_writes(plan, backup_root, options.auto_backup, &mut tx_log);

    let mut file_hashes = match result {
        Ok(hashes) => hashes,
        Err(err) => {
            return Err(rollback_failed_install(conn, tx_log, err));
        }
    };

    // --- Add-on pack registration (part of Writing, still rollback-guarded) ---
    if let ModFormat::AddOnPack { pack_name } = &plan.format {
        let dlclist_path = dlc::dlclist_path(game_root);
        match dlc::add_entry_with_status(&dlclist_path, pack_name) {
            Ok(true) => tx_log.record(InstallAction::DlclistEntryAdded {
                dlclist_path,
                pack_name: pack_name.clone(),
            }),
            Ok(false) => {}
            Err(err) => {
                return Err(rollback_failed_install(conn, tx_log, err));
            }
        }
    }

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
        db_tx.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status, source_path) \
             VALUES (?1, ?2, ?3, 'active', ?4)",
            rusqlite::params![
                name,
                source_type,
                game_root.to_string_lossy(),
                source_path.to_string_lossy()
            ],
        )?;
        let installed_mod_id = db_tx.last_insert_rowid();

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
    // OpenAI-Codex: Validate the new package and unrelated conflicts before removal.
    // Keep this plan alive: archive source paths belong to its staging directory.
    let plan = crate::mod_analyzer::classify(new_source_path, provider)?;
    source_type_str(&plan.format)?;
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
        if let Some(staging_root) = staging_root {
            crate::uninstall::uninstall_with_staging(
                conn,
                mod_id,
                game_root,
                recycle_bin_root,
                staging_root,
            )?;
        } else {
            crate::uninstall::uninstall(conn, mod_id, game_root, recycle_bin_root)?;
        }
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
        crate::state::disable(&conn, id, &staging).unwrap();
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
        let outcome = reinstall(
            &mut conn,
            installed_mod_id,
            &source,
            "v2",
            &provider,
            &game_root,
            &new_backup,
            &dir.path().join("recycle"),
            InstallOptions::default(),
        )
        .unwrap();
        assert!(matches!(outcome, InstallOutcome::Success { .. }));
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
    fn rollback_regression_reports_dlclist_read_failure() {
        let dir = tempfile::tempdir().unwrap();
        let dlclist_path = dir.path().join("dlclist.xml");
        std::fs::write(&dlclist_path, [0xff]).unwrap();
        let mut tx = InstallTransaction::default();
        tx.record(InstallAction::DlclistEntryAdded {
            dlclist_path,
            pack_name: "MyAddonCar".to_string(),
        });

        assert!(
            tx.rollback().is_err(),
            "dlclist read failures must be reported"
        );
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

    #[test]
    fn reinstall_checks_existing_conflicts_before_removing_the_old_version() {
        for tracked in [true, false] {
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
            let outcome = reinstall(
                &mut conn,
                old_id,
                &source,
                "v2",
                &provider,
                &game_root,
                &backup,
                &recycle,
                InstallOptions::default(),
            )
            .unwrap();
            assert!(matches!(outcome, InstallOutcome::RequiresOverride(_)));
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

    #[test]
    fn foreign_conflict_without_override_refuses_and_writes_nothing() {
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

        let outcome = install(
            &mut conn,
            "NewMod",
            &plan,
            &game_root,
            &dir.path().join("backups"),
            InstallOptions::default(), // override_foreign_conflicts: false
            &source,
        )
        .unwrap();

        assert!(matches!(outcome, InstallOutcome::RequiresOverride(_)));
        assert_eq!(std::fs::read(&existing_target).unwrap(), b"already there");
    }

    #[test]
    fn foreign_conflict_with_override_proceeds_and_backs_up() {
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
        conn.execute(
            "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) \
             VALUES (?1, ?2, 'hash')",
            rusqlite::params![other_mod_id, existing_target.to_string_lossy()],
        )
        .unwrap();

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
    }

    #[test]
    fn add_on_pack_install_registers_dlclist_entry() {
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
        assert!(dlc::has_entry(&dlclist_path, "MyAddonCar").unwrap());
    }
}
