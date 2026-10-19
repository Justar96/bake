//! RPC mode: one long-lived session driven by JSONL commands on standard
//! input, with responses and session events on standard output.
//!
//! Ported from Pi v1.3.0 `packages/coding-agent/src/modes/rpc/`
//! (`71023aec1546d6d677a65669c5665c0927ce0dd0`): `rpc-mode.ts`'s loop,
//! commands, and shutdown, `rpc-types.ts`'s records ([`types`]), and
//! `jsonl.ts`'s framing ([`jsonl`]). The events are the ones `--mode json`
//! writes ([`crate::json_event`]), without print mode's session header,
//! which Pi's RPC mode does not write either. `docs/rpc.md`,
//! `docs/rpc-commands.md`, and `docs/rpc-extension-ui.md` describe the
//! protocol; `bake-rs docs rpc` describes this port.
//!
//! What differs from Pi, in each case because Bake's Rust agent has no
//! equivalent subsystem:
//!
//! - **Extensions are deferred (D43)**, so there is no extension UI, no
//!   extension command, and no `extension_error` record. The one
//!   `extension_ui_request` this mode sends is Bake's approval question
//!   ([`approvals`]), which a Pi client can answer. `get_commands` lists
//!   the prompt templates and skills a prompt can invoke.
//! - **`bash` and `abort_bash` are refused**: the `!command` subsystem
//!   Pi's `bash` command drives has no Rust port, so there is no shell
//!   execution outside the model's `bash` tool.
//! - **`set_auto_retry` is refused**: automatic retry follows
//!   `settings.json`'s `retry.enabled`, which no session API changes.
//! - **A prompt is never `handled`**: that disposition reports an
//!   extension command or input handler consuming the prompt, so this
//!   mode answers `started` or `queued`.
//! - **Commands are handled in the order they arrive**, one at a time,
//!   where Pi starts each line's handler concurrently. Only `prompt` runs
//!   on its own task, as it does in Pi, so `abort`, `steer`, `follow_up`,
//!   and `clear_queue` are handled while a turn runs.
//!
//! Shutdown follows Pi: standard input reaching its end shuts the session
//! down and exits 0, and `SIGTERM` and `SIGHUP` exit 143 and 129
//! ([`crate::cli::CliEnvironment::interrupt`]). Either way the session's
//! shutdown stops its MCP servers and background shells and awaits their
//! process trees, the approval broker closes so no queued request is left
//! waiting, the prompt task is awaited, and the writer is flushed before
//! the status is returned.

pub mod approvals;
pub mod jsonl;
pub mod types;

use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use bake_agent::AgentEvent;
use bake_ai::{Model, ModelThinkingLevel};
use serde_json::{Map, Value, json};
use tokio::sync::oneshot;

use crate::agent_session::{
    AgentSession, ForkPosition, SessionEvent, SessionStats, SessionSubscription,
};
use crate::cli::{Interrupt, Replacement, SessionFactory, SessionTarget};
use crate::json_event::session_event_json;
use crate::permissions::ApprovalBroker;
use crate::print_mode::{SharedWriter, write_out};
use crate::rpc::approvals::ApprovalBridge;
use crate::rpc::jsonl::{read_lines, serialize_json_line};
use crate::rpc::types::{
    ExtensionUiResponse, Record, RpcCommand, StreamingBehavior, error_response, read_record,
    success_response,
};
use crate::session::SessionTree;

/// What RPC mode reads, writes, and replaces sessions with.
pub struct RpcModeOptions {
    /// The protocol's input. Read on its own thread, since a blocking
    /// standard-input read cannot be cancelled.
    pub stdin: Box<dyn Read + Send>,
    /// The protocol's output: responses, events, and requests.
    pub stdout: SharedWriter,
    /// Diagnostics, which are never protocol records.
    pub stderr: SharedWriter,
    /// The approval queue this mode answers (D24); without one, every
    /// escalation fails closed.
    pub approvals: Option<ApprovalBroker>,
    /// Builds the sessions `new_session`, `switch_session`, `fork`, and
    /// `clone` replace the running one with; without one they are refused.
    pub factory: Option<SessionFactory>,
    /// Resolves with the exit status when the process should stop.
    pub interrupt: Interrupt,
}

