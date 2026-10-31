// OpenAI-Codex
// Preparation only: run exclusively after separate remote experiment approval.
#![cfg(windows)]

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Component, Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{LocalFree, ERROR_SHARING_VIOLATION, FILETIME};
use windows_sys::Win32::Security::Authorization::{
    GetSecurityInfo, SetSecurityInfo, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetSecurityDescriptorLength,
    IsValidSecurityDescriptor, BACKUP_SECURITY_INFORMATION, DACL_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED, SE_SELF_RELATIVE,
};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileInformationByHandle, ReplaceFileW, SetFileTime, BY_HANDLE_FILE_INFORMATION,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, READ_CONTROL, WRITE_DAC,
};
use windows_sys::Win32::System::SystemServices::ACCESS_SYSTEM_SECURITY;

const OLD_BYTES: &[u8] = b"original fixture payload";
const NEW_BYTES: &[u8] = b"replacement fixture payload";
const OLD_STREAM: &[u8] = b"original-only stream";
const NEW_STREAM: &[u8] = b"replacement-only stream";

// Deliberately omit paths, descriptors, SIDs and the OS error message from Debug.
#[derive(Debug)]
struct ProbeError {
    stage: &'static str,
    code: Option<i32>,
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "probe_stage={} os_code={:?}",
            self.stage, self.code
        )
    }
}

fn failure(stage: &'static str, error: io::Error) -> ProbeError {
    ProbeError {
        stage,
        code: error.raw_os_error(),
    }
}

fn ensure(condition: bool, stage: &'static str) -> Result<(), ProbeError> {
    if condition {
        Ok(())
    } else {
        Err(ProbeError { stage, code: None })
    }
}

fn boolean_result(value: i32, stage: &'static str) -> Result<(), ProbeError> {
    if value != 0 {
        Ok(())
    } else {
        Err(failure(stage, io::Error::last_os_error()))
    }
}

fn ordinary_directory(path: &Path) -> Result<(), ProbeError> {
    // Reject linked/reparse ancestors before trusting an explicitly supplied root.
    for ancestor in path.ancestors() {
        let metadata =
            fs::symlink_metadata(ancestor).map_err(|e| failure("scratch_ancestor", e))?;
        ensure(
            metadata.is_dir() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
            "ordinary_scratch_ancestor",
        )?;
    }
    Ok(())
}

struct Fixture {
    target: PathBuf,
    replacement: PathBuf,
    backup: PathBuf,
    case: &'static str,
}

