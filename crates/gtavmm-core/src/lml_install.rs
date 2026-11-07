// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex

//! Staging only: no publication, active-row creation, deletion or automatic repair.

use crate::{
    error::{CoreError, CoreResult},
    lml_fs,
    lml_package::{self, HashedPackageSnapshot},
    lml_store::{self, NewOperation, OperationSnapshot, Phase},
};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct LmlStageContext {
    pub mode: String,
    pub game_root: PathBuf,
    pub target: String,
    /// Existing, explicitly chosen storage parent. Never created implicitly.
    pub staging_parent: PathBuf,
    /// Existing backup/recycle/other storage roots the caller must protect.
    pub protected_storage_roots: Vec<PathBuf>,
}

// OpenAI-Codex: Preserve experimental output types while staging is test-only.
#[cfg_attr(test, allow(dead_code))]
#[derive(Debug, Clone)]
pub struct StagedPackage {
    pub operation_id: i64,
    pub stage_path: PathBuf,
    pub fingerprint: String,
    pub expected_bytes: u64,
}

#[cfg_attr(test, allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageObservation {
    Absent,
    Verified,
    ChangedOrUnreadable(String),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAssessment {
    VerifiedStaged,
    RequiresReview,
}
#[cfg_attr(test, allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnershipObservation {
    Unassociated,
    ConsistentMetadata,
    ChangedOrUnreadable(String),
}
#[cfg_attr(test, allow(dead_code))]
#[derive(Debug, Clone)]
pub struct LmlRecoveryReport {
    pub operation_id: i64,
    pub metadata_phase: Option<Phase>,
    pub metadata_registered: bool,
    pub associated_mod_id: Option<i64>,
    pub ownership: OwnershipObservation,
    pub stage: PackageObservation,
    pub destination: PackageObservation,
    pub assessment: RecoveryAssessment,
}

fn invalid(message: &str) -> CoreError {
    CoreError::UnsupportedFormat {
        reason: format!("LML staging: {message}"),
    }
}

// Conservative lexical overlap after checked canonicalization. This is not an
// operating-system name identity or race-free handle comparison.
fn overlaps(left: &str, right: &str) -> bool {
    fn one(left: &str, right: &str) -> bool {
        left == right
            || left.starts_with(&format!("{right}/"))
            || right.starts_with(&format!("{left}/"))
    }
    one(&left.to_lowercase(), &right.to_lowercase())
        || one(&left.to_uppercase(), &right.to_uppercase())
}

struct ContextPaths {
    game: PathBuf,
    parent: PathBuf,
    root_key: String,
    parent_key: String,
}

fn context_paths(context: &LmlStageContext) -> CoreResult<ContextPaths> {
    if !matches!(context.mode.as_str(), "legacy-sp" | "legacy-lspdfr") {
        return Err(invalid("only Legacy SP and Legacy LSPDFR are available"));
    }
    crate::lml::LocalReference::parse(&context.target)
        .map_err(|_| invalid("unsafe package folder"))?;
    if context.target.contains('/') {
        return Err(invalid("package folder must be one component"));
    }
    let game = lml_fs::checked_directory(&context.game_root)?;
    let parent = lml_fs::checked_directory(&context.staging_parent)?;
    let root_key = lml_fs::lexical_path_key(&game)?;
    let parent_key = lml_fs::lexical_path_key(&parent)?;
    if overlaps(&parent_key, &format!("{root_key}/lml"))
        || root_key
            .to_uppercase()
            .starts_with(&format!("{}/", parent_key.to_uppercase()))
        || root_key.eq_ignore_ascii_case(&parent_key)
    {
        return Err(invalid(
            "staging parent overlaps the game or loader-visible storage",
        ));
    }
    for protected in &context.protected_storage_roots {
        let key = lml_fs::lexical_path_key(&lml_fs::checked_directory(protected)?)?;
        if overlaps(&parent_key, &key) {
            return Err(invalid("staging overlaps protected storage"));
        }
    }
    Ok(ContextPaths {
        game,
        parent,
        root_key,
        parent_key,
    })
}

