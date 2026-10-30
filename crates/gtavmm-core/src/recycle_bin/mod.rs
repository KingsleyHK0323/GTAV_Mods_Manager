// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex
// OpenAI-Codex_Responsible: Source_Code

//! Recycle bin: snapshot-at-uninstall-time storage of deployed files (written by
//! `uninstall`), no count limit, 15-day retention (`expires_at = deleted_at + 15
//! days`), swept on app/CLI startup — there is no background daemon (per the
//! project's "no always-on service" decision), so sweeping only happens when
//! something actually calls [`sweep_expired`].
//!
//! Restore replays files back through the guarded write path (still subject to
//! `protected_files`), and — since `uninstall` already consumed each file's original
//! `backup_path` by restoring it in place — re-establishes a fresh backup of whatever
//! is currently at each target before overwriting it, so a later re-uninstall can
//! still correctly restore the pre-mod state.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};
use crate::protected_files;
use crate::util::hash_file;

pub const RETENTION_DAYS: i64 = 15;

#[derive(Debug, Clone, Serialize)]
pub struct RecycleBinEntry {
    pub id: i64,
    pub original_installed_mod_id: Option<i64>,
    pub deleted_at: String,
    pub expires_at: String,
}

/// Every recycle bin entry, most recently deleted first — the CLI's own `recycle-bin
/// list` used to run this query inline; factored out here so the GUI can show the
/// same thing without duplicating it.
pub fn list(conn: &Connection) -> CoreResult<Vec<RecycleBinEntry>> {
    list_for_mode(conn, None)
}

