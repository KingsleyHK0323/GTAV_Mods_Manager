// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex
// OpenAI-Codex_Responsible: Source_Code
// OpenAI-Codex_index_review

//! Enable/disable: moves a mod's currently-deployed files to/from a per-mod staging
//! directory (distinct from the recycle bin — this is instant and reversible, no
//! backup needed since files just move within our own managed space). Disabling also
//! restores anything the mod had overwritten (via its recorded `backup_path`) so the
//! game runs in its pre-mod state while disabled; enabling re-applies the mod's files
//! on top again.
//!
//! Lower-risk than install/uninstall (fully reversible by re-running the opposite
//! operation), so this intentionally does not carry the same rollback-action-log
//! guarantee — a failure partway through leaves some files moved and others not,
//! recoverable by re-running the operation once the underlying issue (e.g. a locked
//! file) is fixed.
//!
//! OpenAI-Codex: The preceding retry guidance does not guarantee recovery:
//! incomplete staging may be refused and require manual review.
//!
//! OpenAI-Codex: New disable requests are under development and are refused.
//! Earlier disabling and disabling-retry descriptions are historical.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::error::{CoreError, CoreResult};
use crate::protected_files;

struct ModFileRow {
    id: i64,
    target_path: String,
    backup_path: Option<String>,
}

