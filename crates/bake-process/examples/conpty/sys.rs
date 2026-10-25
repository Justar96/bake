//! The Win32 calls the ConPTY host and its client make. The only `unsafe`
//! code is these calls, each with its contract. Every failure is an
//! `io::Error` naming the call, so the driver's report says which API
//! failed and why.

use std::ffi::{OsStr, OsString, c_void};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    ERROR_INVALID_PARAMETER, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND,
};
use windows_sys::Win32::System::Console::{
    COORD, CTRL_BREAK_EVENT, CTRL_C_EVENT, ClosePseudoConsole, CreatePseudoConsole,
    GenerateConsoleCtrlEvent, GetConsoleMode, HPCON, ResizePseudoConsole, SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::IO::CancelSynchronousIo;
use windows_sys::Win32::System::JobObjects::{
    IsProcessInJob, JobObjectBasicProcessIdList, QueryInformationJobObject,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    GetExitCodeProcess, GetProcessIdOfThread, InitializeProcThreadAttributeList, OpenProcess,
    OpenThread, PROC_THREAD_ATTRIBUTE_JOB_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
    PROCESS_INFORMATION, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, QueryFullProcessImageNameW, ResumeThread,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, SuspendThread, THREAD_QUERY_LIMITED_INFORMATION,
    THREAD_SUSPEND_RESUME, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

use crate::protocol::Ctrl;

/// The error of the last call, prefixed with its name.
fn failed(call: &str) -> io::Error {
    let error = io::Error::last_os_error();
    io::Error::new(error.kind(), format!("{call}: {error}"))
}

fn hresult(call: &str, result: i32) -> io::Result<()> {
    if result < 0 {
        return Err(io::Error::other(format!("{call}: HRESULT {result:#010x}")));
    }
    Ok(())
}

/// UTF-16 with a terminating NUL; an embedded NUL is an error.
pub fn wide(text: &OsStr) -> io::Result<Vec<u16>> {
    let mut units: Vec<u16> = text.encode_wide().collect();
    if units.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a NUL in a path or argument",
        ));
    }
    units.push(0);
    Ok(units)
}

/// A pseudoconsole, closed once, by [`Self::close`] or on drop.
pub struct PseudoConsole {
    hpc: HPCON,
    open: bool,
}

impl PseudoConsole {
    /// Creates a pseudoconsole reading `input` and writing `output`, with
    /// no flags: no inherited cursor, so conhost asks nothing at startup.
    /// The caller closes its copies of both once the client exists.
    pub fn new(
        cols: i16,
        rows: i16,
        input: BorrowedHandle<'_>,
        output: BorrowedHandle<'_>,
    ) -> io::Result<Self> {
        let mut hpc: HPCON = 0;
        // SAFETY: two live synchronous pipe ends and a valid out pointer.
        let result = unsafe {
            CreatePseudoConsole(
                COORD { X: cols, Y: rows },
                input.as_raw_handle(),
                output.as_raw_handle(),
                0,
                &mut hpc,
            )
        };
        hresult("CreatePseudoConsole", result)?;
        Ok(Self { hpc, open: true })
    }

