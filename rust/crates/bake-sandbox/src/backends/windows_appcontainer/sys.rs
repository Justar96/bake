//! The Win32 calls behind the AppContainer runner. Every call is checked; a
//! failure before the resume returns an error, and the command never runs
//! outside its AppContainer and Job.
//!
//! The Job is `bake_process::windows_job::Job`; its pumped wait and its
//! settle within a budget, and the DACL, token-query, attribute-list,
//! handle, and environment helpers, are the restricted-token runner's
//! (`windows_acl::sys`). The only `unsafe` code is the FFI calls, each with
//! its contract.

use std::ffi::{OsStr, OsString, c_void};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::OwnedHandle;
use std::os::windows::io::{AsHandle, AsRawHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    ERROR_INVALID_PARAMETER, ERROR_SHARING_VIOLATION, ERROR_SUCCESS, FILETIME, GENERIC_READ,
    GENERIC_WRITE, GetLastError, HANDLE, LocalFree, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, EXPLICIT_ACCESS_W, GetNamedSecurityInfoW, GetSecurityInfo,
    NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SET_ACCESS, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile,
    DeriveAppContainerSidFromAppContainerName, GetAppContainerFolderPath,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DeriveCapabilitySidsFromName, FreeSid, GetAce,
    NO_INHERITANCE, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_CAPABILITIES,
    SID_AND_ATTRIBUTES, SUB_CONTAINERS_AND_OBJECTS_INHERIT, TOKEN_OWNER, TOKEN_USER, TokenOwner,
    TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, DELETE, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FileDispositionInfo, GetFileInformationByHandle, READ_CONTROL,
    SetFileInformationByHandle,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, EXTENDED_STARTUPINFO_PRESENT,
    GetCurrentProcess, GetProcessTimes, OpenProcess, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_JOB_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
    PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, ResumeThread,
    STARTF_USESHOWWINDOW, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
    WaitForSingleObject,
};

use bake_process::windows_job::Job;

use super::super::windows_acl::build_command_line;
use super::super::windows_acl::sys::{
    Attributes, Local, Result, Sid, current_token, environment_block, grant_exact_checked,
    last_error, merge_and_apply, owned_handle, read_dacl, same_sid, standard_handles,
    terminate_and_drain, token_information, wait_for_exit, wide, win32_error,
};
use super::{
    Cli, FILE_ATTRIBUTE_DIRECTORY, GrantObjectRefusal, MAX_GRANT_OBJECTS, MAX_PROFILE_OBJECTS,
    PROFILE_WRITE_MASK, ProfileCreation, ProfileDeletion, ProfileTag, READ_EXECUTE,
    RUN_RECORD_DIRECTORY, RUNNER_FAILURE_EXIT, RUNNER_SIGNATURE, RunnerMode, WORKSPACE_WRITE_MASK,
    check_not_reparse_point, check_profile_outside_grants, check_records_outside_grants,
    classify_profile_creation, classify_profile_deletion, format_run_record, grant_object_refusal,
    parse_run_record, profile_root_for, read_capability_name, reclaimable_moniker, run_moniker,
    run_record_name, workspace_read_capability_name, write_capability_name,
};

/// `SE_GROUP_ENABLED`: a capability the access check consults.
const GROUP_ENABLED: u32 = 0x4;
const SW_HIDE: u16 = 0;

/// The SDDL text of `sid`, for diagnostics.
fn sid_text(sid: PSID) -> String {
    let mut text: *mut u16 = null_mut();
    // SAFETY: a valid SID; the string is LocalAlloc'd and freed below.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 || text.is_null() {
        return "<unprintable SID>".to_owned();
    }
    let owned = Local(text.cast());
    let mut length = 0;
    // SAFETY: a NUL-terminated string allocated by the call.
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: `length` units precede the NUL.
    let units = unsafe { std::slice::from_raw_parts(text, length) };
    let out = String::from_utf16_lossy(units);
    drop(owned);
    out
}