/// Persist the intent before creating a private stage leaf. Failure preserves all
/// evidence; callers must explicitly arrange any later recovery or cleanup.
pub fn stage_package(
    conn: &mut Connection,
    snapshot: &HashedPackageSnapshot,
    context: &LmlStageContext,
) -> CoreResult<StagedPackage> {
    let paths = context_paths(context)?;
    let source_key = lml_fs::lexical_path_key(&lml_fs::checked_directory(
        &snapshot.inventory().source_root,
    )?)?;
    if overlaps(&source_key, &paths.root_key) || overlaps(&source_key, &paths.parent_key) {
        return Err(invalid("source overlaps game or staging storage"));
    }
    lml_store::ensure_root_idle(conn, &paths.game)?;
    lml_package::revalidate_package(snapshot)?;
    let stage_path = paths.parent.join(format!("lml-{}", snapshot.fingerprint()));
    let stage_key = lml_fs::lexical_path_key(&stage_path)?;
    let destination_key = format!("{}/lml/{}", paths.root_key, context.target);
    for reservation in lml_store::reservations(conn)? {
        let package_key = format!(
            "{}/lml/{}",
            reservation.header.root_key, reservation.header.target
        );
        for reserved in [&reservation.header.staging_path, &package_key] {
            if overlaps(&stage_key, reserved)
                || overlaps(&destination_key, reserved)
                || overlaps(&source_key, reserved)
            {
                return Err(invalid("source or target overlaps reserved storage"));
            }
        }
    }
    match std::fs::symlink_metadata(&stage_path) {
        Ok(_) => return Err(invalid("stage leaf already exists; preserve it for review")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let operation = NewOperation {
        mode: context.mode.clone(),
        root_key: paths.root_key.clone(),
        root_display: paths.root_key,
        target: context.target.clone(),
        staging_path: stage_key,
        manifest: snapshot.inventory().manifest_bytes.clone(),
        manifest_sha256: snapshot
            .files()
            .iter()
            .find(|file| file.relative_path.eq_ignore_ascii_case("install.xml"))
            .ok_or_else(|| invalid("manifest absent from snapshot"))?
            .sha256
            .clone(),
        unresolved_coverage: true,
        files: snapshot.files().to_vec(),
    };
    let operation_id = lml_store::begin_operation(conn, &operation)?;
    let result: CoreResult<StagedPackage> = (|| {
        lml_fs::stage_verified(snapshot, &stage_path)?;
        lml_store::record_phase(conn, operation_id, Phase::Prepared, Phase::Staged, "")?;
        Ok(StagedPackage {
            operation_id,
            stage_path,
            fingerprint: snapshot.fingerprint().to_owned(),
            expected_bytes: snapshot.inventory().total_bytes,
        })
    })();
    match result {
        Ok(staged) => Ok(staged),
        Err(error) => {
            // Keep diagnostics bounded without truncating inside a UTF-8 scalar.
            let diagnostic: String = error.to_string().chars().take(512).collect();
            match lml_store::record_phase(
                conn,
                operation_id,
                Phase::Prepared,
                Phase::RecoveryRequired,
                &diagnostic,
            ) {
                Ok(()) => Err(error),
                Err(journal_error) => Err(invalid(&format!(
                    "{error}; recovery journal also failed: {journal_error}"
                ))),
            }
        }
    }
}

fn observe(path: &Path, operation: &OperationSnapshot) -> PackageObservation {
    // Absence is only trusted after checking the nearest existing ancestor. A
    // dangling intermediate link must not masquerade as an absent destination.
    let mut existing = path;
    let mut missing = false;
    loop {
        match std::fs::symlink_metadata(existing) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing = true;
                let Some(parent) = existing.parent() else {
                    return PackageObservation::ChangedOrUnreadable(
                        "no trustworthy ancestor".into(),
                    );
                };
                existing = parent;
            }
            Err(error) => return PackageObservation::ChangedOrUnreadable(error.to_string()),
            Ok(_) if missing => {
                return match lml_fs::checked_directory(existing) {
                    Ok(_) => PackageObservation::Absent,
                    Err(error) => PackageObservation::ChangedOrUnreadable(error.to_string()),
                }
            }
            Ok(_) => break,
        }
    }
    match lml_package::hash_package(path) {
        Ok(snapshot)
            if snapshot.inventory().manifest_bytes == operation.header.manifest
                && snapshot.files() == operation.header.files.as_slice() =>
        {
            PackageObservation::Verified
        }
        Ok(_) => PackageObservation::ChangedOrUnreadable("complete file snapshot differs".into()),
        Err(error) => PackageObservation::ChangedOrUnreadable(error.to_string()),
    }
}

