//! A session's background jobs: work a tool or the session starts that
//! keeps running after the turn that started it, which the session can
//! read, wait for, and stop, and which the session's shutdown cancels and
//! awaits.
//!
//! [`JobRegistry`] is one session's registry, built over that session's
//! [`WorkGate`] ([`crate::agent_session::AgentSession::jobs`]). Each job is
//! a producer future the registry runs, and it owns a gate of its own: the
//! producer runs as that gate's task, and the processes and tasks it starts
//! through its [`JobContext`] hold that gate's permits. A small driver task,
//! owned by the session's gate, runs the producer, then closes the job's
//! gate and waits for it, and only then records the terminal state. So:
//!
//! - **A turn does not own a job.** `AgentSession::abort` and the end of a
//!   turn leave it running. Only [`JobRegistry::kill`], [`JobRegistry::close`],
//!   and the session gate's close stop it.
//! - **Stopping is a request.** `kill` marks the job `Stopping`, aborts its
//!   [`JobContext::signal`], and closes its gate to new work. The producer
//!   decides the outcome it returns. A job killed while it was finishing
//!   can still end `Completed`.
//! - **A producer's outcome is not the end.** When the producer returns, the
//!   job is `Completing`: what it left in its gate is cancelled and
//!   awaited (a dropped [`bake_process::OwnedProcess`] is killed and settled
//!   under its permit). The job becomes terminal, and waiters, readers, and
//!   [`JobEvents`] see it, only once that drain has finished. A
//!   `Completing` job still counts against the active limit.
//! - **Reporting is a claim, separate from the state.** A terminal job is
//!   reported once: a `kill`, a terminal `read`, a `wait` that returns the
//!   terminal state, [`JobRegistry::claim_report`],
//!   [`JobRegistry::claim_unreported`], and teardown each claim it. A wait
//!   pending when the job settles holds the claim for its waiters, and if
//!   every one of them is dropped before returning, the claim is released
//!   again and [`JobEvents`] reports a change. An attached session reserves
//!   delivery until its message is durable, and returns abandoned
//!   reservations only after that run's persistence listeners are idle.
//! - **Session jobs have a journal.** The session attaches its lifecycle
//!   recorder before admission. The driver awaits a durable start before
//!   calling the producer and a durable terminal fact before publication.
//!   A failed write prevents further session delivery; a failed start
//!   never calls the producer. A standalone registry has no recorder.
//!
//! Admission is atomic under the registry's lock: the shutdown check,
//! validation, the active limit (`running`, `stopping`, and `completing`
//! jobs), the id, and the driver's spawn happen together, or none does and
//! no id is used. The producer itself is called on the driver task, never
//! under a lock. The lifecycle recorder also runs outside that lock.
//! [`JobEvents`] wakes a subscriber, which then claims what it reports.
//!
//! Ids are `<kind>-<n>`, numbered per kind, and carry the registry they
//! came from: another session's [`JobId`] is refused as
//! [`JobError::Foreign`]. Ids are predictable, so this is the boundary, not
//! secrecy. Jobs do not survive the process. A resumed session must not
//! reuse an id an earlier process issued for different work: whoever
//! records ids in the session reserves them with
//! [`JobRegistry::reserve_ordinals`], and those ids then answer
//! [`JobError::EarlierProcess`]. No process is adopted by pid.
//!
//! Memory is bounded: at most [`JobLimits::max_active`] live jobs and
//! [`JobLimits::max_retained`] terminal ones (the oldest reported ones go
//! first, then the oldest unreported; a job with a pending waiter stays),
//! [`JobLimits::stream_bytes`] of unread streamed output per job (oldest
//! dropped first, counted), [`JobLimits::final_output_bytes`] of final
//! output (its tail kept), and labels, details, and reasons cut to
//! [`MAX_TEXT_BYTES`]. A stream job also keeps its last [`VIEW_TAIL_BYTES`]
//! of output for viewers ([`JobRegistry::views`]); reading it consumes
//! nothing the model has not read.
//!
//! Bake's own. Pi v1.3.0 (`71023aec`; MIT, see the crate `NOTICE`) has no
//! job registry; its `packages/durable` task states give the structure
//! here: a `completing` task's outcome is decided but it becomes terminal
//! only once the work it owns is done, a background task is outside a
//! conversation's aborts, and cancelling a wait cancels only the wait. The
//! retained contract is Bake's TypeScript `packages/jobs/jobs/src/{types,index}.ts`,
//! `packages/jobs/jobs-local/src/index.ts`, and the process outcome of
//! `packages/shell/tool-bash/src/background.ts` ([`process`]): `<kind>-N`
//! ids, the per-owner limit of 10 checked before the producer starts, the
//! `running`/`stopping`/terminal statuses with first-wins settlement, the
//! consuming stream cursor beside idempotent final output, bounded waits
//! whose cancellation rejects only a live job, the `reported` claim, and
//! teardown that cancels, claims, and awaits.
//!
//! Differences from the TypeScript: one registry per session rather than
//! per process, so there are no owner scopes or controller layers; no
//! listener callbacks ([`JobEvents`] instead); a `Completing` state, and
//! terminal only after the job's resources settle, where the TypeScript
//! trusted the producer's `done`; cancellation that cannot throw, so no
//! record is force-failed; and bounded retention of terminal records.
//! The session's model tools and background `bash` share this registry.
//! Its journal preserves lifecycle facts, not processes or output bytes.
//!
//! The registry is also where other agents' notices reach the session
//! ([`JobRegistry::post`]): a subagent's message or settlement waits here
//! beside the completion notices and is delivered on their path, and a
//! running subagent holds the live count ([`JobRegistry::hold_live`]), so an
//! owner waiting for jobs waits for it too ([`crate::subagents`]).

pub mod process;

use std::fmt;
use std::future::Future;
use std::io;
use std::pin::{Pin, pin};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use bake_ai::AbortSignal;
use bake_process::{OwnedProcess, ProcessSpec};
use tokio::sync::{Notify, watch};

use crate::child_process;
use crate::owned_work::{Refused, TaskScope, WorkGate};
use crate::session::messages::CustomMessage;

/// The longest label, detail, or stop reason kept, in bytes; longer text is
/// cut at a character boundary.
pub const MAX_TEXT_BYTES: usize = 1024;

/// Turns completion notices may open on an idle session before the next
/// user prompt, `tool-jobs`' `maxConsecutiveWakes`: it bounds a chain in
/// which a woken turn starts the job whose completion wakes it again.
pub const MAX_CONSECUTIVE_WAKES: usize = 3;

/// The output a viewer sees of each stream job: its newest bytes, read or
/// not ([`JobRegistry::views`]).
pub const VIEW_TAIL_BYTES: usize = 16 * 1024;

