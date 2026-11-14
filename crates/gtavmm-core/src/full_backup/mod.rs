// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex_Responsible: Source_Code
// OpenAI-Codex: Store the source edition in the archive comment and guard App restores.

//! Full backup/restore of the entire `mods\` folder as a single zip — the coarse,
//! whole-folder safety net validated by the old prototype (see
//! `docs/planning`/vault, not part of this repo). Distinct from the per-mod
//! backup/rollback in `install`: this is a manual, on-demand snapshot of everything
//! under `mods\` at once, useful before a risky batch of changes or as a periodic
//! checkpoint, not tied to any single mod's install/uninstall lifecycle.
//!
//! Restore is additive/overwrite — it extracts the zip on top of the existing
//! `mods\` folder, it never deletes files that aren't in the archive. That matches
//! the old prototype's behavior and keeps this a "bring back what was saved," not a
//! "make the folder exactly match the snapshot" operation.
//!
//! Real-world `mods\` folders run tens of gigabytes across a modest number of large
//! files (texture/vehicle replacements). Two consequences of that: (1) files are
//! streamed via `std::io::copy`, never buffered whole into memory; (2) entries are
//! stored **uncompressed** (`CompressionMethod::Stored`) rather than Deflated —
//! most of that content is already-compressed binary data (rpf archives, dds
//! textures), so Deflate mostly burns CPU time for negligible size savings, and this
//! is a safety-net snapshot, not long-term archival storage.

use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions;

use crate::error::{CoreError, CoreResult};

const MODS_SUBFOLDER: &str = "mods";
const LEGACY_BACKUP_MARKER: &[u8] = b"GTAVMM_FULL_BACKUP_V1:legacy";
const ENHANCED_BACKUP_MARKER: &[u8] = b"GTAVMM_FULL_BACKUP_V1:enhanced";