/// Runs Pi's RPC mode over `session` until standard input ends or a stop
/// request arrives; returns the exit status. The session is shut down
/// before this returns. Must run inside a multi-threaded Tokio runtime.
pub async fn run_rpc_mode(session: AgentSession, options: RpcModeOptions) -> i32 {
    let RpcModeOptions {
        stdin,
        stdout,
        stderr,
        approvals,
        factory,
        mut interrupt,
    } = options;
    let mut lines = input_lines(stdin);
    let bridge = approvals
        .as_ref()
        .and_then(|broker| ApprovalBridge::connect(broker, &stdout));
    let mut rpc = Rpc {
        session: Arc::new(session),
        factory,
        out: stdout,
        stderr,
        approvals,
        bridge,
        subscription: None,
        run_started: Arc::new(Mutex::new(None)),
        prompts: Vec::new(),
    };
    rpc.subscribe();
    let status = loop {
        tokio::select! {
            line = lines.recv() => match line {
                Some(line) => rpc.handle_line(&line).await,
                // Pi's `process.stdin.on("end")`: an orderly shutdown.
                None => break 0,
            },
            status = &mut interrupt => break status,
        }
    };
    rpc.shutdown().await;
    status
}

/// Reads `input` on its own thread, sending each JSONL record; the channel
/// closes at the input's end. The thread is not joined: a blocking read
/// cannot be cancelled, and the process owns its standard input.
fn input_lines(input: Box<dyn Read + Send>) -> tokio::sync::mpsc::UnboundedReceiver<String> {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("bake-rpc-stdin".into())
        .spawn(move || {
            read_lines(input, |line| {
                // A closed receiver means the loop has stopped; stop reading.
                let _ = sender.send(line);
            });
        })
        .ok();
    receiver
}

/// The RPC loop's state: the session in use, how to replace it, and the
/// records it has sent.
struct Rpc {
    session: Arc<AgentSession>,
    factory: Option<SessionFactory>,
    out: SharedWriter,
    stderr: SharedWriter,
    approvals: Option<ApprovalBroker>,
    bridge: Option<Arc<ApprovalBridge>>,
    subscription: Option<SessionSubscription>,
    /// Set while a prompt waits to learn whether its run started.
    run_started: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    /// The prompt tasks, which answer their own commands. Each is owned
    /// until it ends, so shutdown awaits every response it can still write.
    prompts: Vec<tokio::task::JoinHandle<()>>,
}

impl Rpc {
    /// Writes one protocol record.
    fn send(&self, record: &Value) {
        write_out(&self.out, &serialize_json_line(record));
    }

    /// Writes one protocol record already serialized, without its LF.
    fn send_line(&self, line: &str) {
        write_out(&self.out, &format!("{line}\n"));
    }

    /// Subscribes to the session's events, replacing any earlier
    /// subscription, as Pi's `rebindSession` does.
    fn subscribe(&mut self) {
        if let Some(subscription) = self.subscription.take() {
            subscription.unsubscribe();
        }
        let out = Arc::clone(&self.out);
        let started = Arc::clone(&self.run_started);
        self.subscription = Some(self.session.subscribe(move |event| {
            write_out(&out, &serialize_json_line(&session_event_json(event)));
            if matches!(event, SessionEvent::Agent(AgentEvent::AgentStart))
                && let Some(sender) = started
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
            {
                let _ = sender.send(());
            }
        }));
    }

    /// Pi's `handleInputLine`.
    async fn handle_line(&mut self, line: &str) {
        match read_record(line) {
            Record::Command(id, command) => {
                if let Some(response) = self.handle(id, command).await {
                    self.send(&response);
                }
            }
            Record::ExtensionUiResponse(response) => self.answer(&response),
            Record::Invalid(response) => self.send(&response),
        }
    }