/// The stop reason [`JobRegistry::stop`] records for a stop the user asked
/// for; the completion notice names it.
pub const USER_STOP_REASON: &str = "stopped by the user";

/// The longest kind name, in bytes.
pub const MAX_KIND_BYTES: usize = 32;

/// The registry's bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobLimits {
    /// Live jobs (`running`, `stopping`, or `completing`) at once. The
    /// TypeScript default, 10.
    pub max_active: usize,
    /// Terminal jobs kept for reads.
    pub max_retained: usize,
    /// Unread streamed output kept per job.
    pub stream_bytes: usize,
    /// Final output kept per job (its tail).
    pub final_output_bytes: usize,
    /// The longest wait. The TypeScript job tools' cap, 60 s.
    pub max_wait: Duration,
}

impl Default for JobLimits {
    fn default() -> Self {
        Self {
            max_active: 10,
            max_retained: 32,
            stream_bytes: 256 * 1024,
            final_output_bytes: 256 * 1024,
            max_wait: Duration::from_secs(60),
        }
    }
}

/// How a job's output is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// The producer appends output as it goes ([`JobContext::output`]);
    /// each read consumes what arrived since the last.
    Stream,
    /// The producer returns its output with its outcome; reads are empty
    /// while the job is live and repeat that output once it is terminal.
    Final,
}

/// What to start.
#[derive(Debug, Clone)]
pub struct JobSpec {
    /// The producer's kind and the id prefix: 1 to [`MAX_KIND_BYTES`]
    /// bytes, a lowercase ASCII letter, then lowercase letters, digits, or
    /// `_`.
    pub kind: &'static str,
    /// A one-line label, such as the command. Must not be blank; cut to
    /// [`MAX_TEXT_BYTES`].
    pub label: String,
    /// How its output is read.
    pub output: OutputMode,
}

/// A job's identity: its kind, its number, and the registry that issued
/// it. Displayed as `<kind>-<n>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JobId {
    registry: u64,
    kind: &'static str,
    ordinal: u64,
}

impl JobId {
    /// The producer kind.
    pub fn kind(&self) -> &'static str {
        self.kind
    }

    /// The number within its kind, from 1.
    pub fn ordinal(&self) -> u64 {
        self.ordinal
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.kind, self.ordinal)
    }
}

/// A job's lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    /// The producer is running.
    Running,
    /// A stop was requested; the producer is still running.
    Stopping,
    /// The producer returned its outcome; the work it left is being
    /// cancelled and awaited.
    Completing,
    /// Terminal: the work finished.
    Completed,
    /// Terminal: the work was stopped.
    Killed,
    /// Terminal: the work broke.
    Failed,
}

impl JobStatus {
    /// Whether the job is settled: its outcome is recorded and its
    /// resources are released.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Killed | Self::Failed)
    }

    /// The TypeScript status word: `running`, `stopping`, `completing`,
    /// `completed`, `killed`, or `failed`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Completing => "completing",
            Self::Completed => "completed",
            Self::Killed => "killed",
            Self::Failed => "failed",
        }
    }
}

/// A producer's terminal status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeStatus {
    /// The work finished (a command's nonzero exit included).
    Completed,
    /// The work was stopped.
    Killed,
    /// The work broke.
    Failed,
}

impl From<OutcomeStatus> for JobStatus {
    fn from(status: OutcomeStatus) -> Self {
        match status {
            OutcomeStatus::Completed => Self::Completed,
            OutcomeStatus::Killed => Self::Killed,
            OutcomeStatus::Failed => Self::Failed,
        }
    }
}

/// What a producer returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobOutcome {
    /// How it ended.
    pub status: OutcomeStatus,
    /// Kind-specific detail, such as `exit code: 3`.
    pub detail: Option<String>,
    /// The final output of an [`OutputMode::Final`] job; ignored for a
    /// stream job.
    pub output: Option<String>,
}

impl JobOutcome {
    fn new(status: OutcomeStatus, detail: impl Into<String>) -> Self {
        Self {
            status,
            detail: Some(detail.into()),
            output: None,
        }
    }

    /// A completed outcome with `detail`.
    pub fn completed(detail: impl Into<String>) -> Self {
        Self::new(OutcomeStatus::Completed, detail)
    }

    /// A killed outcome with `detail`.
    pub fn killed(detail: impl Into<String>) -> Self {
        Self::new(OutcomeStatus::Killed, detail)
    }

    /// A failed outcome with `detail`.
    pub fn failed(detail: impl Into<String>) -> Self {
        Self::new(OutcomeStatus::Failed, detail)
    }

    /// This outcome with final `output`.
    #[must_use]
    pub fn with_output(mut self, output: impl Into<String>) -> Self {
        self.output = Some(output.into());
        self
    }
}

/// Why a stop was requested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopCause {
    /// [`JobRegistry::kill`], with its reason.
    Kill {
        /// The caller's reason, cut to [`MAX_TEXT_BYTES`].
        reason: Option<String>,
    },
    /// The registry or the session's gate closed.
    Shutdown,
}

/// A copy of one job's state when it was taken; never live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSnapshot {
    /// The job.
    pub id: JobId,
    /// Its label.
    pub label: String,
    /// How its output is read.
    pub output: OutputMode,
    /// Its lifecycle state.
    pub status: JobStatus,
    /// The first stop requested, if any. A stopped job may still end
    /// `Completed`.
    pub stop: Option<StopCause>,
    /// The producer's detail, once it returned.
    pub detail: Option<String>,
    /// When it was admitted, epoch milliseconds.
    pub started_at: i64,
    /// When it became terminal, epoch milliseconds.
    pub finished_at: Option<i64>,
    /// Whether its terminal state has been claimed for reporting, or a
    /// stop or teardown has said no report is wanted. For a session job,
    /// a reserved delivery also counts; this is not a durability receipt.
    pub reported: bool,
}

/// What [`JobRegistry::read`] returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRead {
    /// A stream job's output since the last read, or a final job's output
    /// once terminal.
    pub text: String,
    /// Bytes not in `text` because of the output bounds: a stream's
    /// dropped bytes since the last read, or the cut head of final output.
    pub omitted_bytes: u64,
    /// The job after the read.
    pub snapshot: JobSnapshot,
}

/// What a viewer shows of one job ([`JobRegistry::views`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobView {
    /// The job.
    pub snapshot: JobSnapshot,
    /// Its newest output, at most [`VIEW_TAIL_BYTES`]: a stream job's
    /// whether or not the model read it, a final job's once terminal.
    pub tail: String,
    /// Bytes of output before `tail`, not kept for viewers.
    pub omitted_bytes: u64,
}

/// What [`JobRegistry::kill`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillResult {
    /// The job was running; a stop is now requested.
    Requested,
    /// The job was already stopping or completing; nothing changed.
    AlreadyStopping,
    /// The job was already terminal.
    AlreadyFinished,
}

