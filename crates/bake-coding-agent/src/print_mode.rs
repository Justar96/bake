//! Single-shot print mode: send the prompts, write the result, exit.
//!
//! Ported from Pi `packages/coding-agent/src/modes/print-mode.ts` (v1.1.0),
//! with v1.4.0's check of a response cut off at the model's output limit
//! (revision `4a080252f06caf63a525d7873c5d94d3c23887e0`, issue #10799).
//! Text mode writes the final assistant message's text blocks, one per
//! line, or its error to standard error with exit code 1; a cut-off
//! response is written, then reported on standard error, also with 1. JSON
//! mode writes the session header and then every session event as one JSON
//! line ([`crate::json_event`]). Either way the session is shut down before
//! returning. Pending speculative summaries publish their terminal accounting
//! before shutdown closes observers; failed required accounting exits with 1.
//!
//! Bake's own: the result object other agent command lines end a headless
//! run with (Claude Code's and Gemini CLI's `--output-format json`), so a
//! program that runs `bake` reads one record instead of the event stream
//! or the answer's text. [`PrintOutput::Result`] writes only it, and
//! [`PrintOutput::StreamJson`] writes JSON mode's lines and then it. It
//! says how the run ended, gives the answer, names the session to
//! continue, and counts this invocation's turns, model calls, time, and
//! usage; both exit as text mode does. A startup failure after the
//! arguments were read writes one too ([`startup_failure_json`]), with the
//! errors standard error got ([`capture_errors`]); an argument error or a
//! stop request writes none.
//!
//! Pi's extension binding, session replacement, and its own signal handlers
//! are not ported; the binary handles `SIGTERM` and `SIGHUP`. JSON lines go
//! to the writer from inside the event dispatch, so a slow reader slows the
//! run, as Pi's backpressure wait does.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use bake_agent::{AgentEvent, RunControl, RunOutcome};
use bake_ai::{AssistantContentBlock, AssistantMessage, ImageContent, StopReason};
use serde_json::{Value, json};

use crate::agent_session::{AgentSession, RecoverableRunResult, SessionEvent, UsageTotals};
use crate::json_event::session_event_json;

/// Pi's print output modes, and Bake's two that end with the result
/// object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintOutput {
    /// The final response only.
    Text,
    /// Every event as a JSON line.
    Json,
    /// Bake's: one result object when the run ends (`--output-format
    /// json`).
    Result,
    /// Bake's: [`Self::Json`]'s lines, then the result object
    /// (`--output-format stream-json`).
    StreamJson,
}

impl PrintOutput {
    /// Whether the session header and every event are written as JSON
    /// lines.
    pub fn streams_events(self) -> bool {
        matches!(self, Self::Json | Self::StreamJson)
    }

    /// Whether the run ends with the result object.
    pub fn writes_result(self) -> bool {
        matches!(self, Self::Result | Self::StreamJson)
    }
}

/// The result object's `subtype` for a run whose turns finished with an
/// answer.
pub const SUCCESS: &str = "success";
/// The `subtype` for a model or runtime failure.
pub const ERROR: &str = "error";
/// The `subtype` for an answer cut off at the model's output limit.
pub const TRUNCATED: &str = "truncated";
/// The `subtype` for a run its owner cancelled.
pub const CANCELLED: &str = "cancelled";
/// The `subtype` for a run `--max-model-calls` or `--run-timeout-ms`
/// stopped.
pub const BUDGET_EXHAUSTED: &str = "budget_exhausted";

/// Pi v1.4.0's report of a response cut off at the output limit.
pub const TRUNCATED_MESSAGE: &str =
    "Response was truncated: the model reached its output token limit.";

/// The most startup error text [`capture_errors`] keeps.
const MAX_CAPTURED_ERRORS: usize = 64 * 1024;

/// A writer shared with the event listener.
pub type SharedWriter = Arc<Mutex<dyn Write + Send>>;