    pub fn raw(&self) -> HPCON {
        self.hpc
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn resize(&self, cols: i16, rows: i16) -> io::Result<()> {
        if !self.open {
            return Err(io::Error::other(
                "ResizePseudoConsole: the pseudoconsole is closed",
            ));
        }
        // SAFETY: a live pseudoconsole.
        hresult("ResizePseudoConsole", unsafe {
            ResizePseudoConsole(self.hpc, COORD { X: cols, Y: rows })
        })
    }

    /// Closes the pseudoconsole, which sends `CTRL_CLOSE_EVENT` to every
    /// client still attached. Before Windows 11 24H2 this blocks until the
    /// clients disconnect, and conhost may write a final frame meanwhile,
    /// so the output pipe must be drained on another thread throughout.
    /// From 24H2 it returns at once. Either way it says nothing about the
    /// clients: their end is read from their own handles and the job.
    pub fn close(&mut self) {
        if std::mem::take(&mut self.open) {
            // SAFETY: a live pseudoconsole, closed once.
            unsafe { ClosePseudoConsole(self.hpc) }
        }
    }
}

impl Drop for PseudoConsole {
    fn drop(&mut self) {
        self.close();
    }
}

/// A one-way, single-instance named pipe server with a random name, for
/// the client's control channel; returns it and its name. Remote clients
/// are refused; if another local process connects first, the client's open
/// fails and it reports that. Synchronous: one thread uses each end.
pub fn named_pipe(inbound: bool) -> io::Result<(OwnedHandle, OsString)> {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| io::Error::other("no random source"))?;
    let id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let name = OsString::from(format!(r"\\.\pipe\bake-conpty-{}-{id}", std::process::id()));
    let access = if inbound {
        PIPE_ACCESS_INBOUND
    } else {
        PIPE_ACCESS_OUTBOUND
    };
    let wide_name = wide(&name)?;
    // SAFETY: a NUL-terminated name; default security (this user and
    // administrators).
    let raw = unsafe {
        CreateNamedPipeW(
            wide_name.as_ptr(),
            access | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            64 * 1024,
            64 * 1024,
            0,
            null(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(failed("CreateNamedPipeW"));
    }
    // SAFETY: a fresh handle owned here.
    Ok((unsafe { OwnedHandle::from_raw_handle(raw) }, name))
}

/// Waits for the client to open `pipe`. Blocks until it does, or until
/// `CancelSynchronousIo` ends the wait (see [`join_bounded`]); a client
/// that opened it first is connected already.
pub fn connect(pipe: BorrowedHandle<'_>) -> io::Result<()> {
    // SAFETY: a live pipe server handle; a synchronous connect.
    if unsafe { ConnectNamedPipe(pipe.as_raw_handle(), null_mut()) } == 0
        && io::Error::last_os_error().raw_os_error() != Some(ERROR_PIPE_CONNECTED as i32)
    {
        return Err(failed("ConnectNamedPipe"));
    }
    Ok(())
}

/// An initialized `PROC_THREAD_ATTRIBUTE_LIST`, deleted on drop; as
/// `bake-process`'s own Windows spawn keeps one.
struct Attributes(Vec<u64>);

impl Attributes {
    fn new(count: u32) -> io::Result<Self> {
        let mut size = 0usize;
        // SAFETY: a size query; it fails and reports the size.
        unsafe { InitializeProcThreadAttributeList(null_mut(), count, 0, &mut size) };
        if size == 0 {
            return Err(failed("InitializeProcThreadAttributeList"));
        }
        let mut buffer = vec![0u64; size.div_ceil(8)];
        // SAFETY: `buffer` holds at least `size` bytes, 8-aligned.
        if unsafe {
            InitializeProcThreadAttributeList(buffer.as_mut_ptr().cast(), count, 0, &mut size)
        } == 0
        {
            return Err(failed("InitializeProcThreadAttributeList"));
        }
        Ok(Self(buffer))
    }

    fn pointer(&mut self) -> *mut c_void {
        self.0.as_mut_ptr().cast()
    }

    /// Sets `attribute` to `size` bytes at `value`, which must outlive the
    /// process creation.
    fn set(&mut self, attribute: u32, value: *const c_void, size: usize) -> io::Result<()> {
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
            return Err(failed("UpdateProcThreadAttribute"));
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

/// A process created attached to a pseudoconsole and born in a job.
pub struct Created {
    pub process: OwnedHandle,
    pub pid: u32,
}

/// Creates `command_line` attached to `console`, inside `job` from its
/// first instruction, inheriting no handle. Its standard handles are null
/// rather than this process's, as Windows Terminal starts its ConPTY
/// clients: without `STARTF_USESTDHANDLES` a client can be handed the
/// host's redirected streams instead of the console. The client opens
/// `CONIN$` and `CONOUT$` itself.
///
/// The attribute list holds the pseudoconsole and the job handle only for
/// the creation; the pseudoconsole and job stay owned by the caller and
/// outlive the process.
pub fn create_client(
    mut command_line: Vec<u16>,
    cwd: Option<&OsStr>,
    console: &PseudoConsole,
    job: BorrowedHandle<'_>,
) -> io::Result<Created> {
    command_line.push(0);
    let cwd = cwd.map(wide).transpose()?;
    let job_list: [HANDLE; 1] = [job.as_raw_handle()];
    let mut attributes = Attributes::new(2)?;
    // The pseudoconsole attribute's value is the HPCON itself.
    attributes.set(
        PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
        console.raw() as *const c_void,
        size_of::<HPCON>(),
    )?;
    attributes.set(
        PROC_THREAD_ATTRIBUTE_JOB_LIST,
        job_list.as_ptr().cast(),
        size_of_val(&job_list),
    )?;
    // SAFETY: a zeroed STARTUPINFOEXW is valid before its fields are set.
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb =
        u32::try_from(size_of::<STARTUPINFOEXW>()).map_err(|_| io::Error::other("size"))?;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.lpAttributeList = attributes.pointer();
    // SAFETY: a zeroed PROCESS_INFORMATION is valid output space.
    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the command line is mutable and NUL-terminated, the working
    // directory NUL-terminated; the attribute values, job, and
    // pseudoconsole outlive the call. A null environment is this
    // process's.
    let created = unsafe {
        CreateProcessW(
            null(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT,
            null(),
            cwd.as_ref().map_or(null(), |cwd| cwd.as_ptr()),
            &startup.StartupInfo,
            &mut info,
        )
    };
    if created == 0 {
        return Err(failed("CreateProcessW"));
    }
    drop(attributes);
    if !info.hThread.is_null() {
        // SAFETY: a fresh thread handle owned here, not needed.
        drop(unsafe { OwnedHandle::from_raw_handle(info.hThread) });
    }
    if info.hProcess.is_null() {
        return Err(io::Error::other(
            "CreateProcessW returned no process handle",
        ));
    }
    // SAFETY: a fresh process handle owned here.
    let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess) };
    Ok(Created {
        process,
        pid: info.dwProcessId,
    })
}

/// Whether `process` is signalled within `wait`.
pub fn signalled(process: BorrowedHandle<'_>, wait: Duration) -> io::Result<bool> {
    let millis = u32::try_from(wait.as_millis()).unwrap_or(u32::MAX - 1);
    // SAFETY: a live handle with SYNCHRONIZE; a bounded wait.
    match unsafe { WaitForSingleObject(process.as_raw_handle(), millis) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        WAIT_FAILED => Err(failed("WaitForSingleObject")),
        other => Err(io::Error::other(format!(
            "WaitForSingleObject returned {other:#x}"
        ))),
    }
}

/// The exit code of a process that has exited.
pub fn exit_code(process: BorrowedHandle<'_>) -> io::Result<u32> {
    let mut code = 0u32;
    // SAFETY: a live process handle and a valid out pointer.
    if unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) } == 0 {
        return Err(failed("GetExitCodeProcess"));
    }
    Ok(code)
}

/// Asks the system to end `process` with `code`. A request only.
pub fn terminate(process: BorrowedHandle<'_>, code: u32) -> io::Result<()> {
    // SAFETY: a live process handle.
    if unsafe { TerminateProcess(process.as_raw_handle(), code) } == 0 {
        return Err(failed("TerminateProcess"));
    }
    Ok(())
}

/// Opens `pid` as a member of `job`, or `None` when no such process exists
/// or it is not a member (an unrelated process given a reused pid). The
/// handle pins the pid for as long as it is held.
pub fn open_member(pid: u32, job: BorrowedHandle<'_>) -> io::Result<Option<OwnedHandle>> {
    // SAFETY: plain call; a null handle is an error.
    let raw = unsafe {
        OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
            0,
            pid,
        )
    };
    if raw.is_null() {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            Ok(None)
        } else {
            Err(failed("OpenProcess"))
        };
    }
    // SAFETY: a fresh handle owned here.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut member = 0;
    // SAFETY: two live handles and a valid out pointer.
    if unsafe { IsProcessInJob(handle.as_raw_handle(), job.as_raw_handle(), &mut member) } == 0 {
        return Err(failed("IsProcessInJob"));
    }
    Ok((member != 0).then_some(handle))
}