/// Why a job was not started. Nothing was started and no id was used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// The registry or the session is shut down.
    Closed,
    /// The active limit is reached.
    LimitReached {
        /// The limit.
        limit: usize,
    },
    /// The kind is not a valid kind name.
    InvalidKind,
    /// The label is blank.
    InvalidLabel,
    /// Every id of this kind is used.
    IdsExhausted,
    /// The session's gate refused the job's task.
    Refused(Refused),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => f.write_str("background jobs unavailable: the session is shut down"),
            Self::LimitReached { limit } => write!(
                f,
                "background job limit reached for this session (limit: {limit}); stop an unneeded job or wait for one to finish, then retry"
            ),
            Self::InvalidKind => f.write_str(
                "invalid job kind: expected a lowercase letter followed by lowercase letters, digits, or underscores",
            ),
            Self::InvalidLabel => f.write_str("invalid job label: expected a non-empty string"),
            Self::IdsExhausted => f.write_str("no job id of this kind is left"),
            Self::Refused(refused) => write!(f, "background job not started: {refused}"),
        }
    }
}

impl std::error::Error for StartError {}

/// Why a lookup, read, stop, or wait failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobError {
    /// No job of this session has this id. Holds the text given, cut to 64
    /// bytes.
    Unknown(String),
    /// The id belongs to another session's registry.
    Foreign(JobId),
    /// The job finished and its record was dropped to bound memory.
    Expired(JobId),
    /// The id was issued by an earlier process of this session; the job
    /// did not survive it.
    EarlierProcess(JobId),
    /// A wait's timeout is zero or above the limit.
    InvalidTimeout {
        /// The longest wait.
        max: Duration,
    },
    /// The caller cancelled the wait while the job was live.
    WaitCancelled(JobId),
}

impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(id) => write!(f, "unknown job {id}"),
            Self::Foreign(id) => write!(f, "job {id} belongs to another session"),
            Self::Expired(id) => write!(f, "job {id} finished and is no longer kept"),
            Self::EarlierProcess(id) => write!(
                f,
                "job {id} ran in an earlier process of this session and did not survive it"
            ),
            Self::InvalidTimeout { max } => write!(
                f,
                "invalid wait timeout: expected more than 0 and at most {} ms",
                max.as_millis()
            ),
            Self::WaitCancelled(id) => write!(f, "the wait for job {id} was cancelled"),
        }
    }
}

impl std::error::Error for JobError {}

/// A process-wide counter that tells registries apart.
static REGISTRIES: AtomicU64 = AtomicU64::new(1);

/// One session's background jobs. Cloning shares the registry.
#[derive(Clone)]
pub struct JobRegistry {
    inner: Arc<Inner>,
}

struct Inner {
    id: u64,
    /// Turns completion notices opened since the last user prompt
    /// ([`JobRegistry::spend_wake`]).
    wakes: AtomicUsize,
    gate: WorkGate,
    limits: JobLimits,
    state: Mutex<State>,
    /// Notified each time a job becomes terminal.
    settled: Notify,
    events: watch::Sender<Events>,
    recorder: OnceLock<JobRecorder>,
}

/// A session's durable lifecycle writer. The driver awaits it before
/// invoking the producer and before publishing a terminal outcome.
pub(crate) type JobRecorder = Arc<
    dyn Fn(JobSnapshot) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync,
>;

#[derive(Debug, Clone, Copy, Default)]
struct Events {
    generation: u64,
    closed: bool,
}

#[derive(Default)]
struct State {
    closed: bool,
    kinds: Vec<KindIds>,
    /// In admission order.
    records: Vec<Record>,
    /// Notices other agents addressed to the session
    /// ([`JobRegistry::post`]), in arrival order.
    mail: Vec<CustomMessage>,
    /// Live work outside the registry, such as a running subagent
    /// ([`JobRegistry::hold_live`]).
    holds: usize,
}

/// The ids of one kind: those at or below `floor` were issued by an
/// earlier process; `last` is the last one issued or reserved.
struct KindIds {
    kind: &'static str,
    floor: u64,
    last: u64,
}

/// The reporting claim on a job's terminal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Claim {
    Unclaimed,
    /// Held for the waits pending when the job settled.
    Waiters,
    /// Reserved by a session delivery, until its message is durable.
    Delivery,
    Claimed,
}

struct Record {
    id: JobId,
    label: String,
    output: OutputMode,
    status: JobStatus,
    stop: Option<StopCause>,
    detail: Option<String>,
    final_output: Option<String>,
    final_omitted: u64,
    stream: Option<Arc<Mutex<StreamBuffer>>>,
    started_at: i64,
    finished_at: Option<i64>,
    claim: Claim,
    admission: Option<Result<(), String>>,
    waiters: usize,
    /// The job's own gate: its producer and everything it started.
    gate: WorkGate,
    /// Notified when the job becomes terminal.
    wake: Arc<Notify>,
}

