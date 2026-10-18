//! The Win32 calls behind the runner. Every call is checked; a failure
//! before the command starts returns an error, and the command is never
//! started without its restricted token and Job.
//!
//! The only `unsafe` code is the FFI calls, each with its contract.

use std::ffi::{OsStr, OsString, c_void};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::time::Duration;

use bake_process::windows_job::Job;

use windows_sys::Win32::Foundation::{
    ERROR_SUCCESS, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, LocalFree,
    SetHandleInformation,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSidToSidW, EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW,
    NO_MULTIPLE_TRUSTEE, REVOKE_ACCESS, SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW,
    TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, CopySid, CreateRestrictedToken, CreateWellKnownSid,
    DACL_SECURITY_INFORMATION, DISABLE_MAX_PRIVILEGE, EqualSid, GetAce, GetLengthSid,
    GetTokenInformation, IsValidSid, LUA_TOKEN, PSECURITY_DESCRIPTOR, PSID, SID_AND_ATTRIBUTES,
    SUB_CONTAINERS_AND_OBJECTS_INHERIT, SetTokenInformation, TOKEN_ADJUST_DEFAULT,
    TOKEN_ASSIGN_PRIMARY, TOKEN_DEFAULT_DACL, TOKEN_DUPLICATE, TOKEN_GROUPS, TOKEN_QUERY,
    TokenDefaultDacl, TokenGroups, WRITE_RESTRICTED, WinWorldSid,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
    GetExitCodeProcess, InitializeProcThreadAttributeList, OpenProcessToken,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION,
    ResumeThread, STARTF_USESHOWWINDOW, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
    UpdateProcThreadAttribute,
};

use super::{
    Cli, GRANT_MASK, PRIVATE_TEMP_PREFIX, RUNNER_FAILURE_EXIT, RUNNER_SIGNATURE, RunnerMode,
    build_command_line, temp_write_sid, workspace_write_sid,
};

/// How long the runner waits for its terminated Job's account to settle
/// before it reports a cleanup failure.
const DRAIN_BUDGET: Duration = Duration::from_secs(30);

const SW_HIDE: u16 = 0;
/// `SE_GROUP_LOGON_ID`.
const LOGON_ID: u32 = 0xC000_0000;
const SECURITY_MAX_SID_SIZE: usize = 68;
const FILE_ALL_ACCESS: u32 = 0x001F_01FF;
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;

type Result<T> = std::result::Result<T, String>;

fn last_error(api: &str, context: impl std::fmt::Display) -> String {
    let error = std::io::Error::last_os_error();
    format!("{api} failed ({context}): {error}")
}

fn win32_error(api: &str, code: u32, context: impl std::fmt::Display) -> String {
    let error = std::io::Error::from_raw_os_error(code as i32);
    format!("{api} failed ({context}): {error}")
}

fn wide(text: &OsStr) -> Result<Vec<u16>> {
    let mut units: Vec<u16> = text.encode_wide().collect();
    if units.contains(&0) {
        return Err(format!("{} contains a NUL character", text.display()));
    }
    units.push(0);
    Ok(units)
}

/// A `LocalAlloc`ed block, freed on drop.
struct Local(*mut c_void);

impl Drop for Local {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from a Win32 allocator documented as
            // freed with `LocalFree`, and is freed once.
            unsafe {
                LocalFree(self.0);
            }
        }
    }
}

/// A SID: one `ConvertStringSidToSidW` allocated, or a copy owned here.
enum Sid {
    Local(Local),
    Owned(Vec<u32>),
}

impl Sid {
    fn parse(text: &str) -> Result<Self> {
        let units = wide(OsStr::new(text))?;
        let mut sid: PSID = null_mut();
        // SAFETY: `units` is NUL-terminated; `sid` receives a LocalAlloc'd SID.
        if unsafe { ConvertStringSidToSidW(units.as_ptr(), &mut sid) } == 0 || sid.is_null() {
            return Err(last_error("ConvertStringSidToSidW", text));
        }
        Ok(Sid::Local(Local(sid)))
    }