fn load_mod_files(conn: &Connection, mod_id: i64) -> CoreResult<Vec<ModFileRow>> {
    let mut stmt = conn.prepare(
        "SELECT target_path, backup_path, id FROM installed_mod_file WHERE installed_mod_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map([mod_id], |row| {
        Ok(ModFileRow {
            target_path: row.get(0)?,
            backup_path: row.get(1)?,
            id: row.get(2)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn mod_status(conn: &Connection, mod_id: i64) -> CoreResult<String> {
    conn.query_row(
        "SELECT status FROM installed_mod WHERE id = ?1",
        [mod_id],
        |row| row.get(0),
    )
    .map_err(|e| {
        if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
            CoreError::UnsupportedFormat {
                reason: format!("no installed mod with id {mod_id}"),
            }
        } else {
            e.into()
        }
    })
}

fn staging_dir(staging_root: &Path, mod_id: i64) -> PathBuf {
    staging_root.join(mod_id.to_string())
}

// OpenAI-Codex: Persist identity and actual staged content, including user edits.
#[derive(serde::Serialize, serde::Deserialize)]
struct StagedFile {
    file_id: i64,
    index: usize,
    hash: String,
}

pub(crate) fn disabled_payloads(
    conn: &Connection,
    mod_id: i64,
    staging_root: &Path,
) -> CoreResult<Vec<(i64, PathBuf)>> {
    let invalid = || {
        CoreError::UnsupportedFormat {
        reason: format!("disabled staging for mod {mod_id} is missing, unsafe, or does not match its file records; preserve it for recovery"),
    }
    };
    let root = std::fs::canonicalize(staging_root)?;
    let directory = staging_dir(staging_root, mod_id);
    if !std::fs::canonicalize(&directory)?.starts_with(&root) {
        return Err(invalid());
    }
    let manifest_path = directory.join("manifest.json");
    let manifest: Option<Vec<StagedFile>> = match std::fs::symlink_metadata(&manifest_path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            Some(serde_json::from_slice(&std::fs::read(&manifest_path)?).map_err(|_| invalid())?)
        }
        Ok(_) => return Err(invalid()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let mut stmt = conn.prepare(
        "SELECT id, file_hash FROM installed_mod_file WHERE installed_mod_id = ?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map([mod_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if rows.is_empty()
        || manifest
            .as_ref()
            .is_some_and(|items| items.len() != rows.len())
    {
        return Err(invalid());
    }
    let mut payloads = Vec::with_capacity(rows.len());
    for (index, (id, legacy_hash)) in rows.into_iter().enumerate() {
        let expected_hash = if let Some(items) = &manifest {
            let item = items
                .iter()
                .find(|item| item.file_id == id && item.index == index)
                .ok_or_else(invalid)?;
            &item.hash
        } else {
            &legacy_hash
        };
        let path = directory.join(index.to_string());
        if !std::fs::symlink_metadata(&path)?.file_type().is_file()
            || !std::fs::canonicalize(&path)?.starts_with(&root)
            || crate::util::hash_file(&path)? != *expected_hash
        {
            return Err(invalid());
        }
        payloads.push((id, path));
    }
    Ok(payloads)
}

// OpenAI-Codex; OpenAI-Codex_Responsible: Source_Code.
fn ensure_disabling_available() -> CoreResult<()> {
    Err(CoreError::ModDisablingUnavailable)
}

/// OpenAI-Codex: Disabling is unavailable; the description below is historical.
/// Deactivates an active mod: its deployed files move to staging, and anything they
/// overwrote is restored from backup so the game runs unmodified while disabled.
pub fn disable(conn: &Connection, mod_id: i64, staging_root: &Path) -> CoreResult<()> {
    ensure_disabling_available()?;
    crate::providers::ensure_installed_mod_available(conn, mod_id)?;
    let status = mod_status(conn, mod_id)?;
    if status != "active" {
        return Err(CoreError::UnsupportedFormat {
            reason: format!("mod {mod_id} is not active (status: {status}); cannot disable"),
        });
    }

    let files = load_mod_files(conn, mod_id)?;
    let mod_staging = staging_dir(staging_root, mod_id);
    std::fs::create_dir_all(&mod_staging)?;
    // OpenAI-Codex: Do not follow a pre-existing manifest link when writing it.
    if !std::fs::canonicalize(&mod_staging)?.starts_with(std::fs::canonicalize(staging_root)?) {
        return Err(CoreError::UnsupportedFormat {
            reason: "disabled staging escapes its root".to_string(),
        });
    }
    match std::fs::symlink_metadata(mod_staging.join("manifest.json")) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(CoreError::UnsupportedFormat {
                reason: "disabled staging manifest is not a regular file".to_string(),
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    for index in 0..files.len() {
        match std::fs::symlink_metadata(mod_staging.join(index.to_string())) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => {
                return Err(CoreError::UnsupportedFormat {
                    reason: "disabled staging destination is not a regular file".to_string(),
                })
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }

    let mut manifest = Vec::new();
    for (index, file) in files.iter().enumerate() {
        let target = PathBuf::from(&file.target_path);
        protected_files::check_write(&target)?;

        if target.exists() {
            let staged = mod_staging.join(index.to_string());
            crate::util::move_file(&target, &staged)?;
            manifest.push(StagedFile {
                file_id: file.id,
                index,
                hash: crate::util::hash_file(&staged)?,
            });
        }

        if let Some(backup) = &file.backup_path {
            let backup_path = PathBuf::from(backup);
            if backup_path.exists() {
                std::fs::copy(&backup_path, &target)?;
            }
        }
    }

    // OpenAI-Codex: Write the manifest before publishing the disabled status.
    std::fs::write(
        mod_staging.join("manifest.json"),
        serde_json::to_vec(&manifest).map_err(|error| CoreError::UnsupportedFormat {
            reason: error.to_string(),
        })?,
    )?;
    conn.execute(
        "UPDATE installed_mod SET status = 'disabled' WHERE id = ?1",
        [mod_id],
    )?;
    conn.execute(
        "INSERT INTO install_event (installed_mod_id, event_type, success) VALUES (?1, 'disable', 1)",
        [mod_id],
    )?;
    Ok(())
}

/// Reactivates a disabled mod: staged files move back to their target paths,
/// overwriting whatever is currently there (the restored-backup state left by
/// `disable`, or nothing if the file didn't exist before this mod was installed).
pub fn enable(conn: &Connection, mod_id: i64, staging_root: &Path) -> CoreResult<()> {
    crate::providers::ensure_installed_mod_available(conn, mod_id)?;
    let status = mod_status(conn, mod_id)?;
    if status != "disabled" {
        return Err(CoreError::UnsupportedFormat {
            reason: format!("mod {mod_id} is not disabled (status: {status}); cannot enable"),
        });
    }

    // OpenAI-Codex: Check disabled payloads before changing targets.
    let _ = disabled_payloads(conn, mod_id, staging_root)?;

    let files = load_mod_files(conn, mod_id)?;

    // OpenAI-Codex_db_prepare_review: Refuse active ownership before moving files.
    let targets = files
        .iter()
        .map(|file| PathBuf::from(&file.target_path))
        .collect::<Vec<_>>();
    for target in &targets {
        protected_files::check_write(target)?;
    }
    let conflicts = crate::conflict::analyze(conn, &targets)?;
    let observed_status = mod_status(conn, mod_id)?;
    if observed_status != "disabled" {
        return Err(CoreError::UnsupportedFormat {
            reason: format!(
                "mod {mod_id} changed status during enable (status: {observed_status}); review state and retry"
            ),
        });
    }
    let mut other_conflict: Option<(PathBuf, i64)> = None;
    for conflict in conflicts.foreign_conflicts {
        if let Some(owner_mod_id) = conflict.owner_mod_id {
            if owner_mod_id == mod_id {
                return Err(CoreError::UnsupportedFormat {
                    reason: format!(
                        "mod {mod_id} became active during enable; review state and retry"
                    ),
                });
            }
            if other_conflict.is_none() {
                other_conflict = Some((conflict.path, owner_mod_id));
            }
        }
    }
    if let Some((path, owner_mod_id)) = other_conflict {
        return Err(CoreError::ForeignConflict { path, owner_mod_id });
    }

    let mod_staging = staging_dir(staging_root, mod_id);

    for (index, file) in files.iter().enumerate() {
        let target = PathBuf::from(&file.target_path);
        protected_files::check_write(&target)?;

        let staged = mod_staging.join(index.to_string());
        if staged.exists() {
            if target.exists() {
                std::fs::remove_file(&target)?;
            }
            crate::util::move_file(&staged, &target)?;
        }
    }

    if mod_staging.exists() {
        let _ = std::fs::remove_dir(&mod_staging); // best-effort; only succeeds if empty
    }

    conn.execute(
        "UPDATE installed_mod SET status = 'active' WHERE id = ?1",
        [mod_id],
    )?;
    conn.execute(
        "INSERT INTO install_event (installed_mod_id, event_type, success) VALUES (?1, 'enable', 1)",
        [mod_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_active_mod(
        conn: &Connection,
        game_root: &Path,
        target_name: &str,
        backup: Option<&Path>,
    ) -> i64 {
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status) \
             VALUES ('TestMod', 'asi', ?1, 'active')",
            rusqlite::params![game_root
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

    // OpenAI-Codex_disable_core_audit: Prepare only synthetic staged test bytes.
    fn stage_synthetic_payloads(
        conn: &Connection,
        staging_root: &Path,
        mod_id: i64,
        payloads: &[&[u8]],
    ) {
        let files = load_mod_files(conn, mod_id).unwrap();
        assert!(!files.is_empty());
        assert_eq!(files.len(), payloads.len());
        std::fs::create_dir_all(staging_root).unwrap();
        let directory = staging_dir(staging_root, mod_id);
        std::fs::create_dir(&directory).unwrap();
        let mut manifest = Vec::with_capacity(files.len());
        for (index, (file, payload)) in files.iter().zip(payloads).enumerate() {
            let staged = directory.join(index.to_string());
            std::fs::write(&staged, *payload).unwrap();
            manifest.push(StagedFile {
                file_id: file.id,
                index,
                hash: crate::util::hash_file(&staged).unwrap(),
            });
        }
        std::fs::write(
            directory.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        assert_eq!(
            disabled_payloads(conn, mod_id, staging_root).unwrap().len(),
            files.len()
        );
    }

    // OpenAI-Codex; OpenAI-Codex_Responsible: Source_Code.
    #[test]
    fn disable_unavailable_preserves_active_mod_and_files() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir(&game_root).unwrap();
        let target = game_root.join("mod.asi");
        let backup = dir.path().join("original.bin");
        let staging = dir.path().join("staging");
        std::fs::write(&target, b"active payload").unwrap();
        std::fs::write(&backup, b"original").unwrap();

        let conn = crate::db::open_in_memory().unwrap();
        let mod_id = setup_active_mod(&conn, &game_root, "mod.asi", Some(&backup));
        let payload_hash = crate::util::hash_file(&target).unwrap();
        assert_eq!(
            conn.execute(
                "UPDATE installed_mod_file SET file_hash = ?1 WHERE installed_mod_id = ?2",
                rusqlite::params![payload_hash, mod_id],
            )
            .unwrap(),
            1
        );
        crate::providers::ensure_installed_mod_available(&conn, mod_id).unwrap();
        let file_row = || -> (String, Option<String>, String) {
            conn.query_row(
                "SELECT target_path, backup_path, file_hash FROM installed_mod_file \
                 WHERE installed_mod_id = ?1",
                [mod_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
        };
        let row_before = file_row();
        let changes_before: i64 = conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        let events_before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM install_event WHERE installed_mod_id = ?1",
                [mod_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(conn.is_autocommit());

        let error = disable(&conn, mod_id, &staging).expect_err("disabling must be unavailable");
        assert!(conn.is_autocommit());
        assert!(
            error.to_string().contains("Mod disabling is unavailable."),
            "{error}"
        );
        assert!(matches!(&error, CoreError::ModDisablingUnavailable));
        assert_eq!(mod_status(&conn, mod_id).unwrap(), "active");
        assert_eq!(std::fs::read(&target).unwrap(), b"active payload");
        assert_eq!(std::fs::read(&backup).unwrap(), b"original");
        assert!(matches!(
            std::fs::symlink_metadata(&staging),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        ));
        assert_eq!(file_row(), row_before);
        assert_eq!(
            conn.query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            changes_before
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM install_event WHERE installed_mod_id = ?1",
                [mod_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            events_before
        );
    }

    #[test]
    fn disable_without_backup_is_unavailable_and_preserves_mod() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        std::fs::write(game_root.join("mod.asi"), b"payload").unwrap();
        let staging_root = dir.path().join("staging");

        let conn = crate::db::open_in_memory().unwrap();
        let mod_id = setup_active_mod(&conn, &game_root, "mod.asi", None);
        let changes_before: i64 = conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();

        let error = disable(&conn, mod_id, &staging_root).unwrap_err();

        assert!(matches!(error, CoreError::ModDisablingUnavailable));
        assert_eq!(
            std::fs::read(game_root.join("mod.asi")).unwrap(),
            b"payload"
        );
        assert!(!staging_root.exists());
        let status: String = conn
            .query_row(
                "SELECT status FROM installed_mod WHERE id = ?1",
                [mod_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "active");
        let changes_after: i64 = conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        assert_eq!(changes_after, changes_before);
    }

    #[test]
    fn enable_synthetic_disabled_mod_reapplies_payload_and_preserves_backup() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let backup_path = dir.path().join("backup.dll");
        std::fs::write(&backup_path, b"original").unwrap();
        std::fs::write(game_root.join("shared.dll"), b"original").unwrap();
        let staging_root = dir.path().join("staging");

        let conn = crate::db::open_in_memory().unwrap();
        let mod_id = setup_active_mod(&conn, &game_root, "shared.dll", Some(&backup_path));
        assert_eq!(
            conn.execute(
                "UPDATE installed_mod SET status = 'disabled' WHERE id = ?1",
                [mod_id],
            )
            .unwrap(),
            1
        );
        stage_synthetic_payloads(&conn, &staging_root, mod_id, &[&b"mod version"[..]]);
        assert_eq!(
            std::fs::read(game_root.join("shared.dll")).unwrap(),
            b"original"
        );

        enable(&conn, mod_id, &staging_root).unwrap();
        assert_eq!(
            std::fs::read(game_root.join("shared.dll")).unwrap(),
            b"mod version"
        );
        assert_eq!(std::fs::read(&backup_path).unwrap(), b"original");

        let status: String = conn
            .query_row(
                "SELECT status FROM installed_mod WHERE id = ?1",
                [mod_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "active");
    }

    #[test]
    fn disable_unavailable_for_non_active_mod() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let conn = crate::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status) \
             VALUES ('X', 'asi', ?1, 'disabled')",
            rusqlite::params![game_root
                .to_str()
                .expect("synthetic game root must be Unicode")],
        )
        .unwrap();
        let mod_id = conn.last_insert_rowid();
        let staging = dir.path().join("staging");
        let changes_before: i64 = conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        let err = disable(&conn, mod_id, &staging).unwrap_err();
        assert!(matches!(err, CoreError::ModDisablingUnavailable));
        assert_eq!(mod_status(&conn, mod_id).unwrap(), "disabled");
        assert!(!staging.exists());
        let changes_after: i64 = conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        assert_eq!(changes_after, changes_before);
    }

    #[test]
    fn cannot_enable_a_mod_that_is_not_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let conn = crate::db::open_in_memory().unwrap();
        let mod_id = setup_active_mod(&conn, &game_root, "mod.asi", None);
        let err = enable(&conn, mod_id, &dir.path().join("staging")).unwrap_err();
        assert!(matches!(
            &err,
            CoreError::UnsupportedFormat { reason }
                if reason == &format!("mod {mod_id} is not disabled (status: active); cannot enable")
        ));
        assert!(matches!(err, CoreError::UnsupportedFormat { .. }));
    }

    // OpenAI-Codex; OpenAI-Codex_Responsible: Source_Code
    struct EnableSafetyFixture {
        conn: Connection,
        game_root: PathBuf,
        staging_root: PathBuf,
        backup_path: PathBuf,
        mod_id: i64,
        temp: tempfile::TempDir,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct EnableSafetySnapshot {
        status: String,
        files: Vec<(i64, String, Option<String>, String)>,
        events: Vec<(i64, String, String, i64, Option<String>)>,
    }

    fn enable_safety_snapshot(conn: &Connection, mod_id: i64) -> EnableSafetySnapshot {
        let files = {
            let mut statement = conn
                .prepare(
                    "SELECT id, target_path, backup_path, file_hash FROM installed_mod_file \
                     WHERE installed_mod_id = ?1 ORDER BY id",
                )
                .unwrap();
            let rows = statement
                .query_map([mod_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })
                .unwrap();
            rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
        };
        let events = {
            let mut statement = conn
                .prepare(
                    "SELECT id, event_type, timestamp, success, error_message FROM install_event \
                     WHERE installed_mod_id = ?1 ORDER BY id",
                )
                .unwrap();
            let rows = statement
                .query_map([mod_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                })
                .unwrap();
            rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
        };
        EnableSafetySnapshot {
            status: mod_status(conn, mod_id).unwrap(),
            files,
            events,
        }
    }

    fn enable_safety_fixture(with_backup: bool) -> EnableSafetyFixture {
        use sha2::Digest;

        let temp = tempfile::tempdir().unwrap();
        let game_root = temp.path().join("game");
        std::fs::create_dir(&game_root).unwrap();
        let alpha = game_root.join("alpha.asi");
        let bravo = game_root.join("bravo.asi");
        let backup_path = temp.path().join("original-alpha.bin");
        std::fs::write(&backup_path, b"original alpha").unwrap();
        if with_backup {
            std::fs::write(&alpha, b"original alpha").unwrap();
        }

        let conn = crate::db::open_in_memory().unwrap();
        let backup = if with_backup {
            Some(backup_path.as_path())
        } else {
            None
        };
        let mod_id = setup_active_mod(&conn, &game_root, "alpha.asi", backup);
        conn.execute(
            "UPDATE installed_mod SET mode = 'legacy-sp', status = 'disabled' WHERE id = ?1",
            [mod_id],
        )
        .unwrap();
        let original_alpha_hash = format!("{:x}", sha2::Sha256::digest(b"installed alpha"));
        conn.execute(
            "UPDATE installed_mod_file SET file_hash = ?1 WHERE installed_mod_id = ?2",
            rusqlite::params![original_alpha_hash, mod_id],
        )
        .unwrap();
        let bravo_hash = format!("{:x}", sha2::Sha256::digest(b"bravo"));
        conn.execute(
            "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) \
             VALUES (?1, ?2, ?3)",
            rusqlite::params![mod_id, bravo.to_string_lossy().into_owned(), bravo_hash],
        )
        .unwrap();
        let staging_root = temp.path().join("staging");
        stage_synthetic_payloads(
            &conn,
            &staging_root,
            mod_id,
            &[&b"edited alpha"[..], &b"bravo"[..]],
        );
        conn.execute(
            "INSERT INTO install_event (installed_mod_id, event_type, success) \
             VALUES (?1, 'disable', 1)",
            [mod_id],
        )
        .unwrap();
        assert_eq!(mod_status(&conn, mod_id).unwrap(), "disabled");
        assert!(crate::providers::ensure_installed_mod_available(&conn, mod_id).is_ok());
        assert_eq!(
            disabled_payloads(&conn, mod_id, &staging_root)
                .unwrap()
                .len(),
            2
        );

        EnableSafetyFixture {
            conn,
            game_root,
            staging_root,
            backup_path,
            mod_id,
            temp,
        }
    }

    fn assert_enable_first_file_preserved(
        fixture: &EnableSafetyFixture,
        expected_target: Option<&[u8]>,
        manifest: &[u8],
    ) {
        let alpha = fixture.game_root.join("alpha.asi");
        match expected_target {
            Some(expected) => assert_eq!(std::fs::read(alpha).unwrap().as_slice(), expected),
            None => assert!(!alpha.exists()),
        }
        assert_eq!(
            std::fs::read(&fixture.backup_path).unwrap().as_slice(),
            b"original alpha"
        );
        let stage = fixture.staging_root.join(fixture.mod_id.to_string());
        assert_eq!(
            std::fs::read(stage.join("0")).unwrap().as_slice(),
            b"edited alpha"
        );
        assert_eq!(
            std::fs::read(stage.join("manifest.json"))
                .unwrap()
                .as_slice(),
            manifest
        );
    }

    // OpenAI-Codex_lml_guard_audit: Capture only synthetic test data.
    fn snapshot_owner_fixture_tree(root: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
        let mut pending = vec![root.to_path_buf()];
        let mut entries = Vec::new();
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let kind = entry.file_type().unwrap();
                if kind.is_dir() {
                    entries.push((path.clone(), None));
                    pending.push(path);
                } else {
                    assert!(kind.is_file());
                    entries.push((path.clone(), Some(std::fs::read(&path).unwrap())));
                }
            }
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
    }

    // OpenAI-Codex_lml_guard_audit: An untracked first target must not hide a later owner.
    #[test]
    fn enable_rejects_recorded_owner_after_untracked_target() {
        let fixture = enable_safety_fixture(true);
        let alpha = fixture.game_root.join("alpha.asi");
        let bravo = fixture.game_root.join("bravo.asi");
        let stage = fixture.staging_root.join(fixture.mod_id.to_string());
        let manifest = std::fs::read(stage.join("manifest.json")).unwrap();
        assert_eq!(std::fs::read(&alpha).unwrap(), b"original alpha");
        assert!(!bravo.exists());

        let other = setup_active_mod(&fixture.conn, &fixture.game_root, "bravo.asi", None);
        std::fs::write(&bravo, b"other active mod").unwrap();
        fixture
            .conn
            .execute(
                "UPDATE installed_mod SET mode='legacy-sp' WHERE id=?1",
                [other],
            )
            .unwrap();
        fixture
            .conn
            .execute(
                "UPDATE installed_mod_file SET file_hash=?1 WHERE installed_mod_id=?2",
                rusqlite::params![crate::util::hash_file(&bravo).unwrap(), other],
            )
            .unwrap();
        crate::providers::ensure_installed_mod_available(&fixture.conn, fixture.mod_id).unwrap();
        assert_eq!(
            disabled_payloads(&fixture.conn, fixture.mod_id, &fixture.staging_root)
                .unwrap()
                .len(),
            2
        );
        let targets = vec![alpha.clone(), bravo.clone()];
        let report = crate::conflict::analyze(&fixture.conn, &targets).unwrap();
        assert!(!report.has_protected_hits());
        assert_eq!(report.foreign_conflicts.len(), 2);
        assert!(report.foreign_conflicts[0].owner_mod_id.is_none());
        assert!(report.foreign_conflicts[0].path.as_path() == alpha.as_path());
        assert_eq!(report.foreign_conflicts[1].owner_mod_id, Some(other));
        assert!(report.foreign_conflicts[1].path.as_path() == bravo.as_path());

        let tree_before = snapshot_owner_fixture_tree(fixture.temp.path());
        let disabled_before = enable_safety_snapshot(&fixture.conn, fixture.mod_id);
        let other_before = enable_safety_snapshot(&fixture.conn, other);
        let changes_before: i64 = fixture
            .conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();

        match enable(&fixture.conn, fixture.mod_id, &fixture.staging_root) {
            Err(CoreError::ForeignConflict { path, owner_mod_id }) => {
                assert_eq!(owner_mod_id, other);
                assert!(
                    path.as_path() == bravo.as_path(),
                    "unexpected conflict path"
                );
            }
            _ => panic!("expected a recorded active-owner conflict"),
        }

        assert!(
            snapshot_owner_fixture_tree(fixture.temp.path()) == tree_before,
            "synthetic fixture tree changed"
        );
        assert_enable_first_file_preserved(&fixture, Some(b"original alpha"), &manifest);
        assert_eq!(std::fs::read(&bravo).unwrap(), b"other active mod");
        assert_eq!(std::fs::read(stage.join("1")).unwrap(), b"bravo");
        assert!(
            enable_safety_snapshot(&fixture.conn, fixture.mod_id) == disabled_before,
            "disabled mod records changed"
        );
        assert!(
            enable_safety_snapshot(&fixture.conn, other) == other_before,
            "other mod records changed"
        );
        let changes_after: i64 = fixture
            .conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        assert_eq!(changes_after, changes_before);
    }

    // OpenAI-Codex_lml_guard_audit: A later protected target must not move earlier files.
    #[test]
    fn enable_rejects_later_protected_target_before_moving_any_payload() {
        fn synthetic_tree(root: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
            let mut pending = vec![root.to_path_buf()];
            let mut entries = Vec::new();
            while let Some(directory) = pending.pop() {
                for entry in std::fs::read_dir(directory).unwrap() {
                    let entry = entry.unwrap();
                    let path = entry.path();
                    let kind = entry.file_type().unwrap();
                    if kind.is_dir() {
                        entries.push((path.clone(), None));
                        pending.push(path);
                    } else {
                        assert!(kind.is_file());
                        entries.push((path.clone(), Some(std::fs::read(&path).unwrap())));
                    }
                }
            }
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            entries
        }

        let fixture = enable_safety_fixture(true);
        let alpha = fixture.game_root.join("alpha.asi");
        let bravo = fixture.game_root.join("bravo.asi");
        let protected = fixture.game_root.join("blocked.exe");
        let stage = fixture.staging_root.join(fixture.mod_id.to_string());
        let manifest = std::fs::read(stage.join("manifest.json")).unwrap();
        assert_eq!(std::fs::read(&alpha).unwrap(), b"original alpha");
        assert_eq!(
            std::fs::read(&fixture.backup_path).unwrap(),
            b"original alpha"
        );
        assert!(!bravo.exists());
        assert!(!protected.exists());

        let rows: Vec<(i64, String)> = {
            let mut statement = fixture.conn.prepare(
                "SELECT id,target_path FROM installed_mod_file WHERE installed_mod_id=?1 ORDER BY id"
            ).unwrap();
            let result = statement
                .query_map([fixture.mod_id], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            result
        };
        assert_eq!(rows.len(), 2);
        assert!(Path::new(&rows[0].1) == alpha.as_path());
        assert!(Path::new(&rows[1].1) == bravo.as_path());
        assert_eq!(fixture.conn.execute(
            "UPDATE installed_mod_file SET target_path=?1 WHERE id=?2 AND installed_mod_id=?3",
            rusqlite::params![protected.to_str().unwrap(), rows[1].0, fixture.mod_id]
        ).unwrap(), 1);
        let active_count: i64 = fixture
            .conn
            .query_row(
                "SELECT COUNT(*) FROM installed_mod WHERE status='active'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(active_count, 0);
        crate::providers::ensure_installed_mod_available(&fixture.conn, fixture.mod_id).unwrap();
        let payloads =
            disabled_payloads(&fixture.conn, fixture.mod_id, &fixture.staging_root).unwrap();
        assert_eq!(payloads.len(), 2);
        assert_eq!((payloads[0].0, payloads[1].0), (rows[0].0, rows[1].0));
        assert!(payloads[0].1.as_path() == stage.join("0").as_path());
        assert!(payloads[1].1.as_path() == stage.join("1").as_path());
        assert!(protected_files::check_write(&alpha).is_ok());
        match protected_files::check_write(&protected) {
            Err(CoreError::ProtectedFileViolation { path }) => {
                assert!(
                    path.as_path() == protected.as_path(),
                    "unexpected protected path"
                );
            }
            _ => panic!("expected a protected target"),
        }
        let tree_before = synthetic_tree(fixture.temp.path());
        let db_before = enable_safety_snapshot(&fixture.conn, fixture.mod_id);
        let changes_before: i64 = fixture
            .conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();

        match enable(&fixture.conn, fixture.mod_id, &fixture.staging_root) {
            Err(CoreError::ProtectedFileViolation { path }) => {
                assert!(
                    path.as_path() == protected.as_path(),
                    "unexpected protected path"
                );
            }
            _ => panic!("expected a protected target refusal"),
        }

        assert!(
            synthetic_tree(fixture.temp.path()) == tree_before,
            "synthetic tree changed"
        );
        assert_enable_first_file_preserved(&fixture, Some(b"original alpha"), &manifest);
        assert_eq!(std::fs::read(stage.join("1")).unwrap(), b"bravo");
        assert!(!bravo.exists());
        assert!(!protected.exists());
        assert!(
            enable_safety_snapshot(&fixture.conn, fixture.mod_id) == db_before,
            "disabled mod records changed"
        );
        let changes_after: i64 = fixture
            .conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        assert_eq!(changes_after, changes_before);
    }

    // OpenAI-Codex_lml_guard_audit: A missing target can still have an active owner.
    #[test]
    fn enable_refuses_missing_target_recorded_by_another_active_mod() {
        let fixture = enable_safety_fixture(true);
        let alpha = fixture.game_root.join("alpha.asi");
        let bravo = fixture.game_root.join("bravo.asi");
        let stage = fixture.staging_root.join(fixture.mod_id.to_string());
        let manifest = std::fs::read(stage.join("manifest.json")).unwrap();
        assert_eq!(std::fs::read(&alpha).unwrap(), b"original alpha");
        assert_eq!(
            std::fs::read(&fixture.backup_path).unwrap(),
            b"original alpha"
        );
        assert!(matches!(std::fs::symlink_metadata(&bravo),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound));

        let other = setup_active_mod(&fixture.conn, &fixture.game_root, "bravo.asi", None);
        fixture
            .conn
            .execute(
                "UPDATE installed_mod SET mode='legacy-sp' WHERE id=?1",
                [other],
            )
            .unwrap();
        // This is a recorded hash only; the missing target's content cannot be verified.
        let recorded_hash = crate::util::hash_file(&stage.join("1")).unwrap();
        fixture
            .conn
            .execute(
                "UPDATE installed_mod_file SET file_hash=?1 WHERE installed_mod_id=?2",
                rusqlite::params![recorded_hash, other],
            )
            .unwrap();
        for id in [fixture.mod_id, other] {
            crate::providers::ensure_installed_mod_available(&fixture.conn, id).unwrap();
        }
        assert_eq!(
            mod_status(&fixture.conn, fixture.mod_id).unwrap(),
            "disabled"
        );
        assert_eq!(mod_status(&fixture.conn, other).unwrap(), "active");
        assert_eq!(
            disabled_payloads(&fixture.conn, fixture.mod_id, &fixture.staging_root)
                .unwrap()
                .len(),
            2
        );
        assert!(matches!(std::fs::symlink_metadata(&bravo),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound));
        let report =
            crate::conflict::analyze(&fixture.conn, &[alpha.clone(), bravo.clone()]).unwrap();
        assert!(!report.has_protected_hits());
        assert_eq!(report.foreign_conflicts.len(), 2);
        assert!(report
            .foreign_conflicts
            .iter()
            .any(|conflict| conflict.owner_mod_id.is_none()
                && conflict.path.as_path() == alpha.as_path()));
        assert!(report
            .foreign_conflicts
            .iter()
            .any(|conflict| conflict.owner_mod_id == Some(other)
                && conflict.path.as_path() == bravo.as_path()));

        let tree_before = snapshot_owner_fixture_tree(fixture.temp.path());
        let disabled_before = enable_safety_snapshot(&fixture.conn, fixture.mod_id);
        let other_before = enable_safety_snapshot(&fixture.conn, other);
        let changes_before: i64 = fixture
            .conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();

        match enable(&fixture.conn, fixture.mod_id, &fixture.staging_root) {
            Err(CoreError::ForeignConflict { path, owner_mod_id }) => {
                assert_eq!(owner_mod_id, other);
                assert!(
                    path.as_path() == bravo.as_path(),
                    "unexpected conflict path"
                );
            }
            _ => panic!("expected a recorded active-owner conflict"),
        }

        assert!(
            snapshot_owner_fixture_tree(fixture.temp.path()) == tree_before,
            "synthetic fixture tree changed"
        );
        assert_enable_first_file_preserved(&fixture, Some(b"original alpha"), &manifest);
        assert_eq!(std::fs::read(stage.join("1")).unwrap(), b"bravo");
        assert!(matches!(std::fs::symlink_metadata(&bravo),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound));
        assert!(
            enable_safety_snapshot(&fixture.conn, fixture.mod_id) == disabled_before,
            "disabled mod records changed"
        );
        assert!(
            enable_safety_snapshot(&fixture.conn, other) == other_before,
            "other mod records changed"
        );
        let changes_after: i64 = fixture
            .conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        assert_eq!(changes_after, changes_before);
    }

    // OpenAI-Codex_db_prepare_review: Check the restored-original enable case.
    #[test]
    fn enable_accepts_restored_original_without_active_owner() {
        let fixture = enable_safety_fixture(true);
        let alpha = fixture.game_root.join("alpha.asi");
        let bravo = fixture.game_root.join("bravo.asi");
        let stage = fixture.staging_root.join(fixture.mod_id.to_string());
        let original = std::fs::read(&fixture.backup_path).unwrap();
        assert_eq!(original.as_slice(), b"original alpha");
        assert_eq!(std::fs::read(&alpha).unwrap(), original);
        assert!(!bravo.exists());
        crate::providers::ensure_installed_mod_available(&fixture.conn, fixture.mod_id).unwrap();
        let payloads =
            disabled_payloads(&fixture.conn, fixture.mod_id, &fixture.staging_root).unwrap();
        assert_eq!(payloads.len(), 2);
        assert_eq!(
            std::fs::read(stage.join("0")).unwrap().as_slice(),
            b"edited alpha"
        );
        assert_eq!(std::fs::read(stage.join("1")).unwrap().as_slice(), b"bravo");

        let report =
            crate::conflict::analyze(&fixture.conn, &[alpha.clone(), bravo.clone()]).unwrap();
        assert!(!report.has_protected_hits());
        assert_eq!(report.foreign_conflicts.len(), 1);
        assert!(report.foreign_conflicts[0].owner_mod_id.is_none());
        assert_eq!(report.foreign_conflicts[0].path.as_path(), alpha.as_path());
        let before = enable_safety_snapshot(&fixture.conn, fixture.mod_id);
        assert_eq!(before.status, "disabled");
        assert_eq!(before.files.len(), 2);
        assert_eq!(Path::new(&before.files[0].1), alpha.as_path());
        let backup = fixture
            .backup_path
            .to_str()
            .expect("synthetic backup must be Unicode");
        assert_eq!(before.files[0].2.as_deref(), Some(backup));

        enable(&fixture.conn, fixture.mod_id, &fixture.staging_root).unwrap();

        assert_eq!(std::fs::read(&alpha).unwrap().as_slice(), b"edited alpha");
        assert_eq!(std::fs::read(&bravo).unwrap().as_slice(), b"bravo");
        assert_eq!(std::fs::read(&fixture.backup_path).unwrap(), original);
        for name in ["0", "1"] {
            assert!(matches!(
                std::fs::symlink_metadata(stage.join(name)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound
            ));
        }
        let after = enable_safety_snapshot(&fixture.conn, fixture.mod_id);
        assert_eq!(after.status, "active");
        assert_eq!(after.files, before.files);
        assert_eq!(after.events.len(), before.events.len() + 1);
        assert_eq!(
            &after.events[..before.events.len()],
            before.events.as_slice()
        );
        let event = after.events.last().unwrap();
        assert_eq!(event.1.as_str(), "enable");
        assert_eq!(event.3, 1);
        assert!(event.4.is_none());
    }

    #[test]
    fn enable_missing_later_payload_preserves_targets_and_database() {
        let fixture = enable_safety_fixture(true);
        let stage = fixture.staging_root.join(fixture.mod_id.to_string());
        let saved = fixture.temp.path().join("saved-bravo.bin");
        let manifest = std::fs::read(stage.join("manifest.json")).unwrap();
        std::fs::rename(stage.join("1"), &saved).unwrap();
        assert!(
            crate::providers::ensure_installed_mod_available(&fixture.conn, fixture.mod_id).is_ok()
        );
        assert!(disabled_payloads(&fixture.conn, fixture.mod_id, &fixture.staging_root).is_err());
        let before = enable_safety_snapshot(&fixture.conn, fixture.mod_id);

        let result = enable(&fixture.conn, fixture.mod_id, &fixture.staging_root);

        assert!(result.is_err());
        assert!(matches!(
            &result,
            Err(CoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound
        ));
        assert_enable_first_file_preserved(&fixture, Some(b"original alpha"), &manifest);
        assert!(!fixture.game_root.join("bravo.asi").exists());
        assert!(!stage.join("1").exists());
        assert_eq!(std::fs::read(saved).unwrap().as_slice(), b"bravo");
        assert_eq!(
            enable_safety_snapshot(&fixture.conn, fixture.mod_id),
            before
        );
    }

    #[test]
    fn enable_corrupt_later_payload_preserves_targets_and_database() {
        let fixture = enable_safety_fixture(true);
        let stage = fixture.staging_root.join(fixture.mod_id.to_string());
        let manifest = std::fs::read(stage.join("manifest.json")).unwrap();
        std::fs::write(stage.join("1"), b"tampered bravo").unwrap();
        assert!(
            crate::providers::ensure_installed_mod_available(&fixture.conn, fixture.mod_id).is_ok()
        );
        assert!(disabled_payloads(&fixture.conn, fixture.mod_id, &fixture.staging_root).is_err());
        let before = enable_safety_snapshot(&fixture.conn, fixture.mod_id);

        let result = enable(&fixture.conn, fixture.mod_id, &fixture.staging_root);

        assert!(result.is_err());
        assert!(matches!(
            &result,
            Err(CoreError::UnsupportedFormat { reason })
                if reason == &format!("disabled staging for mod {} is missing, unsafe, or does not match its file records; preserve it for recovery", fixture.mod_id)
        ));
        assert_enable_first_file_preserved(&fixture, Some(b"original alpha"), &manifest);
        assert!(!fixture.game_root.join("bravo.asi").exists());
        assert_eq!(
            std::fs::read(stage.join("1")).unwrap().as_slice(),
            b"tampered bravo"
        );
        assert_eq!(
            enable_safety_snapshot(&fixture.conn, fixture.mod_id),
            before
        );
    }

    #[test]
    fn enable_other_active_owner_preserves_both_mods() {
        let fixture = enable_safety_fixture(false);
        let stage = fixture.staging_root.join(fixture.mod_id.to_string());
        let target = fixture.game_root.join("bravo.asi");
        let other = setup_active_mod(&fixture.conn, &fixture.game_root, "bravo.asi", None);
        fixture
            .conn
            .execute(
                "UPDATE installed_mod SET mode = 'legacy-sp' WHERE id = ?1",
                [other],
            )
            .unwrap();
        std::fs::write(&target, b"other mod").unwrap();
        fixture
            .conn
            .execute(
                "UPDATE installed_mod_file SET file_hash = ?1 WHERE installed_mod_id = ?2",
                rusqlite::params![crate::util::hash_file(&target).unwrap(), other],
            )
            .unwrap();
        assert!(
            crate::providers::ensure_installed_mod_available(&fixture.conn, fixture.mod_id).is_ok()
        );
        assert_eq!(
            disabled_payloads(&fixture.conn, fixture.mod_id, &fixture.staging_root)
                .unwrap()
                .len(),
            2
        );
        let report =
            crate::conflict::analyze(&fixture.conn, std::slice::from_ref(&target)).unwrap();
        assert!(report
            .foreign_conflicts
            .iter()
            .any(|conflict| conflict.owner_mod_id == Some(other)));
        let manifest = std::fs::read(stage.join("manifest.json")).unwrap();
        let before = enable_safety_snapshot(&fixture.conn, fixture.mod_id);
        let other_before = enable_safety_snapshot(&fixture.conn, other);

        let result = enable(&fixture.conn, fixture.mod_id, &fixture.staging_root);

        assert!(result.is_err());
        assert_enable_first_file_preserved(&fixture, None, &manifest);
        assert_eq!(std::fs::read(&target).unwrap().as_slice(), b"other mod");
        assert_eq!(std::fs::read(stage.join("1")).unwrap().as_slice(), b"bravo");
        assert_eq!(
            enable_safety_snapshot(&fixture.conn, fixture.mod_id),
            before
        );
        assert_eq!(enable_safety_snapshot(&fixture.conn, other), other_before);
    }

    // OpenAI-Codex; OpenAI-Codex_Responsible: Source_Code
    #[test]
    fn enable_missing_or_corrupt_later_payload_refuses_with_clear_targets() {
        for corrupt in [false, true] {
            let fixture = enable_safety_fixture(false);
            let stage = fixture.staging_root.join(fixture.mod_id.to_string());
            let saved = fixture.temp.path().join("saved-bravo.bin");
            let manifest = std::fs::read(stage.join("manifest.json")).unwrap();
            std::fs::rename(stage.join("1"), &saved).unwrap();
            if corrupt {
                std::fs::write(stage.join("1"), b"tampered bravo").unwrap();
            }
            let targets = vec![
                fixture.game_root.join("alpha.asi"),
                fixture.game_root.join("bravo.asi"),
            ];
            let report = crate::conflict::analyze(&fixture.conn, &targets).unwrap();
            assert!(!report.has_protected_hits());
            assert!(!report.requires_explicit_override());
            assert!(crate::providers::ensure_installed_mod_available(
                &fixture.conn,
                fixture.mod_id
            )
            .is_ok());
            assert!(
                disabled_payloads(&fixture.conn, fixture.mod_id, &fixture.staging_root).is_err()
            );
            let before = enable_safety_snapshot(&fixture.conn, fixture.mod_id);

            let result = enable(&fixture.conn, fixture.mod_id, &fixture.staging_root);

            assert!(result.is_err(), "corrupt={corrupt}");
            if corrupt {
                assert!(matches!(
                    &result,
                    Err(CoreError::UnsupportedFormat { reason })
                        if reason == &format!("disabled staging for mod {} is missing, unsafe, or does not match its file records; preserve it for recovery", fixture.mod_id)
                ));
            } else {
                assert!(matches!(
                    &result,
                    Err(CoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound
                ));
            }
            assert_enable_first_file_preserved(&fixture, None, &manifest);
            assert!(!fixture.game_root.join("bravo.asi").exists());
            if corrupt {
                assert_eq!(
                    std::fs::read(stage.join("1")).unwrap().as_slice(),
                    b"tampered bravo"
                );
            } else {
                assert!(!stage.join("1").exists());
            }
            assert_eq!(std::fs::read(saved).unwrap().as_slice(), b"bravo");
            assert_eq!(
                enable_safety_snapshot(&fixture.conn, fixture.mod_id),
                before
            );
        }
    }

    #[test]
    fn enable_valid_two_file_staging_restores_user_edits() {
        for with_backup in [false, true] {
            let fixture = enable_safety_fixture(with_backup);
            let stage = fixture.staging_root.join(fixture.mod_id.to_string());
            let before = enable_safety_snapshot(&fixture.conn, fixture.mod_id);
            assert_eq!(
                disabled_payloads(&fixture.conn, fixture.mod_id, &fixture.staging_root)
                    .unwrap()
                    .len(),
                2
            );

            enable(&fixture.conn, fixture.mod_id, &fixture.staging_root).unwrap();

            assert_eq!(
                std::fs::read(fixture.game_root.join("alpha.asi"))
                    .unwrap()
                    .as_slice(),
                b"edited alpha"
            );
            assert_eq!(
                std::fs::read(fixture.game_root.join("bravo.asi"))
                    .unwrap()
                    .as_slice(),
                b"bravo"
            );
            assert_eq!(
                std::fs::read(&fixture.backup_path).unwrap().as_slice(),
                b"original alpha"
            );
            let after = enable_safety_snapshot(&fixture.conn, fixture.mod_id);
            assert_eq!(after.status, "active");
            assert_eq!(after.files, before.files);
            assert_eq!(after.events.len(), before.events.len() + 1);
            assert_eq!(after.events.last().unwrap().1.as_str(), "enable");
            if stage.join("manifest.json").exists() {
                assert!(stage.join("manifest.json").is_file());
            }
        }
    }

    // OpenAI-Codex_db_prepare_review: Legacy snapshots have no manifest.
    struct LegacyEnableFixture {
        _temp: tempfile::TempDir,
        conn: Connection,
        game_root: PathBuf,
        staging_root: PathBuf,
        stage_dir: PathBuf,
        staged: PathBuf,
        target: PathBuf,
        backup: PathBuf,
        prior_payload: PathBuf,
        mod_id: i64,
        file_id: i64,
    }

    fn legacy_enable_fixture(matching_hash: bool) -> LegacyEnableFixture {
        let temp = tempfile::tempdir().unwrap();
        let game_root = temp.path().join("game");
        std::fs::create_dir(&game_root).unwrap();
        let staging_root = temp.path().join("staging");
        let target = game_root.join("legacy.asi");
        let backup = temp.path().join("original.bak");
        let prior_payload = temp.path().join("prior-payload.bin");
        std::fs::write(&target, b"restored original").unwrap();
        std::fs::write(&backup, b"restored original").unwrap();
        std::fs::write(&prior_payload, b"payload before user edit").unwrap();

        let conn = crate::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status, mode) \
             VALUES ('Legacy fixture', 'asi', ?1, 'disabled', 'legacy-sp')",
            [game_root
                .to_str()
                .expect("synthetic game root must be Unicode")],
        )
        .unwrap();
        let mod_id = conn.last_insert_rowid();
        let stage_dir = staging_root.join(mod_id.to_string());
        std::fs::create_dir_all(&stage_dir).unwrap();
        let staged = stage_dir.join("0");
        std::fs::write(&staged, b"user-edited staged payload").unwrap();
        let staged_hash = crate::util::hash_file(&staged).unwrap();
        let prior_hash = crate::util::hash_file(&prior_payload).unwrap();
        assert_ne!(staged_hash, prior_hash);
        let recorded_hash = if matching_hash {
            &staged_hash
        } else {
            &prior_hash
        };
        conn.execute(
            "INSERT INTO installed_mod_file \
             (installed_mod_id, target_path, backup_path, file_hash) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                mod_id,
                target.to_str().expect("synthetic target must be Unicode"),
                backup.to_str().expect("synthetic backup must be Unicode"),
                recorded_hash,
            ],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO install_event (installed_mod_id, event_type, success) \
             VALUES (?1, 'disable', 1)",
            [mod_id],
        )
        .unwrap();

        assert!(game_root.is_absolute());
        assert!(game_root.is_dir());
        assert!(crate::providers::ensure_installed_mod_available(&conn, mod_id).is_ok());
        assert!(protected_files::check_write(&target).is_ok());
        assert!(matches!(
            std::fs::symlink_metadata(stage_dir.join("manifest.json")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        ));
        LegacyEnableFixture {
            _temp: temp,
            conn,
            game_root,
            staging_root,
            stage_dir,
            staged,
            target,
            backup,
            prior_payload,
            mod_id,
            file_id,
        }
    }

    fn legacy_stage_entries(stage_dir: &Path) -> Vec<std::ffi::OsString> {
        let mut names: Vec<_> = std::fs::read_dir(stage_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn enable_legacy_staging_with_mismatched_hash_preserves_files_and_database() {
        let fixture = legacy_enable_fixture(false);
        let expected_reason = format!(
            "disabled staging for mod {} is missing, unsafe, or does not match its file records; preserve it for recovery",
            fixture.mod_id
        );
        let preflight_error =
            disabled_payloads(&fixture.conn, fixture.mod_id, &fixture.staging_root).unwrap_err();
        assert!(matches!(
            preflight_error,
            CoreError::UnsupportedFormat { reason } if reason == expected_reason
        ));
        let db_before = enable_safety_snapshot(&fixture.conn, fixture.mod_id);
        let changes_before: i64 = fixture
            .conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        let entries_before = legacy_stage_entries(&fixture.stage_dir);

        let error = enable(&fixture.conn, fixture.mod_id, &fixture.staging_root).unwrap_err();

        assert!(matches!(
            error,
            CoreError::UnsupportedFormat { reason } if reason == expected_reason
        ));
        assert_eq!(
            std::fs::read(&fixture.target).unwrap(),
            b"restored original"
        );
        assert_eq!(
            std::fs::read(&fixture.backup).unwrap(),
            b"restored original"
        );
        assert_eq!(
            std::fs::read(&fixture.staged).unwrap(),
            b"user-edited staged payload"
        );
        assert_eq!(
            std::fs::read(&fixture.prior_payload).unwrap(),
            b"payload before user edit"
        );
        assert_eq!(legacy_stage_entries(&fixture.stage_dir), entries_before);
        assert!(matches!(
            std::fs::symlink_metadata(fixture.stage_dir.join("manifest.json")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        ));
        assert_eq!(
            enable_safety_snapshot(&fixture.conn, fixture.mod_id),
            db_before
        );
        let changes_after: i64 = fixture
            .conn
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        assert_eq!(changes_after, changes_before);
    }

    #[test]
    fn enable_legacy_staging_with_matching_hash_restores_payload() {
        let fixture = legacy_enable_fixture(true);
        assert!(fixture.game_root.is_dir());
        assert_eq!(
            disabled_payloads(&fixture.conn, fixture.mod_id, &fixture.staging_root).unwrap(),
            vec![(fixture.file_id, fixture.staged.clone())]
        );
        let db_before = enable_safety_snapshot(&fixture.conn, fixture.mod_id);

        enable(&fixture.conn, fixture.mod_id, &fixture.staging_root).unwrap();

        assert_eq!(
            std::fs::read(&fixture.target).unwrap(),
            b"user-edited staged payload"
        );
        assert!(!fixture.staged.exists());
        assert_eq!(
            std::fs::read(&fixture.backup).unwrap(),
            b"restored original"
        );
        assert_eq!(
            std::fs::read(&fixture.prior_payload).unwrap(),
            b"payload before user edit"
        );
        let db_after = enable_safety_snapshot(&fixture.conn, fixture.mod_id);
        assert_eq!(db_after.status, "active");
        assert_eq!(db_after.files, db_before.files);
        assert_eq!(db_after.events.len(), db_before.events.len() + 1);
        assert_eq!(
            &db_after.events[..db_before.events.len()],
            db_before.events.as_slice()
        );
        assert_eq!(db_after.events.last().unwrap().1.as_str(), "enable");
        assert_eq!(db_after.events.last().unwrap().3, 1);
    }
}