    /// Settles the approval request an answer names; an answer for no
    /// request is ignored, as in Pi.
    fn answer(&self, response: &ExtensionUiResponse) {
        if let Some(bridge) = &self.bridge {
            bridge.answer(response);
        }
    }

    /// Pi's `handleCommand`: the response to send, or `None` when the
    /// command answers itself.
    async fn handle(&mut self, id: Option<String>, command: RpcCommand) -> Option<Value> {
        let kind = command.kind();
        let ok = |data: Option<Value>| Some(success_response(id.clone(), kind, data));
        let failed = |message: String| Some(error_response(id.clone(), kind, &message));
        match command {
            RpcCommand::Prompt {
                message,
                images,
                streaming_behavior,
            } => {
                // Pi's `prompt`: a streaming behavior applies only while a
                // turn runs; otherwise the prompt starts one.
                match streaming_behavior.filter(|_| self.session.is_streaming()) {
                    Some(StreamingBehavior::Steer) => {
                        match self.session.steer(&message, images).await {
                            Ok(()) => ok(Some(json!({ "disposition": "queued" }))),
                            Err(message) => failed(message),
                        }
                    }
                    Some(StreamingBehavior::FollowUp) => {
                        match self.session.follow_up(&message, images).await {
                            Ok(()) => ok(Some(json!({ "disposition": "queued" }))),
                            Err(message) => failed(message),
                        }
                    }
                    None => {
                        self.start_prompt(id, message, images).await;
                        None
                    }
                }
            }
            RpcCommand::Steer { message, images } => match self.session.steer(&message, images).await
            {
                Ok(()) => ok(Some(json!({ "disposition": "queued" }))),
                Err(message) => failed(message),
            },
            RpcCommand::FollowUp { message, images } => {
                match self.session.follow_up(&message, images).await {
                    Ok(()) => ok(Some(json!({ "disposition": "queued" }))),
                    Err(message) => failed(message),
                }
            }
            RpcCommand::Abort => {
                self.session.abort().await;
                ok(None)
            }
            RpcCommand::ClearQueue => {
                let cleared = self.session.clear_queue();
                ok(Some(json!({
                    "steering": cleared.steering,
                    "followUp": cleared.follow_up,
                })))
            }
            RpcCommand::NewSession { parent_session } => {
                // Pi's `newSession` takes the parent session's path; a new
                // session of this working directory needs none.
                if parent_session.is_some() {
                    return failed(
                        "new_session does not take parentSession in bake-rs; use fork or clone."
                            .to_owned(),
                    );
                }
                match self.replace(SessionTarget::New).await {
                    Ok(()) => ok(Some(json!({ "cancelled": false }))),
                    Err(message) => failed(message),
                }
            }
            RpcCommand::GetState => ok(Some(self.state())),
            RpcCommand::SetModel { provider, model_id } => {
                let model = self
                    .session
                    .model_runtime()
                    .snapshot()
                    .model(&provider, &model_id);
                match model {
                    None => failed(format!("Model not found: {provider}/{model_id}")),
                    Some(model) => match self.session.set_model(model.clone(), false).await {
                        Ok(_) => ok(Some(to_value(&model))),
                        Err(message) => failed(message),
                    },
                }
            }
            RpcCommand::CycleModel => match self.next_model() {
                None => ok(Some(Value::Null)),
                Some(model) => match self.session.set_model(model.clone(), false).await {
                    Ok(_) => ok(Some(json!({
                        "model": to_value(&model),
                        "thinkingLevel": self.session.thinking_level().as_str(),
                        // Bake's session keeps no `--models` scope to cycle.
                        "isScoped": false,
                    }))),
                    Err(message) => failed(message),
                },
            },
            RpcCommand::GetAvailableModels => ok(Some(json!({
                "models": to_value(&self.session.model_runtime().snapshot().available()),
            }))),
            RpcCommand::SetThinkingLevel { level } => {
                match self.session.set_thinking_level(level, false).await {
                    Ok(_) => ok(None),
                    Err(message) => failed(message),
                }
            }
            RpcCommand::CycleThinkingLevel => match self.next_thinking_level() {
                None => ok(Some(Value::Null)),
                Some(level) => match self.session.set_thinking_level(level, false).await {
                    Ok(_) => ok(Some(json!({ "level": level.as_str() }))),
                    Err(message) => failed(message),
                },
            },
            RpcCommand::GetAvailableThinkingLevels => ok(Some(json!({
                "levels": levels(&self.session.available_thinking_levels()),
            }))),
            RpcCommand::SetSteeringMode { mode } => {
                match self.session.set_steering_mode(mode.into()).await {
                    Ok(_) => ok(None),
                    Err(message) => failed(message),
                }
            }
            RpcCommand::SetFollowUpMode { mode } => {
                match self.session.set_follow_up_mode(mode.into()).await {
                    Ok(_) => ok(None),
                    Err(message) => failed(message),
                }
            }
            RpcCommand::Compact {
                custom_instructions,
            } => match self.session.compact(custom_instructions.as_deref()).await {
                Ok(result) => ok(Some(to_value(&result))),
                Err(message) => failed(message),
            },
            RpcCommand::SetAutoCompaction { enabled } => {
                match self.session.set_auto_compaction_enabled(enabled).await {
                    Ok(_) => ok(None),
                    Err(message) => failed(message),
                }
            }
            RpcCommand::SetAutoRetry { .. } => failed(
                "set_auto_retry is not supported in bake-rs: automatic retry follows settings.json's retry.enabled."
                    .to_owned(),
            ),
            RpcCommand::AbortRetry => {
                self.session.abort_retry();
                ok(None)
            }
            RpcCommand::Bash { .. } | RpcCommand::AbortBash => failed(format!(
                "{kind} is not supported in bake-rs: it runs no shell command outside the model's bash tool."
            )),
            RpcCommand::GetSessionStats => ok(Some(stats(&self.session.session_stats()))),
            RpcCommand::ExportHtml { output_path } => match self.export_html(output_path).await {
                Ok(path) => ok(Some(json!({ "path": path }))),
                Err(message) => failed(message),
            },
            RpcCommand::SwitchSession { session_path } => {
                match self.switch_session(&session_path).await {
                    Ok(()) => ok(Some(json!({ "cancelled": false }))),
                    Err(message) => failed(message),
                }
            }
            RpcCommand::Fork { entry_id } => {
                match self.fork(&entry_id, ForkPosition::Before).await {
                    Ok(text) => ok(Some(json!({ "text": text, "cancelled": false }))),
                    Err(message) => failed(message),
                }
            }
            RpcCommand::Clone => {
                let Some(leaf) = self.session.leaf_id() else {
                    return failed("Cannot clone session: no current entry selected".to_owned());
                };
                match self.fork(&leaf, ForkPosition::At).await {
                    Ok(_) => ok(Some(json!({ "cancelled": false }))),
                    Err(message) => failed(message),
                }
            }
            RpcCommand::GetForkMessages => ok(Some(json!({
                "messages": self
                    .session
                    .user_messages_for_forking()
                    .into_iter()
                    .map(|point| json!({ "entryId": point.entry_id, "text": point.text }))
                    .collect::<Vec<Value>>(),
            }))),
            RpcCommand::GetEntries { since } => {
                let (entries, leaf) = self.session.session_entries();
                let start = match &since {
                    None => 0,
                    Some(since) => {
                        match entries.iter().position(|entry| entry.id() == since) {
                            None => return failed(format!("Entry not found: {since}")),
                            Some(index) => index + 1,
                        }
                    }
                };
                let entries: Vec<Value> = entries[start..]
                    .iter()
                    .map(|entry| Value::Object(entry.as_json().clone()))
                    .collect();
                ok(Some(json!({ "entries": entries, "leafId": leaf })))
            }
            RpcCommand::GetTree => {
                let (tree, leaf) = self.session.session_tree();
                // The nested record is written without building a nested
                // value, which a long branch would make too deep to
                // serialize or drop safely.
                self.send_line(&tree_response(id.as_deref(), &tree, leaf.as_deref()));
                None
            }
            RpcCommand::GetLastAssistantText => {
                ok(Some(json!({ "text": self.last_assistant_text() })))
            }
            RpcCommand::SetSessionName { name } => {
                if crate::cli::normalize_session_name(&name).is_none() {
                    return failed("Session name cannot be empty".to_owned());
                }
                match self.session.set_session_name(&name).await {
                    Ok(_) => ok(None),
                    Err(message) => failed(message),
                }
            }
            RpcCommand::GetMessages => ok(Some(json!({
                "messages": to_value(&self.session.messages()),
            }))),
            RpcCommand::GetCommands => ok(Some(json!({ "commands": self.commands() }))),
        }
    }