/// Explicit selection of a persistent run interval in print mode.
#[derive(Debug, Clone, Default)]
pub enum PrintRunRecovery {
    /// Ordinary prompts sharing this invocation's control.
    #[default]
    Disabled,
    /// Start one journaled prompt with the control's original limits.
    Start,
    /// Continue only this recorded run on the selected session branch.
    Resume(String),
}

/// Pi's `PrintModeOptions`.
#[derive(Debug, Clone)]
pub struct PrintModeOptions {
    /// One shared admission budget across every prompt in this invocation,
    /// including retries, continuations, and compaction summaries.
    pub control: RunControl,
    /// Durable recovery is disabled unless explicitly selected.
    pub recovery: PrintRunRecovery,
    /// The output mode.
    pub mode: PrintOutput,
    /// The first prompt, with stdin prepended.
    pub initial_message: Option<String>,
    /// Images for the first prompt.
    pub initial_images: Vec<ImageContent>,
    /// Further prompts, each sent after the previous one settles.
    pub messages: Vec<String>,
    /// How long, after the last prompt, to wait for background jobs and
    /// the turns their notices open ([`DEFAULT_JOB_WAIT`]). Recoverable
    /// starts persist this policy; resume uses the selected run's policy.
    pub job_wait: Duration,
}

/// How long print mode waits for background work by default: ten minutes.
pub const DEFAULT_JOB_WAIT: Duration = Duration::from_secs(600);

/// Writes `text`, ignoring a reader that has gone away, as Pi's raw stdout
/// writer does.
pub fn write_out(writer: &SharedWriter, text: &str) {
    let mut writer = writer.lock().unwrap_or_else(PoisonError::into_inner);
    if writer.write_all(text.as_bytes()).is_ok() {
        let _ = writer.flush();
    }
}

/// Pi's `runPrintMode`: returns the exit code. The session is shut down
/// before this returns.
pub async fn run_print_mode(
    session: &AgentSession,
    options: PrintModeOptions,
    stdout: SharedWriter,
    stderr: SharedWriter,
) -> i32 {
    let started = Instant::now();
    let before = session.usage_totals();
    let control = options.control.clone();
    let mode = options.mode;
    let watch = Watch::start(session);
    let ended = run(session, options, &stdout, &stderr).await;
    session.shutdown().await;
    let (turns, run_id) = watch.finish();
    let ending = Ending::of(ended, session);
    match mode {
        PrintOutput::Text => {
            if matches!(ending.subtype, SUCCESS | TRUNCATED) {
                write_text(&stdout, ending.final_message.as_ref());
            }
            if let Some(error) = &ending.error {
                write_out(&stderr, &format!("{error}\n"));
            }
        }
        // Pi's JSON mode reports only a run that stopped short; the events
        // carry the rest.
        PrintOutput::Json => {
            if ending.stopped
                && let Some(error) = &ending.error
            {
                write_out(&stderr, &format!("{error}\n"));
            }
            return i32::from(ending.stopped);
        }
        PrintOutput::Result | PrintOutput::StreamJson => {
            if let Some(error) = &ending.error {
                write_out(&stderr, &format!("{error}\n"));
            }
            let after = session.usage_totals();
            let facts = Facts {
                session_id: Some(session.session_id()),
                session_file: session
                    .session_file()
                    .map(|path| path.to_string_lossy().into_owned()),
                run_id,
                model: session
                    .model()
                    .map(|model| format!("{}/{}", model.provider, model.id)),
                turns,
                model_calls: control.model_calls(),
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                spent: Spent::between(&before, &after),
            };
            write_out(&stdout, &format!("{}\n", result_json(&ending, &facts)));
        }
    }
    ending.code()
}

