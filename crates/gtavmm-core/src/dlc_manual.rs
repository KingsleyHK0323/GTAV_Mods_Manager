// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex
// OpenAI-Codex_dlc_deep_audit

//! Persistent, append-only reminders. This module never reads or edits dlclist XML.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use rusqlite::{params, Connection, Transaction, TransactionBehavior};
use serde::Serialize;

use crate::error::{CoreError, CoreResult};

#[derive(Debug, Clone, Copy)]
pub enum ManualDlcOperation {
    Install,
    Uninstall,
    Restore,
    Reinstall,
}

impl ManualDlcOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Uninstall => "uninstall",
            Self::Restore => "restore",
            Self::Reinstall => "reinstall",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ManualDlcOperationToken {
    operation_id: Option<i64>,
    root_key: String,
    root_display: String,
    packs: Vec<(String, String)>,
    operation: ManualDlcOperation,
    mod_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ManualDlcTask {
    pub id: i64,
    pub root_path: String,
    pub pack_name: String,
    pub item_xml: String,
    pub operation: String,
    pub recommendation: String,
    pub owner_mod_ids: Vec<i64>,
    pub unresolved: bool,
}

fn invalid(reason: &str) -> CoreError {
    CoreError::UnsupportedFormat {
        reason: format!("manual DLC work: {reason}"),
    }
}

fn text(path: &Path) -> CoreResult<&str> {
    path.to_str()
        .ok_or_else(|| invalid("path is not valid Unicode"))
}

fn folded(value: &str) -> String {
    #[cfg(windows)]
    {
        value.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        value.to_string()
    }
}

fn lexical_key(path: &Path) -> CoreResult<String> {
    // Normalize lexical aliases before checking the declared-root boundary and
    // exact XML targets. ParentDir is deliberately retained for caller rejection.
    let normalized_path: PathBuf = path
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect();
    let original = text(&normalized_path)?;
    #[cfg(windows)]
    let normalized = if let Some(unc) = original.strip_prefix(r"\\?\UNC\") {
        format!("//{}", unc.replace('\\', "/"))
    } else {
        original
            .strip_prefix(r"\\?\")
            .unwrap_or(original)
            .replace('\\', "/")
    };
    #[cfg(not(windows))]
    let normalized = original.to_string();
    Ok(folded(normalized.trim_end_matches('/')))
}

fn safe_resolved(path: &Path) -> CoreResult<PathBuf> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(invalid("an absolute path without traversal is required"));
    }
    let mut missing = Vec::new();
    let mut existing = path;
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(
                    existing
                        .file_name()
                        .ok_or_else(|| invalid("path has no existing ancestor"))?
                        .to_owned(),
                );
                existing = existing
                    .parent()
                    .ok_or_else(|| invalid("path has no parent"))?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    for ancestor in existing.ancestors() {
        let metadata = std::fs::symlink_metadata(ancestor)?;
        let linked = metadata.file_type().is_symlink();
        #[cfg(windows)]
        let linked =
            linked || std::os::windows::fs::MetadataExt::file_attributes(&metadata) & 0x400 != 0;
        if linked {
            return Err(invalid(
                "linked or reparse paths cannot identify managed DLC work",
            ));
        }
    }
    if !missing.is_empty() && !std::fs::metadata(existing)?.is_dir() {
        return Err(invalid("a missing descendant has a non-directory ancestor"));
    }
    let mut result = std::fs::canonicalize(existing)?;
    for component in missing.into_iter().rev() {
        result.push(component);
    }
    Ok(result)
}

fn validated_pack(pack: &str) -> CoreResult<String> {
    if pack.is_empty()
        || pack == "."
        || pack == ".."
        || pack.ends_with('.')
        || pack.ends_with(' ')
        || pack.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
    {
        return Err(invalid("pack name is not a single safe path component"));
    }
    Ok(folded(pack))
}

