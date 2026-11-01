// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex
// OpenAI-Codex_Responsible: Source_Code

//! Classifies a mod package (file/folder/archive) into a plan of `(source, target)`
//! file mappings, per the format table in the MVP spec: `.asi`/`.dll` (native and
//! managed)/`.xml` (Menyoo)/folder replacers/`.zip`/`.7z` fully supported; `.rar` uses
//! bounded, read-only extraction for unencrypted single-volume archives; `.oiv` is
//! reported as unsupported (no silent partial handling).
//!
//! **`.oiv` scope decision (2026-08-27)**: this project never attempts to install any
//! `.oiv` package itself, regardless of complexity — even the simple, non-RPF-editing
//! case that an earlier version of this module supported. This follows directly from a
//! real bug found via a real `.oiv` sample: the `assembly.xml` schema this module
//! previously parsed was an unverified guess, and it was wrong in a way that silently
//! misclassified a genuinely RPF-editing package as safe (see git history for
//! `mod_analyzer::oiv` for the full story). Every real `.oiv` sample available for
//! testing needed RPF archive editing; treating `.oiv` installation as *always*
//! out of scope — recommend OpenIV instead — removes that entire risk surface rather
//! than trying to shrink it further.

pub mod dlc;
mod menyoo;
mod pe;

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::{CoreError, CoreResult};

pub use menyoo::{MenyooCategory, MENYOO_ROOT_FOLDER};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum ModFormat {
    Asi,
    NativeDll,
    ManagedDll,
    MenyooXml,
    FolderReplacer,
    /// A standalone add-on content pack (add-on vehicle, add-on map, etc. — anything
    /// shipped as its own `dlc.rpf` rather than replacing an existing file). Carries
    /// the pack name used by the manual registration reminders. File lifecycle
    /// operations record this work through `dlc_manual`; users review and edit
    /// their own exported `dlclist.xml` instead of automatic XML registration.
    AddOnPack {
        pack_name: String,
    },
    Zip,
    SevenZip,
    Rar,
    Unsupported(String),
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PlannedFile {
    pub source: PathBuf,
    pub target: PathBuf,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ModPlan {
    pub format: ModFormat,
    pub files: Vec<PlannedFile>,
    #[serde(skip)]
    staging_dir: Option<Arc<tempfile::TempDir>>,
}

impl ModPlan {
    pub fn new(format: ModFormat, files: Vec<PlannedFile>) -> Self {
        Self {
            format,
            files,
            staging_dir: None,
        }
    }

    fn with_staging_dir(mut self, staging_dir: tempfile::TempDir) -> Self {
        self.staging_dir = Some(Arc::new(staging_dir));
        self
    }
}

/// `scripts\` subfolder name for ScriptHookVDotNet managed assemblies. Re-exported
/// (`pub(crate)`) so `providers::LegacySpProvider` can share the same constant rather
/// than duplicating the literal.
pub(crate) const SCRIPTS_SUBFOLDER: &str = "scripts";
/// `mods\` subfolder name for folder-based replacer mods (per the OpenIV.asi/OpenRPF
/// loose-file-override convention, see the MVP spec's format table).
pub(crate) const MODS_SUBFOLDER: &str = "mods";

/// Classifies `input` (a file or folder path) and produces a `ModPlan`. Target paths
/// are resolved by delegating to `provider` — this function only decides *what kind*
/// of mod `input` is (by extension/content sniffing) and *what relative files it
/// contains*; it never hardcodes a mode's directory layout itself. That's what makes
/// adding a new mode (Enhanced SP, LSPDFR, FiveM) additive: implement a new
/// `ModeProvider`, and none of the dispatch logic here needs to change.
///
/// Archives (`.zip`/`.7z`/`.rar`) are extracted into a temp directory and then recursed
/// into as a folder; the returned plan's `source` paths point into that temp directory,
/// so callers must copy files out of it before it (or its `TempDir` guard) is dropped.
pub fn classify(
    input: &Path,
    provider: &dyn crate::providers::ModeProvider,
) -> CoreResult<ModPlan> {
    let extension = input
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());

    match extension.as_deref() {
        _ if input.is_dir() => classify_folder(input, provider),
        Some("asi") => {
            let name = input.file_name().unwrap_or_default();
            Ok(single_file_plan(
                input,
                provider.resolve_asi_target(name),
                ModFormat::Asi,
            ))
        }
        Some("dll") => classify_dll(input, provider),
        Some("xml") => classify_menyoo_xml(input, provider),
        Some("zip") => classify_zip(input, provider),
        Some("7z") => classify_seven_zip(input, provider),
        Some("rar") => classify_rar(input, provider),
        Some("oiv") => classify_oiv(input),
        Some(other) => Ok(unsupported(
            other,
            &format!("Unrecognized mod file type: .{other}"),
        )),
        None => Err(CoreError::UnsupportedFormat {
            reason: "input has no file extension and is not a directory".to_string(),
        }),
    }
}

fn unsupported(tag: &str, message: &str) -> ModPlan {
    ModPlan::new(
        ModFormat::Unsupported(format!("{tag}: {message}")),
        Vec::new(),
    )
}

fn single_file_plan(source: &Path, target: PathBuf, format: ModFormat) -> ModPlan {
    ModPlan::new(
        format,
        vec![PlannedFile {
            source: source.to_path_buf(),
            target,
        }],
    )
}

fn classify_dll(
    input: &Path,
    provider: &dyn crate::providers::ModeProvider,
) -> CoreResult<ModPlan> {
    let name = input.file_name().unwrap_or_default();
    if pe::is_managed_assembly(input) {
        Ok(single_file_plan(
            input,
            provider.resolve_managed_dll_target(name),
            ModFormat::ManagedDll,
        ))
    } else {
        Ok(single_file_plan(
            input,
            provider.resolve_native_dll_target(name),
            ModFormat::NativeDll,
        ))
    }
}

fn classify_menyoo_xml(
    input: &Path,
    provider: &dyn crate::providers::ModeProvider,
) -> CoreResult<ModPlan> {
    let category = menyoo::detect_category(input);
    let name = input.file_name().unwrap_or_default();
    let target = provider.resolve_menyoo_target(category, name);
    Ok(single_file_plan(input, target, ModFormat::MenyooXml))
}

/// Mirrors a replacer-mod folder's structure via `provider.resolve_folder_replacer_target`
/// (the OpenIV.asi/OpenRPF loose-file-override convention — see the MVP spec;
/// verifying that prerequisite is out of scope for the classifier itself). Add-on
/// packs (folders containing a standalone `dlc.rpf`, per the `dlc` module) are
/// detected and routed to `resolve_add_on_pack_target` instead of a raw structural
/// mirror.
fn classify_folder(
    input: &Path,
    provider: &dyn crate::providers::ModeProvider,
) -> CoreResult<ModPlan> {
    classify_folder_with_name_hint(input, provider, None)
}

/// `name_hint`, when given, is the original archive's file stem (e.g. `"My Car
/// [Add-On] 1.0"` from `My Car [Add-On] 1.0.zip`) — used in place of the pack
/// directory's own name only when that directory *is* `input` itself (a `dlc.rpf`
/// sitting right at the archive's root, no wrapper folder), because in that case the
/// "directory" is really just the OS-generated temp extraction folder and its name
/// (e.g. `.tmpGXU7NN`) is meaningless garbage, not the pack's real name. A real sample
/// zip from gta5-mods.com — an add-on vehicle packaged with `dlc.rpf` directly at the
/// zip root — surfaced this: without the hint, the pack got registered in
/// `dlclist.xml` (and its files installed) under that random temp name instead of the
/// mod's actual name.
fn classify_folder_with_name_hint(
    input: &Path,
    provider: &dyn crate::providers::ModeProvider,
    name_hint: Option<&str>,
) -> CoreResult<ModPlan> {
    if let Some(pack_dir) = dlc::find_dlc_pack_dir(input) {
        let name_override = if pack_dir == input { name_hint } else { None };
        return classify_add_on_pack(&pack_dir, provider, name_override);
    }

    // A `.zip`/`.7z` that's really just an `.oiv` installer wrapped in an archive is a
    // common real-world pattern on gta5-mods.com (found via a real sample: a plain
    // .zip containing nothing but a .oiv and a .url shortcut). Without this check,
    // that .oiv would fall through to the generic folder-replacer walk below and get
    // "installed" as a loose file at its own path — silently violating this module's
    // own documented policy that `.oiv` is never installed by this project, regardless
    // of what container it arrived in.
    if let Some(oiv_path) = find_any_oiv_file(input) {
        return classify_oiv(&oiv_path);
    }

    if let Some(game_root) = provider.lspdfr_package_game_root() {
        match find_lspdfr_package_root(input)? {
            LspdfrRootMatch::Ambiguous(reason) => {
                return Ok(unsupported("lspdfr package", &reason));
            }
            LspdfrRootMatch::Found(package_root) => {
                let files = walkdir::WalkDir::new(&package_root)
                    .into_iter()
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| entry.file_type().is_file())
                    .map(|entry| {
                        let relative = entry
                            .path()
                            .strip_prefix(&package_root)
                            .unwrap_or(entry.path());
                        PlannedFile {
                            source: entry.path().to_path_buf(),
                            target: game_root.join(relative),
                        }
                    })
                    .collect();
                return Ok(ModPlan::new(ModFormat::FolderReplacer, files));
            }
            LspdfrRootMatch::None if contains_dll_file(input) => {
                return Ok(unsupported(
                    "lspdfr package",
                    "contains DLL files but has no explicit Plugins/LSPDFR game-root layout; confirm the package paths and arrange the files manually before installing",
                ));
            }
            LspdfrRootMatch::None => {}
        }
    }

    let files = walkdir::WalkDir::new(input)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|entry| {
            let relative = entry.path().strip_prefix(input).unwrap_or(entry.path());
            PlannedFile {
                source: entry.path().to_path_buf(),
                target: provider.resolve_folder_replacer_target(relative),
            }
        })
        .collect();

    Ok(ModPlan::new(ModFormat::FolderReplacer, files))
}