/// Writes the answer's text blocks, one per line, as Pi's text mode does.
fn write_text(stdout: &SharedWriter, message: Option<&AssistantMessage>) {
    for block in message.into_iter().flat_map(|message| &message.content) {
        if let AssistantContentBlock::Text(text) = block {
            write_out(stdout, &format!("{}\n", text.text));
        }
    }
}

/// The text of the answer's text blocks, joined by newlines.
fn answer_text(message: Option<&AssistantMessage>) -> String {
    message
        .into_iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            AssistantContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The session's last assistant message, passing over the notes a run may
/// end with (a `Stop` hook cap).
fn last_assistant(session: &AgentSession) -> Option<AssistantMessage> {
    session
        .messages()
        .iter()
        .rev()
        .find(|message| message.as_custom().is_none())
        .and_then(|message| message.as_assistant().cloned())
}

/// Why a run stopped before its turns finished: the result object's
/// `subtype` and the message standard error gets.
#[derive(Debug)]
struct Stopped {
    subtype: &'static str,
    message: String,
}

impl From<String> for Stopped {
    fn from(message: String) -> Self {
        Self {
            subtype: ERROR,
            message,
        }
    }
}

impl From<&str> for Stopped {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

impl From<RunOutcome> for Stopped {
    fn from(outcome: RunOutcome) -> Self {
        let subtype = match outcome {
            RunOutcome::Cancelled => CANCELLED,
            RunOutcome::BudgetExhausted { .. } => BUDGET_EXHAUSTED,
            RunOutcome::FinishedTurn | RunOutcome::Failed { .. } => ERROR,
        };
        Self {
            subtype,
            message: outcome.to_string(),
        }
    }
}

/// How a run ended, as text mode reports it and the result object records
/// it.
#[derive(Debug)]
struct Ending {
    subtype: &'static str,
    /// What standard error gets, if anything.
    error: Option<String>,
    /// Whether the run stopped before its turns finished, rather than
    /// ending with an answer that failed or was cut off.
    stopped: bool,
    final_message: Option<AssistantMessage>,
}

impl Ending {
    fn of(ended: Result<Option<AssistantMessage>, Stopped>, session: &AgentSession) -> Self {
        let final_message = match ended {
            Ok(final_message) => final_message,
            Err(stopped) => {
                return Self {
                    subtype: stopped.subtype,
                    error: Some(stopped.message),
                    stopped: true,
                    final_message: last_assistant(session),
                };
            }
        };
        let (subtype, error) = match final_message.as_ref() {
            Some(message)
                if matches!(message.stop_reason, StopReason::Error | StopReason::Aborted) =>
            {
                (
                    ERROR,
                    Some(
                        message
                            .error_message
                            .clone()
                            .filter(|text| !text.is_empty())
                            .unwrap_or_else(|| format!("Request {}", message.stop_reason.as_str())),
                    ),
                )
            }
            Some(message) if message.stop_reason == StopReason::Length => {
                (TRUNCATED, Some(TRUNCATED_MESSAGE.to_owned()))
            }
            _ => (SUCCESS, None),
        };
        Self {
            subtype,
            error,
            stopped: false,
            final_message,
        }
    }

    /// Text mode's exit code, which the result modes share.
    fn code(&self) -> i32 {
        i32::from(self.subtype != SUCCESS)
    }
}

/// What the result object reports besides how the run ended.
#[derive(Debug, Default)]
struct Facts {
    session_id: Option<String>,
    session_file: Option<String>,
    run_id: Option<String>,
    model: Option<String>,
    turns: u64,
    model_calls: u64,
    duration_ms: u64,
    spent: Spent,
}

/// This invocation's usage: the session's totals after it, less those
/// before ([`AgentSession::usage_totals`]).
#[derive(Debug, Default, PartialEq)]
struct Spent {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    cost: f64,
}

impl Spent {
    fn between(before: &UsageTotals, after: &UsageTotals) -> Self {
        let cost = after.cost - before.cost;
        Self {
            input: after.input.saturating_sub(before.input),
            output: after.output.saturating_sub(before.output),
            cache_read: after.cache_read.saturating_sub(before.cache_read),
            cache_write: after.cache_write.saturating_sub(before.cache_write),
            cost: if cost.is_finite() && cost > 0.0 {
                cost
            } else {
                0.0
            },
        }
    }
}

/// Bake's result object, one line of JSON: how the run ended (`subtype`
/// and `isError`), the answer's text (`result`), what failed (`error`),
/// the last answer's `stopReason`, the session to continue (`sessionId`,
/// and `sessionFile` unless `--no-session`), a `--recoverable` run's
/// `runId`, the `model` as `provider/id`, and this invocation's
/// `numTurns`, `modelCalls`, `durationMs`, `usage`, and `costUsd`. Every
/// member is present; what does not apply is `null`.
fn result_json(ending: &Ending, facts: &Facts) -> Value {
    let spent = &facts.spent;
    json!({
        "type": "result",
        "subtype": ending.subtype,
        "isError": ending.subtype != SUCCESS,
        "result": answer_text(ending.final_message.as_ref()),
        "error": ending.error,
        "stopReason": ending
            .final_message
            .as_ref()
            .map(|message| message.stop_reason.as_str()),
        "sessionId": facts.session_id,
        "sessionFile": facts.session_file,
        "runId": facts.run_id,
        "model": facts.model,
        "numTurns": facts.turns,
        "modelCalls": facts.model_calls,
        "durationMs": facts.duration_ms,
        "usage": {
            "input": spent.input,
            "output": spent.output,
            "cacheRead": spent.cache_read,
            "cacheWrite": spent.cache_write,
            "totalTokens": spent
                .input
                .saturating_add(spent.output)
                .saturating_add(spent.cache_read)
                .saturating_add(spent.cache_write),
        },
        "costUsd": spent.cost,
    })
}

/// The result object of a run that failed before its session started,
/// with `error` the errors standard error got.
pub fn startup_failure_json(error: &str) -> String {
    let ending = Ending {
        subtype: ERROR,
        error: Some(error.trim().to_owned()),
        stopped: true,
        final_message: None,
    };
    format!("{}\n", result_json(&ending, &Facts::default()))
}

/// Wraps `stderr` so it also keeps what is written to it except warnings:
/// each `write_out` is one record, and a `Warning: ` record is left out and
/// an `Error: ` prefix dropped. Up to 64 KiB is kept, for
/// [`startup_failure_json`].
pub fn capture_errors(stderr: SharedWriter) -> (SharedWriter, Arc<Mutex<String>>) {
    let kept = Arc::new(Mutex::new(String::new()));
    let writer = Captured {
        inner: stderr,
        kept: Arc::clone(&kept),
    };
    (Arc::new(Mutex::new(writer)), kept)
}

struct Captured {
    inner: SharedWriter,
    kept: Arc<Mutex<String>>,
}

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let text = String::from_utf8_lossy(bytes);
        if !text.starts_with("Warning: ") {
            let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
            if kept.len() < MAX_CAPTURED_ERRORS {
                kept.push_str(text.strip_prefix("Error: ").unwrap_or(&text));
            }
        }
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .write_all(bytes)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .flush()
    }
}

