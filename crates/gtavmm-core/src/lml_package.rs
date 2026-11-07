// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex_lml_inventory_impl
// OpenAI-Codex_lml_hash_fs
// OpenAI-Codex

//! Read-only package observations, never installation permission. Metadata can
//! change after inspection (TOCTOU); hard-link identity is not established.

use crate::lml::{
    parse_manifest, LmlManifest, LocalReference, ReferenceShape, ResourceKind, MAX_MANIFEST_BYTES,
    MAX_PATH_BYTES,
};
use crate::{
    error::CoreResult,
    lml_fs::{checked_directory, invalid, open_regular},
    lml_store::OperationFile,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, Metadata},
    io::Read,
    path::{Component, Path, PathBuf},
};

pub const MAX_TREE_ENTRIES: usize = 10_000;
pub const MAX_TREE_DEPTH: usize = 64;
pub const MAX_OVERLAP_RESULTS: usize = 10_000;
pub const MAX_RESOLVED_REFERENCES: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryFile {
    pub relative_path: String,
    pub metadata_bytes: u64,
}

/// Metadata-only physical inventory. Companion files carry no safety verdict.
/// The manifest is included in `files` and `total_bytes`, but excluded from
/// `unreferenced_files`. Declaration indices match the parsed manifest order.
/// Resolved file occurrences across declarations are limited to 10,000.
#[derive(Debug, Clone)]
pub struct PackageInventory {
    pub source_root: PathBuf,
    pub manifest_path: String,
    pub manifest_bytes: Vec<u8>,
    pub manifest: LmlManifest,
    pub files: Vec<InventoryFile>,
    pub total_bytes: u64,
    pub resolved_declarations: Vec<Vec<String>>,
    pub unreferenced_files: Vec<String>,
}

/// Content observations with private construction. Namespace races remain
/// possible; this is not an atomic filesystem snapshot or hard-link proof.
#[derive(Debug, Clone)]
pub struct HashedPackageSnapshot {
    inventory: PackageInventory,
    files: Vec<OperationFile>,
    fingerprint: String,
}
impl HashedPackageSnapshot {
    pub fn inventory(&self) -> &PackageInventory {
        &self.inventory
    }
    pub fn files(&self) -> &[OperationFile] {
        &self.files
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

/// Streams every physical file including the original manifest and companions.
/// Pre/post inventories bound namespace changes but cannot exclude all races.
pub fn hash_package(root: &Path) -> CoreResult<HashedPackageSnapshot> {
    let root = checked_directory(root)?;
    let inventory = inspect_package(&root).map_err(|e| invalid(&e))?;
    let mut files = Vec::new();
    let mut total = 0u64;
    for entry in &inventory.files {
        let mut file = open_regular(&root.join(&entry.relative_path))?.take(
            entry
                .metadata_bytes
                .checked_add(1)
                .ok_or_else(|| invalid("file size overflow"))?,
        );
        let mut hash = Sha256::new();
        let mut bytes = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            bytes = bytes
                .checked_add(read as u64)
                .ok_or_else(|| invalid("file byte overflow"))?;
            if bytes > entry.metadata_bytes {
                return Err(invalid("file grew during hashing"));
            }
            hash.update(&buffer[..read]);
        }
        if bytes != entry.metadata_bytes {
            return Err(invalid("file shrank during hashing"));
        }
        total = total
            .checked_add(bytes)
            .ok_or_else(|| invalid("total byte overflow"))?;
        let sha256 = format!("{:x}", hash.finalize());
        if entry.relative_path == inventory.manifest_path
            && sha256 != format!("{:x}", Sha256::digest(&inventory.manifest_bytes))
        {
            return Err(invalid("manifest changed during hashing"));
        }
        files.push(OperationFile {
            relative_path: entry.relative_path.clone(),
            size: bytes,
            sha256,
        });
    }
    let after = inspect_package(&root).map_err(|e| invalid(&e))?;
    if inventory.files != after.files
        || inventory.manifest_path != after.manifest_path
        || inventory.manifest_bytes != after.manifest_bytes
        || inventory.total_bytes != total
        || after.total_bytes != total
    {
        return Err(invalid("package inventory changed during hashing"));
    }
    files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    let mut hash = Sha256::new();
    hash.update(b"gtavmm-lml-package-v1\0");
    hash.update((files.len() as u64).to_be_bytes());
    for file in &files {
        hash.update((file.relative_path.len() as u64).to_be_bytes());
        hash.update(file.relative_path.as_bytes());
        hash.update(file.size.to_be_bytes());
        hash.update(file.sha256.as_bytes());
    }
    let fingerprint = format!("{:x}", hash.finalize());
    Ok(HashedPackageSnapshot {
        inventory,
        files,
        fingerprint,
    })
}

pub fn revalidate_package(snapshot: &HashedPackageSnapshot) -> CoreResult<()> {
    let current = hash_package(&snapshot.inventory.source_root)?;
    if current.fingerprint != snapshot.fingerprint {
        return Err(invalid("package content changed"));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarationOverlap {
    pub left_declaration: usize,
    pub right_declaration: usize,
    pub raw_game_path: String,
}

/// Exact declaration equality only; no loader precedence or complete conflict
/// analysis. Different virtual strings may alias, even when coverage is explicit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarationOverlapReport {
    pub exact_matches: Vec<DeclarationOverlap>,
    pub unresolved_coverage: bool,
    pub truncated: bool,
}

struct Scan {
    root: PathBuf,
    files: BTreeMap<String, u64>,
    directories: BTreeSet<String>,
    markers: Vec<String>,
    total_bytes: u64,
}

fn checked_metadata(path: &Path) -> Result<Metadata, String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| format!("package metadata: {e}"))?;
    let linked = metadata.file_type().is_symlink();
    #[cfg(windows)]
    let linked = {
        use std::os::windows::fs::MetadataExt;
        linked || metadata.file_attributes() & 0x400 != 0
    };
    if linked || (!metadata.is_file() && !metadata.is_dir()) {
        return Err("package contains a link, reparse point, or nonregular entry".to_owned());
    }
    Ok(metadata)
}

