//! Windows spawning for [`super::OwnedProcess`]: `CreateProcessW` with the
//! new process born inside a new job (`PROC_THREAD_ATTRIBUTE_JOB_LIST`) and
//! inheriting only its three standard handles
//! (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`). Tokio's and std's `Command` cannot
//! ask for a job list on stable Rust, so this narrow path replaces them for
//! owned processes. The attribute-list use follows Bake's
//! `bake-sandbox` Windows runner (`backends/windows_acl/sys.rs`), and the
//! command line is its quoting ([`crate::command_line`]).
//!
//! If the job, its kill-on-close limit, a pipe, or the process cannot be
//! created, nothing runs and the error is returned.
//!
//! No thread or blocking-pool work is started. Standard handles are named
//! pipes with a random name: the parent holds each pipe's server end as a
//! Tokio overlapped [`NamedPipeServer`], so a read or write is cancelled
//! when its future or the pipe is dropped, even while a descendant that
//! left the job still holds the child's end; the child gets a synchronous
//! client end. The leader's exit is observed by polling its handle with a
//! zero timeout, which is cancel safe.
//!
//! The only `unsafe` code is the FFI calls, each with its contract.

use std::ffi::{OsStr, OsString, c_void};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::ptr::{null, null_mut};
use std::time::Duration;

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{
    HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::JobObjects::{CreateJobObjectW, TerminateJobObject};
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, InitializeProcThreadAttributeList,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject,
};

use super::{ProcessSpec, ProcessStderr, ProcessStdin, ProcessStdout};

/// Pi's `windowsHide: true`.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A Windows leader: its process handle, which keeps its pid from being
/// reused while it is held.
pub(crate) struct Leader {
    process: OwnedHandle,
    pid: u32,
}

impl Leader {
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    /// Never: a Windows process is named by the handle held here.
    #[allow(clippy::unused_self, reason = "the Unix leader can lose its identity")]
    pub(super) fn lost(&self) -> bool {
        false
    }

    /// Nothing to check: the handle held here keeps the identity.
    #[allow(clippy::unused_self, reason = "the Unix leader can lose its identity")]
    pub(super) fn check_identity(&mut self) {}

    /// Asks the system to end the leader. The result is not read as a
    /// fact about the process: whether it exited is only ever learned from
    /// [`Self::wait_exit`].
    pub(super) fn start_kill(&mut self) {
        // SAFETY: a live process handle.
        unsafe {
            TerminateProcess(self.process.as_raw_handle(), 1);
        }
    }

    /// Completes once the leader has exited, polling its handle with a
    /// backoff from 1 ms to 20 ms. The handle stays open. Cancel safe: it
    /// holds nothing between polls.
    pub(super) async fn wait_exit(&mut self) -> io::Result<()> {
        let mut pause = Duration::from_millis(1);
        loop {
            // SAFETY: a live process handle; a zero timeout only checks.
            match unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } {
                WAIT_OBJECT_0 => return Ok(()),
                WAIT_TIMEOUT => {}
                WAIT_FAILED => return Err(io::Error::last_os_error()),
                other => {
                    return Err(io::Error::other(format!(
                        "WaitForSingleObject returned {other:#x}"
                    )));
                }
            }
            tokio::time::sleep(pause).await;
            pause = (pause * 2).min(Duration::from_millis(20));
        }
    }

    /// The leader's exit status, once it has exited.
    pub(super) async fn reap(&mut self) -> io::Result<ExitStatus> {
        self.wait_exit().await?;
        let mut code = 0u32;
        // SAFETY: a live process handle and a valid out pointer.
        if unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ExitStatus::from_raw(code))
    }
}

impl Drop for Leader {
    fn drop(&mut self) {
        // Owners kill before they drop, and the job kill reaches it too;
        // this only asks again before the handle closes.
        self.start_kill();
    }
}

