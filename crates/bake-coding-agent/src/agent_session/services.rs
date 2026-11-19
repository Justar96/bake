//! What a session owns beside its conversation: MCP servers, hooks,
//! background work and jobs, approval records, and its abort and shutdown.

use super::*;

impl AgentSession {
    /// The helpers this session's Wasm plugins have started and not yet
    /// seen stop, in dispatch order; empty without Wasm plugins. After
    /// [`Self::shutdown`] returns, every one of them has been reaped.
    pub fn plugin_helpers(&self) -> Vec<crate::plugins::wasm::HelperInfo> {
        self.plugins
            .as_ref()
            .map(|plugins| plugins.helpers())
            .unwrap_or_default()
    }

    /// Pi's `abort`: signals the current execution and waits for its
    /// preparation, Agent work, recovery, and required writes. The caller of
    /// `prompt` need not keep polling its waiter for cancellation to finish.
    pub async fn abort(&self) {
        let mut execution = None;
        self.core.activity.send_if_modified(|_| {
            execution = self
                .executions
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .current
                .clone();
            prompt_execution::abort_operations(&self.core, &self.agent, self.mcp.get());
            false
        });
        if let Some(execution) = execution {
            let _ = execution.wait().await;
        } else {
            self.agent.wait_for_idle().await;
            self.core.wait_for_idle().await;
        }
    }

    /// Starts the MCP servers of `options` ([`crate::mcp`]), each as a task
    /// the session's gate owns, admitted under the session's sandbox mode
    /// before it is spawned. Returns at once; the first prompt waits up to
    /// [`crate::mcp::McpTimings::startup_wait`] for servers still
    /// connecting, and each prompt declares the tools published when it
    /// starts. [`Self::shutdown`] stops every server and returns only once
    /// each server's process and tree are gone.
    ///
    /// Refused once shutdown has started, when called a second time, or,
    /// before anything starts, when a wait in `options` gives a deadline
    /// the clock cannot represent ([`crate::mcp`]'s checks).
    pub fn start_mcp(&self, options: McpOptions) -> Result<McpServers, String> {
        if self.work.is_closing() {
            return Err(refused_message(Refused::Closed));
        }
        // Before anything is stored or started, so the session may start
        // corrected options later.
        crate::mcp::validate(&options)?;
        let reserved: Vec<String> = self.tools.iter().map(|tool| tool.name.clone()).collect();
        let cwd = PathBuf::from(self.cwd());
        let mut started = None;
        let servers = self.mcp.get_or_init(|| {
            let (servers, refused) = crate::mcp::start(options, cwd, reserved, &self.work);
            started = Some(refused);
            servers
        });
        match started {
            None => Err("MCP servers were already started for this session.".to_owned()),
            Some(Some(refused)) => Err(format!(
                "The session could not start its MCP servers: {refused}."
            )),
            Some(None) => Ok(servers.clone()),
        }
    }

    /// Installs the external hooks of `options` ([`crate::hooks`]): from
    /// the next prompt, steer, follow-up, or tool call on, their events run
    /// the hooks, each process under this session's gate and confined by
    /// `options`' sandbox, and each evaluation is written to the session
    /// file before its decision takes effect. [`Self::shutdown`] kills a
    /// running hook and waits until it and its tree are gone.
    ///
    /// Refused once shutdown has started, while a prompt runs, or when
    /// called a second time. Options without a hook install nothing.
    pub fn set_hooks(&self, options: crate::hooks::HooksOptions) -> Result<(), String> {
        if let Some(reason) = self.closed_reason() {
            return Err(reason);
        }
        if self.is_streaming() {
            return Err("Hooks cannot be installed while a prompt runs.".to_owned());
        }
        if options.files.is_empty() {
            return Ok(());
        }
        let engine = crate::hooks::engine::Hooks::new(options, self.work.clone());
        let installed = Arc::new(hooks::SessionHooks::new(
            engine,
            self.session_id(),
            PathBuf::from(self.cwd()),
        ));
        self.hooks
            .set(installed)
            .map_err(|_| "Hooks were already installed for this session.".to_owned())
    }

    /// The MCP servers [`Self::start_mcp`] started, if it has.
    pub fn mcp_servers(&self) -> Option<&McpServers> {
        self.mcp.get()
    }

    /// Starts work the session owns in the background, beside its turns:
    /// [`Self::abort`] and the end of a turn leave it running; only
    /// [`OwnedTask::cancel`] and [`Self::shutdown`] abort its
    /// [`TaskScope::signal`], and shutdown returns only after it, and every
    /// task it spawned from its scope, has ended. Work that ignores the
    /// signal holds shutdown until it ends.
    ///
    /// `work` is called at once, before this returns, with the task's
    /// scope; its future runs on a Tokio task. Dropping the returned task
    /// gives up its output only. Refused with `The session is shut down.`
    /// once shutdown has started.
    pub fn spawn_background<T, F, Fut>(&self, work: F) -> Result<OwnedTask<T>, String>
    where
        T: Send + 'static,
        F: FnOnce(TaskScope) -> Fut,
        Fut: std::future::Future<Output = T> + Send + 'static,
    {
        self.work.spawn(work).map_err(refused_message)
    }

    /// The session's background jobs ([`crate::jobs`]). Like
    /// [`Self::spawn_background`] work, a job outlives [`Self::abort`] and
    /// the turn that started it; [`Self::shutdown`] stops every job and
    /// returns only once each is terminal, which is after the processes
    /// and tasks it started are settled. Each session has its own registry,
    /// so another session's jobs are refused. Jobs end with the process:
    /// a resumed session starts with none.
    pub fn jobs(&self) -> &JobRegistry {
        &self.jobs
    }