fn check_ancestors(path: &Path) -> Result<(), String> {
    if path.to_str().is_none() {
        return Err("package root or ancestor is not UTF-8".to_owned());
    }
    // Inspect prefixes before resolving `..`, so a linked component cannot be
    // hidden by lexical normalization. Never enumerate any ancestor directory.
    let mut prefix = PathBuf::new();
    for component in path.components() {
        prefix.push(component.as_os_str());
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        if !checked_metadata(&prefix)?.is_dir() {
            return Err("package ancestor is not a directory".to_owned());
        }
    }
    Ok(())
}

// Conservative Unicode lower/upper keys reject possible collisions without
// rewriting original names. This is not a guarantee of host OS name identity.
fn insert_collision_keys(
    path: &str,
    lower: &mut BTreeSet<String>,
    upper: &mut BTreeSet<String>,
) -> bool {
    let lower_new = lower.insert(path.to_lowercase());
    let upper_new = upper.insert(path.to_uppercase());
    lower_new && upper_new
}

fn scan(root: &Path) -> Result<Scan, String> {
    let absolute = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(root)
    };
    check_ancestors(&absolute)?;
    let canonical =
        fs::canonicalize(&absolute).map_err(|e| format!("package canonicalization: {e}"))?;
    check_ancestors(&canonical)?;
    let mut result = Scan {
        root: canonical,
        files: BTreeMap::new(),
        directories: BTreeSet::new(),
        markers: Vec::new(),
        total_bytes: 0,
    };
    let mut pending = vec![(String::new(), 0usize)];
    let mut lower_names = BTreeSet::new();
    let mut upper_names = BTreeSet::new();
    let mut entries = 0usize;
    while let Some((relative_dir, depth)) = pending.pop() {
        let directory = result.root.join(&relative_dir);
        if !checked_metadata(&directory)?.is_dir() {
            return Err("package directory changed kind".to_owned());
        }
        let children =
            fs::read_dir(directory).map_err(|e| format!("package directory read: {e}"))?;
        for child in children {
            let child = child.map_err(|e| format!("package directory entry: {e}"))?;
            entries = entries
                .checked_add(1)
                .ok_or("package entry count overflow")?;
            if entries > MAX_TREE_ENTRIES || depth >= MAX_TREE_DEPTH {
                return Err("package tree limit exceeded".to_owned());
            }
            let name = child
                .file_name()
                .into_string()
                .map_err(|_| "package name is not UTF-8")?;
            let relative = if relative_dir.is_empty() {
                name.clone()
            } else {
                format!("{relative_dir}/{name}")
            };
            if relative.len() > MAX_PATH_BYTES {
                return Err("package relative path exceeds limit".to_owned());
            }
            LocalReference::parse(&relative)?;
            if !insert_collision_keys(&relative, &mut lower_names, &mut upper_names) {
                return Err("package conservative Unicode case collision".to_owned());
            }
            let metadata = checked_metadata(&child.path())?;
            if name.eq_ignore_ascii_case("install.xml") {
                result.markers.push(relative.clone());
            }
            if metadata.is_dir() {
                result.directories.insert(relative.clone());
                pending.push((relative, depth + 1));
            } else {
                result.total_bytes = result
                    .total_bytes
                    .checked_add(metadata.len())
                    .ok_or("package byte count overflow")?;
                result.files.insert(relative, metadata.len());
            }
        }
    }
    result.markers.sort();
    Ok(result)
}