/// 32 hex digits from the OS random source.
fn random_hex() -> Result<String> {
    let rng = ring::rand::SystemRandom::new();
    let mut bytes = [0u8; 16];
    ring::rand::SecureRandom::fill(&rng, &mut bytes)
        .map_err(|_| "the OS random source failed".to_owned())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Frees a `DeriveCapabilitySidsFromName` array: each SID, then the array.
fn free_sid_array(array: *mut PSID, count: u32) {
    if array.is_null() {
        return;
    }
    for index in 0..count as usize {
        // SAFETY: `index` is below the count the API reported.
        let sid = unsafe { *array.add(index) };
        if !sid.is_null() {
            // SAFETY: each SID is LocalAlloc'd by the API and freed once.
            unsafe { LocalFree(sid) };
        }
    }
    // SAFETY: the array is LocalAlloc'd by the API and freed once.
    unsafe { LocalFree(array.cast()) };
}

/// The capability SID `DeriveCapabilitySidsFromName` derives for `name`.
fn capability_sid(name: &str) -> Result<Sid> {
    let units = wide(OsStr::new(name))?;
    let (mut groups, mut group_count): (*mut PSID, u32) = (null_mut(), 0);
    let (mut capabilities, mut capability_count): (*mut PSID, u32) = (null_mut(), 0);
    // SAFETY: a NUL-terminated name and valid out pointers.
    let ok = unsafe {
        DeriveCapabilitySidsFromName(
            units.as_ptr(),
            &mut groups,
            &mut group_count,
            &mut capabilities,
            &mut capability_count,
        )
    };
    let result = if ok == 0 {
        Err(last_error("DeriveCapabilitySidsFromName", name))
    } else if capability_count == 0 || capabilities.is_null() {
        Err(format!(
            "DeriveCapabilitySidsFromName({name}) returned no capability SID"
        ))
    } else {
        // SAFETY: at least one SID was reported.
        Sid::copy_of(unsafe { *capabilities }, name)
    };
    free_sid_array(groups, group_count);
    free_sid_array(capabilities, capability_count);
    result
}

/// The token's user and default owner: the owners a grant root may have.
fn accepted_owners() -> Result<Vec<Sid>> {
    let token = current_token()?;
    let user = token_information(&token, TokenUser, "TokenUser")?;
    // SAFETY: the buffer holds a TOKEN_USER whose SID lies in it.
    let user = Sid::copy_of(
        unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid },
        "token user",
    )?;
    let owner = token_information(&token, TokenOwner, "TokenOwner")?;
    // SAFETY: the buffer holds a TOKEN_OWNER whose SID lies in it.
    let owner = Sid::copy_of(
        unsafe { (*owner.as_ptr().cast::<TOKEN_OWNER>()).Owner },
        "token owner",
    )?;
    Ok(vec![user, owner])
}

/// Refuses a grant root whose own owner is neither the token's user nor its
/// default owner (`Administrators` on an elevated administrator token). Only
/// the root is checked: the grant's inherited ACE still reaches every
/// descendant, whatever its owner.
fn check_owner(path: &Path, accepted: &[Sid]) -> Result<()> {
    let units = wide(path.as_os_str())?;
    let mut owner: PSID = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: NUL-terminated path, valid out pointers; the owner points into
    // the descriptor, which `Local` frees.
    let status = unsafe {
        GetNamedSecurityInfoW(
            units.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(win32_error("GetNamedSecurityInfoW", status, path.display()));
    }
    let _descriptor = Local(descriptor);
    if owner.is_null() || !accepted.iter().any(|sid| same_sid(owner, sid)) {
        return Err(format!(
            "{} is owned by {}, not by this user; refusing to change its DACL",
            path.display(),
            if owner.is_null() {
                "nobody".to_owned()
            } else {
                sid_text(owner)
            }
        ));
    }
    Ok(())
}

/// One object's attributes, link count, and whether its owner is one of
/// `accepted`, all read through one handle opened without following a
/// reparse point and with no access beyond attributes and its descriptor.
fn inspect_object(path: &Path, accepted: &[Sid]) -> Result<(u32, u32, bool)> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .access_mode(FILE_READ_ATTRIBUTES | READ_CONTROL)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    let handle = file.as_raw_handle();
    // SAFETY: a zeroed BY_HANDLE_FILE_INFORMATION is valid output space.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a live handle opened with FILE_READ_ATTRIBUTES.
    if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
        return Err(last_error("GetFileInformationByHandle", path.display()));
    }
    let mut owner: PSID = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: a live handle opened with READ_CONTROL and valid out
    // pointers; the owner points into the descriptor, which `Local` frees.
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(win32_error("GetSecurityInfo", status, path.display()));
    }
    let _descriptor = Local(descriptor);
    let owner_accepted = !owner.is_null() && accepted.iter().any(|sid| same_sid(owner, sid));
    Ok((info.dwFileAttributes, info.nNumberOfLinks, owner_accepted))
}

/// The walk before a grant propagates through `root`: every object under
/// it, the root included and at most [`MAX_GRANT_OBJECTS`], must pass
/// [`grant_object_refusal`]. A reparse point is refused, never followed,
/// and an object that cannot be inspected or listed refuses the grant too.
fn check_grant_tree(root: &Path, accepted: &[Sid]) -> Result<()> {
    let mut pending = vec![root.to_path_buf()];
    let mut seen = 0usize;
    while let Some(path) = pending.pop() {
        seen += 1;
        if seen > MAX_GRANT_OBJECTS {
            return Err(format!(
                "{} holds more than {MAX_GRANT_OBJECTS} objects; refusing to propagate a grant through it",
                root.display()
            ));
        }
        let (attributes, links, owner_accepted) = inspect_object(&path, accepted)?;
        if let Some(refusal) = grant_object_refusal(attributes, links, owner_accepted) {
            let what = match refusal {
                GrantObjectRefusal::ReparsePoint => "is a reparse point".to_owned(),
                GrantObjectRefusal::HardLink { links } => {
                    format!("is a hard link with {links} names")
                }
                GrantObjectRefusal::ForeignOwner => "is owned by another principal".to_owned(),
            };
            return Err(format!(
                "{} {what}; refusing to propagate a grant through {}",
                path.display(),
                root.display()
            ));
        }
        if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            let listing = std::fs::read_dir(&path)
                .map_err(|error| format!("cannot list {}: {error}", path.display()))?;
            for entry in listing {
                let entry =
                    entry.map_err(|error| format!("cannot list {}: {error}", path.display()))?;
                pending.push(entry.path());
            }
        }
    }
    Ok(())
}

