// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex_lml_hash_fs
// OpenAI-Codex

//! Path checks narrow races but do not establish hard-link, mount or handle-based
//! directory identity. Concurrent namespace changes remain a caller risk.
use crate::error::{CoreError, CoreResult};
#[cfg(test)]
use crate::lml_package::{hash_package, revalidate_package, HashedPackageSnapshot};
#[cfg(test)]
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::{
    fs::OpenOptions,
    io::{Read, Write},
};
use std::{
    fs::{self, File, Metadata},
    path::{Component, Path, PathBuf},
};

pub(crate) fn invalid(reason: &str) -> CoreError {
    CoreError::UnsupportedFormat {
        reason: format!("LML filesystem: {reason}"),
    }
}
fn metadata(path: &Path) -> CoreResult<Metadata> {
    let m = fs::symlink_metadata(path)?;
    let linked = m.file_type().is_symlink();
    #[cfg(windows)]
    let linked = {
        use std::os::windows::fs::MetadataExt;
        linked || m.file_attributes() & 0x400 != 0
    };
    if linked || (!m.is_dir() && !m.is_file()) {
        return Err(invalid("link, reparse point or nonregular entry"));
    }
    Ok(m)
}
pub(crate) fn check_existing_parents(path: &Path) -> CoreResult<()> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut prefix = PathBuf::new();
    for component in absolute.components() {
        prefix.push(component.as_os_str());
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        if !metadata(&prefix)?.is_dir() {
            return Err(invalid("ancestor is not a directory"));
        }
    }
    Ok(())
}
pub(crate) fn checked_directory(path: &Path) -> CoreResult<PathBuf> {
    check_existing_parents(path)?;
    let canonical = fs::canonicalize(path)?;
    check_existing_parents(&canonical)?;
    Ok(canonical)
}
pub(crate) fn open_regular(path: &Path) -> CoreResult<File> {
    check_existing_parents(path.parent().ok_or_else(|| invalid("missing parent"))?)?;
    if !metadata(path)?.is_file() {
        return Err(invalid("source is not a file"));
    }
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("opened source is not regular"));
    }
    Ok(file)
}
#[cfg(test)]
pub(crate) fn lexical_path_key(path: &Path) -> CoreResult<String> {
    let raw = path.to_str().ok_or_else(|| invalid("non UTF-8 path"))?;
    #[cfg(not(windows))]
    if raw.contains('\\') {
        return Err(invalid(
            "literal backslash cannot round-trip through a portable path key",
        ));
    }
    let raw = if raw.starts_with("\\\\?\\")
        && raw.as_bytes().get(4).is_some_and(u8::is_ascii_alphabetic)
        && raw.as_bytes().get(5) == Some(&b':')
        && raw.as_bytes().get(6) == Some(&b'\\')
    {
        &raw[4..]
    } else {
        raw
    };
    Ok(raw.replace('\\', "/"))
}

