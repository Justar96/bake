//! The ordered tool-call veto dispatcher. Ported from Pi v1.3.0
//! `ExtensionRunner.emitToolCall` (`src/core/extensions/runner.ts`: handlers
//! in order, the first `block` returned) and `AgentSession._beforeToolCall`
//! (`src/core/agent-session.ts`: a failing handler blocks the call), with
//! Bake's ordering, deadline, sticky failure, and non-sticky cancellation
//! (see the module above).

use std::any::Any;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use bake_agent::BoxFuture;
use bake_ai::AbortSignal;
use serde_json::{Value, json};

use super::{
    MAX_REASON_BYTES, PluginId, PluginRegistry, Registered, ToolCallVeto, ToolCallView, truncate,
};

/// Pi's default text for a block without a reason
/// (`agent-loop.ts` `prepareToolCall`).
const BLOCKED_WITHOUT_REASON: &str = "Tool execution was blocked";

/// Why a plugin was failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureKind {
    /// It returned `Err`.
    Error,
    /// It panicked.
    Panic,
    /// It missed the call deadline.
    Timeout,
    /// It returned a reason over [`MAX_REASON_BYTES`].
    InvalidReturn,
}

impl FailureKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Panic => "panic",
            Self::Timeout => "timeout",
            Self::InvalidReturn => "invalid-return",
        }
    }
}

/// A plugin's failure: its kind and a bounded message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Failure {
    pub kind: FailureKind,
    pub message: String,
}

impl Failure {
    fn new(kind: FailureKind, message: &str) -> Self {
        Self {
            kind,
            message: truncate(message, MAX_REASON_BYTES).to_owned(),
        }
    }
}

/// How a call was blocked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The plugin returned a block.
    Blocked { reason: String },
    /// The plugin failed on this call, or (`earlier`) on an earlier one.
    Failed { failure: Failure, earlier: bool },
}

/// What the plugins decided about one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Dispatch {
    /// No plugin objects: the call goes on to the tool.
    Continue,
    /// The call's signal aborted before a plugin was asked or while one
    /// ran. Nothing is decided or recorded, and no plugin is failed: the
    /// agent loop answers the call `Operation aborted` itself.
    Cancelled,
    /// A plugin blocked the call, or failed on it now or earlier.
    Blocked(VetoDecision),
}

#[cfg(test)]
impl Dispatch {
    /// The decision of a [`Dispatch::Blocked`].
    pub fn decision(self) -> Option<VetoDecision> {
        match self {
            Self::Blocked(decision) => Some(decision),
            Self::Continue | Self::Cancelled => None,
        }
    }
}

/// One plugin's answer, or why there is none.
enum Answer {
    Veto(ToolCallVeto),
    Failed(Failure),
    Cancelled,
}

/// A block to persist and report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VetoDecision {
    pub plugin: PluginId,
    pub version: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub outcome: Outcome,
    pub terminate: bool,
}

impl VetoDecision {
    /// The model-visible text of the denial `toolResult`.
    pub fn denial_text(&self) -> String {
        match &self.outcome {
            Outcome::Blocked { reason } if reason.is_empty() => BLOCKED_WITHOUT_REASON.to_owned(),
            Outcome::Blocked { reason } => reason.clone(),
            Outcome::Failed { failure, earlier } => format!(
                "Plugin \"{}\" {}, blocking execution: {}",
                self.plugin,
                if *earlier { "failed earlier" } else { "failed" },
                failure.message
            ),
        }
    }

    /// The `data` of the [`super::DECISION_ENTRY_TYPE`] entry.
    pub fn entry_data(&self) -> Value {
        let mut data = json!({
            "plugin": self.plugin.as_str(),
            "version": self.version,
            "toolCallId": self.tool_call_id,
            "toolName": self.tool_name,
            "decision": match self.outcome {
                Outcome::Blocked { .. } => "block",
                Outcome::Failed { .. } => "failed",
            },
            "reason": self.denial_text(),
            "terminate": self.terminate,
        });
        if let Outcome::Failed { failure, earlier } = &self.outcome {
            data["failure"] = json!({
                "kind": failure.kind.as_str(),
                "message": failure.message,
                "earlier": earlier,
            });
        }
        data
    }
}

struct Slot {
    registered: Registered,
    /// The first failure; once set, the plugin is not asked again.
    failure: Mutex<Option<Failure>>,
}

impl Slot {
    fn failure(&self) -> Option<Failure> {
        self.failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Records `failure` unless an earlier one is recorded.
    fn fail(&self, failure: Failure) {
        self.failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert(failure);
    }
}

/// Dispatches tool calls to the registered plugins, in order.
pub(crate) struct VetoDispatcher {
    /// Sorted by configured order, then id; fixed for the session.
    slots: Arc<[Slot]>,
    call_timeout: Duration,
}

impl std::fmt::Debug for VetoDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VetoDispatcher")
            .field("plugins", &self.slots.len())
            .field("call_timeout", &self.call_timeout)
            .finish()
    }
}

