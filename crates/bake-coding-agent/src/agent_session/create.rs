//! Building a session ([`AgentSession::create`]): the initial model and
//! tools, and connecting the agent's events to the session
//! ([`AgentSession::connect`]).

use super::*;

/// `tool` as the session runs it. A tool that declares no effect, such as
/// an SDK or plugin tool, is [`ToolEffect::Exec`]: the session cannot see
/// what it does, so its calls run alone, in the order the model wrote them,
/// as the prompt's round-trip rule promises. A tool that explicitly sets
/// `execution_mode` to parallel keeps no effect, the documented opt-in to
/// run beside other calls as in Pi.
fn declare_effect(tool: Arc<AgentTool>) -> Arc<AgentTool> {
    if tool.effect.is_some() || tool.execution_mode == Some(ToolExecutionMode::Parallel) {
        return tool;
    }
    let mut declared = (*tool).clone();
    declared.effect = Some(ToolEffect::Exec);
    Arc::new(declared)
}

/// The profile of `model` in `settings` ([`Settings::model_profile`]);
/// empty without a model.
pub(super) fn model_profile(
    settings: &SharedSettings,
    model: Option<&Model>,
) -> crate::settings::ModelProfile {
    model
        .map(|model| read_settings(settings).model_profile(model))
        .unwrap_or_default()
}

/// The active tool names under `profile`: `base`, the session's, with the
/// profile's `tools` applied, of the session's `tools`. Without `bash` the
/// job tools are left out, as the session's selection leaves them out.
pub(super) fn profile_tool_names(
    profile: &crate::settings::ModelProfile,
    base: &[String],
    tools: &[Arc<AgentTool>],
) -> Vec<String> {
    if profile.tools.is_empty() {
        return base.to_vec();
    }
    let mut names: Vec<String> = profile
        .active_tools(base)
        .into_iter()
        .filter(|name| tools.iter().any(|tool| &tool.name == name))
        .collect();
    if !names.iter().any(|name| name == "bash") {
        names.retain(|name| !crate::tools::job_tools::JOB_TOOL_NAMES.contains(&name.as_str()));
    }
    names
}

/// The model and thinking level a session starts with, Pi's
/// `createAgentSession` selection.
struct Selection {
    model: Option<Model>,
    thinking_level: ModelThinkingLevel,
    fallback_message: Option<String>,
}

struct SelectionInputs<'a> {
    registry: &'a ModelRegistry,
    settings: &'a Settings,
    docs_command: &'a str,
    model: Option<Model>,
    thinking_level: Option<ModelThinkingLevel>,
}

fn select_model(
    inputs: SelectionInputs<'_>,
    has_existing_session: bool,
    session_model: Option<(String, String)>,
    session_thinking: Option<String>,
) -> Selection {
    let SelectionInputs {
        registry,
        settings,
        docs_command,
        mut model,
        thinking_level: explicit_thinking,
    } = inputs;
    let mut fallback_message = None;
    if model.is_none()
        && has_existing_session
        && let Some((provider, id)) = &session_model
    {
        model = registry
            .model(provider, id)
            .filter(|restored| registry.has_configured_auth(&restored.provider));
        if model.is_none() {
            fallback_message = Some(format!("Could not restore model {provider}/{id}"));
        }
    }
    if model.is_none() {
        let initial = find_initial_model(
            settings.default_provider.as_deref(),
            settings.default_model.as_deref(),
            settings.default_thinking_level,
            &settings.model_thinking_levels,
            registry,
        );
        model = initial.model;
        fallback_message = match (&model, fallback_message) {
            (None, _) => Some(format_no_models_available_message(docs_command)),
            (Some(model), Some(message)) => {
                Some(format!("{message}. Using {}/{}", model.provider, model.id))
            }
            (Some(_), None) => None,
        };
    }
    let mut thinking_level = explicit_thinking;
    if thinking_level.is_none() && has_existing_session {
        thinking_level = Some(
            session_thinking
                .as_deref()
                .and_then(crate::model_registry::resolver::parse_thinking_level)
                .or(settings.default_thinking_level)
                .unwrap_or(DEFAULT_THINKING_LEVEL),
        );
    }
    if thinking_level.is_none()
        && let Some(model) = &model
    {
        thinking_level = settings.thinking_level_for(model);
    }
    let thinking_level = thinking_level
        .or(settings.default_thinking_level)
        .unwrap_or(DEFAULT_THINKING_LEVEL);
    let thinking_level = match &model {
        None => ModelThinkingLevel::Off,
        Some(model) => bake_ai::models::clamp_thinking_level(model, thinking_level),
    };
    Selection {
        model,
        thinking_level,
        fallback_message,
    }
}