    /// Records the approvals of an engine whose tools this session runs:
    /// binds `slot` ([`crate::permissions::PermissionEngine::recorder_slot`])
    /// to this session, so each request and its answer is written to the
    /// session file before the call can run ([`crate::permissions::history`]).
    /// A failed write refuses the call and stops the session. A slot binds
    /// once; a second binding is refused. A session that does not persist
    /// (`--no-session`) binds nothing: its engine asks and answers as an
    /// unbound one does, and no entry is kept.
    pub fn record_approvals(&self, slot: &crate::permissions::RecorderSlot) -> Result<(), String> {
        if !lock(&self.session).is_persisted() {
            return Ok(());
        }
        slot.bind(Arc::new(approval_history::SessionRecorder::new(
            &self.core,
            Arc::clone(&self.persist_error),
        )))
    }

    /// Pi's `dispose`, awaited. At the first call's first poll, before it
    /// waits for anything, it removes every listener, refuses new prompts,
    /// changes, owned work, and jobs, and aborts the run, a retry wait, a
    /// compaction, every background task ([`Self::spawn_background`]), and
    /// every job ([`Self::jobs`], claiming each job's report, since a
    /// session being torn down reads no notice). The rest runs on a task of
    /// its own: it waits for the run's task, the session's writes, model
    /// requests, changes, background tasks, and jobs, and for any
    /// listener call already running, then disconnects from the agent and
    /// closes the session file for writing, releasing its lock.
    ///
    /// The teardown runs on the runtime the session was created on, so
    /// `shutdown` may be called and awaited from any thread or executor.
    /// Every call, concurrent or later, returns once that teardown has
    /// finished, and then no listener, hook, or owned work runs or starts.
    /// Dropping a call's future stops neither the teardown nor the refusal:
    /// the session never reopens. Later prompts fail and emit no events,
    /// and a later model or thinking-level change is not written. If the
    /// session's runtime has shut down, or the teardown panics, no teardown
    /// can finish: calls then return without it, having done only the
    /// first poll's part, and the session file is released when the
    /// session is dropped.
    ///
    /// The drain also covers the processes of tools given this session's
    /// gate ([`AgentSessionOptions::work`]), a sandbox runner probe their
    /// call starts included ([`bake_sandbox::ProbeOwner`]): each holds a
    /// permit from its spawn until its tree is settled, and the run's
    /// task, stopped first, drops a running call, which kills the call's
    /// tree and moves the process to a task that settles it under the same
    /// permit. Shutdown
    /// therefore returns only once every such tree is gone, with no time
    /// limit; a process that cannot die (uninterruptible sleep) holds it.
    /// Processes of other sessions, or of tools given no owner, are not
    /// waited for, and a descendant that left its tree is not owned
    /// ([`bake_process`]). A model request's task awaits the
    /// provider task behind its stream, so draining the gate awaits both.
    pub async fn shutdown(&self) {
        let mut done = self.shutdown.get_or_init(|| self.begin_shutdown()).clone();
        // An error means the teardown task ended without finishing: it
        // panicked, or the session's runtime shut down and dropped it.
        let _ = done.wait_for(|done| *done).await;
    }

    /// The synchronous half of [`Self::shutdown`], run once; spawns the
    /// awaited half on the session's runtime and returns its completion.
    fn begin_shutdown(&self) -> tokio::sync::watch::Receiver<bool> {
        // Closed before anything is cancelled: a cancelled retry wait or
        // compaction resumes on another worker at once, and Pi's
        // synchronous `dispose` clears its listeners before any of that
        // work's continuations run.
        self.listeners.close();
        let control = self.core.control();
        if let Some(control) = &control {
            control.cancel();
        }
        self.core.abort_requested.store(true, Ordering::SeqCst);
        self.core.abort_retry_and_compaction();
        if let Some(mcp) = self.mcp.get() {
            mcp.interrupt_startup_wait();
        }
        // Also cancels every MCP server's task, which stops its server
        // before its permit is released.
        // The jobs' drivers are the gate's tasks, so the drain below waits
        // for every job to become terminal; this future need not be kept.
        // The team is left first: a frame is accepted only while its
        // notice can still be read, and closing the jobs clears them.
        if let Some(Ok(station)) = &self.telecom {
            station.leave();
        }
        drop(self.jobs.close());
        if let Some(questions) = &self.questions {
            questions.close();
        }
        let drained = self.work.close();
        let agent = Arc::clone(&self.agent);
        let listeners = Arc::clone(&self.listeners);
        let subscription = Arc::clone(&self.subscription);
        let session = Arc::clone(&self.session);
        let (finished, done) = tokio::sync::watch::channel(false);
        let teardown = async move {
            agent.shutdown().await;
            if let Some(control) = control {
                control.drain_admissions().await;
            }
            // The run's task is gone, so a tool call it was running has
            // dropped its process, whose settling task holds a permit of
            // this gate: the drain waits for it.
            drained.await;
            listeners.quiesce().await;
            if let Some(subscription) = subscription
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                subscription.unsubscribe();
            }
            // Nothing writes the session any more: another writer may take
            // its file over, and a late change stays in memory.
            lock(&session).close_writer();
            finished.send_replace(true);
        };
        // Not owned by the gate, which it closes: every caller of
        // `shutdown` waits for it, and it runs whether or not one does, on
        // the session's runtime whatever thread or executor called.
        drop(self.runtime.spawn(teardown));
        done
    }
}