fn looks_managed(path: &Path) -> CoreResult<bool> {
    let components = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(p) => Some(p.to_str()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| invalid("target path is not valid Unicode"))?;
    Ok(components.windows(4).any(|p| {
        p.iter()
            .zip(["mods", "update", "x64", "dlcpacks"])
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
    }))
}

/// Derive names only from the complete managed prefix, never arbitrary dlcpacks components.
pub fn pack_names_for_paths(root: &Path, paths: &[PathBuf]) -> CoreResult<Vec<String>> {
    if root.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(invalid("game root cannot contain traversal"));
    }
    let mut packs = BTreeMap::new();
    let lexical_prefix = format!("{}/", lexical_key(root)?);
    let mut candidates = Vec::new();
    for path in paths {
        if path.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(invalid("payload targets cannot contain traversal"));
        }
        let key = lexical_key(path)?;
        if let Some(relative) = key.strip_prefix(&lexical_prefix) {
            let parts: Vec<_> = relative.split('/').filter(|p| *p != ".").collect();
            let normalized = parts.join("/");
            if [
                "update/update.rpf/common/data/dlclist.xml",
                "mods/update/update.rpf/common/data/dlclist.xml",
            ]
            .iter()
            .any(|p| normalized.eq_ignore_ascii_case(p))
            {
                return Err(invalid("dlclist XML payloads require manual editing"));
            }
            // A file directly below dlcpacks is an ordinary mirrored RPF, not
            // a pack directory plus its contained payload filename.
            if parts.len() >= 6
                && parts[..4]
                    .iter()
                    .zip(["mods", "update", "x64", "dlcpacks"])
                    .all(|(a, b)| a.eq_ignore_ascii_case(b))
            {
                candidates.push(path);
            }
        } else if looks_managed(path)? {
            return Err(invalid("managed target is outside its declared game root"));
        }
    }
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let root_key = lexical_key(&safe_resolved(root)?)?;
    let prefix = format!("{root_key}/");
    for path in candidates {
        let resolved = safe_resolved(path)?;
        let key = lexical_key(&resolved)?;
        let relative = key
            .strip_prefix(&prefix)
            .ok_or_else(|| invalid("managed target escapes the game root"))?;
        let parts: Vec<_> = relative.split('/').collect();
        if parts.len() < 6
            || !parts[..4]
                .iter()
                .zip(["mods", "update", "x64", "dlcpacks"])
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
        {
            return Err(invalid(
                "managed target does not have the complete DLC prefix and filename",
            ));
        }
        // Preserve the original spelling from the resolved path for display/XML.
        let display_parts = resolved
            .components()
            .filter_map(|c| {
                if let Component::Normal(p) = c {
                    Some(p.to_str())
                } else {
                    None
                }
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| invalid("target path is not valid Unicode"))?;
        let pack = display_parts[display_parts.len() - (parts.len() - 4)];
        packs
            .entry(validated_pack(pack)?)
            .or_insert_with(|| pack.to_string());
    }
    Ok(packs.into_values().collect())
}