/// A complete bounded metadata scan, including nested marker names. Errors must
/// not be treated as marker absence or permission to use a fallback classifier.
pub fn contains_manifest_marker(root: &Path) -> Result<bool, String> {
    Ok(!scan(root)?.markers.is_empty())
}

/// Inspect an explicitly selected package directory. Only manifest content is
/// read; payloads are neither opened nor hashed. No destination is produced.
pub fn inspect_package(root: &Path) -> Result<PackageInventory, String> {
    let scanned = scan(root)?;
    if scanned.markers.len() != 1 {
        return Err("package requires exactly one manifest".to_owned());
    }
    let manifest_path = scanned.markers[0].clone();
    if manifest_path.contains('/') || !scanned.files.contains_key(&manifest_path) {
        return Err("package manifest must be a regular root file".to_owned());
    }
    let path = scanned.root.join(&manifest_path);
    if !checked_metadata(&path)?.is_file() {
        return Err("package manifest changed kind".to_owned());
    }
    let file = fs::File::open(path).map_err(|e| format!("package manifest open: {e}"))?;
    let mut manifest_bytes = Vec::new();
    file.take((MAX_MANIFEST_BYTES + 1) as u64)
        .read_to_end(&mut manifest_bytes)
        .map_err(|e| format!("package manifest read: {e}"))?;
    let manifest = parse_manifest(&manifest_bytes)?;
    let mut referenced = BTreeSet::new();
    let mut resolved_declarations = Vec::new();
    let mut resolved_count = 0usize;
    for declaration in &manifest.resources {
        let local = declaration.local_path.as_str();
        let resolved: Vec<String> = match declaration.shape {
            ReferenceShape::File => {
                if !scanned.files.contains_key(local) {
                    return Err(format!(
                        "package file reference missing or wrong kind: {local}"
                    ));
                }
                if resolved_count == MAX_RESOLVED_REFERENCES {
                    return Err("package declaration resolution limit exceeded".to_owned());
                }
                vec![local.to_owned()]
            }
            ReferenceShape::Directory => {
                if !scanned.directories.contains(local) {
                    return Err(format!(
                        "package directory reference missing or wrong kind: {local}"
                    ));
                }
                let prefix = format!("{local}/");
                let mut matches = Vec::new();
                for (path, _) in scanned
                    .files
                    .range(prefix.clone()..)
                    .take_while(|(path, _)| path.starts_with(&prefix))
                {
                    if matches.len() >= MAX_RESOLVED_REFERENCES - resolved_count {
                        return Err("package declaration resolution limit exceeded".to_owned());
                    }
                    matches.push(path.clone());
                }
                if matches.is_empty() {
                    return Err(format!("package directory reference is empty: {local}"));
                }
                matches
            }
        };
        resolved_count = resolved_count
            .checked_add(resolved.len())
            .ok_or("package resolution count overflow")?;
        referenced.extend(resolved.iter().cloned());
        resolved_declarations.push(resolved);
    }
    let unreferenced_files = scanned
        .files
        .keys()
        .filter(|p| **p != manifest_path && !referenced.contains(*p))
        .cloned()
        .collect();
    Ok(PackageInventory {
        source_root: scanned.root,
        manifest_path,
        manifest_bytes,
        manifest,
        files: scanned
            .files
            .into_iter()
            .map(|(relative_path, metadata_bytes)| InventoryFile {
                relative_path,
                metadata_bytes,
            })
            .collect(),
        total_bytes: scanned.total_bytes,
        resolved_declarations,
        unreferenced_files,
    })
}