impl Fixture {
    fn create(case: &'static str) -> Result<Self, ProbeError> {
        ensure(
            env::var("GTAVMM_METADATA_PROBE_APPROVED").as_deref() == Ok("yes"),
            "explicit_opt_in",
        )?;
        ensure(
            env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
                && env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted")
                && env::var("RUNNER_OS").as_deref() == Ok("Windows"),
            "hosted_windows_only",
        )?;
        let run = env::var("GITHUB_RUN_ID").map_err(|_| ProbeError {
            stage: "run_id",
            code: None,
        })?;
        let attempt = env::var("GITHUB_RUN_ATTEMPT").map_err(|_| ProbeError {
            stage: "run_attempt",
            code: None,
        })?;
        ensure(
            !run.is_empty()
                && run.bytes().all(|b| b.is_ascii_digit())
                && !attempt.is_empty()
                && attempt.bytes().all(|b| b.is_ascii_digit()),
            "numeric_run_identity",
        )?;
        let temp = env::var_os("RUNNER_TEMP")
            .map(PathBuf::from)
            .ok_or(ProbeError {
                stage: "runner_temp",
                code: None,
            })?;
        ensure(temp.is_absolute(), "absolute_runner_temp")?;
        ordinary_directory(&temp)?;
        let temp = fs::canonicalize(temp).map_err(|e| failure("canonical_runner_temp", e))?;
        let root = env::var_os("GTAVMM_METADATA_PROBE_ROOT")
            .map(PathBuf::from)
            .ok_or(ProbeError {
                stage: "scratch_root",
                code: None,
            })?;
        ensure(
            root.is_absolute()
                && !root
                    .components()
                    .any(|c| matches!(c, Component::ParentDir | Component::CurDir)),
            "absolute_scratch_root",
        )?;
        // Canonicalize only the existing parent, never follow an existing fixture.
        let parent = root.parent().ok_or(ProbeError {
            stage: "scratch_parent",
            code: None,
        })?;
        ordinary_directory(parent)?;
        ensure(
            fs::canonicalize(parent).map_err(|e| failure("canonical_scratch_parent", e))? == temp,
            "scratch_direct_child",
        )?;
        let name = format!("gtavmm-metadata-probe-{run}-{attempt}");
        ensure(
            root.file_name() == Some(std::ffi::OsStr::new(&name)),
            "dedicated_scratch_name",
        )?;
        match fs::create_dir(&root) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                ordinary_directory(&root)?
            }
            Err(error) => return Err(failure("create_scratch", error)),
        }
        let directory = root.join(case);
        fs::create_dir(&directory).map_err(|e| failure("exclusive_case_directory", e))?;
        let fixture = Self {
            target: directory.join("target.bin"),
            replacement: directory.join("replacement.bin"),
            backup: directory.join("backup.bin"),
            case,
        };
        println!("fixture={case} retained=true cleanup=none");
        create_payload(&fixture.target, OLD_BYTES)?;
        create_payload(&fixture.replacement, NEW_BYTES)?;
        Ok(fixture)
    }

    fn prepare_contrasting_metadata(&self) -> Result<(), ProbeError> {
        // Read full security BEFORE changing fixture ACLs, streams or timestamps.
        let target = security_file(&self.target, WRITE_DAC | FILE_WRITE_ATTRIBUTES)?;
        let replacement = security_file(&self.replacement, FILE_WRITE_ATTRIBUTES)?;
        let descriptor = SecurityAllocation::read(&target)?;
        let _replacement_descriptor = SecurityAllocation::read(&replacement)?;
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl = null_mut();
        // SAFETY: descriptor owns the allocation; all output pointers are live.
        boolean_result(
            unsafe {
                GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted)
            },
            "read_fixture_dacl",
        )?;
        ensure(present != 0 && !dacl.is_null(), "nonnull_fixture_dacl")?;
        // SAFETY: only our newly created ordinary file is modified. The non-null
        // DACL is borrowed from the still-live descriptor, never an error fallback.
        let status = unsafe {
            SetSecurityInfo(
                target.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                dacl,
                null(),
            )
        };
        if status != 0 {
            return Err(failure(
                "protect_fixture_dacl",
                io::Error::from_raw_os_error(status as i32),
            ));
        }
        let protected = SecurityAllocation::read(&target)?;
        let mut control = 0;
        let mut revision = 0;
        // SAFETY: protected owns the current descriptor, outputs remain live.
        boolean_result(
            unsafe { GetSecurityDescriptorControl(protected.0, &mut control, &mut revision) },
            "fixture_dacl_control",
        )?;
        ensure(
            control & SE_DACL_PROTECTED != 0,
            "fixture_dacl_is_protected",
        )?;
        set_creation_time(&target, 132_537_600_000_000_000)?;
        set_creation_time(&replacement, 132_538_464_000_000_000)?;
        create_payload(&stream_path(&self.target, "original"), OLD_STREAM)?;
        create_payload(&stream_path(&self.replacement, "replacement"), NEW_STREAM)?;
        Ok(())
    }

    fn replace(&self) -> Result<(), ProbeError> {
        let wide = |path: &Path| {
            path.as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>()
        };
        let target = wide(&self.target);
        let replacement = wide(&self.replacement);
        let backup = wide(&self.backup);
        // SAFETY: NUL-terminated buffers remain live through the synchronous call.
        // No IGNORE_ACL/MERGE flags; no unsupported WRITE_THROUGH flag.
        let result = unsafe {
            ReplaceFileW(
                target.as_ptr(),
                replacement.as_ptr(),
                backup.as_ptr(),
                0,
                null(),
                null(),
            )
        };
        boolean_result(result, "replace")
    }

    fn report_paths(&self) {
        for (name, path) in [
            ("target.bin", &self.target),
            ("replacement.bin", &self.replacement),
            ("backup.bin", &self.backup),
        ] {
            match fs::symlink_metadata(path) {
                Ok(meta) => println!(
                    "fixture={}/{} exists=true regular={} length={}",
                    self.case,
                    name,
                    meta.is_file(),
                    meta.len()
                ),
                Err(e) => println!(
                    "fixture={}/{} observation_error={:?}",
                    self.case,
                    name,
                    e.raw_os_error()
                ),
            }
        }
    }
}

