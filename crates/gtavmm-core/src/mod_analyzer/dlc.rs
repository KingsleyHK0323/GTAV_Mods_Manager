// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex
// OpenAI-Codex_Responsible: Source_Code

//! Add-on content packs (add-on vehicles, add-on maps, and anything else shipped as a
//! standalone `dlc.rpf`) — distinct from *replace*-type mods, which just overwrite an
//! existing file at a mirrored path and need no registration anywhere.
//!
//! An add-on pack is a standalone `.rpf` archive copied into
//! `mods\update\x64\dlcpacks\<name>\`. Its corresponding
//! `<Item>dlcpacks:\<name>\</Item>` registration is managed manually.
//!
//! This module discovers pack directories and offers read-only text inspection.
//! It does not edit XML or RPF archives. The file lifecycle records manual work
//! through `crate::dlc_manual` without requiring an exported `dlclist.xml` to exist.
//! The retained loose-path helper is used by synthetic tests; it does not establish
//! that the game loads a loose XML file at that location. Users provide their own
//! exported XML and import edits with their existing archive tool.

use std::path::{Path, PathBuf};

use crate::error::{CoreError, CoreResult};

/// Looks for `dlc.rpf` directly inside `folder`, or one level down (a common
/// packaging convention: `MyAddonCar/dlc.rpf` or `MyAddonCar/mymod/dlc.rpf`).
/// Returns the *pack directory* (the folder that directly contains `dlc.rpf`), whose
/// name becomes the add-on's registered name.
pub fn find_dlc_pack_dir(folder: &Path) -> Option<PathBuf> {
    if folder.join("dlc.rpf").is_file() {
        return Some(folder.to_path_buf());
    }
    std::fs::read_dir(folder)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path| path.is_dir() && path.join("dlc.rpf").is_file())
}

/// The name an add-on pack is registered under, derived from its containing folder.
pub fn pack_name(pack_dir: &Path) -> CoreResult<String> {
    pack_dir
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| CoreError::UnsupportedFormat {
            reason: "add-on pack folder has no usable name".to_string(),
        })
}

/// Loose XML fixture path retained for lifecycle tests, not an RPF archive entry.
pub fn dlclist_path(game_root: &Path) -> PathBuf {
    game_root
        .join("mods")
        .join("update")
        .join("update.rpf")
        .join("common")
        .join("data")
        .join("dlclist.xml")
}

fn entry_line(name: &str) -> String {
    format!(r"dlcpacks:\{name}\")
}

/// Whether a UTF-8 text file contains the requested pack-path string.
/// This does not validate XML structure or prove registration in the game.
pub fn has_entry(path: &Path, name: &str) -> CoreResult<bool> {
    let contents = std::fs::read_to_string(path)?;
    Ok(contents.contains(&entry_line(name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_dlclist() -> &'static str {
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <SMandatoryPacksData>\n  <Paths>\n    <Item>dlcpacks:\\mpchristmas2017\\</Item>\n  </Paths>\n</SMandatoryPacksData>\n"
    }

    #[test]
    fn finds_dlc_rpf_at_top_level() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("dlc.rpf"), b"payload").unwrap();
        assert_eq!(
            find_dlc_pack_dir(dir.path()),
            Some(dir.path().to_path_buf())
        );
    }

    #[test]
    fn finds_dlc_rpf_one_level_down() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("MyAddonCar");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("dlc.rpf"), b"payload").unwrap();
        assert_eq!(find_dlc_pack_dir(dir.path()), Some(nested));
    }

    #[test]
    fn returns_none_when_no_dlc_rpf_anywhere() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("readme.txt"), b"hi").unwrap();
        assert_eq!(find_dlc_pack_dir(dir.path()), None);
    }

    // OpenAI-Codex: Read-only inspection remains useful after writer retirement.
    #[test]
    fn read_only_inspection_reports_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.xml");
        assert!(matches!(
            has_entry(&path, "mymod"),
            Err(CoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound
        ));
        assert!(
            !path.exists(),
            "inspection must not fabricate the missing XML"
        );
    }

    #[test]
    fn read_only_inspection_detects_entries_without_changing_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dlclist.xml");
        let original = base_dlclist().as_bytes();
        std::fs::write(&path, original).unwrap();
        assert!(has_entry(&path, "mpchristmas2017").unwrap());
        assert!(!has_entry(&path, "mymod").unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn read_only_inspection_reports_invalid_utf8_without_changing_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dlclist.xml");
        let original = [0xff, 0xfe];
        std::fs::write(&path, original).unwrap();
        assert!(matches!(
            has_entry(&path, "mymod"),
            Err(CoreError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidData
        ));
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[cfg(windows)]
    #[test]
    fn read_only_inspection_succeeds_while_writes_are_blocked() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dlclist.xml");
        let original = base_dlclist().as_bytes();
        std::fs::write(&path, original).unwrap();
        let _read_only_sharing = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1)
            .open(&path)
            .unwrap();
        assert!(has_entry(&path, "mpchristmas2017").unwrap());
        assert!(!has_entry(&path, "mymod").unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
}