impl VetoDispatcher {
    /// The dispatcher for `registry`, or `None` when it is empty.
    pub fn new(registry: PluginRegistry) -> Option<Self> {
        let PluginRegistry {
            mut plugins,
            call_timeout,
        } = registry;
        if plugins.is_empty() {
            return None;
        }
        plugins.sort_by(|a, b| (a.order, &a.manifest.id).cmp(&(b.order, &b.manifest.id)));
        Some(Self {
            slots: plugins
                .into_iter()
                .map(|registered| Slot {
                    registered,
                    failure: Mutex::new(None),
                })
                .collect(),
            call_timeout,
        })
    }

    /// Asks the plugins that list `call`'s tool, in order, and returns the
    /// first block. A failed plugin blocks without being asked. No lock is
    /// held while a plugin runs.
    ///
    /// Cancellation is checked first, before a plugin's earlier failure,
    /// and wins over its answer while it runs: once `signal` has aborted,
    /// no further plugin is asked and [`Dispatch::Cancelled`] is returned.
    /// A cancelled check records no failure, so the next call asks the
    /// plugin again; dropping its future is how it stops. A failure recorded
    /// before the cancellation stays.
    pub async fn dispatch(&self, call: ToolCallView<'_>, signal: Option<&AbortSignal>) -> Dispatch {
        let slots = Arc::clone(&self.slots);
        for slot in slots
            .iter()
            .filter(|slot| slot.registered.manifest.vetoes(call.tool_name))
        {
            let decide = |outcome, terminate| {
                Dispatch::Blocked(VetoDecision {
                    plugin: slot.registered.manifest.id().clone(),
                    version: slot.registered.manifest.version().to_owned(),
                    tool_call_id: call.tool_call_id.to_owned(),
                    tool_name: call.tool_name.to_owned(),
                    outcome,
                    terminate,
                })
            };
            let earlier = |failure| {
                decide(
                    Outcome::Failed {
                        failure,
                        earlier: true,
                    },
                    false,
                )
            };
            if signal.is_some_and(AbortSignal::aborted) {
                return Dispatch::Cancelled;
            }
            if let Some(failure) = slot.failure() {
                return earlier(failure);
            }
            match self.ask(slot, call, signal).await {
                Answer::Cancelled => return Dispatch::Cancelled,
                // A concurrent call may have failed the plugin meanwhile;
                // its answer then no longer counts.
                Answer::Veto(ToolCallVeto::Continue) => {
                    if let Some(failure) = slot.failure() {
                        return earlier(failure);
                    }
                }
                Answer::Veto(ToolCallVeto::Block { reason, terminate }) => {
                    if reason.len() > MAX_REASON_BYTES {
                        let failure = Failure::new(
                            FailureKind::InvalidReturn,
                            &format!(
                                "returned a block reason of {} bytes, over the {MAX_REASON_BYTES}-byte limit",
                                reason.len()
                            ),
                        );
                        slot.fail(failure.clone());
                        return decide(
                            Outcome::Failed {
                                failure,
                                earlier: false,
                            },
                            false,
                        );
                    }
                    return decide(Outcome::Blocked { reason }, terminate);
                }
                Answer::Failed(failure) => {
                    slot.fail(failure.clone());
                    return decide(
                        Outcome::Failed {
                            failure,
                            earlier: false,
                        },
                        false,
                    );
                }
            }
        }
        Dispatch::Continue
    }

    /// Runs one plugin under the deadline and the call's cancellation,
    /// turning a panic into a failure. Dropping the plugin's future is how
    /// a timeout or cancellation stops it; cancellation wins a tie.
    async fn ask(
        &self,
        slot: &Slot,
        call: ToolCallView<'_>,
        signal: Option<&AbortSignal>,
    ) -> Answer {
        let plugin = &slot.registered.plugin;
        let future = match catch_unwind(AssertUnwindSafe(|| plugin.on_tool_call(call))) {
            Ok(future) => future,
            Err(payload) => {
                return Answer::Failed(Failure::new(
                    FailureKind::Panic,
                    &format!("panicked: {}", panic_message(payload)),
                ));
            }
        };
        let cancelled = async {
            match signal {
                Some(signal) => signal.cancelled().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            () = cancelled => Answer::Cancelled,
            answer = tokio::time::timeout(self.call_timeout, CatchUnwind(future)) => match answer {
                Err(_) => Answer::Failed(Failure::new(
                    FailureKind::Timeout,
                    &format!("no answer within {} ms", self.call_timeout.as_millis()),
                )),
                Ok(Err(panic)) => {
                    Answer::Failed(Failure::new(FailureKind::Panic, &format!("panicked: {panic}")))
                }
                Ok(Ok(Err(error))) => Answer::Failed(Failure::new(FailureKind::Error, &error)),
                Ok(Ok(Ok(veto))) => Answer::Veto(veto),
            },
        }
    }
}

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => match payload.downcast::<&'static str>() {
            Ok(message) => (*message).to_owned(),
            Err(_) => "panicked".to_owned(),
        },
    }
}

/// A future whose panic becomes an `Err` with the panic's message.
struct CatchUnwind<'a, T>(BoxFuture<'a, T>);

impl<T> Future for CatchUnwind<'_, T> {
    type Output = Result<T, String>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let future = self.0.as_mut();
        match catch_unwind(AssertUnwindSafe(|| future.poll(cx))) {
            Ok(Poll::Ready(value)) => Poll::Ready(Ok(value)),
            Ok(Poll::Pending) => Poll::Pending,
            Err(payload) => Poll::Ready(Err(panic_message(payload))),
        }
    }
}