/// [`grant_exact_checked`] with the [`check_grant_tree`] walk: an exact
/// standing ACE is kept without a walk, and a missing one is added only
/// when the walk passes.
fn grant_checked(root: &Path, sid: &Sid, mask: u32, accepted: &[Sid]) -> Result<()> {
    grant_exact_checked(root, sid, mask, || check_grant_tree(root, accepted))
}

fn utf8(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| format!("{} is not valid Unicode", path.display()))
}

/// A NUL-terminated wide string returned by a Win32 call and freed with
/// `CoTaskMemFree`, copied out and freed once.
fn take_co_task_string(text: *mut u16) -> OsString {
    let mut length = 0;
    // SAFETY: a NUL-terminated string returned by the call.
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: `length` units precede the NUL.
    let out = OsString::from_wide(unsafe { std::slice::from_raw_parts(text, length) });
    // SAFETY: allocated by the call, freed once.
    unsafe { CoTaskMemFree(text.cast()) };
    out
}

/// The run's own AppContainer profile. It exists only after a
/// `CreateAppContainerProfile` this run made succeeded, and only it grants
/// the authority to delete that profile; [`OwnedProfile::delete`] is the one
/// way it ends besides being left in place on purpose.
struct OwnedProfile {
    moniker: String,
    sid: Sid,
    /// `Packages\<moniker>`, once resolved and checked.
    root: Option<PathBuf>,
}

impl OwnedProfile {
    /// Creates profile `moniker` exclusively. An existing profile of that
    /// name, or any other failure, is an error and owns nothing.
    fn create(moniker: &str) -> Result<Self> {
        let name = wide(OsStr::new(moniker))?;
        let display = wide(OsStr::new("Bake sandbox run"))?;
        let mut sid: PSID = null_mut();
        // SAFETY: NUL-terminated strings, no capabilities, a valid out
        // pointer.
        let status = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                display.as_ptr(),
                display.as_ptr(),
                null(),
                0,
                &mut sid,
            )
        };
        match classify_profile_creation(status) {
            ProfileCreation::Created => {}
            ProfileCreation::AlreadyExists => {
                return Err(format!(
                    "AppContainer profile {moniker} already exists; refusing to adopt or delete it"
                ));
            }
            ProfileCreation::Failed => {
                return Err(format!(
                    "CreateAppContainerProfile failed ({moniker}): HRESULT {status:#010x}"
                ));
            }
        }
        // From here the profile is this run's: it is either handed back as an
        // owner, whose cleanup deletes it, or deleted here.
        let copied = if sid.is_null() {
            Err("CreateAppContainerProfile returned no SID".to_owned())
        } else {
            let copy = Sid::copy_of(sid, "package SID");
            // SAFETY: documented as freed with `FreeSid`, and freed once.
            unsafe { FreeSid(sid) };
            copy
        };
        match copied {
            Ok(sid) => Ok(Self {
                moniker: moniker.to_owned(),
                sid,
                root: None,
            }),
            Err(error) => {
                // No SID means no run; delete what was created right away.
                let delete = delete_profile(moniker);
                Err(match delete {
                    Ok(()) => error,
                    Err(delete) => format!("{error}; then {delete}"),
                })
            }
        }
    }

    /// Resolves and checks the profile root from `GetAppContainerFolderPath`
    /// and records it for deletion's check.
    fn resolve_root(&mut self) -> Result<PathBuf> {
        let folder = folder_path(&self.sid)?;
        let root = profile_root_for(&folder, &self.moniker).ok_or_else(|| {
            format!(
                "the profile folder {} is not under Packages\\{}; refusing to use it",
                folder.display(),
                self.moniker
            )
        })?;
        // Recorded first, so deletion checks this root whatever fails next.
        self.root = Some(root.clone());
        check_not_reparse_point(&root)?;
        if !root.is_dir() {
            return Err(format!(
                "the profile root {} is not a directory",
                root.display()
            ));
        }
        Ok(root)
    }

    /// Deletes the profile and checks that its root is gone. Returns every
    /// failure.
    fn delete(self) -> Vec<String> {
        let mut failures = Vec::new();
        if let Err(error) = delete_profile(&self.moniker) {
            failures.push(error);
        }
        if let Some(root) = &self.root {
            match root.try_exists() {
                Ok(false) => {}
                Ok(true) => failures.push(format!(
                    "the profile storage {} remains after DeleteAppContainerProfile",
                    root.display()
                )),
                Err(error) => failures.push(format!(
                    "cannot inspect the profile storage {}: {error}",
                    root.display()
                )),
            }
        }
        failures
    }
}