enum LspdfrRootMatch {
    None,
    Found(PathBuf),
    Ambiguous(String),
}

fn find_lspdfr_package_root(input: &Path) -> CoreResult<LspdfrRootMatch> {
    let direct = find_lspdfr_root_in(input)?;
    if let LspdfrRootMatch::Ambiguous(_) = &direct {
        return Ok(direct);
    }

    let wrappers = matching_child_directories(input, "Grand Theft Auto V")?;
    if wrappers.len() > 1 {
        return Ok(LspdfrRootMatch::Ambiguous(
            "contains multiple case-insensitive Grand Theft Auto V wrapper folders".to_string(),
        ));
    }
    let wrapped = if let Some(wrapper) = wrappers.first() {
        find_lspdfr_root_in(wrapper)?
    } else {
        LspdfrRootMatch::None
    };
    if let LspdfrRootMatch::Ambiguous(_) = &wrapped {
        return Ok(wrapped);
    }

    let mut candidates = Vec::new();
    if let LspdfrRootMatch::Found(path) = direct {
        candidates.push(path);
    }
    if let LspdfrRootMatch::Found(path) = wrapped {
        candidates.push(path);
    }

    match candidates.len() {
        0 => Ok(LspdfrRootMatch::None),
        1 => Ok(LspdfrRootMatch::Found(candidates.remove(0))),
        _ => Ok(LspdfrRootMatch::Ambiguous(
            "contains more than one possible Plugins/LSPDFR game-root layout".to_string(),
        )),
    }
}

fn find_lspdfr_root_in(parent: &Path) -> CoreResult<LspdfrRootMatch> {
    let plugins_dirs = matching_child_directories(parent, "Plugins")?;
    if plugins_dirs.len() > 1 {
        return Ok(LspdfrRootMatch::Ambiguous(
            "contains multiple case-insensitive Plugins folders".to_string(),
        ));
    }
    let Some(plugins_dir) = plugins_dirs.first() else {
        return Ok(LspdfrRootMatch::None);
    };

    let lspdfr_dirs = matching_child_directories(plugins_dir, "LSPDFR")?;
    if lspdfr_dirs.len() > 1 {
        return Ok(LspdfrRootMatch::Ambiguous(
            "contains multiple case-insensitive Plugins/LSPDFR folders".to_string(),
        ));
    }
    if lspdfr_dirs.is_empty() {
        Ok(LspdfrRootMatch::None)
    } else {
        // The LSPDFR directory confirms the layout, but all files in this game-root
        // folder belong to the package (including root-level DLL dependencies and
        // resources beside Plugins/LSPDFR).
        Ok(LspdfrRootMatch::Found(parent.to_path_buf()))
    }
}

fn matching_child_directories(parent: &Path, expected_name: &str) -> CoreResult<Vec<PathBuf>> {
    let mut matches = Vec::new();
    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(expected_name)
        {
            matches.push(entry.path());
        }
    }
    Ok(matches)
}

fn contains_dll_file(input: &Path) -> bool {
    walkdir::WalkDir::new(input)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .any(|entry| {
            entry.file_type().is_file()
                && entry
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("dll"))
        })
}

