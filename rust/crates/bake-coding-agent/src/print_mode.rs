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
    /// The output mode.
    pub mode: PrintOutput,
    /// The first prompt, with stdin prepended.
    pub initial_message: Option<String>,
    /// Images for the first prompt.
    pub initial_images: Vec<ImageContent>,
    /// Further prompts, each sent after the previous one settles.
    pub messages: Vec<String>,
}

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
    let result = prompts(session, options.clone()).await;
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

async fn prompts(session: &AgentSession, options: PrintModeOptions) -> Result<(), String> {
    if let Some(initial) = options.initial_message.filter(|text| !text.is_empty()) {
        session.prompt(&initial, options.initial_images).await?;
    }
    for message in options.messages {
        session.prompt(&message, Vec::new()).await?;
    }
    Ok(())
}