    /// A copy of the SID at `sid`, which must stay valid for the call.
    fn copy_of(sid: PSID, context: &str) -> Result<Self> {
        // SAFETY: the caller passes a valid SID pointer.
        let length = unsafe { GetLengthSid(sid) };
        if length == 0 {
            return Err(last_error("GetLengthSid", context));
        }
        let mut buffer = vec![0u32; (length as usize).div_ceil(4)];
        // SAFETY: `buffer` holds at least `length` bytes, 4-byte aligned.
        if unsafe { CopySid(length, buffer.as_mut_ptr().cast(), sid) } == 0 {
            return Err(last_error("CopySid", context));
        }
        Ok(Sid::Owned(buffer))
    }

    fn everyone() -> Result<Self> {
        let mut buffer = vec![0u32; SECURITY_MAX_SID_SIZE / 4];
        let mut size = SECURITY_MAX_SID_SIZE as u32;
        // SAFETY: `buffer` holds `size` bytes; no domain SID is needed.
        if unsafe {
            CreateWellKnownSid(
                WinWorldSid,
                null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        } == 0
        {
            return Err(last_error("CreateWellKnownSid", "Everyone"));
        }
        // SAFETY: the buffer now holds a SID.
        if unsafe { IsValidSid(buffer.as_mut_ptr().cast()) } == 0 {
            return Err(last_error("IsValidSid", "Everyone"));
        }
        Ok(Sid::Owned(buffer))
    }

    fn as_psid(&self) -> PSID {
        match self {
            Sid::Local(local) => local.0,
            Sid::Owned(buffer) => buffer.as_ptr().cast_mut().cast(),
        }
    }
}

fn owned_handle(raw: HANDLE) -> OwnedHandle {
    // SAFETY: callers pass a valid handle they exclusively own.
    unsafe { OwnedHandle::from_raw_handle(raw) }
}

// ---------------------------------------------------------------------------
// DACL grants (`acl.ts`).

/// An `EXPLICIT_ACCESS_W` naming `sid`, inheritable by containers and
/// objects. The SID must outlive the entry's use.
fn explicit_access(sid: &Sid, mode: i32, permissions: u32) -> EXPLICIT_ACCESS_W {
    EXPLICIT_ACCESS_W {
        grfAccessPermissions: permissions,
        grfAccessMode: mode,
        grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid.as_psid().cast(),
        },
    }
}

