// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex
// OpenAI-Codex_Responsible: Source_Code

//! Add-on content packs (add-on vehicles, add-on maps, and anything else shipped as a
//! standalone `dlc.rpf`) — distinct from *replace*-type mods, which just overwrite an
//! existing file at a mirrored path and need no registration anywhere.
//!
//! An add-on pack is a real, standalone `.rpf` archive file (not an edit to an
//! existing archive — that distinction matters, see `oiv.rs`'s doc comment) that must
//! be (1) copied into `mods\update\x64\dlcpacks\<name>\` and (2) registered as an
//! `<Item>dlcpacks:\<name>\</Item>` entry in `dlclist.xml`.
//!
//! `dlclist.xml` itself lives *inside* `update.rpf` in a stock installation. Per the
//! OpenIV/OpenRPF "mods folder" convention, overriding it means placing a plain-text
//! copy at the mirrored path `mods\update\update.rpf\common\data\dlclist.xml` — but
//! that mirrored file does not exist until *something* creates it, and we cannot
//! safely fabricate the base game's list ourselves (it would require reading and
//! redistributing copyrighted archive contents). **MVP boundary**: we read/edit that
//! mirrored file if it already exists (typically because the user extracted it once
//! via OpenIV, which is the same "OpenIV/OpenRPF prerequisite" already assumed for
//! folder-based mods elsewhere in this crate); if it doesn't exist yet, we report a
//! clear, actionable error rather than silently doing nothing or guessing at content.

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

/// Where `dlclist.xml` is expected under the `mods\` loose-override convention.
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

/// `true` if `name` is already registered in the `dlclist.xml` at `path`.
pub fn has_entry(path: &Path, name: &str) -> CoreResult<bool> {
    let contents = std::fs::read_to_string(path)?;
    Ok(contents.contains(&entry_line(name)))
}

/// Adds `<Item>dlcpacks:\<name>\</Item>` before `</Paths>`, if not already present.
/// Requires `dlclist.xml` to already exist at `path` — see this module's doc comment
/// for why we don't fabricate a base file.
pub fn add_entry(path: &Path, name: &str) -> CoreResult<()> {
    add_entry_with_status(path, name).map(|_| ())
}

/// Returns whether this call added the entry, so rollback preserves older entries.
pub(crate) fn add_entry_with_status(path: &Path, name: &str) -> CoreResult<bool> {
    let contents = std::fs::read_to_string(path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            dlclist_missing_error(path)
        } else {
            err.into()
        }
    })?;

    if contents.contains(&entry_line(name)) {
        return Ok(false); // already registered, nothing to do
    }

    let closing_tag = "</Paths>";
    let Some(insert_at) = contents.find(closing_tag) else {
        return Err(CoreError::UnsupportedFormat {
            reason: format!("{} does not contain a </Paths> closing tag", path.display()),
        });
    };

    let mut updated = String::with_capacity(contents.len() + 64);
    updated.push_str(&contents[..insert_at]);
    updated.push_str(&format!("    <Item>{}</Item>\n", entry_line(name)));
    updated.push_str(&contents[insert_at..]);

    std::fs::write(path, updated)?;
    Ok(true)
}

/// Removes matching `<Item>dlcpacks:\<name>\</Item>` entries without removing
/// neighboring content. No-op if the entry (or the file) isn't present.
pub fn remove_entry(path: &Path, name: &str) -> CoreResult<()> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };

    let needle = entry_line(name);
    let mut updated = String::with_capacity(contents.len());
    let mut remaining = contents.as_str();
    let open_tag = "<Item>";
    let close_tag = "</Item>";
    while let Some(start) = remaining.find(open_tag) {
        let after_open = &remaining[start + open_tag.len()..];
        let Some(end) = after_open.find(close_tag) else {
            break;
        };
        let item_end = start + open_tag.len() + end + close_tag.len();
        if after_open[..end].trim() == needle.as_str() {
            updated.push_str(&remaining[..start]);
        } else {
            updated.push_str(&remaining[..item_end]);
        }
        remaining = &remaining[item_end..];
    }
    updated.push_str(remaining);
    if updated == contents {
        return Ok(());
    }

    std::fs::write(path, updated)?;
    Ok(())
}

fn dlclist_missing_error(path: &Path) -> CoreError {
    CoreError::UnsupportedFormat {
        reason: format!(
            "{} does not exist yet. This is expected before your first add-on \
             (vehicle/map) mod — please extract the base dlclist.xml via OpenIV \
             into that path once, then try installing this mod again.",
            path.display()
        ),
    }
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

    #[test]
    fn adding_entry_to_missing_file_gives_actionable_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dlclist.xml");
        let err = add_entry(&path, "mymod").unwrap_err();
        assert!(matches!(err, CoreError::UnsupportedFormat { .. }));
    }

    #[test]
    fn adds_and_detects_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dlclist.xml");
        std::fs::write(&path, base_dlclist()).unwrap();

        assert!(!has_entry(&path, "mymod").unwrap());
        add_entry(&path, "mymod").unwrap();
        assert!(has_entry(&path, "mymod").unwrap());
        assert!(
            has_entry(&path, "mpchristmas2017").unwrap(),
            "pre-existing entries must survive"
        );
    }

    #[test]
    fn adding_twice_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dlclist.xml");
        std::fs::write(&path, base_dlclist()).unwrap();

        add_entry(&path, "mymod").unwrap();
        add_entry(&path, "mymod").unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.matches("mymod").count(), 1);
    }

    #[test]
    fn remove_entry_drops_only_the_matching_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dlclist.xml");
        std::fs::write(&path, base_dlclist()).unwrap();
        add_entry(&path, "mymod").unwrap();

        remove_entry(&path, "mymod").unwrap();

        assert!(!has_entry(&path, "mymod").unwrap());
        assert!(has_entry(&path, "mpchristmas2017").unwrap());
    }

    #[test]
    fn remove_entry_on_missing_file_is_a_harmless_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does_not_exist.xml");
        remove_entry(&path, "mymod").unwrap();
    }

    #[test]
    fn rollback_regression_remove_entry_reports_invalid_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dlclist.xml");
        std::fs::write(&path, [0xff]).unwrap();

        assert!(matches!(
            remove_entry(&path, "mymod"),
            Err(CoreError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidData
        ));
    }

    #[test]
    fn rollback_regression_add_entry_reports_invalid_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dlclist.xml");
        std::fs::write(&path, [0xff]).unwrap();

        assert!(matches!(
            add_entry(&path, "mymod"),
            Err(CoreError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidData
        ));
    }

    #[cfg(windows)]
    #[test]
    fn atomic_regression_add_preserves_file_when_replacement_is_blocked() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dlclist.xml");
        let original = base_dlclist();
        std::fs::write(&path, original).unwrap();
        let _locked_file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1 | 0x2)
            .open(&path)
            .unwrap();

        assert!(add_entry(&path, "mymod").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[cfg(windows)]
    #[test]
    fn atomic_regression_remove_preserves_file_when_replacement_is_blocked() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dlclist.xml");
        let original = base_dlclist();
        std::fs::write(&path, original).unwrap();
        let _locked_file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1 | 0x2)
            .open(&path)
            .unwrap();

        assert!(remove_entry(&path, "mpchristmas2017").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }
}