impl AgentSession {
    /// Pi's `createAgentSession`: selects the model and thinking level,
    /// restores the transcript, records a new session's initial model and
    /// thinking level as one group ([`SessionManager::append_group`]), and
    /// creates the agent. Must run inside a Tokio runtime.
    pub async fn create(options: AgentSessionOptions) -> Result<Self, String> {
        let AgentSessionOptions {
            session,
            registry,
            apis,
            settings,
            settings_store,
            selection: published,
            model,
            thinking_level,
            tools,
            active_tool_names,
            tool_prompts,
            mut system_prompt,
            docs_command,
            plugins,
            work,
            jobs,
        } = options;
        let max_concurrent_read_tools = settings.read_tool_concurrency()?;
        // A malformed model profile stops the session, as a malformed
        // `permission` section does.
        settings.model_profiles()?;
        let nested = nested_context::NestedContext::new(
            settings.nested_context_files()? && !settings.no_context_files,
            &system_prompt.cwd,
            &system_prompt.context_files,
        );
        let plugins = VetoDispatcher::new(plugins).map(Arc::new);
        system_prompt.docs_command = docs_command.clone();
        let task_documents_enabled = settings.task_documents;
        if task_documents_enabled && tools.iter().any(|tool| tool.name == task_documents::NAME) {
            return Err(
                "The task_document tool name is reserved when taskDocuments is enabled.".into(),
            );
        }
        let schedules_enabled = settings.schedules;
        if schedules_enabled
            && tools
                .iter()
                .any(|tool| schedules::NAMES.contains(&tool.name.as_str()))
        {
            return Err("The schedule tool names are reserved when schedules is enabled.".into());
        }
        let mut tools = tools;
        // The session's own `codemode` tool, registered unless the caller
        // gives one; it is active when `active_tool_names` names it, as the
        // CLI's defaults do, and its guideline then joins the rules.
        let own_codemode = !tools
            .iter()
            .any(|tool| tool.name == crate::tools::codemode::CODEMODE_TOOL_NAME);
        let mut tool_prompts = tool_prompts;
        if own_codemode
            && !tool_prompts
                .iter()
                .any(|prompt| prompt.name == crate::tools::codemode::CODEMODE_TOOL_NAME)
        {
            tool_prompts.push(crate::tools::codemode::codemode_tool_prompt());
        }
        // Pi's `_toolPromptGuidelines`, which the codemode tool shows with
        // the declarations it lists.
        let tool_guidelines: Arc<std::collections::HashMap<String, Vec<String>>> = Arc::new(
            tool_prompts
                .iter()
                .map(|prompt| {
                    (
                        prompt.name.clone(),
                        normalize_prompt_guidelines(&prompt.guidelines),
                    )
                })
                .collect(),
        );
        for prompt in &tool_prompts {
            let guidelines = normalize_prompt_guidelines(&prompt.guidelines);
            if !guidelines.is_empty() {
                system_prompt
                    .tool_guidelines
                    .retain(|(name, _)| name != &prompt.name);
                system_prompt
                    .tool_guidelines
                    .push((prompt.name.clone(), guidelines));
            }
        }
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| "An agent session must be created inside a Tokio runtime.".to_owned())?;
        let session: SharedSession = Arc::new(Mutex::new(session));
        let work = work.unwrap_or_default();
        if jobs.as_ref().is_some_and(|jobs| !jobs.is_owned_by(&work)) {
            return Err("The session's job registry must be built over its work gate.".to_owned());
        }
        let jobs = jobs.unwrap_or_else(|| JobRegistry::new(work.clone(), JobLimits::default()));
        // Jobs an earlier process ran: their ids stay reserved, and those
        // never reported carry their recorded outcome with the next prompt.
        let pending_notices = {
            let session = lock(&session);
            let mut notices = job_notices::recover(&session, &jobs);
            notices.extend(crate::subagents::lost_runs(&session));
            Arc::new(Mutex::new(notices))
        };
        let (context, has_thinking_entry, nested_paths) =
            with_session(&work, &session, |session| {
                let context = session.build_session_context();
                let branch = session.branch_entries(None);
                let has_thinking_entry = branch
                    .iter()
                    .any(|entry| entry.entry_type() == "thinking_level_change");
                (
                    context,
                    has_thinking_entry,
                    nested_context::recorded_paths(&branch),
                )
            })
            .await?;
        nested.restore(&nested_paths);
        let has_existing_session = !context.messages.is_empty();
        let snapshot = registry.snapshot();
        let selection = select_model(
            SelectionInputs {
                registry: &snapshot,
                settings: &settings,
                docs_command: &docs_command,
                model,
                thinking_level,
            },
            has_existing_session,
            context
                .model
                .as_ref()
                .map(|model| (model.provider.clone(), model.model_id.clone())),
            has_thinking_entry.then(|| context.thinking_level.clone()),
        );
        let messages: Vec<LiveMessage> = context
            .messages
            .iter()
            .map(crate::session::AgentMessage::to_agent)
            .collect();
        let block = settings.block_images;
        let session_id = lock(&session).session_id().to_owned();
        let stream_fn = with_tool_argument_whitespace(
            registry.stream_fn_owned(Arc::clone(&apis), Some(work.clone())),
            settings.max_tool_argument_whitespace,
        );
        let listeners = Arc::new(Listeners::default());
        let steering_mode = settings.steering_mode;
        let follow_up_mode = settings.follow_up_mode;
        let auto_resize_images = settings.image_auto_resize();
        let settings: SharedSettings = Arc::new(std::sync::RwLock::new(settings));
        let published = published.unwrap_or_default();
        *published.lock() = Selected {
            model: selection.model.clone(),
            thinking_level: selection.thinking_level,
        };
        let core = Arc::new(Core {
            session: Arc::clone(&session),
            settings: Arc::clone(&settings),
            listeners: Arc::clone(&listeners),
            stream_fn: Arc::clone(&stream_fn),
            work: work.clone(),
            agent: OnceLock::new(),
            has_model: AtomicBool::new(selection.model.is_some()),
            abort_requested: AtomicBool::new(false),
            run_control: Mutex::new(None),
            background_compaction: background_compaction::BackgroundCompaction::default(),
            cached_requests: Arc::default(),
            state: Mutex::new(RunState::default()),
            activity: tokio::sync::watch::Sender::new(Activity::default()),
            gap: Arc::default(),
            nested,
            #[cfg(test)]
            hooks: Mutex::default(),
            #[cfg(test)]
            session_io: tokio::sync::watch::Sender::new(0),
        });
        if task_documents_enabled {
            tools.push(task_documents::tool(Arc::clone(&core)));
        }
        let mcp: Arc<OnceLock<McpServers>> = Arc::default();
        let codemode_host = Arc::new(codemode::SessionCodemodeHost {
            core: Arc::clone(&core),
            guidelines: tool_guidelines,
            mcp: Arc::clone(&mcp),
            own_descriptions: Mutex::default(),
        });
        if own_codemode {
            tools.push(Arc::new(crate::tools::codemode::create_codemode_tool(
                Some(Arc::clone(&codemode_host) as Arc<dyn crate::tools::codemode::CodemodeHost>),
            )));
        }
        let hidden_declarations: codemode::HiddenDeclarations = Arc::default();
        let hidden_in_requests = Arc::clone(&hidden_declarations);
        // One conversion for the agent's requests and for a summary that
        // repeats them ([`cached_compaction`]).
        let request_messages: cached_compaction::RequestMessages =
            Arc::new(move |messages: &[LiveMessage]| {
                let converted =
                    codemode::hide_declarations(convert_to_llm(messages), &hidden_in_requests);
                if block {
                    block_images(converted)
                } else {
                    converted
                }
            });
        let _ = core
            .cached_requests
            .convert
            .set(Arc::clone(&request_messages));
        let agent_stream_fn = cached_compaction::capturing(
            Arc::clone(&stream_fn),
            Arc::clone(&settings),
            Arc::clone(&core.cached_requests),
        );
        let schedules = schedules_enabled.then(|| Arc::new(schedules::Schedules::new()));
        if let Some(schedules) = &schedules {
            tools.extend(schedules::tools(&core, schedules));
        }
        let tools: Vec<Arc<AgentTool>> = tools.into_iter().map(declare_effect).collect();
        let after_tool_core = Arc::clone(&core);
        let persist_error = Arc::new(Mutex::new(None));
        let executions: prompt_execution::SharedExecutions = Arc::default();
        let before_tool_call = plugins.as_ref().map(|plugins| {
            plugin_veto::plugin_veto_hook(
                Arc::clone(&core),
                Arc::clone(&persist_error),
                Arc::clone(plugins),
            )
        });
        let shared_hooks: hooks::SharedHooks = Arc::default();
        let before_tool_call = Some(hooks::before_tool_call(
            Arc::clone(&core),
            Arc::clone(&persist_error),
            Arc::clone(&shared_hooks),
            before_tool_call,
        ));
        let before_tool_call = Some(run_journal::tool_hook(
            Arc::clone(&executions),
            before_tool_call,
        ));
        let mut agent_options = AgentOptions {
            max_concurrent_read_tools,
            initial_state: AgentInitialState {
                system_prompt: None,
                model: selection.model.clone(),
                thinking_level: Some(selection.thinking_level),
                tools: Vec::new(),
                messages,
            },
            convert_to_llm: Some(hook::convert_to_llm(move |messages| {
                let converted = request_messages(messages);
                Box::pin(async move { converted })
            })),
            after_tool_call: Some(hook::after_tool_call(move |context, _signal| {
                // The directories a successful file tool call makes the
                // session check for context files, outermost first.
                let directories = if context.is_error {
                    Vec::new()
                } else {
                    after_tool_core
                        .nested
                        .directories(&context.tool_call.name, context.args)
                };
                let nested_core = Arc::clone(&after_tool_core);
                let content = context.result.content.clone();
                let details = context_masking::observation_details(
                    &context,
                    read_settings(&after_tool_core.settings).context_masking,
                );
                let resize_options = after_tool_core
                    .model()
                    .and_then(|model| {
                        ImageResizeOptions::from_input_limits(model.input_limits.as_ref())
                    })
                    .unwrap_or_default();
                let work = after_tool_core.work.clone();
                Box::pin(async move {
                    if !directories.is_empty() {
                        nested_context::find(&nested_core, directories).await;
                    }
                    if !content
                        .iter()
                        .any(|block| matches!(block, UserContentBlock::Image(_)))
                    {
                        return Ok(details.map(|details| AfterToolCallResult {
                            details: Some(details),
                            ..AfterToolCallResult::default()
                        }));
                    }
                    // Owned by the gate, so a cancelled run's resize is
                    // still awaited by shutdown.
                    let Ok(normalizing) = work.spawn_blocking(move || {
                        normalize_tool_result_images(&content, auto_resize_images, resize_options)
                    }) else {
                        return Ok(None);
                    };
                    let normalized = normalizing.await.ok().flatten();
                    Ok(if normalized.is_some() || details.is_some() {
                        Some(AfterToolCallResult {
                            content: normalized,
                            details,
                            ..AfterToolCallResult::default()
                        })
                    } else {
                        None
                    })
                })
            })),
            before_tool_call,
            finish_turn: Some(hooks::finish_turn(Arc::clone(&shared_hooks))),
            prepare_request: (task_documents_enabled || read_settings(&settings).context_masking)
                .then(|| context_masking::request_hook(Arc::clone(&core), task_documents_enabled)),
            stream_fn: Some(agent_stream_fn),
            prepare_next_turn_with_context: Some(next_turn_hook(
                Arc::clone(&core),
                published.clone(),
                jobs.clone(),
                Arc::clone(&shared_hooks),
            )),
            steering_mode,
            follow_up_mode,
            session_id: Some(session_id),
            ..AgentOptions::default()
        };
        // `PostToolUse` hooks see a result before its images are normalized.
        agent_options.after_tool_call = agent_options.after_tool_call.map(|normalize| {
            hooks::after_tool_call(
                Arc::clone(&core),
                Arc::clone(&persist_error),
                Arc::clone(&shared_hooks),
                normalize,
            )
        });
        let tool_hooks = run_journal::ToolHooks {
            before: agent_options.before_tool_call.clone(),
            after: agent_options.after_tool_call.clone(),
        };
        let agent = Agent::new(agent_options).map_err(|error| error.to_string())?;
        let agent = Arc::new(agent);
        let _ = core.agent.set(Arc::downgrade(&agent));

