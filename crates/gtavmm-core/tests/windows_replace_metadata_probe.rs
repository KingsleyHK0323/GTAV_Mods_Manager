// OpenAI-Codex
// Preparation only: run exclusively after separate remote experiment approval.
#![cfg(windows)]

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Component, Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use windows_sys::Win32::Foundation::{
    GetLastError, LocalFree, SetLastError, ERROR_SHARING_VIOLATION, FILETIME, LUID,
};
use windows_sys::Win32::Security::Authorization::{
    GetSecurityInfo, SetSecurityInfo, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, GetKernelObjectSecurity, GetSecurityDescriptorControl,
    GetSecurityDescriptorDacl, GetSecurityDescriptorGroup, GetSecurityDescriptorLength,
    GetSecurityDescriptorOwner, GetSecurityDescriptorSacl, GetTokenInformation,
    IsValidSecurityDescriptor, LookupPrivilegeValueW, TokenPrivileges, BACKUP_SECURITY_INFORMATION,
    DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION, LUID_AND_ATTRIBUTES,
    OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    SACL_SECURITY_INFORMATION, SE_DACL_PROTECTED, SE_PRIVILEGE_ENABLED, SE_SELF_RELATIVE,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileInformationByHandle, ReplaceFileW, SetFileTime, BY_HANDLE_FILE_INFORMATION,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, READ_CONTROL, WRITE_DAC,
};
use windows_sys::Win32::System::SystemServices::ACCESS_SYSTEM_SECURITY;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

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

static PRIVILEGE_SERIAL: Mutex<()> = Mutex::new(());
static RESTORE_FAILED: AtomicBool = AtomicBool::new(false);

fn same_luid(left: LUID, right: LUID) -> bool {
    left.LowPart == right.LowPart && left.HighPart == right.HighPart
}

fn privilege_attributes(token: &OwnedHandle, luid: LUID) -> Result<u32, ProbeError> {
    // Initialized u64 storage provides alignment for the native trailing array.
    // A token exceeding this fixed bound is refused rather than partially read.
    let mut words = vec![0u64; 8192];
    let capacity = std::mem::size_of_val(words.as_slice());
    let mut returned = 0;
    // SAFETY: token remains owned, buffer has capacity bytes and native alignment.
    boolean_result(
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenPrivileges,
                words.as_mut_ptr().cast(),
                capacity as u32,
                &mut returned,
            )
        },
        "query_security_privilege",
    )?;
    let length = returned as usize;
    let offset = std::mem::offset_of!(TOKEN_PRIVILEGES, Privileges);
    ensure(
        length >= offset && length <= capacity,
        "privilege_buffer_length",
    )?;
    // SAFETY: length contains the native count field; words are initialized/aligned.
    let count = unsafe { *words.as_ptr().cast::<u32>() } as usize;
    ensure(
        count <= (length - offset) / std::mem::size_of::<LUID_AND_ATTRIBUTES>(),
        "privilege_array_length",
    )?;
    // SAFETY: the count/offset were checked against the actual returned buffer.
    let entries = unsafe {
        std::slice::from_raw_parts(
            words
                .as_ptr()
                .cast::<u8>()
                .add(offset)
                .cast::<LUID_AND_ATTRIBUTES>(),
            count,
        )
    };
    entries
        .iter()
        .find(|entry| same_luid(entry.Luid, luid))
        .map(|entry| entry.Attributes)
        .ok_or(ProbeError {
            stage: "security_privilege_absent",
            code: None,
        })
}

struct SecurityPrivilege {
    token: OwnedHandle,
    luid: LUID,
    initial: u32,
    dirty: bool,
}