/// Counts the run's turns and keeps a `--recoverable` run's id, for the
/// result object.
struct Watch {
    turns: Arc<AtomicU64>,
    run_id: Arc<Mutex<Option<String>>>,
    subscription: crate::agent_session::SessionSubscription,
}

impl Watch {
    fn start(session: &AgentSession) -> Self {
        let turns = Arc::new(AtomicU64::new(0));
        let run_id = Arc::new(Mutex::new(None));
        let subscription = session.subscribe({
            let turns = Arc::clone(&turns);
            let run_id = Arc::clone(&run_id);
            move |event| match event {
                SessionEvent::Agent(AgentEvent::TurnEnd { .. }) => {
                    turns.fetch_add(1, Ordering::Relaxed);
                }
                SessionEvent::RecoverableRunStarted { run_id: id } => {
                    *run_id.lock().unwrap_or_else(PoisonError::into_inner) = Some(id.clone());
                }
                _ => {}
            }
        });
        Self {
            turns,
            run_id,
            subscription,
        }
    }

    fn finish(self) -> (u64, Option<String>) {
        self.subscription.unsubscribe();
        let run_id = self
            .run_id
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        (self.turns.load(Ordering::Relaxed), run_id)
    }
}

async fn run(
    session: &AgentSession,
    options: PrintModeOptions,
    stdout: &SharedWriter,
    stderr: &SharedWriter,
) -> Result<Option<AssistantMessage>, Stopped> {
    let subscription = if options.mode.streams_events() {
        if let Some(header) = session.header_json() {
            write_out(stdout, &format!("{header}\n"));
        }
        let out = Arc::clone(stdout);
        Some(session.subscribe(move |event| {
            write_out(&out, &format!("{}\n", session_event_json(event)));
        }))
    } else {
        let out = Arc::clone(stderr);
        let file = session.session_file();
        let recoverable = !matches!(options.recovery, PrintRunRecovery::Disabled);
        Some(session.subscribe(move |event| match event {
            SessionEvent::RecoverableRunStarted { run_id } if recoverable => {
                write_out(&out, &format!("Run ID: {run_id}\n"));
                if let Some(file) = &file {
                    write_out(&out, &format!("Session: {file:?}\n"));
                }
            }
            // A context file found while working that was left out.
            SessionEvent::NestedContext { warnings, .. } => {
                for warning in *warnings {
                    write_out(&out, &format!("Warning: {warning}\n"));
                }
            }
            _ => {}
        }))
    };
    let result = prompts(session, options.clone(), stderr).await;
    // Keep observers through owned summary and shutdown persistence.
    let background = session.finish_background_compaction(&options.control).await;
    session.shutdown().await;
    if let Some(subscription) = subscription {
        subscription.unsubscribe();
    }
    if let Some(error) = session.persistence_failure() {
        return Err(error.into());
    }
    require_finished(background)?;
    if let Some(result) = result? {
        if options.mode.streams_events() {
            write_out(
                stdout,
                &format!(
                    "{}\n",
                    serde_json::json!({
                        "type": "recoverable_run_result",
                        "runId": result.run_id,
                        "outcome": result.outcome,
                        "finalMessage": result.final_message,
                    })
                ),
            );
        }
        require_finished(result.outcome)?;
        Ok(result.final_message)
    } else {
        // A note a run ended with (a `Stop` hook cap) may follow the answer.
        Ok(last_assistant(session))
    }
}