fn classify_add_on_pack(
    pack_dir: &Path,
    provider: &dyn crate::providers::ModeProvider,
    name_override: Option<&str>,
) -> CoreResult<ModPlan> {
    let name = match name_override {
        Some(n) => n.to_string(),
        None => dlc::pack_name(pack_dir)?,
    };

    let files = walkdir::WalkDir::new(pack_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|entry| {
            let relative = entry.path().strip_prefix(pack_dir).unwrap_or(entry.path());
            PlannedFile {
                source: entry.path().to_path_buf(),
                target: provider.resolve_add_on_pack_target(&name, relative),
            }
        })
        .collect();

    Ok(ModPlan::new(
        ModFormat::AddOnPack { pack_name: name },
        files,
    ))
}

const MAX_ZIP_ENTRY_COUNT: usize = 10_000;
const MAX_ZIP_MEMBER_OUTPUT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_ZIP_TOTAL_OUTPUT_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_SEVEN_ZIP_ENTRY_COUNT: usize = MAX_ZIP_ENTRY_COUNT;
const MAX_SEVEN_ZIP_MEMBER_OUTPUT_BYTES: u64 = MAX_ZIP_MEMBER_OUTPUT_BYTES;
const MAX_SEVEN_ZIP_TOTAL_OUTPUT_BYTES: u64 = MAX_ZIP_TOTAL_OUTPUT_BYTES;

fn unsupported_archive_limit(format: &str, reason: impl std::fmt::Display) -> CoreError {
    CoreError::UnsupportedFormat {
        reason: format!("unsupported or unsafe {format} archive: {reason}"),
    }
}

fn validate_archive_limits(
    format: &str,
    entry_count: usize,
    entry_sizes: impl IntoIterator<Item = u64>,
    max_entries: usize,
    max_member_output_bytes: u64,
    max_total_output_bytes: u64,
) -> CoreResult<()> {
    if entry_count > max_entries {
        return Err(unsupported_archive_limit(
            format,
            format!("archive contains more than {max_entries} entries"),
        ));
    }

    let mut total_output_bytes = 0_u64;
    for size in entry_sizes {
        if size > max_member_output_bytes {
            return Err(unsupported_archive_limit(
                format,
                format!("an entry exceeds the {max_member_output_bytes}-byte per-entry limit"),
            ));
        }
        total_output_bytes = total_output_bytes.checked_add(size).ok_or_else(|| {
            unsupported_archive_limit(format, "total uncompressed size overflows")
        })?;
        if total_output_bytes > max_total_output_bytes {
            return Err(unsupported_archive_limit(
                format,
                format!("archive exceeds the {max_total_output_bytes}-byte total limit"),
            ));
        }
    }
    Ok(())
}

fn extract_zip_with_limits(
    input: &Path,
    max_entries: usize,
    max_member_output_bytes: u64,
    max_total_output_bytes: u64,
) -> CoreResult<tempfile::TempDir> {
    let file = std::fs::File::open(input)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| CoreError::UnsupportedFormat {
        reason: format!("not a valid zip archive: {e}"),
    })?;

    let entry_count = archive.len();
    if entry_count > max_entries {
        return Err(unsupported_archive_limit(
            "ZIP",
            format!("archive contains more than {max_entries} entries"),
        ));
    }
    let mut entry_sizes = Vec::with_capacity(entry_count);
    for index in 0..entry_count {
        let entry = archive
            .by_index(index)
            .map_err(|e| unsupported_archive_limit("ZIP", format!("invalid entry: {e}")))?;
        entry_sizes.push(entry.size());
    }
    validate_archive_limits(
        "ZIP",
        entry_count,
        entry_sizes,
        max_entries,
        max_member_output_bytes,
        max_total_output_bytes,
    )?;

    let temp_dir = tempfile::tempdir()?;
    archive
        .extract(temp_dir.path())
        .map_err(|e| CoreError::UnsupportedFormat {
            reason: format!("failed to extract zip archive: {e}"),
        })?;
    Ok(temp_dir)
}

fn safe_seven_zip_entry_path(
    entry: &sevenz_rust::SevenZArchiveEntry,
) -> Result<PathBuf, sevenz_rust::Error> {
    let entry_name = entry.name();
    let normalized_name = entry_name.replace('\\', "/");
    let name_bytes = normalized_name.as_bytes();
    let has_windows_drive_prefix =
        name_bytes.len() >= 2 && name_bytes[0].is_ascii_alphabetic() && name_bytes[1] == b':';
    let has_parent_component = normalized_name
        .split('/')
        .any(|component| component == "..");

    let unsafe_path_reason = if entry_name.is_empty() && !entry.is_directory() {
        Some("empty")
    } else if entry_name.contains('\0') {
        Some("NUL-containing")
    } else if normalized_name.starts_with('/') {
        Some("absolute")
    } else if has_windows_drive_prefix {
        Some("drive-prefixed")
    } else if has_parent_component {
        Some("parent-directory")
    } else {
        None
    };

    if let Some(reason) = unsafe_path_reason {
        return Err(sevenz_rust::Error::io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unsafe {reason} path in 7z archive"),
        )));
    }

    Ok(PathBuf::from(normalized_name))
}

fn extract_seven_zip_with_limits(
    input: &Path,
    max_entries: usize,
    max_member_output_bytes: u64,
    max_total_output_bytes: u64,
) -> CoreResult<tempfile::TempDir> {
    let mut archive = sevenz_rust::SevenZReader::open(input, sevenz_rust::Password::empty())
        .map_err(|e| CoreError::UnsupportedFormat {
            reason: format!("failed to read 7z archive: {e}"),
        })?;
    let archive_entries = &archive.archive().files;
    validate_archive_limits(
        "7z",
        archive_entries.len(),
        archive_entries.iter().map(|entry| entry.size()),
        max_entries,
        max_member_output_bytes,
        max_total_output_bytes,
    )?;

    let temp_dir = tempfile::tempdir()?;
    archive
        .for_each_entries(|entry, reader| {
            let relative_path = safe_seven_zip_entry_path(entry)?;
            let output_path = temp_dir.path().join(relative_path);
            sevenz_rust::default_entry_extract_fn(entry, reader, &output_path)
        })
        .map_err(|e| CoreError::UnsupportedFormat {
            reason: format!("failed to extract 7z archive: {e}"),
        })?;
    Ok(temp_dir)
}

