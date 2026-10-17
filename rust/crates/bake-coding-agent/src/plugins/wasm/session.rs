//! One configured Wasm plugin inside one [`AgentSession`]: the adapter the
//! ordered veto dispatcher asks, and the owner of every helper the plugin
//! runs for that session.
//!
//! A [`WasmVeto`] starts no helper until it is first asked. Each helper is
//! one [`WasmPluginHost`] generation, created and retained in the adapter
//! before its first await, so nothing is spawned that the adapter does not
//! own. Calls are serialized: one ask at a time initializes or reuses the
//! current generation and sends its call with the call's cancellation
//! signal, so a cancelled start or call is torn down in place by the host.
//!
//! - **Cancellation** (the call's signal, or the session's shutdown
//!   interrupting the host) answers [`WasmAnswer::Cancelled`]. It fails
//!   nothing: the host it interrupted is torn down, and the next ask
//!   starts a fresh generation, which answers or fails like the first.
//! - **A failure** (the start refused, a trap, a guest error, a missed
//!   deadline, a broken helper) answers [`WasmAnswer::Failed`] after the
//!   adapter has shut that generation down and awaited it. The dispatcher
//!   makes the failure sticky, so the adapter is not asked again.
//! - **Arguments the host will not send** (too long or too deep) answer
//!   [`WasmAnswer::Unchecked`]: that call is blocked, the plugin is not
//!   failed.
//! - **Shutdown** ([`WasmVeto::shutdown`], run by the session's keeper
//!   task when its work gate closes) refuses new generations, interrupts a
//!   pending start or call, and awaits every generation's complete
//!   teardown: the helper reaped and its pipes closed. The keeper is owned
//!   work of the gate, so the session's shutdown returns only after that.
//!
//! [`AgentSession`]: crate::agent_session::AgentSession

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use bake_ai::AbortSignal;

use super::host::{CallError, HostRequest, HostStatus, StartError, WasmPluginHost};
use super::manifest::WasmPackage;
use super::{CompileQueue, HelperCommand, HostLimits};
use crate::plugins::veto::{Failure, FailureKind};
use crate::plugins::{PluginId, ToolCallVeto, ToolCallView};

/// How a session runs its Wasm plugins: the helper executable, the compile
/// queue its hosts share, and their limits. Share one queue across every
/// session of a process.
#[derive(Debug, Clone)]
pub struct WasmSetup {
    /// The helper executable; `None` makes every configured Wasm plugin
    /// unloadable, so each blocks the tools it lists.
    pub command: Option<HelperCommand>,
    /// The queue that admits one helper start at a time.
    pub queue: CompileQueue,
    /// Each helper's limits.
    pub limits: HostLimits,
}

impl Default for WasmSetup {
    /// No helper executable, a queue of its own that lets 32 starts wait,
    /// and the default limits.
    fn default() -> Self {
        Self {
            command: None,
            queue: CompileQueue::new(32),
            limits: HostLimits::default(),
        }
    }
}

/// What one ask of a Wasm plugin came to.
#[derive(Debug)]
pub(crate) enum WasmAnswer {
    /// The plugin's answer.
    Veto(ToolCallVeto),
    /// The call's signal aborted, or the session is shutting down; nothing
    /// was decided.
    Cancelled,
    /// The call was not sent, for this reason; the plugin is not failed.
    Unchecked(String),
    /// The plugin failed; its generation has been shut down.
    Failed(Failure),
}

/// One helper the plugin started in this session, for inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperInfo {
    /// The plugin.
    pub plugin: PluginId,
    /// The host generation, from 1.
    pub generation: u64,
    /// The helper's process id, once spawned.
    pub pid: Option<u32>,
    /// What the host is doing.
    pub status: HostStatus,
}

/// The generations a [`WasmVeto`] owns.
#[derive(Debug, Default)]
struct Generations {
    /// Set by [`WasmVeto::shutdown`]; no generation starts after it.
    closed: bool,
    /// The generation the next ask uses, while it can still answer.
    current: Option<Arc<WasmPluginHost>>,
    /// Every generation not yet known to be stopped, `current` included.
    hosts: Vec<Arc<WasmPluginHost>>,
    /// Generations created so far.
    created: u64,
}

/// A configured Wasm plugin's veto in one session.
#[derive(Debug)]
pub(crate) struct WasmVeto {
    package: WasmPackage,
    command: HelperCommand,
    queue: CompileQueue,
    limits: HostLimits,
    generations: Mutex<Generations>,
    /// One ask at a time; held across a start and its call, never across
    /// a session write.
    serial: tokio::sync::Mutex<()>,
}

impl WasmVeto {
    pub(crate) fn new(
        package: WasmPackage,
        command: HelperCommand,
        queue: CompileQueue,
        limits: HostLimits,
    ) -> Self {
        Self {
            package,
            command,
            queue,
            limits,
            generations: Mutex::default(),
            serial: tokio::sync::Mutex::new(()),
        }
    }

    fn generations(&self) -> MutexGuard<'_, Generations> {
        self.generations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The helpers this plugin has started and not yet seen stop.
    pub(crate) fn helpers(&self) -> Vec<HelperInfo> {
        self.generations()
            .hosts
            .iter()
            .map(|host| HelperInfo {
                plugin: host.manifest().id().clone(),
                generation: host.generation(),
                pid: host.pid(),
                status: *host.status().borrow(),
            })
            .collect()
    }