impl Record {
    fn snapshot(&self) -> JobSnapshot {
        JobSnapshot {
            id: self.id,
            label: self.label.clone(),
            output: self.output,
            status: self.status,
            stop: self.stop.clone(),
            detail: self.detail.clone(),
            started_at: self.started_at,
            finished_at: self.finished_at,
            reported: self.claim != Claim::Unclaimed,
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `text` cut to at most `max` bytes at a character boundary.
fn cut(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// The last at most `max` bytes of `text`, starting at a character
/// boundary, and how many bytes were cut.
fn tail(text: String, max: usize) -> (String, u64) {
    if text.len() <= max {
        return (text, 0);
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    (text[start..].to_owned(), start as u64)
}

fn valid_kind(kind: &str) -> bool {
    let bytes = kind.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_KIND_BYTES
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
}

/// Validates an external identity before a registry or recovery projection
/// decides whether its kind and ordinal are known.
pub(crate) fn parse_job_id(text: &str) -> Option<(&str, u64)> {
    let (kind, number) = text.rsplit_once('-')?;
    if !valid_kind(kind)
        || number.is_empty()
        || number.len() > 20
        || !number.bytes().all(|byte| byte.is_ascii_digit())
        || number.starts_with('0')
    {
        return None;
    }
    Some((kind, number.parse().ok()?))
}

impl State {
    fn kind(&self, kind: &str) -> Option<&KindIds> {
        self.kinds.iter().find(|ids| ids.kind == kind)
    }

    fn kind_mut(&mut self, kind: &'static str) -> &mut KindIds {
        if let Some(index) = self.kinds.iter().position(|ids| ids.kind == kind) {
            return &mut self.kinds[index];
        }
        self.kinds.push(KindIds {
            kind,
            floor: 0,
            last: 0,
        });
        let last = self.kinds.len() - 1;
        &mut self.kinds[last]
    }

    /// The record of `id`, or why there is none.
    fn find(&mut self, registry: u64, id: JobId) -> Result<&mut Record, JobError> {
        if id.registry != registry {
            return Err(JobError::Foreign(id));
        }
        if let Some(index) = self.records.iter().position(|record| record.id == id) {
            return Ok(&mut self.records[index]);
        }
        Err(match self.kind(id.kind) {
            Some(ids) if id.ordinal <= ids.floor => JobError::EarlierProcess(id),
            Some(ids) if id.ordinal <= ids.last => JobError::Expired(id),
            _ => JobError::Unknown(id.to_string()),
        })
    }

    fn active(&self) -> usize {
        self.records
            .iter()
            .filter(|record| !record.status.is_terminal())
            .count()
    }

    /// Drops terminal records beyond `max`: reported ones first, oldest
    /// first, and never one a wait is registered on.
    fn evict(&mut self, max: usize) {
        loop {
            let terminal = self
                .records
                .iter()
                .filter(|record| record.status.is_terminal())
                .count();
            if terminal <= max {
                return;
            }
            let evictable = |record: &Record| record.status.is_terminal() && record.waiters == 0;
            let index = self
                .records
                .iter()
                .position(|record| evictable(record) && record.claim == Claim::Claimed)
                .or_else(|| self.records.iter().position(evictable));
            match index {
                Some(index) => {
                    self.records.remove(index);
                }
                None => return,
            }
        }
    }
}

impl Inner {
    fn delivery_claim(&self) -> Claim {
        if self.recorder.get().is_some() {
            Claim::Delivery
        } else {
            Claim::Claimed
        }
    }

    async fn record(&self, snapshot: JobSnapshot) -> Result<(), String> {
        match self.recorder.get() {
            Some(recorder) => recorder(snapshot).await,
            None => Ok(()),
        }
    }

    fn admitted(&self, id: JobId, admission: Result<(), String>) {
        let wake = {
            let mut state = self.state();
            let Ok(record) = state.find(self.id, id) else {
                return;
            };
            record.admission = Some(admission);
            Arc::clone(&record.wake)
        };
        wake.notify_waiters();
    }

    fn journal_failed(&self, id: JobId, error: String) {
        // The session's write gap prevents further delivery. Keep the
        // registry drainable without announcing an undurable outcome.
        if let Ok(record) = self.state().find(self.id, id) {
            record.claim = Claim::Claimed;
        }
        self.settle(
            id,
            JobOutcome::failed(format!("the job journal failed: {error}")),
        );
    }

    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    fn announce(&self) {
        self.events
            .send_modify(|events| events.generation = events.generation.wrapping_add(1));
    }

    /// Marks a running job stopping for `cause`, and closes its gate, which
    /// aborts its producer's signal. Teardown claims its report.
    fn request_stop(&self, id: JobId, cause: StopCause) {
        let gate = {
            let mut state = self.state();
            let Ok(record) = state.find(self.id, id) else {
                return;
            };
            if record.status.is_terminal() {
                return;
            }
            record.claim = Claim::Claimed;
            if record.status == JobStatus::Running {
                record.status = JobStatus::Stopping;
                record.stop.get_or_insert(cause);
            }
            record.gate.clone()
        };
        drop(gate.close());
    }

    fn producer_returned(&self, id: JobId) {
        if let Ok(record) = self.state().find(self.id, id)
            && !record.status.is_terminal()
        {
            record.status = JobStatus::Completing;
        }
    }

    /// Records `outcome`, once; then wakes the job's waiters, the
    /// registry's close, and subscribers, outside the lock.
    fn settle(&self, id: JobId, outcome: JobOutcome) {
        let (wake, announce) = {
            let mut state = self.state();
            let closed = state.closed;
            let limits = &self.limits;
            let Ok(record) = state.find(self.id, id) else {
                return;
            };
            if record.status.is_terminal() {
                return;
            }
            record.status = outcome.status.into();
            record.detail = outcome.detail.map(|detail| cut(detail, MAX_TEXT_BYTES));
            if record.output == OutputMode::Final {
                let (output, omitted) = tail(
                    outcome.output.unwrap_or_default(),
                    limits.final_output_bytes,
                );
                record.final_output = Some(output);
                record.final_omitted = omitted;
            }
            if let Some(stream) = &record.stream {
                lock(stream).closed = true;
            }
            record.finished_at = Some(bake_ai::now_ms());
            record.claim = match record.claim {
                Claim::Claimed => Claim::Claimed,
                _ if record.waiters > 0 => Claim::Waiters,
                _ => Claim::Unclaimed,
            };
            let announce = !closed && record.claim == Claim::Unclaimed;
            let wake = Arc::clone(&record.wake);
            state.evict(limits.max_retained);
            (wake, announce)
        };
        wake.notify_waiters();
        self.settled.notify_waiters();
        if announce {
            self.announce();
        }
    }
}

impl JobRegistry {
    /// A registry whose jobs `gate` owns: each job's driver is a task of
    /// `gate`, so closing it stops every job and waits for it.
    pub fn new(gate: WorkGate, limits: JobLimits) -> Self {
        Self {
            inner: Arc::new(Inner {
                id: REGISTRIES.fetch_add(1, Ordering::Relaxed),
                wakes: AtomicUsize::new(0),
                gate,
                limits,
                state: Mutex::default(),
                settled: Notify::new(),
                events: watch::Sender::new(Events::default()),
                recorder: OnceLock::new(),
            }),
        }
    }

    /// Attached before a session can admit jobs. Keeping the recorder
    /// immutable prevents an admitted driver from changing its writer.
    pub(crate) fn attach_recorder(&self, recorder: JobRecorder) -> Result<(), String> {
        let state = self.inner.state();
        if !state.records.is_empty() || state.closed {
            return Err(
                "The session's job registry must be empty and open before its journal is attached."
                    .into(),
            );
        }
        self.inner
            .recorder
            .set(recorder)
            .map_err(|_| "The job journal is already attached.".into())
    }

    pub(crate) async fn wait_admitted(&self, id: JobId) -> Result<(), String> {
        let wake = Arc::clone(
            &self
                .inner
                .state()
                .find(self.inner.id, id)
                .map_err(|e| e.to_string())?
                .wake,
        );
        loop {
            let mut changed = pin!(wake.notified());
            changed.as_mut().enable();
            if let Some(result) = self
                .inner
                .state()
                .find(self.inner.id, id)
                .map_err(|e| e.to_string())?
                .admission
                .clone()
            {
                return result;
            }
            changed.await;
        }
    }

    pub(crate) fn acknowledge(&self, text: &str) {
        if let Ok(id) = self.id(text)
            && let Ok(record) = self.inner.state().find(self.inner.id, id)
            && record.status.is_terminal()
        {
            record.claim = Claim::Claimed;
        }
    }

    /// A delivery dropped before its durable message gives its claim
    /// back. Notices intentionally waiting for a prompt keep theirs.
    pub(crate) fn release_deliveries(&self, pending: &[String]) {
        let released = {
            let mut state = self.inner.state();
            if state.closed {
                return;
            }
            let mut released = false;
            for record in &mut state.records {
                if record.claim == Claim::Delivery && !pending.contains(&record.id.to_string()) {
                    record.claim = Claim::Unclaimed;
                    released = true;
                }
            }
            released
        };
        if released {
            self.inner.announce();
        }
    }

    /// Whether a completion notice would open a turn on an idle session:
    /// fewer than [`MAX_CONSECUTIVE_WAKES`] turns were opened by notices
    /// since the last user prompt.
    pub fn notice_would_wake(&self) -> bool {
        self.inner.wakes.load(Ordering::SeqCst) < MAX_CONSECUTIVE_WAKES
    }

    /// Takes one wake from the budget: true when a notice may open a turn.
    pub fn spend_wake(&self) -> bool {
        self.inner
            .wakes
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |spent| {
                (spent < MAX_CONSECUTIVE_WAKES).then_some(spent + 1)
            })
            .is_ok()
    }

    /// Refills the wake budget: a user prompt was sent.
    pub fn reset_wakes(&self) {
        self.inner.wakes.store(0, Ordering::SeqCst);
    }

    /// Whether `gate` owns this registry's jobs.
    pub fn is_owned_by(&self, gate: &WorkGate) -> bool {
        self.inner.gate.is_same(gate)
    }

    /// The registry's bounds.
    pub fn limits(&self) -> &JobLimits {
        &self.inner.limits
    }

    /// Admits a job and starts it, atomically: refused, with nothing
    /// started and no id used, once the registry or its gate is closing,
    /// at the active limit, or for an invalid kind or label. Then
    /// `producer` is called on the job's task, outside every lock, with the
    /// job's [`JobContext`]; the future it returns runs until it returns
    /// the job's outcome. Must be called inside a Tokio runtime.
    pub fn start<F, Fut>(&self, spec: JobSpec, producer: F) -> Result<JobId, StartError>
    where
        F: FnOnce(JobContext) -> Fut + Send + 'static,
        Fut: Future<Output = JobOutcome> + Send + 'static,
    {
        if !valid_kind(spec.kind) {
            return Err(StartError::InvalidKind);
        }
        if spec.label.trim().is_empty() {
            return Err(StartError::InvalidLabel);
        }
        let label = cut(spec.label, MAX_TEXT_BYTES);
        let inner = &self.inner;
        let mut state = inner.state();
        if state.closed {
            return Err(StartError::Closed);
        }
        let limit = inner.limits.max_active;
        if state.active() >= limit {
            return Err(StartError::LimitReached { limit });
        }
        let last = state.kind(spec.kind).map_or(0, |ids| ids.last);
        let ordinal = last.checked_add(1).ok_or(StartError::IdsExhausted)?;
        let id = JobId {
            registry: inner.id,
            kind: spec.kind,
            ordinal,
        };
        let gate = WorkGate::default();
        let stream = (spec.output == OutputMode::Stream)
            .then(|| Arc::new(Mutex::new(StreamBuffer::new(inner.limits.stream_bytes))));
        let driver = {
            let inner = Arc::clone(inner);
            let gate = gate.clone();
            let output = stream.clone().map(|buffer| JobOutput { buffer });
            move |scope| drive(inner, id, gate, output, scope, producer)
        };
        // The driver's first poll takes this lock before recording its
        // start, so it cannot run before its record is installed.
        drop(inner.gate.spawn(driver).map_err(|refused| match refused {
            Refused::Closed => StartError::Closed,
            refused => StartError::Refused(refused),
        })?);
        state.kind_mut(spec.kind).last = ordinal;
        state.records.push(Record {
            id,
            label,
            output: spec.output,
            status: JobStatus::Running,
            stop: None,
            detail: None,
            final_output: None,
            final_omitted: 0,
            stream,
            started_at: bake_ai::now_ms(),
            finished_at: None,
            claim: Claim::Unclaimed,
            admission: None,
            waiters: 0,
            gate,
            wake: Arc::new(Notify::new()),
        });
        Ok(id)
    }

    /// Reserves the ids of `kind` up to `through`, which an earlier process
    /// of this session issued: they are never issued again and answer
    /// [`JobError::EarlierProcess`].
    pub fn reserve_ordinals(&self, kind: &'static str, through: u64) -> Result<(), StartError> {
        if !valid_kind(kind) {
            return Err(StartError::InvalidKind);
        }
        let mut state = self.inner.state();
        let ids = state.kind_mut(kind);
        ids.floor = ids.floor.max(through);
        ids.last = ids.last.max(through);
        Ok(())
    }

    /// The id `<kind>-<n>` names in this registry, issued or reserved.
    pub fn id(&self, text: &str) -> Result<JobId, JobError> {
        let unknown = || JobError::Unknown(cut(text.to_owned(), 64));
        let (kind, ordinal) = parse_job_id(text).ok_or_else(unknown)?;
        let state = self.inner.state();
        let ids = state.kind(kind).ok_or_else(unknown)?;
        if ordinal > ids.last {
            return Err(unknown());
        }
        Ok(JobId {
            registry: self.inner.id,
            kind: ids.kind,
            ordinal,
        })
    }

    /// A snapshot of `id`. Changes nothing.
    pub fn get(&self, id: JobId) -> Result<JobSnapshot, JobError> {
        Ok(self.inner.state().find(self.inner.id, id)?.snapshot())
    }

    /// Snapshots of every kept job, in admission order.
    pub fn list(&self) -> Vec<JobSnapshot> {
        self.inner
            .state()
            .records
            .iter()
            .map(Record::snapshot)
            .collect()
    }

    /// Every retained job with its newest output, for a viewer such as the
    /// terminal's job list. Unlike [`Self::read`], it consumes no output
    /// and claims no report, so the model's reads and notices are
    /// unchanged by it.
    pub fn views(&self) -> Vec<JobView> {
        self.inner
            .state()
            .records
            .iter()
            .map(|record| {
                let (tail, omitted_bytes) = match &record.stream {
                    Some(stream) => lock(stream).recent(),
                    None => match &record.final_output {
                        Some(output) => {
                            let start = tail_start(output, VIEW_TAIL_BYTES);
                            (
                                output[start..].to_owned(),
                                record.final_omitted + start as u64,
                            )
                        }
                        None => (String::new(), 0),
                    },
                };
                JobView {
                    snapshot: record.snapshot(),
                    tail,
                    omitted_bytes,
                }
            })
            .collect()
    }

    /// Reads `id`'s output: a stream job's output since the last read
    /// (consumed; while live, an incomplete trailing character waits for
    /// the next read), or a final job's output, empty while live and the
    /// same on every read once terminal. A read of a terminal job claims
    /// its report.
    pub fn read(&self, id: JobId) -> Result<JobRead, JobError> {
        let mut state = self.inner.state();
        let record = state.find(self.inner.id, id)?;
        let terminal = record.status.is_terminal();
        let (text, omitted_bytes) = match &record.stream {
            Some(stream) => lock(stream).take(terminal),
            None if terminal => (
                record.final_output.clone().unwrap_or_default(),
                record.final_omitted,
            ),
            None => (String::new(), 0),
        };
        if terminal && record.claim != Claim::Claimed {
            record.claim = self.inner.delivery_claim();
        }
        Ok(JobRead {
            text,
            omitted_bytes,
            snapshot: record.snapshot(),
        })
    }

    /// Requests that `id` stop: a running job becomes `Stopping` for
    /// [`StopCause::Kill`], its signal is aborted, and its gate refuses new
    /// work. Idempotent, and never waits. Claims the report: the caller
    /// asked for the stop, so no completion notice is wanted.
    pub fn kill(&self, id: JobId, reason: Option<String>) -> Result<KillResult, JobError> {
        self.request_kill(id, reason, true)
    }

    /// Requests that `id` stop for the user, as [`Self::kill`] does, with
    /// [`USER_STOP_REASON`], but claims no report: the job settles
    /// unreported, so the session gets its completion notice and the model
    /// learns of the stop. A terminal job is left as it is.
    pub fn stop(&self, id: JobId) -> Result<KillResult, JobError> {
        self.request_kill(id, Some(USER_STOP_REASON.to_owned()), false)
    }

    fn request_kill(
        &self,
        id: JobId,
        reason: Option<String>,
        claim: bool,
    ) -> Result<KillResult, JobError> {
        let gate = {
            let mut state = self.inner.state();
            let record = state.find(self.inner.id, id)?;
            if record.status.is_terminal() {
                if claim && record.claim != Claim::Claimed {
                    record.claim = self.inner.delivery_claim();
                }
                return Ok(KillResult::AlreadyFinished);
            }
            if claim {
                record.claim = Claim::Claimed;
            }
            if record.status != JobStatus::Running {
                return Ok(KillResult::AlreadyStopping);
            }
            record.status = JobStatus::Stopping;
            record.stop = Some(StopCause::Kill {
                reason: reason.map(|reason| cut(reason, MAX_TEXT_BYTES)),
            });
            record.gate.clone()
        };
        drop(gate.close());
        Ok(KillResult::Requested)
    }

    /// Waits up to `timeout` for `id` to become terminal, without stopping
    /// it. Returns the terminal snapshot, claiming its report, or the live
    /// one at the timeout. A `signal` aborted while the job is live fails
    /// the wait with [`JobError::WaitCancelled`]; once the job is terminal
    /// the terminal snapshot wins. The wait is unregistered however it
    /// ends, including when its future is dropped; a wait pending when the
    /// job settled holds the report for its caller, and a dropped one
    /// gives it back.
    pub async fn wait(
        &self,
        id: JobId,
        timeout: Duration,
        signal: Option<&AbortSignal>,
    ) -> Result<JobSnapshot, JobError> {
        let max = self.inner.limits.max_wait;
        if timeout.is_zero() || timeout > max {
            return Err(JobError::InvalidTimeout { max });
        }
        let wake = {
            let mut state = self.inner.state();
            let record = state.find(self.inner.id, id)?;
            if record.status.is_terminal() {
                if record.claim != Claim::Claimed {
                    record.claim = self.inner.delivery_claim();
                }
                return Ok(record.snapshot());
            }
            record.waiters += 1;
            Arc::clone(&record.wake)
        };
        let mut waiter = Waiter {
            inner: Arc::clone(&self.inner),
            id,
            registered: true,
        };
        let mut deadline = pin!(tokio::time::sleep(timeout));
        let cancelled = loop {
            let mut notified = pin!(wake.notified());
            // Registered before the check, so a settlement between the
            // check and the await is not missed.
            notified.as_mut().enable();
            if let Some(snapshot) = waiter.finish(false, true)? {
                return Ok(snapshot);
            }
            tokio::select! {
                biased;
                () = &mut notified => {}
                () = child_process::aborted(signal) => break true,
                () = &mut deadline => break false,
            }
        };
        waiter
            .finish(cancelled, false)?
            .ok_or(JobError::WaitCancelled(id))
    }

    /// Claims `id`'s report if it is terminal and unclaimed: true for the
    /// one caller that should report it.
    pub fn claim_report(&self, id: JobId) -> Result<bool, JobError> {
        let mut state = self.inner.state();
        let record = state.find(self.inner.id, id)?;
        let claimed = record.status.is_terminal() && record.claim == Claim::Unclaimed;
        if claimed {
            record.claim = self.inner.delivery_claim();
        }
        Ok(claimed)
    }

    /// Claims and returns every terminal job not yet reported, in
    /// admission order. Empty once the registry is closed.
    pub fn claim_unreported(&self) -> Vec<JobSnapshot> {
        let mut state = self.inner.state();
        if state.closed {
            return Vec::new();
        }
        state
            .records
            .iter_mut()
            .filter(|record| record.status.is_terminal() && record.claim == Claim::Unclaimed)
            .map(|record| {
                record.claim = self.inner.delivery_claim();
                record.snapshot()
            })
            .collect()
    }

    /// A subscription that wakes when a job may have become reportable, a
    /// notice was posted, or held live work changed. Dropping it
    /// unsubscribes.
    pub fn subscribe(&self) -> JobEvents {
        JobEvents {
            receiver: self.inner.events.subscribe(),
        }
    }

    /// Closes the registry when called, before the returned future is
    /// polled: refuses new jobs, closes subscriptions, claims every report
    /// (a session being torn down reads no notice), marks running jobs
    /// `Stopping` for [`StopCause::Shutdown`], and closes their gates. The
    /// future resolves once every job is terminal, which includes the
    /// settlement of its resources. Dropping the future stops nothing; the
    /// gate that owns the drivers waits for them either way.
    pub fn close(&self) -> impl Future<Output = ()> + Send + 'static {
        let gates: Vec<WorkGate> = {
            let mut state = self.inner.state();
            state.closed = true;
            state.mail.clear();
            state
                .records
                .iter_mut()
                .filter_map(|record| {
                    record.claim = Claim::Claimed;
                    if record.status.is_terminal() {
                        return None;
                    }
                    if record.status == JobStatus::Running {
                        record.status = JobStatus::Stopping;
                        record.stop.get_or_insert(StopCause::Shutdown);
                    }
                    Some(record.gate.clone())
                })
                .collect()
        };
        for gate in gates {
            drop(gate.close());
        }
        self.inner.events.send_modify(|events| events.closed = true);
        let inner = Arc::clone(&self.inner);
        async move {
            loop {
                let mut settled = pin!(inner.settled.notified());
                settled.as_mut().enable();
                if inner.state().active() == 0 {
                    return;
                }
                settled.await;
            }
        }
    }

    /// Queues `message`, a notice another agent addressed to this
    /// session, for delivery with the job notices: inside a run at its
    /// next step, else by a turn the session's owner opens within the
    /// wake budget ([`Self::has_undelivered`], [`Self::subscribe`]).
    /// Refused once the registry is closed.
    pub fn post(&self, message: CustomMessage) -> Result<(), StartError> {
        {
            let mut state = self.inner.state();
            if state.closed {
                return Err(StartError::Closed);
            }
            state.mail.push(message);
        }
        self.inner.announce();
        Ok(())
    }

    /// Takes every queued notice ([`Self::post`]), in arrival order.
    pub fn claim_mail(&self) -> Vec<CustomMessage> {
        std::mem::take(&mut self.inner.state().mail)
    }

    /// Whether a terminal job is unreported or a posted notice waits.
    pub fn has_undelivered(&self) -> bool {
        let state = self.inner.state();
        !state.mail.is_empty()
            || state
                .records
                .iter()
                .any(|record| record.status.is_terminal() && record.claim == Claim::Unclaimed)
    }

    /// Counts work outside the registry as live until the hold is dropped,
    /// so a session's owner waits for it as for a running job
    /// ([`Self::live`]). `None` once the registry is closed.
    pub fn hold_live(&self) -> Option<LiveHold> {
        {
            let mut state = self.inner.state();
            if state.closed {
                return None;
            }
            state.holds += 1;
        }
        self.inner.announce();
        Some(LiveHold {
            inner: Arc::clone(&self.inner),
        })
    }

    /// Live jobs and held work ([`Self::hold_live`]).
    pub fn live(&self) -> usize {
        let state = self.inner.state();
        state.active() + state.holds
    }

    /// Whether [`Self::close`] was called.
    pub fn is_closed(&self) -> bool {
        self.inner.state().closed
    }

    /// The waits registered on `id`.
    #[cfg(test)]
    fn waiters(&self, id: JobId) -> usize {
        self.inner
            .state()
            .find(self.inner.id, id)
            .map_or(0, |record| record.waiters)
    }
}

impl fmt::Debug for JobRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.inner.state();
        f.debug_struct("JobRegistry")
            .field("closed", &state.closed)
            .field("jobs", &state.records.len())
            .field("active", &state.active())
            .finish()
    }
}

/// Live work outside the registry ([`JobRegistry::hold_live`]); dropping
/// it releases the count and wakes subscribers.
pub struct LiveHold {
    inner: Arc<Inner>,
}

impl fmt::Debug for LiveHold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LiveHold")
    }
}

