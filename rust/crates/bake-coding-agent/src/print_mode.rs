//! Single-shot print mode: send the prompts, write the result, exit.
//!
//! Ported from Pi `packages/coding-agent/src/modes/print-mode.ts` (v1.1.0).
//! Text mode writes the final assistant message's text blocks, one per
//! line, or its error to standard error with exit code 1; JSON mode writes
//! the session header and then every session event as one JSON line
//! ([`crate::json_event`]). Either way the session is shut down before
//! returning.
//!
//! Pi's extension binding, session replacement, and its own signal handlers
//! are not ported; the binary handles `SIGTERM` and `SIGHUP`. JSON lines go
//! to the writer from inside the event dispatch, so a slow reader slows the
//! run, as Pi's backpressure wait does.

use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bake_agent::{RunControl, RunOutcome};
use bake_ai::{AssistantContentBlock, ImageContent, StopReason};

use crate::agent_session::AgentSession;
use crate::json_event::session_event_json;

/// Pi's print output modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintOutput {
    /// The final response only.
    Text,
    /// Every event as a JSON line.
    Json,
}

/// A writer shared with the event listener.
pub type SharedWriter = Arc<Mutex<dyn Write + Send>>;

/// Pi's `PrintModeOptions`.
#[derive(Debug, Clone)]
pub struct PrintModeOptions {
    /// One shared admission budget across every prompt in this invocation,
    /// including retries, continuations, and compaction summaries.
    pub control: RunControl,
    /// The output mode.
    pub mode: PrintOutput,
    /// The first prompt, with stdin prepended.
    pub initial_message: Option<String>,
    /// Images for the first prompt.
    pub initial_images: Vec<ImageContent>,
    /// Further prompts, each sent after the previous one settles.
    pub messages: Vec<String>,
    /// How long, after the last prompt, to wait for background jobs and
    /// the turns their notices open ([`DEFAULT_JOB_WAIT`]).
    pub job_wait: Duration,
}

/// Bake 0.3 headless `jobWaitMs`'s default: ten minutes.
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
    let result = run(session, options, &stdout, &stderr).await;
    session.shutdown().await;
    match result {
        Ok(code) => code,
        Err(message) => {
            write_out(&stderr, &format!("{message}\n"));
            1
        }
    }
}

async fn run(
    session: &AgentSession,
    options: PrintModeOptions,
    stdout: &SharedWriter,
    stderr: &SharedWriter,
) -> Result<i32, String> {
    let subscription = if options.mode == PrintOutput::Json {
        if let Some(header) = session.header_json() {
            write_out(stdout, &format!("{header}\n"));
        }
        let out = Arc::clone(stdout);
        Some(session.subscribe(move |event| {
            write_out(&out, &format!("{}\n", session_event_json(event)));
        }))
    } else {
        None
    };
    let result = prompts(session, options.clone(), stderr).await;
    // Paid summary attempts and job terminal writes settle during shutdown;
    // keep their event listener attached until the owned work has drained.
    session.shutdown().await;
    if let Some(subscription) = subscription {
        subscription.unsubscribe();
    }
    result?;
    if options.mode == PrintOutput::Text
        && let Some(last) = session.messages().last()
        && let Some(assistant) = last.as_assistant()
    {
        if matches!(
            assistant.stop_reason,
            StopReason::Error | StopReason::Aborted
        ) {
            let message = assistant
                .error_message
                .clone()
                .filter(|message| !message.is_empty())
                .unwrap_or_else(|| format!("Request {}", assistant.stop_reason.as_str()));
            write_out(stderr, &format!("{message}\n"));
            return Ok(1);
        }
        for block in &assistant.content {
            if let AssistantContentBlock::Text(text) = block {
                write_out(stdout, &format!("{}\n", text.text));
            }
        }
    }
    Ok(0)
}

async fn prompts(
    session: &AgentSession,
    options: PrintModeOptions,
    stderr: &SharedWriter,
) -> Result<(), String> {
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
    let left = settle_jobs(session, options.job_wait, options.control).await?;
    if left > 0 {
        write_out(
            stderr,
            &format!(
                "{left} background job(s) still running after {} s were stopped\n",
                options.job_wait.as_secs()
            ),
        );
    }
    Ok(())
}

/// Bake 0.3 headless `settleJobs`: waits, up to `limit`, for the
/// session's background jobs and for the turns their completion notices
/// open, so the answer printed is the last turn's. Returns how many jobs
/// were still running at the limit; the session's shutdown stops them.
/// A notice turn still running at the limit is aborted and awaited,
/// including its cleanup, before this returns.
async fn settle_jobs(
    session: &AgentSession,
    limit: Duration,
    control: RunControl,
) -> Result<usize, String> {
    let deadline = tokio::time::Instant::now() + limit;
    let jobs = session.jobs().clone();
    // Subscribed before the first check, so no settlement is missed.
    let mut events = jobs.subscribe();
    let live_count = || {
        jobs.list()
            .iter()
            .filter(|job| !job.status.is_terminal())
            .count()
    };
    loop {
        if let Some(outcome) = control.stopped() {
            return Err(outcome.to_string());
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
                    return Err(outcome.to_string());
                }
            }
            if let Some(outcome) = shared_stop {
                return Err(outcome.to_string());
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
            outcome = control.wait_stopped() => return Err(outcome.to_string()),
        }
    }
}

fn require_finished(outcome: RunOutcome) -> Result<(), String> {
    match outcome {
        RunOutcome::FinishedTurn => Ok(()),
        other => Err(other.to_string()),
    }
}