// OpenAI-Codex_v1_readiness: Pause backup entry points without changing stored data.
pub fn ensure_backup_available() -> CoreResult<()> {
    Err(CoreError::BackupUnavailable)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameEdition {
    Legacy,
    Enhanced,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct EditionBackupList {
    pub paths: Vec<PathBuf>,
    pub unmarked_count: usize,
    pub unreadable_count: usize,
}

impl GameEdition {
    fn marker(self) -> &'static str {
        match self {
            Self::Legacy => "GTAVMM_FULL_BACKUP_V1:legacy",
            Self::Enhanced => "GTAVMM_FULL_BACKUP_V1:enhanced",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Legacy => "Legacy",
            Self::Enhanced => "Enhanced",
        }
    }
}

/// Creates an unmarked `<backup_root>/full/mods_<timestamp>.zip` from `<game_root>/mods`.
/// This remains the unscoped CLI helper; the App should call [`create_for_edition`].
/// Returns the created zip's path. Errors if the `mods\` folder doesn't exist (there
/// is nothing to back up yet).
pub fn create(game_root: &Path, backup_root: &Path) -> CoreResult<PathBuf> {
    create_with_edition(game_root, backup_root, None)
}

/// Creates a full backup marked for the specified GTA V edition.
pub fn create_for_edition(
    game_root: &Path,
    backup_root: &Path,
    edition: GameEdition,
) -> CoreResult<PathBuf> {
    create_with_edition(game_root, backup_root, Some(edition))
}

fn create_with_edition(
    game_root: &Path,
    backup_root: &Path,
    edition: Option<GameEdition>,
) -> CoreResult<PathBuf> {
    ensure_backup_available()?;
    create_with_edition_inner(game_root, backup_root, edition)
}

// OpenAI-Codex_test_review: Keep the paused entry point separate from its coverage.
fn create_with_edition_inner(
    game_root: &Path,
    backup_root: &Path,
    edition: Option<GameEdition>,
) -> CoreResult<PathBuf> {
    // OpenAI-Codex: Validate the target and requested edition before creating files.
    crate::providers::ensure_game_root_available(game_root)?;
    if edition == Some(GameEdition::Enhanced) {
        crate::providers::ensure_mode_available("enhanced")?;
    }
    create_with_edition_unrestricted(game_root, backup_root, edition)
}

fn create_with_edition_unrestricted(
    game_root: &Path,
    backup_root: &Path,
    edition: Option<GameEdition>,
) -> CoreResult<PathBuf> {
    create_with_edition_clock(game_root, backup_root, edition, chrono::Utc::now)
}

// OpenAI-Codex: Allow deterministic collision coverage without changing public APIs.
fn create_with_edition_clock(
    game_root: &Path,
    backup_root: &Path,
    edition: Option<GameEdition>,
    now: impl FnOnce() -> chrono::DateTime<chrono::Utc>,
) -> CoreResult<PathBuf> {
    let mods_dir = game_root.join(MODS_SUBFOLDER);
    if !mods_dir.is_dir() {
        return Err(CoreError::UnsupportedFormat {
            reason: format!(
                "{} does not exist yet — nothing to back up",
                mods_dir.display()
            ),
        });
    }

    let full_backup_dir = backup_root.join("full");
    std::fs::create_dir_all(&full_backup_dir)?;
    let zip_path = full_backup_dir.join(format!("mods_{}.zip", now().format("%Y%m%d%H%M%S")));

    // OpenAI-Codex: Preserve occupied backup archives and their error source.
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&zip_path)
        .map_err(backup_open_error)?;
    let mut writer = zip::ZipWriter::new(file);
    if let Some(edition) = edition {
        writer.set_comment(edition.marker());
    }
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    for entry in walkdir::WalkDir::new(&mods_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let relative = entry.path().strip_prefix(&mods_dir).unwrap_or(entry.path());
        // Zip entry names are always forward-slash, even on Windows.
        let entry_name = relative.to_string_lossy().replace('\\', "/");

        writer
            .start_file(entry_name, options)
            .map_err(zip_err("writing zip entry"))?;
        let mut source = std::fs::File::open(entry.path())?;
        std::io::copy(&mut source, &mut writer)?;
    }

    writer.finish().map_err(zip_err("finalizing zip"))?;
    Ok(zip_path)
}

// OpenAI-Codex: Only classify the actual backup-open collision specially.
fn backup_open_error(error: std::io::Error) -> CoreError {
    if error.kind() == std::io::ErrorKind::AlreadyExists {
        CoreError::FullBackupNameCollision(error)
    } else {
        CoreError::Io(error)
    }
}

/// Lists all existing full backups (newest first), including unmarked archives.
pub fn list(backup_root: &Path) -> CoreResult<Vec<PathBuf>> {
    ensure_backup_available()?;
    list_inner(backup_root)
}

fn list_inner(backup_root: &Path) -> CoreResult<Vec<PathBuf>> {
    let full_backup_dir = backup_root.join("full");
    if !full_backup_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&full_backup_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "zip"))
        .collect();
    entries.sort();
    entries.reverse();
    Ok(entries)
}

/// Lists only backups marked for the requested edition. Unmarked archives are
/// excluded because their source edition cannot be verified by the App.
pub fn list_for_edition(backup_root: &Path, edition: GameEdition) -> CoreResult<EditionBackupList> {
    ensure_backup_available()?;
    list_for_edition_inner(backup_root, edition)
}

fn list_for_edition_inner(backup_root: &Path, edition: GameEdition) -> CoreResult<EditionBackupList> {
    let mut result = EditionBackupList::default();
    for path in list_inner(backup_root)? {
        match read_backup_edition(&path) {
            Ok(Some(found)) if found == edition => result.paths.push(path),
            Ok(Some(_)) => {}
            Ok(None) => result.unmarked_count += 1,
            Err(_) => result.unreadable_count += 1,
        }
    }
    Ok(result)
}