/// Creates only an exclusive operation leaf under an existing checked parent.
/// Errors preserve every partial file; no cleanup, rename or publication occurs.
#[cfg(test)]
pub fn stage_verified(snapshot: &HashedPackageSnapshot, stage_path: &Path) -> CoreResult<()> {
    let parent = checked_directory(
        stage_path
            .parent()
            .ok_or_else(|| invalid("stage parent missing"))?,
    )?;
    let leaf = stage_path
        .file_name()
        .ok_or_else(|| invalid("stage leaf missing"))?;
    let stage = parent.join(leaf);
    let source = checked_directory(&snapshot.inventory().source_root)?;
    if stage.starts_with(&source) || source.starts_with(&stage) {
        return Err(invalid("source and stage overlap"));
    }
    // create_dir fails for every existing leaf, including dangling symlinks.
    fs::create_dir(&stage)?;
    checked_directory(&stage)?;
    let mut total = 0u64;
    for entry in snapshot.files() {
        let destination = stage.join(&entry.relative_path);
        let relative_parent = Path::new(&entry.relative_path)
            .parent()
            .unwrap_or_else(|| Path::new(""));
        let mut directory = stage.clone();
        for part in relative_parent.components() {
            directory.push(part.as_os_str());
            match fs::create_dir(&directory) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(e) => return Err(e.into()),
            }
            checked_directory(&directory)?;
        }
        let mut input = open_regular(&source.join(&entry.relative_path))?.take(
            entry
                .size
                .checked_add(1)
                .ok_or_else(|| invalid("size overflow"))?,
        );
        check_existing_parents(
            destination
                .parent()
                .ok_or_else(|| invalid("destination parent missing"))?,
        )?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)?;
        let mut digest = Sha256::new();
        let mut count = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            let read = input.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            count = count
                .checked_add(read as u64)
                .ok_or_else(|| invalid("copy overflow"))?;
            if count > entry.size {
                return Err(invalid("source grew"));
            }
            digest.update(&buffer[..read]);
            output.write_all(&buffer[..read])?;
        }
        if count != entry.size || format!("{:x}", digest.finalize()) != entry.sha256 {
            return Err(invalid("source content changed"));
        }
        total = total
            .checked_add(count)
            .ok_or_else(|| invalid("stage total overflow"))?;
        output.sync_all()?;
    }
    if total != snapshot.inventory().total_bytes {
        return Err(invalid("stage total differs"));
    }
    revalidate_package(snapshot)?;
    if hash_package(&stage)?.fingerprint() != snapshot.fingerprint() {
        return Err(invalid("finished stage differs"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lml_package::{hash_package, revalidate_package};
    #[cfg(unix)]
    #[test]
    fn lexical_key_rejects_unix_literal_backslash() {
        assert!(lexical_path_key(Path::new("/tmp/stage\\literal")).is_err());
    }
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("install.xml"), b"<EasyInstall><Name>Fixture</Name><Addons><Addon><StreamingFile>companion.txt</StreamingFile></Addon></Addons></EasyInstall>").unwrap();
        fs::write(dir.path().join("companion.txt"), b"original").unwrap();
        fs::write(dir.path().join("readme.txt"), b"unreferenced companion").unwrap();
        dir
    }
    #[test]
    fn hash_and_exclusive_stage_include_companions() {
        let source = fixture();
        let snapshot = hash_package(source.path()).unwrap();
        assert_eq!(snapshot.files().len(), 3);
        assert!(revalidate_package(&snapshot).is_ok());
        let parent = tempfile::tempdir().unwrap();
        let stage = parent.path().join("stage");
        stage_verified(&snapshot, &stage).unwrap();
        assert_eq!(
            hash_package(&stage).unwrap().fingerprint(),
            snapshot.fingerprint()
        );
        assert!(stage_verified(&snapshot, &stage).is_err());
        assert_eq!(fs::read(stage.join("companion.txt")).unwrap(), b"original");
        fs::write(stage.join("extra"), b"x").unwrap();
        assert_ne!(
            hash_package(&stage).unwrap().fingerprint(),
            snapshot.fingerprint()
        );
        fs::write(source.path().join("companion.txt"), b"modified").unwrap();
        assert!(revalidate_package(&snapshot).is_err());
    }
    #[test]
    fn changed_source_preserves_partial_stage_and_overlap_is_rejected() {
        let source = fixture();
        let snapshot = hash_package(source.path()).unwrap();
        fs::write(source.path().join("companion.txt"), b"changed").unwrap();
        let parent = tempfile::tempdir().unwrap();
        let stage = parent.path().join("stage");
        assert!(stage_verified(&snapshot, &stage).is_err());
        assert!(stage.is_dir());
        assert!(stage_verified(&snapshot, &source.path().join("nested")).is_err());
        assert!(hash_package(&source.path().join("missing")).is_err());
    }
    #[test]
    fn revalidation_rejects_extra_and_missing_files_and_manifest_change() {
        let source = fixture();
        let snapshot = hash_package(source.path()).unwrap();
        fs::write(source.path().join("extra"), b"extra").unwrap();
        assert!(revalidate_package(&snapshot).is_err());
        let source = fixture();
        let snapshot = hash_package(source.path()).unwrap();
        fs::rename(
            source.path().join("companion.txt"),
            source.path().join("renamed.txt"),
        )
        .unwrap();
        assert!(revalidate_package(&snapshot).is_err());
        let source = fixture();
        let snapshot = hash_package(source.path()).unwrap();
        fs::write(source.path().join("install.xml"), b"invalid manifest").unwrap();
        assert!(revalidate_package(&snapshot).is_err());
    }
    #[test]
    fn missing_parent_does_not_get_created() {
        let source = fixture();
        let snapshot = hash_package(source.path()).unwrap();
        let parent = tempfile::tempdir().unwrap();
        let missing = parent.path().join("missing");
        assert!(stage_verified(&snapshot, &missing.join("stage")).is_err());
        assert!(!missing.exists());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn rejects_linked_source_and_parent() {
        use std::os::unix::fs::symlink;
        let source = fixture();
        let parent = tempfile::tempdir().unwrap();
        symlink(source.path(), parent.path().join("linked")).unwrap();
        assert!(hash_package(&parent.path().join("linked")).is_err());
        let snapshot = hash_package(source.path()).unwrap();
        assert!(stage_verified(&snapshot, &parent.path().join("linked/stage")).is_err());
    }
}