const MAX_RAR_ENTRY_COUNT: usize = 10_000;
const MAX_RAR_HEADER_COUNT: u64 = 20_000;
const MAX_RAR_HEADER_BYTES: u64 = 32 * 1024 * 1024;
const MAX_RAR_MEMBER_OUTPUT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_RAR_TOTAL_OUTPUT_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_RAR_READER_WORKSPACE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_RAR_DICTIONARY_BYTES: u64 = 512 * 1024 * 1024;
const MAX_RAR_BUFFERED_DECODE_BYTES: u64 = 64 * 1024 * 1024;

fn rar_read_options() -> rars::ArchiveReadOptions<'static> {
    rars::ArchiveReadOptions::new()
        .with_max_header_count(MAX_RAR_HEADER_COUNT)
        .with_max_header_bytes(MAX_RAR_HEADER_BYTES)
        .with_max_member_output_bytes(MAX_RAR_MEMBER_OUTPUT_BYTES)
        .with_max_total_output_bytes(MAX_RAR_TOTAL_OUTPUT_BYTES)
        .with_max_reader_workspace_bytes(MAX_RAR_READER_WORKSPACE_BYTES)
        .with_rar50_dictionary_size_limit(MAX_RAR_DICTIONARY_BYTES)
        .with_rar50_buffered_decode_limit(MAX_RAR_BUFFERED_DECODE_BYTES)
}

fn unsupported_rar(reason: impl std::fmt::Display) -> CoreError {
    CoreError::UnsupportedFormat {
        reason: format!("unsupported or unsafe RAR archive: {reason}"),
    }
}

/// Converts an archive entry name into a safe relative Windows/Linux path.
/// Both slash styles are treated as separators on every host so validation stays
/// consistent with the target platforms.
fn rar_entry_relative_path(decoded_name: &[u8], is_directory: bool) -> CoreResult<PathBuf> {
    let name = std::str::from_utf8(decoded_name)
        .map_err(|_| unsupported_rar("entry names must decode to UTF-8"))?;
    if name.contains('\0') {
        return Err(unsupported_rar("entry names cannot contain NUL bytes"));
    }

    let normalized = name.replace('\\', "/");
    let normalized = if is_directory {
        normalized.strip_suffix('/').unwrap_or(&normalized)
    } else if normalized.ends_with('/') {
        return Err(unsupported_rar(
            "a file entry cannot end with a path separator",
        ));
    } else {
        &normalized
    };

    if normalized.is_empty() || normalized.starts_with('/') || normalized.contains(':') {
        return Err(unsupported_rar(
            "entry path must be relative and cannot use a drive or stream prefix",
        ));
    }

    let mut relative = PathBuf::new();
    for component in normalized.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(unsupported_rar(
                "entry path contains an empty or traversal component",
            ));
        }
        if matches!(component.chars().last(), Some(' ' | '.'))
            || component
                .chars()
                .any(|ch| ch.is_control() || matches!(ch, '<' | '>' | '"' | '|' | '?' | '*'))
            || is_reserved_windows_component(component)
        {
            return Err(unsupported_rar(
                "entry path contains a name unsafe on Windows",
            ));
        }
        relative.push(component);
    }
    Ok(relative)
}

fn is_reserved_windows_component(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or_default();
    let stem = stem.to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                (suffix.len() == 1
                    && suffix.as_bytes()[0].is_ascii_digit()
                    && suffix.as_bytes()[0] != b'0')
                    || matches!(suffix, "¹" | "²" | "³")
            })
        })
}

fn extract_rar(input: &Path) -> CoreResult<tempfile::TempDir> {
    let mut source = std::fs::File::open(input)?;
    let source_len = source.metadata()?.len();
    let scan_len = source_len.min(rars::SFX_SCAN_LIMIT as u64) as usize;
    let mut scan = vec![0; scan_len];
    source.read_exact(&mut scan)?;
    let signature = rars::ArchiveReader::detect(&scan).map_err(unsupported_rar)?;
    if signature.family == rars::ArchiveFamily::Rar13 {
        return Err(unsupported_rar(
            "RAR 1.3/1.4 archives are not supported because this decoder family does not apply the configured parsing limits",
        ));
    }

    let archive = rars::ArchiveReader::read_reader_with_options(source, rar_read_options())
        .map_err(unsupported_rar)?;

    let mut entries = HashMap::new();
    let mut collision_keys = HashSet::new();
    let mut total_unpacked = 0_u64;
    for member in archive.members() {
        if entries.len() >= MAX_RAR_ENTRY_COUNT {
            return Err(unsupported_rar(format!(
                "archive contains more than {MAX_RAR_ENTRY_COUNT} entries"
            )));
        }
        if member.meta.is_encrypted {
            return Err(unsupported_rar("encrypted entries are not supported"));
        }
        if member.meta.is_split_before || member.meta.is_split_after {
            return Err(unsupported_rar(
                "split or multi-volume entries are not supported",
            ));
        }
        if member.meta.is_redirection || member.is_legacy_unix_symlink() {
            return Err(unsupported_rar(
                "symbolic links and redirected entries are not supported",
            ));
        }

        let decoded_name = member.decoded_name(None).map_err(unsupported_rar)?;
        let relative = rar_entry_relative_path(&decoded_name, member.meta.is_directory)?;
        let collision_key = relative.to_string_lossy().replace('\\', "/").to_lowercase();
        if !collision_keys.insert(collision_key) {
            return Err(unsupported_rar(
                "archive contains duplicate paths or Windows case-insensitive collisions",
            ));
        }
        if !member.meta.is_directory {
            if member.meta.unpacked_size > MAX_RAR_MEMBER_OUTPUT_BYTES {
                return Err(unsupported_rar(
                    "a file exceeds the per-file extraction limit",
                ));
            }
            total_unpacked = total_unpacked
                .checked_add(member.meta.unpacked_size)
                .ok_or_else(|| unsupported_rar("total uncompressed size overflows"))?;
            if total_unpacked > MAX_RAR_TOTAL_OUTPUT_BYTES {
                return Err(unsupported_rar(
                    "archive exceeds the total extraction limit",
                ));
            }
        }
        entries.insert(member.meta.name, (relative, member.meta.is_directory));
    }

    let temp_dir = tempfile::tempdir()?;
    archive
        .extract_to_with_options(rar_read_options(), |meta| {
            let (relative, is_directory) = entries
                .get(&meta.name)
                .ok_or(rars::Error::InvalidHeader("RAR entry was not prevalidated"))?;
            let output_path = temp_dir.path().join(relative);
            if *is_directory || meta.is_directory {
                std::fs::create_dir_all(&output_path)?;
                return Ok(Box::new(std::io::sink()));
            }
            if let Some(parent) = output_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(output_path)?;
            Ok(Box::new(output))
        })
        .map_err(unsupported_rar)?;

    Ok(temp_dir)
}

