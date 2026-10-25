//! A kill-on-close Windows job with the account its end is settled on, the
//! one implementation [`crate::OwnedProcess`]'s Windows trees and
//! `bake-sandbox`'s Windows runner share.
//!
//! [`Job::new`] creates the job, sets kill-on-close, and associates a
//! completion port with it while it has never had a process, all before it
//! returns, so no process can be associated with a [`Job`] the port does
//! not hear about. There is no way to adopt a job created elsewhere: its
//! earlier members could not be accounted for.
//!
//! Its end is never read from the job's active count alone, which reaches 0
//! before a terminated member's process handle is signalled (observed
//! natively in every run of `tests/windows_job_rundown.rs`). It is settled
//! on the account in `job_ledger`: every process the port reported as added
//! is held by handle until signalled, and their distinct number must equal
//! the job's lifetime count before and after the members are checked.
//! [`Job::settle`] is one such check; a lost message, an unknown member, or
//! a contradiction never reads as settled, and a reported pid that no
//! longer opens is read as ended only on the process-id lifetime inference
//! the ledger documents.
//!
//! While members run, [`Job::pump`] takes the port's messages into the
//! account in bounded steps and closes the handles of members seen
//! signalled, so the kernel queue and the held handles stay near what
//! arrives between steps (see `pump` for the bounds and what they assume).
//! An asynchronous owner runs it from a task; a synchronous one waits with
//! [`Job::wait_pumping`], which pumps between bounded waits.
//!
//! The only `unsafe` code is the FFI calls, each with its contract.

use std::io;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::ptr::null;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    ERROR_INVALID_PARAMETER, FILETIME, INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::System::IO::{
    CreateIoCompletionPort, GetQueuedCompletionStatusEx, OVERLAPPED_ENTRY,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_ASSOCIATE_COMPLETION_PORT, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectAssociateCompletionPortInformation,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    WaitForSingleObject,
};

use crate::job_ledger::{Host, Ledger, Opened};

pub use crate::job_ledger::Verdict;

/// `JOB_OBJECT_MSG_NEW_PROCESS`, `JOB_OBJECT_MSG_EXIT_PROCESS`, and
/// `JOB_OBJECT_MSG_ABNORMAL_EXIT_PROCESS` from `winnt.h`.
const JOB_OBJECT_MSG_NEW_PROCESS: u32 = 6;
const JOB_OBJECT_MSG_EXIT_PROCESS: u32 = 7;
const JOB_OBJECT_MSG_ABNORMAL_EXIT_PROCESS: u32 = 8;
/// The key the job's messages carry; the port serves only this job.
const KEY: usize = 1;
/// Messages taken per call, and per settle poll: a backlog left from a long
/// life is drained over several polls, never in one unbounded one.
const BATCH: usize = 64;
const PER_POLL: usize = 64 * BATCH;
/// Messages one pump step takes before it yields.
const PER_STEP: usize = 4 * BATCH;
/// The pump's pacing: after a step that emptied the queue, the next comes
/// after a pause that backs off between these while nothing arrives.
pub(crate) const MIN_PAUSE: Duration = Duration::from_millis(1);
pub(crate) const MAX_PAUSE: Duration = Duration::from_millis(50);
/// A blocking settle's pause between polls backs off up to this.
const MAX_SETTLE_PAUSE: Duration = Duration::from_millis(20);

/// What one [`Job::pump`] step did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    /// Messages taken from the port.
    pub taken: usize,
    /// Whether the queue was seen empty; when not, more is already queued.
    pub empty: bool,
}

impl Step {
    /// The pause before the next step, from the one before this step:
    /// none while the queue is not empty, the shortest after a step that
    /// took messages, and otherwise twice the last, up to the longest.
    pub(crate) fn next_pause(self, last: Duration) -> Duration {
        if !self.empty {
            Duration::ZERO
        } else if self.taken > 0 {
            MIN_PAUSE
        } else {
            (last * 2).clamp(MIN_PAUSE, MAX_PAUSE)
        }
    }
}