/// A process just created in its job.
pub(super) struct Spawned {
    pub(super) leader: Leader,
    pub(super) job: OwnedHandle,
    /// The job's completion port, associated before the leader existed.
    pub(super) port: OwnedHandle,
    pub(super) stdin: Option<ProcessStdin>,
    pub(super) stdout: Option<ProcessStdout>,
    pub(super) stderr: Option<ProcessStderr>,
}

fn last_error() -> io::Error {
    io::Error::last_os_error()
}

fn wide(text: &OsStr) -> io::Result<Vec<u16>> {
    let mut units: Vec<u16> = text.encode_wide().collect();
    if units.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "nul byte found in provided data",
        ));
    }
    units.push(0);
    Ok(units)
}

/// Which way a standard pipe carries data, seen from the child.
#[derive(Clone, Copy)]
enum Direction {
    /// The child reads (standard input).
    ToChild,
    /// The child writes (standard output and error).
    FromChild,
}

/// A connected named pipe: the parent's overlapped server end and the
/// child's synchronous, not yet inheritable, client end. The name is random
/// and the pipe takes one local client, which this function connects at
/// once; were another process to connect first, the open fails and nothing
/// is spawned.
async fn pipe(direction: Direction) -> io::Result<(NamedPipeServer, OwnedHandle)> {
    let mut random = [0u8; 16];
    getrandom::getrandom(&mut random).map_err(|_| io::Error::other("no random source"))?;
    let id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let name = format!(r"\\.\pipe\bake-process-{}-{id}", std::process::id());
    let server = ServerOptions::new()
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .max_instances(1)
        .access_inbound(matches!(direction, Direction::FromChild))
        .access_outbound(matches!(direction, Direction::ToChild))
        .create(&name)?;
    // Opened without `FILE_FLAG_OVERLAPPED`, as a child's standard handle
    // expects; std's files are not inheritable.
    let client = match direction {
        Direction::FromChild => std::fs::OpenOptions::new().write(true).open(&name)?,
        Direction::ToChild => std::fs::OpenOptions::new().read(true).open(&name)?,
    };
    server.connect().await?;
    Ok((server, OwnedHandle::from(client)))
}

fn inheritable(handle: &OwnedHandle) -> io::Result<()> {
    // SAFETY: a live handle owned by the caller.
    if unsafe {
        SetHandleInformation(
            handle.as_raw_handle(),
            HANDLE_FLAG_INHERIT,
            HANDLE_FLAG_INHERIT,
        )
    } == 0
    {
        return Err(last_error());
    }
    Ok(())
}

/// An initialized `PROC_THREAD_ATTRIBUTE_LIST`, deleted on drop.
struct Attributes(Vec<u64>);

impl Attributes {
    fn new(count: u32) -> io::Result<Self> {
        let mut size = 0usize;
        // SAFETY: a size query; it fails and reports the size.
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), count, 0, &mut size);
        }
        if size == 0 {
            return Err(last_error());
        }
        let mut buffer = vec![0u64; size.div_ceil(8)];
        // SAFETY: `buffer` holds at least `size` bytes, 8-aligned.
        if unsafe {
            InitializeProcThreadAttributeList(buffer.as_mut_ptr().cast(), count, 0, &mut size)
        } == 0
        {
            return Err(last_error());
        }
        Ok(Self(buffer))
    }

    fn pointer(&mut self) -> *mut c_void {
        self.0.as_mut_ptr().cast()
    }

    /// Sets `attribute` to the `size` bytes at `value`, which must outlive
    /// the process creation.
    fn set(&mut self, attribute: usize, value: *const c_void, size: usize) -> io::Result<()> {
        // SAFETY: the list is initialized; the caller keeps `value` alive.
        if unsafe {
            UpdateProcThreadAttribute(
                self.pointer(),
                0,
                attribute,
                value,
                size,
                null_mut(),
                null(),
            )
        } == 0
        {
            return Err(last_error());
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

/// The value of `name` in `env`, compared without case; `None` when absent.
fn env_value<'a>(env: &'a [(OsString, OsString)], name: &str) -> Option<&'a OsStr> {
    env.iter()
        .rev()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_os_str())
}

