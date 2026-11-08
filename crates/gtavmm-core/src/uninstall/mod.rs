// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex

//! Uninstall: delete each `installed_mod_file.target_path` (guarded by
//! `protected_files::check_write`, which should never actually trigger here since
//! `install` already prevents ever deploying to a protected path — kept as
//! defense-in-depth, validated for *every* file before any deletion begins so a
//! surprise hit aborts with zero partial effect rather than leaving the mod
//! half-removed), restore each `backup_path`, write an `install_event` row, then hand
//! a snapshot of the deployed files to the recycle bin (snapshot-at-uninstall-time,
//! not at-install-time — see the MVP spec's recycle-bin tradeoff writeup) instead of
//! purging immediately. Add-on packs (detected by an `update/x64/dlcpacks/<name>/`
//! target layout) produce durable manual-registration work. This module never
//! changes `dlclist.xml`.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::dlc_manual::{self, ManualDlcOperation, ManualDlcOperationToken};
use crate::error::{CoreError, CoreResult};
#[cfg(test)]
use crate::mod_analyzer::dlc;
use crate::protected_files;
use crate::recycle_bin::RETENTION_DAYS;

struct ModFileRow {
    id: i64,
    target_path: String,
    backup_path: Option<String>,
}

fn load_mod_files(conn: &Connection, mod_id: i64) -> CoreResult<Vec<ModFileRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, target_path, backup_path FROM installed_mod_file WHERE installed_mod_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map([mod_id], |row| {
        Ok(ModFileRow {
            id: row.get(0)?,
            target_path: row.get(1)?,
            backup_path: row.get(2)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn mod_status(conn: &Connection, mod_id: i64) -> CoreResult<Option<String>> {
    conn.query_row(
        "SELECT status FROM installed_mod WHERE id = ?1",
        [mod_id],
        |row| row.get(0),
    )
    .map(Some)
    .or_else(|e| {
        if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
            Ok(None)
        } else {
            Err(e.into())
        }
    })
}

/// Removes `installed_mod_id`: deletes its deployed files (restoring anything they
/// overwrote), snapshots them into the recycle bin, and records the event and any
/// manual DLC registration work without editing XML.
pub fn uninstall(
    conn: &mut Connection,
    installed_mod_id: i64,
    game_root: &Path,
    recycle_bin_root: &Path,
) -> CoreResult<()> {
    uninstall_impl(
        conn,
        installed_mod_id,
        game_root,
        recycle_bin_root,
        None,
        None,
    )
}

/// OpenAI-Codex: Uses explicit managed storage for disabled payload snapshots.
pub fn uninstall_with_staging(
    conn: &mut Connection,
    installed_mod_id: i64,
    game_root: &Path,
    recycle_bin_root: &Path,
    staging_root: &Path,
) -> CoreResult<()> {
    uninstall_impl(
        conn,
        installed_mod_id,
        game_root,
        recycle_bin_root,
        Some(staging_root),
        None,
    )
}

/// OpenAI-Codex: The outer reinstall owns settlement of its compound intent.
pub(crate) fn uninstall_for_reinstall(
    conn: &mut Connection,
    installed_mod_id: i64,
    game_root: &Path,
    recycle_bin_root: &Path,
    staging_root: Option<&Path>,
    operation: &ManualDlcOperationToken,
) -> CoreResult<()> {
    uninstall_impl(
        conn,
        installed_mod_id,
        game_root,
        recycle_bin_root,
        staging_root,
        Some(operation),
    )
}

// OpenAI-Codex: Compare relative snapshot names at component boundaries.
fn recycle_snapshot_prefix_hit(set: &std::collections::BTreeSet<String>, key: &str) -> bool {
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

fn validate_recycle_snapshot_paths(paths: &[PathBuf]) -> CoreResult<()> {
    let mut lower = std::collections::BTreeSet::new();
    let mut upper = std::collections::BTreeSet::new();
    for relative in paths {
        let mut parts = Vec::new();
        for component in relative.components() {
            match component {
                std::path::Component::Normal(part) => {
                    let part = part.to_str().ok_or_else(|| CoreError::UnsupportedFormat {
                        reason: format!(
                            "recycle snapshot path is not Unicode: {}",
                            relative.display()
                        ),
                    })?;
                    parts.push(part);
                }
                _ => {
                    return Err(CoreError::UnsupportedFormat {
                        reason: format!(
                            "recycle snapshot path is not relative: {}",
                            relative.display()
                        ),
                    });
                }
            }
        }
        let key = parts.join("/");
        crate::lml::LocalReference::parse(&key).map_err(|reason| CoreError::UnsupportedFormat {
            reason: format!(
                "recycle snapshot path is unsupported: {} ({reason})",
                relative.display()
            ),
        })?;
        let low = key.to_lowercase();
        let high = key.to_uppercase();
        if recycle_snapshot_prefix_hit(&lower, &low) || recycle_snapshot_prefix_hit(&upper, &high) {
            return Err(CoreError::UnsupportedFormat {
                reason: format!("recycle snapshot paths overlap: {}", relative.display()),
            });
        }
        lower.insert(low);
        upper.insert(high);
    }
    Ok(())
}

// OpenAI-Codex: Check old snapshot paths before a compound reinstall intent.
pub(crate) fn preflight_recycle_snapshot_paths(
    conn: &Connection,
    mod_id: i64,
    game_root: &Path,
) -> CoreResult<()> {
    let files = load_mod_files(conn, mod_id)?;
    let resolved_root = std::fs::canonicalize(game_root)?;
    let mut relatives = Vec::with_capacity(files.len());
    for file in &files {
        let target = Path::new(&file.target_path);
        let invalid_target = || CoreError::UnsupportedFormat {
            reason: format!(
                "uninstall target is outside its game root or has an unsafe relative path: {}",
                target.display()
            ),
        };
        let relative = target
            .strip_prefix(game_root)
            .map_err(|_| invalid_target())?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(invalid_target());
        }
        let mut ancestor = target;
        loop {
            match std::fs::canonicalize(ancestor) {
                Ok(resolved) => {
                    if !resolved.starts_with(&resolved_root) {
                        return Err(invalid_target());
                    }
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    match std::fs::symlink_metadata(ancestor) {
                        Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => {}
                        Err(other) => return Err(other.into()),
                        Ok(_) => return Err(invalid_target()),
                    }
                    ancestor = ancestor.parent().ok_or_else(invalid_target)?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        relatives.push(relative.to_path_buf());
    }
    validate_recycle_snapshot_paths(&relatives)
}

// OpenAI-Codex: Create a new snapshot leaf without reusing an existing entry.
fn create_recycle_entry_dir(recycle_bin_root: &Path, entry_dir: &Path) -> CoreResult<()> {
    std::fs::create_dir_all(recycle_bin_root)?;
    std::fs::create_dir(entry_dir)?;
    Ok(())
}

fn uninstall_impl(
    conn: &mut Connection,
    installed_mod_id: i64,
    game_root: &Path,
    recycle_bin_root: &Path,
    staging_root: Option<&Path>,
    shared_operation: Option<&ManualDlcOperationToken>,
) -> CoreResult<()> {
    // OpenAI-Codex: Reject pending workspaces before snapshots or file changes.
    crate::providers::ensure_installed_mod_available(conn, installed_mod_id)?;
    crate::providers::ensure_game_root_available(game_root)?;
    let status = mod_status(conn, installed_mod_id)?;
    let disabled = status.as_deref() == Some("disabled");
    if disabled && staging_root.is_none() {
        return Err(CoreError::UnsupportedFormat {
            reason: "disabled uninstall requires an explicit staging root".to_string(),
        });
    }
    match status {
        None => {
            return Err(CoreError::UnsupportedFormat {
                reason: format!("no installed mod with id {installed_mod_id}"),
            })
        }
        Some(status) if status == "uninstalled" => {
            return Err(CoreError::UnsupportedFormat {
                reason: format!("mod {installed_mod_id} is already uninstalled"),
            })
        }
        Some(_) => {}
    }

    let files = load_mod_files(conn, installed_mod_id)?;

    // OpenAI-Codex: Validate both snapshot layout and resolved ancestry before writes.
    let resolved_root = std::fs::canonicalize(game_root)?;
    let mut snapshot_relatives = Vec::with_capacity(files.len());
    for file in &files {
        let target = Path::new(&file.target_path);
        let invalid_target = || CoreError::UnsupportedFormat {
            reason: format!(
                "uninstall target is outside its game root or has an unsafe relative path: {}",
                target.display()
            ),
        };
        let relative = target
            .strip_prefix(game_root)
            .map_err(|_| invalid_target())?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(invalid_target());
        }
        // Missing targets are allowed, but their closest existing ancestor must
        // still resolve inside the root (including junctions/symbolic links).
        let mut ancestor = target;
        loop {
            match std::fs::canonicalize(ancestor) {
                Ok(resolved) => {
                    if !resolved.starts_with(&resolved_root) {
                        return Err(invalid_target());
                    }
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    match std::fs::symlink_metadata(ancestor) {
                        Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => {}
                        Err(other) => return Err(other.into()),
                        Ok(_) => return Err(invalid_target()),
                    }
                    ancestor = ancestor.parent().ok_or_else(invalid_target)?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        snapshot_relatives.push(relative.to_path_buf());
    }
    // OpenAI-Codex: Refuse colliding snapshot destinations before any mutation.
    validate_recycle_snapshot_paths(&snapshot_relatives)?;

    if disabled {
        // OpenAI-Codex: Never touch current game targets, backups, or shared DLC
        // metadata for a disabled mod. Keep staging as recovery data after copying.
        let payloads = crate::state::disabled_payloads(
            conn,
            installed_mod_id,
            staging_root.expect("validated staging context"),
        )?;
        if payloads.len() != files.len()
            || payloads
                .iter()
                .zip(&files)
                .any(|((id, _), file)| *id != file.id)
        {
            return Err(CoreError::UnsupportedFormat {
                reason: "disabled staging file mapping does not match installed records"
                    .to_string(),
            });
        }
        let owned_operation = if shared_operation.is_none() {
            let packs = dlc_manual::pack_names_for_mod(conn, installed_mod_id, game_root)?;
            Some(dlc_manual::begin_tasks(
                conn,
                game_root,
                &packs,
                ManualDlcOperation::Uninstall,
                Some(installed_mod_id),
            )?)
        } else {
            None
        };
        let entry_dir = recycle_bin_root.join(format!(
            "{installed_mod_id}-{}",
            chrono::Utc::now().format("%Y%m%d%H%M%S%3f")
        ));
        create_recycle_entry_dir(recycle_bin_root, &entry_dir)?;
        for ((_, source), relative) in payloads.iter().zip(&snapshot_relatives) {
            let destination = entry_dir.join(relative);
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(source, destination)?;
        }
        let metadata = crate::recycle_bin::encode_snapshot_metadata(
            game_root,
            files.iter().map(|file| file.id).collect(),
        )?;
        let now = chrono::Utc::now();
        let transaction = conn.transaction()?;
        transaction.execute("INSERT INTO recycle_bin_entry (original_installed_mod_id, mod_package_snapshot_path, snapshot_metadata, deleted_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5)", rusqlite::params![installed_mod_id, entry_dir.to_string_lossy(), metadata, now.to_rfc3339(), (now + chrono::Duration::days(RETENTION_DAYS)).to_rfc3339()])?;
        transaction.execute(
            "UPDATE installed_mod SET status = 'uninstalled' WHERE id = ?1",
            [installed_mod_id],
        )?;
        transaction.execute("INSERT INTO install_event (installed_mod_id, event_type, success) VALUES (?1, 'uninstall', 1)", [installed_mod_id])?;
        if let Some(operation) = &owned_operation {
            dlc_manual::settle_tasks(&transaction, operation)?;
        }
        transaction.commit()?;
        return Ok(());
    }

    // OpenAI-Codex: No overwrite-chain ordering is recorded, so refuse ambiguous
    // ownership in either direction rather than deleting another active payload.
    let targets = files
        .iter()
        .map(|file| PathBuf::from(&file.target_path))
        .collect::<Vec<_>>();
    let conflicts = crate::conflict::analyze(conn, &targets)?;
    for conflict in conflicts.foreign_conflicts {
        if let Some(owner_mod_id) = conflict.owner_mod_id {
            if owner_mod_id != installed_mod_id {
                return Err(CoreError::ForeignConflict {
                    path: conflict.path,
                    owner_mod_id,
                });
            }
        }
    }

    // Pass 1: validate all targets and backups before snapshotting or mutating anything.
    let mut deployed_targets = std::collections::HashSet::new();
    for file in &files {
        let target = Path::new(&file.target_path);
        protected_files::check_write(target)?;
        match std::fs::symlink_metadata(target) {
            Ok(metadata) if metadata.file_type().is_file() => {
                // OpenAI-Codex: Catch an already-unreadable snapshot source before
                // any earlier target can be removed. Later I/O can still fail.
                crate::util::hash_file(target)?;
                deployed_targets.insert(std::fs::canonicalize(target)?);
            }
            Ok(_) => {
                return Err(CoreError::UnsupportedFormat {
                    reason: format!(
                        "uninstall target is not a regular file: {}",
                        target.display()
                    ),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        };
    }
    let mut backup_paths = std::collections::HashSet::new();
    for file in &files {
        if let Some(backup) = &file.backup_path {
            if !std::fs::symlink_metadata(backup)?.file_type().is_file() {
                return Err(CoreError::UnsupportedFormat {
                    reason: format!("recorded backup is not a regular file: {backup}"),
                });
            }
            let backup_path = std::fs::canonicalize(backup)?;
            if deployed_targets.contains(&backup_path) {
                return Err(CoreError::UnsupportedFormat {
                    reason: format!("recorded backup is one of the deployed targets: {backup}"),
                });
            }
            if !backup_paths.insert(backup_path.clone()) {
                return Err(CoreError::UnsupportedFormat {
                    reason: format!("recorded backup is shared by multiple targets: {backup}"),
                });
            }
            // OpenAI-Codex: Metadata alone cannot detect a backup blocked by a read lock.
            // This preflight reduces early failures; it is not an atomicity guarantee.
            crate::util::hash_file(&backup_path)?;
        }
    }

    // OpenAI-Codex: Commit intent before any snapshot, removal, or restored backup.
    let owned_operation = if shared_operation.is_none() {
        let packs = dlc_manual::pack_names_for_mod(conn, installed_mod_id, game_root)?;
        Some(dlc_manual::begin_tasks(
            conn,
            game_root,
            &packs,
            ManualDlcOperation::Uninstall,
            Some(installed_mod_id),
        )?)
    } else {
        None
    };

    // Pass 2: snapshot deployed files into the recycle bin, then delete/restore.
    let entry_dir_name = format!(
        "{installed_mod_id}-{}",
        chrono::Utc::now().format("%Y%m%d%H%M%S%3f")
    );
    let entry_dir = recycle_bin_root.join(&entry_dir_name);
    create_recycle_entry_dir(recycle_bin_root, &entry_dir)?;
    let mut snapshotted_file_ids = Vec::new();

    for (file, relative) in files.iter().zip(&snapshot_relatives) {
        let target = PathBuf::from(&file.target_path);

        if target.exists() {
            let snapshot_path = entry_dir.join(relative);
            if let Some(parent) = snapshot_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&target, &snapshot_path)?;
            snapshotted_file_ids.push(file.id);
            std::fs::remove_file(&target)?;
        }

        if let Some(backup) = &file.backup_path {
            let backup_path = PathBuf::from(backup);
            if backup_path.exists() {
                crate::util::move_file(&backup_path, &target)?;
            }
        }
    }

    let deleted_at = chrono::Utc::now();
    let expires_at = deleted_at + chrono::Duration::days(RETENTION_DAYS);
    let snapshot_metadata =
        crate::recycle_bin::encode_snapshot_metadata(game_root, snapshotted_file_ids)?;

    let db_tx = conn.transaction()?;
    db_tx.execute(
        "INSERT INTO recycle_bin_entry (original_installed_mod_id, mod_package_snapshot_path, snapshot_metadata, deleted_at, expires_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            installed_mod_id,
            entry_dir.to_string_lossy(),
            snapshot_metadata,
            deleted_at.to_rfc3339(),
            expires_at.to_rfc3339(),
        ],
    )?;
    db_tx.execute(
        "UPDATE installed_mod SET status = 'uninstalled' WHERE id = ?1",
        [installed_mod_id],
    )?;
    db_tx.execute(
        "INSERT INTO install_event (installed_mod_id, event_type, success) VALUES (?1, 'uninstall', 1)",
        [installed_mod_id],
    )?;
    if let Some(operation) = &owned_operation {
        dlc_manual::settle_tasks(&db_tx, operation)?;
    }
    db_tx.commit()?;

    Ok(())
}

// OpenAI-Codex_disable_frontend_audit: Construct test-only staged metadata.
#[cfg(test)]
pub(crate) fn seed_disabled_fixture_from_staging(
    conn: &Connection,
    mod_id: i64,
    staging_root: &Path,
) {
    #[derive(serde::Serialize)]
    struct SyntheticStagedFile {
        file_id: i64,
        index: usize,
        hash: String,
    }

    assert_eq!(mod_status(conn, mod_id).unwrap().as_deref(), Some("active"));
    let stage = staging_root.join(mod_id.to_string());
    assert!(stage.is_dir());
    let file_ids: Vec<i64> = {
        let mut statement = conn
            .prepare("SELECT id FROM installed_mod_file WHERE installed_mod_id = ?1 ORDER BY id")
            .unwrap();
        let rows = statement.query_map([mod_id], |row| row.get(0)).unwrap();
        rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
    };
    assert!(!file_ids.is_empty());
    let mut manifest = Vec::new();
    for (index, file_id) in file_ids.into_iter().enumerate() {
        let staged = stage.join(index.to_string());
        match std::fs::symlink_metadata(&staged) {
            Ok(metadata) => {
                assert!(metadata.file_type().is_file());
                manifest.push(SyntheticStagedFile {
                    file_id,
                    index,
                    hash: crate::util::hash_file(&staged).unwrap(),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("synthetic staged metadata: {error}"),
        }
    }
    std::fs::write(
        stage.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    assert_eq!(
        conn.execute(
            "UPDATE installed_mod SET status = 'disabled' WHERE id = ?1 AND status = 'active'",
            [mod_id],
        )
        .unwrap(),
        1
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // OpenAI-Codex_recycle_snapshot_review: Capture synthetic recovery data.
    fn snapshot_disabled_fixture_tree(root: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
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

    fn snapshot_disabled_fixture_database(
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

    struct DisabledSnapshotFixture {
        temp: tempfile::TempDir,
        conn: Connection,
        mod_id: i64,
        game_root: PathBuf,
        staging_root: PathBuf,
        recycle_root: PathBuf,
    }

    // OpenAI-Codex_recycle_snapshot_review: Model valid legacy data without a manifest.
    fn disabled_snapshot_fixture(first: &str, second: &str) -> DisabledSnapshotFixture {
        let temp = tempfile::tempdir().unwrap();
        let game_root = temp.path().join("game");
        let staging_root = temp.path().join("staging");
        let recycle_root = temp.path().join("recycle");
        std::fs::create_dir(&game_root).unwrap();
        std::fs::create_dir(&recycle_root).unwrap();
        std::fs::write(game_root.join("unrelated.asi"), b"unrelated game file").unwrap();
        std::fs::write(recycle_root.join("untouched.bin"), b"existing recycle data").unwrap();

        let conn = crate::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status, mode) \
             VALUES ('Legacy disabled', 'asi', ?1, 'disabled', 'legacy-sp')",
            [game_root.to_str().unwrap()],
        )
        .unwrap();
        let mod_id = conn.last_insert_rowid();
        let stage_dir = staging_root.join(mod_id.to_string());
        std::fs::create_dir_all(&stage_dir).unwrap();
        let mut file_ids = Vec::new();
        for (index, (relative, payload)) in [
            (first, b"first staged payload".as_slice()),
            (second, b"second staged payload".as_slice()),
        ]
        .into_iter()
        .enumerate()
        {
            let staged = stage_dir.join(index.to_string());
            std::fs::write(&staged, payload).unwrap();
            let target = game_root.join(relative);
            conn.execute(
                "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) \
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    mod_id,
                    target.to_str().unwrap(),
                    crate::util::hash_file(&staged).unwrap()
                ],
            )
            .unwrap();
            file_ids.push(conn.last_insert_rowid());
        }
        assert_ne!(file_ids[0], file_ids[1]);
        assert!(!stage_dir.join("manifest.json").exists());
        assert!(crate::providers::ensure_installed_mod_available(&conn, mod_id).is_ok());
        let payloads = crate::state::disabled_payloads(&conn, mod_id, &staging_root).unwrap();
        assert_eq!(payloads.len(), 2);
        assert_eq!(
            payloads.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            file_ids
        );
        DisabledSnapshotFixture {
            temp,
            conn,
            mod_id,
            game_root,
            staging_root,
            recycle_root,
        }
    }

    // OpenAI-Codex_recycle_snapshot_review: Never reuse occupied synthetic entries.
    #[test]
    fn recycle_entry_rejects_an_existing_directory_without_changes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("recycle");
        let entry = root.join("42-20261105000000000");
        std::fs::create_dir_all(entry.join("nested")).unwrap();
        std::fs::write(entry.join("nested/payload.asi"), b"old payload").unwrap();
        std::fs::write(root.join("peer.bin"), b"peer data").unwrap();
        let before = snapshot_disabled_fixture_tree(temp.path());
        let error = create_recycle_entry_dir(&root, &entry).unwrap_err();
        assert!(
            matches!(&error, CoreError::Io(e) if e.kind() == std::io::ErrorKind::AlreadyExists),
            "{error:?}"
        );
        assert_eq!(snapshot_disabled_fixture_tree(temp.path()), before);
    }

    // OpenAI-Codex_recycle_snapshot_review: Do not replace an occupied synthetic file.
    #[test]
    fn recycle_entry_rejects_an_existing_file_without_changes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("recycle");
        std::fs::create_dir(&root).unwrap();
        let entry = root.join("42-20261105000000000");
        std::fs::write(&entry, b"occupied file").unwrap();
        std::fs::write(root.join("peer.bin"), b"peer data").unwrap();
        let before = snapshot_disabled_fixture_tree(temp.path());
        let error = create_recycle_entry_dir(&root, &entry).unwrap_err();
        assert!(
            matches!(&error, CoreError::Io(e) if e.kind() == std::io::ErrorKind::AlreadyExists),
            "{error:?}"
        );
        assert_eq!(snapshot_disabled_fixture_tree(temp.path()), before);
    }

    // OpenAI-Codex_recycle_snapshot_review: Create missing parents and one fresh leaf.
    #[test]
    fn recycle_entry_creates_missing_parents_and_preserves_peers() {
        let temp = tempfile::tempdir().unwrap();
        let sentinel = temp.path().join("peer.bin");
        std::fs::write(&sentinel, b"peer data").unwrap();
        let root = temp.path().join("missing/nested/recycle");
        let entry = root.join("42-20261105000000000");
        assert!(!root.exists());
        create_recycle_entry_dir(&root, &entry).unwrap();
        assert!(root.is_dir());
        assert!(entry.is_dir());
        assert_eq!(std::fs::read_dir(&entry).unwrap().count(), 0);
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"peer data");
        let before_retry = snapshot_disabled_fixture_tree(temp.path());
        let error = create_recycle_entry_dir(&root, &entry).unwrap_err();
        assert!(
            matches!(&error, CoreError::Io(e) if e.kind() == std::io::ErrorKind::AlreadyExists),
            "{error:?}"
        );
        assert_eq!(snapshot_disabled_fixture_tree(temp.path()), before_retry);
    }

    // OpenAI-Codex_recycle_snapshot_review: Retain existing siblings under the recycle root.
    #[test]
    fn recycle_entry_creates_a_fresh_leaf_under_existing_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("recycle");
        std::fs::create_dir(&root).unwrap();
        let sibling = root.join("peer.bin");
        std::fs::write(&sibling, b"peer data").unwrap();
        let entry = root.join("42-20261105000000000");
        create_recycle_entry_dir(&root, &entry).unwrap();
        assert!(entry.is_dir());
        assert_eq!(std::fs::read_dir(&entry).unwrap().count(), 0);
        assert_eq!(std::fs::read(&sibling).unwrap(), b"peer data");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);
    }

    // OpenAI-Codex_recycle_snapshot_review: Root-file failures retain their I/O type.
    #[test]
    fn recycle_entry_reports_root_file_io_error_without_changes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root-file");
        std::fs::write(&root, b"root file data").unwrap();
        let entry = root.join("42-20261105000000000");
        let before = snapshot_disabled_fixture_tree(temp.path());
        let error = create_recycle_entry_dir(&root, &entry).unwrap_err();
        assert!(matches!(&error, CoreError::Io(_)), "{error:?}");
        assert_eq!(snapshot_disabled_fixture_tree(temp.path()), before);
    }

    // OpenAI-Codex: Unsupported names must fail independently of path overlap.
    #[test]
    fn recycle_snapshot_rejects_unverifiable_relative_names() {
        for name in [
            "CON.txt".to_string(),
            "name.".to_string(),
            "name ".to_string(),
            "bad:name.asi".to_string(),
            "a".repeat(1025),
        ] {
            let paths = vec![PathBuf::from("safe.asi"), PathBuf::from(&name)];
            let error = validate_recycle_snapshot_paths(&paths).unwrap_err();
            assert!(matches!(
                error,
                CoreError::UnsupportedFormat { reason }
                    if reason.starts_with("recycle snapshot path is unsupported:")
            ));
        }
        assert!(validate_recycle_snapshot_paths(&[
            PathBuf::from("safe.asi"),
            PathBuf::from("nested/other.asi"),
        ])
        .is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn recycle_snapshot_rejects_non_unicode_relative_names() {
        use std::os::unix::ffi::OsStringExt;
        let invalid = std::ffi::OsString::from_vec(vec![0xff]);
        let error = validate_recycle_snapshot_paths(&[PathBuf::from(invalid)]).unwrap_err();
        assert!(matches!(
            error,
            CoreError::UnsupportedFormat { reason }
                if reason.starts_with("recycle snapshot path is not Unicode:")
        ));
    }

    #[cfg(windows)]
    #[test]
    fn recycle_snapshot_rejects_non_unicode_relative_names() {
        use std::os::windows::ffi::OsStringExt;
        let invalid = std::ffi::OsString::from_wide(&[0xd800]);
        let error = validate_recycle_snapshot_paths(&[PathBuf::from(invalid)]).unwrap_err();
        assert!(matches!(
            error,
            CoreError::UnsupportedFormat { reason }
                if reason.starts_with("recycle snapshot path is not Unicode:")
        ));
    }

    struct ActiveSnapshotFixture {
        temp: tempfile::TempDir,
        conn: Connection,
        mod_id: i64,
        game_root: PathBuf,
        recycle_root: PathBuf,
    }

    // OpenAI-Codex_recycle_snapshot_review: Create actual synthetic active targets.
    fn active_snapshot_fixture(first: &str, second: &str) -> ActiveSnapshotFixture {
        let temp = tempfile::tempdir().unwrap();
        let game_root = temp.path().join("game");
        let recycle_root = temp.path().join("recycle");
        std::fs::create_dir(&game_root).unwrap();
        std::fs::create_dir(&recycle_root).unwrap();
        std::fs::write(game_root.join("unrelated.asi"), b"unrelated game file").unwrap();
        std::fs::write(recycle_root.join("untouched.bin"), b"existing recycle data").unwrap();

        let conn = crate::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status, mode) \
             VALUES ('Active synthetic', 'asi', ?1, 'active', 'legacy-sp')",
            [game_root.to_str().unwrap()],
        )
        .unwrap();
        let mod_id = conn.last_insert_rowid();
        let mut file_ids = Vec::new();
        for relative in [first, second] {
            let target = game_root.join(relative);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            if !target.exists() {
                std::fs::write(&target, b"active mod payload").unwrap();
            }
            assert!(std::fs::metadata(&target).unwrap().is_file());
            conn.execute(
                "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) \
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    mod_id,
                    target.to_str().unwrap(),
                    crate::util::hash_file(&target).unwrap()
                ],
            )
            .unwrap();
            file_ids.push(conn.last_insert_rowid());
        }
        assert_ne!(file_ids[0], file_ids[1]);
        conn.execute(
            "INSERT INTO install_event (installed_mod_id, event_type, success) \
             VALUES (?1, 'install', 1)",
            [mod_id],
        )
        .unwrap();
        assert!(crate::providers::ensure_installed_mod_available(&conn, mod_id).is_ok());
        ActiveSnapshotFixture {
            temp,
            conn,
            mod_id,
            game_root,
            recycle_root,
        }
    }

    // OpenAI-Codex_recycle_snapshot_review: Refuse active snapshot aliases before mutation.
    #[test]
    fn active_uninstall_refuses_overlapping_snapshot_relative_paths() {
        for (first, second) in [
            ("shared.asi", "shared.asi"),
            ("Plugins/Callout.dll", "plugins/callout.dll"),
        ] {
            let mut fixture = active_snapshot_fixture(first, second);
            let tree_before = snapshot_disabled_fixture_tree(fixture.temp.path());
            let database_before = snapshot_disabled_fixture_database(&fixture.conn);
            let changes_before: i64 = fixture
                .conn
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .unwrap();
            let result = uninstall(
                &mut fixture.conn,
                fixture.mod_id,
                &fixture.game_root,
                &fixture.recycle_root,
            );
            let second_target = fixture.game_root.join(second);
            let relative = second_target.strip_prefix(&fixture.game_root).unwrap();
            let expected_error = format!("recycle snapshot paths overlap: {}", relative.display());
            assert!(
                matches!(
                    result,
                    Err(CoreError::UnsupportedFormat { reason }) if reason == expected_error
                ),
                "first={first}, second={second}"
            );
            assert_eq!(
                snapshot_disabled_fixture_tree(fixture.temp.path()),
                tree_before,
                "first={first}, second={second}"
            );
            assert_eq!(
                snapshot_disabled_fixture_database(&fixture.conn),
                database_before,
                "first={first}, second={second}"
            );
            let changes_after: i64 = fixture
                .conn
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .unwrap();
            assert_eq!(changes_after, changes_before);
            assert_eq!(
                mod_status(&fixture.conn, fixture.mod_id)
                    .unwrap()
                    .as_deref(),
                Some("active")
            );
        }
    }

    // OpenAI-Codex_recycle_snapshot_review: Disjoint active targets still uninstall normally.
    #[test]
    fn active_uninstall_snapshots_disjoint_relative_paths() {
        let mut fixture = active_snapshot_fixture("first.asi", "nested/second.asi");
        uninstall(
            &mut fixture.conn,
            fixture.mod_id,
            &fixture.game_root,
            &fixture.recycle_root,
        )
        .unwrap();
        assert_eq!(
            mod_status(&fixture.conn, fixture.mod_id)
                .unwrap()
                .as_deref(),
            Some("uninstalled")
        );
        let snapshot: String = fixture
            .conn
            .query_row(
                "SELECT mod_package_snapshot_path FROM recycle_bin_entry \
                 WHERE original_installed_mod_id = ?1",
                [fixture.mod_id],
                |row| row.get(0),
            )
            .unwrap();
        let snapshot = PathBuf::from(snapshot);
        assert_eq!(
            std::fs::read(snapshot.join("first.asi")).unwrap(),
            b"active mod payload"
        );
        assert_eq!(
            std::fs::read(snapshot.join("nested/second.asi")).unwrap(),
            b"active mod payload"
        );
        assert!(!fixture.game_root.join("first.asi").exists());
        assert!(!fixture.game_root.join("nested/second.asi").exists());
        assert_eq!(
            std::fs::read(fixture.game_root.join("unrelated.asi")).unwrap(),
            b"unrelated game file"
        );
        assert_eq!(
            std::fs::read(fixture.recycle_root.join("untouched.bin")).unwrap(),
            b"existing recycle data"
        );
        let uninstall_events: i64 = fixture
            .conn
            .query_row(
                "SELECT COUNT(*) FROM install_event \
                 WHERE installed_mod_id = ?1 AND event_type = 'uninstall'",
                [fixture.mod_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(uninstall_events, 1);
    }

    // OpenAI-Codex_recycle_snapshot_review: Distinct payloads must not share a snapshot path.
    #[test]
    fn disabled_uninstall_refuses_overlapping_snapshot_relative_paths() {
        for (first, second) in [
            ("shared.asi", "shared.asi"),
            ("Plugins/Callout.dll", "plugins/callout.dll"),
            ("parent", "parent/child.asi"),
            ("parent/child.asi", "parent"),
        ] {
            let mut fixture = disabled_snapshot_fixture(first, second);
            let tree_before = snapshot_disabled_fixture_tree(fixture.temp.path());
            let database_before = snapshot_disabled_fixture_database(&fixture.conn);
            let changes_before: i64 = fixture
                .conn
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .unwrap();
            let result = uninstall_with_staging(
                &mut fixture.conn,
                fixture.mod_id,
                &fixture.game_root,
                &fixture.recycle_root,
                &fixture.staging_root,
            );
            let second_target = fixture.game_root.join(second);
            let relative = second_target.strip_prefix(&fixture.game_root).unwrap();
            let expected_error = format!("recycle snapshot paths overlap: {}", relative.display());
            assert!(
                matches!(
                    result,
                    Err(CoreError::UnsupportedFormat { reason }) if reason == expected_error
                ),
                "first={first}, second={second}"
            );
            assert_eq!(
                snapshot_disabled_fixture_tree(fixture.temp.path()),
                tree_before,
                "first={first}, second={second}"
            );
            assert_eq!(
                snapshot_disabled_fixture_database(&fixture.conn),
                database_before,
                "first={first}, second={second}"
            );
            let changes_after: i64 = fixture
                .conn
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .unwrap();
            assert_eq!(changes_after, changes_before);
            assert_eq!(
                mod_status(&fixture.conn, fixture.mod_id)
                    .unwrap()
                    .as_deref(),
                Some("disabled")
            );
        }
    }

    // OpenAI-Codex_recycle_snapshot_review: Separate legacy paths remain snapshotable.
    #[test]
    fn disabled_uninstall_snapshots_disjoint_legacy_relative_paths() {
        let mut fixture = disabled_snapshot_fixture("first.asi", "nested/second.asi");
        uninstall_with_staging(
            &mut fixture.conn,
            fixture.mod_id,
            &fixture.game_root,
            &fixture.recycle_root,
            &fixture.staging_root,
        )
        .unwrap();
        assert_eq!(
            mod_status(&fixture.conn, fixture.mod_id)
                .unwrap()
                .as_deref(),
            Some("uninstalled")
        );
        let snapshot: String = fixture
            .conn
            .query_row(
                "SELECT mod_package_snapshot_path FROM recycle_bin_entry \
                 WHERE original_installed_mod_id = ?1",
                [fixture.mod_id],
                |row| row.get(0),
            )
            .unwrap();
        let snapshot = PathBuf::from(snapshot);
        assert_eq!(
            std::fs::read(snapshot.join("first.asi")).unwrap(),
            b"first staged payload"
        );
        assert_eq!(
            std::fs::read(snapshot.join("nested/second.asi")).unwrap(),
            b"second staged payload"
        );
        let stage_dir = fixture.staging_root.join(fixture.mod_id.to_string());
        assert_eq!(
            std::fs::read(stage_dir.join("0")).unwrap(),
            b"first staged payload"
        );
        assert_eq!(
            std::fs::read(stage_dir.join("1")).unwrap(),
            b"second staged payload"
        );
        assert_eq!(
            std::fs::read(fixture.recycle_root.join("untouched.bin")).unwrap(),
            b"existing recycle data"
        );
        assert_eq!(
            std::fs::read(fixture.game_root.join("unrelated.asi")).unwrap(),
            b"unrelated game file"
        );
        assert!(!fixture.game_root.join("first.asi").exists());
        assert!(!fixture.game_root.join("nested/second.asi").exists());
    }

    // OpenAI-Codex: A shared text prefix is not necessarily a path ancestor.
    #[test]
    fn disabled_uninstall_allows_distinct_snapshot_path_prefixes() {
        let mut fixture = disabled_snapshot_fixture("parent", "parent2/child.asi");
        uninstall_with_staging(
            &mut fixture.conn,
            fixture.mod_id,
            &fixture.game_root,
            &fixture.recycle_root,
            &fixture.staging_root,
        )
        .unwrap();
        let snapshot: String = fixture
            .conn
            .query_row(
                "SELECT mod_package_snapshot_path FROM recycle_bin_entry \
                 WHERE original_installed_mod_id = ?1",
                [fixture.mod_id],
                |row| row.get(0),
            )
            .unwrap();
        let snapshot = PathBuf::from(snapshot);
        assert_eq!(
            std::fs::read(snapshot.join("parent")).unwrap(),
            b"first staged payload"
        );
        assert_eq!(
            std::fs::read(snapshot.join("parent2/child.asi")).unwrap(),
            b"second staged payload"
        );
        assert_eq!(
            std::fs::read(fixture.recycle_root.join("untouched.bin")).unwrap(),
            b"existing recycle data"
        );
    }

    // OpenAI-Codex: Disabled snapshots must contain staged payloads, not originals.
    #[test]
    fn disabled_uninstall_roundtrip_preserves_game_and_backup() {
        for original in [None, Some(b"original".as_slice())] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("game");
            std::fs::create_dir(&root).unwrap();
            let target = root.join("mod.asi");
            let backup = dir.path().join("original.bak");
            if let Some(bytes) = original {
                std::fs::write(&backup, bytes).unwrap();
            }
            std::fs::write(&target, b"first mod payload").unwrap();
            let mut conn = crate::db::open_in_memory().unwrap();
            let id =
                setup_installed_mod(&conn, &root, "mod.asi", original.map(|_| backup.as_path()));
            let staging = dir.path().join("staging");
            let recycle = dir.path().join("recycle");
            let fixture_stage = staging.join(id.to_string());
            std::fs::create_dir_all(&fixture_stage).unwrap();
            std::fs::rename(&target, fixture_stage.join("0")).unwrap();
            if let Some(bytes) = original {
                std::fs::write(&target, bytes).unwrap();
            }
            crate::uninstall::seed_disabled_fixture_from_staging(&conn, id, &staging);
            // OpenAI-Codex: Enable before constructing the edited staged fixture.
            crate::state::enable(&conn, id, &staging).unwrap();
            std::fs::write(&target, b"edited mod payload").unwrap();
            std::fs::rename(&target, fixture_stage.join("0")).unwrap();
            if let Some(bytes) = original {
                std::fs::write(&target, bytes).unwrap();
            }
            crate::uninstall::seed_disabled_fixture_from_staging(&conn, id, &staging);
            assert!(uninstall(&mut conn, id, &root, &recycle).is_err());
            assert!(!recycle.exists());
            uninstall_with_staging(&mut conn, id, &root, &recycle, &staging).unwrap();
            assert_eq!(std::fs::read(&target).ok().as_deref(), original);
            if let Some(bytes) = original {
                assert_eq!(std::fs::read(&backup).unwrap(), bytes);
            }
            assert_eq!(
                std::fs::read(staging.join(id.to_string()).join("0")).unwrap(),
                b"edited mod payload"
            );
            let entry = crate::recycle_bin::list(&conn).unwrap().remove(0);
            crate::recycle_bin::restore(
                &mut conn,
                entry.id,
                &root,
                &dir.path().join("restored-backups"),
            )
            .unwrap();
            assert_eq!(std::fs::read(&target).unwrap(), b"edited mod payload");
            assert_eq!(mod_status(&conn, id).unwrap().as_deref(), Some("active"));
        }
    }

    #[test]
    fn disabled_uninstall_preserves_another_active_owner() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir(&root).unwrap();
        let target = root.join("mod.asi");
        std::fs::write(&target, b"old payload").unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let id = setup_installed_mod(&conn, &root, "mod.asi", None);
        let staging = dir.path().join("staging");
        let fixture_stage = staging.join(id.to_string());
        std::fs::create_dir_all(&fixture_stage).unwrap();
        std::fs::rename(&target, fixture_stage.join("0")).unwrap();
        crate::uninstall::seed_disabled_fixture_from_staging(&conn, id, &staging);
        let newer = setup_installed_mod(&conn, &root, "mod.asi", None);
        std::fs::write(&target, b"new owner payload").unwrap();
        uninstall_with_staging(&mut conn, id, &root, &dir.path().join("recycle"), &staging)
            .unwrap();
        assert_eq!(std::fs::read(target).unwrap(), b"new owner payload");
        assert_eq!(mod_status(&conn, newer).unwrap().as_deref(), Some("active"));
    }

    #[test]
    fn invalid_disabled_payload_is_rejected_before_snapshotting() {
        for invalid in ["missing", "directory", "changed", "mapping"] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("game");
            std::fs::create_dir(&root).unwrap();
            let target = root.join("mod.asi");
            std::fs::write(&target, b"payload").unwrap();
            let mut conn = crate::db::open_in_memory().unwrap();
            let id = setup_installed_mod(&conn, &root, "mod.asi", None);
            let staging = dir.path().join("staging");
            let fixture_stage = staging.join(id.to_string());
            std::fs::create_dir_all(&fixture_stage).unwrap();
            std::fs::rename(&target, fixture_stage.join("0")).unwrap();
            crate::uninstall::seed_disabled_fixture_from_staging(&conn, id, &staging);
            assert_eq!(
                crate::state::disabled_payloads(&conn, id, &staging)
                    .unwrap()
                    .len(),
                1
            );
            let staged = staging.join(id.to_string()).join("0");
            match invalid {
                "missing" => {
                    std::fs::rename(&staged, staged.with_extension("saved")).unwrap();
                }
                "directory" => {
                    std::fs::rename(&staged, staged.with_extension("saved")).unwrap();
                    std::fs::create_dir(&staged).unwrap();
                }
                "mapping" => std::fs::write(
                    staged.parent().unwrap().join("manifest.json"),
                    br#"[{"file_id":-1,"index":0,"hash":"invalid"}]"#,
                )
                .unwrap(),
                _ => std::fs::write(&staged, b"unexpected payload").unwrap(),
            }
            let recycle = dir.path().join("recycle");
            assert!(uninstall_with_staging(&mut conn, id, &root, &recycle, &staging).is_err());
            assert!(!target.exists());
            assert!(!recycle.exists());
            assert_eq!(mod_status(&conn, id).unwrap().as_deref(), Some("disabled"));
        }
    }

    // OpenAI-Codex: Legacy staging is accepted only when content can be verified.
    #[test]
    fn legacy_staging_requires_matching_recorded_hashes() {
        for matches in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("game");
            std::fs::create_dir(&root).unwrap();
            let mut conn = crate::db::open_in_memory().unwrap();
            let id = setup_installed_mod(&conn, &root, "mod.asi", None);
            conn.execute(
                "UPDATE installed_mod SET status = 'disabled' WHERE id = ?1",
                [id],
            )
            .unwrap();
            let staging = dir.path().join("staging");
            let staged = staging.join(id.to_string()).join("0");
            std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
            std::fs::write(&staged, b"legacy payload").unwrap();
            if matches {
                conn.execute(
                    "UPDATE installed_mod_file SET file_hash = ?1 WHERE installed_mod_id = ?2",
                    rusqlite::params![crate::util::hash_file(&staged).unwrap(), id],
                )
                .unwrap();
            }
            let recycle = dir.path().join("recycle");
            assert_eq!(
                uninstall_with_staging(&mut conn, id, &root, &recycle, &staging).is_ok(),
                matches
            );
            assert_eq!(recycle.exists(), matches);
            assert_eq!(std::fs::read(staged).unwrap(), b"legacy payload");
            assert!(!root.join("mod.asi").exists());
        }
    }

    #[test]
    fn disabled_multifile_mapping_never_substitutes_a_missing_payload() {
        for missing_first in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("game");
            std::fs::create_dir(&root).unwrap();
            let mut conn = crate::db::open_in_memory().unwrap();
            let id = setup_installed_mod(&conn, &root, "z.asi", None);
            conn.execute("INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) VALUES (?1, ?2, 'hash')", rusqlite::params![id, root.join("a.asi").to_string_lossy()]).unwrap();
            if !missing_first {
                std::fs::write(root.join("z.asi"), b"z payload").unwrap();
            }
            std::fs::write(root.join("a.asi"), b"a payload").unwrap();
            let staging = dir.path().join("staging");
            let fixture_stage = staging.join(id.to_string());
            std::fs::create_dir_all(&fixture_stage).unwrap();
            if !missing_first {
                std::fs::rename(root.join("z.asi"), fixture_stage.join("0")).unwrap();
            }
            std::fs::rename(root.join("a.asi"), fixture_stage.join("1")).unwrap();
            crate::uninstall::seed_disabled_fixture_from_staging(&conn, id, &staging);
            assert_eq!(
                std::fs::read(fixture_stage.join("1")).unwrap(),
                b"a payload"
            );
            if missing_first {
                assert!(!fixture_stage.join("0").exists());
                let manifest: Vec<serde_json::Value> = serde_json::from_slice(
                    &std::fs::read(fixture_stage.join("manifest.json")).unwrap(),
                )
                .unwrap();
                assert_eq!(manifest.len(), 1);
                assert_eq!(manifest[0]["index"].as_u64(), Some(1));
                let second_file_id: i64 = conn
                    .query_row(
                        "SELECT id FROM installed_mod_file WHERE installed_mod_id = ?1 ORDER BY id LIMIT 1 OFFSET 1",
                        [id],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(manifest[0]["file_id"].as_i64(), Some(second_file_id));
                let actual_hash = crate::util::hash_file(&fixture_stage.join("1")).unwrap();
                assert_eq!(manifest[0]["hash"].as_str(), Some(actual_hash.as_str()));
            } else {
                assert_eq!(
                    std::fs::read(fixture_stage.join("0")).unwrap(),
                    b"z payload"
                );
                assert_eq!(
                    crate::state::disabled_payloads(&conn, id, &staging)
                        .unwrap()
                        .len(),
                    2
                );
            }
            let recycle = dir.path().join("recycle");
            let result = uninstall_with_staging(&mut conn, id, &root, &recycle, &staging);
            if missing_first {
                assert!(result.is_err());
                assert!(matches!(
                    &result,
                    Err(CoreError::UnsupportedFormat { reason })
                        if reason == &format!("disabled staging for mod {id} is missing, unsafe, or does not match its file records; preserve it for recovery")
                ));
                assert!(!recycle.exists());
                assert_eq!(mod_status(&conn, id).unwrap().as_deref(), Some("disabled"));
            } else {
                result.unwrap();
                let entry = crate::recycle_bin::list(&conn).unwrap().remove(0);
                crate::recycle_bin::restore(
                    &mut conn,
                    entry.id,
                    &root,
                    &dir.path().join("backups"),
                )
                .unwrap();
                assert_eq!(std::fs::read(root.join("z.asi")).unwrap(), b"z payload");
                assert_eq!(std::fs::read(root.join("a.asi")).unwrap(), b"a payload");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn disabled_staged_symlink_is_rejected_before_snapshotting() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir(&root).unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let id = setup_installed_mod(&conn, &root, "mod.asi", None);
        conn.execute(
            "UPDATE installed_mod SET status = 'disabled' WHERE id = ?1",
            [id],
        )
        .unwrap();
        let staging = dir.path().join("staging");
        let staged = staging.join(id.to_string()).join("0");
        std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
        let outside = dir.path().join("outside");
        std::fs::write(&outside, b"outside bytes").unwrap();
        std::os::unix::fs::symlink(&outside, &staged).unwrap();
        let recycle = dir.path().join("recycle");
        assert!(uninstall_with_staging(&mut conn, id, &root, &recycle, &staging).is_err());
        assert!(!recycle.exists());
        assert_eq!(std::fs::read(outside).unwrap(), b"outside bytes");
    }

    #[cfg(windows)]
    #[test]
    fn unreadable_disabled_payload_preserves_status_and_game_files() {
        // OpenAI-Codex: Metadata can succeed while the payload cannot be read.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("mod.asi"), b"payload").unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let id = setup_installed_mod(&conn, &root, "mod.asi", None);
        let staging = dir.path().join("staging");
        let fixture_stage = staging.join(id.to_string());
        std::fs::create_dir_all(&fixture_stage).unwrap();
        std::fs::rename(root.join("mod.asi"), fixture_stage.join("0")).unwrap();
        crate::uninstall::seed_disabled_fixture_from_staging(&conn, id, &staging);
        assert_eq!(
            crate::state::disabled_payloads(&conn, id, &staging)
                .unwrap()
                .len(),
            1
        );
        let staged = staging.join(id.to_string()).join("0");
        let locked = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&staged)
            .unwrap();
        locked.try_lock().unwrap();
        let recycle = dir.path().join("recycle");
        let result = uninstall_with_staging(&mut conn, id, &root, &recycle, &staging);
        locked.unlock().unwrap();
        assert!(result.is_err());
        assert!(!recycle.exists());
        assert!(!root.join("mod.asi").exists());
        assert_eq!(std::fs::read(staged).unwrap(), b"payload");
        assert_eq!(mod_status(&conn, id).unwrap().as_deref(), Some("disabled"));
    }

    // OpenAI-Codex: Refusals must preserve payloads, records, and the recycle bin.
    #[test]
    fn another_active_owner_blocks_uninstall_before_snapshotting() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir(&root).unwrap();
        let target = root.join("shared.asi");
        std::fs::write(&target, b"newer payload").unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let old = setup_installed_mod(&conn, &root, "shared.asi", None);
        let newer = setup_installed_mod(&conn, &root, "shared.asi", None);
        let recycle = dir.path().join("recycle");
        for (id, owner) in [(old, newer), (newer, old)] {
            let error = uninstall(&mut conn, id, &root, &recycle).unwrap_err();
            assert!(
                matches!(error, CoreError::ForeignConflict { owner_mod_id, .. } if owner_mod_id == owner)
            );
            assert_eq!(std::fs::read(&target).unwrap(), b"newer payload");
            assert_eq!(mod_status(&conn, id).unwrap().as_deref(), Some("active"));
            assert!(!recycle.exists());
        }
    }

    #[test]
    fn outside_root_targets_preserve_all_files_before_snapshotting() {
        for escape in ["outside.asi", "game/../outside.asi"] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("game");
            std::fs::create_dir(&root).unwrap();
            let valid = root.join("valid.asi");
            let outside = dir.path().join("outside.asi");
            std::fs::write(&valid, b"valid payload").unwrap();
            std::fs::write(&outside, b"outside payload").unwrap();
            let mut conn = crate::db::open_in_memory().unwrap();
            let id = setup_installed_mod(&conn, &root, "valid.asi", None);
            conn.execute(
                "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) VALUES (?1, ?2, 'hash')",
                rusqlite::params![id, dir.path().join(escape).to_string_lossy().as_ref()],
            ).unwrap();
            let recycle = dir.path().join("recycle");
            assert!(uninstall(&mut conn, id, &root, &recycle).is_err());
            assert_eq!(std::fs::read(valid).unwrap(), b"valid payload");
            assert_eq!(std::fs::read(outside).unwrap(), b"outside payload");
            assert_eq!(mod_status(&conn, id).unwrap().as_deref(), Some("active"));
            assert!(!recycle.exists());
        }
    }

    // OpenAI-Codex: Missing descendants and inactive records remain compatible.
    #[test]
    fn missing_nested_target_and_disabled_owner_do_not_block_uninstall() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir(&root).unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let id = setup_installed_mod(&conn, &root, "missing/sub/mod.asi", None);
        let inactive = setup_installed_mod(&conn, &root, "missing/sub/mod.asi", None);
        conn.execute(
            "UPDATE installed_mod SET status = 'disabled' WHERE id = ?1",
            [inactive],
        )
        .unwrap();
        uninstall(&mut conn, id, &root, &dir.path().join("recycle")).unwrap();
        assert_eq!(
            mod_status(&conn, id).unwrap().as_deref(),
            Some("uninstalled")
        );
        assert_eq!(
            mod_status(&conn, inactive).unwrap().as_deref(),
            Some("disabled")
        );
        assert!(!root.join("missing").exists());
    }

    #[cfg(unix)]
    #[test]
    fn parent_symlink_escape_is_rejected_even_when_target_is_missing() {
        for present in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("game");
            let outside = dir.path().join("outside");
            std::fs::create_dir(&root).unwrap();
            std::fs::create_dir(&outside).unwrap();
            std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
            if present {
                std::fs::write(outside.join("mod.asi"), b"outside payload").unwrap();
            }
            let mut conn = crate::db::open_in_memory().unwrap();
            let id = setup_installed_mod(&conn, &root, "linked/mod.asi", None);
            let recycle = dir.path().join("recycle");
            assert!(uninstall(&mut conn, id, &root, &recycle).is_err());
            if present {
                assert_eq!(
                    std::fs::read(outside.join("mod.asi")).unwrap(),
                    b"outside payload"
                );
            }
            assert_eq!(mod_status(&conn, id).unwrap().as_deref(), Some("active"));
            assert!(!recycle.exists());
        }
    }

    #[test]
    fn invalid_backup_preserves_every_target_before_snapshotting() {
        for invalid_kind in [
            "missing",
            "directory",
            "deployed-target",
            "other-deployed-target",
            "shared-backup",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let game_root = dir.path().join("game");
            std::fs::create_dir(&game_root).unwrap();
            let first = game_root.join("first.asi");
            let second = game_root.join("second.asi");
            std::fs::write(&first, b"first mod").unwrap();
            std::fs::write(&second, b"second mod").unwrap();
            let backup = match invalid_kind {
                "directory" => {
                    let path = dir.path().join("backup-directory");
                    std::fs::create_dir(&path).unwrap();
                    path
                }
                "deployed-target" => second.clone(),
                "other-deployed-target" => first.clone(),
                "shared-backup" => {
                    let path = dir.path().join("shared.bak");
                    std::fs::write(&path, b"original").unwrap();
                    path
                }
                _ => dir.path().join("missing.bak"),
            };
            let mut conn = crate::db::open_in_memory().unwrap();
            let first_backup = if invalid_kind == "shared-backup" {
                Some(backup.as_path())
            } else {
                None
            };
            let id = setup_installed_mod(&conn, &game_root, "first.asi", first_backup);
            conn.execute(
                "INSERT INTO installed_mod_file (installed_mod_id, target_path, backup_path, file_hash) VALUES (?1, ?2, ?3, 'hash')",
                rusqlite::params![id, second.to_string_lossy().as_ref(), backup.to_string_lossy().as_ref()],
            ).unwrap();
            let recycle = dir.path().join("recycle");
            assert!(
                uninstall(&mut conn, id, &game_root, &recycle).is_err(),
                "{invalid_kind}"
            );
            assert_eq!(std::fs::read(&first).unwrap(), b"first mod");
            assert_eq!(std::fs::read(&second).unwrap(), b"second mod");
            assert!(!recycle.exists());
            assert_eq!(mod_status(&conn, id).unwrap().as_deref(), Some("active"));
        }
    }

    #[cfg(windows)]
    #[test]
    fn unreadable_locked_backup_does_not_remove_the_deployed_file() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir(&game_root).unwrap();
        let target = game_root.join("mod.asi");
        let backup = dir.path().join("original.bak");
        std::fs::write(&target, b"mod content").unwrap();
        std::fs::write(&backup, b"original content").unwrap();
        let locked_backup = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&backup)
            .unwrap();
        locked_backup.try_lock().unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let id = setup_installed_mod(&conn, &game_root, "mod.asi", Some(&backup));
        let recycle = dir.path().join("recycle");
        let result = uninstall(&mut conn, id, &game_root, &recycle);
        locked_backup.unlock().unwrap();
        assert!(result.is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"mod content");
        assert_eq!(std::fs::read(backup).unwrap(), b"original content");
        assert!(!recycle.exists());
        assert_eq!(mod_status(&conn, id).unwrap().as_deref(), Some("active"));
    }

    #[cfg(windows)]
    #[test]
    fn unreadable_later_target_preserves_earlier_files() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir(&game_root).unwrap();
        let first = game_root.join("first.asi");
        let second = game_root.join("second.asi");
        std::fs::write(&first, b"first mod").unwrap();
        std::fs::write(&second, b"second mod").unwrap();
        let locked_target = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&second)
            .unwrap();
        locked_target.try_lock().unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let id = setup_installed_mod(&conn, &game_root, "first.asi", None);
        conn.execute("INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) VALUES (?1, ?2, 'hash')", rusqlite::params![id, second.to_string_lossy().as_ref()]).unwrap();
        let recycle = dir.path().join("recycle");
        let result = uninstall(&mut conn, id, &game_root, &recycle);
        locked_target.unlock().unwrap();
        assert!(result.is_err());
        assert_eq!(std::fs::read(first).unwrap(), b"first mod");
        assert_eq!(std::fs::read(second).unwrap(), b"second mod");
        assert!(!recycle.exists());
        assert_eq!(mod_status(&conn, id).unwrap().as_deref(), Some("active"));
    }

    fn setup_installed_mod(
        conn: &Connection,
        game_root: &Path,
        target_name: &str,
        backup: Option<&Path>,
    ) -> i64 {
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status) \
             VALUES ('TestMod', 'asi', ?1, 'active')",
            [game_root
                .to_str()
                .expect("synthetic game root must be Unicode")],
        )
        .unwrap();
        let mod_id = conn.last_insert_rowid();
        let target = game_root.join(target_name);
        conn.execute(
            "INSERT INTO installed_mod_file (installed_mod_id, target_path, backup_path, file_hash) \
             VALUES (?1, ?2, ?3, 'hash')",
            rusqlite::params![
                mod_id,
                target.to_string_lossy(),
                backup.map(|p| p.to_string_lossy().into_owned()),
            ],
        )
        .unwrap();
        mod_id
    }

    #[test]
    fn uninstall_deletes_file_and_marks_status() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        std::fs::write(game_root.join("mod.asi"), b"payload").unwrap();

        let mut conn = crate::db::open_in_memory().unwrap();
        let mod_id = setup_installed_mod(&conn, &game_root, "mod.asi", None);

        uninstall(&mut conn, mod_id, &game_root, &dir.path().join("recycle")).unwrap();

        assert!(!game_root.join("mod.asi").exists());
        let status: String = conn
            .query_row(
                "SELECT status FROM installed_mod WHERE id = ?1",
                [mod_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "uninstalled");
    }

    #[test]
    fn uninstall_restores_backed_up_original() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let backup_path = dir.path().join("backup_of_original.dll");
        std::fs::write(&backup_path, b"original content").unwrap();
        std::fs::write(game_root.join("shared.dll"), b"mod's overwritten content").unwrap();

        let mut conn = crate::db::open_in_memory().unwrap();
        let mod_id = setup_installed_mod(&conn, &game_root, "shared.dll", Some(&backup_path));

        uninstall(&mut conn, mod_id, &game_root, &dir.path().join("recycle")).unwrap();

        assert_eq!(
            std::fs::read(game_root.join("shared.dll")).unwrap(),
            b"original content"
        );
    }

    #[test]
    fn uninstall_snapshots_into_recycle_bin_with_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        std::fs::write(game_root.join("mod.asi"), b"payload").unwrap();
        let recycle_root = dir.path().join("recycle");

        let mut conn = crate::db::open_in_memory().unwrap();
        let mod_id = setup_installed_mod(&conn, &game_root, "mod.asi", None);

        uninstall(&mut conn, mod_id, &game_root, &recycle_root).unwrap();

        let (snapshot_path, metadata_json, deleted_at, expires_at): (
            String,
            String,
            String,
            String,
        ) = conn
            .query_row(
                "SELECT mod_package_snapshot_path, snapshot_metadata, deleted_at, expires_at \
                 FROM recycle_bin_entry WHERE original_installed_mod_id = ?1",
                [mod_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();

        let metadata: crate::recycle_bin::SnapshotMetadata =
            serde_json::from_str(&metadata_json).unwrap();
        let file_id: i64 = conn
            .query_row(
                "SELECT id FROM installed_mod_file WHERE installed_mod_id = ?1",
                [mod_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            metadata.target_root,
            game_root.to_string_lossy().into_owned()
        );
        assert_eq!(metadata.file_ids, vec![file_id]);

        let snapshot_file = PathBuf::from(&snapshot_path).join("mod.asi");
        assert!(snapshot_file.exists(), "snapshot should preserve the file");
        assert_eq!(std::fs::read(&snapshot_file).unwrap(), b"payload");

        let deleted_at = chrono::DateTime::parse_from_rfc3339(&deleted_at).unwrap();
        let expires_at = chrono::DateTime::parse_from_rfc3339(&expires_at).unwrap();
        assert_eq!((expires_at - deleted_at).num_days(), RETENTION_DAYS);
    }

    // OpenAI-Codex: Both uninstall branches must leave XML edits to the user.
    fn assert_manual_dlc_uninstall_preserves_xml(disabled: bool) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        let relative = "mods/update/x64/dlcpacks/ManualCar/dlc.rpf";
        let target = root.join(relative);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"synthetic pack").unwrap();
        let xml = dlc::dlclist_path(&root);
        let original = b"<Paths><Item>dlcpacks:\\ManualCar\\</Item></Paths>";
        std::fs::create_dir_all(xml.parent().unwrap()).unwrap();
        std::fs::write(&xml, original).unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let mod_id = setup_installed_mod(&conn, &root, relative, None);
        conn.execute(
            "UPDATE installed_mod SET install_path = ?1, source_type = 'folder' WHERE id = ?2",
            rusqlite::params![root.to_string_lossy(), mod_id],
        )
        .unwrap();
        let staging = dir.path().join("staging");
        if disabled {
            let fixture_stage = staging.join(mod_id.to_string());
            std::fs::create_dir_all(&fixture_stage).unwrap();
            std::fs::rename(&target, fixture_stage.join("0")).unwrap();
            crate::uninstall::seed_disabled_fixture_from_staging(&conn, mod_id, &staging);
        }
        uninstall_with_staging(
            &mut conn,
            mod_id,
            &root,
            &dir.path().join("recycle"),
            &staging,
        )
        .unwrap();
        assert_eq!(std::fs::read(&xml).unwrap(), original);
        assert!(!target.exists());
        let (intents, settled): (i64, i64) = conn.query_row(
                "SELECT COALESCE(SUM(phase = 'intent'), 0), COALESCE(SUM(phase = 'settled'), 0) \
                 FROM manual_dlc_event WHERE pack_display = 'ManualCar' AND operation = 'uninstall'",
                [], |row| Ok((row.get(0)?, row.get(1)?)),
            ).unwrap();
        assert_eq!((intents, settled), (1, 1));
    }

    #[test]
    fn manual_dlc_active_uninstall_preserves_xml_and_records_work() {
        assert_manual_dlc_uninstall_preserves_xml(false);
    }

    #[test]
    fn manual_dlc_disabled_uninstall_preserves_xml_and_records_work() {
        assert_manual_dlc_uninstall_preserves_xml(true);
    }

    #[test]
    fn uninstalling_add_on_pack_defers_dlclist_entry_removal() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        let dlclist_path = dlc::dlclist_path(&game_root);
        std::fs::create_dir_all(dlclist_path.parent().unwrap()).unwrap();
        std::fs::write(
            &dlclist_path,
            "<SMandatoryPacksData>\n  <Paths>\n    <Item>dlcpacks:\\MyAddonCar\\</Item>\n  </Paths>\n</SMandatoryPacksData>\n",
        )
        .unwrap();
        let pack_target = game_root.join("mods/update/x64/dlcpacks/MyAddonCar/dlc.rpf");
        std::fs::create_dir_all(pack_target.parent().unwrap()).unwrap();
        std::fs::write(&pack_target, b"payload").unwrap();

        let mut conn = crate::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status) \
             VALUES ('My Addon Car', 'folder', ?1, 'active')",
            [game_root
                .to_str()
                .expect("synthetic game root must be Unicode")],
        )
        .unwrap();
        let mod_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) VALUES (?1, ?2, 'hash')",
            rusqlite::params![mod_id, pack_target.to_string_lossy()],
        )
        .unwrap();

        uninstall(&mut conn, mod_id, &game_root, &dir.path().join("recycle")).unwrap();

        assert!(dlc::has_entry(&dlclist_path, "MyAddonCar").unwrap());
        let tasks = dlc_manual::list_tasks(&conn, Some(&game_root)).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].recommendation, "check_removal");
    }

    #[test]
    fn uninstalling_unknown_mod_id_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let err = uninstall(&mut conn, 999, dir.path(), &dir.path().join("recycle")).unwrap_err();
        assert!(matches!(err, CoreError::UnsupportedFormat { .. }));
    }

    #[test]
    fn uninstalling_twice_errors_on_second_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        std::fs::write(game_root.join("mod.asi"), b"payload").unwrap();

        let mut conn = crate::db::open_in_memory().unwrap();
        let mod_id = setup_installed_mod(&conn, &game_root, "mod.asi", None);

        uninstall(&mut conn, mod_id, &game_root, &dir.path().join("recycle")).unwrap();
        let err =
            uninstall(&mut conn, mod_id, &game_root, &dir.path().join("recycle")).unwrap_err();
        assert!(matches!(err, CoreError::UnsupportedFormat { .. }));
    }
}