// No Drop implementation: neither success nor failure automatically deletes data.
fn create_payload(path: &Path, bytes: &[u8]) -> Result<(), ProbeError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| failure("exclusive_payload", e))?;
    file.write_all(bytes)
        .map_err(|e| failure("payload_write", e))?;
    file.sync_all().map_err(|e| failure("payload_flush", e))
}

fn stream_path(path: &Path, stream: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(":");
    name.push(stream);
    PathBuf::from(name)
}

fn read_stream(path: &Path, stream: &str) -> Result<Vec<u8>, ProbeError> {
    fs::read(stream_path(path, stream)).map_err(|e| failure("read_ads", e))
}

fn security_file(path: &Path, extra_access: u32) -> Result<File, ProbeError> {
    OpenOptions::new()
        .access_mode(READ_CONTROL | ACCESS_SYSTEM_SECURITY | FILE_READ_ATTRIBUTES | extra_access)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(path)
        .map_err(|e| failure("full_security_unverifiable", e))
}

struct SecurityAllocation(PSECURITY_DESCRIPTOR);

impl SecurityAllocation {
    fn read(file: &File) -> Result<Self, ProbeError> {
        let mut pointer = null_mut();
        // SAFETY: borrowed File owns its live handle; output pointer is initialized.
        let status = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                BACKUP_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
                &mut pointer,
            )
        };
        if status != 0 {
            return Err(failure(
                "full_security_unverifiable",
                io::Error::from_raw_os_error(status as i32),
            ));
        }
        ensure(!pointer.is_null(), "security_descriptor_present")?;
        let descriptor = Self(pointer);
        // SAFETY: successful GetSecurityInfo returned this live LocalAlloc buffer.
        ensure(
            unsafe { IsValidSecurityDescriptor(descriptor.0) } != 0,
            "valid_security_descriptor",
        )?;
        Ok(descriptor)
    }

    fn bytes(&self) -> Result<Vec<u8>, ProbeError> {
        let mut control = 0;
        let mut revision = 0;
        // SAFETY: descriptor is valid and owned by self, outputs have correct types.
        boolean_result(
            unsafe { GetSecurityDescriptorControl(self.0, &mut control, &mut revision) },
            "descriptor_control",
        )?;
        ensure(control & SE_SELF_RELATIVE != 0, "self_relative_descriptor")?;
        // SAFETY: same validated live descriptor. Length is checked before slicing.
        let length = unsafe { GetSecurityDescriptorLength(self.0) } as usize;
        ensure((20..=1_048_576).contains(&length), "descriptor_length")?;
        // Byte equality is conservative: layout differences can reject equivalent
        // descriptors. Equality is not proof for security identities not contrasted.
        Ok(unsafe { std::slice::from_raw_parts(self.0.cast::<u8>(), length) }.to_vec())
    }
}

impl Drop for SecurityAllocation {
    fn drop(&mut self) {
        // SAFETY: only the successful GetSecurityInfo allocation is owned here.
        unsafe {
            LocalFree(self.0);
        }
    }
}

fn filetime(value: FILETIME) -> u64 {
    (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
}

fn set_creation_time(file: &File, ticks: u64) -> Result<(), ProbeError> {
    let time = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    // SAFETY: File owns the handle, time lives through the call; other times omitted.
    boolean_result(
        unsafe { SetFileTime(file.as_raw_handle(), &time, null(), null()) },
        "set_fixture_creation_time",
    )
}

struct Observation {
    security: Vec<u8>,
    creation_time: u64,
    last_write: u64,
    last_access: u64,
    attributes: u32,
    file_id: (u32, u32, u32),
}

impl Observation {
    fn stable_metadata_eq(&self, other: &Self) -> bool {
        // Reading payloads can itself change last-access time; do not classify
        // that observer effect as a failed replacement writing the file.
        self.security == other.security
            && self.creation_time == other.creation_time
            && self.last_write == other.last_write
            && self.attributes == other.attributes
            && self.file_id == other.file_id
    }
}

fn observe(path: &Path) -> Result<Observation, ProbeError> {
    let file = security_file(path, 0)?;
    let descriptor = SecurityAllocation::read(&file)?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: File and initialized correctly sized output remain live.
    boolean_result(
        unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) },
        "file_information",
    )?;
    Ok(Observation {
        security: descriptor.bytes()?,
        creation_time: filetime(info.ftCreationTime),
        last_write: filetime(info.ftLastWriteTime),
        last_access: filetime(info.ftLastAccessTime),
        attributes: info.dwFileAttributes,
        file_id: (
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
        ),
    })
}