/// The `LOCALAPPDATA` folder `GetAppContainerFolderPath` reports for the
/// profile of package `sid`.
fn folder_path(sid: &Sid) -> Result<PathBuf> {
    let text = sid_text(sid.as_psid());
    let units = wide(OsStr::new(&text))?;
    let mut path: *mut u16 = null_mut();
    // SAFETY: a NUL-terminated SID string and a valid out pointer; the
    // returned string is freed by `take_co_task_string`.
    let status = unsafe { GetAppContainerFolderPath(units.as_ptr(), &mut path) };
    let folder = (!path.is_null()).then(|| PathBuf::from(take_co_task_string(path)));
    folder.filter(|_| status >= 0).ok_or_else(|| {
        format!("GetAppContainerFolderPath failed for {text}: HRESULT {status:#010x}")
    })
}

fn delete_profile(moniker: &str) -> Result<()> {
    let name = wide(OsStr::new(moniker))?;
    // SAFETY: a NUL-terminated name of a profile this run created.
    let status = unsafe { DeleteAppContainerProfile(name.as_ptr()) };
    if status < 0 {
        return Err(format!(
            "DeleteAppContainerProfile failed ({moniker}): HRESULT {status:#010x}"
        ));
    }
    Ok(())
}

/// [`RUN_RECORD_DIRECTORY`] under this runner's temporary directory.
fn record_directory() -> PathBuf {
    std::env::temp_dir().join(RUN_RECORD_DIRECTORY)
}

/// This run's record in [`RUN_RECORD_DIRECTORY`]: created exclusively
/// before the profile, holding this runner's pid and creation time, and
/// kept open without sharing for the runner's life, so a later run sees a
/// sharing violation while this runner lives. It is deleted through its own
/// handle after the profile is gone; a runner that exits first, killed or
/// with an unconfirmed tree, leaves it for a later run to reclaim
/// ([`reclaim_exited_runs`]).
struct RunRecord {
    file: std::fs::File,
    path: PathBuf,
}

impl RunRecord {
    fn create(moniker: &str) -> Result<Self> {
        let directory = record_directory();
        std::fs::create_dir_all(&directory).map_err(|error| {
            format!(
                "cannot create the run record directory {}: {error}",
                directory.display()
            )
        })?;
        let path = directory.join(run_record_name(moniker));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE)
            .share_mode(0)
            .open(&path)
            .map_err(|error| format!("cannot create the run record {}: {error}", path.display()))?;
        let written = process_created(current_process())
            .ok_or_else(|| last_error("GetProcessTimes", "this runner"))
            .and_then(|created| {
                let text = format_run_record(std::process::id(), created);
                std::io::Write::write_all(&mut &file, text.as_bytes()).map_err(|error| {
                    format!("cannot write the run record {}: {error}", path.display())
                })
            });
        let record = Self { file, path };
        match written {
            Ok(()) => Ok(record),
            Err(detail) => Err(match record.remove() {
                Ok(()) => detail,
                Err(remove) => format!("{detail}; then {remove}"),
            }),
        }
    }

    /// Deletes the record through its own handle, then closes it.
    fn remove(self) -> Result<()> {
        delete_on_close(&self.file, &self.path)
    }
}

/// Marks the file open as `file` for deletion when its last handle closes;
/// `file` must have been opened with `DELETE`.
fn delete_on_close(file: &std::fs::File, path: &Path) -> Result<()> {
    let info = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: a live handle opened with DELETE and a valid info block of
    // the size passed.
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            std::ptr::from_ref(&info).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(last_error("SetFileInformationByHandle", path.display()));
    }
    Ok(())
}

/// A pseudo-handle to this process, which needs no closing.
fn current_process() -> HANDLE {
    // SAFETY: plain call returning a pseudo-handle.
    unsafe { GetCurrentProcess() }
}

/// The creation time of the process `handle` names, as one integer.
fn process_created(handle: HANDLE) -> Option<u64> {
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: a process handle with query access and valid out pointers.
    let ok = unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) };
    (ok != 0).then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

/// Whether the runner a record names has exited: no process has its pid,
/// the process with that pid was created at another time, or that process
/// is signalled, which it is only after its handle table, its Job handle
/// and record handle included, is closed. Anything that cannot be decided
/// is not an exit.
fn runner_exited(pid: u32, created: u64) -> bool {
    // SAFETY: plain call; null means it failed.
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if raw.is_null() {
        // SAFETY: plain call.
        return unsafe { GetLastError() } == ERROR_INVALID_PARAMETER;
    }
    let process = owned_handle(raw);
    let handle = process.as_raw_handle();
    match process_created(handle) {
        Some(time) if time != created => true,
        // SAFETY: a live handle opened with SYNCHRONIZE.
        Some(_) => (unsafe { WaitForSingleObject(handle, 0) }) == WAIT_OBJECT_0,
        None => false,
    }
}