    /// Pi's `prompt` case: the run starts on its own task, and the
    /// response reports whether it started, as Pi's `preflightResult`
    /// does. A refusal before the run starts is the command's failure; a
    /// failure after it started is in the event stream, as in Pi.
    async fn start_prompt(
        &mut self,
        id: Option<String>,
        message: String,
        images: Vec<bake_ai::ImageContent>,
    ) {
        // One prompt runs at a time: the session refuses a second one,
        // whose task ends at once. Finished tasks are reaped here.
        self.reap_prompts().await;
        let (sender, started) = oneshot::channel();
        *self
            .run_started
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(sender);
        let session = Arc::clone(&self.session);
        let out = Arc::clone(&self.out);
        let slot = Arc::clone(&self.run_started);
        self.prompts.push(tokio::spawn(async move {
            let run = session.prompt(&message, images);
            let outcome = tokio::select! {
                // The run's first event wins a tie: the prompt did start.
                biased;
                _ = started => None,
                result = run => result.err(),
            };
            slot.lock().unwrap_or_else(PoisonError::into_inner).take();
            let response = match outcome {
                None => success_response(id, "prompt", Some(json!({ "disposition": "started" }))),
                Some(message) => error_response(id, "prompt", &message),
            };
            write_out(&out, &serialize_json_line(&response));
        }));
    }