/// `path`'s DACL (null for a NULL DACL) and the descriptor that owns it.
fn read_dacl(path: &Path) -> Result<(Local, *mut ACL)> {
    let units = wide(path.as_os_str())?;
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `units` is NUL-terminated; the out pointers are valid; the
    // DACL points into the descriptor, which `Local` frees.
    let status = unsafe {
        GetNamedSecurityInfoW(
            units.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(win32_error("GetNamedSecurityInfoW", status, path.display()));
    }
    Ok((Local(descriptor), dacl))
}

/// Each allow ACE of `dacl`: its flags, mask, and SID pointer.
fn allow_aces(dacl: *mut ACL) -> Result<Vec<(u8, u32, PSID)>> {
    let mut aces = Vec::new();
    if dacl.is_null() {
        return Ok(aces);
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
        if header.AceType != ACCESS_ALLOWED_ACE_TYPE
            || usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>()
        {
            continue;
        }
        let allowed = ace.cast::<ACCESS_ALLOWED_ACE>();
        // SAFETY: an allow ACE of at least this size holds a mask and an
        // inline SID starting at `SidStart`.
        let (mask, sid) = unsafe {
            (
                (*allowed).Mask,
                std::ptr::addr_of_mut!((*allowed).SidStart).cast::<c_void>(),
            )
        };
        aces.push((header.AceFlags, mask, sid));
    }
    Ok(aces)
}

fn same_sid(left: PSID, right: &Sid) -> bool {
    // SAFETY: both pointers are valid SIDs.
    unsafe { EqualSid(left, right.as_psid()) != 0 }
}

/// Runs `action` holding the exclusive per-path lock (`withPathLock`): one
/// byte locked in `<temp>\bake-acl-locks\<sha256(lowercase path)[..16]>.lock`,
/// opened without delete sharing so the file cannot be replaced under a
/// holder.
fn with_path_lock<T>(path: &Path, action: impl FnOnce() -> Result<T>) -> Result<T> {
    let key = path
        .to_str()
        .ok_or_else(|| format!("{} is not valid Unicode", path.display()))?
        .to_lowercase();
    let digest = super::sha256(&[key.as_bytes()]);
    let name: String = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let directory = std::env::temp_dir().join("bake-acl-locks");
    std::fs::create_dir_all(&directory).map_err(|error| {
        format!(
            "cannot create the ACL lock directory {}: {error}",
            directory.display()
        )
    })?;
    let lock_path = directory.join(format!("{name}.lock"));
    const FILE_SHARE_READ: u32 = 1;
    const FILE_SHARE_WRITE: u32 = 2;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(&lock_path)
        .map_err(|error| format!("cannot open the ACL lock {}: {error}", lock_path.display()))?;
    file.lock()
        .map_err(|error| format!("cannot lock {}: {error}", lock_path.display()))?;
    let result = action();
    // Closing the handle releases the lock.
    drop(file);
    result
}

/// Merges `entry` into `dacl` and applies the result to `path`.
fn merge_and_apply(
    path: &Path,
    entry: &EXPLICIT_ACCESS_W,
    dacl: *mut ACL,
    label: &str,
) -> Result<()> {
    let mut merged: *mut ACL = null_mut();
    // SAFETY: `entry` is one valid entry; `dacl` is null or a valid ACL.
    let status = unsafe { SetEntriesInAclW(1, entry, dacl, &mut merged) };
    if status != ERROR_SUCCESS || merged.is_null() {
        return Err(win32_error(
            "SetEntriesInAclW",
            status,
            format_args!("{label} {}", path.display()),
        ));
    }
    let merged = Local(merged.cast());
    let units = wide(path.as_os_str())?;
    // SAFETY: `units` is NUL-terminated and `merged` a valid ACL; only the
    // DACL is set, which propagates to inheriting descendants.
    let status = unsafe {
        SetNamedSecurityInfoW(
            units.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            merged.0.cast(),
            null(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(win32_error(
            "SetNamedSecurityInfoW",
            status,
            format_args!("{label} {}", path.display()),
        ));
    }
    Ok(())
}

/// `grantWrite`: adds the `sid` write ACE to `path`, unless the exact ACE
/// (explicit, OI|CI, [`GRANT_MASK`]) already stands there, in which case the
/// full-tree propagation is skipped.
fn grant_write(path: &Path, sid: &Sid) -> Result<()> {
    with_path_lock(path, || {
        let (_descriptor, dacl) = read_dacl(path)?;
        let exact = allow_aces(dacl)?.into_iter().any(|(flags, mask, ace_sid)| {
            u32::from(flags) == SUB_CONTAINERS_AND_OBJECTS_INHERIT
                && mask == GRANT_MASK
                && same_sid(ace_sid, sid)
        });
        if exact {
            return Ok(());
        }
        merge_and_apply(
            path,
            &explicit_access(sid, GRANT_ACCESS, GRANT_MASK),
            dacl,
            "grant",
        )
    })
}

/// `revokeWrite`: removes every ACE naming `sid` from `path`'s DACL.
fn revoke_write(path: &Path, sid: &Sid) -> Result<()> {
    with_path_lock(path, || {
        let (_descriptor, dacl) = read_dacl(path)?;
        if dacl.is_null() {
            return Ok(());
        }
        merge_and_apply(
            path,
            &explicit_access(sid, REVOKE_ACCESS, 0),
            dacl,
            "revoke",
        )
    })
}

pub(super) fn allow_aces_naming(path: &Path, sid: &str) -> Result<usize> {
    let sid = Sid::parse(sid)?;
    let (_descriptor, dacl) = read_dacl(path)?;
    Ok(allow_aces(dacl)?
        .into_iter()
        .filter(|(_, _, ace_sid)| same_sid(*ace_sid, &sid))
        .count())
}

// ---------------------------------------------------------------------------
// The restricted token (`token.ts`).

/// A token information block, 8-byte aligned.
fn token_information(token: &OwnedHandle, class: i32, label: &str) -> Result<Vec<u64>> {
    let mut needed = 0u32;
    // SAFETY: a size query with no buffer; it fails with
    // ERROR_INSUFFICIENT_BUFFER and reports the size.
    unsafe {
        GetTokenInformation(token.as_raw_handle(), class, null_mut(), 0, &mut needed);
    }
    if needed == 0 {
        return Err(last_error(
            "GetTokenInformation",
            format_args!("{label} size"),
        ));
    }
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
    let length = u32::try_from(buffer.len() * 8).map_err(|_| format!("{label} is too large"))?;
    // SAFETY: `buffer` holds `length` bytes.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            class,
            buffer.as_mut_ptr().cast(),
            length,
            &mut needed,
        )
    } == 0
    {
        return Err(last_error("GetTokenInformation", label));
    }
    Ok(buffer)
}

fn current_token() -> Result<OwnedHandle> {
    let mut token: HANDLE = null_mut();
    // SAFETY: the pseudo-handle needs no closing; `token` receives a handle.
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ADJUST_DEFAULT | TOKEN_ASSIGN_PRIMARY,
            &mut token,
        )
    } == 0
        || token.is_null()
    {
        return Err(last_error("OpenProcessToken", "current process"));
    }
    Ok(owned_handle(token))
}