impl SecurityPrivilege {
    fn open(allow_adjustment: bool) -> Result<Self, ProbeError> {
        let mut handle = null_mut();
        let access = TOKEN_QUERY
            | if allow_adjustment {
                TOKEN_ADJUST_PRIVILEGES
            } else {
                0
            };
        // SAFETY: output handle is initialized; pseudo process handle is borrowed.
        boolean_result(
            unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut handle) },
            "open_test_process_token",
        )?;
        ensure(!handle.is_null(), "token_handle_present")?;
        // SAFETY: successful OpenProcessToken transfers one owned, closable handle.
        let token = unsafe { OwnedHandle::from_raw_handle(handle) };
        let mut luid = LUID::default();
        // SAFETY: literal is NUL-terminated; the LUID output lives through the call.
        boolean_result(
            unsafe {
                LookupPrivilegeValueW(
                    null(),
                    windows_sys::core::w!("SeSecurityPrivilege"),
                    &mut luid,
                )
            },
            "lookup_security_privilege",
        )?;
        let initial = privilege_attributes(&token, luid)?;
        println!(
            "security_privilege_present=true initially_enabled={}",
            initial & SE_PRIVILEGE_ENABLED != 0
        );
        Ok(Self {
            token,
            luid,
            initial,
            dirty: false,
        })
    }

    fn enable(&mut self, allow_adjustment: bool) -> Result<(), ProbeError> {
        if self.initial & SE_PRIVILEGE_ENABLED != 0 {
            return Ok(());
        }
        ensure(
            allow_adjustment,
            "security_privilege_explicit_opt_in_required",
        )?;
        let request = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: self.luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let mut previous = TOKEN_PRIVILEGES::default();
        let mut returned = 0;
        // Arm restoration before an API can change the process-wide token.
        self.dirty = true;
        // SAFETY: all native buffers have the generated type and correct size.
        // GetLastError is captured immediately, including success/NOT_ALL_ASSIGNED.
        let (result, code) = unsafe {
            SetLastError(0);
            let result = AdjustTokenPrivileges(
                self.token.as_raw_handle(),
                0,
                &request,
                std::mem::size_of::<TOKEN_PRIVILEGES>() as u32,
                &mut previous,
                &mut returned,
            );
            (result, GetLastError())
        };
        if result == 0 || code != 0 {
            return Err(failure(
                "enable_security_privilege",
                io::Error::from_raw_os_error(code as i32),
            ));
        }
        ensure(
            previous.PrivilegeCount == 1
                && same_luid(previous.Privileges[0].Luid, self.luid)
                && previous.Privileges[0].Attributes == self.initial,
            "previous_privilege_state",
        )?;
        // Use the actual previous state returned by the successful adjustment.
        self.initial = previous.Privileges[0].Attributes;
        ensure(
            privilege_attributes(&self.token, self.luid)? & SE_PRIVILEGE_ENABLED != 0,
            "security_privilege_enabled_verified",
        )?;
        println!("security_privilege_adjusted=true");
        Ok(())
    }

    fn restore_verified(&mut self) -> Result<(), ProbeError> {
        let changed = self.dirty;
        if changed {
            let request = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES {
                    Luid: self.luid,
                    Attributes: self.initial & SE_PRIVILEGE_ENABLED,
                }],
            };
            // SAFETY: valid owned token; restore only this privilege's enabled bit.
            let (result, code) = unsafe {
                SetLastError(0);
                let result = AdjustTokenPrivileges(
                    self.token.as_raw_handle(),
                    0,
                    &request,
                    0,
                    null_mut(),
                    null_mut(),
                );
                (result, GetLastError())
            };
            if result == 0 || code != 0 {
                return Err(failure(
                    "restore_security_privilege",
                    io::Error::from_raw_os_error(code as i32),
                ));
            }
        }
        ensure(
            privilege_attributes(&self.token, self.luid)? & SE_PRIVILEGE_ENABLED
                == self.initial & SE_PRIVILEGE_ENABLED,
            "security_privilege_restore_verified",
        )?;
        self.dirty = false;
        println!("security_privilege_restored=true changed={changed}");
        Ok(())
    }
}

impl Drop for SecurityPrivilege {
    fn drop(&mut self) {
        if self.dirty {
            // Fallback only. A failed explicit restoration already invalidated the
            // experiment, even if this attempt succeeds during stack unwinding.
            RESTORE_FAILED.store(true, Ordering::SeqCst);
            if let Err(error) = self.restore_verified() {
                eprintln!("security_privilege_fallback_restore_failed {error}");
            }
        }
    }
}

fn with_security_privilege(
    body: fn(&mut SecurityPrivilege) -> Result<(), ProbeError>,
) -> Result<(), ProbeError> {
    let outcome = with_security_privilege_impl(body);
    if let Err(error) = &outcome {
        eprintln!(
            "probe=failure stage={} os_code={:?}",
            error.stage, error.code
        );
    }
    outcome
}