    /// Awaits the prompt tasks that have ended, keeping the rest.
    async fn reap_prompts(&mut self) {
        for task in std::mem::take(&mut self.prompts) {
            if task.is_finished() {
                let _ = task.await;
            } else {
                self.prompts.push(task);
            }
        }
    }

    /// Awaits every prompt task, so nothing writes after this returns.
    async fn await_prompts(&mut self) {
        for task in std::mem::take(&mut self.prompts) {
            let _ = task.await;
        }
    }

    /// Pi's `RpcSessionState`.
    fn state(&self) -> Value {
        let mut state = Map::new();
        if let Some(model) = self.session.model() {
            state.insert("model".into(), to_value(&model));
        }
        state.insert(
            "thinkingLevel".into(),
            self.session.thinking_level().as_str().into(),
        );
        state.insert("isStreaming".into(), self.session.is_streaming().into());
        state.insert("isCompacting".into(), self.session.is_compacting().into());
        state.insert(
            "steeringMode".into(),
            to_value(&self.session.steering_mode()),
        );
        state.insert(
            "followUpMode".into(),
            to_value(&self.session.follow_up_mode()),
        );
        if let Some(file) = self.session.session_file() {
            state.insert("sessionFile".into(), path_json(&file));
        }
        state.insert("sessionId".into(), self.session.session_id().into());
        if let Some(name) = self.session.session_name() {
            state.insert("sessionName".into(), name.into());
        }
        state.insert(
            "autoCompactionEnabled".into(),
            self.session.auto_compaction_enabled().into(),
        );
        state.insert("messageCount".into(), self.session.messages().len().into());
        let queued = self.session.queued_messages();
        state.insert(
            "pendingMessageCount".into(),
            (queued.steering.len() + queued.follow_up.len()).into(),
        );
        Value::Object(state)
    }