        let model = selection.model.clone();
        let thinking = selection.thinking_level;
        with_session(&work, &session, move |session| {
            if has_existing_session {
                if !has_thinking_entry {
                    session.append_thinking_level_change(thinking.as_str())?;
                }
            } else {
                // One group. A new session writes nothing until its first
                // message, which creates the file with both entries; a
                // written file without a conversation gets them in order.
                let mut group = Vec::with_capacity(2);
                if let Some(model) = &model {
                    group.push(GroupEntry::model_change(&model.provider, &model.id));
                }
                group.push(GroupEntry::thinking_level_change(thinking.as_str()));
                session.append_group(group)?;
            }
            Ok::<(), crate::session::SessionError>(())
        })
        .await?
        .map_err(|error| error.to_string())?;

        let active_tool_names: Vec<String> = active_tool_names
            .into_iter()
            .filter(|name| tools.iter().any(|tool| &tool.name == name))
            .collect();
        job_journal::attach(&core, &jobs)?;
        if let Some(shared) = &schedules {
            schedules::start(&core, jobs.clone(), Arc::clone(shared))?;
        }
        // Last, after every fallible step: the keeper of the Wasm plugins'
        // helpers is owned work of the gate from here on.
        if let Some(plugins) = &plugins {
            plugins.attach(&work).map_err(refused_message)?;
        }
        let gap = Arc::clone(&core.gap);
        let created = Self {
            apis,
            agent,
            core,
            executions,
            work,
            jobs,
            pending_notices,
            session,
            registry,
            settings,
            settings_store,
            selection: published,
            control: Arc::default(),
            tools,
            active_tool_names,
            prompt_options: system_prompt,
            prompt_templates: Vec::new(),
            docs_command,
            listeners,
            persist_error,
            subscription: Arc::default(),
            shutdown: OnceLock::new(),
            runtime,
            model_fallback_message: selection.fallback_message,
            queue: Arc::default(),
            write_failure: gap,
            mcp,
            codemode: codemode_host,
            hidden_declarations,
            plugins,
            hooks: shared_hooks,
            tool_hooks,
            subagents: None,
            telecom: None,
            telecom_subscription: None,
            questions: None,
            spawned: None,
            permission: None,
            #[cfg(test)]
            schedules,
        };
        created.connect();
        Ok(created)
    }

    /// Commits finished messages before publishing them to observers.
    /// Persistence participates in execution: its failure stops the run.
    fn connect(&self) {
        let core = Arc::clone(&self.core);
        let persist_error = Arc::clone(&self.persist_error);
        let queue = Arc::clone(&self.queue);
        let write_failure = Arc::clone(&self.write_failure);
        let jobs = self.jobs.clone();
        let pending_notices = Arc::clone(&self.pending_notices);
        let executions = Arc::clone(&self.executions);
        // Weak: the agent owns this listener.
        let agent = Arc::downgrade(&self.agent);
        let subscription = self.agent.subscribe_critical(move |event, _signal| {
            let core = Arc::clone(&core);
            let persist_error = Arc::clone(&persist_error);
            let queue = Arc::clone(&queue);
            let write_failure = Arc::clone(&write_failure);
            let jobs = jobs.clone();
            let pending_notices = Arc::clone(&pending_notices);
            let executions = Arc::clone(&executions);
            let agent = agent.clone();
            Box::pin(async move {
                if let AgentEvent::MessageStart { message } = event
                    && let Some(user) = message.as_user()
                {
                    core.state().overflow_recovery_attempted = false;
                    // Pi removes a delivered prompt from its queue before
                    // emitting the message, so listeners see the new queue.
                    // A prompt of images alone is queued as empty text and
                    // leaves the queue the same way (Pi v1.5.2, #8612).
                    let text = bake_ai::utils::text::user_content_text(&user.content, "");
                    let delivered = queue.lock().delivered(&text);
                    if delivered {
                        queue.announce(&core);
                    }
                }
                if let AgentEvent::MessageEnd { message } = event {
                    let entry_id = persist_message(&core, &persist_error, message).await;
                    if matches!(entry_id, Ok(Some(_)))
                        && let Some(id) = job_notices::observation_id(message)
                    {
                        #[cfg(test)]
                        {
                            let hook = core.hooks().after_job_observation.take();
                            if let Some(hook) = hook {
                                hook();
                            }
                        }
                        jobs.acknowledge(id);
                        pending_notices
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .retain(|notice| {
                                job_notices::notice_id(notice.details.as_ref()) != Some(id)
                            });
                    }
                    // A posted notice waiting for a prompt is delivered
                    // once: its durable entry takes it off the list.
                    if matches!(entry_id, Ok(Some(_)))
                        && let bake_agent::AgentMessage::Custom(custom) = message
                        && let Some(custom) = custom.as_any().downcast_ref::<CustomMessage>()
                        && custom.custom_type != job_notices::NOTICE_TYPE
                    {
                        pending_notices
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .retain(|notice| {
                                notice.custom_type != custom.custom_type
                                    || notice.details != custom.details
                            });
                    }
                    if entry_id.is_err() {
                        let failed = persist_error
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .clone();
                        write_failure
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .get_or_insert(failed.unwrap_or_default());
                        if let Some(agent) = agent.upgrade() {
                            agent.abort();
                        }
                        return;
                    }
                    let entry_id = entry_id.unwrap_or_default();
                    if let Some(journal) = run_journal::current(&executions)
                        && let Err(error) = journal.message(message, entry_id.clone()).await
                    {
                        persist_error
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .get_or_insert(error.clone());
                        if let Some(control) = core.control() {
                            control.fail(error);
                        }
                        if let Some(agent) = agent.upgrade() {
                            agent.abort();
                        }
                        return;
                    }
                    track_message(&core, message, entry_id);
                }
                match event {
                    AgentEvent::AgentEnd { messages } => {
                        let will_retry = core.will_retry_after_agent_end(messages);
                        core.emit(&SessionEvent::AgentEnd {
                            messages,
                            will_retry,
                        });
                    }
                    event => core.emit(&SessionEvent::Agent(event)),
                }
            })
        });
        *self
            .subscription
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(subscription);
    }
}