impl Drop for LiveHold {
    fn drop(&mut self) {
        {
            let mut state = self.inner.state();
            state.holds = state.holds.saturating_sub(1);
        }
        self.inner.announce();
    }
}

/// One registered wait; dropped unfinished, it unregisters and gives back
/// a report it was holding.
struct Waiter {
    inner: Arc<Inner>,
    id: JobId,
    registered: bool,
}

impl Waiter {
    /// Unregisters the wait if the job is terminal, claiming its report and
    /// returning its snapshot. Unless `only_terminal`, unregisters it
    /// anyway, returning the live snapshot for a timeout and `None` for a
    /// cancellation.
    fn finish(
        &mut self,
        cancelled: bool,
        only_terminal: bool,
    ) -> Result<Option<JobSnapshot>, JobError> {
        let mut state = self.inner.state();
        let record = state.find(self.inner.id, self.id)?;
        let terminal = record.status.is_terminal();
        if !terminal && only_terminal {
            return Ok(None);
        }
        self.registered = false;
        record.waiters = record.waiters.saturating_sub(1);
        if terminal {
            if record.claim != Claim::Claimed {
                record.claim = self.inner.delivery_claim();
            }
            return Ok(Some(record.snapshot()));
        }
        Ok((!cancelled).then(|| record.snapshot()))
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        if !self.registered {
            return;
        }
        let announce = {
            let mut state = self.inner.state();
            let closed = state.closed;
            let Ok(record) = state.find(self.inner.id, self.id) else {
                return;
            };
            record.waiters = record.waiters.saturating_sub(1);
            let released = record.waiters == 0 && record.claim == Claim::Waiters;
            if released {
                record.claim = Claim::Unclaimed;
            }
            state.evict(self.inner.limits.max_retained);
            released && !closed
        };
        if announce {
            self.inner.announce();
        }
    }
}