/// The image path of a process, or an empty string when it cannot be read.
pub fn image(process: BorrowedHandle<'_>) -> String {
    let mut buffer = vec![0u16; 2048];
    let mut size = u32::try_from(buffer.len()).unwrap_or(0);
    // SAFETY: a live handle with query access and a buffer of `size` units.
    if unsafe {
        QueryFullProcessImageNameW(
            process.as_raw_handle(),
            PROCESS_NAME_WIN32,
            buffer.as_mut_ptr(),
            &mut size,
        )
    } == 0
    {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..size as usize])
}

/// The pids of the job's live members, as one listing.
pub fn job_pids(job: BorrowedHandle<'_>) -> io::Result<Vec<u32>> {
    /// `JOBOBJECT_BASIC_PROCESS_ID_LIST` with room for 1024 pids.
    #[repr(C)]
    struct List {
        assigned: u32,
        listed: u32,
        ids: [usize; 1024],
    }
    let mut list = List {
        assigned: 0,
        listed: 0,
        ids: [0; 1024],
    };
    let size = u32::try_from(size_of::<List>()).map_err(|_| io::Error::other("size"))?;
    // SAFETY: `list` has the size passed and the layout the class expects.
    if unsafe {
        QueryInformationJobObject(
            job.as_raw_handle(),
            JobObjectBasicProcessIdList,
            std::ptr::addr_of_mut!(list).cast(),
            size,
            null_mut(),
        )
    } == 0
    {
        return Err(failed(
            "QueryInformationJobObject(JobObjectBasicProcessIdList)",
        ));
    }
    if list.listed < list.assigned {
        return Err(io::Error::other(format!(
            "the job has {} members; the listing holds 1024",
            list.assigned
        )));
    }
    let listed = (list.listed as usize).min(list.ids.len());
    Ok(list.ids[..listed]
        .iter()
        .filter_map(|&pid| u32::try_from(pid).ok())
        .collect())
}

