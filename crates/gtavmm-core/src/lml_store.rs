// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex_lml_store_impl; OpenAI-Codex

use crate::{
    error::{CoreError, CoreResult},
    lml::{parse_manifest, LocalReference, MAX_MANIFEST_BYTES},
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// Metadata journal only. No function reads or publishes package files. Root keys
/// must be canonical identities verified by the caller; lexical validation cannot
/// establish real filesystem identity, links, volumes, or publication safety.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewOperation {
    pub mode: String,
    pub root_key: String,
    pub root_display: String,
    pub target: String,
    pub staging_path: String,
    pub manifest: Vec<u8>,
    pub manifest_sha256: String,
    pub unresolved_coverage: bool,
    pub files: Vec<OperationFile>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationFile {
    pub relative_path: String,
    pub size: u64,
    pub sha256: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Prepared,
    Staged,
    Publishing,
    Registered,
    RecoveryRequired,
}
impl Phase {
    fn text(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Staged => "staged",
            Self::Publishing => "publishing",
            Self::Registered => "registered",
            Self::RecoveryRequired => "recovery_required",
        }
    }
    fn parse(s: &str) -> CoreResult<Self> {
        match s {
            "prepared" => Ok(Self::Prepared),
            "staged" => Ok(Self::Staged),
            "publishing" => Ok(Self::Publishing),
            "registered" => Ok(Self::Registered),
            "recovery_required" => Ok(Self::RecoveryRequired),
            _ => Err(invalid("unknown phase")),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationEvent {
    pub phase: Phase,
    pub diagnostic: String,
    pub created_at: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationSnapshot {
    pub id: i64,
    pub header: NewOperation,
    pub created_at: String,
    pub events: Vec<OperationEvent>,
    pub registered: bool,
    pub installed_mod_id: Option<i64>,
}
fn invalid(reason: &str) -> CoreError {
    CoreError::UnsupportedFormat {
        reason: format!("LML metadata: {reason}"),
    }
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn hash_ok(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn relative(s: &str) -> CoreResult<()> {
    LocalReference::parse(s)
        .map(|_| ())
        .map_err(|_| invalid("unsafe relative path"))
}
// Accept portable drive-absolute or POSIX paths; reject device/UNC, traversal,
// ambiguous separators and components. This is deliberately only a lexical guard.
fn absolute(s: &str) -> CoreResult<String> {
    if s.len() > 4096 || s.chars().any(char::is_control) {
        return Err(invalid("invalid absolute path"));
    }
    let path = s.replace('\\', "/");
    let tail = if path.starts_with('/') && !path.starts_with("//") {
        &path[1..]
    } else if path.as_bytes().get(1) == Some(&b':')
        && path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && path.as_bytes().get(2) == Some(&b'/')
    {
        &path[3..]
    } else {
        return Err(invalid("absolute path required"));
    };
    relative(tail)?;
    Ok(path)
}
fn validate(n: &NewOperation) -> CoreResult<()> {
    if !matches!(n.mode.as_str(), "legacy-sp" | "legacy-lspdfr") {
        return Err(invalid("unsupported mode"));
    }
    let root = absolute(&n.root_key)?;
    let display = absolute(&n.root_display)?;
    let stage = absolute(&n.staging_path)?;
    for game in [root, display] {
        let game = game.to_uppercase();
        let lml = format!("{game}/LML");
        let stage = stage.to_uppercase();
        if stage == game
            || game.starts_with(&(stage.clone() + "/"))
            || stage == lml
            || stage.starts_with(&(lml + "/"))
        {
            return Err(invalid("staging must be outside game lml"));
        }
    }
    relative(&n.target)?;
    if n.target.contains('/') {
        return Err(invalid("target must be one package directory"));
    }
    if n.manifest.len() > MAX_MANIFEST_BYTES
        || !hash_ok(&n.manifest_sha256)
        || digest(&n.manifest) != n.manifest_sha256
    {
        return Err(invalid("invalid manifest digest or size"));
    }
    parse_manifest(&n.manifest).map_err(|_| invalid("invalid manifest"))?;
    if n.files.is_empty() || n.files.len() > 10_000 {
        return Err(invalid("invalid file count"));
    }
    let (mut lower, mut upper) = (BTreeSet::new(), BTreeSet::new());
    let mut total = 0i64;
    let mut manifest = false;
    for f in &n.files {
        relative(&f.relative_path)?;
        if f.relative_path
            .rsplit('/')
            .next()
            .is_some_and(|name| name.eq_ignore_ascii_case("install.xml"))
            && !f.relative_path.eq_ignore_ascii_case("install.xml")
        {
            return Err(invalid("nested manifest is not a single package"));
        }
        if !lower.insert(f.relative_path.to_lowercase())
            || !upper.insert(f.relative_path.to_uppercase())
            || !hash_ok(&f.sha256)
        {
            return Err(invalid("duplicate path or invalid digest"));
        }
        let size = i64::try_from(f.size).map_err(|_| invalid("file size overflow"))?;
        total = total
            .checked_add(size)
            .ok_or_else(|| invalid("total size overflow"))?;
        if f.relative_path.eq_ignore_ascii_case("install.xml") {
            manifest = f.size == n.manifest.len() as u64 && f.sha256 == n.manifest_sha256;
        }
    }
    if !manifest {
        return Err(invalid(
            "complete file set must include matching install.xml",
        ));
    }
    Ok(())
}
fn diagnostic(s: &str) -> CoreResult<()> {
    if s.len() > 4096 {
        Err(invalid("diagnostic exceeds 4 KiB"))
    } else {
        Ok(())
    }
}
fn positive(id: i64) -> CoreResult<()> {
    if id > 0 {
        Ok(())
    } else {
        Err(invalid("positive ID required"))
    }
}
fn current(c: &Connection, id: i64) -> CoreResult<Phase> {
    let s: String = c.query_row(
        "SELECT phase FROM lml_operation_event WHERE operation_id=?1 ORDER BY id DESC LIMIT 1",
        [id],
        |r| r.get(0),
    )?;
    Phase::parse(&s)
}
fn event(c: &Connection, id: i64, phase: Phase, d: &str) -> CoreResult<()> {
    c.execute(
        "INSERT INTO lml_operation_event(operation_id,phase,diagnostic) VALUES(?1,?2,?3)",
        params![id, phase.text(), d],
    )?;
    Ok(())
}
/// Atomically reserves the target, preserving the immutable complete snapshot.
pub fn begin_operation(c: &mut Connection, n: &NewOperation) -> CoreResult<i64> {
    validate(n)?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute("INSERT INTO lml_operation(mode,root_key,root_display,target,staging_path,manifest,manifest_sha256,unresolved_coverage) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![n.mode,n.root_key,n.root_display,n.target,n.staging_path,n.manifest,n.manifest_sha256,n.unresolved_coverage])?;
    let id = tx.last_insert_rowid();
    for f in &n.files {
        tx.execute("INSERT INTO lml_operation_file(operation_id,relative_path,path_lower,path_upper,size,sha256) VALUES(?1,?2,?3,?4,?5,?6)",params![id,f.relative_path,f.relative_path.to_lowercase(),f.relative_path.to_uppercase(),f.size as i64,f.sha256])?;
    }
    tx.execute("INSERT INTO lml_package(operation_id,root_key,target_lower,target_upper) VALUES(?1,?2,?3,?4)",params![id,n.root_key,n.target.to_lowercase(),n.target.to_uppercase()])?;
    event(&tx, id, Phase::Prepared, "")?;
    tx.commit()?;
    Ok(id)
}
/// Appends a caller-reported phase; this does not attest filesystem state.
pub fn record_phase(
    c: &mut Connection,
    id: i64,
    expected: Phase,
    next: Phase,
    d: &str,
) -> CoreResult<()> {
    positive(id)?;
    diagnostic(d)?;
    if !matches!(
        (expected, next),
        (Phase::Prepared, Phase::Staged)
            | (Phase::Staged, Phase::Publishing)
            | (
                Phase::Prepared | Phase::Staged | Phase::Publishing,
                Phase::RecoveryRequired
            )
    ) {
        return Err(invalid("invalid phase transition"));
    }
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if current(&tx, id)? != expected {
        return Err(invalid("stale expected phase"));
    }
    event(&tx, id, next, d)?;
    tx.commit()?;
    Ok(())
}
/// Finalizes ownership and the registered event together. Never creates an
/// installed_mod row or installed_mod_file row and never writes game files.
pub fn register_ownership(
    c: &mut Connection,
    id: i64,
    installed_mod_id: Option<i64>,
    d: &str,
) -> CoreResult<()> {
    positive(id)?;
    diagnostic(d)?;
    if let Some(id) = installed_mod_id {
        positive(id)?;
    }
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if current(&tx, id)? != Phase::Publishing {
        return Err(invalid("publishing phase required"));
    }
    if let Some(installed) = installed_mod_id {
        verify_installed_relation(&tx, id, installed)?;
    }
    if tx.execute("UPDATE lml_package SET registered=1,installed_mod_id=?2 WHERE operation_id=?1 AND registered=0",params![id,installed_mod_id])?!=1 {return Err(invalid("missing target reservation"));}
    event(&tx, id, Phase::Registered, d)?;
    tx.commit()?;
    Ok(())
}
/// Loads immutable metadata and ordered events; no package filesystem access.
pub fn load_operation(c: &Connection, id: i64) -> CoreResult<Option<OperationSnapshot>> {
    positive(id)?;
    let tx = c.unchecked_transaction()?;
    let snapshot = load_in_transaction(&tx, id)?;
    tx.commit()?;
    Ok(snapshot)
}
fn load_in_transaction(c: &Connection, id: i64) -> CoreResult<Option<OperationSnapshot>> {
    positive(id)?;
    let row=c.query_row("SELECT mode,root_key,root_display,target,staging_path,manifest,manifest_sha256,unresolved_coverage,created_at FROM lml_operation WHERE id=?1",[id],|r|Ok((NewOperation{mode:r.get(0)?,root_key:r.get(1)?,root_display:r.get(2)?,target:r.get(3)?,staging_path:r.get(4)?,manifest:r.get(5)?,manifest_sha256:r.get(6)?,unresolved_coverage:r.get(7)?,files:Vec::new()},r.get::<_,String>(8)?))).optional()?;
    let Some((mut header, created_at)) = row else {
        return Ok(None);
    };
    let mut stmt=c.prepare("SELECT relative_path,size,sha256 FROM lml_operation_file WHERE operation_id=?1 ORDER BY relative_path")?;
    header.files = stmt
        .query_map([id], |r| {
            Ok(OperationFile {
                relative_path: r.get(0)?,
                size: r.get::<_, i64>(1)? as u64,
                sha256: r.get(2)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    let mut stmt=c.prepare("SELECT phase,diagnostic,created_at FROM lml_operation_event WHERE operation_id=?1 ORDER BY id")?;
    let rows = stmt
        .query_map([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let events = rows
        .into_iter()
        .map(|(p, diagnostic, created_at)| {
            Ok(OperationEvent {
                phase: Phase::parse(&p)?,
                diagnostic,
                created_at,
            })
        })
        .collect::<CoreResult<Vec<_>>>()?;
    let (registered, installed_mod_id) = c.query_row(
        "SELECT registered,installed_mod_id FROM lml_package WHERE operation_id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(Some(OperationSnapshot {
        id,
        header,
        created_at,
        events,
        registered,
        installed_mod_id,
    }))
}
/// Includes recovery_required reservations; only registered operations are finished.
pub fn list_unfinished_for_root(
    c: &Connection,
    root_key: &str,
) -> CoreResult<Vec<OperationSnapshot>> {
    absolute(root_key)?;
    let tx = c.unchecked_transaction()?;
    let mut stmt=tx.prepare("SELECT operation_id FROM lml_package WHERE root_key=?1 AND registered=0 ORDER BY operation_id")?;
    let ids = stmt
        .query_map([root_key], |r| r.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    let rows = ids
        .into_iter()
        .map(|id| load_in_transaction(&tx, id)?.ok_or_else(|| invalid("missing operation")))
        .collect::<CoreResult<Vec<_>>>()?;
    tx.commit()?;
    Ok(rows)
}

// Existing generic rows are linkable only when their full tracked path/hash set
// already equals this operation. This is metadata agreement, not proof of files
// on disk or permission to expose an active LML row through generic lifecycles.
fn verify_installed_relation(c: &Connection, operation: i64, installed: i64) -> CoreResult<()> {
    let snapshot =
        load_in_transaction(c, operation)?.ok_or_else(|| invalid("missing operation"))?;
    let (mode, path): (Option<String>, String) = c.query_row(
        "SELECT mode,install_path FROM installed_mod WHERE id=?1",
        [installed],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let root = absolute(&snapshot.header.root_display)?;
    let package_path = format!("{root}/lml/{}", snapshot.header.target);
    if mode.as_deref() != Some(snapshot.header.mode.as_str()) || absolute(&path)? != root {
        return Err(invalid("unrelated installed mod"));
    }
    let mut statement=c.prepare("SELECT target_path,file_hash FROM installed_mod_file WHERE installed_mod_id=?1 ORDER BY target_path LIMIT 10001")?;
    let rows = statement
        .query_map([installed], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if rows.len() != snapshot.header.files.len() {
        return Err(invalid("installed file set differs"));
    }
    let expected = snapshot
        .header
        .files
        .into_iter()
        .map(|f| (format!("{package_path}/{}", f.relative_path), f.sha256))
        .collect::<BTreeSet<_>>();
    let actual = rows
        .into_iter()
        .map(|(p, h)| Ok((absolute(&p)?, h)))
        .collect::<CoreResult<BTreeSet<_>>>()?;
    if actual != expected {
        return Err(invalid("installed paths or digests differ"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn staging_lml_guard_and_nested_manifest_are_conservative() {
        let mut c = crate::db::open_in_memory().unwrap();
        for path in [
            "C:/Game/lml/stage",
            "c:/GAME/LmL/stage",
            "C:/Game/lml",
            "C:/Game",
            "C:/Game/lmlish/stage",
        ] {
            let mut n = input();
            n.staging_path = path.into();
            let result = begin_operation(&mut c, &n);
            if path.ends_with("lmlish/stage") {
                assert!(result.is_ok(), "{path}");
            } else {
                assert!(result.is_err(), "{path}");
            }
        }
        let mut n = input();
        n.files.push(OperationFile {
            relative_path: "nested/install.xml".into(),
            size: 1,
            sha256: "a".repeat(64),
        });
        assert!(begin_operation(&mut c, &n).is_err());
    }
    #[test]
    fn registration_event_failure_rolls_back_ownership() {
        let mut c = crate::db::open_in_memory().unwrap();
        let id = begin_operation(&mut c, &input()).unwrap();
        record_phase(&mut c, id, Phase::Prepared, Phase::Staged, "").unwrap();
        record_phase(&mut c, id, Phase::Staged, Phase::Publishing, "").unwrap();
        c.execute_batch("CREATE TRIGGER fail_registered BEFORE INSERT ON lml_operation_event WHEN NEW.phase='registered' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
        assert!(register_ownership(&mut c, id, None, "").is_err());
        let snapshot = load_operation(&c, id).unwrap().unwrap();
        assert!(!snapshot.registered);
        assert_eq!(snapshot.events.len(), 3);
        assert!(c
            .execute("UPDATE lml_operation SET target='changed'", [])
            .is_err());
        assert!(c.execute("DELETE FROM lml_operation_file", []).is_err());
        assert!(c
            .execute("UPDATE lml_operation_event SET diagnostic='changed'", [])
            .is_err());
    }
    #[test]
    fn target_is_one_directory_and_prepared_file_set_is_sealed() {
        let mut c = crate::db::open_in_memory().unwrap();
        let mut n = input();
        n.target = "a/b".into();
        assert!(begin_operation(&mut c, &n).is_err());
        n.target = "a".into();
        let id = begin_operation(&mut c, &n).unwrap();
        assert!(c.execute("INSERT INTO lml_operation_file(operation_id,relative_path,path_lower,path_upper,size,sha256) VALUES(?1,'extra','extra','EXTRA',1,?2)",params![id,"a".repeat(64)]).is_err());
    }
    #[test]
    fn installed_mod_link_requires_complete_matching_metadata() {
        let mut c = crate::db::open_in_memory().unwrap();
        let n = input();
        let id = begin_operation(&mut c, &n).unwrap();
        record_phase(&mut c, id, Phase::Prepared, Phase::Staged, "").unwrap();
        record_phase(&mut c, id, Phase::Staged, Phase::Publishing, "").unwrap();
        c.execute("INSERT INTO installed_mod(name,source_type,install_path,status,mode) VALUES('Existing','folder','C:/Game','active','legacy-sp')",[]).unwrap();
        let installed = c.last_insert_rowid();
        assert!(register_ownership(&mut c, id, Some(installed), "").is_err());
        c.execute("INSERT INTO installed_mod_file(installed_mod_id,target_path,file_hash) VALUES(?1,'C:/Game/lml/Example/install.xml',?2)",params![installed,n.manifest_sha256]).unwrap();
        c.execute(
            "UPDATE installed_mod SET mode='legacy-lspdfr' WHERE id=?1",
            [installed],
        )
        .unwrap();
        assert!(register_ownership(&mut c, id, Some(installed), "").is_err());
        c.execute(
            "UPDATE installed_mod SET mode='legacy-sp',install_path='C:/Unrelated' WHERE id=?1",
            [installed],
        )
        .unwrap();
        assert!(register_ownership(&mut c, id, Some(installed), "").is_err());
        c.execute(
            "UPDATE installed_mod SET install_path='C:/Game' WHERE id=?1",
            [installed],
        )
        .unwrap();
        register_ownership(&mut c, id, Some(installed), "").unwrap();
        assert_eq!(
            load_operation(&c, id).unwrap().unwrap().installed_mod_id,
            Some(installed)
        );
    }
    #[test]
    fn public_snapshot_reads_finish_their_read_transactions() {
        let mut c = crate::db::open_in_memory().unwrap();
        let id = begin_operation(&mut c, &input()).unwrap();
        assert!(load_operation(&c, id).unwrap().is_some());
        assert!(c.is_autocommit());
        assert_eq!(list_unfinished_for_root(&c, "C:/Game").unwrap().len(), 1);
        assert!(c.is_autocommit());
    }
    fn input() -> NewOperation {
        let manifest = b"<EasyInstall><Name>Example</Name></EasyInstall>".to_vec();
        let hash = digest(&manifest);
        NewOperation {
            mode: "legacy-sp".into(),
            root_key: "C:/Game".into(),
            root_display: "C:/Game".into(),
            target: "Example".into(),
            staging_path: "C:/Stage/Example".into(),
            manifest: manifest.clone(),
            manifest_sha256: hash.clone(),
            unresolved_coverage: true,
            files: vec![OperationFile {
                relative_path: "install.xml".into(),
                size: manifest.len() as u64,
                sha256: hash,
            }],
        }
    }
    #[test]
    fn phase_order_cas_terminal_and_reservation() {
        let mut c = crate::db::open_in_memory().unwrap();
        let n = input();
        let id = begin_operation(&mut c, &n).unwrap();
        assert!(begin_operation(&mut c, &n).is_err());
        assert!(record_phase(&mut c, id, Phase::Prepared, Phase::Publishing, "").is_err());
        record_phase(&mut c, id, Phase::Prepared, Phase::Staged, "").unwrap();
        assert!(record_phase(&mut c, id, Phase::Prepared, Phase::Staged, "").is_err());
        assert!(record_phase(&mut c, id, Phase::Staged, Phase::Registered, "").is_err());
        record_phase(&mut c, id, Phase::Staged, Phase::Publishing, "").unwrap();
        assert!(register_ownership(&mut c, id, Some(999999), "").is_err());
        assert_eq!(load_operation(&c, id).unwrap().unwrap().events.len(), 3);
        register_ownership(&mut c, id, None, "").unwrap();
        assert!(list_unfinished_for_root(&c, &n.root_key)
            .unwrap()
            .is_empty());
        assert!(record_phase(&mut c, id, Phase::Registered, Phase::RecoveryRequired, "").is_err());
        let mut other = n.clone();
        other.root_key = "C:/Other".into();
        other.root_display = other.root_key.clone();
        assert!(begin_operation(&mut c, &other).is_ok());
    }
    #[test]
    fn rejects_invalid_metadata_and_case_collisions() {
        let mut c = crate::db::open_in_memory().unwrap();
        for change in 0..8 {
            let mut n = input();
            match change {
                0 => n.mode = "enhanced".into(),
                1 => n.target = "../bad".into(),
                2 => n.files[0].sha256 = "wrong".into(),
                3 => n.files.push(n.files[0].clone()),
                4 => n.staging_path = "C:/Game/lml/stage".into(),
                5 => n.manifest.push(b'x'),
                6 => n.files[0].size += 1,
                _ => n.files.push(OperationFile {
                    relative_path: "INSTALL.XML".into(),
                    size: 1,
                    sha256: "a".repeat(64),
                }),
            };
            assert!(begin_operation(&mut c, &n).is_err());
        }
        assert!(load_operation(&c, 0).is_err());
    }
    #[test]
    fn injected_insert_failures_roll_back_all_metadata() {
        for table in [
            "lml_operation",
            "lml_operation_file",
            "lml_operation_event",
            "lml_package",
        ] {
            let mut c = crate::db::open_in_memory().unwrap();
            c.execute_batch(&format!("CREATE TRIGGER fail_insert BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT,'injected'); END;")).unwrap();
            assert!(begin_operation(&mut c, &input()).is_err());
            for table in [
                "lml_operation",
                "lml_operation_file",
                "lml_operation_event",
                "lml_package",
            ] {
                assert_eq!(
                    c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                        .get::<_, i64>(0))
                        .unwrap(),
                    0
                );
            }
        }
    }
    #[test]
    fn recovery_and_unfinished_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.db");
        let id = {
            let mut c = crate::db::open(&path).unwrap();
            let id = begin_operation(&mut c, &input()).unwrap();
            record_phase(
                &mut c,
                id,
                Phase::Prepared,
                Phase::RecoveryRequired,
                "interrupted",
            )
            .unwrap();
            id
        };
        let mut c = crate::db::open(&path).unwrap();
        let rows = list_unfinished_for_root(&c, "C:/Game").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert!(record_phase(&mut c, id, Phase::RecoveryRequired, Phase::Staged, "").is_err());
        assert!(begin_operation(&mut c, &input()).is_err());
    }
}