/// The most record entries one run reads, and the most runs it reclaims.
const MAX_RECORDS_SCANNED: usize = 1024;
const MAX_RECLAIMS: usize = 8;

/// Reclaims the profiles of runs in this run's namespace
/// ([`reclaimable_moniker`]) whose runner has exited, killed ones included:
/// each record not held open by a live runner and naming a runner that
/// [`runner_exited`] is opened exclusively, its profile deleted, its root
/// checked gone, and only then the record deleted. A record that is held,
/// unparsable, or names a runner that may live is left alone, and so is a
/// profile without a record. Returns each failure, whose record stays for a
/// later run; residue grants a later run nothing, because every run holds a
/// fresh package SID.
fn reclaim_exited_runs(tag: Option<&ProfileTag>) -> Vec<String> {
    let directory = record_directory();
    let listing = match std::fs::read_dir(&directory) {
        Ok(listing) => listing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            return vec![format!(
                "cannot list the run records in {}: {error}",
                directory.display()
            )];
        }
    };
    let (mut failures, mut reclaimed) = (Vec::new(), 0usize);
    for entry in listing.take(MAX_RECORDS_SCANNED) {
        if reclaimed == MAX_RECLAIMS {
            break;
        }
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let Some(moniker) = name
            .to_str()
            .and_then(|name| reclaimable_moniker(name, tag))
        else {
            continue;
        };
        match reclaim_run(&entry.path(), moniker) {
            Ok(true) => reclaimed += 1,
            Ok(false) => {}
            Err(error) => failures.push(error),
        }
    }
    failures
}

/// Reclaims run `moniker` from its record at `path`; `false` when the
/// record is left alone.
fn reclaim_run(path: &Path, moniker: &str) -> Result<bool> {
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .access_mode(GENERIC_READ | DELETE)
        .share_mode(0)
        .open(path);
    let file = match opened {
        Ok(file) => file,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || error.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32) =>
        {
            return Ok(false);
        }
        Err(error) => {
            return Err(format!(
                "cannot open the run record {}: {error}",
                path.display()
            ));
        }
    };
    let mut text = String::new();
    if std::io::Read::read_to_string(&mut std::io::Read::take(&file, 64), &mut text).is_err() {
        return Ok(false);
    }
    let Some((pid, created)) = parse_run_record(&text) else {
        return Ok(false);
    };
    if !runner_exited(pid, created) {
        return Ok(false);
    }
    reclaim_profile(moniker)?;
    delete_on_close(&file, path)?;
    Ok(true)
}

/// Deletes the profile of an exited run and checks that its root, when
/// Windows still names it, is gone. An absent profile is already reclaimed.
fn reclaim_profile(moniker: &str) -> Result<()> {
    let name = wide(OsStr::new(moniker))?;
    let mut sid: PSID = null_mut();
    // SAFETY: a NUL-terminated name and a valid out pointer; the SID is
    // freed with `FreeSid` below.
    let derived = unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) };
    let root = if derived >= 0 && !sid.is_null() {
        let copy = Sid::copy_of(sid, "package SID");
        // SAFETY: allocated by the call, freed once.
        unsafe { FreeSid(sid) };
        copy.ok()
            .and_then(|sid| folder_path(&sid).ok())
            .and_then(|folder| profile_root_for(&folder, moniker))
    } else {
        None
    };
    // SAFETY: a NUL-terminated profile name in this run's namespace.
    let status = unsafe { DeleteAppContainerProfile(name.as_ptr()) };
    if classify_profile_deletion(status) == ProfileDeletion::Failed {
        return Err(format!(
            "cannot reclaim the AppContainer profile {moniker} of an exited runner: HRESULT {status:#010x}"
        ));
    }
    match root.map(|root| (root.try_exists(), root)) {
        None | Some((Ok(false), _)) => Ok(()),
        Some((Ok(true), root)) => Err(format!(
            "the reclaimed profile storage {} remains after DeleteAppContainerProfile",
            root.display()
        )),
        Some((Err(error), root)) => Err(format!(
            "cannot inspect the reclaimed profile storage {}: {error}",
            root.display()
        )),
    }
}

/// `ACCESS_ALLOWED_ACE_TYPE` and `ACCESS_ALLOWED_CALLBACK_ACE_TYPE`: the
/// allow ACEs whose layout starts with a mask and an inline SID.
const ALLOW_ACE_TYPES: [u8; 2] = [0, 9];
/// `GENERIC_ALL | GENERIC_WRITE`: what an inherit-only ACE may still hold
/// unmapped for the objects that inherit it.
const GENERIC_WRITE_RIGHTS: u32 = 0x1000_0000 | 0x4000_0000;