pub fn compare_declared_targets(
    left: &PackageInventory,
    right: &PackageInventory,
) -> DeclarationOverlapReport {
    fn explicit(manifest: &LmlManifest) -> BTreeMap<&str, Vec<usize>> {
        let mut map: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (index, resource) in manifest.resources.iter().enumerate() {
            if resource.kind == ResourceKind::Replacement && resource.shape == ReferenceShape::File
            {
                if let Some(path) = &resource.game_path {
                    map.entry(path.as_str()).or_default().push(index);
                }
            }
        }
        map
    }
    let left_map = explicit(&left.manifest);
    let right_map = explicit(&right.manifest);
    // Cross-package aliasing is unresolved for every unequal raw target, as well
    // as all implicit, streaming, data, and plural declarations.
    let fully_explicit = |manifest: &LmlManifest| {
        manifest.resources.iter().all(|r| {
            r.kind == ResourceKind::Replacement
                && r.shape == ReferenceShape::File
                && r.game_path.is_some()
        })
    };
    let same_only_target = left_map.len() == 1
        && right_map.len() == 1
        && left_map.keys().next() == right_map.keys().next();
    let mut report = DeclarationOverlapReport {
        exact_matches: Vec::new(),
        unresolved_coverage: !fully_explicit(&left.manifest)
            || !fully_explicit(&right.manifest)
            || (!left.manifest.resources.is_empty()
                && !right.manifest.resources.is_empty()
                && !same_only_target),
        truncated: false,
    };
    for (target, left_indices) in left_map {
        if let Some(right_indices) = right_map.get(target) {
            for left_index in left_indices {
                for right_index in right_indices {
                    if report.exact_matches.len() == MAX_OVERLAP_RESULTS {
                        report.truncated = true;
                        report.unresolved_coverage = true;
                        return report;
                    }
                    report.exact_matches.push(DeclarationOverlap {
                        left_declaration: left_index,
                        right_declaration: *right_index,
                        raw_game_path: target.to_owned(),
                    });
                }
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }
    fn manifest(root: &Path, body: &str) -> Vec<u8> {
        let bytes = format!("<EasyInstall>{body}</EasyInstall>").into_bytes();
        std::fs::write(root.join("install.xml"), &bytes).unwrap();
        bytes
    }
    #[test]
    fn inventory_preserves_bytes_metadata_references_and_companions() {
        let temp = fixture();
        let root = temp.path();
        std::fs::create_dir(root.join("stream")).unwrap();
        std::fs::write(root.join("stream/a.ytd"), b"abc").unwrap();
        std::fs::write(root.join("readme.txt"), b"unknown").unwrap();
        let bytes = manifest(root, "<Name>Example</Name><Addons><Addon><StreamingFile>stream/a.ytd</StreamingFile><StreamingFiles>stream</StreamingFiles></Addon></Addons>");
        let value = inspect_package(root).unwrap();
        assert_eq!(value.manifest_bytes, bytes);
        assert_eq!(value.manifest.metadata.name.as_deref(), Some("Example"));
        assert_eq!(value.files.len(), 3);
        assert_eq!(value.total_bytes, bytes.len() as u64 + 10);
        assert_eq!(
            value.resolved_declarations,
            vec![
                vec!["stream/a.ytd".to_owned()],
                vec!["stream/a.ytd".to_owned()]
            ]
        );
        assert_eq!(value.unreferenced_files, vec!["readme.txt"]);
    }
    #[test]
    fn rejects_missing_wrong_kind_empty_and_case_guesses() {
        for (tag, path) in [
            ("StreamingFile", "missing"),
            ("StreamingFile", "dir"),
            ("StreamingFiles", "file"),
            ("StreamingFiles", "dir"),
            ("StreamingFile", "FILE"),
        ] {
            let temp = fixture();
            let root = temp.path();
            std::fs::create_dir(root.join("dir")).unwrap();
            std::fs::write(root.join("file"), b"x").unwrap();
            manifest(
                root,
                &format!("<Addons><Addon><{tag}>{path}</{tag}></Addon></Addons>"),
            );
            assert!(inspect_package(root).is_err());
        }
    }
    #[test]
    fn marker_detects_nested_manifest_without_parsing() {
        let temp = fixture();
        let root = temp.path();
        assert!(!contains_manifest_marker(root).unwrap());
        std::fs::create_dir(root.join("nested")).unwrap();
        std::fs::write(root.join("nested/INSTALL.XML"), b"not XML").unwrap();
        assert!(contains_manifest_marker(root).unwrap());
        assert!(inspect_package(root).is_err());
        manifest(root, "");
        assert!(inspect_package(root).is_err());
    }
    #[test]
    fn exact_virtual_overlap_leaves_other_coverage_unresolved() {
        let temp = fixture();
        let root = temp.path();
        std::fs::write(root.join("a"), b"x").unwrap();
        manifest(root, "<Replacements><Replacement><FileReplacement><GamePath>update:/a</GamePath><FilePath>a</FilePath></FileReplacement><FileReplacement>a</FileReplacement></Replacement></Replacements>");
        let left = inspect_package(root).unwrap();
        let mut right = left.clone();
        let report = compare_declared_targets(&left, &right);
        assert_eq!(report.exact_matches.len(), 1);
        assert!(report.unresolved_coverage);
        right.manifest = crate::lml::parse_manifest(b"<EasyInstall><Replacements><Replacement><FileReplacement><GamePath>UPDATE:/a</GamePath><FilePath>a</FilePath></FileReplacement></Replacement></Replacements></EasyInstall>").unwrap();
        assert!(compare_declared_targets(&left, &right)
            .exact_matches
            .is_empty());
    }
    #[test]
    fn rejects_depth_and_entry_limits() {
        let temp = fixture();
        let root = temp.path();
        let mut path = root.to_path_buf();
        for _ in 0..=MAX_TREE_DEPTH {
            path.push("a");
            std::fs::create_dir(&path).unwrap();
        }
        assert!(contains_manifest_marker(root).is_err());
        let temp = fixture();
        let root = temp.path();
        for i in 0..=MAX_TREE_ENTRIES {
            std::fs::write(root.join(format!("f{i}")), b"").unwrap();
        }
        assert!(contains_manifest_marker(root).is_err());
    }
    #[test]
    fn rejects_oversize_manifest_and_unsafe_tree_names() {
        let temp = fixture();
        let root = temp.path();
        std::fs::write(root.join("install.xml"), vec![b' '; MAX_MANIFEST_BYTES + 1]).unwrap();
        assert!(inspect_package(root).is_err());
        // Reserved names are validated without depending on host filesystem
        // support for creating them.
        assert!(LocalReference::parse("dir/CON.txt").is_err());
        assert!(LocalReference::parse(&"a/".repeat(MAX_PATH_BYTES)).is_err());
    }
    #[test]
    fn rejects_resolution_output_expansion() {
        let temp = fixture();
        let root = temp.path();
        std::fs::create_dir(root.join("stream")).unwrap();
        for i in 0..101 {
            std::fs::write(root.join(format!("stream/{i}")), b"").unwrap();
        }
        manifest(
            root,
            &format!(
                "<Addons><Addon>{}</Addon></Addons>",
                "<StreamingFiles>stream</StreamingFiles>".repeat(100)
            ),
        );
        assert!(inspect_package(root)
            .unwrap_err()
            .contains("resolution limit"));
    }
    #[test]
    fn unicode_collision_keys_are_conservative() {
        for (left, right) in [("Ä", "ä"), ("Σ", "ς"), ("SS", "ß")] {
            assert!(
                left.to_lowercase() == right.to_lowercase()
                    || left.to_uppercase() == right.to_uppercase()
            );
            let mut lower = BTreeSet::new();
            let mut upper = BTreeSet::new();
            assert!(insert_collision_keys(left, &mut lower, &mut upper));
            assert!(!insert_collision_keys(right, &mut lower, &mut upper));
        }
    }
    #[test]
    fn overlap_output_is_capped_for_repeated_declarations() {
        let temp = fixture();
        let root = temp.path();
        std::fs::write(root.join("a"), b"x").unwrap();
        manifest(root, "<Replacements><Replacement><FileReplacement><GamePath>x:/a</GamePath><FilePath>a</FilePath></FileReplacement></Replacement></Replacements>");
        let mut inventory = inspect_package(root).unwrap();
        inventory.manifest.resources = vec![inventory.manifest.resources[0].clone(); 101];
        let report = compare_declared_targets(&inventory, &inventory);
        assert_eq!(report.exact_matches.len(), MAX_OVERLAP_RESULTS);
        assert!(report.truncated && report.unresolved_coverage);
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn rejects_links_case_collisions_and_non_utf8_names() {
        use std::os::unix::{ffi::OsStringExt, fs::symlink};
        let temp = fixture();
        let root = temp.path();
        symlink(root, root.join("link")).unwrap();
        assert!(contains_manifest_marker(root).is_err());
        assert!(contains_manifest_marker(&root.join("link")).is_err());
        assert!(contains_manifest_marker(&root.join("link/..")).is_err());
        let temp = fixture();
        let root = temp.path();
        std::fs::write(root.join("a"), b"").unwrap();
        std::fs::write(root.join("A"), b"").unwrap();
        assert!(contains_manifest_marker(root).is_err());
        let temp = fixture();
        let root = temp.path();
        std::fs::write(root.join("Ä"), b"").unwrap();
        std::fs::write(root.join("ä"), b"").unwrap();
        assert!(contains_manifest_marker(root).is_err());
        let temp = fixture();
        let root = temp.path();
        std::fs::write(root.join(std::ffi::OsString::from_vec(vec![255])), b"").unwrap();
        assert!(contains_manifest_marker(root).is_err());
    }
}