#[test]
#[ignore = "Requires separately approved GitHub-hosted Windows scratch experiment"]
fn replacement_observes_security_and_metadata_preservation() -> Result<(), ProbeError> {
    let fixture = Fixture::create("preservation")?;
    fixture.prepare_contrasting_metadata()?;
    let original = observe(&fixture.target)?;
    let replacement = observe(&fixture.replacement)?;
    ensure(
        original.security != replacement.security,
        "security_contrast",
    )?;
    ensure(
        original.creation_time != replacement.creation_time,
        "creation_contrast",
    )?;
    let result = fixture.replace();
    fixture.report_paths();
    result?;
    let restored = observe(&fixture.target)?;
    let backup = observe(&fixture.backup)?;
    ensure(
        fs::read(&fixture.target).map_err(|e| failure("target_bytes", e))? == NEW_BYTES,
        "replacement_bytes",
    )?;
    ensure(
        fs::read(&fixture.backup).map_err(|e| failure("backup_bytes", e))? == OLD_BYTES,
        "original_backup_bytes",
    )?;
    ensure(
        !fixture
            .replacement
            .try_exists()
            .map_err(|e| failure("replacement_exists", e))?,
        "replacement_name_consumed",
    )?;
    ensure(
        restored.security == original.security,
        "target_security_bytes",
    )?;
    ensure(
        backup.security == original.security,
        "backup_security_bytes",
    )?;
    ensure(
        restored.creation_time == original.creation_time,
        "target_creation_time",
    )?;
    ensure(
        restored.file_id == replacement.file_id,
        "replacement_file_id",
    )?;
    ensure(
        backup.file_id == original.file_id,
        "original_backup_file_id",
    )?;
    ensure(
        read_stream(&fixture.target, "original")? == OLD_STREAM,
        "original_ads",
    )?;
    ensure(
        read_stream(&fixture.target, "replacement")? == NEW_STREAM,
        "replacement_ads",
    )?;
    println!("probe=preservation observed=true coverage=partial full_security_proof=false");
    println!(
        "attributes_equal={} last_write_equal={} last_access_equal={}",
        restored.attributes == original.attributes,
        restored.last_write == original.last_write,
        restored.last_access == original.last_access
    );
    println!("uncovered=distinct_owner,distinct_group,distinct_sacl,efs,compression,power_loss,1175,1176,1177");
    Ok(())
}

#[test]
#[ignore = "Requires separately approved GitHub-hosted Windows scratch experiment"]
fn denied_delete_sharing_preserves_all_recovery_files() -> Result<(), ProbeError> {
    let fixture = Fixture::create("deny-delete")?;
    // Full-security readability is required before attempting any replacement.
    let original = observe(&fixture.target)?;
    let replacement = observe(&fixture.replacement)?;
    let blocker = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(&fixture.target)
        .map_err(|e| failure("sharing_handle", e))?;
    let result = fixture.replace();
    // Keep the conflicting handle alive until ReplaceFileW has returned.
    drop(blocker);
    fixture.report_paths();
    let error = result.err().ok_or(ProbeError {
        stage: "replacement_must_fail",
        code: None,
    })?;
    println!(
        "probe=deny-delete stage={} os_code={:?}",
        error.stage, error.code
    );
    ensure(
        error.stage == "replace" && error.code == Some(ERROR_SHARING_VIOLATION as i32),
        "expected_sharing_violation",
    )?;
    ensure(
        fs::read(&fixture.target).map_err(|e| failure("target_bytes", e))? == OLD_BYTES,
        "original_target_bytes",
    )?;
    ensure(
        fs::read(&fixture.replacement).map_err(|e| failure("replacement_bytes", e))? == NEW_BYTES,
        "replacement_retained",
    )?;
    ensure(
        observe(&fixture.target)?.stable_metadata_eq(&original),
        "original_metadata_retained",
    )?;
    ensure(
        observe(&fixture.replacement)?.stable_metadata_eq(&replacement),
        "replacement_metadata_retained",
    )?;
    ensure(
        !fixture
            .backup
            .try_exists()
            .map_err(|e| failure("backup_exists", e))?,
        "no_backup_created",
    )?;
    println!("probe=deny-delete observed=true coverage=sharing_only full_security_proof=false");
    Ok(())
}