/// Whether an allow ACE in `dacl` for `sid`, explicit or inherited,
/// critical or not, carries any right of [`PROFILE_WRITE_MASK`] or a
/// generic write right.
fn package_may_write(dacl: *mut ACL, sid: &Sid) -> Result<bool> {
    if dacl.is_null() {
        // A null DACL grants everyone everything.
        return Ok(true);
    }
    // SAFETY: `dacl` is a valid ACL owned by a live descriptor.
    let count = unsafe { (*dacl).AceCount };
    for index in 0..u32::from(count) {
        let mut ace: *mut c_void = null_mut();
        // SAFETY: `index` is below the ACL's ACE count.
        if unsafe { GetAce(dacl, index, &mut ace) } == 0 || ace.is_null() {
            return Err(last_error("GetAce", index));
        }
        // SAFETY: every ACE starts with an ACE_HEADER.
        let header = unsafe { *ace.cast::<ACE_HEADER>() };
        if !ALLOW_ACE_TYPES.contains(&header.AceType)
            || usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>()
        {
            continue;
        }
        let body = ace.cast::<ACCESS_ALLOWED_ACE>();
        // SAFETY: an allow ACE of at least this size holds a mask and an
        // inline SID at `SidStart`.
        let (mask, ace_sid) = unsafe {
            (
                (*body).Mask,
                std::ptr::addr_of_mut!((*body).SidStart).cast::<c_void>(),
            )
        };
        if mask & (PROFILE_WRITE_MASK | GENERIC_WRITE_RIGHTS) != 0 && same_sid(ace_sid, sid) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Replaces every explicit ACE for `package` on `path` with one
/// [`READ_EXECUTE`] allow ACE, inheritable (OI|CI) on a directory, unless no
/// allow ACE for it can write there. `SET_ACCESS` drops the profile's own
/// critical full-control ACE too; the inherited ones below a rewritten
/// directory are recomputed from it as the DACL is applied. Returns whether
/// it wrote. Profile paths are this run's alone, so no cross-run lock is
/// taken.
fn restrict_profile_object(path: &Path, package: &Sid, directory: bool) -> Result<bool> {
    let (_descriptor, dacl) = read_dacl(path)?;
    if !package_may_write(dacl, package)? {
        return Ok(false);
    }
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: READ_EXECUTE,
        grfAccessMode: SET_ACCESS,
        grfInheritance: if directory {
            SUB_CONTAINERS_AND_OBJECTS_INHERIT
        } else {
            NO_INHERITANCE
        },
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: package.as_psid().cast(),
        },
    };
    merge_and_apply(path, &entry, dacl, "profile read-only")?;
    let (_descriptor, dacl) = read_dacl(path)?;
    if package_may_write(dacl, package)? {
        return Err(format!(
            "the package can still write {} after its ACEs were replaced",
            path.display()
        ));
    }
    Ok(true)
}

/// The `read-only` pass: on every object under `root`, the root included,
/// the package keeps read and execute access only. A reparse point, an
/// unreadable entry, or more than [`MAX_PROFILE_OBJECTS`] objects fails the
/// pass, and the run with it. Returns how many objects it wrote.
fn restrict_profile_tree(root: &Path, package: &Sid) -> Result<usize> {
    let mut pending = vec![root.to_path_buf()];
    let (mut seen, mut written) = (0usize, 0usize);
    while let Some(path) = pending.pop() {
        seen += 1;
        if seen > MAX_PROFILE_OBJECTS {
            return Err(format!(
                "the profile {} holds more than {MAX_PROFILE_OBJECTS} objects",
                root.display()
            ));
        }
        check_not_reparse_point(&path)?;
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
        let directory = meta.is_dir();
        if restrict_profile_object(&path, package, directory)? {
            written += 1;
        }
        if directory {
            let listing = std::fs::read_dir(&path)
                .map_err(|error| format!("cannot list {}: {error}", path.display()))?;
            for entry in listing {
                let entry =
                    entry.map_err(|error| format!("cannot list {}: {error}", path.display()))?;
                pending.push(entry.path());
            }
        }
    }
    Ok(written)
}

pub(super) fn run(cli: &Cli) -> i32 {
    let mut profile = None;
    let mut record = None;
    let mut settled = true;
    let outcome = run_inner(cli, &mut profile, &mut record, &mut settled);
    // The record outlives a profile that remains, so a later run in this
    // namespace reclaims that profile once this runner has exited.
    let mut keep_record = false;
    match profile {
        Some(profile) if settled => {
            let failures = profile.delete();
            keep_record = !failures.is_empty();
            for failure in failures {
                eprintln!("{RUNNER_SIGNATURE}: cleanup: {failure}");
            }
        }
        // A process that may still run must not lose its storage. The
        // profile, its restricted DACLs, and the grants stay; the Job's last handle
        // closes with this process, which (kill-on-close) ends any member.
        Some(profile) => {
            keep_record = true;
            eprintln!(
                "{RUNNER_SIGNATURE}: cleanup: AppContainer profile {} ({}) is left in place: the confined tree's exit was not confirmed",
                profile.moniker,
                profile.root.as_deref().map_or_else(
                    || "root unresolved".to_owned(),
                    |root| root.display().to_string()
                )
            );
        }
        None => {}
    }
    if let Some(record) = record.filter(|_| !keep_record)
        && let Err(failure) = record.remove()
    {
        eprintln!("{RUNNER_SIGNATURE}: cleanup: {failure}");
    }
    match outcome {
        Ok(code) => code as i32,
        Err(detail) => super::fail(&detail),
    }
}

