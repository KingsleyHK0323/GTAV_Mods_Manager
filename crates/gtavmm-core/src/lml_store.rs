// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex_lml_store_impl; OpenAI-Codex

use crate::{
    error::{CoreError, CoreResult},
    lml::{parse_manifest, LocalReference, MAX_MANIFEST_BYTES},
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

// OpenAI-Codex_lml_generic_guards; OpenAI-Codex
/// Read all permanent reservations in one consistent metadata snapshot.
pub fn reservations(c: &Connection) -> CoreResult<Vec<OperationSnapshot>> {
    let tx = c.unchecked_transaction()?;
    let mut statement = tx.prepare("SELECT operation_id FROM lml_package ORDER BY operation_id")?;
    let ids = statement
        .query_map([], |r| r.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    let rows = ids
        .into_iter()
        .map(|id| {
            load_in_transaction(&tx, id)?.ok_or_else(|| invalid("missing reserved operation"))
        })
        .collect::<CoreResult<Vec<_>>>()?;
    tx.commit()?;
    Ok(rows)
}

fn path_text(path: &Path) -> CoreResult<String> {
    let text = path
        .to_str()
        .ok_or_else(|| invalid("non Unicode guard path"))?;
    // Canonical Windows drive paths use the extended prefix; stored keys do not.
    let portable = text.strip_prefix(r"\\?\").unwrap_or(text);
    absolute(portable)
}

fn folded_overlap(a: &str, b: &str) -> bool {
    fn overlap(a: String, b: String) -> bool {
        a == b || a.starts_with(&(b.clone() + "/")) || b.starts_with(&(a + "/"))
    }
    overlap(a.to_lowercase(), b.to_lowercase()) || overlap(a.to_uppercase(), b.to_uppercase())
}

/// Inspect existing ancestors before canonicalization. Never traverse a known link.
fn checked_identity(path: &Path) -> CoreResult<String> {
    let lexical = path_text(path)?;
    let mut suffix = Vec::new();
    let mut existing = path;
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                suffix.push(
                    existing
                        .file_name()
                        .ok_or_else(|| invalid("missing path ancestor"))?
                        .to_os_string(),
                );
                existing = existing
                    .parent()
                    .ok_or_else(|| invalid("missing path ancestor"))?;
                // Portable drive fixtures are lexical on non-Windows hosts.
                if existing.as_os_str().is_empty() && !path.is_absolute() {
                    return Ok(lexical);
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    for ancestor in existing.ancestors() {
        let metadata = std::fs::symlink_metadata(ancestor)?;
        if metadata.file_type().is_symlink() {
            return Err(invalid("linked guard ancestor"));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(invalid("reparse guard ancestor"));
            }
        }
    }
    let mut canonical = std::fs::canonicalize(existing)?;
    for component in suffix.into_iter().rev() {
        canonical.push(component);
    }
    path_text(&canonical)
}

// OpenAI-Codex: Reuse conservative path comparisons for backup preflight.
pub(crate) fn backup_guard_identity(path: &Path) -> CoreResult<String> {
    if !path.is_absolute() {
        return Err(CoreError::UnsupportedFormat {
            reason: format!("backup guard path must be absolute: {}", path.display()),
        });
    }
    checked_identity(path)
}

pub(crate) fn backup_guard_overlap(a: &str, b: &str) -> bool {
    folded_overlap(a, b)
}

fn same_root(root: &str, operation: &NewOperation) -> CoreResult<bool> {
    for stored in [&operation.root_key, &operation.root_display] {
        let stored = checked_identity(Path::new(stored))?;
        if root.to_lowercase() == stored.to_lowercase()
            || root.to_uppercase() == stored.to_uppercase()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Refuse ambiguous or unfinished operations, including conservative case aliases.
pub fn ensure_root_idle(c: &Connection, game_root: &Path) -> CoreResult<()> {
    let root = checked_identity(game_root)?;
    for operation in reservations(c)? {
        if !operation.registered && same_root(&root, &operation.header)? {
            return Err(invalid("unfinished operation reserves this game root"));
        }
    }
    Ok(())
}

/// Generic writers cannot enter LML or any stage/package reservation, across roots.
pub fn ensure_generic_targets_allowed(
    c: &Connection,
    game_root: &Path,
    targets: &[PathBuf],
) -> CoreResult<()> {
    let root = checked_identity(game_root)?;
    let lml = format!("{root}/lml");
    let operations = reservations(c)?;
    let mut reserved = vec![lml];
    for operation in operations {
        if !operation.registered && same_root(&root, &operation.header)? {
            return Err(invalid("unfinished operation reserves this game root"));
        }
        reserved.push(checked_identity(Path::new(&operation.header.staging_path))?);
        for stored in [&operation.header.root_key, &operation.header.root_display] {
            reserved.push(checked_identity(
                &Path::new(stored).join("lml").join(&operation.header.target),
            )?);
        }
    }
    for target in targets {
        let target = checked_identity(target)?;
        if reserved.iter().any(|path| folded_overlap(&target, path)) {
            return Err(invalid("generic target overlaps an LML reservation"));
        }
    }
    Ok(())
}

/// Persisted association and tracked targets both protect generic lifecycles.
pub fn ensure_generic_mod_allowed(c: &Connection, mod_id: i64) -> CoreResult<()> {
    ensure_generic_mod_allowed_inner(c, mod_id, false)
}

// OpenAI-Codex: Never allow an operation after its caller observed an empty root.
pub(crate) fn reject_observed_empty_root(c: &Connection, mod_id: i64) -> CoreResult<()> {
    ensure_generic_mod_allowed_inner(c, mod_id, true)
}

fn ensure_generic_mod_allowed_inner(
    c: &Connection,
    mod_id: i64,
    reject_after_lookup: bool,
) -> CoreResult<()> {
    let tx = c.unchecked_transaction()?;
    let associated: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM lml_package WHERE installed_mod_id=?1)",
        [mod_id],
        |r| r.get(0),
    )?;
    if associated {
        return Err(invalid("LML ownership requires dedicated lifecycle"));
    }
    let root: String = tx.query_row(
        "SELECT install_path FROM installed_mod WHERE id=?1",
        [mod_id],
        |r| r.get(0),
    )?;
    let mut statement =
        tx.prepare("SELECT target_path FROM installed_mod_file WHERE installed_mod_id=?1")?;
    let targets = statement
        .query_map([mod_id], |r| r.get::<_, String>(0))?
        .map(|row| row.map(PathBuf::from))
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    // Finish this read transaction before the reservation helper opens its snapshot.
    tx.commit()?;
    if root.is_empty() {
        return Err(CoreError::UnsupportedFormat {
            reason: format!(
                concat!(
                    "mod {} has no recorded game folder; its original installation ",
                    "cannot be verified and automatic recovery is not available yet"
                ),
                mod_id
            ),
        });
    }
    if reject_after_lookup {
        return Err(CoreError::UnsupportedFormat {
            reason: format!("mod {mod_id} game folder changed while checking; retry the operation"),
        });
    }
    ensure_generic_targets_allowed(c, Path::new(&root), &targets)
}

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
    // OpenAI-Codex_lml_generic_guards; OpenAI-Codex: Serialize root intent inside
    // the write lock. Metadata comparison is conservative and never reads files.
    let mut statement = tx.prepare("SELECT o.root_key,o.root_display FROM lml_operation o JOIN lml_package p ON p.operation_id=o.id WHERE p.registered=0")?;
    let unfinished = statement
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for (key, display) in unfinished {
        for old in [&key, &display] {
            let old = absolute(old)?;
            for new in [&n.root_key, &n.root_display] {
                let new = absolute(new)?;
                if old.to_lowercase() == new.to_lowercase()
                    || old.to_uppercase() == new.to_uppercase()
                {
                    return Err(invalid("unfinished operation reserves this game root"));
                }
            }
        }
    }
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

// OpenAI-Codex_lml_production_plan: Synthetic database transaction only; no publication.
#[cfg(test)]
fn finalize_published_rows_for_test(
    c: &mut Connection,
    operation_id: i64,
    name: &str,
    source_type: &str,
    source_path: Option<&str>,
) -> CoreResult<i64> {
    positive(operation_id)?;
    if name.trim().is_empty() || name.contains('\0') {
        return Err(invalid("invalid synthetic mod name"));
    }
    if !matches!(source_type, "folder" | "zip" | "sevenzip" | "rar")
        || source_path.is_some_and(|path| path.contains('\0'))
    {
        return Err(invalid("invalid synthetic source metadata"));
    }
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let op = load_in_transaction(&tx, operation_id)?
        .ok_or_else(|| invalid("missing operation"))?;
    validate(&op.header)?;
    if op.header.root_key != op.header.root_display
        || current(&tx, operation_id)? != Phase::Publishing
        || op.registered
        || op.installed_mod_id.is_some()
    {
        return Err(invalid("operation is not eligible for metadata registration"));
    }
    let root = absolute(&op.header.root_display)?;
    let base = format!("{root}/lml/{}", op.header.target);
    tx.execute(
        "INSERT INTO installed_mod \
         (name,source_type,install_path,status,source_path,mode,mode_inferred) \
         VALUES(?1,?2,?3,'active',?4,?5,0)",
        params![
            name,
            source_type,
            op.header.root_display.as_str(),
            source_path,
            op.header.mode.as_str()
        ],
    )?;
    let installed_id = tx.last_insert_rowid();
    {
        let mut insert_file = tx.prepare_cached(
            "INSERT INTO installed_mod_file(installed_mod_id,target_path,backup_path,file_hash) \
             VALUES(?1,?2,NULL,?3)",
        )?;
        for file in &op.header.files {
            let target = format!("{base}/{}", file.relative_path);
            insert_file.execute(params![installed_id, target, file.sha256.as_str()])?;
        }
    }
    verify_installed_relation(&tx, operation_id, installed_id)?;
    tx.execute(
        "INSERT INTO install_event(installed_mod_id,event_type,success) \
         VALUES(?1,'install',1)",
        [installed_id],
    )?;
    if tx.execute(
        "UPDATE lml_package SET registered=1,installed_mod_id=?2 \
         WHERE operation_id=?1 AND registered=0 AND installed_mod_id IS NULL",
        params![operation_id, installed_id],
    )? != 1
    {
        return Err(invalid("reservation is no longer available"));
    }
    event(&tx, operation_id, Phase::Registered, "")?;
    tx.commit()?;
    Ok(installed_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    // OpenAI-Codex_lml_production_plan: Synthetic metadata tests never publish files.
    fn synthetic_publishing(mode: &str) -> (Connection, NewOperation, i64) {
        let mut c = crate::db::open_in_memory().unwrap();
        let mut operation = input();
        operation.mode = mode.to_string();
        operation.files.push(OperationFile {
            relative_path: "zz-companion.bin".to_string(),
            size: b"payload".len() as u64,
            sha256: digest(b"payload"),
        });
        let id = begin_operation(&mut c, &operation).unwrap();
        record_phase(&mut c, id, Phase::Prepared, Phase::Staged, "").unwrap();
        record_phase(&mut c, id, Phase::Staged, Phase::Publishing, "").unwrap();
        (c, operation, id)
    }

    fn synthetic_database_snapshot(
        c: &Connection,
    ) -> Vec<(String, Vec<Vec<rusqlite::types::Value>>)> {
        let mut statement = c
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='table' \
                 AND (name NOT LIKE 'sqlite_%' OR name='sqlite_sequence') ORDER BY name",
            )
            .unwrap();
        let names = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        drop(statement);
        names
            .into_iter()
            .map(|name| {
                let sql = format!(
                    r#"SELECT rowid, * FROM "{}" ORDER BY rowid"#,
                    name.replace('"', "\"\"")
                );
                let mut statement = c.prepare(&sql).unwrap();
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
                (name, rows)
            })
            .collect()
    }

    #[test]
    fn synthetic_lml_finalization_records_both_legacy_modes_once() {
        for mode in ["legacy-sp", "legacy-lspdfr"] {
            let (mut c, operation, id) = synthetic_publishing(mode);
            let fk: bool = c
                .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
                .unwrap();
            assert!(fk);
            let mod_id = finalize_published_rows_for_test(
                &mut c,
                id,
                "Synthetic package",
                "folder",
                Some("synthetic/source"),
            )
            .unwrap();
            let row: (String, String, String, String, Option<String>, Option<String>, i64) = c
                .query_row(
                    "SELECT name,source_type,install_path,status,source_path,mode,mode_inferred \
                     FROM installed_mod WHERE id=?1",
                    [mod_id],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                        ))
                    },
                )
                .unwrap();
            assert_eq!(row.0, "Synthetic package");
            assert_eq!(row.1, "folder");
            assert_eq!(row.2, operation.root_display);
            assert_eq!(row.3, "active");
            assert_eq!(row.4.as_deref(), Some("synthetic/source"));
            assert_eq!(row.5.as_deref(), Some(mode));
            assert_eq!(row.6, 0);

            let mut statement = c
                .prepare(
                    "SELECT target_path,backup_path,file_hash FROM installed_mod_file \
                     WHERE installed_mod_id=?1 ORDER BY target_path",
                )
                .unwrap();
            let actual = statement
                .query_map([mod_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            drop(statement);
            let mut expected = operation
                .files
                .iter()
                .map(|file| {
                    (
                        format!(
                            "{}/lml/{}/{}",
                            operation.root_display, operation.target, file.relative_path
                        ),
                        None::<String>,
                        file.sha256.clone(),
                    )
                })
                .collect::<Vec<_>>();
            expected.sort();
            assert_eq!(actual, expected);
            let events: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM install_event \
                     WHERE installed_mod_id=?1 AND event_type='install' AND success=1",
                    [mod_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(events, 1);
            let state = load_operation(&c, id).unwrap().unwrap();
            assert!(state.registered);
            assert_eq!(state.installed_mod_id, Some(mod_id));
            assert_eq!(
                state.events.last().map(|event| event.phase),
                Some(Phase::Registered)
            );
            let mut check = c.prepare("PRAGMA foreign_key_check").unwrap();
            let mut rows = check.query([]).unwrap();
            assert!(rows.next().unwrap().is_none());
            drop(rows);
            drop(check);

            let before_retry = synthetic_database_snapshot(&c);
            let error = finalize_published_rows_for_test(
                &mut c,
                id,
                "Synthetic package",
                "folder",
                Some("synthetic/source"),
            )
            .unwrap_err();
            assert!(matches!(
                error,
                CoreError::UnsupportedFormat { reason }
                    if reason == "LML metadata: operation is not eligible for metadata registration"
            ));
            assert_eq!(synthetic_database_snapshot(&c), before_retry);
            assert!(c.is_autocommit());
        }
    }

    #[test]
    fn synthetic_lml_finalization_rolls_back_every_insert_and_registration() {
        let cases = [
            (
                "mod",
                "CREATE TRIGGER synthetic_fail BEFORE INSERT ON installed_mod \
                 BEGIN SELECT RAISE(ABORT,'synthetic-lml-stop'); END;",
            ),
            (
                "second file",
                "CREATE TRIGGER synthetic_fail BEFORE INSERT ON installed_mod_file \
                 WHEN NEW.target_path LIKE '%/zz-companion.bin' \
                 BEGIN SELECT RAISE(ABORT,'synthetic-lml-stop'); END;",
            ),
            (
                "install event",
                "CREATE TRIGGER synthetic_fail BEFORE INSERT ON install_event \
                 BEGIN SELECT RAISE(ABORT,'synthetic-lml-stop'); END;",
            ),
            (
                "ownership update",
                "CREATE TRIGGER synthetic_fail BEFORE UPDATE ON lml_package \
                 WHEN NEW.registered=1 \
                 BEGIN SELECT RAISE(ABORT,'synthetic-lml-stop'); END;",
            ),
            (
                "registered event",
                "CREATE TRIGGER synthetic_fail BEFORE INSERT ON lml_operation_event \
                 WHEN NEW.phase='registered' \
                 BEGIN SELECT RAISE(ABORT,'synthetic-lml-stop'); END;",
            ),
        ];
        for (label, trigger) in cases {
            let (mut c, _, id) = synthetic_publishing("legacy-sp");
            c.execute_batch(trigger).unwrap();
            let original_operation = load_operation(&c, id).unwrap().unwrap();
            let original_database = synthetic_database_snapshot(&c);
            let error = finalize_published_rows_for_test(
                &mut c,
                id,
                "Synthetic package",
                "folder",
                Some("synthetic/source"),
            )
            .unwrap_err();
            assert!(
                matches!(
                    &error,
                    CoreError::Database(rusqlite::Error::SqliteFailure(_, Some(message)))
                        if message == "synthetic-lml-stop"
                ),
                "{label}: unexpected error {error:?}"
            );
            assert_eq!(synthetic_database_snapshot(&c), original_database, "{label}");
            let state = load_operation(&c, id).unwrap().unwrap();
            assert_eq!(state, original_operation, "{label}");
            assert_eq!(
                state.events.last().map(|event| event.phase),
                Some(Phase::Publishing),
                "{label}"
            );
            assert!(!state.registered, "{label}");
            assert_eq!(state.installed_mod_id, None, "{label}");
            assert!(c.is_autocommit(), "{label}");
        }
    }

    #[test]
    fn synthetic_lml_finalization_rejects_wrong_phase_without_changes() {
        let mut c = crate::db::open_in_memory().unwrap();
        let operation = input();
        let id = begin_operation(&mut c, &operation).unwrap();
        for phase in [Phase::Prepared, Phase::Staged] {
            let before = synthetic_database_snapshot(&c);
            let error = finalize_published_rows_for_test(
                &mut c,
                id,
                "Synthetic package",
                "folder",
                Some("synthetic/source"),
            )
            .unwrap_err();
            assert!(matches!(
                error,
                CoreError::UnsupportedFormat { reason }
                    if reason == "LML metadata: operation is not eligible for metadata registration"
            ));
            assert_eq!(synthetic_database_snapshot(&c), before);
            assert!(c.is_autocommit());
            if phase == Phase::Prepared {
                record_phase(&mut c, id, Phase::Prepared, Phase::Staged, "").unwrap();
            }
        }
    }

    // OpenAI-Codex_lml_generic_guards; OpenAI-Codex: Intent serialization precedes stage writes.
    #[test]
    fn different_targets_cannot_reserve_an_unfinished_root() {
        let mut c = crate::db::open_in_memory().unwrap();
        let n = input();
        let id = begin_operation(&mut c, &n).unwrap();
        let mut other = n.clone();
        other.target = "Other".into();
        other.staging_path = "C:/Stage/Other".into();
        assert!(begin_operation(&mut c, &other).is_err());
        other.root_key = "c:/GAME".into();
        other.root_display = "c:/GAME".into();
        assert!(begin_operation(&mut c, &other).is_err());
        record_phase(&mut c, id, Phase::Prepared, Phase::Staged, "").unwrap();
        record_phase(&mut c, id, Phase::Staged, Phase::Publishing, "").unwrap();
        register_ownership(&mut c, id, None, "").unwrap();
        assert!(begin_operation(&mut c, &other).is_ok());
    }
    #[cfg(windows)]
    #[test]
    fn missing_suffix_is_rebuilt_before_drive_root_validation() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing").join("payload.asi");
        let identity = checked_identity(&missing).unwrap();
        assert!(identity.ends_with("/missing/payload.asi"));
        // Exercise a nearest existing ancestor that is precisely the drive root.
        let drive = directory.path().ancestors().last().unwrap();
        let leaf = format!("gtavmm-guard-missing-{}", std::process::id());
        let candidate = drive.join(leaf).join("payload.asi");
        assert!(checked_identity(&candidate)
            .unwrap()
            .ends_with("/payload.asi"));
    }
    // OpenAI-Codex_lml_generic_guards; OpenAI-Codex: Source-first guard regressions.
    #[test]
    fn generic_guards_block_reservations_before_writes() {
        let mut c = crate::db::open_in_memory().unwrap();
        let root = std::path::Path::new("C:/Game");
        assert!(
            ensure_generic_targets_allowed(&c, root, &["C:/Game/scripts/a.dll".into()]).is_ok()
        );
        assert!(ensure_generic_targets_allowed(&c, root, &["C:/Game/LML/pkg/a".into()]).is_err());
        let id = begin_operation(&mut c, &input()).unwrap();
        assert!(ensure_root_idle(&c, root).is_err());
        assert!(ensure_root_idle(&c, std::path::Path::new("c:/GAME")).is_err());
        let other = std::path::Path::new("C:/Other");
        for target in ["C:/Stage/Example/a", "C:/Game/lml/Example/a", "C:/Stage"] {
            assert!(ensure_generic_targets_allowed(&c, other, &[target.into()]).is_err());
        }
        assert!(
            ensure_generic_targets_allowed(&c, other, &["C:/Other/scripts/a.dll".into()]).is_ok()
        );
        record_phase(&mut c, id, Phase::Prepared, Phase::Staged, "").unwrap();
        record_phase(&mut c, id, Phase::Staged, Phase::Publishing, "").unwrap();
        register_ownership(&mut c, id, None, "").unwrap();
        assert!(ensure_root_idle(&c, root).is_ok());
        assert!(ensure_generic_targets_allowed(&c, root, &["C:/Stage/Example/a".into()]).is_err());
        assert!(
            ensure_generic_targets_allowed(&c, root, &["C:/Game/scripts/a.dll".into()]).is_ok()
        );
    }
    #[test]
    fn generic_guard_query_errors_fail_closed() {
        let c = Connection::open_in_memory().unwrap();
        assert!(ensure_root_idle(&c, std::path::Path::new("C:/Game")).is_err());
    }
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

    // OpenAI-Codex: Verify guard calls do not execute SQL data changes.
    fn diagnostic_changes(c: &Connection) -> i64 {
        c.query_row("SELECT total_changes()", [], |r| r.get(0))
            .unwrap()
    }

    fn expect_missing_root(result: CoreResult<()>, mod_id: i64) {
        match result {
            Err(CoreError::UnsupportedFormat { reason }) => assert_eq!(
                reason,
                format!(
                    concat!(
                        "mod {} has no recorded game folder; its original installation ",
                        "cannot be verified and automatic recovery is not available yet"
                    ),
                    mod_id
                )
            ),
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[test]
    fn empty_root_zero_and_one_target_keep_rows_and_synthetic_files() {
        let c = crate::db::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("mod.asi");
        let staging = dir.path().join("staging");
        std::fs::write(&target, b"keep").unwrap();
        let mut rows = Vec::new();

        for (status, with_target) in [
            ("active", false),
            ("active", true),
            ("disabled", true),
        ] {
            c.execute(
                "INSERT INTO installed_mod (name, source_type, install_path, status, mode) \
                 VALUES ('blank', 'asi', '', ?1, 'legacy-sp')",
                [status],
            )
            .unwrap();
            let mod_id = c.last_insert_rowid();
            if with_target {
                c.execute(
                    "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) \
                     VALUES (?1, ?2, 'synthetic')",
                    rusqlite::params![mod_id, target.to_string_lossy().as_ref()],
                )
                .unwrap();
            }
            rows.push((mod_id, status, with_target));
        }

        c.pragma_update(None, "query_only", true).unwrap();
        let before = diagnostic_changes(&c);
        for (mod_id, status, with_target) in rows {
            expect_missing_root(ensure_generic_mod_allowed(&c, mod_id), mod_id);
            expect_missing_root(
                crate::providers::ensure_installed_mod_available(&c, mod_id),
                mod_id,
            );
            // OpenAI-Codex_disable_ai_audit: Preserve the reachable missing-root guards.
            if status == "active" {
                assert!(matches!(
                    crate::state::disable(&c, mod_id, &staging),
                    Err(CoreError::ModDisablingUnavailable)
                ));
            } else {
                expect_missing_root(crate::state::enable(&c, mod_id, &staging), mod_id);
            }

            let record: (String, String) = c
                .query_row(
                    "SELECT install_path, status FROM installed_mod WHERE id=?1",
                    [mod_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(record, (String::new(), status.to_string()));
            let target_count: i64 = c
                .query_row(
                    "SELECT count(*) FROM installed_mod_file WHERE installed_mod_id=?1",
                    [mod_id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(target_count, if with_target { 1 } else { 0 });
        }

        let events: i64 = c
            .query_row("SELECT count(*) FROM install_event", [], |r| r.get(0))
            .unwrap();
        assert_eq!(events, 0);
        assert_eq!(diagnostic_changes(&c), before);
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        assert!(!staging.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert!(c.is_autocommit());
    }

    fn registered_blank_mod(mode: &str) -> (Connection, i64) {
        let mut c = crate::db::open_in_memory().unwrap();
        let n = input();
        let operation = begin_operation(&mut c, &n).unwrap();
        record_phase(&mut c, operation, Phase::Prepared, Phase::Staged, "").unwrap();
        record_phase(&mut c, operation, Phase::Staged, Phase::Publishing, "").unwrap();

        c.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status, mode) \
             VALUES ('linked', 'folder', 'C:/Game', 'active', 'legacy-sp')",
            [],
        )
        .unwrap();
        let mod_id = c.last_insert_rowid();
        c.execute(
            "INSERT INTO installed_mod_file (installed_mod_id, target_path, file_hash) \
             VALUES (?1, 'C:/Game/lml/Example/install.xml', ?2)",
            rusqlite::params![mod_id, n.manifest_sha256.as_str()],
        )
        .unwrap();
        register_ownership(&mut c, operation, Some(mod_id), "").unwrap();

        // Simulate an existing association whose linked row later lost its root.
        c.execute(
            "UPDATE installed_mod SET install_path='', mode=?1 WHERE id=?2",
            rusqlite::params![mode, mod_id],
        )
        .unwrap();
        (c, mod_id)
    }

    #[test]
    fn lml_association_precedes_empty_root_diagnostic() {
        let (c, mod_id) = registered_blank_mod("legacy-sp");
        c.pragma_update(None, "query_only", true).unwrap();
        let before = diagnostic_changes(&c);

        for result in [
            ensure_generic_mod_allowed(&c, mod_id),
            crate::providers::ensure_installed_mod_available(&c, mod_id),
        ] {
            match result {
                Err(CoreError::UnsupportedFormat { reason }) => assert_eq!(
                    reason,
                    "LML metadata: LML ownership requires dedicated lifecycle"
                ),
                other => panic!("unexpected result: {other:?}"),
            }
        }

        let association: i64 = c
            .query_row(
                "SELECT count(*) FROM lml_package WHERE installed_mod_id=?1",
                [mod_id],
                |r| r.get(0),
            )
            .unwrap();
        let root: String = c
            .query_row(
                "SELECT install_path FROM installed_mod WHERE id=?1",
                [mod_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(association, 1);
        assert_eq!(root, "");
        assert_eq!(diagnostic_changes(&c), before);
        assert!(c.is_autocommit());
    }

    #[test]
    fn pending_and_unknown_modes_precede_lml_and_empty_root() {
        for (mode, expected) in [
            ("enhanced-sp", "GTA V Enhanced support is under development"),
            ("enhanced-lspdfr", "GTA V Enhanced support is under development"),
            ("fivem-client", "FiveM support is under development"),
            ("fivem-server", "FiveM support is under development"),
            ("fivem-converter", "FiveM support is under development"),
            ("unknown", "unknown workspace mode: unknown"),
        ] {
            let (c, mod_id) = registered_blank_mod(mode);
            c.pragma_update(None, "query_only", true).unwrap();
            let before = diagnostic_changes(&c);

            match crate::providers::ensure_installed_mod_available(&c, mod_id) {
                Err(CoreError::UnsupportedFormat { reason }) => assert_eq!(reason, expected),
                other => panic!("unexpected result for {mode}: {other:?}"),
            }

            assert_eq!(diagnostic_changes(&c), before);
            assert!(c.is_autocommit());
        }
    }

    #[test]
    fn empty_root_preserves_guard_sql_errors_and_association_priority() {
        for case in 0..3 {
            let c = Connection::open_in_memory().unwrap();
            c.execute_batch(
                "CREATE TABLE installed_mod \
                 (id INTEGER PRIMARY KEY, mode TEXT, install_path TEXT NOT NULL);
                 INSERT INTO installed_mod VALUES (1, 'legacy-sp', '');",
            )
            .unwrap();
            if case != 0 {
                c.execute_batch("CREATE TABLE lml_package (installed_mod_id INTEGER);")
                    .unwrap();
            }
            if case == 2 {
                c.execute("INSERT INTO lml_package VALUES (1)", []).unwrap();
            }
            // The installed_mod_file table is intentionally absent.
            c.pragma_update(None, "query_only", true).unwrap();
            let before = diagnostic_changes(&c);

            match (case, crate::providers::ensure_installed_mod_available(&c, 1)) {
                (0 | 1, Err(CoreError::Database(_))) => {}
                (2, Err(CoreError::UnsupportedFormat { reason })) => assert_eq!(
                    reason,
                    "LML metadata: LML ownership requires dedicated lifecycle"
                ),
                (which, other) => panic!("unexpected case {which}: {other:?}"),
            }

            assert_eq!(diagnostic_changes(&c), before);
            assert!(c.is_autocommit());
        }
    }

    #[test]
    fn observed_empty_root_remains_rejected_after_record_changes() {
        let c = crate::db::open_in_memory().unwrap();
        c.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status, mode) \
             VALUES ('changed', 'asi', '', 'active', 'legacy-sp')",
            [],
        )
        .unwrap();
        let mod_id = c.last_insert_rowid();
        let observed: String = c
            .query_row(
                "SELECT install_path FROM installed_mod WHERE id=?1",
                [mod_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(observed, "");

        // Model a change between the provider's observation and its guard call.
        c.execute(
            "UPDATE installed_mod SET install_path='relative/changed' WHERE id=?1",
            [mod_id],
        )
        .unwrap();
        c.pragma_update(None, "query_only", true).unwrap();
        let before = diagnostic_changes(&c);

        match reject_observed_empty_root(&c, mod_id) {
            Err(CoreError::UnsupportedFormat { reason }) => assert_eq!(
                reason,
                format!("mod {mod_id} game folder changed while checking; retry the operation")
            ),
            other => panic!("unexpected result: {other:?}"),
        }

        assert_eq!(diagnostic_changes(&c), before);
        assert!(c.is_autocommit());
    }

    #[test]
    fn nonempty_enhanced_root_remains_blocked_for_legacy_mode() {
        let c = crate::db::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("game");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("GTA5_Enhanced.exe"), b"synthetic").unwrap();
        c.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status, mode) \
             VALUES ('enhanced', 'asi', ?1, 'active', 'legacy-sp')",
            [root.to_str().expect("synthetic game root must be Unicode")],
        )
        .unwrap();
        let mod_id = c.last_insert_rowid();
        c.pragma_update(None, "query_only", true).unwrap();
        let before = diagnostic_changes(&c);

        match crate::providers::ensure_installed_mod_available(&c, mod_id) {
            Err(CoreError::UnsupportedFormat { reason }) => assert_eq!(
                reason,
                "GTA V Enhanced support is under development"
            ),
            other => panic!("unexpected result: {other:?}"),
        }

        assert_eq!(std::fs::read(root.join("GTA5_Enhanced.exe")).unwrap(), b"synthetic");
        assert_eq!(diagnostic_changes(&c), before);
        assert!(c.is_autocommit());
    }

    // OpenAI-Codex: A missing mode does not recover an empty stored root.
    #[test]
    fn null_mode_with_empty_root_is_refused_without_updating_the_row() {
        let c = crate::db::open_in_memory().unwrap();
        c.execute(
            "INSERT INTO installed_mod (name, source_type, install_path, status, mode) \
             VALUES ('unknown', 'asi', '', 'active', NULL)",
            [],
        )
        .unwrap();
        let mod_id = c.last_insert_rowid();
        c.pragma_update(None, "query_only", true).unwrap();
        let before = diagnostic_changes(&c);

        expect_missing_root(
            crate::providers::ensure_installed_mod_available(&c, mod_id),
            mod_id,
        );

        let row: (Option<String>, String) = c
            .query_row(
                "SELECT mode, install_path FROM installed_mod WHERE id=?1",
                [mod_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, (None, String::new()));
        assert_eq!(diagnostic_changes(&c), before);
        assert!(c.is_autocommit());
    }

    // OpenAI-Codex: Accepted Legacy modes do not authorize an empty root.
    #[test]
    fn supported_legacy_modes_with_empty_root_are_refused() {
        let c = crate::db::open_in_memory().unwrap();
        let mut rows = Vec::new();
        for mode in ["legacy-sp", "legacy-lspdfr", "sp", "lspdfr"] {
            c.execute(
                "INSERT INTO installed_mod (name, source_type, install_path, status, mode) \
                 VALUES (?1, 'asi', '', 'active', ?1)",
                [mode],
            )
            .unwrap();
            rows.push((c.last_insert_rowid(), mode));
        }
        c.pragma_update(None, "query_only", true).unwrap();
        let before = diagnostic_changes(&c);

        for (mod_id, mode) in rows {
            expect_missing_root(
                crate::providers::ensure_installed_mod_available(&c, mod_id),
                mod_id,
            );
            let row: (String, String) = c
                .query_row(
                    "SELECT mode, install_path FROM installed_mod WHERE id=?1",
                    [mod_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(row, (mode.to_string(), String::new()));
        }

        assert_eq!(diagnostic_changes(&c), before);
        assert!(c.is_autocommit());
    }
}