/// Lists recycle-bin entries, optionally restricted to the owning mod's workspace mode.
pub fn list_for_mode(conn: &Connection, mode: Option<&str>) -> CoreResult<Vec<RecycleBinEntry>> {
    let mut stmt = conn.prepare(
        "SELECT rb.id, rb.original_installed_mod_id, rb.deleted_at, rb.expires_at \
         FROM recycle_bin_entry AS rb \
         LEFT JOIN installed_mod AS owner_mod ON owner_mod.id = rb.original_installed_mod_id \
         WHERE (?1 IS NULL OR owner_mod.mode = ?1) \
         ORDER BY rb.deleted_at DESC",
    )?;
    let rows = stmt.query_map([mode], |row| {
        Ok(RecycleBinEntry {
            id: row.get(0)?,
            original_installed_mod_id: row.get(1)?,
            deleted_at: row.get(2)?,
            expires_at: row.get(3)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

struct RecycleBinEntryRow {
    original_installed_mod_id: Option<i64>,
    mod_package_snapshot_path: String,
    snapshot_metadata: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SnapshotMetadata {
    pub target_root: String,
    pub file_ids: Vec<i64>,
}

pub(crate) fn encode_snapshot_metadata(
    target_root: &Path,
    file_ids: Vec<i64>,
) -> CoreResult<String> {
    serde_json::to_string(&SnapshotMetadata {
        target_root: target_root.to_string_lossy().into_owned(),
        file_ids,
    })
    .map_err(|error| CoreError::UnsupportedFormat {
        reason: format!("could not encode recycle-bin snapshot metadata: {error}"),
    })
}

fn load_entry(conn: &Connection, entry_id: i64) -> CoreResult<RecycleBinEntryRow> {
    conn.query_row(
        "SELECT original_installed_mod_id, mod_package_snapshot_path, snapshot_metadata \
         FROM recycle_bin_entry WHERE id = ?1",
        [entry_id],
        |row| {
            Ok(RecycleBinEntryRow {
                original_installed_mod_id: row.get(0)?,
                mod_package_snapshot_path: row.get(1)?,
                snapshot_metadata: row.get(2)?,
            })
        },
    )
    .map_err(|e| {
        if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
            CoreError::UnsupportedFormat {
                reason: format!("no recycle bin entry with id {entry_id}"),
            }
        } else {
            e.into()
        }
    })
}

/// Reports whether an entry has enough saved metadata to restore without a current
/// game-root lookup.
pub fn has_recorded_snapshot_metadata(conn: &Connection, entry_id: i64) -> CoreResult<bool> {
    Ok(load_entry(conn, entry_id)?.snapshot_metadata.is_some())
}

// OpenAI-Codex: Resolve missing descendants through their nearest existing ancestor.
// This is a preflight check, not protection against later directory replacement.
fn resolved_restore_path(path: &Path) -> CoreResult<PathBuf> {
    let invalid = || CoreError::UnsupportedFormat {
        reason: format!("unsafe restore path: {}", path.display()),
    };
    if !path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err(invalid());
    }
    let mut ancestor = path;
    let mut missing = Vec::new();
    loop {
        match std::fs::canonicalize(ancestor) {
            Ok(mut resolved) => {
                for name in missing.iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::symlink_metadata(ancestor) {
                    Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => {}
                    Err(other) => return Err(other.into()),
                    Ok(_) => return Err(invalid()),
                }
                missing.push(ancestor.file_name().ok_or_else(invalid)?.to_os_string());
                ancestor = ancestor.parent().ok_or_else(invalid)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn restore_path_key(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

/// Restores a recycle-bin entry: copies its snapshotted files back to their original
/// target paths, reinstates the owning mod as `active`, and removes the entry (its
/// job is done — the 15-day window doesn't apply to something already restored).
pub fn restore(
    conn: &mut Connection,
    entry_id: i64,
    game_root: &Path,
    backup_root: &Path,
) -> CoreResult<()> {
    restore_with_optional_game_root(conn, entry_id, Some(game_root), backup_root)
}

/// Restores entries created with recorded snapshot metadata without needing the
/// current GTA V root. Legacy entries use the supplied root as a fallback.
pub fn restore_with_optional_game_root(
    conn: &mut Connection,
    entry_id: i64,
    game_root: Option<&Path>,
    backup_root: &Path,
) -> CoreResult<()> {
    let entry = load_entry(conn, entry_id)?;
    let mod_id = entry
        .original_installed_mod_id
        .ok_or_else(|| CoreError::UnsupportedFormat {
            reason: format!("recycle bin entry {entry_id} has no associated mod to restore"),
        })?;
    // OpenAI-Codex: Old snapshots must not reactivate a pending workspace.
    crate::providers::ensure_installed_mod_available(conn, mod_id)?;
    // OpenAI-Codex: An older entry must not replay over an active/disabled mod.
    let status: String = conn.query_row(
        "SELECT status FROM installed_mod WHERE id = ?1",
        [mod_id],
        |row| row.get(0),
    )?;
    if status != "uninstalled" {
        return Err(CoreError::UnsupportedFormat {
            reason: format!("mod {mod_id} is not uninstalled (status: {status}); cannot restore its recycle-bin entry"),
        });
    }
    let snapshot_root = PathBuf::from(&entry.mod_package_snapshot_path);
    let metadata: Option<SnapshotMetadata> = entry
        .snapshot_metadata
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|error| CoreError::UnsupportedFormat {
            reason: format!("invalid recycle-bin snapshot metadata: {error}"),
        })?;
    let snapshot_game_root = if let Some(metadata) = &metadata {
        PathBuf::from(&metadata.target_root)
    } else {
        game_root
            .ok_or_else(|| CoreError::UnsupportedFormat {
                reason: format!(
                    "recycle bin entry {entry_id} has no recorded target root; a game root is required for this legacy entry"
                ),
            })?
            .to_path_buf()
    };

    crate::providers::ensure_game_root_available(&snapshot_game_root)?;

    let invalid_layout = || CoreError::UnsupportedFormat {
        reason: format!("recycle bin entry {entry_id} has unsafe or overlapping restore storage"),
    };
    if !std::fs::symlink_metadata(&snapshot_root)?
        .file_type()
        .is_dir()
        || !std::fs::metadata(&snapshot_game_root)?.is_dir()
    {
        return Err(invalid_layout());
    }
    let resolved_game = restore_path_key(&resolved_restore_path(&snapshot_game_root)?);
    let resolved_snapshot = restore_path_key(&resolved_restore_path(&snapshot_root)?);
    let resolved_backup = restore_path_key(&resolved_restore_path(backup_root)?);
    if resolved_backup.starts_with(&resolved_snapshot)
        || resolved_snapshot.starts_with(&resolved_backup)
    {
        return Err(invalid_layout());
    }
    match std::fs::symlink_metadata(backup_root) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => return Err(invalid_layout()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let mut stmt = conn.prepare(
        "SELECT id, target_path FROM installed_mod_file WHERE installed_mod_id = ?1 ORDER BY id",
    )?;
    let files: Vec<(i64, String)> = stmt
        .query_map([mod_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    drop(stmt);

    let all_file_ids = files
        .iter()
        .map(|(file_id, _)| *file_id)
        .collect::<std::collections::HashSet<_>>();
    let expected_file_ids = metadata
        .as_ref()
        .map(|metadata| {
            metadata
                .file_ids
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
        })
        .unwrap_or_else(|| all_file_ids.clone());
    if expected_file_ids.is_empty() {
        return Err(CoreError::UnsupportedFormat {
            reason: format!("recycle bin entry {entry_id} has no snapshotted files to restore"),
        });
    }
    if expected_file_ids
        .iter()
        .any(|file_id| !all_file_ids.contains(file_id))
    {
        return Err(CoreError::UnsupportedFormat {
            reason: format!(
                "recycle bin entry {entry_id} refers to missing installed-file records"
            ),
        });
    }

    // Validate every expected snapshot before writing any target or changing the DB.
    // New entries record the exact file set; legacy entries conservatively require all
    // current file rows to have snapshots rather than consuming a possibly incomplete bin.
    let mut restore_files = Vec::with_capacity(expected_file_ids.len());
    let mut target_keys = std::collections::HashSet::new();
    let mut source_keys = std::collections::HashSet::new();
    for (file_id, target_path) in &files {
        if !expected_file_ids.contains(file_id) {
            continue;
        }
        let target = PathBuf::from(target_path);
        let relative = target.strip_prefix(&snapshot_game_root).map_err(|_| {
            CoreError::UnsupportedFormat {
                reason: format!(
                    "recycle bin entry {entry_id} target is outside its recorded root: {target_path}"
                ),
            }
        })?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(invalid_layout());
        }
        let target_key = restore_path_key(&resolved_restore_path(&target)?);
        if !target_key.starts_with(&resolved_game)
            || target_key.starts_with(&resolved_snapshot)
            || !target_keys.insert(target_key)
        {
            return Err(invalid_layout());
        }
        match std::fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.file_type().is_file() => {
                hash_file(&target)?;
            }
            Ok(_) => return Err(invalid_layout()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let snapshot_file = snapshot_root.join(relative);
        if !std::fs::symlink_metadata(&snapshot_file)?
            .file_type()
            .is_file()
        {
            return Err(CoreError::UnsupportedFormat {
                reason: format!(
                    "recycle bin entry {entry_id} is missing snapshot for {target_path}"
                ),
            });
        }
        let source_key = restore_path_key(&resolved_restore_path(&snapshot_file)?);
        if !source_key.starts_with(&resolved_snapshot) || !source_keys.insert(source_key) {
            return Err(invalid_layout());
        }
        hash_file(&snapshot_file)?;
        protected_files::check_write(&target)?;
        restore_files.push((*file_id, target, snapshot_file));
    }
    if !target_keys.is_disjoint(&source_keys)
        || target_keys
            .iter()
            .any(|target| resolved_backup.starts_with(target))
    {
        return Err(invalid_layout());
    }

    // OpenAI-Codex: Check the complete write set before directories, backups, or
    // file-row changes. Untracked originals retain the existing backup behavior.
    let targets: Vec<PathBuf> = restore_files
        .iter()
        .map(|(_, target, _)| target.clone())
        .collect();
    let report = crate::conflict::analyze(conn, &targets)?;
    for conflict in report.foreign_conflicts {
        if let Some(owner_mod_id) = conflict.owner_mod_id {
            if owner_mod_id != mod_id {
                return Err(CoreError::ForeignConflict {
                    path: conflict.path,
                    owner_mod_id,
                });
            }
        }
    }

    // OpenAI-Codex: Complete every backup before changing the first game target.
    // Keep partial backups on error. create_new plus the retained handle prevents
    // overwriting an existing candidate; it does not make target writes atomic.
    let mut backups = std::collections::HashMap::new();
    for (file_id, target, _) in &restore_files {
        let new_backup_path = if target.try_exists()? {
            std::fs::create_dir_all(backup_root)?;
            let mut selected = None;
            for attempt in 0..64 {
                let path = backup_root.join(format!("restore-{entry_id}-{file_id}-{attempt}.bak"));
                let key = restore_path_key(&resolved_restore_path(&path)?);
                if key.starts_with(&resolved_snapshot)
                    || target_keys.contains(&key)
                    || source_keys.contains(&key)
                {
                    continue;
                }
                let mut output = match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                {
                    Ok(file) => file,
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(error.into()),
                };
                let mut input = std::fs::File::open(target)?;
                std::io::copy(&mut input, &mut output)?;
                output.sync_all()?;
                selected = Some(path);
                break;
            }
            Some(selected.ok_or_else(|| CoreError::UnsupportedFormat { reason: format!("no unused restore backup candidate for file {file_id}; existing recovery data was preserved") })?)
        } else {
            None
        };
        backups.insert(*file_id, new_backup_path);
    }

    for (file_id, target, snapshot_file) in restore_files {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let new_backup_path = backups.remove(&file_id).flatten();

        std::fs::copy(&snapshot_file, &target)?;
        let hash = hash_file(&target)?;

        conn.execute(
            "UPDATE installed_mod_file SET backup_path = ?1, file_hash = ?2 WHERE id = ?3",
            rusqlite::params![
                new_backup_path.map(|p| p.to_string_lossy().into_owned()),
                hash,
                file_id,
            ],
        )?;
    }

    let db_tx = conn.transaction()?;
    db_tx.execute(
        "UPDATE installed_mod SET status = 'active' WHERE id = ?1",
        [mod_id],
    )?;
    db_tx.execute(
        "INSERT INTO install_event (installed_mod_id, event_type, success) VALUES (?1, 'restore', 1)",
        [mod_id],
    )?;
    db_tx.execute("DELETE FROM recycle_bin_entry WHERE id = ?1", [entry_id])?;
    db_tx.commit()?;

    if snapshot_root.exists() {
        let _ = std::fs::remove_dir_all(&snapshot_root); // best-effort cleanup
    }

    Ok(())
}

/// Deletes any `recycle_bin_entry` rows (and their snapshot storage) past
/// `expires_at`. Called once at app/CLI startup and on demand — there is no
/// background scheduler.
/// OpenAI-Codex: Pending or unverifiable workspaces retain their snapshots.
pub fn sweep_expired(conn: &Connection) -> CoreResult<usize> {
    let now = chrono::Utc::now().to_rfc3339();

    let mut stmt = conn.prepare(
        "SELECT id, mod_package_snapshot_path, original_installed_mod_id FROM recycle_bin_entry WHERE expires_at < ?1",
    )?;
    let expired: Vec<(i64, String, Option<i64>)> = stmt
        .query_map([&now], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<_, _>>()?;
    drop(stmt);

    let mut removed = 0;
    for (id, snapshot_path, mod_id) in &expired {
        // OpenAI-Codex: Keep snapshots whose workspace cannot currently be used.
        if let Some(mod_id) = mod_id {
            if crate::providers::ensure_installed_mod_available(conn, *mod_id).is_err() {
                continue;
            }
        }
        let path = PathBuf::from(snapshot_path);
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        conn.execute("DELETE FROM recycle_bin_entry WHERE id = ?1", [id])?;
        removed += 1;
    }

    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    // OpenAI-Codex: Unsafe plans must fail without touching targets or the entry.
    #[test]
    fn unsafe_restore_paths_are_rejected_before_writes() {
        for invalid in [
            "parent",
            "empty",
            "outside",
            "duplicate",
            "snapshot-target",
            "backup-snapshot",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("game");
            std::fs::create_dir(&root).unwrap();
            let mut conn = crate::db::open_in_memory().unwrap();
            let (id, entry) = setup_uninstalled_mod_with_recycle_entry(
                &conn,
                &root,
                &dir.path().join("recycle"),
                "mod.asi",
                15,
            );
            let snapshot =
                PathBuf::from(load_entry(&conn, entry).unwrap().mod_package_snapshot_path);
            std::fs::write(root.join("mod.asi"), b"current original").unwrap();
            let outside = dir.path().join("outside.asi");
            std::fs::write(&outside, b"outside data").unwrap();
            let bad_target = match invalid {
                "parent" => Some(root.join("../outside.asi")),
                "empty" => Some(root.clone()),
                "outside" => Some(outside.clone()),
                _ => None,
            };
            if let Some(path) = bad_target {
                conn.execute(
                    "UPDATE installed_mod_file SET target_path = ?1 WHERE installed_mod_id = ?2",
                    rusqlite::params![path.to_string_lossy(), id],
                )
                .unwrap();
            }
            if invalid == "duplicate" {
                conn.execute("INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) VALUES (?1, ?2, 'hash')", rusqlite::params![id, root.join("mod.asi").to_string_lossy()]).unwrap();
            }
            if invalid == "snapshot-target" {
                conn.execute(
                    "UPDATE recycle_bin_entry SET mod_package_snapshot_path = ?1 WHERE id = ?2",
                    rusqlite::params![root.to_string_lossy(), entry],
                )
                .unwrap();
            }
            let backups = if invalid == "backup-snapshot" {
                snapshot.join("backups")
            } else {
                dir.path().join("backups")
            };
            assert!(
                restore(&mut conn, entry, &root, &backups).is_err(),
                "{invalid}"
            );
            assert_eq!(
                std::fs::read(root.join("mod.asi")).unwrap(),
                b"current original"
            );
            assert_eq!(std::fs::read(outside).unwrap(), b"outside data");
            assert_eq!(
                std::fs::read(snapshot.join("mod.asi")).unwrap(),
                b"snapshotted content"
            );
            assert!(!backups.exists());
            assert_eq!(list(&conn).unwrap().len(), 1);
        }
    }

    #[test]
    fn restore_keeps_existing_backup_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir(&root).unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let (id, entry) = setup_uninstalled_mod_with_recycle_entry(
            &conn,
            &root,
            &dir.path().join("recycle"),
            "mod.asi",
            15,
        );
        let file_id: i64 = conn
            .query_row(
                "SELECT id FROM installed_mod_file WHERE installed_mod_id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        let backups = dir.path().join("backups");
        std::fs::create_dir(&backups).unwrap();
        let old = backups.join(format!("restore-{file_id}.bak"));
        let collision = backups.join(format!("restore-{entry}-{file_id}-0.bak"));
        std::fs::write(&old, b"old recovery data").unwrap();
        std::fs::write(&collision, b"earlier attempt").unwrap();
        std::fs::write(root.join("mod.asi"), b"current original").unwrap();
        restore(&mut conn, entry, &root, &backups).unwrap();
        assert_eq!(std::fs::read(old).unwrap(), b"old recovery data");
        assert_eq!(std::fs::read(&collision).unwrap(), b"earlier attempt");
        let recorded: String = conn
            .query_row(
                "SELECT backup_path FROM installed_mod_file WHERE id = ?1",
                [file_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_ne!(Path::new(&recorded), collision.as_path());
        assert_eq!(std::fs::read(recorded).unwrap(), b"current original");
        assert_eq!(
            std::fs::read(root.join("mod.asi")).unwrap(),
            b"snapshotted content"
        );
    }

    #[test]
    fn later_backup_failure_preserves_all_targets_and_database_rows() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir(&root).unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let (id, entry) = setup_uninstalled_mod_with_recycle_entry(
            &conn,
            &root,
            &dir.path().join("recycle"),
            "first.asi",
            15,
        );
        conn.execute("INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) VALUES (?1, ?2, 'hash')", rusqlite::params![id, root.join("second.asi").to_string_lossy()]).unwrap();
        let later = conn.last_insert_rowid();
        let snapshot = PathBuf::from(load_entry(&conn, entry).unwrap().mod_package_snapshot_path);
        std::fs::write(snapshot.join("second.asi"), b"second snapshot").unwrap();
        std::fs::write(root.join("first.asi"), b"first current").unwrap();
        std::fs::write(root.join("second.asi"), b"second current").unwrap();
        let backups = dir.path().join("backups");
        std::fs::create_dir(&backups).unwrap();
        for attempt in 0..64 {
            std::fs::write(
                backups.join(format!("restore-{entry}-{later}-{attempt}.bak")),
                b"retained recovery data",
            )
            .unwrap();
        }
        assert!(restore(&mut conn, entry, &root, &backups).is_err());
        assert_eq!(
            std::fs::read(root.join("first.asi")).unwrap(),
            b"first current"
        );
        assert_eq!(
            std::fs::read(root.join("second.asi")).unwrap(),
            b"second current"
        );
        assert_eq!(list(&conn).unwrap().len(), 1);
        let untouched: i64 = conn.query_row("SELECT COUNT(*) FROM installed_mod_file WHERE installed_mod_id = ?1 AND backup_path IS NULL AND file_hash = 'hash'", [id], |row| row.get(0)).unwrap();
        assert_eq!(untouched, 2);
        assert_eq!(
            std::fs::read(backups.join(format!("restore-{entry}-{later}-0.bak"))).unwrap(),
            b"retained recovery data"
        );
    }

    #[cfg(unix)]
    #[test]
    fn restore_rejects_linked_sources_and_escaping_parents() {
        for invalid in [
            "source-file",
            "source-parent",
            "target-file",
            "target-parent",
            "missing-target-parent",
            "backup-parent",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("game");
            std::fs::create_dir(&root).unwrap();
            let mut conn = crate::db::open_in_memory().unwrap();
            let (id, entry) = setup_uninstalled_mod_with_recycle_entry(
                &conn,
                &root,
                &dir.path().join("recycle"),
                "mod.asi",
                15,
            );
            let snapshot =
                PathBuf::from(load_entry(&conn, entry).unwrap().mod_package_snapshot_path);
            let outside = dir.path().join("outside");
            std::fs::create_dir(&outside).unwrap();
            let outside_file = outside.join("mod.asi");
            std::fs::write(&outside_file, b"outside data").unwrap();
            std::fs::write(root.join("mod.asi"), b"current original").unwrap();
            let mut backups = dir.path().join("backups");
            match invalid {
                "backup-parent" => {
                    let alias = dir.path().join("backup-alias");
                    std::os::unix::fs::symlink(&snapshot, &alias).unwrap();
                    backups = alias.join("backups");
                }
                "source-file" => {
                    std::fs::rename(snapshot.join("mod.asi"), snapshot.join("saved.asi")).unwrap();
                    std::os::unix::fs::symlink(&outside_file, snapshot.join("mod.asi")).unwrap();
                }
                "target-file" => {
                    std::fs::rename(root.join("mod.asi"), root.join("saved.asi")).unwrap();
                    std::os::unix::fs::symlink(&outside_file, root.join("mod.asi")).unwrap();
                }
                _ => {
                    let relative = if invalid == "missing-target-parent" {
                        "linked/missing/mod.asi"
                    } else {
                        "linked/mod.asi"
                    };
                    conn.execute("UPDATE installed_mod_file SET target_path = ?1 WHERE installed_mod_id = ?2", rusqlite::params![root.join(relative).to_string_lossy(), id]).unwrap();
                    if invalid == "source-parent" {
                        std::os::unix::fs::symlink(&outside, snapshot.join("linked")).unwrap();
                    } else {
                        std::fs::create_dir_all(snapshot.join(relative).parent().unwrap()).unwrap();
                        std::fs::write(snapshot.join(relative), b"linked snapshot").unwrap();
                        std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
                    }
                }
            }
            assert!(
                restore(&mut conn, entry, &root, &backups).is_err(),
                "{invalid}"
            );
            assert_eq!(std::fs::read(&outside_file).unwrap(), b"outside data");
            assert!(!backups.exists());
            assert!(snapshot.exists());
            assert_eq!(list(&conn).unwrap().len(), 1);
        }
    }

    #[test]
    fn restore_creates_missing_target_descendants_inside_an_existing_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir(&root).unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let (id, entry) = setup_uninstalled_mod_with_recycle_entry(
            &conn,
            &root,
            &dir.path().join("recycle"),
            "mod.asi",
            15,
        );
        let snapshot = PathBuf::from(load_entry(&conn, entry).unwrap().mod_package_snapshot_path);
        let relative = Path::new("missing").join("nested").join("mod.asi");
        std::fs::create_dir_all(snapshot.join(&relative).parent().unwrap()).unwrap();
        std::fs::rename(snapshot.join("mod.asi"), snapshot.join(&relative)).unwrap();
        conn.execute(
            "UPDATE installed_mod_file SET target_path = ?1 WHERE installed_mod_id = ?2",
            rusqlite::params![root.join(&relative).to_string_lossy(), id],
        )
        .unwrap();
        let backups = dir.path().join("backups");
        restore(&mut conn, entry, &root, &backups).unwrap();
        assert_eq!(
            std::fs::read(root.join(relative)).unwrap(),
            b"snapshotted content"
        );
        assert!(!backups.exists());
        assert!(list(&conn).unwrap().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn unreadable_later_snapshot_preserves_all_targets() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir(&root).unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let (id, entry) = setup_uninstalled_mod_with_recycle_entry(
            &conn,
            &root,
            &dir.path().join("recycle"),
            "first.asi",
            15,
        );
        conn.execute("INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) VALUES (?1, ?2, 'hash')", rusqlite::params![id, root.join("second.asi").to_string_lossy()]).unwrap();
        let snapshot = PathBuf::from(load_entry(&conn, entry).unwrap().mod_package_snapshot_path);
        let second = snapshot.join("second.asi");
        std::fs::write(&second, b"second snapshot").unwrap();
        std::fs::write(root.join("first.asi"), b"first current").unwrap();
        std::fs::write(root.join("second.asi"), b"second current").unwrap();
        let locked = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&second)
            .unwrap();
        locked.try_lock().unwrap();
        let backups = dir.path().join("backups");
        let result = restore(&mut conn, entry, &root, &backups);
        locked.unlock().unwrap();
        assert!(result.is_err());
        assert!(!backups.exists());
        assert_eq!(
            std::fs::read(root.join("first.asi")).unwrap(),
            b"first current"
        );
        assert_eq!(
            std::fs::read(root.join("second.asi")).unwrap(),
            b"second current"
        );
        assert_eq!(list(&conn).unwrap().len(), 1);
    }

    // OpenAI-Codex: A conflict anywhere must stop the entire restore before writes.
    fn assert_active_owner_blocks_restore(conflicting_name: &str, owner_spelling: &str) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir(&root).unwrap();
        let recycle = dir.path().join("recycle");
        let backups = dir.path().join("backups");
        let mut conn = crate::db::open_in_memory().unwrap();
        let (old, entry) =
            setup_uninstalled_mod_with_recycle_entry(&conn, &root, &recycle, "first.asi", 15);
        let snapshot = PathBuf::from(load_entry(&conn, entry).unwrap().mod_package_snapshot_path);
        std::fs::write(snapshot.join("second.asi"), b"second snapshot").unwrap();
        conn.execute("INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) VALUES (?1, ?2, 'hash')", rusqlite::params![old, root.join("second.asi").to_string_lossy()]).unwrap();
        std::fs::write(root.join("first.asi"), b"first current").unwrap();
        std::fs::write(root.join("second.asi"), b"second current").unwrap();
        conn.execute("INSERT INTO installed_mod (name, source_type, install_path, status) VALUES ('New owner', 'asi', '', 'active')", []).unwrap();
        let owner = conn.last_insert_rowid();
        conn.execute("INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) VALUES (?1, ?2, 'owner hash')", rusqlite::params![owner, root.join(owner_spelling).to_string_lossy()]).unwrap();
        let error = restore(&mut conn, entry, &root, &backups).unwrap_err();
        assert!(
            matches!(error, CoreError::ForeignConflict { owner_mod_id, ref path } if owner_mod_id == owner && path.file_name().unwrap().eq_ignore_ascii_case(conflicting_name))
        );
        assert_eq!(
            std::fs::read(root.join("first.asi")).unwrap(),
            b"first current"
        );
        assert_eq!(
            std::fs::read(root.join("second.asi")).unwrap(),
            b"second current"
        );
        assert_eq!(
            std::fs::read(snapshot.join("first.asi")).unwrap(),
            b"snapshotted content"
        );
        assert_eq!(
            std::fs::read(snapshot.join("second.asi")).unwrap(),
            b"second snapshot"
        );
        assert!(!backups.exists());
        assert_eq!(list(&conn).unwrap().len(), 1);
        let states: (String, String) = conn.query_row("SELECT (SELECT status FROM installed_mod WHERE id = ?1), (SELECT status FROM installed_mod WHERE id = ?2)", rusqlite::params![old, owner], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(states, ("uninstalled".to_string(), "active".to_string()));
        let untouched: i64 = conn.query_row("SELECT COUNT(*) FROM installed_mod_file WHERE installed_mod_id = ?1 AND backup_path IS NULL AND file_hash = 'hash'", [old], |row| row.get(0)).unwrap();
        assert_eq!(untouched, 2);
        let events: i64 = conn
            .query_row("SELECT COUNT(*) FROM install_event", [], |row| row.get(0))
            .unwrap();
        assert_eq!(events, 0);
    }

    #[test]
    fn another_active_owner_blocks_the_entire_restore() {
        for target in ["first.asi", "second.asi"] {
            assert_active_owner_blocks_restore(target, target);
        }
    }

    #[cfg(windows)]
    #[test]
    fn active_owner_case_alias_blocks_restore() {
        assert_active_owner_blocks_restore("second.asi", "SECOND.ASI");
    }

    #[test]
    fn restore_rejects_a_mod_that_is_no_longer_uninstalled() {
        for status in ["active", "disabled"] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("game");
            std::fs::create_dir(&root).unwrap();
            let mut conn = crate::db::open_in_memory().unwrap();
            let (id, entry) = setup_uninstalled_mod_with_recycle_entry(
                &conn,
                &root,
                &dir.path().join("recycle"),
                "mod.asi",
                15,
            );
            conn.execute(
                "UPDATE installed_mod SET status = ?1 WHERE id = ?2",
                rusqlite::params![status, id],
            )
            .unwrap();
            std::fs::write(root.join("mod.asi"), b"current content").unwrap();
            let backups = dir.path().join("backups");
            assert!(matches!(
                restore(&mut conn, entry, &root, &backups),
                Err(CoreError::UnsupportedFormat { .. })
            ));
            assert_eq!(
                std::fs::read(root.join("mod.asi")).unwrap(),
                b"current content"
            );
            assert!(!backups.exists());
            assert_eq!(list(&conn).unwrap().len(), 1);
            let snapshot =
                PathBuf::from(load_entry(&conn, entry).unwrap().mod_package_snapshot_path);
            assert_eq!(
                std::fs::read(snapshot.join("mod.asi")).unwrap(),
                b"snapshotted content"
            );
        }
    }

    #[test]
    fn disabled_uninstall_snapshot_cannot_overwrite_a_new_active_owner() {
        // OpenAI-Codex: Exercise the actual staging-aware snapshot producer.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir(&root).unwrap();
        let target = root.join("mod.asi");
        std::fs::write(&target, b"old mod payload").unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        conn.execute("INSERT INTO installed_mod (name, source_type, install_path, status) VALUES ('Old', 'asi', '', 'active')", []).unwrap();
        let old = conn.last_insert_rowid();
        conn.execute("INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) VALUES (?1, ?2, 'old hash')", rusqlite::params![old, target.to_string_lossy()]).unwrap();
        let staging = dir.path().join("staging");
        let recycle = dir.path().join("recycle");
        crate::state::disable(&conn, old, &staging).unwrap();
        crate::uninstall::uninstall_with_staging(&mut conn, old, &root, &recycle, &staging)
            .unwrap();
        let entry = list(&conn).unwrap().remove(0);
        conn.execute("INSERT INTO installed_mod (name, source_type, install_path, status) VALUES ('New', 'asi', '', 'active')", []).unwrap();
        let owner = conn.last_insert_rowid();
        conn.execute("INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) VALUES (?1, ?2, 'new hash')", rusqlite::params![owner, target.to_string_lossy()]).unwrap();
        std::fs::write(&target, b"new owner payload").unwrap();
        let backups = dir.path().join("backups");
        assert!(
            matches!(restore(&mut conn, entry.id, &root, &backups), Err(CoreError::ForeignConflict { owner_mod_id, .. }) if owner_mod_id == owner)
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"new owner payload");
        assert!(!backups.exists());
        assert_eq!(list(&conn).unwrap().len(), 1);
        let snapshot = PathBuf::from(
            load_entry(&conn, entry.id)
                .unwrap()
                .mod_package_snapshot_path,
        );
        assert_eq!(
            std::fs::read(snapshot.join("mod.asi")).unwrap(),
            b"old mod payload"
        );
    }

    #[test]
    fn pending_mode_snapshots_survive_expiry_cleanup_and_cannot_be_restored() {
        let mut conn = crate::db::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        let recycle = dir.path().join("recycle");
        let (mod_id, entry_id) = setup_uninstalled_mod_with_recycle_entry(
            &conn,
            &game_root,
            &recycle,
            "pending.asi",
            -1,
        );
        conn.execute(
            "UPDATE installed_mod SET mode = 'fivem-client' WHERE id = ?1",
            [mod_id],
        )
        .unwrap();
        assert_eq!(sweep_expired(&conn).unwrap(), 0);
        assert_eq!(list(&conn).unwrap().len(), 1);
        assert!(restore(&mut conn, entry_id, &game_root, &dir.path().join("backups")).is_err());
        assert!(!game_root.exists());
        assert_eq!(list(&conn).unwrap().len(), 1);
    }

    fn setup_uninstalled_mod_with_recycle_entry(
        conn: &Connection,
        game_root: &Path,
        recycle_root: &Path,
        target_name: &str,
        expires_offset_days: i64,
    ) -> (i64, i64) {
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status) \
             VALUES ('TestMod', 'asi', '', 'uninstalled')",
            [],
        )
        .unwrap();
        let mod_id = conn.last_insert_rowid();
        let target = game_root.join(target_name);
        conn.execute(
            "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) \
             VALUES (?1, ?2, 'hash')",
            rusqlite::params![mod_id, target.to_string_lossy()],
        )
        .unwrap();

        let entry_dir = recycle_root.join(format!("{mod_id}-entry"));
        std::fs::create_dir_all(&entry_dir).unwrap();
        std::fs::write(entry_dir.join(target_name), b"snapshotted content").unwrap();

        let deleted_at = chrono::Utc::now();
        let expires_at = deleted_at + chrono::Duration::days(expires_offset_days);
        conn.execute(
            "INSERT INTO recycle_bin_entry (original_installed_mod_id, mod_package_snapshot_path, deleted_at, expires_at) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![mod_id, entry_dir.to_string_lossy(), deleted_at.to_rfc3339(), expires_at.to_rfc3339()],
        )
        .unwrap();
        let entry_id = conn.last_insert_rowid();

        (mod_id, entry_id)
    }

    #[test]
    fn restore_copies_file_back_and_reactivates_mod() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let recycle_root = dir.path().join("recycle");
        let backup_root = dir.path().join("backups");

        let mut conn = crate::db::open_in_memory().unwrap();
        let (mod_id, entry_id) = setup_uninstalled_mod_with_recycle_entry(
            &conn,
            &game_root,
            &recycle_root,
            "mod.asi",
            15,
        );

        restore(&mut conn, entry_id, &game_root, &backup_root).unwrap();

        assert_eq!(
            std::fs::read(game_root.join("mod.asi")).unwrap(),
            b"snapshotted content"
        );
        let status: String = conn
            .query_row(
                "SELECT status FROM installed_mod WHERE id = ?1",
                [mod_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "active");

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM recycle_bin_entry", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            remaining, 0,
            "restored entry should be removed from the recycle bin"
        );
    }

    #[test]
    fn restore_backs_up_whatever_currently_occupies_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        // Simulate the restored pre-mod original sitting at the target (as left by
        // `uninstall`'s backup-restore step).
        std::fs::write(game_root.join("mod.asi"), b"pre-mod original").unwrap();
        let recycle_root = dir.path().join("recycle");
        let backup_root = dir.path().join("backups");

        let mut conn = crate::db::open_in_memory().unwrap();
        let (_, entry_id) = setup_uninstalled_mod_with_recycle_entry(
            &conn,
            &game_root,
            &recycle_root,
            "mod.asi",
            15,
        );

        restore(&mut conn, entry_id, &game_root, &backup_root).unwrap();

        let backup_path: Option<String> = conn
            .query_row(
                "SELECT backup_path FROM installed_mod_file WHERE target_path = ?1",
                [game_root.join("mod.asi").to_string_lossy()],
                |r| r.get(0),
            )
            .unwrap();
        let backup_path =
            PathBuf::from(backup_path.expect("a fresh backup should have been recorded"));
        assert_eq!(std::fs::read(&backup_path).unwrap(), b"pre-mod original");
    }

    #[test]
    fn restore_uses_the_root_recorded_when_the_snapshot_was_created() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("gta-v");
        let client_root = dir.path().join("fivem");
        let recycle_root = dir.path().join("recycle");
        let backup_root = dir.path().join("backups");
        let target = client_root
            .join("resources")
            .join("vehicles")
            .join("fxmanifest.lua");
        let relative = Path::new("resources/vehicles/fxmanifest.lua");
        let entry_dir = recycle_root.join("client-entry");
        std::fs::create_dir_all(entry_dir.join(relative).parent().unwrap()).unwrap();
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&game_root).unwrap();
        std::fs::write(entry_dir.join(relative), b"client snapshot").unwrap();

        let mut conn = crate::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status) \
             VALUES ('FiveM Test Mod', 'folder', '', 'uninstalled')",
            [],
        )
        .unwrap();
        let mod_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) \
             VALUES (?1, ?2, 'hash')",
            rusqlite::params![mod_id, target.to_string_lossy()],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        let metadata = encode_snapshot_metadata(&client_root, vec![file_id]).unwrap();
        let deleted_at = chrono::Utc::now();
        conn.execute(
            "INSERT INTO recycle_bin_entry (original_installed_mod_id, mod_package_snapshot_path, snapshot_metadata, deleted_at, expires_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                mod_id,
                entry_dir.to_string_lossy(),
                metadata,
                deleted_at.to_rfc3339(),
                (deleted_at + chrono::Duration::days(RETENTION_DAYS)).to_rfc3339(),
            ],
        )
        .unwrap();
        let entry_id = conn.last_insert_rowid();

        restore(&mut conn, entry_id, &game_root, &backup_root).unwrap();

        assert_eq!(std::fs::read(target).unwrap(), b"client snapshot");
        assert!(!entry_dir.exists());
    }

    #[test]
    fn restore_keeps_entry_and_snapshot_when_a_recorded_file_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("gta-v");
        let client_root = dir.path().join("fivem");
        let recycle_root = dir.path().join("recycle");
        let backup_root = dir.path().join("backups");
        let target = client_root
            .join("resources")
            .join("vehicles")
            .join("fxmanifest.lua");
        let entry_dir = recycle_root.join("client-entry");
        std::fs::create_dir_all(&entry_dir).unwrap();
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&game_root).unwrap();

        let mut conn = crate::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status) \
             VALUES ('FiveM Test Mod', 'folder', '', 'uninstalled')",
            [],
        )
        .unwrap();
        let mod_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) \
             VALUES (?1, ?2, 'hash')",
            rusqlite::params![mod_id, target.to_string_lossy()],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        let metadata = encode_snapshot_metadata(&client_root, vec![file_id]).unwrap();
        let deleted_at = chrono::Utc::now();
        conn.execute(
            "INSERT INTO recycle_bin_entry (original_installed_mod_id, mod_package_snapshot_path, snapshot_metadata, deleted_at, expires_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                mod_id,
                entry_dir.to_string_lossy(),
                metadata,
                deleted_at.to_rfc3339(),
                (deleted_at + chrono::Duration::days(RETENTION_DAYS)).to_rfc3339(),
            ],
        )
        .unwrap();
        let entry_id = conn.last_insert_rowid();

        let result = restore(&mut conn, entry_id, &game_root, &backup_root);

        assert!(result.is_err());
        assert!(entry_dir.exists());
        assert!(!target.exists());
        let status: String = conn
            .query_row(
                "SELECT status FROM installed_mod WHERE id = ?1",
                [mod_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "uninstalled");
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM recycle_bin_entry WHERE id = ?1",
                [entry_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 1);
    }

    #[test]
    fn sweep_expired_removes_only_past_due_entries() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let recycle_root = dir.path().join("recycle");

        let conn = crate::db::open_in_memory().unwrap();
        let (_, expired_entry) = setup_uninstalled_mod_with_recycle_entry(
            &conn,
            &game_root,
            &recycle_root,
            "old.asi",
            -1, // already expired
        );
        let (_, fresh_entry) = setup_uninstalled_mod_with_recycle_entry(
            &conn,
            &game_root,
            &recycle_root,
            "new.asi",
            15, // not expired
        );

        let removed = sweep_expired(&conn).unwrap();

        assert_eq!(removed, 1);
        let remaining_id: i64 = conn
            .query_row("SELECT id FROM recycle_bin_entry", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining_id, fresh_entry);
        assert_ne!(remaining_id, expired_entry);
    }

    #[test]
    fn restore_unknown_entry_id_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let err = restore(&mut conn, 999, dir.path(), &dir.path().join("backups")).unwrap_err();
        assert!(matches!(err, CoreError::UnsupportedFormat { .. }));
    }

    #[test]
    fn list_returns_entries_most_recently_deleted_first() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        let recycle_root = dir.path().join("recycle");
        std::fs::create_dir_all(&game_root).unwrap();
        let conn = crate::db::open_in_memory().unwrap();
        assert!(list(&conn).unwrap().is_empty());

        let (_, first_id) =
            setup_uninstalled_mod_with_recycle_entry(&conn, &game_root, &recycle_root, "a.asi", 15);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let (_, second_id) =
            setup_uninstalled_mod_with_recycle_entry(&conn, &game_root, &recycle_root, "b.asi", 15);

        let entries = list(&conn).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, second_id);
        assert_eq!(entries[1].id, first_id);
    }
}