async fn prompts(
    session: &AgentSession,
    options: PrintModeOptions,
    stderr: &SharedWriter,
) -> Result<Option<RecoverableRunResult>, Stopped> {
    match options.recovery {
        PrintRunRecovery::Start => {
            if !options.messages.is_empty() {
                return Err("--recoverable accepts one prompt per invocation.".into());
            }
            let initial = options
                .initial_message
                .filter(|text| !text.trim().is_empty())
                .ok_or_else(|| "--recoverable requires one nonempty prompt.".to_owned())?;
            let result = session
                .prompt_recoverable_with_job_wait(
                    &initial,
                    options.initial_images,
                    options.control.limits(),
                    options.job_wait,
                )
                .await?;
            report_remaining_recoverable_jobs(session, stderr);
            return Ok(Some(result));
        }
        PrintRunRecovery::Resume(run_id) => {
            if options.initial_message.is_some()
                || !options.initial_images.is_empty()
                || !options.messages.is_empty()
            {
                return Err("--resume-run takes no new prompt, piped input, or files.".into());
            }
            let result = session.resume_run(&run_id).await?;
            report_remaining_recoverable_jobs(session, stderr);
            return Ok(Some(result));
        }
        PrintRunRecovery::Disabled => {}
    }
    if let Some(initial) = options.initial_message.filter(|text| !text.is_empty()) {
        let outcome = session
            .prompt_with_control(&initial, options.initial_images, options.control.clone())
            .await?;
        require_finished(outcome)?;
    }
    for message in options.messages {
        let outcome = session
            .prompt_with_control(&message, Vec::new(), options.control.clone())
            .await?;
        require_finished(outcome)?;
    }
    let left = settle_jobs(session, options.job_wait, options.control.clone()).await?;
    // The team is left before the last look at the notices, so nothing a
    // member is told was accepted goes unread. What arrived meanwhile, or
    // was held past the wake budget, gets one more turn.
    if let Some(Ok(station)) = session.telecom() {
        station.leave();
        let jobs = session.jobs();
        jobs.release_held();
        if left == 0 && jobs.has_undelivered() {
            let (outcome, _) = session
                .deliver_job_notices_controlled(options.control)
                .await?;
            require_finished(outcome)?;
        }
    }
    if left > 0 {
        // An open ask holds the live count only when telecom is on.
        let what = match session.telecom() {
            Some(Ok(_)) => "background job(s), subagent run(s), or unanswered ask(s) still open",
            _ => "background job(s) or subagent run(s) still running",
        };
        write_out(
            stderr,
            &format!(
                "{left} {what} after {} s were stopped\n",
                options.job_wait.as_secs()
            ),
        );
    }
    Ok(None)
}

