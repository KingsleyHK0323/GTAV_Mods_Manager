// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex
// OpenAI-Codex_Responsible: Source_Code

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

/// Deactivates an active mod: its deployed files move to staging, and anything they
/// overwrote is restored from backup so the game runs unmodified while disabled.
pub fn disable(conn: &Connection, mod_id: i64, staging_root: &Path) -> CoreResult<()> {
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

    let files = load_mod_files(conn, mod_id)?;
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
             VALUES ('TestMod', 'asi', '', 'active')",
            [],
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
    fn disable_moves_file_to_staging_and_updates_status() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        std::fs::write(game_root.join("mod.asi"), b"payload").unwrap();
        let staging_root = dir.path().join("staging");

        let conn = crate::db::open_in_memory().unwrap();
        let mod_id = setup_active_mod(&conn, &game_root, "mod.asi", None);

        disable(&conn, mod_id, &staging_root).unwrap();

        assert!(!game_root.join("mod.asi").exists());
        let status: String = conn
            .query_row(
                "SELECT status FROM installed_mod WHERE id = ?1",
                [mod_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "disabled");
    }

    #[test]
    fn disable_restores_backed_up_original_then_enable_reapplies_mod_file() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let backup_path = dir.path().join("backup.dll");
        std::fs::write(&backup_path, b"original").unwrap();
        std::fs::write(game_root.join("shared.dll"), b"mod version").unwrap();
        let staging_root = dir.path().join("staging");

        let conn = crate::db::open_in_memory().unwrap();
        let mod_id = setup_active_mod(&conn, &game_root, "shared.dll", Some(&backup_path));

        disable(&conn, mod_id, &staging_root).unwrap();
        assert_eq!(
            std::fs::read(game_root.join("shared.dll")).unwrap(),
            b"original"
        );

        enable(&conn, mod_id, &staging_root).unwrap();
        assert_eq!(
            std::fs::read(game_root.join("shared.dll")).unwrap(),
            b"mod version"
        );

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
    fn cannot_disable_a_mod_that_is_not_active() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status) \
             VALUES ('X', 'asi', '', 'disabled')",
            [],
        )
        .unwrap();
        let mod_id = conn.last_insert_rowid();
        let err = disable(&conn, mod_id, &dir.path().join("staging")).unwrap_err();
        assert!(matches!(err, CoreError::UnsupportedFormat { .. }));
    }

    #[test]
    fn cannot_enable_a_mod_that_is_not_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let conn = crate::db::open_in_memory().unwrap();
        let mod_id = setup_active_mod(&conn, &game_root, "mod.asi", None);
        let err = enable(&conn, mod_id, &dir.path().join("staging")).unwrap_err();
        assert!(matches!(err, CoreError::UnsupportedFormat { .. }));
    }
}