/// Extracts `zip_path` on top of `<game_root>/mods` — additive/overwrite, never
/// deletes files that aren't present in the archive. This is the unscoped CLI helper;
/// the App should call [`restore_for_edition`].
pub fn restore(zip_path: &Path, game_root: &Path) -> CoreResult<()> {
    ensure_backup_available()?;
    restore_inner(zip_path, game_root)
}

fn restore_inner(zip_path: &Path, game_root: &Path) -> CoreResult<()> {
    crate::providers::ensure_game_root_available(game_root)?;
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file).map_err(zip_err("reading backup zip"))?;
    if edition_from_archive(&archive) == Some(GameEdition::Enhanced) {
        crate::providers::ensure_mode_available("enhanced")?;
    }
    extract_archive(&mut archive, game_root)
}

/// Restores a full backup only when its edition marker matches the target page.
pub fn restore_for_edition(
    zip_path: &Path,
    game_root: &Path,
    expected_edition: GameEdition,
) -> CoreResult<()> {
    ensure_backup_available()?;
    restore_for_edition_inner(zip_path, game_root, expected_edition)
}

fn restore_for_edition_inner(
    zip_path: &Path,
    game_root: &Path,
    expected_edition: GameEdition,
) -> CoreResult<()> {
    crate::providers::ensure_game_root_available(game_root)?;
    if expected_edition == GameEdition::Enhanced {
        crate::providers::ensure_mode_available("enhanced")?;
    }
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file).map_err(zip_err("reading backup zip"))?;
    let actual_edition = edition_from_archive(&archive);
    match actual_edition {
        Some(edition) if edition == expected_edition => {}
        Some(edition) => {
            return Err(CoreError::UnsupportedFormat {
                reason: format!(
                    "backup edition {} does not match the selected {} edition",
                    edition.label(),
                    expected_edition.label()
                ),
            });
        }
        None => {
            return Err(CoreError::UnsupportedFormat {
                reason: "backup has no supported GTA V edition marker; create a new full backup in the selected edition".to_string(),
            });
        }
    }
    extract_archive(&mut archive, game_root)
}

fn read_backup_edition(zip_path: &Path) -> CoreResult<Option<GameEdition>> {
    let file = std::fs::File::open(zip_path)?;
    let archive = zip::ZipArchive::new(file).map_err(zip_err("reading backup zip"))?;
    Ok(edition_from_archive(&archive))
}

fn edition_from_archive<R: std::io::Read + std::io::Seek>(
    archive: &zip::ZipArchive<R>,
) -> Option<GameEdition> {
    let comment = archive.comment();
    if comment == LEGACY_BACKUP_MARKER {
        Some(GameEdition::Legacy)
    } else if comment == ENHANCED_BACKUP_MARKER {
        Some(GameEdition::Enhanced)
    } else {
        None
    }
}

fn extract_archive<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    game_root: &Path,
) -> CoreResult<()> {
    let mods_dir = game_root.join(MODS_SUBFOLDER);
    std::fs::create_dir_all(&mods_dir)?;
    archive
        .extract(&mods_dir)
        .map_err(zip_err("extracting backup zip"))?;
    Ok(())
}