fn with_security_privilege_impl(
    body: fn(&mut SecurityPrivilege) -> Result<(), ProbeError>,
) -> Result<(), ProbeError> {
    // Validate all hosted/approval/path gates BEFORE opening or adjusting a token.
    let _root = approved_scratch_root()?;
    let _serial = PRIVILEGE_SERIAL.lock().map_err(|_| ProbeError {
        stage: "privilege_serialization",
        code: None,
    })?;
    ensure(
        !RESTORE_FAILED.load(Ordering::SeqCst),
        "previous_restore_failure",
    )?;
    let allow =
        env::var("GTAVMM_METADATA_PROBE_SECURITY_PRIVILEGE_APPROVED").as_deref() == Ok("yes");
    let mut privilege = SecurityPrivilege::open(allow)?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        privilege.enable(allow)?;
        body(&mut privilege)
    }));
    // Explicit restoration runs for successful cases, ordinary errors and panics.
    if let Err(error) = privilege.restore_verified() {
        RESTORE_FAILED.store(true, Ordering::SeqCst);
        if let Ok(Err(original)) = &result {
            eprintln!(
                "probe=original-failure stage={} os_code={:?}",
                original.stage, original.code
            );
        }
        return Err(error);
    }
    match result {
        Ok(outcome) => outcome,
        Err(panic) => {
            eprintln!("probe=panic security_privilege_restore_verified=true");
            std::panic::resume_unwind(panic)
        }
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

fn approved_scratch_root() -> Result<PathBuf, ProbeError> {
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
    Ok(root)
}

impl Fixture {
    fn create(case: &'static str) -> Result<Self, ProbeError> {
        let root = approved_scratch_root()?;
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
        let target = security_file(
            &self.target,
            WRITE_DAC | FILE_WRITE_ATTRIBUTES,
            "prepare_target_open",
        )?;
        let replacement = security_file(
            &self.replacement,
            FILE_WRITE_ATTRIBUTES,
            "prepare_replacement_open",
        )?;
        let descriptor = SecurityAllocation::read(&target, "prepare_target_get_security_info")?;
        let _replacement_descriptor =
            SecurityAllocation::read(&replacement, "prepare_replacement_get_security_info")?;
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
        println!("probe=fixture phase=protect_target_dacl api=SetSecurityInfo event=before");
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
        println!("probe=fixture phase=protect_target_dacl api=SetSecurityInfo event=after os_code={status}");
        if status != 0 {
            return Err(failure(
                "protect_fixture_dacl",
                io::Error::from_raw_os_error(status as i32),
            ));
        }
        let protected = SecurityAllocation::read(&target, "protected_target_get_security_info")?;
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
        println!("probe=fixture phase=target_dacl_protected event=complete");
        println!("probe=fixture phase=target_creation_time api=SetFileTime event=before");
        set_creation_time(&target, 132_537_600_000_000_000)?;
        println!("probe=fixture phase=target_creation_time event=complete");
        println!("probe=fixture phase=replacement_creation_time api=SetFileTime event=before");
        set_creation_time(&replacement, 132_538_464_000_000_000)?;
        println!("probe=fixture phase=replacement_creation_time event=complete");
        println!("probe=fixture phase=target_original_ads event=before");
        create_payload(&stream_path(&self.target, "original"), OLD_STREAM)?;
        println!("probe=fixture phase=target_original_ads event=complete");
        println!("probe=fixture phase=replacement_ads event=before");
        create_payload(&stream_path(&self.replacement, "replacement"), NEW_STREAM)?;
        println!("probe=fixture phase=replacement_ads event=complete");
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

fn security_file(path: &Path, extra_access: u32, stage: &'static str) -> Result<File, ProbeError> {
    let access = READ_CONTROL | ACCESS_SYSTEM_SECURITY | FILE_READ_ATTRIBUTES | extra_access;
    println!("probe=security phase={stage} api=OpenOptions_open event=before access_mask={access}");
    let result = OpenOptions::new()
        .access_mode(access)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(path);
    match result {
        Ok(file) => {
            println!("probe=security phase={stage} api=OpenOptions_open event=after os_code=0");
            Ok(file)
        }
        Err(error) => {
            println!(
                "probe=security phase={stage} api=OpenOptions_open event=after os_code={:?}",
                error.raw_os_error()
            );
            Err(failure(stage, error))
        }
    }
}

struct SecurityAllocation(PSECURITY_DESCRIPTOR);

impl SecurityAllocation {
    fn read(file: &File, stage: &'static str) -> Result<Self, ProbeError> {
        Self::query(file, stage, BACKUP_SECURITY_INFORMATION)
    }

    fn query(file: &File, stage: &'static str, information: u32) -> Result<Self, ProbeError> {
        let mut pointer = null_mut();
        println!("probe=security phase={stage} api=GetSecurityInfo event=before security_information={information}");
        // SAFETY: borrowed File owns its live handle; output pointer is initialized.
        let status = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                information,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
                &mut pointer,
            )
        };
        if status != 0 {
            println!(
                "probe=security phase={stage} api=GetSecurityInfo event=after os_code={status}"
            );
            return Err(failure(stage, io::Error::from_raw_os_error(status as i32)));
        }
        ensure(!pointer.is_null(), "security_descriptor_present")?;
        let descriptor = Self(pointer);
        // Own the allocation before reporting success, including on output panic.
        println!("probe=security phase={stage} api=GetSecurityInfo event=after os_code=0");
        // SAFETY: successful GetSecurityInfo returned this live LocalAlloc buffer.
        let valid = unsafe { IsValidSecurityDescriptor(descriptor.0) } != 0;
        println!("probe=query_descriptor phase={stage} valid={valid}");
        ensure(valid, "valid_security_descriptor")?;
        Ok(descriptor)
    }

    fn report_requested_parts(
        &self,
        information: u32,
        stage: &'static str,
    ) -> Result<(), ProbeError> {
        let all = information & BACKUP_SECURITY_INFORMATION != 0;
        let owner_requested = all || information & OWNER_SECURITY_INFORMATION != 0;
        let group_requested = all || information & GROUP_SECURITY_INFORMATION != 0;
        let dacl_requested = all || information & DACL_SECURITY_INFORMATION != 0;
        let sacl_requested = all || information & SACL_SECURITY_INFORMATION != 0;
        let mut owner = null_mut();
        let mut group = null_mut();
        let mut dacl = null_mut();
        let mut sacl = null_mut();
        let mut defaulted = 0;
        let mut dacl_present = 0;
        let mut sacl_present = 0;
        // SAFETY: self owns a validated live descriptor. Each requested getter
        // receives correctly typed output storage, never an unrequested pointer.
        if owner_requested {
            boolean_result(
                unsafe { GetSecurityDescriptorOwner(self.0, &mut owner, &mut defaulted) },
                "query_parts_owner",
            )?;
        }
        if group_requested {
            boolean_result(
                unsafe { GetSecurityDescriptorGroup(self.0, &mut group, &mut defaulted) },
                "query_parts_group",
            )?;
        }
        if dacl_requested {
            boolean_result(
                unsafe {
                    GetSecurityDescriptorDacl(self.0, &mut dacl_present, &mut dacl, &mut defaulted)
                },
                "query_parts_dacl",
            )?;
        }
        if sacl_requested {
            boolean_result(
                unsafe {
                    GetSecurityDescriptorSacl(self.0, &mut sacl_present, &mut sacl, &mut defaulted)
                },
                "query_parts_sacl",
            )?;
        }
        // Absent and null ACLs are observations, not fabricated empty ACLs or a
        // reason to allow replacement. No pointer or SID bytes are emitted.
        println!("probe=query_parts phase={stage} descriptor_valid=true owner_requested={owner_requested} owner_present={} group_requested={group_requested} group_present={} dacl_requested={dacl_requested} dacl_present={} dacl_null={} sacl_requested={sacl_requested} sacl_present={} sacl_null={}",
            !owner.is_null(), !group.is_null(), dacl_present != 0, dacl_present != 0 && dacl.is_null(),
            sacl_present != 0, sacl_present != 0 && sacl.is_null());
        Ok(())
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

fn observe(
    path: &Path,
    open_stage: &'static str,
    read_stage: &'static str,
) -> Result<Observation, ProbeError> {
    let file = security_file(path, 0, open_stage)?;
    let descriptor = SecurityAllocation::read(&file, read_stage)?;
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
fn security_query_distinguishes_general_sacl_and_backup() -> Result<(), ProbeError> {
    with_security_privilege(query_matrix_observations)
}

#[test]
#[ignore = "Requires separately approved GitHub-hosted Windows scratch experiment"]
fn replacement_observes_security_and_metadata_preservation() -> Result<(), ProbeError> {
    with_security_privilege(|_| preservation_observations())
}

// OpenAI-Codex_dlc_deep_audit: snapshots own bytes only; no descriptor pointers escape their owner.
#[derive(PartialEq, Eq)]
struct AclSnapshot {
    present: bool,
    defaulted: bool,
    bytes: Option<Vec<u8>>,
}

struct SecurityPartsSnapshot {
    revision: u8,
    control: u16,
    owner: Option<Vec<u8>>,
    group: Option<Vec<u8>>,
    dacl: Option<AclSnapshot>,
    sacl: Option<AclSnapshot>,
}

fn snapshot_span(bytes: &[u8], offset: usize, length: usize) -> Result<&[u8], ProbeError> {
    let end = offset.checked_add(length).ok_or(ProbeError {
        stage: "snapshot_span_overflow",
        code: None,
    })?;
    bytes.get(offset..end).ok_or(ProbeError {
        stage: "snapshot_component_bounds",
        code: None,
    })
}

fn snapshot_component(
    bytes: &[u8],
    offset: usize,
    sid: bool,
) -> Result<Option<Vec<u8>>, ProbeError> {
    if offset == 0 {
        return Ok(None);
    }
    ensure(offset >= 20 && offset & 3 == 0, "snapshot_component_offset")?;
    let header = snapshot_span(bytes, offset, 8)?;
    let length = if sid {
        ensure(header[0] == 1 && header[1] <= 15, "snapshot_sid_header")?;
        8 + usize::from(header[1]) * 4
    } else {
        let length = usize::from(u16::from_le_bytes([header[2], header[3]]));
        ensure(length >= 8, "snapshot_acl_length")?;
        length
    };
    Ok(Some(snapshot_span(bytes, offset, length)?.to_vec()))
}

impl SecurityPartsSnapshot {
    fn from_bytes(bytes: &[u8], information: u32) -> Result<Self, ProbeError> {
        let header = snapshot_span(bytes, 0, 20)?;
        let control = u16::from_le_bytes([header[2], header[3]]);
        ensure(control & SE_SELF_RELATIVE != 0, "snapshot_relative")?;
        let offset = |at: usize| {
            u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
                as usize
        };
        let all = information & BACKUP_SECURITY_INFORMATION != 0;
        let owner = if all || information & OWNER_SECURITY_INFORMATION != 0 {
            snapshot_component(bytes, offset(4), true)?
        } else {
            None
        };
        let group = if all || information & GROUP_SECURITY_INFORMATION != 0 {
            snapshot_component(bytes, offset(8), true)?
        } else {
            None
        };
        let acl = |present_mask, defaulted_mask, at| -> Result<AclSnapshot, ProbeError> {
            let present = control & present_mask != 0;
            Ok(AclSnapshot {
                present,
                defaulted: control & defaulted_mask != 0,
                // An absent ACL's offset is not meaningful. Present/null remains
                // distinct from absent because the presence bit is retained.
                bytes: if present {
                    snapshot_component(bytes, offset(at), false)?
                } else {
                    None
                },
            })
        };
        Ok(Self {
            revision: header[0],
            control,
            owner,
            group,
            dacl: if all || information & DACL_SECURITY_INFORMATION != 0 {
                Some(acl(0x0004, 0x0008, 16)?)
            } else {
                None
            },
            sacl: if all || information & SACL_SECURITY_INFORMATION != 0 {
                Some(acl(0x0010, 0x0020, 12)?)
            } else {
                None
            },
        })
    }

    fn general_eq(&self, baseline: &Self) -> bool {
        // OWNER/GROUP_DEFAULTED, DACL_PRESENT/DEFAULTED, and the DACL
        // AUTO_INHERIT_REQ/AUTO_INHERITED/PROTECTED control bits only.
        const GENERAL_CONTROL: u16 = 0x150f;
        self.revision == baseline.revision
            && self.owner.is_some()
            && self.group.is_some()
            && self.control & GENERAL_CONTROL == baseline.control & GENERAL_CONTROL
            && self.owner == baseline.owner
            && self.group == baseline.group
            && self.dacl == baseline.dacl
    }

    fn sacl_eq(&self, baseline: &Self) -> bool {
        // SACL_PRESENT/DEFAULTED/AUTO_INHERIT_REQ/AUTO_INHERITED/PROTECTED.
        const SACL_CONTROL: u16 = 0x2a30;
        self.revision == baseline.revision
            && self.control & SACL_CONTROL == baseline.control & SACL_CONTROL
            && self.sacl == baseline.sacl
    }
}

// OpenAI-Codex: caller-owned storage must never enter SecurityAllocation/LocalFree.
fn kernel_backup_query(file: &File) -> Result<SecurityPartsSnapshot, ProbeError> {
    const CAPACITY: u32 = 1_048_576;
    let mut storage = vec![0u64; CAPACITY as usize / std::mem::size_of::<u64>()];
    let pointer = storage.as_mut_ptr().cast();
    let mut needed = 0;
    println!("probe=kernel_backup api=GetKernelObjectSecurity event=before security_information=65536 capacity={CAPACITY}");
    // SAFETY: File owns the live handle; initialized, aligned storage has CAPACITY
    // writable bytes and remains alive through every descriptor inspection.
    let result = unsafe {
        GetKernelObjectSecurity(
            file.as_raw_handle(),
            BACKUP_SECURITY_INFORMATION,
            pointer,
            CAPACITY,
            &mut needed,
        )
    };
    // Capture failure immediately; success does not define the last-error value.
    let code = if result == 0 {
        Some(unsafe { GetLastError() })
    } else {
        None
    };
    println!("probe=kernel_backup api=GetKernelObjectSecurity event=after success={} os_code={code:?} needed={needed}", result != 0);
    if let Some(code) = code {
        return Err(failure(
            "kernel_backup",
            io::Error::from_raw_os_error(code as i32),
        ));
    }
    ensure((20..=CAPACITY).contains(&needed), "kernel_backup_needed")?;
    // SAFETY: successful native API supplied a self-relative descriptor in our
    // bounded buffer. This trusts the OS output contract, not external bytes.
    ensure(
        unsafe { IsValidSecurityDescriptor(pointer) } != 0,
        "kernel_backup_valid",
    )?;
    let mut control = 0;
    let mut revision = 0;
    boolean_result(
        unsafe { GetSecurityDescriptorControl(pointer, &mut control, &mut revision) },
        "kernel_backup_control",
    )?;
    ensure(control & SE_SELF_RELATIVE != 0, "kernel_backup_relative")?;
    // SAFETY: descriptor validity was checked before requesting its length.
    let length = unsafe { GetSecurityDescriptorLength(pointer) };
    ensure((20..=needed).contains(&length), "kernel_backup_length")?;
    println!("probe=kernel_backup validated=true self_relative=true length={length} full_security_proof=false");
    // SAFETY: length is bounded by needed/CAPACITY, and storage remains owned
    // here. Component offsets/lengths are checked using safe slices before copy.
    let bytes = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length as usize) };
    SecurityPartsSnapshot::from_bytes(bytes, BACKUP_SECURITY_INFORMATION)
}

fn query_matrix_observations(privilege: &mut SecurityPrivilege) -> Result<(), ProbeError> {
    let fixture = Fixture::create("security-query")?;
    // Match the earlier failing prepare-target handle's exact requested rights.
    // Do not call prepare_contrasting_metadata: this case never changes ACLs,
    // timestamps or streams, and never calls ReplaceFileW.
    let target = security_file(
        &fixture.target,
        WRITE_DAC | FILE_WRITE_ATTRIBUTES,
        "query_target_open",
    )?;
    let queries = [
        (
            "query_general",
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
        ),
        ("query_sacl", SACL_SECURITY_INFORMATION),
        ("query_backup", BACKUP_SECURITY_INFORMATION),
    ];
    let mut first_failure = None;
    let mut backup_succeeded = false;
    let mut general_baseline = None;
    let mut sacl_baseline = None;
    for (stage, information) in queries {
        // Every request uses this same File and process token; no enable/restore
        // or fixture mutation is performed between the three bounded requests.
        let context =
            privilege_attributes(&privilege.token, privilege.luid).and_then(|attributes| {
                ensure(
                    attributes & SE_PRIVILEGE_ENABLED != 0,
                    "query_matrix_privilege_enabled",
                )
            });
        if let Err(error) = context {
            println!(
                "probe=query_context_failure phase={stage} stage={} os_code={:?}",
                error.stage, error.code
            );
            return Err(first_failure.unwrap_or(error));
        }
        println!("probe=query_context phase={stage} security_privilege_enabled=true");
        let outcome =
            SecurityAllocation::query(&target, stage, information).and_then(|descriptor| {
                descriptor.report_requested_parts(information, stage)?;
                if information != BACKUP_SECURITY_INFORMATION {
                    let snapshot =
                        SecurityPartsSnapshot::from_bytes(&descriptor.bytes()?, information)?;
                    if information == SACL_SECURITY_INFORMATION {
                        sacl_baseline = Some(snapshot);
                    } else {
                        general_baseline = Some(snapshot);
                    }
                }
                Ok(())
            });
        if information == BACKUP_SECURITY_INFORMATION {
            backup_succeeded = outcome.is_ok();
        }
        match outcome {
            Ok(()) => {
                println!("probe=query_result phase={stage} success=true full_security_proof=false")
            }
            Err(error) => {
                println!("probe=query_result phase={stage} success=false stage={} os_code={:?} full_security_proof=false",
                    error.stage, error.code);
                if first_failure.is_none() {
                    first_failure = Some(error);
                }
            }
        }
    }
    // OpenAI-Codex: one additional fixed query, never a fallback or retry.
    let privilege_check =
        privilege_attributes(&privilege.token, privilege.luid).and_then(|attributes| {
            ensure(
                attributes & SE_PRIVILEGE_ENABLED != 0,
                "kernel_backup_privilege_enabled",
            )
        });
    if let Err(error) = privilege_check {
        println!(
            "probe=kernel_context_failure stage={} os_code={:?}",
            error.stage, error.code
        );
        return Err(first_failure.unwrap_or(error));
    }
    println!("probe=query_context phase=kernel_backup security_privilege_enabled=true");
    let kernel_query = kernel_backup_query(&target);
    match &kernel_query {
        Ok(_) => println!("probe=kernel_backup_result success=true full_security_proof=false"),
        Err(error) => println!("probe=kernel_backup_result success=false stage={} os_code={:?} full_security_proof=false", error.stage, error.code),
    }
    let kernel_outcome = kernel_query.and_then(|snapshot| {
        let available = general_baseline.is_some() && sacl_baseline.is_some();
        let general_equal = general_baseline.as_ref().is_some_and(|baseline| snapshot.general_eq(baseline));
        let sacl_equal = sacl_baseline.as_ref().is_some_and(|baseline| snapshot.sacl_eq(baseline));
        println!("probe=kernel_consistency baselines_available={available} general_equal={general_equal} sacl_equal={sacl_equal} full_security_proof=false");
        ensure(available, "kernel_consistency_baselines")?;
        ensure(general_equal && sacl_equal, "kernel_consistency_mismatch")?;
        println!("probe=kernel_consistency verified=true full_security_proof=false");
        Ok(())
    });
    match kernel_outcome {
        Ok(()) => (),
        Err(error) => {
            println!("probe=kernel_consistency_result success=false stage={} os_code={:?} full_security_proof=false", error.stage, error.code);
            if first_failure.is_none() {
                first_failure = Some(error);
            }
        }
    }
    println!("probe=query_matrix completed=true backup_succeeded={backup_succeeded} full_security_proof=false");
    // Nonzero statuses are collected only inside this finite diagnostic matrix.
    // Any failed request invalidates this case and stops subsequent workflow cases.
    if let Some(error) = first_failure {
        return Err(error);
    }
    ensure(backup_succeeded, "query_matrix_backup_required")?;
    println!("probe=security-query observed=true coverage=query_only full_security_proof=false");
    Ok(())
}

fn preservation_observations() -> Result<(), ProbeError> {
    let fixture = Fixture::create("preservation")?;
    let (original, replacement) = prepare_preservation_observations(&fixture)?;
    let result = fixture.replace();
    fixture.report_paths();
    result?;
    verify_preservation(&fixture, original, replacement, "preservation")
}

fn prepare_preservation_observations(
    fixture: &Fixture,
) -> Result<(Observation, Observation), ProbeError> {
    fixture.prepare_contrasting_metadata()?;
    let original = observe(
        &fixture.target,
        "pre_target_open",
        "pre_target_get_security_info",
    )?;
    let replacement = observe(
        &fixture.replacement,
        "pre_replacement_open",
        "pre_replacement_get_security_info",
    )?;
    ensure(
        original.security != replacement.security,
        "security_contrast",
    )?;
    ensure(
        original.creation_time != replacement.creation_time,
        "creation_contrast",
    )?;
    // observe() owns and closes its handles before returning these snapshots.
    Ok((original, replacement))
}

fn verify_preservation(
    fixture: &Fixture,
    original: Observation,
    replacement: Observation,
    case: &'static str,
) -> Result<(), ProbeError> {
    let restored = observe(
        &fixture.target,
        "post_target_open",
        "post_target_get_security_info",
    )?;
    let backup = observe(
        &fixture.backup,
        "post_backup_open",
        "post_backup_get_security_info",
    )?;
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
    println!("probe={case} observed=true coverage=partial full_security_proof=false");
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
    with_security_privilege(|_| denied_delete_observations())
}

#[test]
#[ignore = "Requires separately approved GitHub-hosted Windows scratch experiment"]
fn replacement_preserves_metadata_after_security_privilege_is_restored() -> Result<(), ProbeError> {
    with_security_privilege(preservation_after_restore_observations)
}

fn preservation_after_restore_observations(
    privilege: &mut SecurityPrivilege,
) -> Result<(), ProbeError> {
    // Restoring an initially enabled token would not model the unenabled case.
    // Refuse before fixture changes instead of silently changing that baseline.
    ensure(
        privilege.initial & SE_PRIVILEGE_ENABLED == 0,
        "initial_security_privilege_must_be_disabled",
    )?;
    let fixture = Fixture::create("restored-before-replace")?;
    let (original, replacement) = prepare_preservation_observations(&fixture)?;
    if let Err(error) = privilege.restore_verified() {
        RESTORE_FAILED.store(true, Ordering::SeqCst);
        return Err(error);
    }
    let enabled =
        privilege_attributes(&privilege.token, privilege.luid)? & SE_PRIVILEGE_ENABLED != 0;
    ensure(!enabled, "security_privilege_disabled_before_replace")?;
    println!("security_privilege_at_replace_enabled=false");
    // No ACCESS_SYSTEM_SECURITY observation handles remain open across replacement.
    // Only the observation phase below re-enables this existing privilege.
    let result = fixture.replace();
    fixture.report_paths();
    result?;
    let allow =
        env::var("GTAVMM_METADATA_PROBE_SECURITY_PRIVILEGE_APPROVED").as_deref() == Ok("yes");
    privilege.enable(allow)?;
    verify_preservation(&fixture, original, replacement, "restored-before-replace")
}

fn denied_delete_observations() -> Result<(), ProbeError> {
    let fixture = Fixture::create("deny-delete")?;
    // Full-security readability is required before attempting any replacement.
    let original = observe(
        &fixture.target,
        "sharing_pre_target_open",
        "sharing_pre_target_get_security_info",
    )?;
    let replacement = observe(
        &fixture.replacement,
        "sharing_pre_replacement_open",
        "sharing_pre_replacement_get_security_info",
    )?;
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
        observe(
            &fixture.target,
            "sharing_post_target_open",
            "sharing_post_target_get_security_info",
        )?
        .stable_metadata_eq(&original),
        "original_metadata_retained",
    )?;
    ensure(
        observe(
            &fixture.replacement,
            "sharing_post_replacement_open",
            "sharing_post_replacement_get_security_info",
        )?
        .stable_metadata_eq(&replacement),
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