/// Suspends every thread of `pid`, whose process handle the caller holds so
/// the pid is pinned. Snapshots are repeated until one finds no thread not
/// already suspended, so a thread started meanwhile is caught; a thread id
/// that opens as another process's is skipped. Returns the suspended
/// threads' handles, which [`resume`] takes.
pub fn suspend(pid: u32) -> io::Result<Vec<OwnedHandle>> {
    let mut held: Vec<(u32, OwnedHandle)> = Vec::new();
    for _ in 0..16 {
        let mut found = false;
        for tid in threads(pid)? {
            if held.iter().any(|(known, _)| *known == tid) {
                continue;
            }
            // SAFETY: plain call; a null handle means the thread is gone.
            let raw = unsafe {
                OpenThread(
                    THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION,
                    0,
                    tid,
                )
            };
            if raw.is_null() {
                continue;
            }
            // SAFETY: a fresh handle owned here.
            let thread = unsafe { OwnedHandle::from_raw_handle(raw) };
            // SAFETY: a live thread handle with query access.
            if unsafe { GetProcessIdOfThread(thread.as_raw_handle()) } != pid {
                continue;
            }
            // SAFETY: a live thread handle with suspend access.
            if unsafe { SuspendThread(thread.as_raw_handle()) } == u32::MAX {
                let error = failed("SuspendThread");
                resume(held.into_iter().map(|(_, thread)| thread).collect());
                return Err(error);
            }
            held.push((tid, thread));
            found = true;
        }
        if !found {
            return Ok(held.into_iter().map(|(_, thread)| thread).collect());
        }
    }
    resume(held.into_iter().map(|(_, thread)| thread).collect());
    Err(io::Error::other(
        "the program kept starting threads while it was being suspended",
    ))
}

/// Resumes threads [`suspend`] returned, once each, and closes them.
pub fn resume(threads: Vec<OwnedHandle>) {
    for thread in threads {
        // SAFETY: a live thread handle with suspend access; the count it
        // returns is not needed.
        unsafe { ResumeThread(thread.as_raw_handle()) };
    }
}

