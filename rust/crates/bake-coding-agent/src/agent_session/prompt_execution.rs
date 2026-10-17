//! The session owns prompt preparation, execution, recovery, and settlement.
//! Callers only wait for the immutable execution they admitted.

use super::*;

#[derive(Default)]
pub(super) struct Executions {
    next_id: u64,
    pub current: Option<Arc<Execution>>,
}

pub(super) type SharedExecutions = Arc<Mutex<Executions>>;

pub(super) struct Execution {
    id: u64,
    pub control: RunControl,
    jobs: JobRegistry,
    pending_notices: Arc<Mutex<Vec<CustomMessage>>>,
    delivery_only: bool,
    pub notice_turn_started: AtomicBool,
    finished: tokio::sync::watch::Receiver<Option<RunOutcome>>,
}

pub(super) type Completion = tokio::sync::watch::Sender<Option<RunOutcome>>;

pub(super) enum Input {
    Prompt {
        text: String,
        images: Vec<ImageContent>,
    },
    Notices,
}

impl Executions {
    /// Called while the session's activity claim is locked.
    pub fn claim(
        &mut self,
        control: RunControl,
        jobs: JobRegistry,
        pending_notices: Arc<Mutex<Vec<CustomMessage>>>,
        delivery_only: bool,
    ) -> Result<(Arc<Execution>, Completion), String> {
        if self.current.is_some() {
            return Err("The previous session execution has not finished.".into());
        }
        let id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| "The session exhausted its execution identities.".to_owned())?;
        self.next_id = id;
        let (completion, finished) = tokio::sync::watch::channel(None);
        let execution = Arc::new(Execution {
            id,
            control,
            jobs,
            pending_notices,
            delivery_only,
            notice_turn_started: AtomicBool::new(false),
            finished,
        });
        self.current = Some(Arc::clone(&execution));
        Ok((execution, completion))
    }
}

impl Execution {
    pub async fn wait(&self) -> Result<RunOutcome, String> {
        let mut finished = self.finished.clone();
        finished
            .wait_for(Option::is_some)
            .await
            .map_err(|_| "The session execution ended before reporting its outcome.".to_owned())?
            .clone()
            .ok_or_else(|| "The session execution reported no outcome.".to_owned())
    }
}

/// Live services stay shared; prompt configuration is fixed at admission.
pub(super) struct PromptExecution {
    agent: Arc<Agent>,
    core: Arc<Core>,
    work: WorkGate,
    registry: ModelRuntime,
    settings: SharedSettings,
    tools: Vec<Arc<AgentTool>>,
    active_tool_names: Vec<String>,
    prompt_options: SystemPromptOptions,
    prompt_templates: Vec<PromptTemplate>,
    docs_command: String,
    listeners: Arc<Listeners>,
    persist_error: Arc<Mutex<Option<String>>>,
    mcp: Arc<OnceLock<McpServers>>,
    executions: SharedExecutions,
    runtime: tokio::runtime::Handle,
    jobs: JobRegistry,
    pending_notices: Arc<Mutex<Vec<CustomMessage>>>,
}

impl PromptExecution {
    pub fn new(session: &AgentSession) -> Self {
        Self {
            agent: Arc::clone(&session.agent),
            core: Arc::clone(&session.core),
            work: session.work.clone(),
            registry: session.registry.clone(),
            settings: Arc::clone(&session.settings),
            tools: session.tools.clone(),
            active_tool_names: session.active_tool_names.clone(),
            prompt_options: session.prompt_options.clone(),
            prompt_templates: session.prompt_templates.clone(),
            docs_command: session.docs_command.clone(),
            listeners: Arc::clone(&session.listeners),
            persist_error: Arc::clone(&session.persist_error),
            mcp: Arc::clone(&session.mcp),
            executions: Arc::clone(&session.executions),
            runtime: session.runtime.clone(),
            jobs: session.jobs.clone(),
            pending_notices: Arc::clone(&session.pending_notices),
        }
    }