fn run_inner(
    cli: &Cli,
    profile: &mut Option<OwnedProfile>,
    record: &mut Option<RunRecord>,
    settled: &mut bool,
) -> Result<u32> {
    // SAFETY: plain call with no handler routine.
    if unsafe { SetConsoleCtrlHandler(None, 1) } == 0 {
        return Err(last_error("SetConsoleCtrlHandler", "ignore Ctrl+C"));
    }
    let owners = accepted_owners()?;
    let workspace = utf8(&cli.workspace)?;
    // Every owner check before the first change, so a refused root leaves
    // every DACL untouched and no profile behind.
    check_owner(&cli.workspace, &owners)?;
    for root in cli.grants.read_roots() {
        check_owner(root, &owners)?;
    }
    check_records_outside_grants(&record_directory(), &cli.workspace, cli.grants.read_roots())?;
    // Best effort: residue a killed runner left grants this run nothing.
    for failure in reclaim_exited_runs(cli.tag.as_ref()) {
        eprintln!("{RUNNER_SIGNATURE}: cleanup: {failure}");
    }
    let moniker = run_moniker(cli.tag.as_ref(), &random_hex()?);
    record.replace(RunRecord::create(&moniker)?);
    let owned = profile.insert(OwnedProfile::create(&moniker)?);
    let root = owned.resolve_root()?;
    check_profile_outside_grants(&root, &cli.workspace, cli.grants.read_roots())?;
    // Each grant that would propagate first walks its root
    // (`check_grant_tree`): a hard link, a reparse point, or a foreign
    // owner under it refuses the run before that DACL changes.
    let read = capability_sid(&workspace_read_capability_name(workspace))?;
    grant_checked(&cli.workspace, &read, READ_EXECUTE, &owners)?;
    let mut capabilities: Vec<Sid> = vec![read];
    for root in cli.grants.read_roots() {
        let sid = capability_sid(&read_capability_name(utf8(root)?))?;
        grant_checked(root, &sid, READ_EXECUTE, &owners)?;
        capabilities.push(sid);
    }
    let read_only = cli.mode == RunnerMode::ReadOnly;
    if read_only {
        restrict_profile_tree(&root, &owned.sid)?;
    } else {
        let write = capability_sid(&write_capability_name(workspace))?;
        grant_checked(&cli.workspace, &write, WORKSPACE_WRITE_MASK, &owners)?;
        capabilities.push(write);
    }
    for grant in cli.grants.network() {
        let sid = grant
            .capability_sid()
            .ok_or_else(|| format!("network grant {grant} has no capability"))?;
        capabilities.push(Sid::parse(sid)?);
    }
    let mut attributes: Vec<SID_AND_ATTRIBUTES> = capabilities
        .iter()
        .map(|sid| SID_AND_ATTRIBUTES {
            Sid: sid.as_psid(),
            Attributes: GROUP_ENABLED,
        })
        .collect();
    let temp = root.join("AC").join("Temp");
    let environment = environment_block(Some(&temp))?;
    let package = &owned.sid;
    // Objects process creation added under the profile are restricted
    // before the command's first instruction.
    let before_resume = || {
        if read_only {
            restrict_profile_tree(&root, package).map(drop)
        } else {
            Ok(())
        }
    };
    // Created, with its completion port, before the command: a failure here
    // starts nothing, so the profile is deleted.
    let job = Job::new().map_err(|error| {
        format!("cannot create the kill-on-close Job and its completion port: {error}")
    })?;
    // From here a member may exist: the profile is deleted only when the
    // Job's account settles, whatever ended the command.
    *settled = false;
    let outcome = spawn(
        package,
        &mut attributes,
        &job,
        &cli.command,
        &environment,
        &before_resume,
    )
    .and_then(|process| wait_for_exit(&job, &process));
    // Every path, a failed start or resume included, terminates the Job and
    // must prove each member it ever had ended.
    let drained = terminate_and_drain(&job);
    *settled = drained.is_ok();
    match (outcome, drained) {
        (Ok(code), Ok(())) => Ok(code),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(drain)) => Err(format!("{error}; then {drain}")),
    }
}