/// The ids of `pid`'s threads in one system snapshot.
fn threads(pid: u32) -> io::Result<Vec<u32>> {
    // SAFETY: plain call; INVALID_HANDLE_VALUE is an error.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return Err(failed("CreateToolhelp32Snapshot"));
    }
    // SAFETY: a fresh handle owned here.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
    // SAFETY: a zeroed entry is valid once its size is set.
    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize =
        u32::try_from(size_of::<THREADENTRY32>()).map_err(|_| io::Error::other("size"))?;
    let mut ids = Vec::new();
    // SAFETY: a live snapshot and an entry with its size set.
    let mut more = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) } != 0;
    while more {
        if entry.th32OwnerProcessID == pid {
            ids.push(entry.th32ThreadID);
        }
        // SAFETY: as above.
        more = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) } != 0;
    }
    Ok(ids)
}

/// `GetConsoleMode` of an input and an output console handle.
pub fn console_modes(
    input: BorrowedHandle<'_>,
    output: BorrowedHandle<'_>,
) -> io::Result<(u32, u32)> {
    let (mut input_mode, mut output_mode) = (0u32, 0u32);
    // SAFETY: live console handles and valid out pointers.
    if unsafe { GetConsoleMode(input.as_raw_handle(), &mut input_mode) } == 0 {
        return Err(failed("GetConsoleMode(CONIN$)"));
    }
    // SAFETY: as above.
    if unsafe { GetConsoleMode(output.as_raw_handle(), &mut output_mode) } == 0 {
        return Err(failed("GetConsoleMode(CONOUT$)"));
    }
    Ok((input_mode, output_mode))
}

/// Generates `event` for every process attached to this process's console.
pub fn generate(event: Ctrl) -> io::Result<()> {
    let kind = match event {
        Ctrl::C => CTRL_C_EVENT,
        Ctrl::Break => CTRL_BREAK_EVENT,
    };
    // SAFETY: plain call; group 0 is every process on this console.
    if unsafe { GenerateConsoleCtrlEvent(kind, 0) } == 0 {
        return Err(failed("GenerateConsoleCtrlEvent"));
    }
    Ok(())
}

/// Handles Ctrl+C and Ctrl+Break by ignoring them, so the client survives
/// the events it generates for the program. A close, logoff, or shutdown
/// falls through to the default handler, which ends the client.
pub fn ignore_interrupts() -> io::Result<()> {
    unsafe extern "system" fn handler(kind: u32) -> windows_sys::core::BOOL {
        i32::from(kind == CTRL_C_EVENT || kind == CTRL_BREAK_EVENT)
    }
    // SAFETY: a handler that only reads its argument, registered for the
    // process's life.
    if unsafe { SetConsoleCtrlHandler(Some(handler), 1) } == 0 {
        return Err(failed("SetConsoleCtrlHandler"));
    }
    Ok(())
}

/// Joins `thread`, letting `tick` run (it may block up to the duration it
/// is given) while it finishes. Past `budget`, its synchronous I/O is
/// cancelled with `CancelSynchronousIo`, repeatedly for a further bounded
/// time, since the thread may enter a read just after a cancel. A thread
/// that still does not stop is an error: it ends with the process, which
/// the caller then fails.
pub fn join_bounded<T>(
    thread: JoinHandle<T>,
    budget: Duration,
    mut tick: impl FnMut(Duration),
) -> Result<T, String> {
    let name = thread.thread().name().unwrap_or("worker").to_owned();
    let started = Instant::now();
    while !thread.is_finished() && started.elapsed() < budget {
        tick(Duration::from_millis(10));
    }
    let cancelled = Instant::now();
    while !thread.is_finished() {
        if cancelled.elapsed() >= Duration::from_secs(5) {
            return Err(format!(
                "the {name} thread did not stop after its I/O was cancelled"
            ));
        }
        // SAFETY: the thread's handle stays valid while the JoinHandle is
        // held; cancelling a thread with no I/O pending fails harmlessly.
        unsafe { CancelSynchronousIo(thread.as_handle().as_raw_handle()) };
        tick(Duration::from_millis(10));
    }
    thread
        .join()
        .map_err(|_| format!("the {name} thread panicked"))
}