    pub async fn run(
        self,
        execution: Arc<Execution>,
        completion: Completion,
        scope: TaskScope,
        input: Input,
    ) {
        let guard = RunGuard {
            core: Arc::clone(&self.core),
            agent: Arc::clone(&self.agent),
            executions: Arc::clone(&self.executions),
            execution: Arc::clone(&execution),
            completion,
            runtime: self.runtime.clone(),
            permit: Some(scope.permit().share()),
            finished: false,
        };
        let control = &execution.control;
        let result = {
            let prompt = async {
                match input {
                    Input::Prompt { text, images } => {
                        self.prompt_inner(&text, images, control).await
                    }
                    Input::Notices => self.deliver_notices(&execution).await,
                }
            };
            tokio::pin!(prompt);
            tokio::select! {
                result = &mut prompt => result,
                _ = control.wait_stopped() => {
                    self.request_abort();
                    prompt.await
                }
                () = scope.signal().cancelled() => {
                    control.cancel();
                    self.request_abort();
                    prompt.await
                }
            }
        };
        if self.core.aborted() {
            self.core.finish_cancelled_retry();
        }
        drain_execution(&self.core, &self.agent, control).await;
        let persisted = self
            .persist_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let gap = lock_gap(&self.core.gap).clone();
        let outcome = if let Some(error) = persisted.or(gap) {
            control.fail(error)
        } else {
            control.settle(result.unwrap_or_else(|message| RunOutcome::Failed { message }))
        };
        guard.finish(outcome);
    }

    fn model(&self) -> Option<Model> {
        self.core.model()
    }

    fn closed_reason(&self) -> Option<String> {
        closed_reason(&self.work, &self.listeners, &self.core.gap)
    }

    fn request_abort(&self) {
        abort_operations(&self.core, &self.agent, self.mcp.get());
    }

    async fn prepare_user_input(
        &self,
        text: &str,
        images: Vec<ImageContent>,
        model: Option<Model>,
    ) -> Result<(String, Vec<ImageContent>), String> {
        prepare_user_input(
            &self.work,
            &self.prompt_options,
            &self.prompt_templates,
            &self.settings,
            text,
            images,
            model,
        )
        .await
    }
}

pub(super) fn closed_reason(
    work: &WorkGate,
    listeners: &Listeners,
    gap: &Mutex<Option<String>>,
) -> Option<String> {
    if listeners.closed.load(Ordering::SeqCst) || work.is_closing() {
        return Some("The session is shut down.".into());
    }
    lock_gap(gap).as_ref().map(|error| {
        format!(
            "The session file could not be written ({error}), so the session takes no more prompts."
        )
    })
}

pub(super) fn abort_operations(core: &Core, agent: &Agent, mcp: Option<&McpServers>) {
    if let Some(control) = core.control() {
        control.cancel();
    }
    core.abort_requested.store(true, Ordering::SeqCst);
    core.abort_retry_and_compaction();
    if let Some(mcp) = mcp {
        mcp.interrupt_startup_wait();
    }
    agent.abort();
}

async fn drain_execution(core: &Core, agent: &Agent, control: &RunControl) {
    agent.wait_for_idle().await;
    control.drain_admissions().await;
    let mut activity = core.activity.subscribe();
    let _ = activity
        .wait_for(|activity| activity.context_commits == 0)
        .await;
}

/// Clears only this admission. An old waiter or cleanup cannot release a
/// successor, even when both prompts deliberately share the same control.
pub(super) fn release_execution(
    core: &Core,
    executions: &Mutex<Executions>,
    execution: &Execution,
) -> bool {
    let mut released = false;
    core.activity.send_if_modified(|activity| {
        let mut executions = executions.lock().unwrap_or_else(PoisonError::into_inner);
        if !executions
            .current
            .as_ref()
            .is_some_and(|current| current.id == execution.id)
        {
            return false;
        }
        let pending: Vec<String> = execution
            .pending_notices
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter_map(|notice| job_notices::notice_id(notice.details.as_ref()).map(str::to_owned))
            .collect();
        execution.jobs.release_deliveries(&pending);
        executions.current = None;
        core.run_control
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        activity.run = false;
        released = true;
        true
    });
    released
}

struct RunGuard {
    core: Arc<Core>,
    agent: Arc<Agent>,
    executions: SharedExecutions,
    execution: Arc<Execution>,
    completion: Completion,
    runtime: tokio::runtime::Handle,
    permit: Option<WorkPermit>,
    finished: bool,
}

