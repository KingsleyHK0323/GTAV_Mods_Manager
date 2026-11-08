// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex
// OpenAI-Codex_Responsible: Source_Code
// OpenAI-Codex_index_review

//! Multi-profile support: a Profile is a named set of mods that should be active
//! together (e.g. a "Roleplay" profile vs. a "Graphics Showcase" profile on the same
//! game install). Switching profiles disables the mods other profiles want active
//! that this profile doesn't, then enables this profile's mods that are currently
//! disabled — reusing the existing `state::enable`/`disable` machinery unchanged, not
//! a parallel deploy mechanism.
//!
//! Profile membership is **opt-in per mod** (via [`add_mod`]): a mod never assigned
//! to any profile is left untouched by [`switch`]. This keeps the feature additive
//! on top of the MVP's existing behavior rather than forcing every installed mod
//! into a profile up front.

use std::collections::HashSet;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::CoreResult;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Profile {
    pub id: i64,
    pub name: String,
    pub created_at: String,
    pub is_active: bool,
}

pub fn create(conn: &Connection, name: &str) -> CoreResult<i64> {
    conn.execute("INSERT INTO profile (name) VALUES (?1)", [name])?;
    Ok(conn.last_insert_rowid())
}

/// Deletes a profile (does not uninstall or otherwise touch its mods). If `profile_id`
/// is the currently active profile, clears `active_profile_id` first — otherwise the
/// delete would fail its foreign key constraint (a real bug found via testing: `switch`
/// followed by `delete` on that same profile used to error with an opaque "database
/// error" instead of either succeeding or failing clearly).
pub fn delete(conn: &Connection, profile_id: i64) -> CoreResult<()> {
    conn.execute(
        "UPDATE user_settings SET active_profile_id = NULL \
         WHERE id = 1 AND active_profile_id = ?1",
        [profile_id],
    )?;
    conn.execute("DELETE FROM profile WHERE id = ?1", [profile_id])?;
    Ok(())
}