fn report_remaining_recoverable_jobs(session: &AgentSession, stderr: &SharedWriter) {
    let live = session.jobs().live();
    if live > 0 {
        write_out(
            stderr,
            &format!(
                "{live} background job(s) or subagent run(s) still running at the end of the recoverable run were stopped\n"
            ),
        );
    }
}

/// Waits, up to `limit`, for the
/// session's background jobs and subagent runs and for the turns their
/// notices open, so the answer printed is the last turn's. Returns how
/// many were still running at the limit; the session's shutdown stops
/// them.
/// A notice turn still running at the limit is aborted and awaited,
/// including its cleanup, before this returns.
async fn settle_jobs(
    session: &AgentSession,
    limit: Duration,
    control: RunControl,
) -> Result<usize, Stopped> {
    let deadline = tokio::time::Instant::now() + limit;
    let jobs = session.jobs().clone();
    // Subscribed before the first check, so no settlement is missed.
    let mut events = jobs.subscribe();
    // Running subagents hold the registry's live count.
    let live_count = || jobs.live();
    loop {
        if let Some(outcome) = control.stopped() {
            return Err(outcome.into());
        }
        // Counted before the delivery, so a job that settles after the
        // delivery's claim is still waited for and delivered next time.
        let live = live_count();
        // In particular, a zero budget must not admit a notice turn.
        if tokio::time::Instant::now() >= deadline {
            return Ok(live);
        }
        let delivery = session.deliver_job_notices_controlled(control.clone());
        tokio::pin!(delivery);
        let mut delivery_polled = false;
        let delivered = {
            let poll_delivery = std::future::poll_fn(|cx| {
                delivery_polled = true;
                delivery.as_mut().poll(cx)
            });
            tokio::select! {
                biased;
                () = tokio::time::sleep_until(deadline) => None,
                _ = control.wait_stopped() => None,
                result = poll_delivery => Some(result?),
            }
        };
        let Some(delivered) = delivered else {
            let shared_stop = control.stopped();
            // If the deadline won before delivery's first poll, there is
            // no admitted turn to drain and polling it would start one.
            if delivery_polled {
                // Keep polling while abort waits for the delivery's run
                // guard and owned request to settle.
                let ((), result) = tokio::join!(session.abort(), &mut delivery);
                let (outcome, _) = result?;
                if matches!(outcome, RunOutcome::Failed { .. }) {
                    return Err(outcome.into());
                }
            }
            if let Some(outcome) = shared_stop {
                return Err(outcome.into());
            }
            return Ok(live_count());
        };
        require_finished(delivered.0)?;
        if delivered.1 {
            continue;
        }
        // Every settled job's notice was delivered, or waits for a prompt
        // this run will not send.
        if live == 0 {
            return Ok(0);
        }
        tokio::select! {
            changed = events.changed() => if !changed {
                return Ok(0);
            },
            () = tokio::time::sleep_until(deadline) => return Ok(live_count()),
            outcome = control.wait_stopped() => return Err(outcome.into()),
        }
    }
}