/// The program's path. A path with a directory part is used as given,
/// relative ones against the child's working directory; a bare name is
/// looked up on the child's `PATH`, with `.exe` added when it has no
/// extension, so the current directory is never searched implicitly.
/// Batch files are refused: `CreateProcessW` would run them through
/// `cmd.exe` with its own quoting rules, which this path does not
/// implement.
fn resolve_program(spec: &ProcessSpec) -> io::Result<PathBuf> {
    let program = Path::new(&spec.program);
    let resolved = if program.components().count() > 1 || program.is_absolute() {
        if program.is_absolute() {
            program.to_path_buf()
        } else {
            let base = match &spec.cwd {
                Some(cwd) => cwd.clone(),
                None => std::env::current_dir()?,
            };
            base.join(program)
        }
    } else {
        let path = match &spec.env {
            Some(env) => env_value(env, "PATH").map(OsStr::to_os_string),
            None => std::env::var_os("PATH"),
        }
        .unwrap_or_default();
        let names: Vec<OsString> = if program.extension().is_some() {
            vec![spec.program.clone()]
        } else {
            let mut exe = spec.program.clone();
            exe.push(".exe");
            vec![exe]
        };
        std::env::split_paths(&path)
            .filter(|dir| dir.is_absolute())
            .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
            .find(|candidate| candidate.is_file())
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?
    };
    let batch = resolved
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("bat") || ext.eq_ignore_ascii_case("cmd"));
    if batch {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "batch files cannot be run as owned processes",
        ));
    }
    Ok(resolved)
}