fn active_profile_id(conn: &Connection) -> CoreResult<Option<i64>> {
    conn.query_row(
        "SELECT active_profile_id FROM user_settings WHERE id = 1",
        [],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

pub fn list(conn: &Connection) -> CoreResult<Vec<Profile>> {
    let active_id = active_profile_id(conn)?;
    let mut stmt = conn.prepare("SELECT id, name, created_at FROM profile ORDER BY id")?;
    let rows = stmt.query_map([], |row| {
        let id: i64 = row.get(0)?;
        Ok(Profile {
            id,
            name: row.get(1)?,
            created_at: row.get(2)?,
            is_active: Some(id) == active_id,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub fn add_mod(conn: &Connection, profile_id: i64, installed_mod_id: i64) -> CoreResult<()> {
    conn.execute(
        "INSERT OR IGNORE INTO profile_mod (profile_id, installed_mod_id) VALUES (?1, ?2)",
        [profile_id, installed_mod_id],
    )?;
    Ok(())
}

pub fn remove_mod(conn: &Connection, profile_id: i64, installed_mod_id: i64) -> CoreResult<()> {
    conn.execute(
        "DELETE FROM profile_mod WHERE profile_id = ?1 AND installed_mod_id = ?2",
        [profile_id, installed_mod_id],
    )?;
    Ok(())
}

/// IDs of every mod currently assigned to `profile_id`, in no particular order —
/// exposed so a UI can render membership (e.g. checkboxes) without needing its own
/// query against `profile_mod`.
pub fn mod_ids_in_profile(conn: &Connection, profile_id: i64) -> CoreResult<Vec<i64>> {
    let mut stmt =
        conn.prepare("SELECT installed_mod_id FROM profile_mod WHERE profile_id = ?1")?;
    let rows = stmt.query_map([profile_id], |row| row.get(0))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

// OpenAI-Codex_fresh_guard_review: Capture the observed database state for a switch.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ProfileSwitchSnapshot {
    exists: bool,
    active_profile: Option<i64>,
    memberships: Vec<(i64, i64)>,
    mods: Vec<(i64, String, String, Option<String>)>,
    files: Vec<(i64, i64, String, Option<String>, String)>,
}

fn profile_switch_snapshot(
    conn: &Connection,
    profile_id: i64,
) -> CoreResult<ProfileSwitchSnapshot> {
    if !conn.is_autocommit() {
        return profile_switch_snapshot_in_transaction(conn, profile_id);
    }
    let tx = conn.unchecked_transaction()?;
    let snapshot = profile_switch_snapshot_in_transaction(&tx, profile_id)?;
    tx.commit()?;
    Ok(snapshot)
}

fn profile_switch_snapshot_in_transaction(
    conn: &Connection,
    profile_id: i64,
) -> CoreResult<ProfileSwitchSnapshot> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM profile WHERE id=?1)",
        [profile_id],
        |r| r.get(0),
    )?;
    let active_profile: Option<i64> = conn.query_row(
        "SELECT active_profile_id FROM user_settings WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    let memberships = {
        let mut stmt = conn.prepare(
            "SELECT profile_id,installed_mod_id FROM profile_mod ORDER BY profile_id,installed_mod_id"
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<(i64, i64)>>>()?
    };
    let mods = {
        let mut stmt =
            conn.prepare("SELECT id,status,install_path,mode FROM installed_mod ORDER BY id")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        rows.collect::<rusqlite::Result<Vec<(i64, String, String, Option<String>)>>>()?
    };
    let files = {
        let mut stmt = conn.prepare(
            "SELECT id,installed_mod_id,target_path,backup_path,file_hash \
             FROM installed_mod_file ORDER BY installed_mod_id,id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<(i64, i64, String, Option<String>, String)>>>()?
    };
    Ok(ProfileSwitchSnapshot {
        exists,
        active_profile,
        memberships,
        mods,
        files,
    })
}

fn profile_switch_changed() -> crate::error::CoreError {
    crate::error::CoreError::UnsupportedFormat {
        reason: "cannot verify profile switch state; mod files or statuses may already have changed; review the current state before taking further action".to_string(),
    }
}

fn profile_switch_require_snapshot(
    conn: &Connection,
    profile_id: i64,
    expected: &ProfileSwitchSnapshot,
) -> CoreResult<()> {
    if profile_switch_snapshot(conn, profile_id)? != *expected {
        return Err(profile_switch_changed());
    }
    Ok(())
}

impl ProfileSwitchSnapshot {
    fn status(&self, id: i64) -> Option<&str> {
        self.mods
            .iter()
            .find(|row| row.0 == id)
            .map(|row| row.1.as_str())
    }
    fn targets(&self, profile_id: i64) -> HashSet<i64> {
        self.memberships
            .iter()
            .filter(|row| row.0 == profile_id)
            .map(|row| row.1)
            .collect()
    }
    fn other_active(&self, profile_id: i64) -> HashSet<i64> {
        self.memberships
            .iter()
            .filter(|row| row.0 != profile_id && self.status(row.1) == Some("active"))
            .map(|row| row.1)
            .collect()
    }
    fn transition(&mut self, id: i64, from: &str, to: &str) -> CoreResult<()> {
        let row = self
            .mods
            .iter_mut()
            .find(|row| row.0 == id)
            .ok_or_else(profile_switch_changed)?;
        if row.1 != from {
            return Err(profile_switch_changed());
        }
        row.1 = to.to_owned();
        Ok(())
    }
}

// OpenAI-Codex_fresh_guard_review: Refuse unverifiable profile ownership changes.
struct ProfileGuardTarget {
    owner: i64,
    path: std::path::PathBuf,
    identity: String,
}

fn profile_owner_preflight(snapshot: &ProfileSwitchSnapshot, profile_id: i64) -> CoreResult<()> {
    use crate::error::CoreError;
    let targets = snapshot.targets(profile_id);
    let other_active = snapshot.other_active(profile_id);
    let outgoing: HashSet<i64> = other_active.difference(&targets).copied().collect();
    let incoming: HashSet<i64> = targets
        .iter()
        .copied()
        .filter(|id| snapshot.status(*id) == Some("disabled"))
        .collect();
    if outgoing.is_empty() && incoming.is_empty() {
        return Ok(());
    }

    // OpenAI-Codex_fresh_guard_review: Refuse active mods with unknown targets.
    for row in &snapshot.mods {
        if row.1.as_str() == "active" && !snapshot.files.iter().any(|file| file.1 == row.0) {
            return Err(CoreError::UnsupportedFormat {
                reason: format!(
                    "profile switch cannot determine targets for active mod {}: no file records",
                    row.0
                ),
            });
        }
    }
    for row in &snapshot.files {
        if outgoing.contains(&row.1) || incoming.contains(&row.1) {
            crate::protected_files::check_write(Path::new(&row.2))?;
        }
    }
    for row in &snapshot.files {
        if !outgoing.contains(&row.1) && !incoming.contains(&row.1) {
            continue;
        }
        match std::fs::symlink_metadata(&row.2) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => {
                return Err(CoreError::UnsupportedFormat {
                    reason: format!("profile target is not a regular file: {}", row.2),
                })
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && outgoing.contains(&row.1) =>
            {
                return Err(CoreError::UnsupportedFormat {
                    reason: format!(
                        "missing target for profile outgoing mod #{} file #{}: {}",
                        row.1, row.0, row.2
                    ),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }

    let mut active_paths = Vec::new();
    let mut incoming_paths = Vec::new();
    for row in &snapshot.files {
        let status = snapshot
            .status(row.1)
            .ok_or_else(|| CoreError::UnsupportedFormat {
                reason: format!(
                    "cannot identify owner mod #{} of recorded file #{}",
                    row.1, row.0
                ),
            })?;
        if !matches!(status, "active" | "disabled" | "uninstalled") {
            return Err(profile_switch_changed());
        }
        let active = status == "active";
        if !active && !incoming.contains(&row.1) {
            continue;
        }
        let path = std::path::PathBuf::from(&row.2);
        let identity = crate::lml_store::backup_guard_identity(&path)?;
        let entry = ProfileGuardTarget {
            owner: row.1,
            path,
            identity,
        };
        if active {
            active_paths.push(entry);
        } else {
            incoming_paths.push(entry);
        }
    }
    // OpenAI-Codex_fresh_guard_review: Refuse overlapping targets within planned mods.
    for paths in [&active_paths[..], &incoming_paths[..]] {
        for (index, left) in paths.iter().enumerate() {
            if !outgoing.contains(&left.owner) && !incoming.contains(&left.owner) {
                continue;
            }
            for right in paths.iter().skip(index + 1) {
                if left.owner == right.owner
                    && crate::lml_store::backup_guard_overlap(&left.identity, &right.identity)
                {
                    return Err(CoreError::UnsupportedFormat {
                        reason: format!(
                            "profile mod #{} has overlapping recorded targets: {} and {}",
                            left.owner,
                            left.path.display(),
                            right.path.display()
                        ),
                    });
                }
            }
        }
    }
    for own in active_paths
        .iter()
        .filter(|entry| outgoing.contains(&entry.owner))
    {
        for other in &active_paths {
            if own.owner != other.owner
                && crate::lml_store::backup_guard_overlap(&own.identity, &other.identity)
            {
                return Err(CoreError::ForeignConflict {
                    path: other.path.clone(),
                    owner_mod_id: other.owner,
                });
            }
        }
    }
    for next in &incoming_paths {
        for owner in &active_paths {
            if crate::lml_store::backup_guard_overlap(&next.identity, &owner.identity) {
                return Err(CoreError::ForeignConflict {
                    path: owner.path.clone(),
                    owner_mod_id: owner.owner,
                });
            }
        }
    }
    for (index, left) in incoming_paths.iter().enumerate() {
        for right in incoming_paths.iter().skip(index + 1) {
            if left.owner != right.owner
                && crate::lml_store::backup_guard_overlap(&left.identity, &right.identity)
            {
                return Err(CoreError::UnsupportedFormat {
                    reason: format!(
                        "profile mods #{} and #{} have overlapping targets: {} and {}",
                        left.owner,
                        right.owner,
                        left.path.display(),
                        right.path.display()
                    ),
                });
            }
        }
    }
    Ok(())
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct SwitchOutcome {
    pub enabled: Vec<i64>,
    pub disabled: Vec<i64>,
}

/// Switches the active profile to `profile_id`. Disables any active mod that belongs
/// to a *different* profile and isn't also wanted by this one, then enables this
/// profile's mods that are currently disabled. A mod belonging to no profile, or to
/// both the outgoing and incoming profile, is left exactly as it is.
pub fn switch(
    conn: &Connection,
    profile_id: i64,
    staging_root: &Path,
) -> CoreResult<SwitchOutcome> {
    // OpenAI-Codex: Refuse a missing profile before changing active mods.
    let profile_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM profile WHERE id = ?1)",
        [profile_id],
        |row| row.get(0),
    )?;
    if !profile_exists {
        return Err(crate::error::CoreError::UnsupportedFormat {
            reason: format!("no profile with id {profile_id}"),
        });
    }

    // OpenAI-Codex_fresh_guard_review: Keep an auditable profile switch snapshot.
    let mut profile_expected = profile_switch_snapshot(conn, profile_id)?;
    if !profile_expected.exists {
        return Err(profile_switch_changed());
    }

    let target_mods: HashSet<i64> = mod_ids_in_profile(conn, profile_id)?.into_iter().collect();

    let mut stmt = conn.prepare(
        "SELECT DISTINCT pm.installed_mod_id \
         FROM profile_mod pm \
         JOIN installed_mod m ON m.id = pm.installed_mod_id \
         WHERE pm.profile_id != ?1 AND m.status = 'active'",
    )?;
    let other_active: Vec<i64> = stmt
        .query_map([profile_id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    let observed_other: HashSet<i64> = other_active.iter().copied().collect();
    if profile_expected.targets(profile_id) != target_mods
        || profile_expected.other_active(profile_id) != observed_other
    {
        return Err(profile_switch_changed());
    }

    // OpenAI-Codex: Validate the whole switch before changing the first mod.
    for mod_id in target_mods.iter().chain(other_active.iter()) {
        crate::providers::ensure_installed_mod_available(conn, *mod_id)?;
    }

    // OpenAI-Codex: Check incoming staging before disabling other mods.
    for mod_id in &target_mods {
        let status: String = conn.query_row(
            "SELECT status FROM installed_mod WHERE id = ?1",
            [*mod_id],
            |row| row.get(0),
        )?;
        if profile_expected.status(*mod_id) != Some(status.as_str()) {
            return Err(profile_switch_changed());
        }
        if status == "disabled" {
            let _ = crate::state::disabled_payloads(conn, *mod_id, staging_root)?;
        }
    }

    profile_switch_require_snapshot(conn, profile_id, &profile_expected)?;
    let profile_preflight_result = profile_owner_preflight(&profile_expected, profile_id);
    profile_switch_require_snapshot(conn, profile_id, &profile_expected)?;
    profile_preflight_result?;

    let mut outcome = SwitchOutcome::default();
    for mod_id in other_active {
        if target_mods.contains(&mod_id) {
            continue;
        }
        profile_switch_require_snapshot(conn, profile_id, &profile_expected)?;
        crate::state::disable(conn, mod_id, staging_root)?;
        profile_expected.transition(mod_id, "active", "disabled")?;
        outcome.disabled.push(mod_id);
    }

    for mod_id in target_mods {
        profile_switch_require_snapshot(conn, profile_id, &profile_expected)?;
        let status: String = conn.query_row(
            "SELECT status FROM installed_mod WHERE id = ?1",
            [mod_id],
            |row| row.get(0),
        )?;
        if profile_expected.status(mod_id) != Some(status.as_str()) {
            return Err(profile_switch_changed());
        }
        if status == "disabled" {
            crate::state::enable(conn, mod_id, staging_root)?;
            profile_expected.transition(mod_id, "disabled", "active")?;
            outcome.enabled.push(mod_id);
        }
    }

    profile_switch_require_snapshot(conn, profile_id, &profile_expected)?;
    conn.execute(
        "UPDATE user_settings SET active_profile_id = ?1 WHERE id = 1",
        [profile_id],
    )?;

    Ok(outcome)
}

/// A portable description of a profile: its name and the *names* of the mods it
/// contains — not the mod files themselves, which this project never redistributes
/// (see the licensing/non-commercial decisions in the project docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileExport {
    pub name: String,
    pub mod_names: Vec<String>,
}

pub fn export(conn: &Connection, profile_id: i64) -> CoreResult<ProfileExport> {
    let name: String = conn.query_row(
        "SELECT name FROM profile WHERE id = ?1",
        [profile_id],
        |row| row.get(0),
    )?;
    let mut stmt = conn.prepare(
        "SELECT m.name FROM profile_mod pm \
         JOIN installed_mod m ON m.id = pm.installed_mod_id \
         WHERE pm.profile_id = ?1 ORDER BY m.name",
    )?;
    let mod_names = stmt
        .query_map([profile_id], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(ProfileExport { name, mod_names })
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct ImportOutcome {
    pub profile_id: i64,
    pub matched: Vec<String>,
    pub not_found_locally: Vec<String>,
}

/// Imports a [`ProfileExport`] as a new profile, matching each mod name against mods
/// already installed on *this* machine (by exact name, excluding uninstalled ones).
/// This project has no source integration yet (see the roadmap) and never auto-
/// downloads mods, so a name that doesn't match anything installed locally is
/// reported back rather than fetched — the user installs it themselves first.
pub fn import(conn: &Connection, export: &ProfileExport) -> CoreResult<ImportOutcome> {
    let profile_id = create(conn, &export.name)?;
    let mut outcome = ImportOutcome {
        profile_id,
        ..Default::default()
    };
    for name in &export.mod_names {
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM installed_mod WHERE name = ?1 AND status != 'uninstalled'",
                [name],
                |row| row.get(0),
            )
            .optional()?;
        match existing {
            Some(mod_id) => {
                add_mod(conn, profile_id, mod_id)?;
                outcome.matched.push(name.clone());
            }
            None => outcome.not_found_locally.push(name.clone()),
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    // OpenAI-Codex_fresh_test_review: Register a synthetic active owner of a regular file.
    fn owner_add_active(
        conn: &Connection,
        game: &Path,
        target: &Path,
        backup: Option<&Path>,
        name: &str,
    ) -> i64 {
        let digest = crate::util::hash_file(target).unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name,source_type,install_path,status,mode) \
             VALUES (?1,'asi',?2,'active','legacy-sp')",
            rusqlite::params![name, game.to_str().expect("synthetic root must be Unicode")],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO installed_mod_file (installed_mod_id,target_path,backup_path,file_hash) \
             VALUES (?1,?2,?3,?4)",
            rusqlite::params![
                id,
                target.to_str().unwrap(),
                backup.map(|p| p.to_str().unwrap()),
                digest
            ],
        )
        .unwrap();
        id
    }

    // OpenAI-Codex_profile_fixture_review: Build only synthetic test records.
    #[derive(serde::Serialize)]
    struct OwnerSyntheticStagedFile {
        file_id: i64,
        index: usize,
        hash: String,
    }

    fn owner_synthetic_hash(bytes: &[u8]) -> String {
        use sha2::Digest;
        format!("{:x}", sha2::Sha256::digest(bytes))
    }

    fn owner_seed_event(conn: &Connection, id: i64, kind: &str) {
        assert!(matches!(kind, "disable" | "enable"));
        assert_eq!(
            conn.execute(
                "INSERT INTO install_event (installed_mod_id,event_type,success) \
                 VALUES (?1,?2,1)",
                rusqlite::params![id, kind],
            )
            .unwrap(),
            1
        );
    }

    fn owner_add_disabled(
        conn: &Connection,
        game: &Path,
        target: &Path,
        backup: Option<&Path>,
        name: &str,
        installed_payload: &[u8],
    ) -> i64 {
        conn.execute(
            "INSERT INTO installed_mod (name,source_type,install_path,status,mode) \
             VALUES (?1,'asi',?2,'disabled','legacy-sp')",
            rusqlite::params![name, game.to_str().unwrap()],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO installed_mod_file (installed_mod_id,target_path,backup_path,file_hash) \
             VALUES (?1,?2,?3,?4)",
            rusqlite::params![
                id,
                target.to_str().unwrap(),
                backup.map(|path| path.to_str().unwrap()),
                owner_synthetic_hash(installed_payload),
            ],
        )
        .unwrap();
        owner_seed_event(conn, id, "disable");
        id
    }

    fn owner_seed_stage(conn: &Connection, staging: &Path, id: i64, payloads: &[&[u8]]) {
        let ids = {
            let mut statement = conn
                .prepare("SELECT id FROM installed_mod_file WHERE installed_mod_id=?1 ORDER BY id")
                .unwrap();
            let rows = statement
                .query_map([id], |row| row.get::<_, i64>(0))
                .unwrap();
            rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
        };
        assert!(!ids.is_empty());
        assert_eq!(ids.len(), payloads.len());
        std::fs::create_dir_all(staging).unwrap();
        let directory = staging.join(id.to_string());
        std::fs::create_dir(&directory).unwrap();
        let mut manifest = Vec::with_capacity(ids.len());
        for (index, (&file_id, payload)) in ids.iter().zip(payloads).enumerate() {
            let staged = directory.join(index.to_string());
            std::fs::write(&staged, *payload).unwrap();
            manifest.push(OwnerSyntheticStagedFile {
                file_id,
                index,
                hash: crate::util::hash_file(&staged).unwrap(),
            });
        }
        std::fs::write(
            directory.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        owner_require_stage(conn, id, staging, payloads);
    }

    // OpenAI-Codex; OpenAI-Codex_Responsible: Source_Code.
    #[test]
    fn switch_requiring_disabling_is_refused_before_changes() {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        std::fs::create_dir(&game).unwrap();
        let target = game.join("outgoing.asi");
        let backup = temp.path().join("original.bin");
        let staging = temp.path().join("staging");
        std::fs::write(&target, b"outgoing payload").unwrap();
        std::fs::write(&backup, b"original").unwrap();

        let conn = crate::db::open_in_memory().unwrap();
        let mod_id = owner_add_active(&conn, &game, &target, Some(&backup), "Outgoing");
        let old_profile = create(&conn, "Old").unwrap();
        let new_profile = create(&conn, "New").unwrap();
        add_mod(&conn, old_profile, mod_id).unwrap();
        assert_eq!(
            conn.execute(
                "UPDATE user_settings SET active_profile_id = ?1 WHERE id = 1",
                [old_profile],
            )
            .unwrap(),
            1
        );
        crate::providers::ensure_installed_mod_available(&conn, mod_id).unwrap();
        let tree_before = profile_preflight_tree(temp.path());
        let db_before = profile_preflight_db(&conn);
        let changes_before = profile_preflight_changes(&conn);
        assert!(conn.is_autocommit());

        let error = switch(&conn, new_profile, &staging)
            .expect_err("a switch requiring disabling must be unavailable");
        assert!(conn.is_autocommit());
        assert!(
            error.to_string().contains("Mod disabling is unavailable."),
            "{error}"
        );
        assert_eq!(profile_preflight_tree(temp.path()), tree_before);
        assert_eq!(profile_preflight_db(&conn), db_before);
        assert_eq!(profile_preflight_changes(&conn), changes_before);
        assert_eq!(active_profile_id(&conn).unwrap(), Some(old_profile));
    }

    fn owner_status(conn: &Connection, id: i64) -> String {
        conn.query_row("SELECT status FROM installed_mod WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
    }

    type OwnerTestEvent = (i64, Option<i64>, String, String, i64, Option<String>);
    fn owner_events(conn: &Connection) -> Vec<OwnerTestEvent> {
        let mut stmt = conn
            .prepare(
                "SELECT id,installed_mod_id,event_type,timestamp,success,error_message \
             FROM install_event ORDER BY id",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })
            .unwrap();
        rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
    }

    fn owner_require_stage(conn: &Connection, id: i64, staging: &Path, expected: &[&[u8]]) {
        let dir = staging.join(id.to_string());
        let manifest_path = dir.join("manifest.json");
        assert!(std::fs::symlink_metadata(&manifest_path)
            .unwrap()
            .file_type()
            .is_file());
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        let items = manifest.as_array().unwrap();
        let payloads = crate::state::disabled_payloads(conn, id, staging).unwrap();
        assert_eq!(items.len(), expected.len());
        assert_eq!(payloads.len(), expected.len());
        for (i, (file_id, path)) in payloads.iter().enumerate() {
            assert_eq!(path.as_path(), dir.join(i.to_string()).as_path());
            assert_eq!(std::fs::read(path).unwrap().as_slice(), expected[i]);
            let hash = crate::util::hash_file(path).unwrap();
            assert_eq!(
                items[i].get("file_id").and_then(|v| v.as_i64()),
                Some(*file_id)
            );
            assert_eq!(
                items[i].get("index").and_then(|v| v.as_u64()),
                Some(i as u64)
            );
            assert_eq!(
                items[i].get("hash").and_then(|v| v.as_str()),
                Some(hash.as_str())
            );
        }
    }

    fn owner_require_fixture(f: &ProfilePreflightFixture, extra: &[i64]) {
        for id in [f.outgoing_id, f.incoming_id] {
            crate::providers::ensure_installed_mod_available(&f.conn, id).unwrap();
        }
        for &id in extra {
            crate::providers::ensure_installed_mod_available(&f.conn, id).unwrap();
        }
        assert_eq!(owner_status(&f.conn, f.outgoing_id), "active");
        assert_eq!(owner_status(&f.conn, f.incoming_id), "disabled");
        assert_eq!(
            active_profile_id(&f.conn).unwrap(),
            Some(f.outgoing_profile)
        );
        owner_require_stage(
            &f.conn,
            f.incoming_id,
            &f.staging,
            &[&b"alpha edited before disable"[..], &b"bravo payload"[..]],
        );
        assert!(
            matches!(std::fs::symlink_metadata(f.staging.join(f.outgoing_id.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound)
        );
        assert_eq!(
            std::fs::read(&f.outgoing_target).unwrap(),
            b"outgoing payload"
        );
        assert_eq!(
            std::fs::read(&f.outgoing_backup).unwrap(),
            b"outgoing original"
        );
        assert_eq!(
            std::fs::read(&f.incoming_backup).unwrap(),
            b"alpha original"
        );
    }

    fn owner_refusal_preserves_fixture(f: &ProfilePreflightFixture, target: &Path, owners: &[i64]) {
        let tree = profile_preflight_tree(f.temp.path());
        let db = profile_preflight_db(&f.conn);
        let events = owner_events(&f.conn);
        let changes = profile_preflight_changes(&f.conn);
        match switch(&f.conn, f.incoming_profile, &f.staging) {
            Err(crate::error::CoreError::ForeignConflict { path, owner_mod_id }) => {
                assert_eq!(path.as_path(), target);
                assert!(owners.contains(&owner_mod_id), "unexpected conflict owner");
            }
            _ => panic!("expected a recorded active-owner conflict"),
        }
        assert!(
            profile_preflight_tree(f.temp.path()) == tree,
            "synthetic tree changed"
        );
        assert!(
            profile_preflight_db(&f.conn) == db,
            "synthetic database changed"
        );
        assert_eq!(owner_events(&f.conn), events);
        assert_eq!(profile_preflight_changes(&f.conn), changes);
        owner_require_fixture(f, &[]);
    }

    // OpenAI-Codex_fresh_test_review: A retained active owner blocks the incoming target.
    fn owner_retained_incoming_case(shared: bool) {
        let f = profile_preflight_fixture();
        std::fs::write(&f.incoming_alpha, b"retained active owner").unwrap();
        let other = owner_add_active(
            &f.conn,
            &f.temp.path().join("game"),
            &f.incoming_alpha,
            None,
            "Retained",
        );
        if shared {
            add_mod(&f.conn, f.outgoing_profile, other).unwrap();
            add_mod(&f.conn, f.incoming_profile, other).unwrap();
        }
        let memberships: i64 = f
            .conn
            .query_row(
                "SELECT COUNT(*) FROM profile_mod WHERE installed_mod_id=?1",
                [other],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(memberships, if shared { 2 } else { 0 });
        assert_eq!(owner_status(&f.conn, other), "active");
        owner_require_fixture(&f, &[other]);
        let report =
            crate::conflict::analyze(&f.conn, std::slice::from_ref(&f.incoming_alpha)).unwrap();
        assert!(!report.has_protected_hits());
        assert_eq!(report.foreign_conflicts.len(), 1);
        assert_eq!(report.foreign_conflicts[0].owner_mod_id, Some(other));
        assert_eq!(
            report.foreign_conflicts[0].path.as_path(),
            f.incoming_alpha.as_path()
        );
        owner_refusal_preserves_fixture(&f, &f.incoming_alpha, &[other]);
    }

    #[test]
    fn switch_refuses_shared_active_owner_before_any_disable() {
        owner_retained_incoming_case(true);
    }

    #[test]
    fn switch_refuses_unassigned_active_owner_before_any_disable() {
        owner_retained_incoming_case(false);
    }

    // OpenAI-Codex_fresh_test_review: Outgoing targets cannot overlap another active owner.
    fn owner_outgoing_collision_case(shared: bool) {
        let f = profile_preflight_fixture();
        let other = owner_add_active(
            &f.conn,
            &f.temp.path().join("game"),
            &f.outgoing_target,
            None,
            "Another active owner",
        );
        add_mod(&f.conn, f.outgoing_profile, other).unwrap();
        if shared {
            add_mod(&f.conn, f.incoming_profile, other).unwrap();
        }
        assert_eq!(owner_status(&f.conn, other), "active");
        owner_require_fixture(&f, &[other]);
        let report =
            crate::conflict::analyze(&f.conn, std::slice::from_ref(&f.outgoing_target)).unwrap();
        assert!(!report.has_protected_hits());
        assert_eq!(report.foreign_conflicts.len(), 2);
        for id in [f.outgoing_id, other] {
            assert!(report
                .foreign_conflicts
                .iter()
                .any(|item| item.owner_mod_id == Some(id)
                    && item.path.as_path() == f.outgoing_target.as_path()));
        }
        if shared {
            owner_refusal_preserves_fixture(&f, &f.outgoing_target, &[other]);
        } else {
            owner_refusal_preserves_fixture(&f, &f.outgoing_target, &[f.outgoing_id, other]);
        }
    }

    #[test]
    fn switch_refuses_outgoing_collision_with_shared_active_owner() {
        owner_outgoing_collision_case(true);
    }

    #[test]
    fn switch_refuses_collision_between_two_outgoing_mods() {
        owner_outgoing_collision_case(false);
    }

    // OpenAI-Codex_fresh_test_review: Ambiguous same-target profile handoffs.
    struct OwnerHandoffFixture {
        temp: tempfile::TempDir,
        conn: Connection,
        staging: std::path::PathBuf,
        target: std::path::PathBuf,
        from_profile: i64,
        to_profile: i64,
        outgoing: i64,
        incoming: i64,
        outgoing_bytes: &'static [u8],
        incoming_bytes: &'static [u8],
        outgoing_backup: Option<std::path::PathBuf>,
        incoming_backup: Option<std::path::PathBuf>,
        duplicate_outgoing_stage: bool,
    }

    fn owner_handoff_profiles(conn: &Connection, outgoing: i64, incoming: i64) -> (i64, i64) {
        let from = create(conn, "From profile").unwrap();
        let to = create(conn, "To profile").unwrap();
        add_mod(conn, from, outgoing).unwrap();
        add_mod(conn, to, incoming).unwrap();
        assert_eq!(
            conn.execute(
                "UPDATE user_settings SET active_profile_id=?1 WHERE id=1",
                [from]
            )
            .unwrap(),
            1
        );
        (from, to)
    }

    fn owner_handoff_file_info(conn: &Connection, id: i64) -> (String, Option<String>, String) {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM installed_mod_file WHERE installed_mod_id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        conn.query_row(
            "SELECT target_path,backup_path,file_hash FROM installed_mod_file WHERE installed_mod_id=?1",
            [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap()
    }

    // This duplicate is synthetic: normal enable removes the staged payload.
    fn owner_make_duplicate_active_stage(
        conn: &Connection,
        staging: &Path,
        id: i64,
        target: &Path,
        bytes: &[u8],
    ) {
        crate::providers::ensure_installed_mod_available(conn, id).unwrap();
        assert_eq!(owner_status(conn, id), "active");
        assert_eq!(std::fs::read(target).unwrap().as_slice(), bytes);
        let staged = staging.join(id.to_string()).join("0");
        assert!(matches!(std::fs::symlink_metadata(&staged),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound));
        owner_seed_stage(conn, staging, id, &[bytes]);
        owner_seed_event(conn, id, "disable");
        owner_seed_event(conn, id, "enable");
        owner_require_stage(conn, id, staging, &[bytes]);
    }

    fn owner_equal_handoff(backups_exist: bool, duplicate_stage: bool) -> OwnerHandoffFixture {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        let staging = temp.path().join("staging");
        std::fs::create_dir(&game).unwrap();
        let target = game.join("shared.asi");
        let (out_backup, in_backup) = if backups_exist {
            let out_root = temp.path().join("backup-outgoing");
            let in_root = temp.path().join("backup-incoming");
            std::fs::create_dir(&out_root).unwrap();
            std::fs::create_dir(&in_root).unwrap();
            let out = out_root.join("before.bin");
            let incoming = in_root.join("before.bin");
            std::fs::write(&out, b"same backup bytes").unwrap();
            std::fs::write(&incoming, b"same backup bytes").unwrap();
            (Some(out), Some(incoming))
        } else {
            (None, None)
        };
        let conn = crate::db::open_in_memory().unwrap();
        let incoming = owner_add_disabled(
            &conn,
            &game,
            &target,
            in_backup.as_deref(),
            "Incoming",
            b"same payload",
        );
        owner_seed_stage(&conn, &staging, incoming, &[&b"same payload"[..]]);
        owner_require_stage(&conn, incoming, &staging, &[&b"same payload"[..]]);
        std::fs::write(&target, b"same payload").unwrap();
        let outgoing = owner_add_active(&conn, &game, &target, out_backup.as_deref(), "Outgoing");
        if duplicate_stage {
            owner_make_duplicate_active_stage(&conn, &staging, outgoing, &target, b"same payload");
        }
        let (from_profile, to_profile) = owner_handoff_profiles(&conn, outgoing, incoming);
        OwnerHandoffFixture {
            temp,
            conn,
            staging,
            target,
            from_profile,
            to_profile,
            outgoing,
            incoming,
            outgoing_bytes: b"same payload",
            incoming_bytes: b"same payload",
            outgoing_backup: out_backup,
            incoming_backup: in_backup,
            duplicate_outgoing_stage: duplicate_stage,
        }
    }

    fn owner_predecessor_handoff() -> OwnerHandoffFixture {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        let staging = temp.path().join("staging");
        let out_root = temp.path().join("backup-A");
        let in_root = temp.path().join("backup-B");
        std::fs::create_dir(&game).unwrap();
        std::fs::create_dir(&out_root).unwrap();
        std::fs::create_dir(&in_root).unwrap();
        let target = game.join("shared.asi");
        let backup_a = out_root.join("before-A.bin");
        let backup_b = in_root.join("before-B.bin");
        let conn = crate::db::open_in_memory().unwrap();
        std::fs::write(&target, b"original").unwrap();
        std::fs::copy(&target, &backup_a).unwrap();
        std::fs::write(&target, b"A payload").unwrap();
        let a = owner_add_active(&conn, &game, &target, Some(&backup_a), "A");
        owner_make_duplicate_active_stage(&conn, &staging, a, &target, b"A payload");
        std::fs::copy(&target, &backup_b).unwrap();
        std::fs::write(&target, b"B payload").unwrap();
        let b = owner_add_active(&conn, &game, &target, Some(&backup_b), "B");
        for id in [a, b] {
            crate::providers::ensure_installed_mod_available(&conn, id).unwrap();
        }
        assert_eq!(owner_status(&conn, a), "active");
        assert_eq!(owner_status(&conn, b), "active");
        // Construct only synthetic test state; never call production disable.
        owner_seed_stage(&conn, &staging, b, &[&b"B payload"[..]]);
        std::fs::copy(&backup_b, &target).unwrap();
        assert_eq!(
            conn.execute(
                "UPDATE installed_mod SET status='disabled' WHERE id=?1",
                [b]
            )
            .unwrap(),
            1
        );
        owner_seed_event(&conn, b, "disable");
        assert_eq!(std::fs::read(&target).unwrap(), b"A payload");
        owner_require_stage(&conn, a, &staging, &[&b"A payload"[..]]);
        owner_require_stage(&conn, b, &staging, &[&b"B payload"[..]]);
        let (from_profile, to_profile) = owner_handoff_profiles(&conn, a, b);
        OwnerHandoffFixture {
            temp,
            conn,
            staging,
            target,
            from_profile,
            to_profile,
            outgoing: a,
            incoming: b,
            outgoing_bytes: b"A payload",
            incoming_bytes: b"B payload",
            outgoing_backup: Some(backup_a),
            incoming_backup: Some(backup_b),
            duplicate_outgoing_stage: true,
        }
    }

    fn owner_handoff_must_refuse_unchanged(f: &OwnerHandoffFixture) {
        for id in [f.outgoing, f.incoming] {
            crate::providers::ensure_installed_mod_available(&f.conn, id).unwrap();
        }
        assert_eq!(owner_status(&f.conn, f.outgoing), "active");
        assert_eq!(owner_status(&f.conn, f.incoming), "disabled");
        assert_eq!(active_profile_id(&f.conn).unwrap(), Some(f.from_profile));
        assert_eq!(
            mod_ids_in_profile(&f.conn, f.from_profile).unwrap(),
            vec![f.outgoing]
        );
        assert_eq!(
            mod_ids_in_profile(&f.conn, f.to_profile).unwrap(),
            vec![f.incoming]
        );
        assert!(std::fs::symlink_metadata(&f.target)
            .unwrap()
            .file_type()
            .is_file());
        assert_eq!(
            std::fs::read(&f.target).unwrap().as_slice(),
            f.outgoing_bytes
        );
        crate::protected_files::check_write(&f.target).unwrap();
        let out_row = owner_handoff_file_info(&f.conn, f.outgoing);
        let in_row = owner_handoff_file_info(&f.conn, f.incoming);
        assert_eq!(Path::new(&out_row.0), f.target.as_path());
        assert_eq!(Path::new(&in_row.0), f.target.as_path());
        assert_eq!(
            out_row.1.as_deref(),
            f.outgoing_backup.as_deref().map(|p| p.to_str().unwrap())
        );
        assert_eq!(
            in_row.1.as_deref(),
            f.incoming_backup.as_deref().map(|p| p.to_str().unwrap())
        );
        assert_eq!(out_row.2, crate::util::hash_file(&f.target).unwrap());
        owner_require_stage(&f.conn, f.incoming, &f.staging, &[f.incoming_bytes]);
        assert_eq!(
            in_row.2,
            crate::util::hash_file(&f.staging.join(f.incoming.to_string()).join("0")).unwrap()
        );
        if f.duplicate_outgoing_stage {
            owner_require_stage(&f.conn, f.outgoing, &f.staging, &[f.outgoing_bytes]);
        } else {
            assert!(
                matches!(std::fs::symlink_metadata(f.staging.join(f.outgoing.to_string())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound)
            );
        }
        let report = crate::conflict::analyze(&f.conn, std::slice::from_ref(&f.target)).unwrap();
        assert!(!report.has_protected_hits());
        assert_eq!(report.foreign_conflicts.len(), 1);
        assert_eq!(report.foreign_conflicts[0].owner_mod_id, Some(f.outgoing));
        assert_eq!(
            report.foreign_conflicts[0].path.as_path(),
            f.target.as_path()
        );
        let tree = profile_preflight_tree(f.temp.path());
        let db = profile_preflight_db(&f.conn);
        let events = owner_events(&f.conn);
        let changes = profile_preflight_changes(&f.conn);
        match switch(&f.conn, f.to_profile, &f.staging) {
            Err(crate::error::CoreError::ForeignConflict { path, owner_mod_id }) => {
                assert_eq!(path.as_path(), f.target.as_path());
                assert_eq!(owner_mod_id, f.outgoing);
            }
            _ => panic!("expected outgoing-owner same-target refusal"),
        }
        assert!(
            profile_preflight_tree(f.temp.path()) == tree,
            "synthetic tree changed"
        );
        assert!(
            profile_preflight_db(&f.conn) == db,
            "synthetic database changed"
        );
        assert_eq!(owner_events(&f.conn), events);
        assert_eq!(profile_preflight_changes(&f.conn), changes);
        assert_eq!(active_profile_id(&f.conn).unwrap(), Some(f.from_profile));
        assert_eq!(owner_status(&f.conn, f.outgoing), "active");
        assert_eq!(owner_status(&f.conn, f.incoming), "disabled");
        assert_eq!(
            std::fs::read(&f.target).unwrap().as_slice(),
            f.outgoing_bytes
        );
        owner_require_stage(&f.conn, f.incoming, &f.staging, &[f.incoming_bytes]);
        if f.duplicate_outgoing_stage {
            owner_require_stage(&f.conn, f.outgoing, &f.staging, &[f.outgoing_bytes]);
        }
    }

    #[test]
    fn switch_refuses_same_target_when_both_backups_none_and_hashes_equal() {
        let f = owner_equal_handoff(false, true);
        let out = owner_handoff_file_info(&f.conn, f.outgoing);
        let incoming = owner_handoff_file_info(&f.conn, f.incoming);
        assert!(out.1.is_none() && incoming.1.is_none());
        assert_eq!(out.0, incoming.0);
        assert_eq!(out.2, incoming.2);
        owner_handoff_must_refuse_unchanged(&f);
    }

    #[test]
    fn switch_refuses_same_target_with_equal_existing_backups_and_hashes() {
        let f = owner_equal_handoff(true, true);
        let out = owner_handoff_file_info(&f.conn, f.outgoing);
        let incoming = owner_handoff_file_info(&f.conn, f.incoming);
        assert_eq!(out.0, incoming.0);
        assert_eq!(out.2, incoming.2);
        let a = f.outgoing_backup.as_ref().unwrap();
        let b = f.incoming_backup.as_ref().unwrap();
        assert_ne!(a.parent(), b.parent());
        assert!(std::fs::symlink_metadata(a).unwrap().file_type().is_file());
        assert!(std::fs::symlink_metadata(b).unwrap().file_type().is_file());
        assert_eq!(std::fs::read(a).unwrap(), b"same backup bytes");
        assert_eq!(std::fs::read(b).unwrap(), b"same backup bytes");
        assert_eq!(
            crate::util::hash_file(a).unwrap(),
            crate::util::hash_file(b).unwrap()
        );
        owner_handoff_must_refuse_unchanged(&f);
    }

    #[test]
    fn switch_refuses_same_target_when_active_owner_has_no_stage() {
        let f = owner_equal_handoff(false, false);
        owner_handoff_must_refuse_unchanged(&f);
    }

    #[test]
    fn switch_refuses_same_target_when_incoming_backup_contains_outgoing_payload() {
        let f = owner_predecessor_handoff();
        let backup_a = f.outgoing_backup.as_ref().unwrap();
        let backup_b = f.incoming_backup.as_ref().unwrap();
        assert_ne!(backup_a.parent(), backup_b.parent());
        assert!(std::fs::symlink_metadata(backup_a)
            .unwrap()
            .file_type()
            .is_file());
        assert!(std::fs::symlink_metadata(backup_b)
            .unwrap()
            .file_type()
            .is_file());
        assert_eq!(std::fs::read(backup_a).unwrap(), b"original");
        assert_eq!(std::fs::read(backup_b).unwrap(), b"A payload");
        assert_eq!(
            crate::util::hash_file(backup_b).unwrap(),
            crate::util::hash_file(&f.target).unwrap()
        );
        owner_handoff_must_refuse_unchanged(&f);
    }

    // OpenAI-Codex_fresh_test_review: Active mod with no recorded file rows.
    fn owner_add_empty_active(conn: &Connection, game: &Path, name: &str) -> i64 {
        conn.execute(
            "INSERT INTO installed_mod (name,source_type,install_path,status,mode) \
             VALUES (?1,'asi',?2,'active','legacy-sp')",
            rusqlite::params![name, game.to_str().expect("synthetic root must be Unicode")],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[derive(Clone, Copy, Debug)]
    enum OwnerEmptyRole {
        Outgoing,
        Shared,
        Unassigned,
    }

    struct OwnerEmptyFixture {
        temp: tempfile::TempDir,
        conn: Connection,
        staging: std::path::PathBuf,
        incoming_target: std::path::PathBuf,
        outgoing_target: Option<std::path::PathBuf>,
        outgoing_backup: Option<std::path::PathBuf>,
        from_profile: i64,
        to_profile: i64,
        incoming: i64,
        outgoing: i64,
        empty: i64,
        role: OwnerEmptyRole,
    }

    fn owner_empty_fixture(role: OwnerEmptyRole) -> OwnerEmptyFixture {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        let staging = temp.path().join("staging");
        std::fs::create_dir(&game).unwrap();
        let incoming_target = game.join("incoming.asi");
        let conn = crate::db::open_in_memory().unwrap();
        let incoming = owner_add_disabled(
            &conn,
            &game,
            &incoming_target,
            None,
            "Incoming",
            b"incoming payload",
        );
        owner_seed_stage(&conn, &staging, incoming, &[&b"incoming payload"[..]]);
        let empty = owner_add_empty_active(&conn, &game, "Empty active");
        let (outgoing, outgoing_target, outgoing_backup) = match role {
            OwnerEmptyRole::Outgoing => (empty, None, None),
            OwnerEmptyRole::Shared | OwnerEmptyRole::Unassigned => {
                let target = game.join("outgoing.asi");
                let backup = temp.path().join("outgoing-original.bak");
                std::fs::write(&target, b"outgoing payload").unwrap();
                std::fs::write(&backup, b"outgoing original").unwrap();
                let id = owner_add_active(&conn, &game, &target, Some(&backup), "Outgoing");
                (id, Some(target), Some(backup))
            }
        };
        let from_profile = create(&conn, "From").unwrap();
        let to_profile = create(&conn, "To").unwrap();
        add_mod(&conn, from_profile, outgoing).unwrap();
        add_mod(&conn, to_profile, incoming).unwrap();
        if matches!(role, OwnerEmptyRole::Shared) {
            add_mod(&conn, from_profile, empty).unwrap();
            add_mod(&conn, to_profile, empty).unwrap();
        }
        assert_eq!(
            conn.execute(
                "UPDATE user_settings SET active_profile_id=?1 WHERE id=1",
                [from_profile]
            )
            .unwrap(),
            1
        );
        OwnerEmptyFixture {
            temp,
            conn,
            staging,
            incoming_target,
            outgoing_target,
            outgoing_backup,
            from_profile,
            to_profile,
            incoming,
            outgoing,
            empty,
            role,
        }
    }

    fn owner_empty_memberships(conn: &Connection, id: i64) -> Vec<i64> {
        let mut stmt = conn
            .prepare(
                "SELECT profile_id FROM profile_mod WHERE installed_mod_id=?1 ORDER BY profile_id",
            )
            .unwrap();
        let rows = stmt.query_map([id], |r| r.get(0)).unwrap();
        rows.collect::<rusqlite::Result<Vec<i64>>>().unwrap()
    }

    fn owner_empty_must_refuse(role: OwnerEmptyRole) {
        let f = owner_empty_fixture(role);
        let count: i64 = f
            .conn
            .query_row(
                "SELECT COUNT(*) FROM installed_mod_file WHERE installed_mod_id=?1",
                [f.empty],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
        assert_eq!(owner_status(&f.conn, f.empty), "active");
        assert_eq!(owner_status(&f.conn, f.outgoing), "active");
        assert_eq!(owner_status(&f.conn, f.incoming), "disabled");
        assert_eq!(active_profile_id(&f.conn).unwrap(), Some(f.from_profile));
        let expected_memberships = match f.role {
            OwnerEmptyRole::Outgoing => vec![f.from_profile],
            OwnerEmptyRole::Shared => vec![f.from_profile, f.to_profile],
            OwnerEmptyRole::Unassigned => vec![],
        };
        assert_eq!(
            owner_empty_memberships(&f.conn, f.empty),
            expected_memberships
        );
        for id in [f.empty, f.outgoing, f.incoming] {
            crate::providers::ensure_installed_mod_available(&f.conn, id).unwrap();
        }
        owner_require_stage(&f.conn, f.incoming, &f.staging, &[&b"incoming payload"[..]]);
        assert!(matches!(std::fs::symlink_metadata(&f.incoming_target),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound));
        let report =
            crate::conflict::analyze(&f.conn, std::slice::from_ref(&f.incoming_target)).unwrap();
        assert!(!report.has_protected_hits());
        assert!(report.foreign_conflicts.is_empty());
        if let Some(target) = &f.outgoing_target {
            assert_eq!(std::fs::read(target).unwrap(), b"outgoing payload");
            crate::protected_files::check_write(target).unwrap();
        }
        if let Some(backup) = &f.outgoing_backup {
            assert_eq!(std::fs::read(backup).unwrap(), b"outgoing original");
        }
        let actual_outgoing = {
            let mut stmt = f
                .conn
                .prepare(
                    "SELECT DISTINCT pm.installed_mod_id FROM profile_mod pm \
                 JOIN installed_mod m ON m.id=pm.installed_mod_id \
                 WHERE pm.profile_id!=?1 AND m.status='active' \
                 AND NOT EXISTS (SELECT 1 FROM profile_mod t WHERE t.profile_id=?1 \
                                 AND t.installed_mod_id=pm.installed_mod_id) \
                 ORDER BY pm.installed_mod_id",
                )
                .unwrap();
            let rows = stmt.query_map([f.to_profile], |r| r.get(0)).unwrap();
            rows.collect::<rusqlite::Result<Vec<i64>>>().unwrap()
        };
        let actual_incoming = {
            let mut stmt = f
                .conn
                .prepare(
                    "SELECT pm.installed_mod_id FROM profile_mod pm \
                 JOIN installed_mod m ON m.id=pm.installed_mod_id \
                 WHERE pm.profile_id=?1 AND m.status='disabled' ORDER BY pm.installed_mod_id",
                )
                .unwrap();
            let rows = stmt.query_map([f.to_profile], |r| r.get(0)).unwrap();
            rows.collect::<rusqlite::Result<Vec<i64>>>().unwrap()
        };
        assert_eq!(actual_outgoing, vec![f.outgoing]);
        assert_eq!(actual_incoming, vec![f.incoming]);
        let tree = profile_preflight_tree(f.temp.path());
        let db = profile_preflight_db(&f.conn);
        let events = owner_events(&f.conn);
        let changes = profile_preflight_changes(&f.conn);
        match switch(&f.conn, f.to_profile, &f.staging) {
            Err(crate::error::CoreError::UnsupportedFormat { reason }) => assert_eq!(
                reason,
                format!(
                    "profile switch cannot determine targets for active mod {}: no file records",
                    f.empty
                )
            ),
            _ => panic!("expected the active-mod no-file-records refusal"),
        }
        assert!(
            profile_preflight_tree(f.temp.path()) == tree,
            "synthetic tree changed"
        );
        assert!(
            profile_preflight_db(&f.conn) == db,
            "synthetic database changed"
        );
        assert_eq!(owner_events(&f.conn), events);
        assert_eq!(profile_preflight_changes(&f.conn), changes);
        assert_eq!(active_profile_id(&f.conn).unwrap(), Some(f.from_profile));
        assert_eq!(owner_status(&f.conn, f.empty), "active");
        owner_require_stage(&f.conn, f.incoming, &f.staging, &[&b"incoming payload"[..]]);
    }

    #[test]
    fn switch_refuses_empty_active_outgoing_before_changes() {
        owner_empty_must_refuse(OwnerEmptyRole::Outgoing);
    }
    #[test]
    fn switch_refuses_empty_shared_active_before_changes() {
        owner_empty_must_refuse(OwnerEmptyRole::Shared);
    }
    #[test]
    fn switch_refuses_empty_unassigned_active_before_changes() {
        owner_empty_must_refuse(OwnerEmptyRole::Unassigned);
    }

    // OpenAI-Codex_fresh_test_review: A no-move switch may keep zero-row active mods.
    #[test]
    fn switch_with_no_lifecycle_actions_allows_empty_active_records() {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        let staging = temp.path().join("staging");
        std::fs::create_dir(&game).unwrap();
        let conn = crate::db::open_in_memory().unwrap();
        let shared = owner_add_empty_active(&conn, &game, "Shared empty");
        let unassigned = owner_add_empty_active(&conn, &game, "Unassigned empty");
        let from = create(&conn, "From").unwrap();
        let to = create(&conn, "To").unwrap();
        add_mod(&conn, from, shared).unwrap();
        add_mod(&conn, to, shared).unwrap();
        assert_eq!(
            conn.execute(
                "UPDATE user_settings SET active_profile_id=?1 WHERE id=1",
                [from]
            )
            .unwrap(),
            1
        );
        assert_eq!(owner_empty_memberships(&conn, shared), vec![from, to]);
        assert!(owner_empty_memberships(&conn, unassigned).is_empty());
        for id in [shared, unassigned] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM installed_mod_file WHERE installed_mod_id=?1",
                    [id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 0);
            assert_eq!(owner_status(&conn, id), "active");
            crate::providers::ensure_installed_mod_available(&conn, id).unwrap();
        }
        assert!(matches!(std::fs::symlink_metadata(&staging),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound));
        let columns = {
            let mut stmt = conn.prepare("PRAGMA table_info(user_settings)").unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, String>(1)).unwrap();
            rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
        };
        let active_col = columns
            .iter()
            .position(|name| name == "active_profile_id")
            .unwrap()
            + 1;
        let mut expected_db = profile_preflight_db(&conn);
        let settings = expected_db
            .iter_mut()
            .find(|(name, _)| name == "user_settings")
            .unwrap();
        assert_eq!(settings.1.len(), 1);
        assert_eq!(
            settings.1[0][active_col],
            rusqlite::types::Value::Integer(from)
        );
        settings.1[0][active_col] = rusqlite::types::Value::Integer(to);
        let tree = profile_preflight_tree(temp.path());
        let events = owner_events(&conn);
        let changes = profile_preflight_changes(&conn);
        let outcome = switch(&conn, to, &staging).unwrap();
        assert!(outcome.disabled.is_empty());
        assert!(outcome.enabled.is_empty());
        assert_eq!(active_profile_id(&conn).unwrap(), Some(to));
        assert_eq!(owner_status(&conn, shared), "active");
        assert_eq!(owner_status(&conn, unassigned), "active");
        assert!(profile_preflight_tree(temp.path()) == tree);
        assert!(profile_preflight_db(&conn) == expected_db);
        assert_eq!(owner_events(&conn), events);
        assert_eq!(profile_preflight_changes(&conn), changes + 1);
        assert!(matches!(std::fs::symlink_metadata(&staging),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound));
    }

    // OpenAI-Codex_fresh_test_review: Preserve caller transaction ownership for an empty switch.
    #[test]
    fn empty_profile_switch_preserves_caller_transaction_and_rollback() {
        let conn = crate::db::open_in_memory().unwrap();
        let previous = create(&conn, "Previous empty profile").unwrap();
        let target = create(&conn, "Target empty profile").unwrap();
        assert_eq!(
            conn.execute(
                "UPDATE user_settings SET active_profile_id=?1 WHERE id=1",
                [previous]
            )
            .unwrap(),
            1
        );
        assert!(mod_ids_in_profile(&conn, previous).unwrap().is_empty());
        assert!(mod_ids_in_profile(&conn, target).unwrap().is_empty());
        let mod_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM installed_mod", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mod_count, 0);
        let baseline = profile_preflight_db(&conn);
        assert!(conn.is_autocommit());
        conn.execute("BEGIN DEFERRED", []).unwrap();
        assert!(!conn.is_autocommit());
        assert_eq!(
            conn.execute(
                "UPDATE user_settings SET language='synthetic-caller-marker' WHERE id=1",
                []
            )
            .unwrap(),
            1
        );
        let outcome = switch(&conn, target, Path::new("unused-staging")).unwrap();
        assert!(outcome.enabled.is_empty());
        assert!(outcome.disabled.is_empty());
        assert!(!conn.is_autocommit());
        assert_eq!(active_profile_id(&conn).unwrap(), Some(target));
        let marker: String = conn
            .query_row("SELECT language FROM user_settings WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(marker, "synthetic-caller-marker");
        // This is the caller's explicit rollback, not an action by switch.
        conn.execute("ROLLBACK", []).unwrap();
        assert!(conn.is_autocommit());
        assert_eq!(active_profile_id(&conn).unwrap(), Some(previous));
        assert!(
            profile_preflight_db(&conn) == baseline,
            "caller rollback did not restore the DB"
        );
    }

    // OpenAI-Codex_fresh_test_review: Both incoming mods are disabled, not active owners.
    #[test]
    fn switch_refuses_two_disabled_incoming_mods_with_same_target() {
        let f = profile_preflight_fixture();
        let game = f.temp.path().join("game");
        let second_original = game.join("second-incoming.asi");
        let second = owner_add_disabled(
            &f.conn,
            &game,
            &second_original,
            None,
            "Second incoming",
            b"second payload",
        );
        crate::providers::ensure_installed_mod_available(&f.conn, second).unwrap();
        owner_seed_stage(&f.conn, &f.staging, second, &[&b"second payload"[..]]);
        owner_require_stage(&f.conn, second, &f.staging, &[&b"second payload"[..]]);
        assert!(matches!(std::fs::symlink_metadata(&second_original),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound));
        assert_eq!(
            f.conn
                .execute(
                    "UPDATE installed_mod_file SET target_path=?1 WHERE installed_mod_id=?2",
                    rusqlite::params![f.incoming_alpha.to_str().unwrap(), second],
                )
                .unwrap(),
            1
        );
        add_mod(&f.conn, f.incoming_profile, second).unwrap();
        owner_require_fixture(&f, &[second]);
        assert_eq!(owner_status(&f.conn, second), "disabled");
        owner_require_stage(&f.conn, second, &f.staging, &[&b"second payload"[..]]);
        assert_eq!(std::fs::read(&f.incoming_alpha).unwrap(), b"alpha original");
        let overlap: i64 = f.conn.query_row(
            "SELECT COUNT(*) FROM installed_mod_file x JOIN installed_mod m ON m.id=x.installed_mod_id \
             WHERE x.target_path=?1 AND x.installed_mod_id IN (?2,?3) AND m.status='disabled'",
            rusqlite::params![f.incoming_alpha.to_str().unwrap(), f.incoming_id, second], |r| r.get(0),
        ).unwrap();
        assert_eq!(overlap, 2);
        let report =
            crate::conflict::analyze(&f.conn, std::slice::from_ref(&f.incoming_alpha)).unwrap();
        assert!(!report.has_protected_hits());
        assert_eq!(report.foreign_conflicts.len(), 1);
        assert!(report.foreign_conflicts[0].owner_mod_id.is_none());
        assert_eq!(
            report.foreign_conflicts[0].path.as_path(),
            f.incoming_alpha.as_path()
        );
        let tree = profile_preflight_tree(f.temp.path());
        let db = profile_preflight_db(&f.conn);
        let events = owner_events(&f.conn);
        let changes = profile_preflight_changes(&f.conn);
        let forward = format!(
            "profile mods #{} and #{} have overlapping targets: {} and {}",
            f.incoming_id,
            second,
            f.incoming_alpha.display(),
            f.incoming_alpha.display()
        );
        let reverse = format!(
            "profile mods #{} and #{} have overlapping targets: {} and {}",
            second,
            f.incoming_id,
            f.incoming_alpha.display(),
            f.incoming_alpha.display()
        );
        match switch(&f.conn, f.incoming_profile, &f.staging) {
            Err(crate::error::CoreError::UnsupportedFormat { reason }) => {
                assert!(
                    reason == forward || reason == reverse,
                    "unexpected overlap diagnostic"
                );
            }
            _ => panic!("expected the disabled-incoming overlap refusal"),
        }
        assert!(
            profile_preflight_tree(f.temp.path()) == tree,
            "synthetic tree changed"
        );
        assert!(
            profile_preflight_db(&f.conn) == db,
            "synthetic database changed"
        );
        assert_eq!(owner_events(&f.conn), events);
        assert_eq!(profile_preflight_changes(&f.conn), changes);
        assert_eq!(
            active_profile_id(&f.conn).unwrap(),
            Some(f.outgoing_profile)
        );
        assert_eq!(owner_status(&f.conn, f.outgoing_id), "active");
        assert_eq!(owner_status(&f.conn, f.incoming_id), "disabled");
        assert_eq!(owner_status(&f.conn, second), "disabled");
        assert_eq!(std::fs::read(&f.incoming_alpha).unwrap(), b"alpha original");
        owner_require_stage(
            &f.conn,
            f.incoming_id,
            &f.staging,
            &[&b"alpha edited before disable"[..], &b"bravo payload"[..]],
        );
        owner_require_stage(&f.conn, second, &f.staging, &[&b"second payload"[..]]);
    }

    // OpenAI-Codex_fresh_test_review: Capture recorded rows for a synthetic owner.
    fn owner_self_rows(conn: &Connection, id: i64) -> Vec<(i64, String, Option<String>, String)> {
        let mut stmt = conn
            .prepare(
                "SELECT id,target_path,backup_path,file_hash FROM installed_mod_file \
             WHERE installed_mod_id=?1 ORDER BY id",
            )
            .unwrap();
        let rows = stmt
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap();
        rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
    }

    fn owner_self_duplicate_must_refuse(f: &ProfilePreflightFixture, id: i64, target: &Path) {
        owner_require_fixture(f, &[]);
        let rows = owner_self_rows(&f.conn, id);
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0].0, rows[1].0);
        assert_eq!(Path::new(&rows[0].1), target);
        assert_eq!(Path::new(&rows[1].1), target);
        crate::protected_files::check_write(target).unwrap();
        let tree = profile_preflight_tree(f.temp.path());
        let db = profile_preflight_db(&f.conn);
        let events = owner_events(&f.conn);
        let changes = profile_preflight_changes(&f.conn);
        match switch(&f.conn, f.incoming_profile, &f.staging) {
            Err(crate::error::CoreError::UnsupportedFormat { reason }) => assert_eq!(
                reason,
                format!(
                    "profile mod #{} has overlapping recorded targets: {} and {}",
                    id,
                    target.display(),
                    target.display()
                )
            ),
            _ => panic!("expected the same-mod target overlap refusal"),
        }
        assert!(
            profile_preflight_tree(f.temp.path()) == tree,
            "synthetic tree changed"
        );
        assert!(
            profile_preflight_db(&f.conn) == db,
            "synthetic database changed"
        );
        assert_eq!(owner_events(&f.conn), events);
        assert_eq!(profile_preflight_changes(&f.conn), changes);
        assert_eq!(
            active_profile_id(&f.conn).unwrap(),
            Some(f.outgoing_profile)
        );
        owner_require_fixture(f, &[]);
    }

    // The outgoing has two recorded rows for the same existing regular target.
    #[test]
    fn switch_refuses_duplicate_targets_within_outgoing() {
        let f = profile_preflight_fixture();
        assert_eq!(owner_self_rows(&f.conn, f.outgoing_id).len(), 1);
        assert_eq!(f.conn.execute(
            "INSERT INTO installed_mod_file (installed_mod_id,target_path,backup_path,file_hash) \
             SELECT installed_mod_id,target_path,backup_path,file_hash \
             FROM installed_mod_file WHERE installed_mod_id=?1",
            [f.outgoing_id],
        ).unwrap(), 1);
        owner_self_duplicate_must_refuse(&f, f.outgoing_id, &f.outgoing_target);
    }

    // The real two-file staging manifest stays valid after only the synthetic DB target changes.
    #[test]
    fn switch_refuses_duplicate_targets_within_incoming() {
        let f = profile_preflight_fixture();
        let rows = owner_self_rows(&f.conn, f.incoming_id);
        assert_eq!(rows.len(), 2);
        assert_eq!(Path::new(&rows[0].1), f.incoming_alpha.as_path());
        assert_eq!(Path::new(&rows[1].1), f.incoming_bravo.as_path());
        assert_eq!(f.conn.execute(
            "UPDATE installed_mod_file SET target_path=?1 WHERE id=?2 AND installed_mod_id=?3",
            rusqlite::params![f.incoming_alpha.to_str().unwrap(), rows[1].0, f.incoming_id],
        ).unwrap(), 1);
        owner_require_stage(
            &f.conn,
            f.incoming_id,
            &f.staging,
            &[&b"alpha edited before disable"[..], &b"bravo payload"[..]],
        );
        owner_self_duplicate_must_refuse(&f, f.incoming_id, &f.incoming_alpha);
    }

    // An unrelated retained active mod may contain duplicate rows without a new veto.
    #[test]
    fn switch_allows_unrelated_retained_active_with_duplicate_targets() {
        let f = profile_preflight_fixture();
        add_mod(&f.conn, f.incoming_profile, f.outgoing_id).unwrap();
        let game = f.temp.path().join("game");
        let target = game.join("retained.asi");
        std::fs::write(&target, b"retained payload").unwrap();
        let retained = owner_add_active(&f.conn, &game, &target, None, "Retained");
        assert_eq!(f.conn.execute(
            "INSERT INTO installed_mod_file (installed_mod_id,target_path,backup_path,file_hash) \
             SELECT installed_mod_id,target_path,backup_path,file_hash \
             FROM installed_mod_file WHERE installed_mod_id=?1",
            [retained],
        ).unwrap(), 1);
        let memberships: i64 = f
            .conn
            .query_row(
                "SELECT COUNT(*) FROM profile_mod WHERE installed_mod_id=?1",
                [retained],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(memberships, 0);
        assert_eq!(owner_status(&f.conn, retained), "active");
        owner_require_fixture(&f, &[retained]);
        let retained_before = owner_self_rows(&f.conn, retained);
        let outgoing_before = owner_self_rows(&f.conn, f.outgoing_id);
        let incoming_before = owner_self_rows(&f.conn, f.incoming_id);
        assert_eq!(retained_before.len(), 2);
        assert_ne!(retained_before[0].0, retained_before[1].0);
        assert_eq!(Path::new(&retained_before[0].1), target.as_path());
        assert_eq!(Path::new(&retained_before[1].1), target.as_path());
        let report = crate::conflict::analyze(&f.conn, std::slice::from_ref(&target)).unwrap();
        assert!(!report.has_protected_hits());
        assert_eq!(report.foreign_conflicts.len(), 2);
        assert!(report
            .foreign_conflicts
            .iter()
            .all(|item| item.owner_mod_id == Some(retained)));
        let events_before = owner_events(&f.conn);
        let outcome = switch(&f.conn, f.incoming_profile, &f.staging).unwrap();
        assert!(outcome.disabled.is_empty());
        assert_eq!(outcome.enabled, vec![f.incoming_id]);
        assert_eq!(
            active_profile_id(&f.conn).unwrap(),
            Some(f.incoming_profile)
        );
        assert_eq!(owner_status(&f.conn, f.outgoing_id), "active");
        assert_eq!(owner_status(&f.conn, f.incoming_id), "active");
        let incoming_stage = f.staging.join(f.incoming_id.to_string());
        for index in ["0", "1"] {
            assert!(
                matches!(std::fs::symlink_metadata(incoming_stage.join(index)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound)
            );
        }
        assert_eq!(owner_status(&f.conn, retained), "active");
        assert_eq!(owner_self_rows(&f.conn, retained), retained_before);
        assert_eq!(owner_self_rows(&f.conn, f.outgoing_id), outgoing_before);
        assert_eq!(owner_self_rows(&f.conn, f.incoming_id), incoming_before);
        assert_eq!(std::fs::read(&target).unwrap(), b"retained payload");
        assert_eq!(
            std::fs::read(&f.outgoing_target).unwrap(),
            b"outgoing payload"
        );
        assert_eq!(
            std::fs::read(&f.outgoing_backup).unwrap(),
            b"outgoing original"
        );
        assert_eq!(
            std::fs::read(&f.incoming_alpha).unwrap(),
            b"alpha edited before disable"
        );
        assert_eq!(std::fs::read(&f.incoming_bravo).unwrap(), b"bravo payload");
        assert_eq!(
            std::fs::read(&f.incoming_backup).unwrap(),
            b"alpha original"
        );
        assert!(
            matches!(std::fs::symlink_metadata(f.staging.join(f.outgoing_id.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound)
        );
        let events_after = owner_events(&f.conn);
        assert_eq!(events_after.len(), events_before.len() + 1);
        assert_eq!(
            &events_after[..events_before.len()],
            events_before.as_slice()
        );
        let event = events_after.last().unwrap();
        assert_eq!(event.1, Some(f.incoming_id));
        assert_eq!(event.2.as_str(), "enable");
        assert_eq!(event.4, 1);
        assert!(event.5.is_none());
    }

    // OpenAI-Codex_fresh_test_review: Refuse a missing outgoing target before any disable.
    #[test]
    fn switch_refuses_missing_outgoing_target_before_first_disable() {
        let f = profile_preflight_fixture();
        let missing = f.temp.path().join("game").join("missing-outgoing.asi");
        assert!(matches!(std::fs::symlink_metadata(&missing),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound));
        assert_eq!(
            f.conn
                .execute(
                    "UPDATE installed_mod_file SET target_path=?1 WHERE installed_mod_id=?2",
                    rusqlite::params![missing.to_str().unwrap(), f.outgoing_id],
                )
                .unwrap(),
            1
        );
        owner_require_fixture(&f, &[]);
        let (file_id, recorded): (i64, String) = f
            .conn
            .query_row(
                "SELECT id,target_path FROM installed_mod_file WHERE installed_mod_id=?1",
                [f.outgoing_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(Path::new(&recorded), missing.as_path());
        crate::protected_files::check_write(&missing).unwrap();
        let tree = profile_preflight_tree(f.temp.path());
        let db = profile_preflight_db(&f.conn);
        let events = owner_events(&f.conn);
        let changes = profile_preflight_changes(&f.conn);
        match switch(&f.conn, f.incoming_profile, &f.staging) {
            Err(crate::error::CoreError::UnsupportedFormat { reason }) => assert_eq!(
                reason,
                format!(
                    "missing target for profile outgoing mod #{} file #{}: {}",
                    f.outgoing_id, file_id, recorded
                )
            ),
            _ => panic!("expected the missing outgoing target refusal"),
        }
        assert!(
            profile_preflight_tree(f.temp.path()) == tree,
            "synthetic tree changed"
        );
        assert!(
            profile_preflight_db(&f.conn) == db,
            "synthetic database changed"
        );
        assert_eq!(owner_events(&f.conn), events);
        assert_eq!(profile_preflight_changes(&f.conn), changes);
        assert_eq!(
            active_profile_id(&f.conn).unwrap(),
            Some(f.outgoing_profile)
        );
        owner_require_stage(
            &f.conn,
            f.incoming_id,
            &f.staging,
            &[&b"alpha edited before disable"[..], &b"bravo payload"[..]],
        );
    }

    // OpenAI-Codex: A nonexistent profile must not deactivate existing mods.
    #[test]
    fn nonexistent_profile_refuses_before_changing_active_members() {
        let temp = tempfile::tempdir().unwrap();
        let game_root = temp.path().join("game");
        let staging_root = temp.path().join("staging");
        std::fs::create_dir(&game_root).unwrap();
        let target = game_root.join("current.asi");
        std::fs::write(&target, b"current mod").unwrap();

        let conn = crate::db::open_in_memory().unwrap();
        let profile_id = create(&conn, "Current").unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status, mode) \
             VALUES ('Current mod', 'asi', ?1, 'active', 'legacy-sp')",
            [game_root.to_str().unwrap()],
        )
        .unwrap();
        let mod_id = conn.last_insert_rowid();
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
        add_mod(&conn, profile_id, mod_id).unwrap();
        conn.execute(
            "UPDATE user_settings SET active_profile_id = ?1 WHERE id = 1",
            [profile_id],
        )
        .unwrap();

        let missing_id = profile_id.checked_add(1_000_000).unwrap();
        let missing_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM profile WHERE id = ?1)",
                [missing_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!missing_exists);
        assert!(crate::providers::ensure_installed_mod_available(&conn, mod_id).is_ok());
        assert!(crate::protected_files::check_write(&target).is_ok());
        assert_eq!(active_profile_id(&conn).unwrap(), Some(profile_id));
        let events_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM install_event", [], |row| row.get(0))
            .unwrap();

        let error = switch(&conn, missing_id, &staging_root).unwrap_err();
        assert!(matches!(
            error,
            crate::error::CoreError::UnsupportedFormat { reason }
                if reason == format!("no profile with id {missing_id}")
        ));
        assert_eq!(std::fs::read(&target).unwrap(), b"current mod");
        assert!(!staging_root.exists());
        let status: String = conn
            .query_row(
                "SELECT status FROM installed_mod WHERE id = ?1",
                [mod_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "active");
        assert_eq!(active_profile_id(&conn).unwrap(), Some(profile_id));
        let events_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM install_event", [], |row| row.get(0))
            .unwrap();
        assert_eq!(events_after, events_before);

        let valid = switch(&conn, profile_id, &staging_root).unwrap();
        assert!(valid.enabled.is_empty());
        assert!(valid.disabled.is_empty());
        assert_eq!(std::fs::read(&target).unwrap(), b"current mod");
        assert_eq!(active_profile_id(&conn).unwrap(), Some(profile_id));
    }

    #[test]
    fn pending_profile_member_blocks_the_whole_switch_before_legacy_changes() {
        let conn = crate::db::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let outgoing = create(&conn, "outgoing").unwrap();
        let incoming = create(&conn, "incoming").unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status, mode) VALUES ('Legacy', 'asi', ?1, 'active', 'legacy-sp')",
            [dir.path().to_string_lossy().as_ref()],
        ).unwrap();
        let legacy_id = conn.last_insert_rowid();
        add_mod(&conn, outgoing, legacy_id).unwrap();
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status, mode) VALUES ('Pending', 'asi', ?1, 'disabled', 'fivem-client')",
            [dir.path().to_string_lossy().as_ref()],
        ).unwrap();
        add_mod(&conn, incoming, conn.last_insert_rowid()).unwrap();
        let result = switch(&conn, incoming, &dir.path().join("staging"));
        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(crate::error::CoreError::UnsupportedFormat { reason })
                if reason == "FiveM support is under development"
        ));
        let status: String = conn
            .query_row(
                "SELECT status FROM installed_mod WHERE id = ?1",
                [legacy_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "active");
        assert!(!dir.path().join("staging").exists());
    }

    fn insert_mod(conn: &Connection, name: &str, status: &str) -> i64 {
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status) \
             VALUES (?1, 'asi', '', ?2)",
            rusqlite::params![name, status],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    // OpenAI-Codex_lml_guard_audit: Observe only synthetic fixture trees.
    fn profile_preflight_tree(root: &Path) -> Vec<(std::path::PathBuf, Option<Vec<u8>>)> {
        let mut pending = vec![root.to_path_buf()];
        let mut entries = Vec::new();
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
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

    // OpenAI-Codex_lml_guard_audit: Capture synthetic rows without updating the database.
    fn profile_preflight_db(conn: &Connection) -> Vec<(String, Vec<Vec<rusqlite::types::Value>>)> {
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
                let query = format!(
                    "SELECT rowid, * FROM \"{}\" ORDER BY rowid",
                    table.replace('"', "\"\"")
                );
                let mut statement = conn.prepare(&query).unwrap();
                let columns = statement.column_count();
                let rows = statement
                    .query_map([], |row| {
                        (0..columns)
                            .map(|i| row.get::<_, rusqlite::types::Value>(i))
                            .collect::<rusqlite::Result<Vec<_>>>()
                    })
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap();
                (table, rows)
            })
            .collect()
    }

    fn profile_preflight_changes(conn: &Connection) -> i64 {
        conn.query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap()
    }

    fn assert_profile_staging_error(error: crate::error::CoreError, mod_id: i64, corrupt: bool) {
        match (corrupt, error) {
            (false, crate::error::CoreError::Io(source)) => {
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            (true, crate::error::CoreError::UnsupportedFormat { reason }) => {
                assert_eq!(reason, format!("disabled staging for mod {mod_id} is missing, unsafe, or does not match its file records; preserve it for recovery"));
            }
            (_, _) => panic!("unexpected staging error kind"),
        }
    }

    // OpenAI-Codex_lml_guard_audit: Keep the temporary directory alive for each case.
    struct ProfilePreflightFixture {
        temp: tempfile::TempDir,
        conn: Connection,
        staging: std::path::PathBuf,
        outgoing_id: i64,
        incoming_id: i64,
        outgoing_profile: i64,
        incoming_profile: i64,
        outgoing_target: std::path::PathBuf,
        outgoing_backup: std::path::PathBuf,
        incoming_alpha: std::path::PathBuf,
        incoming_bravo: std::path::PathBuf,
        incoming_backup: std::path::PathBuf,
    }

    fn profile_preflight_fixture() -> ProfilePreflightFixture {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        let staging = temp.path().join("staging");
        std::fs::create_dir(&game).unwrap();
        let outgoing_target = game.join("outgoing.asi");
        let outgoing_backup = temp.path().join("outgoing-original.bak");
        let incoming_alpha = game.join("alpha.asi");
        let incoming_bravo = game.join("bravo.asi");
        let incoming_backup = temp.path().join("alpha-original.bak");
        std::fs::write(&outgoing_target, b"outgoing payload").unwrap();
        std::fs::write(&outgoing_backup, b"outgoing original").unwrap();
        std::fs::write(&incoming_alpha, b"alpha original").unwrap();
        std::fs::write(&incoming_backup, b"alpha original").unwrap();

        let conn = crate::db::open_in_memory().unwrap();
        let game_text = game.to_str().expect("synthetic game root must be Unicode");
        conn.execute(
            "INSERT INTO installed_mod (name,source_type,install_path,status,mode) \
             VALUES ('Outgoing','asi',?1,'active','legacy-sp')",
            [game_text],
        )
        .unwrap();
        let outgoing_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO installed_mod (name,source_type,install_path,status,mode) \
             VALUES ('Incoming','asi',?1,'disabled','legacy-sp')",
            [game_text],
        )
        .unwrap();
        let incoming_id = conn.last_insert_rowid();
        for (id, target, backup, installed_payload) in [
            (
                outgoing_id,
                &outgoing_target,
                Some(&outgoing_backup),
                &b"outgoing payload"[..],
            ),
            (
                incoming_id,
                &incoming_alpha,
                Some(&incoming_backup),
                &b"alpha installed"[..],
            ),
            (incoming_id, &incoming_bravo, None, &b"bravo payload"[..]),
        ] {
            let backup_text = backup.map(|path| path.to_str().unwrap());
            let digest = owner_synthetic_hash(installed_payload);
            conn.execute(
                "INSERT INTO installed_mod_file (installed_mod_id,target_path,backup_path,file_hash) \
                 VALUES (?1,?2,?3,?4)",
                rusqlite::params![id, target.to_str().unwrap(), backup_text, digest]
            ).unwrap();
        }
        owner_seed_stage(
            &conn,
            &staging,
            incoming_id,
            &[&b"alpha edited before disable"[..], &b"bravo payload"[..]],
        );
        owner_seed_event(&conn, incoming_id, "disable");

        let outgoing_profile = create(&conn, "Outgoing profile").unwrap();
        let incoming_profile = create(&conn, "Incoming profile").unwrap();
        add_mod(&conn, outgoing_profile, outgoing_id).unwrap();
        add_mod(&conn, incoming_profile, incoming_id).unwrap();
        assert_eq!(
            conn.execute(
                "UPDATE user_settings SET active_profile_id=?1 WHERE id=1",
                [outgoing_profile]
            )
            .unwrap(),
            1
        );
        for id in [outgoing_id, incoming_id] {
            crate::providers::ensure_installed_mod_available(&conn, id).unwrap();
        }
        let stage = staging.join(incoming_id.to_string());
        assert!(stage.join("manifest.json").is_file());
        assert_eq!(
            crate::state::disabled_payloads(&conn, incoming_id, &staging)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            std::fs::read(stage.join("0")).unwrap(),
            b"alpha edited before disable"
        );
        assert_eq!(std::fs::read(stage.join("1")).unwrap(), b"bravo payload");
        assert_eq!(
            std::fs::read(&outgoing_target).unwrap(),
            b"outgoing payload"
        );
        assert_eq!(std::fs::read(&incoming_alpha).unwrap(), b"alpha original");
        assert!(!incoming_bravo.exists());
        assert!(!staging.join(outgoing_id.to_string()).exists());
        assert_eq!(active_profile_id(&conn).unwrap(), Some(outgoing_profile));
        ProfilePreflightFixture {
            temp,
            conn,
            staging,
            outgoing_id,
            incoming_id,
            outgoing_profile,
            incoming_profile,
            outgoing_target,
            outgoing_backup,
            incoming_alpha,
            incoming_bravo,
            incoming_backup,
        }
    }

    // OpenAI-Codex_lml_guard_audit: Check all incoming files before disabling outgoing mods.
    fn assert_profile_incoming_rejected(corrupt: bool) {
        let f = profile_preflight_fixture();
        let stage = f.staging.join(f.incoming_id.to_string());
        let second = stage.join("1");
        let saved = f.temp.path().join("saved-bravo.bin");
        let expected_hash = crate::util::hash_file(&second).unwrap();
        let manifest = std::fs::read(stage.join("manifest.json")).unwrap();
        std::fs::rename(&second, &saved).unwrap();
        if corrupt {
            std::fs::write(&second, b"corrupt bravo").unwrap();
            assert_ne!(crate::util::hash_file(&second).unwrap(), expected_hash);
        } else {
            assert!(!second.exists());
        }
        assert_eq!(std::fs::read(&saved).unwrap(), b"bravo payload");
        assert_eq!(
            std::fs::read(stage.join("manifest.json")).unwrap(),
            manifest
        );
        for id in [f.outgoing_id, f.incoming_id] {
            crate::providers::ensure_installed_mod_available(&f.conn, id).unwrap();
        }
        let observed = match crate::state::disabled_payloads(&f.conn, f.incoming_id, &f.staging) {
            Ok(_) => panic!("expected a staging rejection"),
            Err(error) => error,
        };
        assert_profile_staging_error(observed, f.incoming_id, corrupt);
        let tree_before = profile_preflight_tree(f.temp.path());
        let db_before = profile_preflight_db(&f.conn);
        let changes_before = profile_preflight_changes(&f.conn);

        let error = match switch(&f.conn, f.incoming_profile, &f.staging) {
            Ok(_) => panic!("expected profile switch rejection"),
            Err(error) => error,
        };

        assert_profile_staging_error(error, f.incoming_id, corrupt);
        assert!(
            profile_preflight_tree(f.temp.path()) == tree_before,
            "synthetic tree changed"
        );
        assert!(
            profile_preflight_db(&f.conn) == db_before,
            "synthetic database changed"
        );
        assert_eq!(profile_preflight_changes(&f.conn), changes_before);
        assert_eq!(
            active_profile_id(&f.conn).unwrap(),
            Some(f.outgoing_profile)
        );
        assert_eq!(
            std::fs::read(&f.outgoing_target).unwrap(),
            b"outgoing payload"
        );
        assert_eq!(
            std::fs::read(&f.outgoing_backup).unwrap(),
            b"outgoing original"
        );
        assert_eq!(std::fs::read(&f.incoming_alpha).unwrap(), b"alpha original");
        assert_eq!(
            std::fs::read(&f.incoming_backup).unwrap(),
            b"alpha original"
        );
        assert!(!f.incoming_bravo.exists());
        assert!(!f.staging.join(f.outgoing_id.to_string()).exists());
    }

    #[test]
    fn switch_rejects_missing_incoming_before_outgoing_changes() {
        assert_profile_incoming_rejected(false);
    }

    #[test]
    fn switch_rejects_corrupt_incoming_before_outgoing_changes() {
        assert_profile_incoming_rejected(true);
    }

    // OpenAI-Codex_lml_guard_audit: A valid two-file switch must remain available.
    #[test]
    fn switch_accepts_valid_incoming_and_preserves_backups() {
        let f = profile_preflight_fixture();
        add_mod(&f.conn, f.incoming_profile, f.outgoing_id).unwrap();
        let stage = f.staging.join(f.incoming_id.to_string());
        let manifest = std::fs::read(stage.join("manifest.json")).unwrap();
        let outgoing_rows = owner_self_rows(&f.conn, f.outgoing_id);
        let incoming_rows = owner_self_rows(&f.conn, f.incoming_id);
        let events_before = owner_events(&f.conn);
        let event_count = |id| -> i64 {
            f.conn
                .query_row(
                    "SELECT COUNT(*) FROM install_event WHERE installed_mod_id=?1",
                    [id],
                    |row| row.get(0),
                )
                .unwrap()
        };
        let outgoing_events = event_count(f.outgoing_id);
        let incoming_events = event_count(f.incoming_id);

        let outcome = switch(&f.conn, f.incoming_profile, &f.staging).unwrap();

        assert!(outcome.disabled.is_empty());
        assert_eq!(outcome.enabled, vec![f.incoming_id]);
        assert_eq!(
            active_profile_id(&f.conn).unwrap(),
            Some(f.incoming_profile)
        );
        for id in [f.outgoing_id, f.incoming_id] {
            assert_eq!(owner_status(&f.conn, id), "active");
        }
        assert_eq!(owner_self_rows(&f.conn, f.outgoing_id), outgoing_rows);
        assert_eq!(owner_self_rows(&f.conn, f.incoming_id), incoming_rows);
        assert_eq!(event_count(f.outgoing_id), outgoing_events);
        assert_eq!(event_count(f.incoming_id), incoming_events + 1);
        let events_after = owner_events(&f.conn);
        assert_eq!(events_after.len(), events_before.len() + 1);
        assert_eq!(
            &events_after[..events_before.len()],
            events_before.as_slice()
        );
        let event = events_after.last().unwrap();
        assert_eq!(event.1, Some(f.incoming_id));
        assert_eq!(event.2.as_str(), "enable");
        assert_eq!(event.4, 1);
        assert!(event.5.is_none());
        assert_eq!(
            std::fs::read(&f.outgoing_target).unwrap(),
            b"outgoing payload"
        );
        assert_eq!(
            std::fs::read(&f.outgoing_backup).unwrap(),
            b"outgoing original"
        );
        assert!(
            matches!(std::fs::symlink_metadata(f.staging.join(f.outgoing_id.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound)
        );
        assert_eq!(
            std::fs::read(&f.incoming_alpha).unwrap(),
            b"alpha edited before disable"
        );
        assert_eq!(std::fs::read(&f.incoming_bravo).unwrap(), b"bravo payload");
        assert_eq!(
            std::fs::read(&f.incoming_backup).unwrap(),
            b"alpha original"
        );
        assert!(!stage.join("0").exists());
        assert!(!stage.join("1").exists());
        assert_eq!(
            std::fs::read(stage.join("manifest.json")).unwrap(),
            manifest
        );
    }

    #[test]
    fn create_list_and_delete_roundtrip() {
        let conn = crate::db::open_in_memory().unwrap();
        let id = create(&conn, "Roleplay").unwrap();
        let profiles = list(&conn).unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].id, id);
        assert_eq!(profiles[0].name, "Roleplay");
        assert!(!profiles[0].is_active);

        delete(&conn, id).unwrap();
        assert!(list(&conn).unwrap().is_empty());
    }

    #[test]
    fn switch_requiring_disable_rejects_before_enabling_incoming() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let staging_root = dir.path().join("staging");

        let conn = crate::db::open_in_memory().unwrap();

        // A mod belonging to profile A, currently active.
        let mod_a = insert_mod(&conn, "ModA", "active");
        conn.execute(
            "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) \
             VALUES (?1, ?2, 'hash')",
            rusqlite::params![mod_a, game_root.join("a.asi").to_string_lossy()],
        )
        .unwrap();
        std::fs::write(game_root.join("a.asi"), b"a").unwrap();

        // A mod belonging to profile B, currently disabled.
        let mod_b = insert_mod(&conn, "ModB", "disabled");
        conn.execute(
            "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) \
             VALUES (?1, ?2, 'hash')",
            rusqlite::params![mod_b, game_root.join("b.asi").to_string_lossy()],
        )
        .unwrap();
        std::fs::create_dir_all(staging_root.join(mod_b.to_string())).unwrap();
        std::fs::write(staging_root.join(mod_b.to_string()).join("0"), b"b").unwrap();
        // OpenAI-Codex_lml_guard_review: Prepare verifiable synthetic staging.
        assert_eq!(
            conn.execute(
                "UPDATE installed_mod SET install_path = ?1 WHERE id IN (?2, ?3)",
                rusqlite::params![
                    game_root
                        .to_str()
                        .expect("synthetic game root must be Unicode"),
                    mod_a,
                    mod_b,
                ],
            )
            .unwrap(),
            2
        );
        let staged_b = staging_root.join(mod_b.to_string()).join("0");
        // OpenAI-Codex_disable_core_audit: Verify no-manifest staging.
        assert!(matches!(
            std::fs::symlink_metadata(staging_root.join(mod_b.to_string()).join("manifest.json")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        ));
        let staged_hash = crate::util::hash_file(&staged_b).unwrap();
        assert_eq!(
            conn.execute(
                "UPDATE installed_mod_file SET file_hash = ?1 WHERE installed_mod_id = ?2",
                rusqlite::params![staged_hash, mod_b],
            )
            .unwrap(),
            1
        );
        for mod_id in [mod_a, mod_b] {
            crate::providers::ensure_installed_mod_available(&conn, mod_id).unwrap();
        }
        assert_eq!(
            crate::state::disabled_payloads(&conn, mod_b, &staging_root)
                .unwrap()
                .len(),
            1
        );

        let profile_a = create(&conn, "A").unwrap();
        add_mod(&conn, profile_a, mod_a).unwrap();
        let profile_b = create(&conn, "B").unwrap();
        add_mod(&conn, profile_b, mod_b).unwrap();

        let tree_before = profile_preflight_tree(dir.path());
        let db_before = profile_preflight_db(&conn);
        let changes_before = profile_preflight_changes(&conn);
        let active_before = active_profile_id(&conn).unwrap();
        let error = switch(&conn, profile_b, &staging_root).unwrap_err();
        assert!(matches!(
            error,
            crate::error::CoreError::ModDisablingUnavailable
        ));
        assert_eq!(profile_preflight_tree(dir.path()), tree_before);
        assert_eq!(profile_preflight_db(&conn), db_before);
        assert_eq!(profile_preflight_changes(&conn), changes_before);
        assert_eq!(active_profile_id(&conn).unwrap(), active_before);
        assert_eq!(owner_status(&conn, mod_a), "active");
        assert_eq!(owner_status(&conn, mod_b), "disabled");
        assert_eq!(std::fs::read(game_root.join("a.asi")).unwrap(), b"a");
        assert!(!game_root.join("b.asi").exists());
        assert_eq!(std::fs::read(&staged_b).unwrap(), b"b");
        let profiles = list(&conn).unwrap();
        let b = profiles.iter().find(|p| p.id == profile_b).unwrap();
        assert!(!b.is_active);
    }

    #[test]
    fn deleting_the_currently_active_profile_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let staging_root = dir.path().join("staging");
        let conn = crate::db::open_in_memory().unwrap();

        let profile_id = create(&conn, "Roleplay").unwrap();
        switch(&conn, profile_id, &staging_root).unwrap();
        assert!(list(&conn).unwrap()[0].is_active);

        // Before the fix, this failed its foreign key constraint (active_profile_id
        // still pointed at the row being deleted) instead of succeeding or erroring
        // clearly.
        delete(&conn, profile_id).unwrap();
        assert!(list(&conn).unwrap().is_empty());
    }

    #[test]
    fn export_then_import_matches_locally_installed_mods_by_name() {
        let conn = crate::db::open_in_memory().unwrap();
        let mod_a = insert_mod(&conn, "ModA", "active");
        let profile_id = create(&conn, "Original").unwrap();
        add_mod(&conn, profile_id, mod_a).unwrap();

        let exported = export(&conn, profile_id).unwrap();
        assert_eq!(exported.name, "Original");
        assert_eq!(exported.mod_names, vec!["ModA".to_string()]);

        // Simulate importing on a machine that also has ModA installed, but not ModB.
        let mut for_import = exported.clone();
        for_import.mod_names.push("ModB".to_string());
        let outcome = import(&conn, &for_import).unwrap();
        assert_eq!(outcome.matched, vec!["ModA".to_string()]);
        assert_eq!(outcome.not_found_locally, vec!["ModB".to_string()]);

        let imported_mods = mod_ids_in_profile(&conn, outcome.profile_id).unwrap();
        assert_eq!(imported_mods, vec![mod_a]);
    }
}