/// A kill-on-close job and the account of its members; see the module
/// docs. Dropping it closes the job's handle, which (kill-on-close) starts
/// the termination of every member but does not wait for it: only a
/// [`Verdict::Settled`] says the members have ended.
#[derive(Debug)]
pub struct Job {
    handle: OwnedHandle,
    /// Associated with the job while it was empty; not inheritable and
    /// never shared, so only the job posts to it.
    port: OwnedHandle,
    ledger: Mutex<Ledger<OwnedHandle>>,
}

impl Job {
    /// Creates an empty, kill-on-close job whose completion port is already
    /// associated. Its handle is not inheritable. On an error nothing was
    /// associated with it, and what was created is closed.
    pub fn new() -> io::Result<Self> {
        // SAFETY: null attributes (not inheritable) and no name.
        let raw = unsafe { CreateJobObjectW(null(), null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh handle owned here.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        set_kill_on_close(&handle, true)?;
        // SAFETY: a new port, not tied to a file.
        let port =
            unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, std::ptr::null_mut(), 0, 1) };
        if port.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh handle owned here.
        let port = unsafe { OwnedHandle::from_raw_handle(port) };
        // Associated while the job has never had a process, so every
        // association, the first one included, reports to it.
        set_port(&handle, port.as_raw_handle())?;
        Ok(Self {
            handle,
            port,
            ledger: Mutex::new(Ledger::default()),
        })
    }