pub fn pack_names_for_mod(conn: &Connection, mod_id: i64, root: &Path) -> CoreResult<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT target_path FROM installed_mod_file WHERE installed_mod_id=?1 ORDER BY id",
    )?;
    let paths = stmt
        .query_map([mod_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    pack_names_for_paths(root, &paths)
}

/// Persist all intent events before callers start any payload mutation.
pub fn begin_tasks(
    conn: &mut Connection,
    root: &Path,
    packs: &[String],
    operation: ManualDlcOperation,
    mod_id: Option<i64>,
) -> CoreResult<ManualDlcOperationToken> {
    let mut token = ManualDlcOperationToken {
        operation_id: None,
        root_key: String::new(),
        root_display: String::new(),
        packs: Vec::new(),
        operation,
        mod_id,
    };
    if packs.is_empty() {
        return Ok(token);
    }
    token.root_key = lexical_key(&safe_resolved(root)?)?;
    token.root_display = text(root)?.to_string();
    let mut unique = BTreeMap::new();
    for pack in packs {
        unique
            .entry(validated_pack(pack)?)
            .or_insert_with(|| pack.clone());
    }
    token.packs = unique.into_iter().collect();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let highest: i64 = tx.query_row(
        "SELECT COALESCE(MAX(id),0) FROM manual_dlc_event",
        [],
        |r| r.get(0),
    )?;
    let operation_id = highest
        .checked_add(1)
        .ok_or_else(|| invalid("event sequence exhausted"))?;
    token.operation_id = Some(operation_id);
    for (index, (key, display)) in token.packs.iter().enumerate() {
        let id = operation_id
            .checked_add(index as i64)
            .ok_or_else(|| invalid("event sequence exhausted"))?;
        tx.execute("INSERT INTO manual_dlc_event(id,operation_id,root_key,root_display,archive_scope,pack_key,pack_display,operation,phase,installed_mod_id) VALUES(?1,?2,?3,?4,'dlclist',?5,?6,?7,'intent',?8)",
            params![id, operation_id, token.root_key, token.root_display, key, display, operation.as_str(), mod_id])?;
    }
    tx.commit()?;
    Ok(token)
}

/// Append settlement inside the caller's existing successful status/event transaction.
pub fn settle_tasks(tx: &Transaction<'_>, token: &ManualDlcOperationToken) -> CoreResult<()> {
    let Some(operation_id) = token.operation_id else {
        return Ok(());
    };
    for (key, display) in &token.packs {
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM manual_dlc_event WHERE operation_id=?1 AND root_key=?2 AND archive_scope='dlclist' AND pack_key=?3 AND phase='intent' AND operation=?4 AND installed_mod_id IS ?5", params![operation_id, token.root_key, key, token.operation.as_str(), token.mod_id], |r| r.get(0))?;
        if count != 1 {
            return Err(invalid("operation token has no unique persisted intent"));
        }
        tx.execute("INSERT INTO manual_dlc_event(operation_id,root_key,root_display,archive_scope,pack_key,pack_display,operation,phase,installed_mod_id) VALUES(?1,?2,?3,'dlclist',?4,?5,?6,'settled',?7)",
            params![operation_id, token.root_key, token.root_display, key, display, token.operation.as_str(), token.mod_id])?;
    }
    Ok(())
}

fn item_xml(pack: &str) -> String {
    let escaped = pack
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;");
    format!("<Item>dlcpacks:\\{escaped}\\</Item>")
}

/// Recompute conservative recommendations without updating or resolving any event.
pub fn list_tasks(conn: &Connection, root: Option<&Path>) -> CoreResult<Vec<ManualDlcTask>> {
    // Keep ledger and owner reads in one SQLite snapshot, including when the
    // caller already owns a transaction. Never publish a mixed-state removal hint.
    if !conn.is_autocommit() {
        return list_tasks_in_snapshot(conn, root);
    }
    let tx = conn.unchecked_transaction()?;
    let tasks = list_tasks_in_snapshot(&tx, root)?;
    tx.commit()?;
    Ok(tasks)
}

fn list_tasks_in_snapshot(
    conn: &Connection,
    root: Option<&Path>,
) -> CoreResult<Vec<ManualDlcTask>> {
    let filter = root
        .map(|p| safe_resolved(p).and_then(|p| lexical_key(&p)))
        .transpose()?;
    let mut stmt = conn.prepare("SELECT id,operation_id,root_key,root_display,pack_key,pack_display,operation,phase,installed_mod_id FROM manual_dlc_event WHERE archive_scope='dlclist' ORDER BY id")?;
    let events = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, Option<i64>>(8)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut tasks = BTreeMap::new();
    let mut unresolved = BTreeSet::new();
    for (id, operation_id, root_key, display, pack_key, pack, operation, phase, mod_id) in events {
        if filter.as_ref().is_some_and(|f| f != &root_key) {
            continue;
        }
        let intent_key = (operation_id, root_key.clone(), pack_key.clone());
        if phase == "intent" {
            unresolved.insert(intent_key);
        } else {
            unresolved.remove(&intent_key);
        }
        tasks.insert((root_key, pack_key), (id, display, pack, operation, mod_id));
    }
    let mut owners = conn.prepare("SELECT m.id,m.install_path,m.status,f.target_path FROM installed_mod m LEFT JOIN installed_mod_file f ON f.installed_mod_id=m.id WHERE m.status IN ('active','disabled') ORDER BY m.id,f.id")?;
    let owners = owners
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut result = Vec::new();
    for ((root_key, pack_key), (id, display, pack, operation, source_mod)) in tasks {
        validated_pack(&pack)?;
        let unresolved = unresolved
            .iter()
            .any(|(_, r, p)| r == &root_key && p == &pack_key);
        let root = Path::new(&display);
        let trusted = root.is_dir()
            && safe_resolved(root)
                .and_then(|p| lexical_key(&p))
                .is_ok_and(|k| k == root_key);
        let mut uncertain = !trusted;
        let mut ids = BTreeSet::new();
        let mut disabled = false;
        for (owner_id, owner_root, status, path) in &owners {
            let same_root = safe_resolved(Path::new(owner_root))
                .and_then(|p| lexical_key(&p))
                .is_ok_and(|k| k == root_key);
            let target_here = path
                .as_ref()
                .and_then(|p| lexical_key(Path::new(p)).ok())
                .is_some_and(|p| p.starts_with(&format!("{root_key}/")));
            if !same_root && !target_here {
                continue;
            }
            let Some(path) = path else {
                uncertain = true;
                continue;
            };
            match pack_names_for_paths(root, &[PathBuf::from(path)]) {
                Ok(packs)
                    if packs
                        .iter()
                        .any(|p| validated_pack(p).is_ok_and(|k| k == pack_key)) =>
                {
                    ids.insert(*owner_id);
                    disabled |= status == "disabled";
                    uncertain |= !same_root;
                }
                Ok(_) => (),
                Err(_) => uncertain = true,
            }
        }
        let owner_mod_ids = ids.into_iter().collect::<Vec<_>>();
        let recommendation = if unresolved
            || uncertain
            || disabled
            || owner_mod_ids.len() > 1
            || source_mod.is_some_and(|id| !owner_mod_ids.is_empty() && owner_mod_ids[0] != id)
        {
            "review"
        } else if owner_mod_ids.len() == 1 {
            "check_present"
        } else if operation == "uninstall" || operation == "reinstall" {
            "check_removal"
        } else {
            "review"
        };
        result.push(ManualDlcTask {
            id,
            root_path: display,
            pack_name: pack.clone(),
            item_xml: item_xml(&pack),
            operation,
            recommendation: recommendation.to_string(),
            owner_mod_ids,
            unresolved,
        });
    }
    result.sort_by_key(|task| task.id);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(conn: &Connection, root: &Path, pack: &str, status: &str, mode: &str) -> i64 {
        conn.execute("INSERT INTO installed_mod(name,source_type,install_path,status,mode) VALUES('pack','folder',?1,?2,?3)",
            params![root.to_str().unwrap(), status, mode]).unwrap();
        let id = conn.last_insert_rowid();
        conn.execute("INSERT INTO installed_mod_file(installed_mod_id,target_path,file_hash) VALUES(?1,?2,'hash')",
            params![id, root.join("mods/update/x64/dlcpacks").join(pack).join("dlc.rpf").to_str().unwrap()]).unwrap();
        id
    }

    #[test]
    fn empty_work_does_not_require_a_root_or_write_events() {
        let mut conn = crate::db::open_in_memory().unwrap();
        assert!(
            pack_names_for_paths(Path::new("missing"), &[PathBuf::from("other/file.asi")])
                .unwrap()
                .is_empty()
        );
        let token = begin_tasks(
            &mut conn,
            Path::new("missing"),
            &[],
            ManualDlcOperation::Install,
            None,
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        settle_tasks(&tx, &token).unwrap();
        tx.commit().unwrap();
        assert!(list_tasks(&conn, None).unwrap().is_empty());
    }

    #[test]
    fn managed_words_inside_root_do_not_classify_an_ordinary_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("mods/update/x64/dlcpacks/game");
        assert!(pack_names_for_paths(&root, &[root.join("ordinary.asi")])
            .unwrap()
            .is_empty());
        let outside = dir.path().join("mods/update/x64/dlcpacks/Other/dlc.rpf");
        assert!(pack_names_for_paths(&root, &[outside]).is_err());
    }

    #[test]
    fn restart_preserves_unresolved_work_and_later_success_cannot_hide_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.sqlite3");
        let packs = vec!["Car".to_string()];
        {
            let mut conn = crate::db::open(&path).unwrap();
            begin_tasks(
                &mut conn,
                dir.path(),
                &packs,
                ManualDlcOperation::Install,
                None,
            )
            .unwrap();
            let token = begin_tasks(
                &mut conn,
                dir.path(),
                &packs,
                ManualDlcOperation::Restore,
                None,
            )
            .unwrap();
            let tx = conn.transaction().unwrap();
            settle_tasks(&tx, &token).unwrap();
            tx.commit().unwrap();
        }
        let conn = crate::db::open(&path).unwrap();
        let tasks = list_tasks(&conn, Some(dir.path())).unwrap();
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].unresolved);
        assert_eq!(tasks[0].recommendation, "review");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM manual_dlc_event", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn rolled_back_settlement_preserves_intent() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let token = begin_tasks(
            &mut conn,
            dir.path(),
            &["Car".into()],
            ManualDlcOperation::Uninstall,
            None,
        )
        .unwrap();
        {
            let tx = conn.transaction().unwrap();
            settle_tasks(&tx, &token).unwrap();
        }
        assert!(list_tasks(&conn, None).unwrap()[0].unresolved);
    }

    #[test]
    fn shared_cross_mode_and_disabled_owners_require_review() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let a = owner(&conn, dir.path(), "Car", "active", "legacy-sp");
        let token = begin_tasks(
            &mut conn,
            dir.path(),
            &["Car".into()],
            ManualDlcOperation::Install,
            Some(a),
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        settle_tasks(&tx, &token).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            list_tasks(&conn, None).unwrap()[0].recommendation,
            "check_present"
        );
        let b = owner(&conn, dir.path(), "Car", "active", "legacy-lspdfr");
        let task = &list_tasks(&conn, None).unwrap()[0];
        assert_eq!(task.owner_mod_ids, vec![a, b]);
        assert_eq!(task.recommendation, "review");
        conn.execute(
            "UPDATE installed_mod SET status='uninstalled' WHERE id=?1",
            [b],
        )
        .unwrap();
        conn.execute(
            "UPDATE installed_mod SET status='disabled' WHERE id=?1",
            [a],
        )
        .unwrap();
        assert_eq!(list_tasks(&conn, None).unwrap()[0].recommendation, "review");
    }

    #[test]
    fn legacy_owner_with_unknown_root_is_not_removal_advice() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let id = owner(&conn, dir.path(), "Car", "active", "legacy-sp");
        conn.execute("UPDATE installed_mod SET install_path='' WHERE id=?1", [id])
            .unwrap();
        assert_eq!(
            pack_names_for_mod(&conn, id, dir.path()).unwrap(),
            vec!["Car"]
        );
        let token = begin_tasks(
            &mut conn,
            dir.path(),
            &["Car".into()],
            ManualDlcOperation::Uninstall,
            Some(id),
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        settle_tasks(&tx, &token).unwrap();
        tx.commit().unwrap();
        let task = &list_tasks(&conn, None).unwrap()[0];
        assert_eq!(task.owner_mod_ids, vec![id]);
        assert_eq!(task.recommendation, "review");
    }

    #[cfg(windows)]
    #[test]
    fn windows_case_aliases_share_one_pack_intent() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        begin_tasks(
            &mut conn,
            dir.path(),
            &["Car".into(), "cAR".into()],
            ManualDlcOperation::Install,
            None,
        )
        .unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM manual_dlc_event", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn reinstall_union_deduplicates_without_closing_before_commit() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        let token = begin_tasks(
            &mut conn,
            dir.path(),
            &["Old".into(), "New".into(), "Old".into()],
            ManualDlcOperation::Reinstall,
            None,
        )
        .unwrap();
        assert_eq!(list_tasks(&conn, None).unwrap().len(), 2);
        let tx = conn.transaction().unwrap();
        settle_tasks(&tx, &token).unwrap();
        tx.commit().unwrap();
        for task in list_tasks(&conn, None).unwrap() {
            assert!(!task.unresolved);
            assert_eq!(task.recommendation, "check_removal");
        }
    }

    #[test]
    fn managed_paths_are_exact_and_xml_is_escaped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let target = root.join("mods/update/x64/dlcpacks/A&B/dlc.rpf");
        assert_eq!(pack_names_for_paths(root, &[target]).unwrap(), vec!["A&B"]);
        assert!(
            pack_names_for_paths(root, &[root.join("unrelated/dlcpacks/Car/dlc.rpf")])
                .unwrap()
                .is_empty()
        );
        assert!(
            pack_names_for_paths(root, &[root.join("mods/update/x64/dlcpacks/Car")])
                .unwrap()
                .is_empty()
        );
        assert!(
            pack_names_for_paths(root, &[root.join("mods/update/x64/dlcpacks/thing.rpf")])
                .unwrap()
                .is_empty()
        );
        assert!(pack_names_for_paths(
            root,
            &[root.join("mods/update/x64/dlcpacks/Car/../Other/dlc.rpf")]
        )
        .is_err());
        let mut conn = crate::db::open_in_memory().unwrap();
        begin_tasks(
            &mut conn,
            root,
            &["A&B".into()],
            ManualDlcOperation::Install,
            None,
        )
        .unwrap();
        assert_eq!(
            list_tasks(&conn, None).unwrap()[0].item_xml,
            "<Item>dlcpacks:\\A&amp;B\\</Item>"
        );
    }

    #[test]
    fn known_dlclist_payloads_are_refused_but_other_xml_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        for relative in [
            "update/update.rpf/common/data/dlclist.xml",
            "mods/update/update.rpf/common/data/dlclist.xml",
            "MODS/UPDATE/UPDATE.RPF/COMMON/DATA/DLCLIST.XML",
            "./mods/update/update.rpf/common/data/dlclist.xml",
            "mods//update/update.rpf/common/data/dlclist.xml",
            "mods/./update/update.rpf/common/data/dlclist.xml",
        ] {
            assert!(pack_names_for_paths(dir.path(), &[dir.path().join(relative)]).is_err());
        }
        assert!(pack_names_for_paths(
            &dir.path().join("."),
            &[dir
                .path()
                .join("mods/update/update.rpf/common/data/dlclist.xml")]
        )
        .is_err());
        assert!(pack_names_for_paths(
            dir.path(),
            &[dir
                .path()
                .join("mods/update/../update/update.rpf/common/data/dlclist.xml")]
        )
        .is_err());
        assert!(
            pack_names_for_paths(dir.path(), &[dir.path().join("scripts/settings.xml")])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn different_roots_do_not_share_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let mut conn = crate::db::open_in_memory().unwrap();
        for root in [&a, &b] {
            begin_tasks(
                &mut conn,
                root,
                &["Car".into()],
                ManualDlcOperation::Install,
                None,
            )
            .unwrap();
        }
        assert_eq!(list_tasks(&conn, None).unwrap().len(), 2);
        assert_eq!(list_tasks(&conn, Some(&a)).unwrap().len(), 1);
    }
}