/// The environment block of `env`, later entries winning, sorted without
/// case as `CreateProcessW` requires.
fn environment_block(env: &[(OsString, OsString)]) -> io::Result<Vec<u16>> {
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
    let mut variables: Vec<(Vec<u16>, &OsString, &OsString)> = Vec::new();
    for (key, value) in env {
        let folded = upper(key);
        variables.retain(|(existing, _, _)| *existing != folded);
        variables.push((folded, key, value));
    }
    variables.sort_by(|left, right| left.0.cmp(&right.0));
    let mut block = Vec::new();
    for (_, key, value) in variables {
        let entry: Vec<u16> = key
            .encode_wide()
            .chain([u16::from(b'=')])
            .chain(value.encode_wide())
            .collect();
        if entry.contains(&0) || key.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "an environment variable is empty or contains a NUL character",
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

/// Creates `spec` inside a new job, with its standard input piped or read
/// from `NUL` and its standard output and error piped.
///
/// Cancellation contract: every await (the pipe connections) comes before
/// `CreateProcessW`. From the creation on, this runs to its return without
/// suspending, and [`super::OwnedProcess::spawn`] builds its owner from the
/// result in the same poll, so no cancellation can fall between a created
/// process and the owner (with its permit) that settles it. Dropped before
/// the creation, it leaves pipes and a job with no member, which close.
pub(super) async fn spawn(spec: &ProcessSpec) -> io::Result<Spawned> {
    let application = resolve_program(spec)?;
    let mut argv: Vec<Vec<u16>> = vec![application.as_os_str().encode_wide().collect()];
    argv.extend(spec.args.iter().map(|arg| arg.encode_wide().collect()));
    if argv.iter().any(|arg| arg.contains(&0)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "nul byte found in provided data",
        ));
    }
    let mut command_line = crate::command_line::build_command_line(&argv);
    command_line.push(0);
    let application = wide(application.as_os_str())?;
    let cwd = spec
        .cwd
        .as_ref()
        .map(|cwd| wide(cwd.as_os_str()))
        .transpose()?;
    let environment = spec.env.as_deref().map(environment_block).transpose()?;

    // Kill-on-close from before the command exists: if this process dies,
    // or the job's handle is closed for any reason but a normal detach,
    // the whole tree is ended. A command that exits normally has the limit
    // cleared first (`detach`), which leaves its descendants running, as Pi
    // does.
    // SAFETY: null attributes (not inheritable) and no name.
    let job = unsafe { CreateJobObjectW(null(), null()) };
    if job.is_null() {
        return Err(last_error());
    }
    // SAFETY: a fresh handle owned here.
    let job = unsafe { OwnedHandle::from_raw_handle(job) };
    crate::tree::set_kill_on_close(&job, true)?;
    // Associated while the job has never had a process, so every
    // association, the leader's first, reports to it: a settled kill names
    // its members from these messages (`crate::job_ledger`).
    let port = crate::tree::completion_port()?;
    crate::tree::associate_port(&job, &port)?;

    let (stdout_read, stdout_write) = pipe(Direction::FromChild).await?;
    let (stderr_read, stderr_write) = pipe(Direction::FromChild).await?;
    let (stdin_child, stdin_parent) = if spec.stdin {
        let (write, read) = pipe(Direction::ToChild).await?;
        (read, Some(write))
    } else {
        let null_device = std::fs::OpenOptions::new().read(true).open("NUL")?;
        (OwnedHandle::from(null_device), None)
    };
    // The child's ends become inheritable only now, and only the handle
    // list decides what this child inherits. A process another thread
    // creates meanwhile with inheritance and no list may inherit them;
    // that holds a pipe open past the child, which the exit grace bounds
    // and an overlapped read does not outlive.
    for handle in [&stdin_child, &stdout_write, &stderr_write] {
        inheritable(handle)?;
    }
    let inherited: [HANDLE; 3] = [
        stdin_child.as_raw_handle(),
        stdout_write.as_raw_handle(),
        stderr_write.as_raw_handle(),
    ];
    let job_list: [HANDLE; 1] = [job.as_raw_handle()];
    let mut attributes = Attributes::new(2)?;
    attributes.set(
        PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
        job_list.as_ptr().cast(),
        size_of_val(&job_list),
    )?;
    attributes.set(
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
        inherited.as_ptr().cast(),
        size_of_val(&inherited),
    )?;
    // SAFETY: a zeroed STARTUPINFOEXW is valid before its fields are set.
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb =
        u32::try_from(size_of::<STARTUPINFOEXW>()).map_err(|_| io::Error::other("size"))?;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = inherited[0];
    startup.StartupInfo.hStdOutput = inherited[1];
    startup.StartupInfo.hStdError = inherited[2];
    startup.lpAttributeList = attributes.pointer();
    // SAFETY: a zeroed PROCESS_INFORMATION is valid output space.
    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: every pointer is valid and NUL-terminated where required;
    // `command_line` is mutable as the API requires; the attribute values
    // and listed handles outlive the call; the environment block is
    // double-NUL-terminated.
    let created = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            1,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
            environment
                .as_ref()
                .map_or(null(), |block| block.as_ptr().cast()),
            cwd.as_ref().map_or(null(), |cwd| cwd.as_ptr()),
            &startup.StartupInfo,
            &mut info,
        )
    };
    if created == 0 {
        return Err(last_error());
    }
    drop(attributes);
    // The child holds its own copies now.
    drop((stdin_child, stdout_write, stderr_write));
    if !info.hThread.is_null() {
        // SAFETY: a fresh thread handle owned here, not needed.
        drop(unsafe { OwnedHandle::from_raw_handle(info.hThread) });
    }
    if info.hProcess.is_null() {
        // `CreateProcessW` documents a valid process handle on success, so
        // this does not happen; if it did, there is no handle to wait on,
        // and the error says the tree's end was not observed.
        // SAFETY: `job` is live.
        unsafe {
            TerminateJobObject(job.as_raw_handle(), 1);
        }
        return Err(io::Error::other(
            "CreateProcessW returned no process handle; its job was terminated, unobserved",
        ));
    }
    // SAFETY: a fresh process handle owned here.
    let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess) };
    let leader = Leader {
        process,
        pid: info.dwProcessId,
    };
    Ok(Spawned {
        leader,
        job,
        port,
        stdin: stdin_parent,
        stdout: Some(stdout_read),
        stderr: Some(stderr_read),
    })
}