fn classify_rar(
    input: &Path,
    provider: &dyn crate::providers::ModeProvider,
) -> CoreResult<ModPlan> {
    let temp_dir = extract_rar(input)?;
    let name_hint = input.file_stem().and_then(|s| s.to_str());
    let mut plan = classify_folder_with_name_hint(temp_dir.path(), provider, name_hint)?;
    if matches!(plan.format, ModFormat::FolderReplacer) {
        plan.format = ModFormat::Rar;
    }
    if plan.files.is_empty() {
        return Ok(plan);
    }
    Ok(plan.with_staging_dir(temp_dir))
}

fn classify_zip(
    input: &Path,
    provider: &dyn crate::providers::ModeProvider,
) -> CoreResult<ModPlan> {
    let temp_dir = extract_zip_with_limits(
        input,
        MAX_ZIP_ENTRY_COUNT,
        MAX_ZIP_MEMBER_OUTPUT_BYTES,
        MAX_ZIP_TOTAL_OUTPUT_BYTES,
    )?;
    let name_hint = input.file_stem().and_then(|s| s.to_str());
    let mut plan = classify_folder_with_name_hint(temp_dir.path(), provider, name_hint)?;
    // Only relabel a plain folder-replacer result as "Zip" — an AddOnPack or
    // Unsupported result from the folder walk must survive intact (this used to
    // unconditionally overwrite `format`, discarding e.g. an AddOnPack's `pack_name`
    // and silently turning it into an unregistered, un-installable "Zip").
    if matches!(plan.format, ModFormat::FolderReplacer) {
        plan.format = ModFormat::Zip;
    }
    if plan.files.is_empty() {
        return Ok(plan);
    }
    Ok(plan.with_staging_dir(temp_dir))
}

fn classify_seven_zip(
    input: &Path,
    provider: &dyn crate::providers::ModeProvider,
) -> CoreResult<ModPlan> {
    let temp_dir = extract_seven_zip_with_limits(
        input,
        MAX_SEVEN_ZIP_ENTRY_COUNT,
        MAX_SEVEN_ZIP_MEMBER_OUTPUT_BYTES,
        MAX_SEVEN_ZIP_TOTAL_OUTPUT_BYTES,
    )?;
    let name_hint = input.file_stem().and_then(|s| s.to_str());
    let mut plan = classify_folder_with_name_hint(temp_dir.path(), provider, name_hint)?;
    // See classify_zip's note on why this is conditional, not unconditional.
    if matches!(plan.format, ModFormat::FolderReplacer) {
        plan.format = ModFormat::SevenZip;
    }
    if plan.files.is_empty() {
        return Ok(plan);
    }
    Ok(plan.with_staging_dir(temp_dir))
}

/// Finds the first `.oiv` file anywhere under `dir` (recursively), if any.
fn find_any_oiv_file(dir: &Path) -> Option<PathBuf> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .find(|e| {
            e.file_type().is_file()
                && e.path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("oiv"))
        })
        .map(|e| e.path().to_path_buf())
}