/// `findLogonSid`: a copy of the token's logon-session SID.
fn logon_sid(token: &OwnedHandle) -> Result<Sid> {
    let buffer = token_information(token, TokenGroups, "TokenGroups")?;
    let base = buffer.as_ptr().cast::<TOKEN_GROUPS>();
    // SAFETY: the buffer holds a TOKEN_GROUPS header.
    let count = unsafe { (*base).GroupCount } as usize;
    let first = std::mem::offset_of!(TOKEN_GROUPS, Groups);
    if first + count * size_of::<SID_AND_ATTRIBUTES>() > buffer.len() * 8 {
        return Err(format!("implausible TokenGroups count {count}"));
    }
    // SAFETY: bounds checked above; the groups follow the header.
    let groups = unsafe {
        std::slice::from_raw_parts(
            buffer
                .as_ptr()
                .cast::<u8>()
                .add(first)
                .cast::<SID_AND_ATTRIBUTES>(),
            count,
        )
    };
    for group in groups {
        if group.Attributes & LOGON_ID == LOGON_ID && !group.Sid.is_null() {
            return Sid::copy_of(group.Sid, "logon SID");
        }
    }
    Err(format!("no logon SID found among {count} token groups"))
}

/// `createRestrictedToken` and `setTokenDefaultDaclGrant`: the
/// write-restricted token with `capabilities` after the logon SID and
/// Everyone, whose default DACL also grants `default_owner` full access.
fn restricted_token(
    capabilities: &[&Sid],
    default_owner: &Sid,
    everyone: &Sid,
) -> Result<OwnedHandle> {
    let current = current_token()?;
    let logon = logon_sid(&current)?;
    let mut restricting: Vec<SID_AND_ATTRIBUTES> = [&logon, everyone]
        .into_iter()
        .chain(capabilities.iter().copied())
        .map(|sid| SID_AND_ATTRIBUTES {
            Sid: sid.as_psid(),
            Attributes: 0,
        })
        .collect();
    let mut token: HANDLE = null_mut();
    // SAFETY: `restricting` holds valid SIDs that outlive the call.
    if unsafe {
        CreateRestrictedToken(
            current.as_raw_handle(),
            DISABLE_MAX_PRIVILEGE | LUA_TOKEN | WRITE_RESTRICTED,
            0,
            null(),
            0,
            null(),
            restricting.len() as u32,
            restricting.as_mut_ptr(),
            &mut token,
        )
    } == 0
        || token.is_null()
    {
        return Err(last_error(
            "CreateRestrictedToken",
            format_args!("{} restricting SIDs", restricting.len()),
        ));
    }
    let token = owned_handle(token);
    let buffer = token_information(&token, TokenDefaultDacl, "TokenDefaultDacl")?;
    // SAFETY: the buffer holds a TOKEN_DEFAULT_DACL whose ACL lies in it.
    let current_dacl = unsafe { (*buffer.as_ptr().cast::<TOKEN_DEFAULT_DACL>()).DefaultDacl };
    if current_dacl.is_null() {
        return Err("the token carries no default DACL to extend".to_owned());
    }
    let entry = explicit_access(default_owner, GRANT_ACCESS, FILE_ALL_ACCESS);
    let mut merged: *mut ACL = null_mut();
    // SAFETY: one valid entry and a valid ACL.
    let status = unsafe { SetEntriesInAclW(1, &entry, current_dacl, &mut merged) };
    if status != ERROR_SUCCESS || merged.is_null() {
        return Err(win32_error("SetEntriesInAclW", status, "default DACL"));
    }
    let merged = Local(merged.cast());
    let info = TOKEN_DEFAULT_DACL {
        DefaultDacl: merged.0.cast(),
    };
    // SAFETY: `info` points at a valid ACL; the call copies it.
    if unsafe {
        SetTokenInformation(
            token.as_raw_handle(),
            TokenDefaultDacl,
            std::ptr::addr_of!(info).cast(),
            size_of::<TOKEN_DEFAULT_DACL>() as u32,
        )
    } == 0
    {
        return Err(last_error("SetTokenInformation", "TokenDefaultDacl"));
    }
    Ok(token)
}