/// Wakes when a job may have become reportable: a job settled unclaimed,
/// a dropped wait gave a report back, a notice was posted, or held live
/// work began or ended. Dropping it unsubscribes.
#[derive(Debug)]
pub struct JobEvents {
    receiver: watch::Receiver<Events>,
}

impl JobEvents {
    /// Completes at the next change since the last call (or since
    /// subscribing): true for a change, false once the registry is closed
    /// or dropped. Cancel safe.
    pub async fn changed(&mut self) -> bool {
        match self.receiver.has_changed() {
            Err(_) => return false,
            Ok(true) => return !self.receiver.borrow_and_update().closed,
            Ok(false) if self.receiver.borrow().closed => return false,
            Ok(false) => {}
        }
        self.receiver.changed().await.is_ok() && !self.receiver.borrow_and_update().closed
    }
}

/// What a producer receives: its job's id, cancellation, gate, and output.
pub struct JobContext {
    id: JobId,
    scope: TaskScope,
    gate: WorkGate,
    output: Option<JobOutput>,
}

impl JobContext {
    /// The job.
    pub fn id(&self) -> JobId {
        self.id
    }

    /// Aborted when the job is killed, the registry closes, or the
    /// session's gate closes.
    pub fn signal(&self) -> &AbortSignal {
        self.scope.signal()
    }