/// `.oiv` is never installed by this project — see this module's doc comment for why.
fn classify_oiv(_input: &Path) -> CoreResult<ModPlan> {
    Ok(unsupported(
        "oiv",
        "This app does not install .oiv packages. Please install it with OpenIV instead.",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{
        EnhancedLspdfrProvider, LegacyLspdfrProvider, LegacySpProvider, ModeProvider,
    };

    fn provider(game_root: &Path) -> LegacySpProvider {
        LegacySpProvider::new(game_root.to_path_buf())
    }

    #[test]
    fn asi_targets_game_root() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let mod_file = dir.path().join("cool_mod.asi");
        std::fs::write(&mod_file, b"payload").unwrap();

        let plan = classify(&mod_file, &provider(&game_root)).unwrap();
        assert_eq!(plan.format, ModFormat::Asi);
        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.files[0].target, game_root.join("cool_mod.asi"));
    }

    #[test]
    fn native_dll_targets_game_root() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let mod_file = dir.path().join("native.dll");
        std::fs::write(&mod_file, b"not a real PE file").unwrap();

        let plan = classify(&mod_file, &provider(&game_root)).unwrap();
        assert_eq!(plan.format, ModFormat::NativeDll);
        assert_eq!(plan.files[0].target, game_root.join("native.dll"));
    }

    #[test]
    fn folder_mirrors_into_mods_subfolder() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let mod_folder = dir.path().join("replacer_mod");
        std::fs::create_dir_all(mod_folder.join("update/x64/dlcpacks")).unwrap();
        std::fs::write(mod_folder.join("update/x64/dlcpacks/thing.rpf"), b"payload").unwrap();

        let plan = classify(&mod_folder, &provider(&game_root)).unwrap();
        assert_eq!(plan.format, ModFormat::FolderReplacer);
        assert_eq!(plan.files.len(), 1);
        assert_eq!(
            plan.files[0].target,
            game_root.join("mods/update/x64/dlcpacks/thing.rpf")
        );
    }

    #[test]
    fn a_folder_containing_an_oiv_is_refused_not_installed_as_a_loose_file() {
        // Real-world sample: gta5-mods.com authors commonly zip up a bare .oiv
        // installer + a .url shortcut with no other content. Extracted, that's just a
        // folder containing an .oiv — without the fix, that .oiv would fall through to
        // the generic folder-replacer walk and get "installed" as a loose file at its
        // own path, silently defeating this project's own "never install .oiv" policy.
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let mod_folder = dir.path().join("extracted_zip");
        std::fs::create_dir_all(&mod_folder).unwrap();
        std::fs::write(mod_folder.join("Cool Mod 1.0.oiv"), b"fake oiv payload").unwrap();
        std::fs::write(mod_folder.join("Cool Mod 1.0.url"), b"[InternetShortcut]").unwrap();

        let plan = classify(&mod_folder, &provider(&game_root)).unwrap();
        assert!(matches!(plan.format, ModFormat::Unsupported(_)));
        assert!(plan.files.is_empty());
    }

    #[test]
    fn a_folder_with_no_oiv_is_still_classified_as_a_normal_folder_replacer() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let mod_folder = dir.path().join("replacer_mod");
        std::fs::create_dir_all(&mod_folder).unwrap();
        std::fs::write(mod_folder.join("readme.txt"), b"not an oiv").unwrap();

        let plan = classify(&mod_folder, &provider(&game_root)).unwrap();
        assert_eq!(plan.format, ModFormat::FolderReplacer);
    }

    /// Builds a real `.zip` file at `zip_path` containing `entries` (relative path ->
    /// content), so `classify_zip` gets exercised end-to-end through a genuine archive
    /// rather than a pre-extracted folder — the two real bugs found via a real sample
    /// (name-hint loss, format overwrite) only reproduce through this actual path.
    fn write_test_zip(zip_path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(zip_path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (name, content) in entries {
            writer.start_file(*name, options).unwrap();
            std::io::Write::write_all(&mut writer, content).unwrap();
        }
        writer.finish().unwrap();
    }

    /// Builds a genuine, unencrypted single-volume RAR 5 archive for classifier tests.
    fn write_test_rar(rar_path: &Path, entries: &[(&str, &[u8])]) {
        let mut builder = rars::Builder::new(rars::ArchiveVersion::Rar50);
        for (name, content) in entries {
            builder
                .add_bytes(name.as_bytes().to_vec(), content.to_vec(), None, None)
                .unwrap();
        }
        std::fs::write(rar_path, builder.to_bytes().unwrap()).unwrap();
    }

    fn write_test_seven_zip(seven_zip_path: &Path, entries: &[(&str, &[u8])]) {
        let source_dir = seven_zip_path.with_extension("input");
        for (name, content) in entries {
            let path = source_dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        sevenz_rust::compress_to_path(&source_dir, seven_zip_path).unwrap();
    }

    #[test]
    fn zip_staging_is_removed_when_plan_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let zip_path = dir.path().join("replacer.zip");
        write_test_zip(&zip_path, &[("update.rpf", b"payload")]);

        let plan = classify(&zip_path, &provider(&game_root)).unwrap();
        assert_eq!(plan.format, ModFormat::Zip);
        let staging_dir = plan.files[0].source.parent().unwrap().to_path_buf();
        assert!(staging_dir.exists());

        drop(plan);

        assert!(
            !staging_dir.exists(),
            "ZIP staging should be removed with its plan"
        );
    }

    #[test]
    fn seven_zip_staging_is_removed_when_plan_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let seven_zip_path = dir.path().join("replacer.7z");
        write_test_seven_zip(
            &seven_zip_path,
            &[("update/x64/dlcpacks/thing.rpf", b"payload")],
        );

        let plan = classify(&seven_zip_path, &provider(&game_root)).unwrap();
        assert_eq!(plan.format, ModFormat::SevenZip);
        assert_eq!(plan.files.len(), 1);
        assert_eq!(std::fs::read(&plan.files[0].source).unwrap(), b"payload");
        let staged_file = plan.files[0].source.clone();

        drop(plan);

        assert!(
            !staged_file.exists(),
            "7z staging should be removed with its plan"
        );
    }

    #[test]
    fn zip_extraction_rejects_more_entries_than_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("too-many.zip");
        write_test_zip(&zip_path, &[("one.asi", b"1"), ("two.asi", b"2")]);

        let result = extract_zip_with_limits(&zip_path, 1, 16, 16);

        assert!(matches!(result, Err(CoreError::UnsupportedFormat { .. })));
    }

    #[test]
    fn zip_extraction_rejects_an_oversized_member() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("oversized-member.zip");
        write_test_zip(&zip_path, &[("large.asi", b"payload")]);

        let result = extract_zip_with_limits(&zip_path, 10, 6, 16);

        assert!(matches!(result, Err(CoreError::UnsupportedFormat { .. })));
    }

    #[test]
    fn zip_extraction_rejects_total_uncompressed_size_over_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("oversized-total.zip");
        write_test_zip(&zip_path, &[("one.asi", b"123"), ("two.asi", b"456")]);

        let result = extract_zip_with_limits(&zip_path, 10, 5, 5);

        assert!(matches!(result, Err(CoreError::UnsupportedFormat { .. })));
    }

    #[test]
    fn seven_zip_extraction_rejects_more_entries_than_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let seven_zip_path = dir.path().join("too-many.7z");
        write_test_seven_zip(&seven_zip_path, &[("one.asi", b"1"), ("two.asi", b"2")]);

        let result = extract_seven_zip_with_limits(&seven_zip_path, 1, 16, 16);

        assert!(matches!(result, Err(CoreError::UnsupportedFormat { .. })));
    }

    #[test]
    fn seven_zip_extraction_rejects_an_oversized_member() {
        let dir = tempfile::tempdir().unwrap();
        let seven_zip_path = dir.path().join("oversized-member.7z");
        write_test_seven_zip(&seven_zip_path, &[("large.asi", b"payload")]);

        let result = extract_seven_zip_with_limits(&seven_zip_path, 10, 6, 16);

        assert!(matches!(result, Err(CoreError::UnsupportedFormat { .. })));
    }

    #[test]
    fn seven_zip_extraction_rejects_absolute_entry_paths() {
        let dir = tempfile::tempdir().unwrap();
        let seven_zip_path = dir.path().join("absolute-entry.7z");
        let payload_path = dir.path().join("payload.bin");
        let escaped_path = dir.path().join("escaped.txt");
        std::fs::write(&payload_path, b"payload").unwrap();

        let entry = sevenz_rust::SevenZArchiveEntry::from_path(
            &payload_path,
            escaped_path.to_string_lossy().into_owned(),
        );
        let mut writer = sevenz_rust::SevenZWriter::create(&seven_zip_path).unwrap();
        writer
            .push_archive_entry(entry, Some(std::fs::File::open(&payload_path).unwrap()))
            .unwrap();
        writer.finish().unwrap();

        let result = extract_seven_zip_with_limits(&seven_zip_path, 10, 1024, 1024);

        assert!(
            matches!(result, Err(CoreError::UnsupportedFormat { .. })),
            "7z extraction must reject an absolute entry path"
        );
        assert!(
            !escaped_path.exists(),
            "entry must not be written outside staging"
        );
    }

    #[test]
    fn seven_zip_extraction_rejects_parent_directory_entry_paths() {
        let dir = tempfile::tempdir().unwrap();
        let seven_zip_path = dir.path().join("parent-entry.7z");
        let payload_path = dir.path().join("payload.bin");
        let escaped_path = dir.path().join("escaped.txt");
        let fixture_dir_name = dir.path().file_name().unwrap().to_string_lossy();
        let entry_name = format!("../{fixture_dir_name}/escaped.txt");
        std::fs::write(&payload_path, b"payload").unwrap();

        let entry = sevenz_rust::SevenZArchiveEntry::from_path(&payload_path, entry_name);
        let mut writer = sevenz_rust::SevenZWriter::create(&seven_zip_path).unwrap();
        writer
            .push_archive_entry(entry, Some(std::fs::File::open(&payload_path).unwrap()))
            .unwrap();
        writer.finish().unwrap();

        let result = extract_seven_zip_with_limits(&seven_zip_path, 10, 1024, 1024);

        assert!(
            matches!(result, Err(CoreError::UnsupportedFormat { .. })),
            "7z extraction must reject parent-directory entry paths"
        );
        assert!(
            !escaped_path.exists(),
            "entry must not be written outside staging"
        );
    }

    #[test]
    fn seven_zip_extraction_rejects_total_uncompressed_size_over_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let seven_zip_path = dir.path().join("oversized-total.7z");
        write_test_seven_zip(&seven_zip_path, &[("one.asi", b"123"), ("two.asi", b"456")]);

        let result = extract_seven_zip_with_limits(&seven_zip_path, 10, 5, 5);

        assert!(matches!(result, Err(CoreError::UnsupportedFormat { .. })));
    }

    #[test]
    fn zip_with_dlc_rpf_at_its_root_uses_the_archive_filename_as_pack_name() {
        // Real-world sample: an add-on vehicle zip with dlc.rpf directly at the zip
        // root (no wrapper folder) — extremely common on gta5-mods.com. Before the
        // fix, the pack got named after the OS-random temp extraction folder instead
        // of the mod's actual name.
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let zip_path = dir.path().join("2024 Ford Expedition [Add-On] 1.0.zip");
        write_test_zip(
            &zip_path,
            &[
                ("dlc.rpf", b"fake rpf payload"),
                (
                    "2024 Ford Expedition [Add-On] 1.0.url",
                    b"[InternetShortcut]",
                ),
            ],
        );

        let plan = classify(&zip_path, &provider(&game_root)).unwrap();
        match plan.format {
            ModFormat::AddOnPack { pack_name } => {
                assert_eq!(pack_name, "2024 Ford Expedition [Add-On] 1.0");
            }
            other => panic!("expected AddOnPack, got {other:?}"),
        }
        assert!(plan
            .files
            .iter()
            .all(|f| f.target.to_string_lossy().contains("2024 Ford Expedition")));
    }

    #[test]
    fn zip_with_a_bare_oiv_at_its_root_is_refused_not_relabelled_as_zip() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let zip_path = dir.path().join("Realistic Bullet Hole Size 1.1.zip");
        write_test_zip(
            &zip_path,
            &[
                ("Realistic Bullet Hole Size 1.1.oiv", b"fake oiv payload"),
                ("Realistic Bullet Hole Size 1.1.url", b"[InternetShortcut]"),
            ],
        );

        let plan = classify(&zip_path, &provider(&game_root)).unwrap();
        assert!(matches!(plan.format, ModFormat::Unsupported(_)));
        assert!(plan.files.is_empty());
    }

    #[test]
    fn zip_with_a_plain_replacer_folder_is_still_labelled_zip() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let zip_path = dir.path().join("some_replacer.zip");
        write_test_zip(&zip_path, &[("update/x64/dlcpacks/thing.rpf", b"payload")]);

        let plan = classify(&zip_path, &provider(&game_root)).unwrap();
        assert_eq!(plan.format, ModFormat::Zip);
        assert_eq!(plan.files.len(), 1);
    }

    #[test]
    fn lspdfr_zip_with_game_root_layout_maps_all_files_from_game_root() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let zip_path = dir.path().join("MCCCallouts.zip");
        write_test_zip(
            &zip_path,
            &[
                ("CalloutInterfaceAPI.dll", b"dependency"),
                ("plugins/LSPDFR/MCCCallouts.dll", b"callout"),
                ("lspdfr/audio/MCCCallouts/Audio.dat", b"audio"),
                ("ReadMe.pdf", b"package instructions"),
            ],
        );

        let providers: [Box<dyn ModeProvider>; 2] = [
            Box::new(LegacyLspdfrProvider::new(game_root.clone())),
            Box::new(EnhancedLspdfrProvider::new(game_root.clone())),
        ];
        for provider in providers {
            let plan = classify(&zip_path, provider.as_ref()).unwrap();

            assert_eq!(plan.format, ModFormat::Zip);
            assert_eq!(plan.files.len(), 4);
            assert!(plan
                .files
                .iter()
                .any(|file| { file.target == game_root.join("CalloutInterfaceAPI.dll") }));
            assert!(plan
                .files
                .iter()
                .any(|file| { file.target == game_root.join("plugins/LSPDFR/MCCCallouts.dll") }));
            assert!(plan.files.iter().any(|file| {
                file.target == game_root.join("lspdfr/audio/MCCCallouts/Audio.dat")
            }));
            assert!(plan
                .files
                .iter()
                .any(|file| file.target == game_root.join("ReadMe.pdf")));
        }
    }

    #[test]
    fn lspdfr_zip_strips_the_explicit_grand_theft_auto_v_wrapper_only() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let zip_path = dir.path().join("StoryCallouts.zip");
        write_test_zip(
            &zip_path,
            &[
                ("grand theft auto v/CalloutInterfaceAPI.dll", b"dependency"),
                (
                    "grand theft auto v/plugins/lspdfr/StoryCallouts.dll",
                    b"callout",
                ),
                ("INSTRUCTIONS.txt", b"install the named folder contents"),
            ],
        );

        let plan = classify(&zip_path, &LegacyLspdfrProvider::new(game_root.clone())).unwrap();

        assert_eq!(plan.format, ModFormat::Zip);
        assert_eq!(plan.files.len(), 2);
        assert!(plan
            .files
            .iter()
            .any(|file| { file.target == game_root.join("CalloutInterfaceAPI.dll") }));
        assert!(plan
            .files
            .iter()
            .any(|file| { file.target == game_root.join("plugins/lspdfr/StoryCallouts.dll") }));
        assert!(!plan
            .files
            .iter()
            .any(|file| file.target.ends_with("INSTRUCTIONS.txt")));
    }

    #[test]
    fn ambiguous_lspdfr_zip_with_dlls_is_refused_instead_of_mapped_to_mods() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let zip_path = dir.path().join("UnknownCallout.zip");
        write_test_zip(
            &zip_path,
            &[
                ("UnknownCallout.dll", b"callout"),
                ("readme.txt", b"readme"),
            ],
        );

        let plan = classify(&zip_path, &LegacyLspdfrProvider::new(game_root)).unwrap();

        assert!(matches!(plan.format, ModFormat::Unsupported(_)));
        assert!(plan.files.is_empty());
    }

    #[test]
    fn lspdfr_folder_with_explicit_game_root_layout_maps_from_the_folder_root() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let mod_folder = dir.path().join("extracted_callout");
        std::fs::create_dir_all(mod_folder.join("Plugins/LSPDFR")).unwrap();
        std::fs::write(mod_folder.join("CalloutInterfaceAPI.dll"), b"dependency").unwrap();
        std::fs::write(mod_folder.join("Plugins/LSPDFR/Callout.dll"), b"callout").unwrap();

        let plan = classify(&mod_folder, &LegacyLspdfrProvider::new(game_root.clone())).unwrap();

        assert_eq!(plan.format, ModFormat::FolderReplacer);
        assert_eq!(plan.files.len(), 2);
        assert!(plan
            .files
            .iter()
            .any(|file| file.target == game_root.join("CalloutInterfaceAPI.dll")));
        assert!(plan
            .files
            .iter()
            .any(|file| { file.target == game_root.join("Plugins/LSPDFR/Callout.dll") }));
    }

    #[test]
    fn ambiguous_lspdfr_folder_with_dlls_is_refused_instead_of_mapped_to_mods() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let mod_folder = dir.path().join("unknown_callout");
        std::fs::create_dir_all(&mod_folder).unwrap();
        std::fs::write(mod_folder.join("UnknownCallout.dll"), b"callout").unwrap();

        let plan = classify(&mod_folder, &LegacyLspdfrProvider::new(game_root)).unwrap();

        assert!(matches!(plan.format, ModFormat::Unsupported(_)));
        assert!(plan.files.is_empty());
    }

    #[test]
    fn lspdfr_archive_with_multiple_explicit_roots_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let zip_path = dir.path().join("ambiguous.zip");
        write_test_zip(
            &zip_path,
            &[
                ("Plugins/LSPDFR/Direct.dll", b"direct"),
                ("Grand Theft Auto V/Plugins/LSPDFR/Wrapped.dll", b"wrapped"),
            ],
        );

        let plan = classify(&zip_path, &LegacyLspdfrProvider::new(game_root)).unwrap();

        assert!(matches!(plan.format, ModFormat::Unsupported(_)));
        assert!(plan.files.is_empty());
    }

    #[test]
    fn lspdfr_zip_with_a_plain_replacer_still_targets_mods() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let zip_path = dir.path().join("replacer.zip");
        write_test_zip(&zip_path, &[("update/x64/dlcpacks/thing.rpf", b"payload")]);

        let plan = classify(&zip_path, &LegacyLspdfrProvider::new(game_root.clone())).unwrap();

        assert_eq!(plan.format, ModFormat::Zip);
        assert_eq!(plan.files.len(), 1);
        assert_eq!(
            plan.files[0].target,
            game_root.join("mods/update/x64/dlcpacks/thing.rpf")
        );
    }

    #[test]
    fn invalid_rar_is_rejected_with_a_format_error() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        let mod_file = dir.path().join("archive.rar");
        std::fs::write(&mod_file, b"fake rar").unwrap();

        assert!(matches!(
            classify(&mod_file, &provider(&game_root)),
            Err(CoreError::UnsupportedFormat { .. })
        ));
    }

    #[test]
    fn rar_with_a_plain_replacer_folder_is_classified_and_staged_until_plan_drop() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let rar_path = dir.path().join("replacer.rar");
        write_test_rar(&rar_path, &[("update/x64/dlcpacks/thing.rpf", b"payload")]);

        let plan = classify(&rar_path, &provider(&game_root)).unwrap();
        assert_eq!(plan.format, ModFormat::Rar);
        assert_eq!(plan.files.len(), 1);
        assert_eq!(std::fs::read(&plan.files[0].source).unwrap(), b"payload");
        let staged_file = plan.files[0].source.clone();
        drop(plan);
        assert!(
            !staged_file.exists(),
            "RAR staging should be removed with its plan"
        );
    }

    #[test]
    fn rar_rejects_windows_case_insensitive_path_collisions() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        std::fs::create_dir_all(&game_root).unwrap();
        let rar_path = dir.path().join("collisions.rar");
        write_test_rar(
            &rar_path,
            &[
                ("update/x64/thing.rpf", b"first"),
                ("UPDATE/X64/THING.RPF", b"second"),
            ],
        );

        assert!(matches!(
            classify(&rar_path, &provider(&game_root)),
            Err(CoreError::UnsupportedFormat { .. })
        ));
    }

    #[test]
    fn rar_entry_paths_reject_traversal_and_accept_separators_and_utf8() {
        assert!(rar_entry_relative_path(b"../outside.txt", false).is_err());
        assert!(rar_entry_relative_path(b"..\\outside.txt", false).is_err());
        assert!(rar_entry_relative_path(b"C:\\outside.txt", false).is_err());
        assert_eq!(
            rar_entry_relative_path(b"update\\x64\\thing.rpf", false).unwrap(),
            Path::new("update").join("x64").join("thing.rpf")
        );
        assert_eq!(
            rar_entry_relative_path("mods/警車.rpf".as_bytes(), false).unwrap(),
            Path::new("mods").join("警車.rpf")
        );
    }

    #[test]
    fn add_on_vehicle_pack_targets_dlcpacks_folder() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        let pack_folder = dir.path().join("MyAddonCar");
        std::fs::create_dir_all(&pack_folder).unwrap();
        std::fs::write(pack_folder.join("dlc.rpf"), b"payload").unwrap();

        let plan = classify(&pack_folder, &provider(&game_root)).unwrap();
        match plan.format {
            ModFormat::AddOnPack { pack_name } => assert_eq!(pack_name, "MyAddonCar"),
            other => panic!("expected AddOnPack, got {other:?}"),
        }
        assert_eq!(
            plan.files[0].target,
            game_root.join("mods/update/x64/dlcpacks/MyAddonCar/dlc.rpf")
        );
    }

    #[test]
    fn add_on_pack_one_level_down_is_still_detected() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        let outer = dir.path().join("DownloadedZipContents");
        let inner = outer.join("MyAddonCar");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("dlc.rpf"), b"payload").unwrap();

        let plan = classify(&outer, &provider(&game_root)).unwrap();
        match plan.format {
            ModFormat::AddOnPack { pack_name } => assert_eq!(pack_name, "MyAddonCar"),
            other => panic!("expected AddOnPack, got {other:?}"),
        }
    }

    #[test]
    fn menyoo_xml_targets_detected_subfolder() {
        let dir = tempfile::tempdir().unwrap();
        let game_root = dir.path().join("game");
        let mod_file = dir.path().join("cool_outfit.xml");
        std::fs::write(&mod_file, b"<Outfit></Outfit>").unwrap();

        let plan = classify(&mod_file, &provider(&game_root)).unwrap();
        assert_eq!(plan.format, ModFormat::MenyooXml);
        assert_eq!(
            plan.files[0].target,
            game_root.join("menyooStuff/Outfits/cool_outfit.xml")
        );
    }
}