fn observe_ownership(conn: &Connection, operation: &OperationSnapshot) -> OwnershipObservation {
    let Some(id) = operation.installed_mod_id else {
        return if operation.registered {
            OwnershipObservation::ChangedOrUnreadable(
                "registered metadata has no associated mod".into(),
            )
        } else {
            OwnershipObservation::Unassociated
        };
    };
    let result = (|| -> CoreResult<()> {
        let tx = conn.unchecked_transaction()?;
        let (mode, root, status): (Option<String>, String, String) = tx.query_row(
            "SELECT mode,install_path,status FROM installed_mod WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if !operation.registered
            || operation.events.last().map(|event| event.phase) != Some(Phase::Registered)
            || mode.as_deref() != Some(operation.header.mode.as_str())
            || status != "active"
            || lml_fs::lexical_path_key(Path::new(&root))? != operation.header.root_display
        {
            return Err(invalid("associated mod metadata differs"));
        }
        let mut statement = tx.prepare("SELECT target_path,file_hash FROM installed_mod_file WHERE installed_mod_id=?1 ORDER BY target_path LIMIT 10001")?;
        let files = statement
            .query_map([id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let expected = operation
            .header
            .files
            .iter()
            .map(|file| {
                (
                    format!(
                        "{}/lml/{}/{}",
                        operation.header.root_display, operation.header.target, file.relative_path
                    ),
                    file.sha256.clone(),
                )
            })
            .collect::<std::collections::BTreeSet<_>>();
        let actual = files
            .iter()
            .map(|(path, hash)| Ok((lml_fs::lexical_path_key(Path::new(path))?, hash.clone())))
            .collect::<CoreResult<std::collections::BTreeSet<_>>>()?;
        if files.len() != operation.header.files.len() || actual != expected {
            return Err(invalid("associated file metadata differs"));
        }
        tx.commit()?;
        Ok(())
    })();
    match result {
        Ok(()) => OwnershipObservation::ConsistentMetadata,
        Err(error) => OwnershipObservation::ChangedOrUnreadable(error.to_string()),
    }
}

/// Observe both trees without changing files, phases or reservations. Registered
/// metadata never substitutes for a successful filesystem observation.
pub fn inspect_staged_operation(
    conn: &Connection,
    id: i64,
    context: &LmlStageContext,
) -> CoreResult<LmlRecoveryReport> {
    let paths = context_paths(context)?;
    let operation =
        lml_store::load_operation(conn, id)?.ok_or_else(|| invalid("operation does not exist"))?;
    if operation.header.mode != context.mode
        || operation.header.root_key != paths.root_key
        || operation.header.target != context.target
    {
        return Err(invalid("context differs from recorded operation"));
    }
    let stage_path = Path::new(&operation.header.staging_path);
    let stage_parent = stage_path
        .parent()
        .ok_or_else(|| invalid("stage lacks a parent"))?;
    if lml_fs::lexical_path_key(stage_parent)? != paths.parent_key {
        return Err(invalid(
            "stage is not a direct child of the selected parent",
        ));
    }
    let phase = operation.events.last().map(|event| event.phase);
    let stage = observe(stage_path, &operation);
    let destination = observe(&paths.game.join("lml").join(&context.target), &operation);
    let ownership = observe_ownership(conn, &operation);
    let assessment = if phase == Some(Phase::Staged)
        && ownership == OwnershipObservation::Unassociated
        && stage == PackageObservation::Verified
        && destination == PackageObservation::Absent
    {
        RecoveryAssessment::VerifiedStaged
    } else {
        RecoveryAssessment::RequiresReview
    };
    Ok(LmlRecoveryReport {
        operation_id: id,
        metadata_phase: phase,
        metadata_registered: operation.registered,
        associated_mod_id: operation.installed_mod_id,
        ownership,
        stage,
        destination,
        assessment,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (
        tempfile::TempDir,
        LmlStageContext,
        crate::lml_package::HashedPackageSnapshot,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let game = temp.path().join("game");
        let staging = temp.path().join("staging");
        for path in [&source, &game, &staging] {
            std::fs::create_dir(path).unwrap();
        }
        std::fs::write(source.join("install.xml"), b"<EasyInstall><Addons><Addon><StreamingFile>a.ytd</StreamingFile></Addon></Addons></EasyInstall>").unwrap();
        std::fs::write(source.join("a.ytd"), b"payload").unwrap();
        let snapshot = crate::lml_package::hash_package(&source).unwrap();
        let context = LmlStageContext {
            mode: "legacy-sp".into(),
            game_root: game,
            target: "Example".into(),
            staging_parent: staging,
            protected_storage_roots: Vec::new(),
        };
        (temp, context, snapshot)
    }

    #[test]
    fn stages_whole_package_without_publication_or_active_rows() {
        let (_temp, context, snapshot) = fixture();
        let mut conn = crate::db::open_in_memory().unwrap();
        let staged = stage_package(&mut conn, &snapshot, &context).unwrap();
        assert_eq!(
            std::fs::read(staged.stage_path.join("a.ytd")).unwrap(),
            b"payload"
        );
        assert!(!context.game_root.join("lml").exists());
        assert_eq!(
            conn.query_row("SELECT count(*) FROM installed_mod", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let record = crate::lml_store::load_operation(&conn, staged.operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            record.events.last().unwrap().phase,
            crate::lml_store::Phase::Staged
        );
        let report = inspect_staged_operation(&conn, staged.operation_id, &context).unwrap();
        assert_eq!(report.assessment, RecoveryAssessment::VerifiedStaged);
        assert!(stage_package(&mut conn, &snapshot, &context).is_err());
    }

    #[test]
    fn changed_source_and_existing_stage_are_never_adopted() {
        let (_temp, context, snapshot) = fixture();
        let mut conn = crate::db::open_in_memory().unwrap();
        let leaf = context
            .staging_parent
            .join(format!("lml-{}", snapshot.fingerprint()));
        std::fs::create_dir(&leaf).unwrap();
        std::fs::write(leaf.join("external"), b"keep").unwrap();
        assert!(stage_package(&mut conn, &snapshot, &context).is_err());
        assert_eq!(std::fs::read(leaf.join("external")).unwrap(), b"keep");
        assert_eq!(
            conn.query_row("SELECT count(*) FROM lml_operation", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        std::fs::write(snapshot.inventory().source_root.join("a.ytd"), b"changed").unwrap();
        assert!(stage_package(&mut conn, &snapshot, &context).is_err());
    }

    #[test]
    fn recovery_observations_preserve_extra_files_and_unknown_destinations() {
        let (_temp, context, snapshot) = fixture();
        let mut conn = crate::db::open_in_memory().unwrap();
        let staged = stage_package(&mut conn, &snapshot, &context).unwrap();
        std::fs::write(staged.stage_path.join("external"), b"keep").unwrap();
        let destination = context.game_root.join("lml").join(&context.target);
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(destination.join("external"), b"keep destination").unwrap();
        let report = inspect_staged_operation(&conn, staged.operation_id, &context).unwrap();
        assert_eq!(report.assessment, RecoveryAssessment::RequiresReview);
        assert!(matches!(
            report.stage,
            PackageObservation::ChangedOrUnreadable(_)
        ));
        assert!(matches!(
            report.destination,
            PackageObservation::ChangedOrUnreadable(_)
        ));
        assert_eq!(
            std::fs::read(destination.join("external")).unwrap(),
            b"keep destination"
        );
        assert!(staged.stage_path.join("external").exists());
        assert_eq!(
            crate::lml_store::load_operation(&conn, staged.operation_id)
                .unwrap()
                .unwrap()
                .events
                .len(),
            2
        );
    }

    #[test]
    fn failed_staged_event_keeps_payload_and_recovery_record() {
        let (_temp, context, snapshot) = fixture();
        let mut conn = crate::db::open_in_memory().unwrap();
        conn.execute_batch("CREATE TRIGGER fail_staged BEFORE INSERT ON lml_operation_event WHEN NEW.phase='staged' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
        assert!(stage_package(&mut conn, &snapshot, &context).is_err());
        let root = crate::lml_fs::lexical_path_key(
            &crate::lml_fs::checked_directory(&context.game_root).unwrap(),
        )
        .unwrap();
        let rows = crate::lml_store::list_unfinished_for_root(&conn, &root).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].events.last().unwrap().phase,
            crate::lml_store::Phase::RecoveryRequired
        );
        assert!(std::path::Path::new(&rows[0].header.staging_path)
            .join("a.ytd")
            .exists());
        assert!(!context.game_root.join("lml").exists());
    }

    #[test]
    fn unsafe_context_is_rejected_before_reservation_or_stage_creation() {
        let (_temp, mut context, snapshot) = fixture();
        let mut conn = crate::db::open_in_memory().unwrap();
        context.target = "a/b".into();
        assert!(stage_package(&mut conn, &snapshot, &context).is_err());
        context.target = "Example".into();
        context
            .protected_storage_roots
            .push(context.staging_parent.clone());
        assert!(stage_package(&mut conn, &snapshot, &context).is_err());
        assert_eq!(
            conn.query_row("SELECT count(*) FROM lml_operation", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_refuses_a_dangling_loader_directory_link() {
        let (_temp, context, snapshot) = fixture();
        let mut conn = crate::db::open_in_memory().unwrap();
        let staged = stage_package(&mut conn, &snapshot, &context).unwrap();
        std::os::unix::fs::symlink(
            context.game_root.join("absent"),
            context.game_root.join("lml"),
        )
        .unwrap();
        let report = inspect_staged_operation(&conn, staged.operation_id, &context).unwrap();
        assert_eq!(report.assessment, RecoveryAssessment::RequiresReview);
        assert!(matches!(
            report.destination,
            PackageObservation::ChangedOrUnreadable(_)
        ));
    }

    #[test]
    fn registered_metadata_without_a_mod_is_not_consistent_ownership() {
        let (_temp, context, snapshot) = fixture();
        let mut conn = crate::db::open_in_memory().unwrap();
        let staged = stage_package(&mut conn, &snapshot, &context).unwrap();
        crate::lml_store::record_phase(
            &mut conn,
            staged.operation_id,
            Phase::Staged,
            Phase::Publishing,
            "",
        )
        .unwrap();
        crate::lml_store::register_ownership(&mut conn, staged.operation_id, None, "").unwrap();
        let report = inspect_staged_operation(&conn, staged.operation_id, &context).unwrap();
        assert_eq!(report.assessment, RecoveryAssessment::RequiresReview);
        assert!(matches!(
            report.ownership,
            OwnershipObservation::ChangedOrUnreadable(_)
        ));
    }

    #[test]
    fn lspdfr_stage_and_registered_file_metadata_are_observed_separately() {
        let (_temp, mut context, snapshot) = fixture();
        context.mode = "legacy-lspdfr".into();
        let mut conn = crate::db::open_in_memory().unwrap();
        let staged = stage_package(&mut conn, &snapshot, &context).unwrap();
        let record = crate::lml_store::load_operation(&conn, staged.operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(record.header.mode, "legacy-lspdfr");
        assert_eq!(
            inspect_staged_operation(&conn, staged.operation_id, &context)
                .unwrap()
                .assessment,
            RecoveryAssessment::VerifiedStaged
        );
        conn.execute("INSERT INTO installed_mod(name,source_type,install_path,status,mode) VALUES('Synthetic','folder',?1,'active','legacy-lspdfr')", [&record.header.root_display]).unwrap();
        let mod_id = conn.last_insert_rowid();
        for file in &record.header.files {
            let target = format!(
                "{}/lml/{}/{}",
                record.header.root_display, context.target, file.relative_path
            );
            conn.execute("INSERT INTO installed_mod_file(installed_mod_id,target_path,file_hash) VALUES(?1,?2,?3)", rusqlite::params![mod_id, target, file.sha256]).unwrap();
        }
        crate::lml_store::record_phase(
            &mut conn,
            staged.operation_id,
            Phase::Staged,
            Phase::Publishing,
            "synthetic metadata only",
        )
        .unwrap();
        crate::lml_store::register_ownership(&mut conn, staged.operation_id, Some(mod_id), "")
            .unwrap();
        let report = inspect_staged_operation(&conn, staged.operation_id, &context).unwrap();
        assert_eq!(report.ownership, OwnershipObservation::ConsistentMetadata);
        assert_eq!(report.destination, PackageObservation::Absent);
        assert_eq!(report.assessment, RecoveryAssessment::RequiresReview);
        conn.execute(
            "UPDATE installed_mod_file SET file_hash='changed' WHERE installed_mod_id=?1",
            [mod_id],
        )
        .unwrap();
        assert!(matches!(
            inspect_staged_operation(&conn, staged.operation_id, &context)
                .unwrap()
                .ownership,
            OwnershipObservation::ChangedOrUnreadable(_)
        ));
    }
}