    /// The current generation if it can still answer, or a new one, which
    /// is retained before this returns; `None` once shut down.
    fn host(&self) -> Result<Option<Arc<WasmPluginHost>>, Failure> {
        let mut generations = self.generations();
        if generations.closed {
            return Ok(None);
        }
        if let Some(current) = &generations.current
            && matches!(
                *current.status().borrow(),
                HostStatus::New | HostStatus::Idle
            )
        {
            return Ok(Some(Arc::clone(current)));
        }
        // Stopped generations need nothing more; the others stay owned
        // until their teardown is complete.
        generations
            .hosts
            .retain(|host| *host.status().borrow() != HostStatus::Stopped);
        let generation = generations.created.checked_add(1).ok_or_else(|| {
            Failure::new(
                FailureKind::Unavailable,
                "the plugin generation limit was reached",
            )
        })?;
        let host = WasmPluginHost::new(HostRequest {
            command: self.command.clone(),
            package: self.package.clone(),
            limits: self.limits.clone(),
            queue: self.queue.clone(),
            generation,
            status: None,
        })
        .map(Arc::new)
        .map_err(|error| Failure::new(FailureKind::Start, &error.to_string()))?;
        generations.created = generation;
        generations.current = Some(Arc::clone(&host));
        generations.hosts.push(Arc::clone(&host));
        Ok(Some(host))
    }

    /// Asks the plugin about `call` (see the module documentation).
    pub(crate) async fn ask(
        &self,
        call: ToolCallView<'_>,
        signal: Option<&AbortSignal>,
    ) -> WasmAnswer {
        let _serial = tokio::select! {
            biased;
            () = cancelled(signal) => return WasmAnswer::Cancelled,
            serial = self.serial.lock() => serial,
        };
        // A generation found unavailable (its earlier operation was dropped
        // and torn down) is replaced once.
        for _ in 0..2 {
            let host = match self.host() {
                Ok(Some(host)) => host,
                Ok(None) => return WasmAnswer::Cancelled,
                Err(failure) => return WasmAnswer::Failed(failure),
            };
            if *host.status().borrow() == HostStatus::New {
                match host.initialize(signal).await {
                    Ok(()) => {}
                    Err(StartError::Cancelled { .. } | StartError::ShutDown { .. }) => {
                        return WasmAnswer::Cancelled;
                    }
                    Err(error) => {
                        let kind = match error {
                            // The host side, not the plugin: the queue, the
                            // spawn, or the helper's own limit setup.
                            StartError::QueueFull
                            | StartError::QueueTimeout
                            | StartError::Spawn(_)
                            | StartError::Handshake { .. }
                            | StartError::InvalidLimits(_)
                            | StartError::AlreadyStarted => FailureKind::Unavailable,
                            _ => FailureKind::Start,
                        };
                        return self.fail(&host, kind, &error.to_string()).await;
                    }
                }
            }
            match host.on_tool_call(call, signal).await {
                Ok(checked) => return WasmAnswer::Veto(checked.veto),
                Err(CallError::Cancelled { .. } | CallError::ShutDown { .. }) => {
                    return WasmAnswer::Cancelled;
                }
                Err(CallError::Unavailable) => {
                    if signal.is_some_and(AbortSignal::aborted) {
                        return WasmAnswer::Cancelled;
                    }
                    self.retire(&host);
                }
                Err(error @ (CallError::InvalidCall(_) | CallError::ArgumentsTooLarge)) => {
                    return WasmAnswer::Unchecked(error.to_string());
                }
                Err(error) => {
                    let kind = match error {
                        CallError::Trap { .. } => FailureKind::Trap,
                        CallError::Timeout { .. } => FailureKind::Timeout,
                        CallError::Guest(_) => FailureKind::Error,
                        _ => FailureKind::Helper,
                    };
                    return self.fail(&host, kind, &error.to_string()).await;
                }
            }
        }
        WasmAnswer::Failed(Failure::new(
            FailureKind::Helper,
            "the plugin helper stopped again after a restart",
        ))
    }

    /// Makes `host` no longer current; it stays owned until it has stopped.
    fn retire(&self, host: &Arc<WasmPluginHost>) {
        let mut generations = self.generations();
        if generations
            .current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, host))
        {
            generations.current = None;
        }
    }

    /// Retires and shuts `host` down, awaiting one bounded teardown, then
    /// answers the failure. An unfinished teardown is resumed by
    /// [`Self::shutdown`].
    async fn fail(
        &self,
        host: &Arc<WasmPluginHost>,
        kind: FailureKind,
        message: &str,
    ) -> WasmAnswer {
        self.retire(host);
        let _ = host.shutdown().await;
        WasmAnswer::Failed(Failure::new(kind, message))
    }

    /// Refuses new generations and awaits every generation's complete
    /// teardown, interrupting a pending start or call. Each pass is bounded
    /// by the host's teardown waits; a helper that never exits holds this,
    /// as a process that cannot die holds the session's shutdown.
    pub(crate) async fn shutdown(&self) {
        let hosts = {
            let mut generations = self.generations();
            generations.closed = true;
            generations.current = None;
            std::mem::take(&mut generations.hosts)
        };
        for host in hosts {
            while !host.shutdown().await.is_complete() {}
        }
    }
}

async fn cancelled(signal: Option<&AbortSignal>) {
    match signal {
        Some(signal) => signal.cancelled().await,
        None => std::future::pending().await,
    }
}