    /// Pi's `_cycleAvailableModel` over the available models: the one
    /// after the selected model, wrapping, or `None` with fewer than two.
    fn next_model(&self) -> Option<Model> {
        let models = self.session.model_runtime().snapshot().available();
        if models.len() < 2 {
            return None;
        }
        let current = self.session.model();
        let index = current
            .as_ref()
            .and_then(|model| {
                models
                    .iter()
                    .position(|entry| entry.provider == model.provider && entry.id == model.id)
            })
            .unwrap_or(0);
        models.get((index + 1) % models.len()).cloned()
    }

    /// Pi's `cycleThinkingLevel`: the next level the selected model
    /// supports, or `None` when it does not reason.
    fn next_thinking_level(&self) -> Option<ModelThinkingLevel> {
        if !self.session.model().is_some_and(|model| model.reasoning) {
            return None;
        }
        let levels = self.session.available_thinking_levels();
        let current = self.session.thinking_level();
        let index = levels
            .iter()
            .position(|level| *level == current)
            .unwrap_or(0);
        levels.get((index + 1) % levels.len().max(1)).copied()
    }

    /// Pi's `getLastAssistantText`: the last assistant message's text,
    /// trimmed, skipping an aborted message with no content.
    fn last_assistant_text(&self) -> Value {
        let text = self
            .session
            .messages()
            .iter()
            .rev()
            .filter_map(|message| message.as_assistant())
            .find(|assistant| {
                !(assistant.stop_reason == bake_ai::StopReason::Aborted
                    && assistant.content.is_empty())
            })
            .map(|assistant| {
                assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        bake_ai::AssistantContentBlock::Text(text) => Some(text.text.as_str()),
                        _ => None,
                    })
                    .collect::<String>()
            })
            .map(|text| crate::tools::js::trim(&text).to_owned())
            .filter(|text| !text.is_empty());
        match text {
            Some(text) => text.into(),
            None => Value::Null,
        }
    }

    /// Pi's `get_commands` without extension commands: the prompt
    /// templates and skills a prompt can invoke.
    fn commands(&self) -> Vec<Value> {
        let mut commands = Vec::new();
        for template in self.session.prompt_templates() {
            commands.push(json!({
                "name": template.name,
                "description": template.description,
                "source": "prompt",
                "sourceInfo": source_info(&template.file_path, &template.source, None),
            }));
        }
        for skill in self.session.skills() {
            commands.push(json!({
                "name": format!("skill:{}", skill.name),
                "description": skill.description,
                "source": "skill",
                "sourceInfo": source_info(&skill.file_path, &skill.source, Some(&skill.base_dir)),
            }));
        }
        commands
    }

    /// Pi's `exportToHtml`: the page, written where the command asks or,
    /// without a path, beside the working directory as
    /// `bake-session-<session file>.html`, as Pi names its default.
    async fn export_html(&self, output_path: Option<String>) -> Result<String, String> {
        let session = Arc::clone(&self.session);
        let cwd = self.session.cwd();
        let path = match output_path {
            Some(path) => crate::session::paths::normalize_path(&path),
            None => {
                let file = self
                    .session
                    .session_file()
                    .ok_or_else(|| crate::agent_session::share::EXPORT_IN_MEMORY.to_owned())?;
                let stem = file.file_stem().unwrap_or_default().to_string_lossy();
                Path::new(&cwd).join(format!("bake-session-{stem}.html"))
            }
        };
        tokio::task::spawn_blocking(move || {
            let html = session.export_html()?;
            std::fs::write(&path, html)
                .map_err(|error| format!("Could not write {}: {error}", path.display()))?;
            Ok(path.to_string_lossy().into_owned())
        })
        .await
        .map_err(|error| format!("The export failed: {error}"))?
    }

    /// Pi's `switch_session`: the session file at `path`, which must hold
    /// a session started in this working directory.
    async fn switch_session(&mut self, path: &str) -> Result<(), String> {
        let path = crate::session::paths::normalize_path(path);
        let read = path.clone();
        let id = tokio::task::spawn_blocking(move || {
            crate::session::list::build_session_info(&read, None).map(|info| info.id)
        })
        .await
        .map_err(|error| format!("Could not read the session: {error}"))?
        .ok_or_else(|| format!("{} is not a session file", path.display()))?;
        self.replace(SessionTarget::Open { path, id }).await
    }

    /// Pi's runtime `fork`: the new session holds this session's history up
    /// to `entry_id`; `Before` returns the message's text, for an editor.
    async fn fork(
        &mut self,
        entry_id: &str,
        position: ForkPosition,
    ) -> Result<Option<String>, String> {
        let factory = self.factory()?;
        let (replacement, text) = factory.fork(&self.session, entry_id, position).await?;
        self.commit(replacement).await;
        Ok(text)
    }

    /// Replaces the session with the one `target` names.
    async fn replace(&mut self, target: SessionTarget) -> Result<(), String> {
        let factory = self.factory()?;
        let replacement = factory.replace(&self.session, target).await?;
        self.commit(replacement).await;
        Ok(())
    }

    fn factory(&self) -> Result<SessionFactory, String> {
        self.factory
            .clone()
            .ok_or_else(|| "This session cannot be replaced.".to_owned())
    }

    /// Takes `replacement` into use, as the terminal frontend does: the
    /// running session is shut down first, so its MCP servers are gone
    /// before the replacement's start, and the events of the new session
    /// reach the same output. The approval broker stays connected, so the
    /// new session's tools ask the same client.
    async fn commit(&mut self, mut replacement: Replacement) {
        self.session.shutdown().await;
        self.await_prompts().await;
        let started = replacement.start_servers();
        let mut warnings = replacement.warnings;
        if let Err(message) = started {
            warnings.push(message);
        }
        for warning in warnings {
            write_out(&self.stderr, &format!("Warning: {warning}\n"));
        }
        self.session = Arc::new(replacement.session);
        self.subscribe();
    }

    /// Pi's `shutdown`: the approval queue closes first, so a request the
    /// client never answered is cancelled while the session can still
    /// record that decision, and its call is refused before any effect;
    /// then the session's own shutdown stops its MCP servers, background
    /// shells, and plugin helpers and awaits their process trees, the
    /// prompt task is awaited, and the writer is flushed.
    async fn shutdown(mut self) {
        if let Some(broker) = &self.approvals {
            broker.close();
        }
        // The run stops here, before the session tears down, so the
        // refusal of a request nobody answered is written while the
        // session still admits writes ([`crate::permissions::history`]).
        self.session.abort().await;
        self.session.shutdown().await;
        self.await_prompts().await;
        if let Some(subscription) = self.subscription.take() {
            subscription.unsubscribe();
        }
        let mut out = self.out.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = std::io::Write::flush(&mut *out);
    }
}