    /// Terminates every process in the job with exit code 1, as
    /// `taskkill /F` leaves. A request only: whether the members have ended
    /// is only ever learned from [`Self::settle`].
    pub fn terminate(&self) -> io::Result<()> {
        // SAFETY: a live job handle owned here.
        if unsafe { TerminateJobObject(self.handle.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// One bounded step: takes at most a fixed number of queued messages
    /// into the account, retries members not yet known, and closes the
    /// handles of members now signalled. Reads no verdict. A port that
    /// cannot be read is an error that changes nothing: a settle still
    /// drains, and fails closed.
    pub fn pump(&self) -> io::Result<Step> {
        let mut ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        let mut host = System { job: &self.handle };
        let (taken, empty) = self.drain(&mut ledger, &mut host, PER_STEP)?;
        ledger.retry(&mut host);
        // An unreadable handle stays held; a settle reports it.
        let _ = ledger.check(&mut host);
        Ok(Step { taken, empty })
    }

    /// Waits until `process` is signalled, pumping the account between
    /// waits ([`Self::pump`], paced as the asynchronous owner's pump is),
    /// for a synchronous owner whose leader runs in this job. Waits as long
    /// as the process runs; an error is a wait that failed, never an exit.
    pub fn wait_pumping(&self, process: BorrowedHandle<'_>) -> io::Result<()> {
        let mut pause = MIN_PAUSE;
        loop {
            let millis = u32::try_from(pause.as_millis()).unwrap_or(u32::MAX);
            // SAFETY: the caller's live handle; a bounded wait.
            match unsafe { WaitForSingleObject(process.as_raw_handle(), millis) } {
                WAIT_OBJECT_0 => return Ok(()),
                WAIT_TIMEOUT => {}
                WAIT_FAILED => return Err(io::Error::last_os_error()),
                other => {
                    return Err(io::Error::other(format!(
                        "WaitForSingleObject returned {other:#x}"
                    )));
                }
            }
            pause = match self.pump() {
                Ok(step) => step.next_pause(pause),
                // Nothing is lost: the settle drains and fails closed.
                Err(_) => (pause * 2).clamp(MIN_PAUSE, MAX_PAUSE),
            };
        }
    }

    /// One settle poll: drains a bounded batch of messages, retries unknown
    /// members, reads the job's lifetime count, checks every member, and
    /// reads the count again. Does not terminate anything; see
    /// [`Self::terminate_and_settle`].
    pub fn settle(&self) -> Verdict {
        let mut ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        let mut host = System { job: &self.handle };
        match self.drain(&mut ledger, &mut host, PER_POLL) {
            Ok((_, true)) => {}
            Ok((_, false)) => return Verdict::Pending("messages are still queued"),
            Err(_) => return Verdict::Pending("the completion port could not be read"),
        }
        ledger.retry(&mut host);
        let Ok((before, _)) = accounting(&self.handle) else {
            return Verdict::Pending("the job could not be queried");
        };
        // Messages for associations up to `before` may still arrive; a
        // short account is pending, never settled.
        match self.drain(&mut ledger, &mut host, PER_POLL) {
            Ok((_, true)) => {}
            Ok((_, false)) => return Verdict::Pending("messages are still queued"),
            Err(_) => return Verdict::Pending("the completion port could not be read"),
        }
        let waiting = ledger.check(&mut host);
        let Ok((after, active)) = accounting(&self.handle) else {
            return Verdict::Pending("the job could not be queried");
        };
        ledger.verdict(before, after, active, waiting)
    }

    /// Terminates the job and polls [`Self::settle`] until it is settled,
    /// terminating it again before every later poll (a member may have
    /// created a process as the job was terminated), with a pause backing
    /// off from 1 ms to 20 ms. Blocks the calling thread: for synchronous
    /// owners. `Err` holds the last verdict once `budget` has passed, or at
    /// once on a contradiction, which never settles: the members' end is
    /// then unproved, not disproved.
    pub fn terminate_and_settle(&self, budget: Duration) -> Result<(), Verdict> {
        let started = Instant::now();
        let mut pause = MIN_PAUSE;
        loop {
            // The result is not read: only the account says what ended.
            let _ = self.terminate();
            match self.settle() {
                Verdict::Settled => return Ok(()),
                contradiction @ Verdict::Contradiction(_) => return Err(contradiction),
                pending @ Verdict::Pending(_) => {
                    let elapsed = started.elapsed();
                    if elapsed >= budget {
                        return Err(pending);
                    }
                    std::thread::sleep(pause.min(budget - elapsed));
                    pause = (pause * 2).min(MAX_SETTLE_PAUSE);
                }
            }
        }
    }

    /// Clears kill-on-close, then removes the port association, best
    /// effort, since nothing will drain the port again. On an error the
    /// limit stands. For a tree whose leader exited by itself; the account
    /// is not kept afterwards.
    pub(crate) fn detach(&self) -> io::Result<()> {
        set_kill_on_close(&self.handle, false)?;
        // Failing leaves messages queued until the job ends.
        let _ = set_port(&self.handle, std::ptr::null_mut());
        Ok(())
    }

    /// The account's distinct members and those still open.
    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> (u64, usize) {
        let ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        (ledger.counted(), ledger.open_members())
    }

    /// Records queued new-process and exit messages, in the order the port
    /// delivers them, at most `budget` rounded up to a whole batch; returns how many messages were taken and whether the
    /// queue was seen empty.
    fn drain(
        &self,
        ledger: &mut Ledger<OwnedHandle>,
        host: &mut System<'_>,
        budget: usize,
    ) -> io::Result<(usize, bool)> {
        let mut taken = 0;
        while taken < budget {
            let mut entries = [OVERLAPPED_ENTRY::default(); BATCH];
            let mut removed = 0u32;
            // SAFETY: `entries` holds `BATCH` entries; a zero timeout only
            // takes what is queued.
            let ok = unsafe {
                GetQueuedCompletionStatusEx(
                    self.port.as_raw_handle(),
                    entries.as_mut_ptr(),
                    BATCH as u32,
                    &mut removed,
                    0,
                    0,
                )
            };
            if ok == 0 {
                let error = io::Error::last_os_error();
                return if error.raw_os_error() == Some(WAIT_TIMEOUT as i32) {
                    Ok((taken, true))
                } else {
                    Err(error)
                };
            }
            let removed = (removed as usize).min(BATCH);
            for entry in &entries[..removed] {
                if entry.lpCompletionKey != KEY {
                    ledger.contradict("a message on the job's port carried another key");
                    continue;
                }
                let pid = u32::try_from(entry.lpOverlapped as usize);
                match (entry.dwNumberOfBytesTransferred, pid) {
                    (JOB_OBJECT_MSG_NEW_PROCESS, Ok(pid)) => ledger.reported(pid, host),
                    (JOB_OBJECT_MSG_NEW_PROCESS, Err(_)) => {
                        ledger.contradict("a new-process message's pid is out of range");
                    }
                    // An exit message's pid out of range marks nothing.
                    (JOB_OBJECT_MSG_EXIT_PROCESS | JOB_OBJECT_MSG_ABNORMAL_EXIT_PROCESS, pid) => {
                        if let Ok(pid) = pid {
                            ledger.exited(pid);
                        }
                    }
                    _ => {}
                }
            }
            taken += removed;
            if removed < BATCH {
                return Ok((taken, true));
            }
        }
        Ok((taken, false))
    }
}

/// The job's handle, borrowed for a process creation's
/// `PROC_THREAD_ATTRIBUTE_JOB_LIST`, `AssignProcessToJobObject`,
/// `IsProcessInJob`, and queries. A process associated through it is
/// reported to the port, since the port was associated first. The account
/// holds only while the handle is not duplicated and the job's
/// kill-on-close limit, completion port, and breakaway limits are left as
/// [`Job::new`] set them.
impl AsHandle for Job {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.handle.as_handle()
    }
}

fn set_port(job: &OwnedHandle, port: windows_sys::Win32::Foundation::HANDLE) -> io::Result<()> {
    let info = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
        CompletionKey: KEY as *mut core::ffi::c_void,
        CompletionPort: port,
    };
    let size = u32::try_from(size_of::<JOBOBJECT_ASSOCIATE_COMPLETION_PORT>())
        .map_err(|_| io::Error::other("size"))?;
    // SAFETY: `info` has the size passed; both handles are live.
    let ok = unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectAssociateCompletionPortInformation,
            std::ptr::addr_of!(info).cast(),
            size,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Sets or clears the job's kill-on-close limit, the only limit set on it.
fn set_kill_on_close(job: &OwnedHandle, on: bool) -> io::Result<()> {
    // SAFETY: a zeroed struct is a valid starting value.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    if on {
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    }
    let size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
        .map_err(|_| io::Error::other("size"))?;
    // SAFETY: `limits` has the size passed; the job handle is live.
    let ok = unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            std::ptr::addr_of!(limits).cast(),
            size,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The job's `(TotalProcesses, ActiveProcesses)`: every process ever
/// associated, failed associations included, and those still active.
fn accounting(job: &OwnedHandle) -> io::Result<(u32, u32)> {
    // SAFETY: a zeroed struct is valid output space.
    let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
    let size = u32::try_from(size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>())
        .map_err(|_| io::Error::other("size"))?;
    // SAFETY: `info` has the size passed; the job handle is live.
    let ok = unsafe {
        QueryInformationJobObject(
            job.as_raw_handle(),
            JobObjectBasicAccountingInformation,
            std::ptr::addr_of_mut!(info).cast(),
            size,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((info.TotalProcesses, info.ActiveProcesses))
}

/// The system, for the ledger.
struct System<'a> {
    job: &'a OwnedHandle,
}

impl Host for System<'_> {
    type Handle = OwnedHandle;

    fn open(&mut self, pid: u32) -> Opened<OwnedHandle> {
        // SAFETY: plain call; a null handle is an error.
        let raw = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        if raw.is_null() {
            return if io::Error::last_os_error().raw_os_error()
                == Some(ERROR_INVALID_PARAMETER as i32)
            {
                Opened::NoSuchProcess
            } else {
                Opened::Unknown
            };
        }
        // SAFETY: a fresh handle owned here.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut member = 0;
        // SAFETY: both handles are live.
        if unsafe {
            IsProcessInJob(
                handle.as_raw_handle(),
                self.job.as_raw_handle(),
                &mut member,
            )
        } == 0
        {
            return Opened::Unknown;
        }
        if member == 0 {
            return Opened::OtherProcess;
        }
        let mut times = [FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        }; 4];
        let [created, exited, kernel, user] = &mut times;
        // A failed query leaves a member held, without a creation time; the
        // failure never reads as an exit.
        // SAFETY: a live handle with query access and valid out pointers.
        let timed =
            unsafe { GetProcessTimes(handle.as_raw_handle(), created, exited, kernel, user) } != 0;
        let created = timed.then(|| {
            (u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime)
        });
        Opened::Member { created, handle }
    }

    fn signalled(&mut self, handle: &OwnedHandle) -> Option<bool> {
        // SAFETY: a live handle opened with SYNCHRONIZE; a zero timeout
        // only checks.
        match unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } {
            WAIT_OBJECT_0 => Some(true),
            WAIT_TIMEOUT => Some(false),
            _ => None,
        }
    }
}