    /// The producer's task scope: tasks spawned from its permit are the
    /// job's, cancelled with it and awaited before it is terminal.
    pub fn scope(&self) -> &TaskScope {
        &self.scope
    }

    /// The job's gate, to give tools as their owner: what they start under
    /// it is awaited before the job is terminal. Closed when the job is
    /// stopped or its producer returns.
    pub fn owner(&self) -> &WorkGate {
        &self.gate
    }

    /// The output of a stream job; `None` for a final-output job.
    pub fn output(&self) -> Option<&JobOutput> {
        self.output.as_ref()
    }

    /// Spawns `spec` as an owned process tree under the job's gate
    /// ([`bake_process::OwnedProcess`]): the job is terminal only once the
    /// process is settled, by the producer or, if dropped, by its settling
    /// task. Refused, with nothing spawned, once the job is stopping.
    pub async fn spawn_process(&self, spec: &ProcessSpec) -> io::Result<OwnedProcess> {
        child_process::spawn(spec, Some(&self.gate)).await
    }
}

impl fmt::Debug for JobContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobContext")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// A stream job's output sink. Cloning shares it.
#[derive(Clone)]
pub struct JobOutput {
    buffer: Arc<Mutex<StreamBuffer>>,
}

impl JobOutput {
    /// Appends `bytes`; false, with nothing kept, once the job is
    /// terminal. Past [`JobLimits::stream_bytes`] unread, the oldest bytes
    /// are dropped and counted.
    pub fn append(&self, bytes: &[u8]) -> bool {
        lock(&self.buffer).append(bytes)
    }
}