/// A value as JSON, `null` where it cannot be serialized, as the JSON
/// events do.
fn to_value(value: &impl serde::Serialize) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// Pi's `SessionStats` as its response data.
fn stats(stats: &SessionStats) -> Value {
    let mut out = Map::new();
    if let Some(file) = &stats.session_file {
        out.insert("sessionFile".into(), path_json(file));
    }
    out.insert("sessionId".into(), stats.session_id.clone().into());
    out.insert("userMessages".into(), stats.user_messages.into());
    out.insert("assistantMessages".into(), stats.assistant_messages.into());
    out.insert("toolCalls".into(), stats.tool_calls.into());
    out.insert("toolResults".into(), stats.tool_results.into());
    out.insert("totalMessages".into(), stats.total_messages.into());
    out.insert(
        "tokens".into(),
        json!({
            "input": stats.input,
            "output": stats.output,
            "cacheRead": stats.cache_read,
            "cacheWrite": stats.cache_write,
            "total": stats.total_tokens(),
        }),
    );
    out.insert("cost".into(), stats.cost.into());
    Value::Object(out)
}

/// A path as the protocol carries it.
fn path_json(path: &Path) -> Value {
    path.to_string_lossy().into_owned().into()
}

/// The thinking levels as their wire names.
fn levels(levels: &[ModelThinkingLevel]) -> Vec<Value> {
    levels.iter().map(|level| level.as_str().into()).collect()
}