// On real jobs. The oracle is each member's own process handle, which the
// test holds independently of the account.
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::io::AsRawHandle;
    use std::process::{Child, Command, Stdio};
    use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

    const BOUND: Duration = Duration::from_secs(20);

    /// A child whose stdin remains open until its owner releases it.
    struct HeldChild(Child);

    impl Drop for HeldChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    #[ignore = "subprocess fixture: the parent owns its stdin and termination"]
    fn held_member_fixture() {
        use std::io::Read;
        let mut byte = [0];
        let _ = std::io::stdin().read(&mut byte);
    }

    fn assigned(job: &Job) -> HeldChild {
        let child = HeldChild(
            Command::new(std::env::current_exe().expect("test executable"))
                .args([
                    "--exact",
                    "windows_job::tests::held_member_fixture",
                    "--ignored",
                    "--nocapture",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn held member"),
        );
        // SAFETY: two live handles. The stdin guard keeps the child alive
        // until this assignment, even if the host pauses this test.
        let ok = unsafe {
            AssignProcessToJobObject(job.as_handle().as_raw_handle(), child.0.as_raw_handle())
        };
        assert_ne!(ok, 0, "assign: {}", io::Error::last_os_error());
        child
    }

    fn signalled(child: &Child) -> bool {
        // SAFETY: a live process handle; a zero timeout only checks.
        unsafe { WaitForSingleObject(child.as_raw_handle(), 0) == WAIT_OBJECT_0 }
    }

    // A job no process ever joined, as after a failed creation, is settled
    // at once: nothing was associated and nothing is counted.
    #[test]
    fn an_empty_job_settles_at_once() {
        let job = Job::new().expect("job");
        assert_eq!(job.settle(), Verdict::Settled);
        assert_eq!(job.terminate_and_settle(Duration::ZERO), Ok(()));
    }

    // A running member keeps the account pending; once terminated, the
    // settle returns only after the member's own handle is signalled.
    #[test]
    fn a_terminated_member_settles_only_once_signalled() {
        let job = Job::new().expect("job");
        let mut child = assigned(&job);
        assert!(
            matches!(job.settle(), Verdict::Pending(_)),
            "a running member read as settled"
        );
        job.terminate_and_settle(BOUND).expect("settled");
        assert!(
            signalled(&child.0),
            "settled while the member was unsignalled"
        );
        let _ = child.0.wait();
    }

    // Release the child only once the wait's pump has counted it. A timeout
    // bounds failure; it never stands in for the observed account state.
    #[test]
    fn wait_pumping_counts_the_member_and_returns_at_its_exit() {
        let job = Job::new().expect("job");
        let mut child = assigned(&job);
        let stdin = child.0.stdin.take().expect("held stdin");
        let (wait, counted) = std::thread::scope(|scope| {
            let release = scope.spawn(|| {
                let started = Instant::now();
                let counted = loop {
                    if job.snapshot().0 > 0 {
                        break true;
                    }
                    if started.elapsed() >= BOUND {
                        break false;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                };
                // Also release on timeout, so a broken pump cannot strand
                // the synchronous waiter or this owned thread.
                drop(stdin);
                counted
            });
            let wait = job.wait_pumping(child.0.as_handle());
            (wait, release.join().expect("release thread"))
        });
        wait.expect("wait");
        assert!(counted, "the wait's pump did not count the held member");
        assert!(signalled(&child.0));
        // An arbitrarily large caller budget must not overflow Instant.
        job.terminate_and_settle(Duration::MAX).expect("settled");
        let _ = child.0.wait();
    }

    // The pacing both pumps share: no pause while the queue is not empty,
    // the shortest after taking messages, and a bounded backoff when idle.
    #[test]
    fn the_pump_pacing_backs_off_only_when_idle() {
        let step = |taken, empty| Step { taken, empty };
        assert_eq!(step(256, false).next_pause(MAX_PAUSE), Duration::ZERO);
        assert_eq!(step(3, true).next_pause(MAX_PAUSE), MIN_PAUSE);
        assert_eq!(step(0, true).next_pause(Duration::ZERO), MIN_PAUSE);
        assert_eq!(step(0, true).next_pause(MIN_PAUSE), MIN_PAUSE * 2);
        assert_eq!(step(0, true).next_pause(MAX_PAUSE), MAX_PAUSE);
    }
}