/// Starts `argv` as the AppContainer `package` with `capabilities`,
/// suspended and born inside `job`, runs `before_resume`, then resumes it.
/// A failure after creation terminates the suspended child.
fn spawn(
    package: &Sid,
    capabilities: &mut [SID_AND_ATTRIBUTES],
    job: &Job,
    argv: &[OsString],
    environment: &[u16],
    before_resume: &dyn Fn() -> Result<()>,
) -> Result<OwnedHandle> {
    let units: Vec<Vec<u16>> = argv.iter().map(|arg| arg.encode_wide().collect()).collect();
    if units.iter().any(|arg| arg.contains(&0)) {
        return Err("the command contains a NUL character".to_owned());
    }
    let mut command_line = build_command_line(&units);
    command_line.push(0);
    let application = match argv.first() {
        Some(program) if Path::new(program).is_absolute() => Some(wide(program)?),
        _ => None,
    };
    let std_handles = standard_handles()?;
    let mut inherited: Vec<HANDLE> = Vec::new();
    for handle in std_handles {
        if !handle.is_null() && !inherited.contains(&handle) {
            inherited.push(handle);
        }
    }
    let security = SECURITY_CAPABILITIES {
        AppContainerSid: package.as_psid(),
        Capabilities: if capabilities.is_empty() {
            null_mut()
        } else {
            capabilities.as_mut_ptr()
        },
        CapabilityCount: u32::try_from(capabilities.len())
            .map_err(|_| "too many capabilities".to_owned())?,
        Reserved: 0,
    };
    let job_list: [HANDLE; 1] = [job.as_handle().as_raw_handle()];
    let mut attributes = Attributes::new(if inherited.is_empty() { 2 } else { 3 })?;
    attributes.set(
        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
        std::ptr::from_ref(&security).cast::<c_void>(),
        size_of::<SECURITY_CAPABILITIES>(),
        "security capabilities",
    )?;
    attributes.set(
        PROC_THREAD_ATTRIBUTE_JOB_LIST,
        job_list.as_ptr().cast(),
        size_of_val(&job_list),
        "job list",
    )?;
    if !inherited.is_empty() {
        attributes.set(
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
            inherited.as_ptr().cast(),
            inherited.len() * size_of::<HANDLE>(),
            "handle list",
        )?;
    }
    // SAFETY: a zeroed STARTUPINFOEXW is valid before its fields are set.
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
    startup.StartupInfo.wShowWindow = SW_HIDE;
    startup.StartupInfo.hStdInput = std_handles[0];
    startup.StartupInfo.hStdOutput = std_handles[1];
    startup.StartupInfo.hStdError = std_handles[2];
    startup.lpAttributeList = attributes.pointer();
    // SAFETY: a zeroed PROCESS_INFORMATION is valid output space.
    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: every pointer is valid and NUL-terminated where required;
    // `command_line` is mutable; the attribute values (`security`, the
    // capability SIDs, `job_list`, `inherited`) outlive the call; the
    // environment block is double-NUL-terminated.
    let created = unsafe {
        CreateProcessW(
            application.as_ref().map_or(null(), |name| name.as_ptr()),
            command_line.as_mut_ptr(),
            null(),
            null(),
            i32::from(!inherited.is_empty()),
            CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr().cast(),
            null(),
            &startup.StartupInfo,
            &mut info,
        )
    };
    if created == 0 {
        return Err(last_error(
            "CreateProcessW",
            format_args!("AppContainer command: {}", argv[0].display()),
        ));
    }
    drop(attributes);
    if info.hProcess.is_null() || info.hThread.is_null() {
        return Err("CreateProcessW returned a null process or thread handle".to_owned());
    }
    let process = owned_handle(info.hProcess);
    let thread = owned_handle(info.hThread);
    let resumed = before_resume().and_then(|()| {
        // SAFETY: a live, suspended thread handle.
        if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
            Err(last_error(
                "ResumeThread",
                format_args!("pid {}", info.dwProcessId),
            ))
        } else {
            Ok(())
        }
    });
    if let Err(detail) = resumed {
        // SAFETY: a live process handle. It stays a Job member the settle
        // must see end; the Job's termination also ends it.
        unsafe {
            TerminateProcess(process.as_raw_handle(), RUNNER_FAILURE_EXIT as u32);
        }
        return Err(detail);
    }
    Ok(process)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `DeriveCapabilitySidsFromName` is a pure derivation with no host
    /// state, so it runs here; profile creation, which writes per-user
    /// state, is left to the enforcement tests.
    #[test]
    fn capability_sids_are_stable_distinct_app_authority_sids() {
        let text = |name: &str| sid_text(capability_sid(name).expect(name).as_psid());
        let read = text(&read_capability_name(r"C:\Tools\a"));
        assert!(read.starts_with("S-1-15-3-1024-"), "{read}");
        assert_eq!(read, text(&read_capability_name(r"C:\Tools\a")));
        assert_ne!(read, text(&read_capability_name(r"C:\Tools\b")));
        assert_ne!(read, text(&write_capability_name(r"C:\Tools\a")));
    }

    #[test]
    fn the_token_user_is_an_accepted_owner() {
        let owners = accepted_owners().expect("this process's token");
        assert_eq!(owners.len(), 2);
        // A directory this test creates is owned by one of them.
        let dir = std::env::temp_dir().join(format!("bake-ac-owner-{}", std::process::id()));
        std::fs::create_dir(&dir).expect("fresh directory");
        let result = check_owner(&dir, &owners);
        let _ = std::fs::remove_dir(&dir);
        result.expect("a directory this user created");
        // A system-owned directory is refused.
        let system = PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"));
        let error = check_owner(&system, &owners).expect_err("the system root");
        assert!(error.contains("not by this user"), "{error}");
    }
}