/// Pi's `SourceInfo` for a resource Bake found itself: `scope` from where
/// it was found, and `origin` always `top-level`, since Bake has no
/// package manager.
fn source_info(path: &str, source: &str, base_dir: Option<&str>) -> Value {
    let mut info = Map::new();
    info.insert("path".into(), path.into());
    info.insert("source".into(), source.into());
    info.insert(
        "scope".into(),
        match source {
            "user" => "user",
            "project" => "project",
            _ => "temporary",
        }
        .into(),
    );
    info.insert("origin".into(), "top-level".into());
    if let Some(base) = base_dir {
        info.insert("baseDir".into(), base.into());
    }
    Value::Object(info)
}

/// Pi's `get_tree` response, written without building a nested value: a
/// branch is as deep as the session is long, and a nested
/// [`Value`] that deep could not be serialized or dropped without
/// overflowing the stack. Each node is Pi's `SessionTreeNode`: its entry,
/// its children oldest first, and its label. A node is written once, so a
/// damaged parent chain cannot repeat it or loop.
fn tree_response(id: Option<&str>, tree: &SessionTree, leaf: Option<&str>) -> String {
    /// Whether to open a node or to close the one above it.
    enum Step {
        Open(usize),
        Close,
    }
    let mut out = String::new();
    if let Some(id) = id {
        out.push_str(&format!("{{\"id\":{},", string(id)));
    } else {
        out.push('{');
    }
    out.push_str(
        "\"type\":\"response\",\"command\":\"get_tree\",\"success\":true,\"data\":{\"tree\":[",
    );
    let mut written = vec![false; tree.nodes.len()];
    let mut steps: Vec<Step> = tree
        .roots
        .iter()
        .rev()
        .map(|root| Step::Open(*root))
        .collect();
    let mut first = vec![true];
    while let Some(step) = steps.pop() {
        match step {
            Step::Close => {
                out.push_str("]}");
                first.pop();
            }
            Step::Open(index) => {
                let Some(node) = tree.node(index) else {
                    continue;
                };
                if std::mem::replace(&mut written[index], true) {
                    continue;
                }
                if let Some(first) = first.last_mut() {
                    if !*first {
                        out.push(',');
                    }
                    *first = false;
                }
                out.push_str("{\"entry\":");
                out.push_str(&Value::Object(node.entry.as_json().clone()).to_string());
                if let Some(label) = &node.label {
                    out.push_str(&format!(",\"label\":{}", string(label)));
                }
                if let Some(timestamp) = &node.label_timestamp {
                    out.push_str(&format!(",\"labelTimestamp\":{}", string(timestamp)));
                }
                out.push_str(",\"children\":[");
                steps.push(Step::Close);
                first.push(true);
                for child in node.children.iter().rev() {
                    steps.push(Step::Open(*child));
                }
            }
        }
    }
    out.push_str("],\"leafId\":");
    out.push_str(&match leaf {
        Some(leaf) => string(leaf),
        None => "null".to_owned(),
    });
    out.push_str("}}");
    out
}

/// `text` as a JSON string.
fn string(text: &str) -> String {
    Value::String(text.to_owned()).to_string()
}

#[cfg(test)]
#[path = "approval_tests.rs"]
mod approval_tests;
#[cfg(test)]
#[path = "tests.rs"]
mod tests;
