//! The session's `before_tool_call` hook over its plugins, Pi v1.3.0's
//! `AgentSession._beforeToolCall` (`src/core/agent-session.ts`) with Bake's
//! persisted decision: a block is written to the session file before the
//! agent emits the denial `toolResult`, and a failed write stops the run.
//!
//! Cancellation and commit are linearized at the decision write's admission:
//! the call's signal is read under the session lock, after the gap check and
//! before the entry is appended. Aborted there or earlier (in dispatch), no
//! entry is written, and the cancellation itself fails no plugin; a failure
//! a plugin recorded before it still stands. Aborted after admission, the
//! admitted entry stays, since it is owned by the session's `WorkGate` and
//! finishes before shutdown returns, but the agent loop still answers the
//! call `Operation aborted`, because it checks the signal after the hook and
//! before it reads the block. That is the tested case in which a decision
//! entry has no matching denial result; it is not the only cause, since a
//! crash or shutdown before the result is written, or a failed write of the
//! result, leaves the same trace. No decision write is admitted after
//! `AgentSession::shutdown` returns.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};

use bake_agent::{BeforeToolCall, BeforeToolCallResult, hook};
use bake_ai::AbortSignal;

use super::recovery::Core;
use super::{WriteRefused, write_session};
use crate::plugins::veto::{Dispatch, VetoDecision, VetoDispatcher};
use crate::plugins::{DECISION_ENTRY_TYPE, ToolCallView};

/// The text a cancelled call is blocked with. The agent loop answers an
/// aborted call with this same text before it reads any block.
const ABORTED: &str = "Operation aborted";

/// The hook that asks `plugins` about each tool call. A block becomes the
/// call's error result once its decision entry is written; `Err`, which the
/// agent loop also turns into an error result, means it could not be. A
/// cancelled call is still blocked, fail-closed, but records nothing.
pub(super) fn plugin_veto_hook(
    core: Arc<Core>,
    persist_error: Arc<Mutex<Option<String>>>,
    plugins: Arc<VetoDispatcher>,
) -> BeforeToolCall {
    hook::before_tool_call(move |context, signal| {
        let core = Arc::clone(&core);
        let persist_error = Arc::clone(&persist_error);
        let plugins = Arc::clone(&plugins);
        Box::pin(async move {
            // Read-only from here on: no plugin can change what runs.
            let args = &*context.args;
            let call = ToolCallView {
                tool_call_id: &context.tool_call.id,
                tool_name: &context.tool_call.name,
                args,
            };
            let decision = match plugins.dispatch(call, signal.as_ref()).await {
                Dispatch::Continue => return Ok(None),
                Dispatch::Cancelled => return Ok(Some(aborted())),
                Dispatch::Blocked(decision) => decision,
            };
            if !persist_decision(&core, &persist_error, &decision, signal).await? {
                return Ok(Some(aborted()));
            }
            if let Some(error) = &decision.unrecorded {
                // The call stays blocked and the plugin failed for this
                // session, but later sessions may not know: stop the run
                // and report it, as a failed decision write does.
                return Err(stop(
                    &core,
                    &persist_error,
                    format!("{error}, so the run stopped"),
                ));
            }
            Ok(Some(BeforeToolCallResult {
                block: true,
                reason: Some(decision.denial_text()),
                terminate: decision.terminate,
            }))
        })
    })
}

fn aborted() -> BeforeToolCallResult {
    BeforeToolCallResult {
        block: true,
        reason: Some(ABORTED.to_owned()),
        terminate: false,
    }
}

/// Writes `decision` at the session's write boundary unless `signal` has
/// aborted by its admission (see the module docs); `Ok(false)` means it was
/// not written for that reason. The write is blocking work of the
/// session's gate, admitted under a permit taken before it starts, so
/// shutdown waits for it even if the run is cancelled meanwhile. A failure
/// records the gap, aborts the run (as a failed change's write does), and
/// returns the reason.
async fn persist_decision(
    core: &Core,
    persist_error: &Mutex<Option<String>>,
    decision: &VetoDecision,
    signal: Option<AbortSignal>,
) -> Result<bool, String> {
    let Some(permit) = core.work.enter() else {
        return Err("The session is shut down.".to_owned());
    };
    let session = Arc::clone(&core.session);
    let gap = Arc::clone(&core.gap);
    let data = decision.entry_data();
    #[cfg(test)]
    let (fail, before_admission, admitted) = {
        let mut hooks = core.hooks();
        (
            std::mem::take(&mut hooks.fail_plugin_decision),
            hooks.before_plugin_decision_admission.take(),
            hooks.plugin_decision_admitted.take(),
        )
    };
    let write = permit.spawn_blocking(move || {
        #[cfg(test)]
        if let Some(hook) = before_admission {
            hook();
        }
        write_session(&session, &gap, |session| {
            // The admission: cancelled by now, nothing is written.
            if signal.as_ref().is_some_and(AbortSignal::aborted) {
                return Ok(false);
            }
            #[cfg(test)]
            if let Some(hook) = admitted {
                hook();
            }
            #[cfg(test)]
            if fail {
                return Err(super::injected_failure().to_string());
            }
            session
                .append_custom_entry(DECISION_ENTRY_TYPE, Some(data))
                .map(|_| true)
                .map_err(|error| error.to_string())
        })
    });
    drop(permit);
    // Refused only outside a runtime: nothing ran, as when the gate is shut.
    let written = write.map_err(|refused| refused.to_string())?.await;
    let error = match written {
        Ok(Ok(written)) => return Ok(written),
        Ok(Err(WriteRefused::AfterGap(error) | WriteRefused::Failed(error))) => error,
        Err(error) => {
            // Whether the entry landed is unknown, so later writes stop.
            let error = format!("Session I/O task failed: {error}");
            super::lock_gap(&core.gap).get_or_insert_with(|| error.clone());
            error
        }
    };
    stop(core, persist_error, error.clone());
    Err(format!(
        "The plugin decision could not be written to the session file ({error}), so the session stopped and takes no more prompts."
    ))
}

/// Aborts the run and records `error` as its result; returns `error`.
fn stop(core: &Core, persist_error: &Mutex<Option<String>>, error: String) -> String {
    core.abort_requested.store(true, Ordering::SeqCst);
    if let Some(agent) = core.agent() {
        agent.abort();
    }
    persist_error
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert_with(|| error.clone());
    error
}
