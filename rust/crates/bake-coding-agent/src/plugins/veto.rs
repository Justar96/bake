//! The ordered tool-call veto dispatcher. Ported from Pi v1.3.0
//! `ExtensionRunner.emitToolCall` (`src/core/extensions/runner.ts`: handlers
//! in order, the first `block` returned) and `AgentSession._beforeToolCall`
//! (`src/core/agent-session.ts`: a failing handler blocks the call), with
//! Bake's ordering, deadline, sticky failure, and non-sticky cancellation
//! (see the module above), and the configured Wasm plugins of
//! [`super::installed`]: their failures recorded in the Bake home, and their
//! helpers shut down by a keeper task the session's work gate owns.

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

use super::wasm::session::{HelperInfo, WasmAnswer, WasmVeto};
use super::{
    Contribution, MAX_REASON_BYTES, PluginId, PluginRegistry, Registered, ToolCallVeto,
    ToolCallView, truncate,
};
use crate::owned_work::{Refused, WorkGate};

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
    /// A Wasm guest trapped or hit a limit.
    Trap,
    /// A Wasm plugin's helper died or broke the protocol during a call.
    Helper,
    /// A Wasm plugin's helper refused the component, missed its compile
    /// deadline, or died while loading it.
    Start,
    /// A Wasm plugin could not be started for a reason outside the plugin:
    /// the compile queue, the spawn, or the helper's memory-limit setup.
    Unavailable,
    /// A configured plugin's package could not be loaded from the store.
    Unloadable,
}

impl FailureKind {
    /// Every kind, for parsing a recorded one.
    const ALL: [Self; 9] = [
        Self::Error,
        Self::Panic,
        Self::Timeout,
        Self::InvalidReturn,
        Self::Trap,
        Self::Helper,
        Self::Start,
        Self::Unavailable,
        Self::Unloadable,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Panic => "panic",
            Self::Timeout => "timeout",
            Self::InvalidReturn => "invalid-return",
            Self::Trap => "trap",
            Self::Helper => "helper",
            Self::Start => "start",
            Self::Unavailable => "unavailable",
            Self::Unloadable => "unloadable",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == text)
    }

    /// Whether a configured plugin's failure of this kind is recorded in
    /// the Bake home, so every later session blocks its tools until the
    /// plugin is disabled. The kinds that are not recorded come from the
    /// host or the store, not the plugin, and are found again (or not) by
    /// the next session.
    pub(crate) fn persists(self) -> bool {
        !matches!(self, Self::Unavailable | Self::Unloadable)
    }
}

/// A plugin's failure: its kind and a bounded message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Failure {
    pub kind: FailureKind,
    pub message: String,
}