fn require_finished(outcome: RunOutcome) -> Result<(), Stopped> {
    match outcome {
        RunOutcome::FinishedTurn => Ok(()),
        other => Err(other.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn totals(input: u64, output: u64, read: u64, write: u64, cost: f64) -> UsageTotals {
        UsageTotals {
            input,
            output,
            cache_read: read,
            cache_write: write,
            cost,
            latest_cache_hit: None,
        }
    }

    // A continued session's earlier usage is not this invocation's.
    #[test]
    fn the_usage_spent_is_the_difference_and_never_negative() {
        assert_eq!(
            Spent::between(&totals(10, 5, 100, 2, 0.5), &totals(15, 9, 180, 2, 0.75)),
            Spent {
                input: 5,
                output: 4,
                cache_read: 80,
                cache_write: 0,
                cost: 0.25,
            }
        );
        assert_eq!(
            Spent::between(&totals(10, 5, 0, 0, 1.0), &totals(1, 1, 0, 0, f64::NAN)),
            Spent::default()
        );
    }

    #[test]
    fn a_startup_failure_has_every_member_and_the_errors() {
        let line = startup_failure_json("Model \"x\" not found.\n");
        assert!(line.ends_with('\n') && !line.trim_end().contains('\n'));
        let result: Value = serde_json::from_str(&line).expect("JSON");
        assert_eq!(result["type"], "result");
        assert_eq!(result["subtype"], ERROR);
        assert_eq!(result["isError"], true);
        assert_eq!(result["error"], "Model \"x\" not found.");
        assert_eq!(result["result"], "");
        for member in ["stopReason", "sessionId", "sessionFile", "runId", "model"] {
            assert_eq!(result[member], Value::Null, "{member}");
        }
        assert_eq!(result["numTurns"], 0);
        assert_eq!(result["usage"]["totalTokens"], 0);
    }

    #[test]
    fn captured_errors_leave_out_warnings_and_the_error_prefix() {
        let bytes = Arc::new(Mutex::new(Vec::<u8>::new()));
        let (stderr, kept) = capture_errors(bytes.clone());
        let records = [
            "Warning: errors loading models.json:\nline 3\n",
            "Error: --provider requires --model\n",
            "No session found matching 'x'\n",
        ];
        for record in records {
            write_out(&stderr, record);
        }
        assert_eq!(
            kept.lock().expect("kept").as_str(),
            "--provider requires --model\nNo session found matching 'x'\n"
        );
        // Standard error still gets every record.
        assert_eq!(
            String::from_utf8_lossy(&bytes.lock().expect("stderr")),
            records.concat()
        );
    }

    // With telecom on, an ask sent without waiting keeps print mode until
    // its answer arrives, which opens one more turn; then the session
    // leaves the team.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn print_mode_waits_for_an_open_asks_answer() {
        use crate::agent_session::tests::{harness, options};
        use crate::jobs::{JobLimits, JobRegistry};
        use crate::owned_work::WorkGate;
        use crate::telecom::{Station, StationOptions};
        use bake_ai::StopReason;
        use bake_ai::providers::faux::{
            faux_assistant_blocks, faux_assistant_message, faux_tool_call,
        };

        let dir = crate::test_support::TempDir::new("print-telecom");
        let ws = dir.join("ws");
        std::fs::create_dir_all(&ws).expect("ws");
        let station = |session: &str, jobs: &JobRegistry, work: &WorkGate| {
            let options = StationOptions {
                root: dir.join("telecom"),
                cwd: ws.clone(),
                session_id: session.into(),
                callsign: None,
                parent: None,
                model: None,
            };
            Station::start(options, jobs.clone(), work).expect("station")
        };
        let work = WorkGate::default();
        let jobs = JobRegistry::new(work.clone(), JobLimits::default());
        let alpha = station("s-alpha", &jobs, &work);
        let bravo_work = WorkGate::default();
        let bravo_jobs = JobRegistry::new(bravo_work.clone(), JobLimits::default());
        let bravo = station("s-bravo", &bravo_jobs, &bravo_work);

        let h = harness();
        h.faux.set_responses(vec![
            faux_assistant_blocks(
                vec![faux_tool_call(
                    "telecom",
                    serde_json::json!({ "action": "ask", "to": "bravo", "message": "ready?", "wait": false }),
                    Some("t1"),
                )],
                StopReason::ToolUse,
            )
            .into(),
            faux_assistant_message("asked").into(),
            faux_assistant_message("bravo is ready").into(),
        ]);
        let store = crate::session::SessionManager::create(
            &ws.to_string_lossy(),
            Some(&dir.join("sessions")),
            crate::session::NewSessionOptions::default(),
        )
        .expect("store");
        let mut session_options = options(&h, store, &ws.to_string_lossy());
        session_options.tools = vec![Arc::new(crate::telecom::tool::tool(alpha.clone()).tool)];
        session_options.active_tool_names = vec!["telecom".into()];
        session_options.work = Some(work);
        session_options.jobs = Some(jobs);
        let mut session = AgentSession::create(session_options)
            .await
            .expect("session");
        session.set_telecom(Ok(alpha.clone()));

        // Bravo broadcasts and answers once the prompt's run has made its
        // last request, so only print mode's wait for the open ask can take
        // the answer, and the broadcast rides on the turn it opens.
        let answer = {
            let bravo = bravo.clone();
            let faux = Arc::clone(&h.faux);
            tokio::spawn(async move {
                while bravo.owed().is_empty() || faux.call_count() < 2 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                bravo.broadcast("main is frozen").await.expect("broadcast");
                bravo.reply("yes", None, None).await
            })
        };
        let stdout = Arc::new(Mutex::new(Vec::<u8>::new()));
        let code = tokio::time::timeout(
            Duration::from_secs(30),
            run_print_mode(
                &session,
                PrintModeOptions {
                    control: RunControl::default(),
                    recovery: PrintRunRecovery::Disabled,
                    mode: PrintOutput::Text,
                    initial_message: Some("check with bravo".into()),
                    initial_images: Vec::new(),
                    messages: Vec::new(),
                    job_wait: Duration::from_secs(30),
                },
                Arc::clone(&stdout) as SharedWriter,
                Arc::new(Mutex::new(Vec::<u8>::new())),
            ),
        )
        .await
        .expect("print mode returns");
        assert_eq!(code, 0);
        assert!(answer.await.expect("answered").is_ok());
        let printed = String::from_utf8(stdout.lock().unwrap().clone()).expect("utf-8");
        assert_eq!(printed, "bravo is ready\n");
        let transcript = format!("{:?}", session.messages());
        assert!(
            transcript.contains("bravo answered your ask"),
            "{transcript}"
        );
        assert!(
            transcript.contains("[telecom] New broadcasts:\\n- bravo: main is frozen"),
            "{transcript}"
        );
        assert!(bravo.peers().is_empty(), "print mode left the team");
        session.shutdown().await;
        bravo_work.close().await;
    }
}
