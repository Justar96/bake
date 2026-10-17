//! The Win32 calls behind the AppContainer runner. Every call is checked; a
//! failure before the resume returns an error, and the command never runs
//! outside its AppContainer and Job.
//!
//! The DACL, token-query, Job, attribute-list, handle, and environment
//! helpers are the restricted-token runner's (`windows_acl::sys`). The only
//! `unsafe` code is the FFI calls, each with its contract.

use std::ffi::{OsStr, OsString, c_void};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::OwnedHandle;
use std::os::windows::io::{AsHandle, AsRawHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetNamedSecurityInfoW, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{
    DeriveCapabilitySidsFromName, FreeSid, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES, TOKEN_OWNER, TOKEN_USER, TokenOwner, TokenUser,
};
use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, EXTENDED_STARTUPINFO_PRESENT,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ResumeThread,
    STARTF_USESHOWWINDOW, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
};

use bake_process::windows_job::Job;

use super::super::windows_acl::build_command_line;
use super::super::windows_acl::sys::{
    Attributes, Local, Result, Sid, current_token, environment_block, grant_exact, last_error,
    owned_handle, revoke_write, same_sid, standard_handles, terminate_and_drain, token_information,
    wait_for_exit, wide, win32_error,
};
use super::{
    Cli, PRIVATE_TEMP_PREFIX, READ_EXECUTE, RUNNER_FAILURE_EXIT, RUNNER_SIGNATURE, RunnerMode,
    WORKSPACE_WRITE_MASK, profile_name, read_capability_name, write_capability_name,
};

/// `HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)`, which
/// `CreateAppContainerProfile` returns for an existing profile.
const PROFILE_EXISTS: i32 = 0x8007_00B7_u32 as i32;
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

/// The workspace's package SID: its profile created, or derived when the
/// profile already exists (Microsoft: `CreateAppContainerProfile` returns
/// `HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)`).
fn package_sid(name: &str) -> Result<Sid> {
    let units = wide(OsStr::new(name))?;
    let display = wide(OsStr::new("Bake sandbox workspace"))?;
    let mut sid: PSID = null_mut();
    // SAFETY: NUL-terminated strings, no capabilities, a valid out pointer.
    let status = unsafe {
        CreateAppContainerProfile(
            units.as_ptr(),
            display.as_ptr(),
            display.as_ptr(),
            null(),
            0,
            &mut sid,
        )
    };
    if status == PROFILE_EXISTS {
        sid = null_mut();
        // SAFETY: a NUL-terminated name and a valid out pointer.
        let status = unsafe { DeriveAppContainerSidFromAppContainerName(units.as_ptr(), &mut sid) };
        if status < 0 {
            return Err(format!(
                "DeriveAppContainerSidFromAppContainerName failed ({name}): HRESULT {status:#010x}"
            ));
        }
    } else if status < 0 {
        return Err(format!(
            "CreateAppContainerProfile failed ({name}): HRESULT {status:#010x}"
        ));
    }
    if sid.is_null() {
        return Err(format!("the AppContainer profile {name} returned no SID"));
    }
    let copy = Sid::copy_of(sid, "package SID");
    // SAFETY: allocated by the profile API, documented as freed with
    // `FreeSid`, and freed once.
    unsafe { FreeSid(sid) };
    copy
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

fn utf8(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| format!("{} is not valid Unicode", path.display()))
}

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
        // The capability first: a failure leaves no directory behind.
        let sid = capability_sid(&format!("bake.temp.{name}"))?;
        let path = root.join(format!("{PRIVATE_TEMP_PREFIX}{name}"));
        std::fs::create_dir(&path).map_err(|error| {
            format!(
                "cannot create the private temp directory {}: {error}",
                path.display()
            )
        })?;
        Ok(Self { path, sid })
    }

    /// Revokes the temp grant and removes the directory, which holds no
    /// links the removal could follow. Returns every failure.
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
        // A process that may still run must not lose its directory or grant.
        Some(temp) => eprintln!(
            "{RUNNER_SIGNATURE}: cleanup: {} and its temp grant are left in place: the confined tree's exit was not confirmed",
            temp.path.display()
        ),
        None => {}
    }
    match outcome {
        Ok(code) => code as i32,
        Err(detail) => super::fail(&detail),
    }
}

fn run_inner(cli: &Cli, temp: &mut Option<PrivateTemp>, settled: &mut bool) -> Result<u32> {
    // SAFETY: plain call with no handler routine.
    if unsafe { SetConsoleCtrlHandler(None, 1) } == 0 {
        return Err(last_error("SetConsoleCtrlHandler", "ignore Ctrl+C"));
    }
    let owners = accepted_owners()?;
    let workspace = utf8(&cli.workspace)?;
    // Every owner check before the first grant, so a refused root leaves
    // every DACL untouched.
    check_owner(&cli.workspace, &owners)?;
    for root in cli.grants.read_roots() {
        check_owner(root, &owners)?;
    }
    let package = package_sid(&profile_name(workspace))?;
    grant_exact(&cli.workspace, &package, READ_EXECUTE)?;
    let mut capabilities: Vec<Sid> = Vec::new();
    for root in cli.grants.read_roots() {
        let sid = capability_sid(&read_capability_name(utf8(root)?))?;
        grant_exact(root, &sid, READ_EXECUTE)?;
        capabilities.push(sid);
    }
    if cli.mode == RunnerMode::WorkspaceWrite {
        let write = capability_sid(&write_capability_name(workspace))?;
        grant_exact(&cli.workspace, &write, WORKSPACE_WRITE_MASK)?;
        capabilities.push(write);
        let private = temp.insert(PrivateTemp::create(&cli.temp)?);
        grant_exact(&private.path, &private.sid, WORKSPACE_WRITE_MASK)?;
    }
    for grant in cli.grants.network() {
        let sid = grant
            .capability_sid()
            .ok_or_else(|| format!("network grant {grant} has no capability"))?;
        capabilities.push(Sid::parse(sid)?);
    }
    let private = temp.as_ref();
    let mut attributes: Vec<SID_AND_ATTRIBUTES> = capabilities
        .iter()
        .chain(private.map(|p| &p.sid))
        .map(|sid| SID_AND_ATTRIBUTES {
            Sid: sid.as_psid(),
            Attributes: GROUP_ENABLED,
        })
        .collect();
    let environment = environment_block(private.map(|p| p.path.as_path()))?;
    let job = Job::new().map_err(|error| {
        format!("cannot create the kill-on-close Job and its completion port: {error}")
    })?;
    let outcome = spawn(&package, &mut attributes, &job, &cli.command, &environment)
        .and_then(|process| wait_for_exit(&job, &process));
    let drained = terminate_and_drain(&job);
    *settled = drained.is_ok();
    match (outcome, drained) {
        (Ok(code), Ok(())) => Ok(code),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(drain)) => Err(format!("{error}; then {drain}")),
    }
}

/// Starts `argv` as the AppContainer `package` with `capabilities`,
/// suspended and born inside `job`, then resumes it. A failure after
/// creation terminates the suspended child.
fn spawn(
    package: &Sid,
    capabilities: &mut [SID_AND_ATTRIBUTES],
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
    // SAFETY: a live, suspended thread handle.
    if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
        let detail = last_error("ResumeThread", format_args!("pid {}", info.dwProcessId));
        // SAFETY: a live process handle; the Job's drain also ends it.
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