// ---------------------------------------------------------------------------
// The Job: `bake_process::windows_job::Job`, kill-on-close with its
// completion port associated before any process exists, and settled only on
// its account, never on the active count alone.

/// Terminates the confined tree and waits, within [`DRAIN_BUDGET`], until
/// the Job's account proves every member ended: each process the Job
/// reported is held by handle until signalled. An error means that proof is
/// missing, not that a process survives; the caller then keeps the run's
/// grants and directory.
fn terminate_and_drain(job: &Job) -> Result<()> {
    job.terminate_and_settle(DRAIN_BUDGET).map_err(|verdict| {
        format!(
            "the confined tree's exit was not confirmed within {} s of termination ({verdict})",
            DRAIN_BUDGET.as_secs()
        )
    })
}

// ---------------------------------------------------------------------------
// The confined process (`spawnInheritedJobProcess`).

/// The runner's standard handles, made inheritable: the only handles the
/// command inherits. Missing handles stay null.
fn standard_handles() -> Result<[HANDLE; 3]> {
    let mut handles = [null_mut(); 3];
    for (slot, (selector, label)) in handles.iter_mut().zip([
        (STD_INPUT_HANDLE, "stdin"),
        (STD_OUTPUT_HANDLE, "stdout"),
        (STD_ERROR_HANDLE, "stderr"),
    ]) {
        // SAFETY: plain call.
        let handle = unsafe { GetStdHandle(selector) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            continue;
        }
        // SAFETY: `handle` is this process's live standard handle.
        if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0 {
            return Err(last_error("SetHandleInformation", label));
        }
        *slot = handle;
    }
    Ok(handles)
}

/// An initialized `PROC_THREAD_ATTRIBUTE_LIST`, deleted on drop.
struct Attributes(Vec<u64>);

impl Attributes {
    fn new(count: u32) -> Result<Self> {
        let mut size = 0usize;
        // SAFETY: a size query; it fails and reports the size.
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), count, 0, &mut size);
        }
        if size == 0 {
            return Err(last_error("InitializeProcThreadAttributeList", "size"));
        }
        let mut buffer = vec![0u64; size.div_ceil(8)];
        // SAFETY: `buffer` holds `size` bytes.
        if unsafe {
            InitializeProcThreadAttributeList(buffer.as_mut_ptr().cast(), count, 0, &mut size)
        } == 0
        {
            return Err(last_error("InitializeProcThreadAttributeList", count));
        }
        Ok(Self(buffer))
    }

    fn pointer(&mut self) -> *mut c_void {
        self.0.as_mut_ptr().cast()
    }

    /// Sets `attribute` to the `size` bytes at `value`, which must outlive
    /// the process creation.
    fn set(
        &mut self,
        attribute: u32,
        value: *const c_void,
        size: usize,
        label: &str,
    ) -> Result<()> {
        // SAFETY: the list is initialized; the caller keeps `value` alive.
        if unsafe {
            UpdateProcThreadAttribute(
                self.pointer(),
                0,
                attribute as usize,
                value,
                size,
                null_mut(),
                null(),
            )
        } == 0
        {
            return Err(last_error("UpdateProcThreadAttribute", label));
        }
        Ok(())
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: the list was initialized and is deleted once.
        unsafe { DeleteProcThreadAttributeList(self.pointer()) }
    }
}

