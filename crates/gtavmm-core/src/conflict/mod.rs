// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex
// OpenAI-Codex_Responsible: Source_Code

//! Compares a planned install's target files against existing `InstalledModFile` rows
//! (the "who owns which file" ownership tracking that lets the core distinguish, e.g.,
//! a Legacy SP mod's own file from a same-path file owned by a different mod/provider).
//!
//! Classifies each overlap per the MVP spec's three-tier policy:
//! - **Protected hit** — target matches `protected_files` → always fatal, no override,
//!   checked independently of everything else below.
//! - **Self-update** — the planned targets overlap mostly with one *existing active*
//!   mod's own files (overlap ratio ≥ [`SELF_UPDATE_OVERLAP_THRESHOLD`]) → suggested,
//!   not forced; the overlap still requires explicit confirmation.
//! - **Foreign conflict** — a planned target matches a file owned by a *different*
//!   active mod → requires explicit override from the caller before `install`
//!   proceeds.
//! - **Untracked existing file** — a target exists without a recorded active owner;
//!   it also requires confirmation, with no fabricated owner ID.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::error::{CoreError, CoreResult};
use crate::protected_files;

/// Overlap ratio (matching files / existing mod's total files) at or above which we
/// suggest "this looks like an update to an existing mod" rather than treating it as
/// an unrelated new install.
///
/// This is a suggestion only (see the MVP spec, section 4.2) — a human confirms it
/// before anything happens. One known ambiguity this doesn't try to resolve: if an
/// *unrelated* existing mod happens to own exactly one file and the new install's
/// only target coincidentally matches it, the ratio is 1.0 and this would suggest
/// "update" for a mod that isn't actually related. That's an acceptable tradeoff
/// precisely because the suggestion always goes through human review — it never
/// silently reclassifies a foreign conflict as safe.
pub const SELF_UPDATE_OVERLAP_THRESHOLD: f64 = 0.5;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ProtectedHit {
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ForeignConflict {
    /// None when the target exists but has no active owner recorded by this app.
    pub owner_mod_id: Option<i64>,
    pub owner_name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SelfUpdateSuggestion {
    pub existing_mod_id: i64,
    pub existing_name: String,
    pub overlap_ratio: f64,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct ConflictReport {
    pub protected_hits: Vec<ProtectedHit>,
    pub foreign_conflicts: Vec<ForeignConflict>,
    pub self_update: Option<SelfUpdateSuggestion>,
}

impl ConflictReport {
    /// `true` if `install` must refuse to proceed without the caller supplying an
    /// explicit override (protected hits are never overridable and aren't included in
    /// this check — see [`ConflictReport::has_protected_hits`]).
    pub fn requires_explicit_override(&self) -> bool {
        !self.foreign_conflicts.is_empty()
    }

    pub fn has_protected_hits(&self) -> bool {
        !self.protected_hits.is_empty()
    }
}

/// One row of `installed_mod_file` joined with its owning mod, for active mods only.
struct ExistingFile {
    mod_id: i64,
    mod_name: String,
    target_path: String,
}

fn load_active_files(conn: &Connection) -> CoreResult<Vec<ExistingFile>> {
    let mut stmt = conn.prepare(
        "SELECT m.id, m.name, f.target_path \
         FROM installed_mod_file f \
         JOIN installed_mod m ON m.id = f.installed_mod_id \
         WHERE m.status = 'active'",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ExistingFile {
            mod_id: row.get(0)?,
            mod_name: row.get(1)?,
            target_path: row.get(2)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// OpenAI-Codex: Compare existing aliases consistently while retaining display paths.
fn comparison_key(path: &Path) -> PathBuf {
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

/// Analyzes `planned_targets` (absolute paths this install would write) against the
/// database's current active-mod file ownership.
pub fn analyze(conn: &Connection, planned_targets: &[PathBuf]) -> CoreResult<ConflictReport> {
    let mut report = ConflictReport::default();

    for target in planned_targets {
        if protected_files::is_protected(target) {
            report.protected_hits.push(ProtectedHit {
                path: target.clone(),
            });
        }
    }

    let existing = load_active_files(conn)?;
    let tracked_targets: std::collections::HashSet<PathBuf> = existing
        .iter()
        .map(|file| comparison_key(Path::new(&file.target_path)))
        .collect();
    let mut untracked_targets = std::collections::HashSet::new();
    for target in planned_targets {
        if protected_files::is_protected(target) {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(target) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !metadata.file_type().is_file() {
            return Err(CoreError::UnsupportedFormat {
                reason: format!("install target is not a regular file: {}", target.display()),
            });
        }
        let key = comparison_key(target);
        if !tracked_targets.contains(&key) && untracked_targets.insert(key) {
            report.foreign_conflicts.push(ForeignConflict {
                owner_mod_id: None,
                owner_name: "Unknown (existing file not tracked by this app)".to_string(),
                path: target.clone(),
            });
        }
    }
    let planned_set: std::collections::HashSet<PathBuf> = planned_targets
        .iter()
        .map(|path| comparison_key(path))
        .collect();

    // Group existing files by owning mod so we can compute a per-mod overlap ratio.
    let mut by_mod: HashMap<i64, (String, Vec<String>)> = HashMap::new();
    for file in &existing {
        by_mod
            .entry(file.mod_id)
            .or_insert_with(|| (file.mod_name.clone(), Vec::new()))
            .1
            .push(file.target_path.clone());
    }

    let mut best_suggestion: Option<SelfUpdateSuggestion> = None;
    for (mod_id, (name, paths)) in &by_mod {
        if paths.is_empty() {
            continue;
        }
        let overlap_count = paths
            .iter()
            .filter(|p| planned_set.contains(&comparison_key(Path::new(p.as_str()))))
            .count();
        let ratio = overlap_count as f64 / paths.len() as f64;
        if ratio >= SELF_UPDATE_OVERLAP_THRESHOLD {
            let is_better = best_suggestion
                .as_ref()
                .is_none_or(|current| ratio > current.overlap_ratio);
            if is_better {
                best_suggestion = Some(SelfUpdateSuggestion {
                    existing_mod_id: *mod_id,
                    existing_name: name.clone(),
                    overlap_ratio: ratio,
                });
            }
        }
    }
    report.self_update = best_suggestion;

    for file in &existing {
        // OpenAI-Codex: A similarity hint is not proof of ownership or consent.
        if planned_set.contains(&comparison_key(Path::new(file.target_path.as_str()))) {
            report.foreign_conflicts.push(ForeignConflict {
                owner_mod_id: Some(file.mod_id),
                owner_name: file.mod_name.clone(),
                path: PathBuf::from(&file.target_path),
            });
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn symbolic_link_target_is_rejected_before_overwriting_its_destination() {
        let conn = crate::db::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.asi");
        let target = dir.path().join("link.asi");
        std::fs::write(&original, b"unchanged").unwrap();
        std::os::unix::fs::symlink(&original, &target).unwrap();
        assert!(analyze(&conn, &[target]).is_err());
        assert_eq!(std::fs::read(original).unwrap(), b"unchanged");
    }

    #[test]
    fn existing_untracked_file_requires_confirmation_without_an_owner_id() {
        let conn = crate::db::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("existing.asi");
        std::fs::write(&target, b"manual installation").unwrap();
        let report = analyze(&conn, &[target.clone(), target.clone()]).unwrap();
        assert!(report.requires_explicit_override());
        assert_eq!(report.foreign_conflicts.len(), 1);
        assert_eq!(report.foreign_conflicts[0].owner_mod_id, None);
        assert_eq!(report.foreign_conflicts[0].path, target);
    }

    #[test]
    fn known_owner_is_not_also_reported_as_untracked() {
        let conn = crate::db::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("existing.asi");
        std::fs::write(&target, b"existing").unwrap();
        let owner = insert_active_mod(&conn, "Existing", &[target.to_str().unwrap()]);
        let report = analyze(&conn, &[target]).unwrap();
        assert_eq!(report.foreign_conflicts.len(), 1);
        assert_eq!(report.foreign_conflicts[0].owner_mod_id, Some(owner));
    }

    #[test]
    fn existing_directory_is_not_an_overridable_file_conflict() {
        let conn = crate::db::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        assert!(analyze(&conn, &[dir.path().to_path_buf()]).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_case_alias_of_existing_target_is_a_conflict() {
        let conn = crate::db::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("Shared.asi");
        std::fs::write(&existing, b"existing").unwrap();
        let owner = insert_active_mod(&conn, "Existing", &[existing.to_str().unwrap()]);
        let alias = dir.path().join("shared.ASI");
        let report = analyze(&conn, &[alias]).unwrap();
        assert!(report.requires_explicit_override());
        assert_eq!(report.foreign_conflicts[0].owner_mod_id, Some(owner));
    }

    #[test]
    fn unrelated_single_file_overlap_requires_confirmation_despite_update_hint() {
        let conn = crate::db::open_in_memory().unwrap();
        let owner = insert_active_mod(&conn, "Unrelated original", &["/game/shared.asi"]);
        let report = analyze(&conn, &[PathBuf::from("/game/shared.asi")]).unwrap();
        assert_eq!(report.self_update.as_ref().unwrap().existing_mod_id, owner);
        assert!(report.requires_explicit_override());
        assert_eq!(report.foreign_conflicts.len(), 1);
        assert_eq!(report.foreign_conflicts[0].owner_mod_id, Some(owner));
    }

    fn insert_active_mod(conn: &Connection, name: &str, files: &[&str]) -> i64 {
        conn.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status) \
             VALUES (?1, 'asi', '', 'active')",
            [name],
        )
        .unwrap();
        let mod_id = conn.last_insert_rowid();
        for file in files {
            conn.execute(
                "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) \
                 VALUES (?1, ?2, 'deadbeef')",
                rusqlite::params![mod_id, file],
            )
            .unwrap();
        }
        mod_id
    }

    #[test]
    fn no_conflict_on_fresh_targets() {
        let conn = crate::db::open_in_memory().unwrap();
        let report = analyze(&conn, &[PathBuf::from("/game/new_mod.asi")]).unwrap();
        assert!(report.protected_hits.is_empty());
        assert!(report.foreign_conflicts.is_empty());
        assert!(report.self_update.is_none());
    }

    #[test]
    fn protected_file_hit_is_always_flagged() {
        // Built with `.join`, not a hand-formatted backslash string — see
        // `protected_files::tests::game_path`'s comment for why that matters on Linux.
        let conn = crate::db::open_in_memory().unwrap();
        let path = PathBuf::from("game").join("GTA5.exe");
        let report = analyze(&conn, &[path]).unwrap();
        assert_eq!(report.protected_hits.len(), 1);
    }

    #[test]
    fn high_overlap_with_existing_mod_suggests_self_update() {
        let conn = crate::db::open_in_memory().unwrap();
        insert_active_mod(&conn, "CoolMod", &["/game/a.asi", "/game/b.dll"]);

        let planned = vec![PathBuf::from("/game/a.asi"), PathBuf::from("/game/b.dll")];
        let report = analyze(&conn, &planned).unwrap();

        let suggestion = report
            .self_update
            .clone()
            .expect("expected a self-update suggestion");
        assert_eq!(suggestion.existing_name, "CoolMod");
        assert_eq!(suggestion.overlap_ratio, 1.0);
        assert_eq!(report.foreign_conflicts.len(), 2);
        assert!(report.requires_explicit_override());
    }

    #[test]
    fn low_overlap_with_different_mod_is_foreign_conflict() {
        let conn = crate::db::open_in_memory().unwrap();
        insert_active_mod(
            &conn,
            "OtherMod",
            &[
                "/game/shared.dll",
                "/game/other1.asi",
                "/game/other2.asi",
                "/game/other3.asi",
            ],
        );

        // Only one of OtherMod's four files overlaps -> ratio 0.25, below threshold.
        let planned = vec![
            PathBuf::from("/game/shared.dll"),
            PathBuf::from("/game/new.asi"),
        ];
        let report = analyze(&conn, &planned).unwrap();

        assert!(report.self_update.is_none());
        assert_eq!(report.foreign_conflicts.len(), 1);
        assert_eq!(report.foreign_conflicts[0].owner_name, "OtherMod");
        assert!(report.requires_explicit_override());
    }

    #[test]
    fn disabled_mods_do_not_count_as_active_conflicts() {
        let conn = crate::db::open_in_memory().unwrap();
        insert_active_mod(&conn, "WillBeDisabled", &["/game/a.asi"]);
        conn.execute("UPDATE installed_mod SET status = 'disabled'", [])
            .unwrap();

        let report = analyze(&conn, &[PathBuf::from("/game/a.asi")]).unwrap();
        assert!(report.foreign_conflicts.is_empty());
        assert!(report.self_update.is_none());
    }
}