fn zip_err(context: &'static str) -> impl Fn(zip::result::ZipError) -> CoreError {
    move |e| CoreError::UnsupportedFormat {
        reason: format!("{context}: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // OpenAI-Codex_test_review: Exercise the retained implementation independently of the pause.
    fn create(game_root: &Path, backup_root: &Path) -> CoreResult<PathBuf> {
        create_with_edition_inner(game_root, backup_root, None)
    }

    fn create_for_edition(
        game_root: &Path,
        backup_root: &Path,
        edition: GameEdition,
    ) -> CoreResult<PathBuf> {
        create_with_edition_inner(game_root, backup_root, Some(edition))
    }

    fn list(backup_root: &Path) -> CoreResult<Vec<PathBuf>> {
        list_inner(backup_root)
    }

    fn list_for_edition(backup_root: &Path, edition: GameEdition) -> CoreResult<EditionBackupList> {
        list_for_edition_inner(backup_root, edition)
    }

    fn restore(zip_path: &Path, game_root: &Path) -> CoreResult<()> {
        restore_inner(zip_path, game_root)
    }

    fn restore_for_edition(
        zip_path: &Path,
        game_root: &Path,
        edition: GameEdition,
    ) -> CoreResult<()> {
        restore_for_edition_inner(zip_path, game_root, edition)
    }

    #[test]
    fn pending_backup_operations_do_not_create_target_directories() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path().join("game");
        let backups = dir.path().join("backups");
        assert!(matches!(
            super::create_for_edition(&game, &backups, GameEdition::Enhanced),
            Err(CoreError::BackupUnavailable)
        ));
        assert!(!backups.exists());
        assert!(matches!(
            super::restore_for_edition(
                &dir.path().join("missing.zip"),
                &game,
                GameEdition::Enhanced
            ),
            Err(CoreError::BackupUnavailable)
        ));
        assert!(!game.exists());
        assert!(matches!(
            super::create(&game, &backups),
            Err(CoreError::BackupUnavailable)
        ));
        assert!(matches!(
            super::list(&backups),
            Err(CoreError::BackupUnavailable)
        ));
        assert!(matches!(
            super::list_for_edition(&backups, GameEdition::Legacy),
            Err(CoreError::BackupUnavailable)
        ));
        assert!(matches!(
            super::restore(&dir.path().join("missing.zip"), &game),
            Err(CoreError::BackupUnavailable)
        ));
        assert!(!backups.exists());
        assert!(!game.exists());
    }

    // OpenAI-Codex_test_review: Paused operations preserve existing archive and game bytes.
    #[test]
    fn pending_backup_operations_preserve_existing_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path().join("game");
        let backups = dir.path().join("backups");
        let sentinel = game.join("mods/sentinel.asi");
        let archive = backups.join("full/existing.zip");
        std::fs::create_dir_all(sentinel.parent().unwrap()).unwrap();
        std::fs::create_dir_all(archive.parent().unwrap()).unwrap();
        std::fs::write(&sentinel, b"game sentinel").unwrap();
        std::fs::write(&archive, b"archive sentinel").unwrap();

        assert!(matches!(
            super::create(&game, &backups),
            Err(CoreError::BackupUnavailable)
        ));
        assert!(matches!(
            super::create_for_edition(&game, &backups, GameEdition::Legacy),
            Err(CoreError::BackupUnavailable)
        ));
        assert!(matches!(
            super::list(&backups),
            Err(CoreError::BackupUnavailable)
        ));
        assert!(matches!(
            super::list_for_edition(&backups, GameEdition::Legacy),
            Err(CoreError::BackupUnavailable)
        ));
        assert!(matches!(
            super::restore(&archive, &game),
            Err(CoreError::BackupUnavailable)
        ));
        assert!(matches!(
            super::restore_for_edition(&archive, &game, GameEdition::Legacy),
            Err(CoreError::BackupUnavailable)
        ));
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"game sentinel");
        assert_eq!(std::fs::read(&archive).unwrap(), b"archive sentinel");
    }

    #[test]
    fn create_fails_when_mods_folder_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let err = create(&game_root, &dir.path().join("backups")).unwrap_err();
        assert!(matches!(err, CoreError::UnsupportedFormat { .. }));
    }

    // OpenAI-Codex: Same-name collisions retain the first full backup.
    #[test]
    fn same_second_backup_collision_preserves_first_archive() {
        use std::io::Read;

        fn fixed_backup_time() -> chrono::DateTime<chrono::Utc> {
            chrono::DateTime::parse_from_rfc3339("2026-01-02T03:04:05Z")
                .unwrap()
                .with_timezone(&chrono::Utc)
        }

        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        let mods_dir = game_root.join("mods");
        std::fs::create_dir_all(&mods_dir).unwrap();
        std::fs::write(mods_dir.join("mod.asi"), b"original payload").unwrap();
        let backup_root = dir.path().join("backups");

        let first = create_with_edition_clock(
            &game_root,
            &backup_root,
            Some(GameEdition::Legacy),
            fixed_backup_time,
        )
        .unwrap();
        let before = std::fs::read(&first).unwrap();
        let error = create_with_edition_clock(&game_root, &backup_root, None, fixed_backup_time)
            .unwrap_err();
        assert!(matches!(
            &error,
            CoreError::FullBackupNameCollision(source)
                if source.kind() == std::io::ErrorKind::AlreadyExists
        ));
        assert_eq!(
            error.to_string(),
            "a full backup filename for this second is already in use; retry later"
        );
        assert_eq!(std::fs::read(&first).unwrap(), before);
        assert_eq!(
            std::fs::read(mods_dir.join("mod.asi")).unwrap(),
            b"original payload"
        );

        let mut archive = zip::ZipArchive::new(std::fs::File::open(&first).unwrap()).unwrap();
        assert_eq!(archive.comment(), LEGACY_BACKUP_MARKER);
        let mut restored = Vec::new();
        archive
            .by_name("mod.asi")
            .unwrap()
            .read_to_end(&mut restored)
            .unwrap();
        assert_eq!(restored, b"original payload");
    }

    // OpenAI-Codex: Non-collision open errors retain their original I/O detail.
    #[test]
    fn backup_open_error_preserves_non_collision_io() {
        let error = backup_open_error(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "synthetic permission failure",
        ));
        assert!(matches!(
            error,
            CoreError::Io(source)
                if source.kind() == std::io::ErrorKind::PermissionDenied
                    && source.to_string() == "synthetic permission failure"
        ));
    }

    #[test]
    fn create_then_list_then_restore_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        let mods_dir = game_root.join("mods");
        std::fs::create_dir_all(mods_dir.join("update/x64/dlcpacks/mymod")).unwrap();
        std::fs::write(
            mods_dir.join("update/x64/dlcpacks/mymod/dlc.rpf"),
            b"payload",
        )
        .unwrap();
        std::fs::write(mods_dir.join("root_file.asi"), b"asi payload").unwrap();
        let backup_root = dir.path().join("backups");

        let zip_path = create(&game_root, &backup_root).unwrap();
        assert!(zip_path.exists());

        let backups = list(&backup_root).unwrap();
        assert_eq!(backups, vec![zip_path.clone()]);

        // Simulate loss: delete the mods folder entirely, then restore.
        std::fs::remove_dir_all(&mods_dir).unwrap();
        assert!(!mods_dir.exists());

        restore(&zip_path, &game_root).unwrap();

        assert_eq!(
            std::fs::read(mods_dir.join("update/x64/dlcpacks/mymod/dlc.rpf")).unwrap(),
            b"payload"
        );
        assert_eq!(
            std::fs::read(mods_dir.join("root_file.asi")).unwrap(),
            b"asi payload"
        );
    }

    #[test]
    fn restore_is_additive_and_does_not_delete_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        let mods_dir = game_root.join("mods");
        std::fs::create_dir_all(&mods_dir).unwrap();
        std::fs::write(mods_dir.join("a.asi"), b"a").unwrap();
        let backup_root = dir.path().join("backups");
        let zip_path = create(&game_root, &backup_root).unwrap();

        // A file added *after* the backup was taken should survive a restore.
        std::fs::write(mods_dir.join("b.asi"), b"b").unwrap();

        restore(&zip_path, &game_root).unwrap();

        assert!(mods_dir.join("a.asi").exists());
        assert!(
            mods_dir.join("b.asi").exists(),
            "restore must not delete files absent from the backup"
        );
    }

    #[test]
    fn list_is_empty_when_no_backups_exist() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list(&dir.path().join("backups")).unwrap().is_empty());
    }

    #[test]
    fn edition_listing_excludes_other_and_unmarked_backups() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(game_root.join("mods")).unwrap();
        std::fs::write(game_root.join("mods/mod.asi"), b"payload").unwrap();
        let backup_root = dir.path().join("backups");

        let legacy = create_for_edition(&game_root, &backup_root, GameEdition::Legacy).unwrap();
        let legacy_path = legacy.with_file_name("legacy.zip");
        std::fs::rename(legacy, &legacy_path).unwrap();
        // OpenAI-Codex: Build an old-format fixture without enabling the pending API.
        let enhanced =
            create_with_edition_unrestricted(&game_root, &backup_root, Some(GameEdition::Enhanced))
                .unwrap();
        let enhanced_path = enhanced.with_file_name("enhanced.zip");
        std::fs::rename(enhanced, &enhanced_path).unwrap();
        let unmarked = create(&game_root, &backup_root).unwrap();
        std::fs::rename(&unmarked, unmarked.with_file_name("unmarked.zip")).unwrap();

        let legacy_listing = list_for_edition(&backup_root, GameEdition::Legacy).unwrap();
        assert_eq!(legacy_listing.paths, vec![legacy_path]);
        assert_eq!(legacy_listing.unmarked_count, 1);
        assert_eq!(legacy_listing.unreadable_count, 0);

        let enhanced_listing = list_for_edition(&backup_root, GameEdition::Enhanced).unwrap();
        assert_eq!(enhanced_listing.paths, vec![enhanced_path]);
        assert_eq!(enhanced_listing.unmarked_count, 1);
        assert_eq!(enhanced_listing.unreadable_count, 0);
    }

    #[test]
    fn edition_listing_skips_unreadable_archives_and_reports_them() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(game_root.join("mods")).unwrap();
        std::fs::write(game_root.join("mods/mod.asi"), b"payload").unwrap();
        let backup_root = dir.path().join("backups");
        let valid = create_for_edition(&game_root, &backup_root, GameEdition::Legacy).unwrap();
        let full_backup_dir = backup_root.join("full");
        std::fs::write(full_backup_dir.join("partial.zip"), b"incomplete archive").unwrap();

        let listing = list_for_edition(&backup_root, GameEdition::Legacy).unwrap();

        assert_eq!(listing.paths, vec![valid]);
        assert_eq!(listing.unreadable_count, 1);
    }

    #[test]
    fn edition_restore_rejects_mismatch_and_unmarked_archives_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("legacy-game");
        std::fs::create_dir_all(game_root.join("mods")).unwrap();
        std::fs::write(game_root.join("mods/mod.asi"), b"payload").unwrap();
        let backup_root = dir.path().join("backups");
        let legacy = create_for_edition(&game_root, &backup_root, GameEdition::Legacy).unwrap();

        let enhanced_target = dir.path().join("enhanced-target");
        let mismatch = restore_for_edition(&legacy, &enhanced_target, GameEdition::Enhanced)
            .unwrap_err()
            .to_string();
        assert!(mismatch.contains("under development"));
        assert!(mismatch.contains("Enhanced"));
        assert!(!enhanced_target.join(MODS_SUBFOLDER).exists());

        let unmarked = create(&game_root, &dir.path().join("unmarked-backups")).unwrap();
        let unmarked_target = dir.path().join("unmarked-target");
        let missing_marker = restore_for_edition(&unmarked, &unmarked_target, GameEdition::Legacy)
            .unwrap_err()
            .to_string();
        assert!(missing_marker.contains("no supported GTA V edition marker"));
        assert!(!unmarked_target.join(MODS_SUBFOLDER).exists());
    }
}