/// The environment block: this process's variables with `TMP` and `TEMP`
/// replaced by `temp` when given, sorted case-insensitively.
fn environment_block(temp: Option<&Path>) -> Result<Vec<u16>> {
    let mut variables: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(key, _)| {
            temp.is_none() || !(key.eq_ignore_ascii_case("TMP") || key.eq_ignore_ascii_case("TEMP"))
        })
        .collect();
    if let Some(temp) = temp {
        variables.push(("TEMP".into(), temp.as_os_str().to_owned()));
        variables.push(("TMP".into(), temp.as_os_str().to_owned()));
    }
    let upper = |key: &OsStr| -> Vec<u16> {
        key.encode_wide()
            .map(|unit| {
                if (0x61..=0x7a).contains(&unit) {
                    unit - 0x20
                } else {
                    unit
                }
            })
            .collect()
    };
    variables.sort_by_cached_key(|(key, _)| upper(key));
    let mut block = Vec::new();
    for (key, value) in &variables {
        let entry: Vec<u16> = key
            .encode_wide()
            .chain([u16::from(b'=')])
            .chain(value.encode_wide())
            .collect();
        if entry.contains(&0) {
            return Err(format!(
                "environment variable {} contains a NUL character",
                key.display()
            ));
        }
        block.extend(entry);
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

/// Starts `argv` with `token`, suspended and born inside `job`, then
/// resumes it. A failure after creation terminates the suspended child,
/// which stays a Job member the drain must see end.
fn spawn(
    token: &OwnedHandle,
    job: &Job,
    argv: &[OsString],
    environment: &[u16],
) -> Result<OwnedHandle> {
    let units: Vec<Vec<u16>> = argv.iter().map(|arg| arg.encode_wide().collect()).collect();
    if units.iter().any(|arg| arg.contains(&0)) {
        return Err("the command contains a NUL character".to_owned());
    }
    let mut command_line = build_command_line(&units);
    command_line.push(0);
    // An absolute program is passed as the application name, so the
    // command-line search never reaches the current directory.
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
    // Borrowed for the creation only; the Job keeps its handle.
    let job_list: [HANDLE; 1] = [job.as_handle().as_raw_handle()];
    let mut attributes = Attributes::new(if inherited.is_empty() { 1 } else { 2 })?;
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
    // The console stays shared: CREATE_NO_WINDOW and CREATE_NEW_CONSOLE
    // children fail DLL initialization under the restricted token.
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
    startup.StartupInfo.wShowWindow = SW_HIDE;
    startup.StartupInfo.hStdInput = std_handles[0];
    startup.StartupInfo.hStdOutput = std_handles[1];
    startup.StartupInfo.hStdError = std_handles[2];
    startup.lpAttributeList = attributes.pointer();
    // SAFETY: a zeroed PROCESS_INFORMATION is valid output space.
    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: every pointer is valid and NUL-terminated where required;
    // `command_line` is mutable as the API requires; the attribute values
    // outlive the call; the environment block is double-NUL-terminated.
    let created = unsafe {
        CreateProcessAsUserW(
            token.as_raw_handle(),
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
            "CreateProcessAsUserW",
            format_args!("command: {}", argv[0].display()),
        ));
    }
    drop(attributes);
    if info.hProcess.is_null() || info.hThread.is_null() {
        return Err("CreateProcessAsUserW returned a null process or thread handle".to_owned());
    }
    let process = owned_handle(info.hProcess);
    let thread = owned_handle(info.hThread);
    // SAFETY: a live, suspended thread handle.
    if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
        let detail = last_error("ResumeThread", format_args!("pid {}", info.dwProcessId));
        // SAFETY: a live process handle; the Job's drop also ends it.
        unsafe {
            TerminateProcess(process.as_raw_handle(), RUNNER_FAILURE_EXIT as u32);
        }
        return Err(detail);
    }
    Ok(process)
}

/// Waits for the command's exit while the Job's account is pumped, so its
/// members are opened soon after they start and the port's queue does not
/// grow with the command's life (`Job::wait_pumping`).
fn wait_for_exit(job: &Job, process: &OwnedHandle) -> Result<u32> {
    job.wait_pumping(process.as_handle())
        .map_err(|error| format!("WaitForSingleObject failed (command): {error}"))?;
    let mut code = 0u32;
    // SAFETY: a live process handle and a valid out pointer.
    if unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) } == 0 {
        return Err(last_error("GetExitCodeProcess", "command"));
    }
    Ok(code)
}

// ---------------------------------------------------------------------------
// One run (`runner.ts` main).

/// The run's private temp directory and its capability.
struct PrivateTemp {
    path: PathBuf,
    sid: Sid,
}