impl RunGuard {
    fn finish(mut self, outcome: RunOutcome) {
        publish_completion(
            &self.core,
            &self.executions,
            &self.execution,
            &self.completion,
            outcome,
        );
        self.finished = true;
    }
}

fn publish_completion(
    core: &Core,
    executions: &Mutex<Executions>,
    execution: &Execution,
    completion: &Completion,
    outcome: RunOutcome,
) {
    if release_execution(core, executions, execution)
        && (!execution.delivery_only || execution.notice_turn_started.load(Ordering::SeqCst))
    {
        core.emit(&SessionEvent::AgentSettled {
            outcome: &outcome,
            aborted: outcome.is_cancelled(),
        });
    }
    completion.send_replace(Some(outcome));
}

pub(super) fn reject_execution(
    core: &Core,
    executions: &Mutex<Executions>,
    execution: &Execution,
    completion: &Completion,
    message: String,
) {
    let outcome = execution.control.settle(RunOutcome::Failed { message });
    publish_completion(core, executions, execution, completion, outcome);
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let core = Arc::clone(&self.core);
        let agent = Arc::clone(&self.agent);
        let executions = Arc::clone(&self.executions);
        let execution = Arc::clone(&self.execution);
        let completion = self.completion.clone();
        let permit = self.permit.take();
        let current = executions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .current
            .as_ref()
            .is_some_and(|current| current.id == execution.id);
        if !current {
            completion.send_replace(Some(RunOutcome::Failed {
                message: "The session prompt task ended before settlement.".into(),
            }));
            return;
        }
        execution
            .control
            .fail("The session prompt task ended before settlement.".into());
        agent.abort();
        drop(self.runtime.spawn(async move {
            // This share was admitted with the driver, so shutdown also
            // awaits its cleanup if the task unwinds after the gate closes.
            let _permit = permit;
            drain_execution(&core, &agent, &execution.control).await;
            let outcome = if let Some(error) = lock_gap(&core.gap).clone() {
                execution.control.fail(error)
            } else {
                execution.control.stopped().unwrap_or_else(|| {
                    execution
                        .control
                        .fail("The session prompt task ended before settlement.".into())
                })
            };
            publish_completion(&core, &executions, &execution, &completion, outcome);
        }));
    }
}