impl fmt::Debug for JobOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobOutput").finish_non_exhaustive()
    }
}

/// Unread streamed output, at most `cap` bytes: the newest are kept.
/// Beside it, the newest [`VIEW_TAIL_BYTES`] of all output, read or not,
/// for viewers.
struct StreamBuffer {
    bytes: Vec<u8>,
    dropped: u64,
    cap: usize,
    closed: bool,
    recent: Vec<u8>,
    total: u64,
}

/// The first char boundary of `text` that leaves at most `max` bytes after it.
fn tail_start(text: &str, max: usize) -> usize {
    let mut start = text.len().saturating_sub(max);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    start
}

/// The length of the sequence a UTF-8 lead byte starts, if it is one.
fn sequence_length(byte: u8) -> Option<usize> {
    match byte {
        0xC0..=0xDF => Some(2),
        0xE0..=0xEF => Some(3),
        0xF0..=0xF7 => Some(4),
        _ => None,
    }
}

impl StreamBuffer {
    fn new(cap: usize) -> Self {
        Self {
            bytes: Vec::new(),
            dropped: 0,
            cap,
            closed: false,
            recent: Vec::new(),
            total: 0,
        }
    }

    /// The newest output, read or not, as text, and the bytes before it.
    /// A cut through a character at the start drops the rest of it; an
    /// incomplete character at the end shows as a replacement.
    fn recent(&self) -> (String, u64) {
        let skip = self
            .recent
            .iter()
            .take(3)
            .take_while(|byte| **byte & 0xC0 == 0x80)
            .count();
        let shown = &self.recent[skip..];
        (
            String::from_utf8_lossy(shown).into_owned(),
            self.total - shown.len() as u64,
        )
    }

    fn append(&mut self, data: &[u8]) -> bool {
        if self.closed {
            return false;
        }
        self.total += data.len() as u64;
        let keep = data.len().min(VIEW_TAIL_BYTES);
        self.recent.extend_from_slice(&data[data.len() - keep..]);
        let excess = self.recent.len().saturating_sub(VIEW_TAIL_BYTES);
        self.recent.drain(..excess);
        if data.len() >= self.cap {
            self.dropped += (self.bytes.len() + data.len() - self.cap) as u64;
            self.bytes.clear();
            self.bytes.extend_from_slice(&data[data.len() - self.cap..]);
        } else {
            self.bytes.extend_from_slice(data);
            let excess = self.bytes.len().saturating_sub(self.cap);
            if excess > 0 {
                self.bytes.drain(..excess);
                self.dropped += excess as u64;
            }
        }
        // A cut that landed inside a character drops the rest of it too.
        if self.dropped > 0 {
            let partial = self
                .bytes
                .iter()
                .take(3)
                .take_while(|byte| **byte & 0xC0 == 0x80)
                .count();
            self.bytes.drain(..partial);
            self.dropped += partial as u64;
        }
        true
    }

    /// The unread text and the bytes dropped since the last take. Unless
    /// `flush`, an incomplete trailing character stays for the next take.
    fn take(&mut self, flush: bool) -> (String, u64) {
        let mut end = self.bytes.len();
        if !flush {
            for back in 1..=end.min(3) {
                let byte = self.bytes[end - back];
                if let Some(length) = sequence_length(byte) {
                    if length > back {
                        end -= back;
                    }
                    break;
                }
                if byte & 0xC0 != 0x80 {
                    break;
                }
            }
        }
        let text = String::from_utf8_lossy(&self.bytes[..end]).into_owned();
        self.bytes.drain(..end);
        (text, std::mem::take(&mut self.dropped))
    }
}

/// A job's driver, a task of the session's gate: runs the producer under
/// the job's gate, stops it if the session's gate closes, then closes the
/// job's gate and waits for everything in it before recording the
/// outcome.
async fn drive<F, Fut>(
    inner: Arc<Inner>,
    id: JobId,
    gate: WorkGate,
    output: Option<JobOutput>,
    scope: TaskScope,
    producer: F,
) where
    F: FnOnce(JobContext) -> Fut + Send + 'static,
    Fut: Future<Output = JobOutcome> + Send + 'static,
{
    let started = inner
        .state()
        .find(inner.id, id)
        .map(|record| record.snapshot());
    let admission = match started {
        Ok(snapshot) => inner.record(snapshot).await,
        Err(error) => Err(error.to_string()),
    };
    inner.admitted(id, admission.clone());
    if let Err(error) = admission {
        gate.close().await;
        inner.journal_failed(id, error);
        return;
    }
    if scope.signal().aborted() {
        inner.request_stop(id, StopCause::Shutdown);
    }
    let context_gate = gate.clone();
    let produced = gate.spawn(move |task_scope| {
        producer(JobContext {
            id,
            scope: task_scope,
            gate: context_gate,
            output,
        })
    });
    let outcome = match produced {
        // Stopped before this task first ran: the producer never runs.
        Err(_) => JobOutcome::killed("stopped before it started"),
        Ok(task) => {
            let mut task = pin!(task);
            let joined = tokio::select! {
                joined = &mut task => joined,
                () = scope.signal().cancelled() => {
                    inner.request_stop(id, StopCause::Shutdown);
                    task.await
                }
            };
            joined.unwrap_or_else(|error| {
                JobOutcome::failed(format!("the job's task failed: {error}"))
            })
        }
    };
    inner.producer_returned(id);
    gate.close().await;
    let terminal = inner.state().find(inner.id, id).map(|record| {
        let mut snapshot = record.snapshot();
        snapshot.status = outcome.status.into();
        snapshot.detail = outcome
            .detail
            .clone()
            .map(|detail| cut(detail, MAX_TEXT_BYTES));
        snapshot.finished_at = Some(bake_ai::now_ms());
        snapshot
    });
    if let Ok(snapshot) = terminal
        && let Err(error) = inner.record(snapshot).await
    {
        inner.journal_failed(id, error);
        return;
    }
    inner.settle(id, outcome);
}

#[cfg(test)]
mod tests;