impl PrivateTemp {
    fn create(root: &Path) -> Result<Self> {
        let rng = ring::rand::SystemRandom::new();
        let mut bytes = [0u8; 16];
        ring::rand::SecureRandom::fill(&rng, &mut bytes)
            .map_err(|_| "the OS random source failed".to_owned())?;
        let name: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = root.join(format!("{PRIVATE_TEMP_PREFIX}{name}"));
        std::fs::create_dir(&path).map_err(|error| {
            format!(
                "cannot create the private temp directory {}: {error}",
                path.display()
            )
        })?;
        let sid = match path.to_str() {
            Some(text) => Sid::parse(&temp_write_sid(text)),
            None => Err(format!("{} is not valid Unicode", path.display())),
        };
        match sid {
            Ok(sid) => Ok(Self { path, sid }),
            Err(error) => {
                let _ = std::fs::remove_dir(&path);
                Err(error)
            }
        }
    }

    /// Revokes the temp grant and removes the directory, which holds no
    /// links the removal could follow (`remove_dir_all` does not traverse
    /// junctions or symbolic links). Returns every failure.
    fn remove(self) -> Vec<String> {
        let mut failures = Vec::new();
        if let Err(error) = revoke_write(&self.path, &self.sid) {
            failures.push(error);
        }
        if let Err(error) = std::fs::remove_dir_all(&self.path) {
            failures.push(format!("cannot remove {}: {error}", self.path.display()));
        }
        failures
    }
}

pub(super) fn run(cli: &Cli) -> i32 {
    let mut temp = None;
    let mut settled = true;
    let outcome = run_inner(cli, &mut temp, &mut settled);
    match temp {
        Some(temp) if settled => {
            for failure in temp.remove() {
                eprintln!("{RUNNER_SIGNATURE}: cleanup: {failure}");
            }
        }
        // A process that may still run must not lose its directory or its
        // grant underneath it. The residue is inert once the tree is gone:
        // the Job's last handle closes with this process and kills it.
        Some(temp) => eprintln!(
            "{RUNNER_SIGNATURE}: cleanup: {} and its temp grant are left in place: the confined tree's exit was not confirmed",
            temp.path.display()
        ),
        None => {}
    }
    match outcome {
        // The full 32-bit Windows exit code passes through.
        Ok(code) => code as i32,
        Err(detail) => super::fail(&detail),
    }
}

/// One run. `settled` turns false when a process may still run: the Job
/// existed and its drain could not prove it empty.
fn run_inner(cli: &Cli, temp: &mut Option<PrivateTemp>, settled: &mut bool) -> Result<u32> {
    // Ignore Ctrl+C so the runner survives to clean up (TypeScript's
    // `setConsoleCtrlHandler(null, 1)`).
    // SAFETY: plain call with no handler routine.
    if unsafe { SetConsoleCtrlHandler(None, 1) } == 0 {
        return Err(last_error("SetConsoleCtrlHandler", "ignore Ctrl+C"));
    }
    let everyone = Sid::everyone()?;
    let workspace_sid = match cli.mode {
        RunnerMode::ReadOnly => None,
        RunnerMode::WorkspaceWrite => {
            let text = cli
                .workspace
                .to_str()
                .ok_or_else(|| format!("{} is not valid Unicode", cli.workspace.display()))?;
            let sid = Sid::parse(&workspace_write_sid(text))?;
            // Standing: the ACE is the reuse cache and is never revoked.
            grant_write(&cli.workspace, &sid)?;
            let private = temp.insert(PrivateTemp::create(&cli.temp)?);
            grant_write(&private.path, &private.sid)?;
            Some(sid)
        }
    };
    let private = temp.as_ref();
    let capabilities: Vec<&Sid> = workspace_sid
        .iter()
        .chain(private.map(|p| &p.sid))
        .collect();
    let default_owner = private.map(|p| &p.sid).unwrap_or(&everyone);
    let token = restricted_token(&capabilities, default_owner, &everyone)?;
    let environment = environment_block(private.map(|p| p.path.as_path()))?;
    // Created, with its port, before the command: a failure here starts
    // nothing.
    let job = Job::new().map_err(|error| {
        format!("cannot create the kill-on-close Job and its completion port: {error}")
    })?;
    let outcome = spawn(&token, &job, &cli.command, &environment)
        .and_then(|process| wait_for_exit(&job, &process));
    // The command's exit (or a failure around it) ends the run: whatever it
    // left behind is terminated, and the Job's account must prove every
    // member ended before cleanup.
    let drained = terminate_and_drain(&job);
    *settled = drained.is_ok();
    match (outcome, drained) {
        (Ok(code), Ok(())) => Ok(code),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(drain)) => Err(format!("{error}; then {drain}")),
    }
}