impl PromptExecution {
    fn pending_job_notices(&self) -> Vec<CustomMessage> {
        self.pending_notices
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    async fn deliver_notices(&self, execution: &Execution) -> Result<RunOutcome, String> {
        if let Some(stopped) = execution.control.stopped() {
            return Ok(stopped);
        }
        if self.closed_reason().is_some() {
            return Ok(RunOutcome::FinishedTurn);
        }
        let notices: Vec<CustomMessage> = self
            .jobs
            .claim_unreported()
            .iter()
            .map(job_notices::completion_notice)
            .collect();
        if notices.is_empty() {
            return Ok(RunOutcome::FinishedTurn);
        }
        if self.model().is_none() || !self.jobs.spend_wake() {
            self.pending_notices
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend(notices);
            return Ok(RunOutcome::FinishedTurn);
        }
        execution.notice_turn_started.store(true, Ordering::SeqCst);
        let mut messages = Vec::new();
        if let Some(update) = self.prepare_prompt_and_tools()? {
            messages.push(update.into());
        }
        messages.extend(
            self.pending_job_notices()
                .into_iter()
                .map(job_notices::live),
        );
        messages.extend(notices.into_iter().map(job_notices::live));
        self.run_agent_prompt(messages, &execution.control).await
    }

    /// The system message patching the sections the transcript replays,
    /// Pi's `_preparePromptAndToolLoadout`; it also activates the tools.
    fn prepare_prompt_and_tools(&self) -> Result<Option<SystemMessage>, String> {
        let mut active: Vec<Arc<AgentTool>> = self
            .active_tool_names
            .iter()
            .filter_map(|name| self.tools.iter().find(|tool| &tool.name == name))
            .cloned()
            .collect();
        // The MCP tools published now, one generation per server; the run
        // keeps these for its whole length.
        if let Some(mcp) = self.mcp.get() {
            active.extend(mcp.tools());
        }
        self.agent.set_tools(active);
        let mut options = self.prompt_options.clone();
        options.selected_tools = self.active_tool_names.clone();
        options.hidden_tools = Vec::new();
        let sections = build_system_prompt_sections(&options).map_err(|error| error.to_string())?;
        let systems: Vec<Message> = self
            .agent
            .messages()
            .iter()
            .filter_map(|message| message.as_system().cloned().map(Message::System))
            .collect();
        let previous = get_current_system_message(&systems)
            .and_then(|system| system.sections)
            .unwrap_or_default();
        Ok(
            diff_system_prompt_sections(&previous, &sections).map(|sections| SystemMessage {
                content: SystemContent::Text(String::new()),
                sections: Some(sections),
                tools_added: None,
                tools_removed: None,
                timestamp: bake_ai::now_ms(),
            }),
        )
    }

    async fn prompt_inner(
        &self,
        text: &str,
        images: Vec<ImageContent>,
        control: &RunControl,
    ) -> Result<RunOutcome, String> {
        if let Some(outcome) = control.stopped() {
            return Ok(outcome);
        }
        let docs_command = &self.docs_command;
        if let Some(reason) = self.closed_reason() {
            return Err(reason);
        }
        let Some(model) = self.model() else {
            return Err(format_no_model_selected_message(docs_command));
        };
        let registry = self.registry.snapshot();
        if !registry.has_configured_auth(&model.provider)
            && registry.check_auth(&model.provider).is_none()
        {
            return Err(format_no_api_key_found_message(
                &model.provider,
                docs_command,
            ));
        }
        // Compact before sending when the last response calls for it,
        // aborted ones included; the new prompt is sent either way.
        let last = self
            .agent
            .messages()
            .iter()
            .rev()
            .find_map(|message| message.as_assistant().cloned());
        if let Some(last) = last {
            self.core
                .check_compaction(&last, None, false, Vec::new())
                .await?;
        }
        if let Some(outcome) = control.stopped() {
            return Ok(outcome);
        }
        let (text, images) = self.prepare_user_input(text, images, Some(model)).await?;
        // Preparation may outlast a shutdown or a failed write.
        if let Some(reason) = self.closed_reason() {
            return Err(reason);
        }
        // The first prompt waits for MCP servers still connecting, so their
        // tools are declared in its first request, as Pi's
        // `waitForDirectServers` does.
        if let Some(mcp) = self.mcp.get() {
            mcp.wait_for_startup(|| self.core.aborted() || self.closed_reason().is_some())
                .await;
            if let Some(reason) = self.closed_reason() {
                return Err(reason);
            }
        }
        let mut content = vec![UserContentBlock::Text(TextContent::new(text))];
        content.extend(images.into_iter().map(UserContentBlock::Image));
        let mut messages: Vec<LiveMessage> = Vec::with_capacity(2);
        if let Some(update) = self.prepare_prompt_and_tools()? {
            messages.push(update.into());
        }
        self.jobs.reset_wakes();
        messages.extend(
            self.pending_job_notices()
                .into_iter()
                .map(job_notices::live),
        );
        messages.extend(
            self.jobs
                .claim_unreported()
                .iter()
                .map(|job| job_notices::live(job_notices::completion_notice(job))),
        );
        messages.push(
            UserMessage {
                content: UserContent::Blocks(content),
                timestamp: bake_ai::now_ms(),
            }
            .into(),
        );
        self.run_agent_prompt(messages, control).await
    }

    /// Pi's `_runAgentPrompt`: the run, then retries, overflow recovery,
    /// and queued messages until the session settles.
    async fn run_agent_prompt(
        &self,
        messages: Vec<LiveMessage>,
        control: &RunControl,
    ) -> Result<RunOutcome, String> {
        let core = &self.core;
        {
            let mut state = core.state();
            state.last_assistant = None;
            state.last_tool_results.clear();
            state.turn_tool_results.clear();
        }
        let mut result = if core.aborted() {
            Ok(control.stopped().unwrap_or(RunOutcome::Cancelled))
        } else {
            self.agent
                .prompt_with_control(PromptInput::Messages(messages), control.clone())
                .await
                .map_err(|error: AgentError| error.to_string())
        };
        while result.is_ok() && !core.aborted() {
            if matches!(result, Ok(RunOutcome::Cancelled)) {
                control.cancel();
                break;
            }
            match self.handle_post_agent_run().await {
                Ok(true) => {
                    if core.aborted() {
                        break;
                    }
                    result = self
                        .agent
                        .continue_with_control(control.clone())
                        .await
                        .map_err(|e| e.to_string());
                    continue;
                }
                Ok(false) => {}
                Err(error) => {
                    result = Err(error);
                    break;
                }
            }
            if core.aborted() {
                break;
            }
            if self.agent.has_queued_messages() {
                result = self
                    .agent
                    .continue_with_control(control.clone())
                    .await
                    .map_err(|e| e.to_string());
                continue;
            }
            let notices: Vec<LiveMessage> = self
                .jobs
                .claim_unreported()
                .iter()
                .map(|job| job_notices::live(job_notices::completion_notice(job)))
                .collect();
            if notices.is_empty() {
                break;
            }
            result = self
                .agent
                .prompt_with_control(PromptInput::Messages(notices), control.clone())
                .await
                .map_err(|error| error.to_string());
        }
        result
    }

    /// Pi's `_handlePostAgentRun`: whether to continue the agent.
    async fn handle_post_agent_run(&self) -> Result<bool, String> {
        let core = &self.core;
        let (last, tool_results) = {
            let mut state = core.state();
            (
                state.last_assistant.take(),
                std::mem::take(&mut state.last_tool_results),
            )
        };
        if core.aborted() {
            core.finish_cancelled_retry();
            return Ok(false);
        }
        let Some((message, entry_id)) = last else {
            return Ok(self.agent.has_queued_messages());
        };
        if core.is_retryable_error(&message)
            && core.prepare_retry(&message, entry_id.clone()).await?
        {
            if core.aborted() {
                core.finish_cancelled_retry();
            }
            return Ok(!core.aborted());
        }
        if core.aborted() {
            core.finish_cancelled_retry();
            return Ok(false);
        }
        if message.stop_reason == bake_ai::StopReason::Error {
            let attempt = std::mem::take(&mut core.state().retry_attempt);
            if attempt > 0 {
                core.emit(&SessionEvent::AutoRetryEnd {
                    success: false,
                    attempt,
                    final_error: message.error_message.clone(),
                });
            }
        }
        if core
            .check_compaction(&message, entry_id, true, tool_results)
            .await?
        {
            return Ok(!core.aborted());
        }
        // The loop drains both queues before `agent_end`; messages queued
        // by `agent_end` listeners need a fresh run.
        Ok(!core.aborted() && self.agent.has_queued_messages())
    }
}

pub(super) async fn prepare_user_input(
    work: &WorkGate,
    prompt_options: &SystemPromptOptions,
    prompt_templates: &[PromptTemplate],
    settings: &SharedSettings,
    text: &str,
    images: Vec<ImageContent>,
    model: Option<Model>,
) -> Result<(String, Vec<ImageContent>), String> {
    let shut_down = || "The session is shut down.".to_owned();
    let mut text = if text.starts_with("/skill:") {
        let skills = prompt_options.skills.clone();
        let command = text.to_owned();
        // The gate owns the work, so shutdown waits for it even when
        // this future is dropped.
        work.spawn_blocking(move || expand_skill_command(&command, &skills))
            .map_err(|_| shut_down())?
            .await
            .unwrap_or_else(|_| text.to_owned())
    } else {
        text.to_owned()
    };
    text = expand_prompt_template(&text, prompt_templates);
    if images.is_empty() {
        return Ok((text, images));
    }
    let auto_resize = read_settings(settings).image_auto_resize();
    let resize_options = model
        .and_then(|model| ImageResizeOptions::from_input_limits(model.input_limits.as_ref()))
        .unwrap_or_default();
    let (normalized, hints) = work
        .spawn_blocking(move || normalize_prompt_images(&images, auto_resize, resize_options))
        .map_err(|_| shut_down())?
        .await
        .map_err(|error| format!("Image processing task failed: {error}"))?;
    if !hints.is_empty() {
        text = format!("{text}\n\n{}", hints.join("\n"));
    }
    Ok((text, normalized))
}