impl Failure {
    pub(crate) fn new(kind: FailureKind, message: &str) -> Self {
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
    /// A Wasm plugin could not be asked about this call (its arguments are
    /// over the host's limits); the call is blocked, the plugin is not
    /// failed.
    Unchecked { message: String },
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
    /// A configured Wasm plugin's package hash.
    pub sha256: Option<String>,
    pub tool_call_id: String,
    pub tool_name: String,
    pub outcome: Outcome,
    pub terminate: bool,
    /// The failure this call recorded could not be written to the Bake
    /// home; the hook stops the run with this reason after writing the
    /// decision.
    pub unrecorded: Option<String>,
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
            Outcome::Unchecked { message } => format!(
                "Plugin \"{}\" cannot check this call, blocking execution: {message}",
                self.plugin
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
                Outcome::Unchecked { .. } => "unchecked",
            },
            "reason": self.denial_text(),
            "terminate": self.terminate,
        });
        if let Some(sha256) = &self.sha256 {
            data["sha256"] = json!(sha256);
        }
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
    /// The session's gate, once [`Self::attach`]ed: Wasm asks and failure
    /// records run under its permits, and its keeper shuts the helpers
    /// down. Unattached, a Wasm plugin is never asked.
    work: std::sync::OnceLock<WorkGate>,
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
                .map(|registered| {
                    // A configured plugin that failed before, or cannot be
                    // loaded, blocks from the first call.
                    let failure = match &registered.plugin {
                        Contribution::Blocked(failure) => Some(failure.clone()),
                        _ => None,
                    };
                    Slot {
                        registered,
                        failure: Mutex::new(failure),
                    }
                })
                .collect(),
            call_timeout,
            work: std::sync::OnceLock::new(),
        })
    }

    /// The Wasm adapters, in dispatch order.
    fn wasm(&self) -> Vec<Arc<WasmVeto>> {
        self.slots
            .iter()
            .filter_map(|slot| match &slot.registered.plugin {
                Contribution::Wasm(veto) => Some(Arc::clone(veto)),
                _ => None,
            })
            .collect()
    }

    /// Binds the dispatcher to the session's gate. With Wasm plugins, it
    /// spawns their keeper: an owned task that waits for the gate to close
    /// and then shuts every helper down, so the gate's drain, and the
    /// session's shutdown, wait for each helper to be reaped. Call once,
    /// before the first dispatch.
    pub fn attach(&self, work: &WorkGate) -> Result<(), Refused> {
        if self.work.set(work.clone()).is_err() {
            return Ok(());
        }
        let wasm = self.wasm();
        if wasm.is_empty() {
            return Ok(());
        }
        let keeper = work.spawn(move |scope| async move {
            scope.signal().cancelled().await;
            for veto in wasm {
                veto.shutdown().await;
            }
        })?;
        // The gate owns and awaits the keeper; its output is nothing.
        drop(keeper);
        Ok(())
    }

    /// The helpers the session's Wasm plugins have started and not yet
    /// seen stop, in dispatch order.
    pub fn helpers(&self) -> Vec<HelperInfo> {
        self.wasm().iter().flat_map(|veto| veto.helpers()).collect()
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
                    sha256: slot.registered.sha256.clone(),
                    tool_call_id: call.tool_call_id.to_owned(),
                    tool_name: call.tool_name.to_owned(),
                    outcome,
                    terminate,
                    unrecorded: None,
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
            if let Contribution::Wasm(veto) = &slot.registered.plugin {
                match self.ask_wasm(slot, veto, call, signal).await {
                    Some(Dispatch::Continue) => continue,
                    Some(blocked @ Dispatch::Blocked(_)) => return blocked,
                    Some(Dispatch::Cancelled) | None => return Dispatch::Cancelled,
                }
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

    /// Asks one Wasm plugin, under a permit of the session's gate held
    /// until its failure, if any, is recorded: `None` when the gate is
    /// closing or was never attached, which the caller treats as a
    /// cancellation. The host bounds every phase itself (queue, start,
    /// compile, call, and teardown), so no outer deadline drops the ask;
    /// a cancelled start or call is torn down in place. Cancellation wins a
    /// tie with an answer, as for trusted plugins.
    async fn ask_wasm(
        &self,
        slot: &Slot,
        veto: &WasmVeto,
        call: ToolCallView<'_>,
        signal: Option<&AbortSignal>,
    ) -> Option<Dispatch> {
        let permit = self.work.get()?.enter()?;
        let decide = |outcome, terminate, unrecorded| {
            Dispatch::Blocked(VetoDecision {
                plugin: slot.registered.manifest.id().clone(),
                version: slot.registered.manifest.version().to_owned(),
                sha256: slot.registered.sha256.clone(),
                tool_call_id: call.tool_call_id.to_owned(),
                tool_name: call.tool_name.to_owned(),
                outcome,
                terminate,
                unrecorded,
            })
        };
        let answer = veto.ask(call, signal).await;
        if signal.is_some_and(AbortSignal::aborted) {
            return Some(Dispatch::Cancelled);
        }
        Some(match answer {
            WasmAnswer::Cancelled => Dispatch::Cancelled,
            WasmAnswer::Unchecked(message) => decide(Outcome::Unchecked { message }, false, None),
            WasmAnswer::Veto(ToolCallVeto::Continue) => match slot.failure() {
                Some(failure) => decide(
                    Outcome::Failed {
                        failure,
                        earlier: true,
                    },
                    false,
                    None,
                ),
                None => Dispatch::Continue,
            },
            // The host refuses a reason over the limit as a broken reply.
            WasmAnswer::Veto(ToolCallVeto::Block { reason, terminate }) => {
                decide(Outcome::Blocked { reason }, terminate, None)
            }
            WasmAnswer::Failed(failure) => {
                slot.fail(failure.clone());
                let unrecorded = match &slot.registered.record {
                    Some(record) if failure.kind.persists() => {
                        let record = record.clone();
                        let recorded = failure.clone();
                        // A blocking job of the gate: it finishes even if this
                        // call is dropped meanwhile.
                        match permit.spawn_blocking(move || record.record(&recorded)) {
                            Ok(job) => match job.await {
                                Ok(Ok(())) => None,
                                Ok(Err(error)) => Some(error),
                                Err(error) => Some(format!("the record task failed: {error}")),
                            },
                            Err(refused) => Some(refused.to_string()),
                        }
                    }
                    _ => None,
                };
                decide(
                    Outcome::Failed {
                        failure,
                        earlier: false,
                    },
                    false,
                    unrecorded,
                )
            }
        })
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
        let Contribution::Trusted(plugin) = &slot.registered.plugin else {
            // Blocked slots are failed from the start, and Wasm slots are
            // asked by `ask_wasm`.
            return Answer::Failed(Failure::new(
                FailureKind::Error,
                "the plugin cannot be asked",
            ));
        };
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
