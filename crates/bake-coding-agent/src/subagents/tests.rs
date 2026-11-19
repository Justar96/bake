//! Subagents through real sessions over a faux model: one scripted model
//! answers the parent and every child, telling them apart by the request's
//! model and transcript, so each child's route is seen per request. Waits
//! are on session state, never on time.

use std::time::Duration;

use bake_agent::AgentTool;
use bake_ai::providers::faux::{
    FauxProvider, FauxProviderOptions, FauxResponseStep, faux_assistant_blocks,
    faux_assistant_message, faux_tool_call,
};
use bake_ai::{
    ApiRegistry, AssistantMessage, Message, Model, ModelThinkingLevel, StopReason as Stop,
};
use serde_json::json;

use super::*;
use crate::agent_session::AgentSessionOptions;
use crate::auth_storage::AuthStorage;
use crate::model_registry::ModelRegistry;
use crate::model_registry::config::ModelConfig;
use crate::session::NewSessionOptions;
use crate::test_support::TempDir;
use crate::tools::definition::ToolDefinition;

/// What the scripted model answers: the request's model id and its
/// transcript. `None` holds the request until it is cancelled.
type Brain = Arc<dyn Fn(&str, &[Message]) -> Option<AssistantMessage> + Send + Sync>;

struct Harness {
    faux: Arc<FauxProvider>,
    runtime: ModelRuntime,
    apis: Arc<ApiRegistry>,
    dir: TempDir,
    /// The sessions directory: the parent's file and the children's
    /// directory. `dir` unless a restart test names another.
    place: PathBuf,
    /// Each request's model and its last user-side text.
    requests: Arc<Mutex<Vec<(String, String)>>>,
    /// The work gate of each child built, in build order.
    child_gates: Mutex<Vec<WorkGate>>,
    /// The job registry of each child built, in build order.
    child_jobs: Mutex<Vec<JobRegistry>>,
    /// The settings a parent session runs with.
    parent_settings: Mutex<Settings>,
    /// The settings each child is built with; the default unless a test
    /// sets them.
    child_settings: Mutex<Settings>,
}

fn harness(name: &str) -> Arc<Harness> {
    harness_in(name, None)
}

/// A harness whose sessions are in `place`, a directory the caller owns,
/// or in its own temporary directory.
fn harness_in(name: &str, place: Option<PathBuf>) -> Arc<Harness> {
    let faux = Arc::new(FauxProvider::new(FauxProviderOptions::default()));
    let apis = Arc::new(ApiRegistry::new());
    apis.register(faux.clone());
    let mut registry = ModelRegistry::new(ModelConfig::empty(), AuthStorage::empty());
    registry
        .add_provider_config(
            "faux",
            json!({
                "baseUrl": "http://127.0.0.1:9/unused",
                "api": faux.model().api,
                "models": [
                    { "id": "faux-1", "reasoning": true },
                    { "id": "faux-2", "reasoning": true },
                ],
            }),
            Some("test-key".into()),
        )
        .expect("faux provider");
    let dir = TempDir::new(name);
    Arc::new(Harness {
        faux,
        runtime: ModelRuntime::new(registry),
        apis,
        place: place.unwrap_or_else(|| dir.path().to_owned()),
        dir,
        requests: Arc::default(),
        child_gates: Mutex::default(),
        child_jobs: Mutex::default(),
        parent_settings: Mutex::default(),
        child_settings: Mutex::default(),
    })
}

/// The text of a user message or custom message the model was sent.
fn user_text(message: &Message) -> Option<String> {
    match message {
        Message::User(user) => Some(bake_ai::utils::text::user_content_text(&user.content, "\n")),
        _ => None,
    }
}

/// The last user-side text of a transcript.
fn last_user(messages: &[Message]) -> String {
    messages
        .iter()
        .rev()
        .find_map(user_text)
        .unwrap_or_default()
}

/// The id in the transcript's `started subagent {id}` result.
fn started_child(messages: &[Message]) -> Option<String> {
    messages.iter().find_map(|message| match message {
        Message::ToolResult(result) => bake_ai::utils::text::user_content_text(
            &bake_ai::UserContent::Blocks(result.content.clone()),
            "",
        )
        .strip_prefix("started subagent ")
        .map(str::to_owned),
        _ => None,
    })
}

/// Whether the transcript ends with a tool result.
fn after_tool(messages: &[Message]) -> bool {
    matches!(messages.last(), Some(Message::ToolResult(_)))
}

/// Whether the transcript is a child's: its task names its parent.
fn is_child(messages: &[Message]) -> bool {
    messages
        .iter()
        .filter_map(user_text)
        .any(|text| text.contains("Your parent agent id is"))
}

fn call(name: &str, arguments: serde_json::Value) -> AssistantMessage {
    faux_assistant_blocks(vec![faux_tool_call(name, arguments, None)], Stop::ToolUse)
}

impl Harness {
    fn model(&self, id: &str) -> Model {
        self.runtime.snapshot().model("faux", id).expect("model")
    }

    /// Installs `brain` for the next requests.
    fn think(&self, brain: Brain) {
        let steps = (0..64)
            .map(|_| {
                let brain = Arc::clone(&brain);
                let requests = Arc::clone(&self.requests);
                FauxResponseStep::Factory(Arc::new(move |context, options, _state, model| {
                    let messages = context.messages();
                    requests
                        .lock()
                        .expect("requests")
                        .push((model.id.clone(), last_user(messages)));
                    let reply = brain(&model.id, messages);
                    let signal = options.base.signal.clone();
                    Box::pin(async move {
                        match reply {
                            Some(reply) => Ok(reply),
                            None => {
                                if let Some(signal) = signal {
                                    signal.cancelled().await;
                                }
                                Err("request cancelled".into())
                            }
                        }
                    })
                }))
            })
            .collect();
        self.faux.set_responses(steps);
    }

    #[allow(clippy::too_many_arguments)]
    async fn session(
        &self,
        store: SessionManager,
        tools: Vec<ToolDefinition>,
        work: WorkGate,
        jobs: JobRegistry,
        selection: SessionSelection,
        model: Option<Model>,
        thinking_level: Option<ModelThinkingLevel>,
        settings: Settings,
    ) -> Result<AgentSession, String> {
        let tools: Vec<Arc<AgentTool>> = tools
            .into_iter()
            .map(|definition| Arc::new(definition.tool))
            .collect();
        AgentSession::create(AgentSessionOptions {
            session: store,
            registry: self.runtime.clone(),
            apis: Arc::clone(&self.apis),
            settings,
            settings_store: None,
            selection: Some(selection),
            model,
            thinking_level,
            active_tool_names: tools.iter().map(|tool| tool.name.clone()).collect(),
            tools,
            tool_prompts: Vec::new(),
            system_prompt: crate::system_prompt::SystemPromptOptions::new("/tmp"),
            docs_command: "bake docs".into(),
            plugins: crate::plugins::PluginRegistry::default(),
            work: Some(work),
            jobs: Some(jobs),
        })
        .await
    }
}

/// A builder of children in the harness's directory, with only the
/// child's `send_message`.
fn builder(h: &Arc<Harness>, parent_id: String, parent_file: Option<PathBuf>) -> ChildBuilder {
    let h = Arc::clone(h);
    Arc::new(move |spec: ChildSpec| {
        let h = Arc::clone(&h);
        let parent_id = parent_id.clone();
        let parent_file = parent_file.clone();
        Box::pin(async move {
            let store = match &spec.source {
                ChildSource::New { id, label } => new_child_store(
                    "/tmp",
                    &h.place,
                    true,
                    &parent_id,
                    parent_file.as_deref(),
                    id,
                    label,
                )?,
                ChildSource::Open { path } => {
                    SessionManager::open(path, Some(&children_dir(&h.place)), None)
                        .map_err(|error| error.to_string())?
                }
            };
            let work = WorkGate::default();
            h.child_gates.lock().expect("gates").push(work.clone());
            let jobs = JobRegistry::new(work.clone(), crate::jobs::JobLimits::default());
            h.child_jobs.lock().expect("jobs").push(jobs.clone());
            let settings = h.child_settings.lock().expect("settings").clone();
            h.session(
                store,
                child_tools(&spec.link),
                work,
                jobs,
                SessionSelection::default(),
                spec.route.as_ref().map(|route| route.model.clone()),
                spec.route.and_then(|route| route.thinking_level),
                settings,
            )
            .await
        })
    })
}

struct Parent {
    session: Arc<AgentSession>,
    host: SubagentHost,
    /// The parent's gate, which owns every resident child.
    work: WorkGate,
}

/// A persisted parent on faux-1 with the parent tools, opened from
/// `store` (a new one when `None`).
async fn parent(h: &Arc<Harness>, store: Option<SessionManager>) -> Parent {
    let store = store.unwrap_or_else(|| {
        SessionManager::create("/tmp", Some(&h.place), NewSessionOptions::default())
            .expect("parent store")
    });
    let work = WorkGate::default();
    let jobs = JobRegistry::new(work.clone(), crate::jobs::JobLimits::default());
    let selection = SessionSelection::default();
    let parent_id = store.session_id().to_owned();
    let parent_file = store.session_file().map(PathBuf::from);
    let host = SubagentHost::new(HostOptions {
        parent_id: parent_id.clone(),
        parent_file: parent_file.clone(),
        jobs: jobs.clone(),
        work: work.clone(),
        selection: selection.clone(),
        registry: h.runtime.clone(),
        settings: Settings::default(),
        builder: builder(h, parent_id, parent_file),
        recovered: recover(&store),
    });
    let settings = h.parent_settings.lock().expect("settings").clone();
    let mut session = h
        .session(
            store,
            parent_tools(&host, true),
            work.clone(),
            jobs,
            selection,
            Some(h.model("faux-1")),
            Some(ModelThinkingLevel::High),
            settings,
        )
        .await
        .expect("parent");
    session.set_subagents(host.clone());
    Parent {
        session: Arc::new(session),
        host,
        work,
    }
}

/// Waits until no child runs and its notices are posted, then delivers
/// what the parent's run did not already take, as an idle owner would.
async fn settled(parent: &Parent) {
    let jobs = parent.session.jobs().clone();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let mut events = jobs.subscribe();
            if parent.host.running() == 0 && jobs.live() == 0 {
                return;
            }
            events.changed().await;
        }
    })
    .await
    .expect("children settled");
    tokio::time::timeout(
        Duration::from_secs(20),
        parent.session.deliver_job_notices(),
    )
    .await
    .expect("the notice turn ended")
    .expect("notice turn");
}

/// The settlements the parent's file records ([`SETTLED_TYPE`]), as their
/// run and stop reason.
fn file_settled(path: &std::path::Path) -> Vec<(u64, String)> {
    std::fs::read_to_string(path)
        .expect("parent file")
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|entry| entry["type"] == "custom" && entry["customType"] == SETTLED_TYPE)
        .map(|entry| {
            (
                entry["data"]["runId"].as_u64().unwrap_or_default(),
                entry["data"]["stopReason"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            )
        })
        .collect()
}

/// Awaits `work`, failing the test if it takes longer than 30 s.
async fn bounded<T>(what: &str, work: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(30), work)
        .await
        .unwrap_or_else(|_| panic!("{what} did not finish"))
}

/// The custom messages of `session` with `custom_type`, as their texts
/// and details.
fn customs(session: &AgentSession, custom_type: &str) -> Vec<(String, serde_json::Value)> {
    session
        .messages()
        .iter()
        .filter_map(|message| {
            let bake_agent::AgentMessage::Custom(custom) = message else {
                return None;
            };
            let custom = custom.as_any().downcast_ref::<CustomMessage>()?;
            (custom.custom_type == custom_type).then(|| {
                (
                    bake_ai::utils::text::user_content_text(&custom.content, "\n"),
                    custom.details.clone().unwrap_or_default(),
                )
            })
        })
        .collect()
}

fn tool_results(session: &AgentSession, name: &str) -> Vec<(String, bool, serde_json::Value)> {
    session
        .messages()
        .iter()
        .filter_map(|message| message.as_tool_result())
        .filter(|result| result.tool_name == name)
        .map(|result| {
            (
                bake_ai::utils::text::user_content_text(
                    &bake_ai::UserContent::Blocks(result.content.clone()),
                    "\n",
                ),
                result.is_error,
                result.details.clone().unwrap_or_default(),
            )
        })
        .collect()
}

// The `continuable` default: a background child returns its id at once,
// runs on the route it was given, and its single settlement notice opens
// the idle parent's next turn with its closing message.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_background_child_settles_once_into_the_parent() {
    let h = harness("subagent-background");
    h.think(Arc::new(|model, messages| {
        if is_child(messages) {
            return Some(faux_assistant_message(&format!("child answer from {model}")));
        }
        let last = last_user(messages);
        if last.starts_with("Background subagent") {
            return Some(faux_assistant_message("parent read the notice"));
        }
        if after_tool(messages) {
            return Some(faux_assistant_message("waiting for the child"));
        }
        Some(call(
            "subagent",
            json!({ "description": "count things", "prompt": "Count.", "provider": "faux", "model": "faux-2" }),
        ))
    }));
    let parent = parent(&h, None).await;
    parent
        .session
        .prompt("delegate", Vec::new())
        .await
        .expect("prompt");
    let started = tool_results(&parent.session, "subagent");
    assert_eq!(started.len(), 1);
    let (text, is_error, details) = &started[0];
    assert!(!is_error, "{text}");
    let id = details["subagent"]["agentId"]
        .as_str()
        .expect("id")
        .to_owned();
    assert_eq!(text, &format!("started subagent {id}"));
    assert_eq!(details["subagent"]["model"], "faux-2");
    settled(&parent).await;
    let notices = customs(&parent.session, messages::NOTICE_TYPE);
    assert_eq!(notices.len(), 1);
    assert_eq!(
        notices[0].0,
        format!(
            "Background subagent {id} finished and will do no further work unless you send it more.\nIts closing message:\nchild answer from faux-2"
        )
    );
    assert_eq!(
        notices[0].1,
        json!({ "agentId": id, "runId": 1, "stopReason": "completed" })
    );
    let requests = h.requests.lock().expect("requests").clone();
    let child: Vec<_> = requests
        .iter()
        .filter(|(_, text)| text.contains("Your parent agent id is"))
        .collect();
    assert_eq!(child.len(), 1);
    assert_eq!(child[0].0, "faux-2", "the child's request used its route");
    assert!(child[0].1.starts_with(&format!(
        "Count.\n\nYour parent agent id is \"{}\".",
        parent.host.parent_id()
    )));
    // The child's file is beside the parent's, out of session discovery.
    let file = PathBuf::from(details["subagent"]["file"].as_str().expect("file"));
    // A frontend lists the run's settlement and the file with the child,
    // through the host its session holds.
    assert_eq!(
        parent.session.subagents().map(SubagentHost::list),
        Some(vec![ChildRow {
            id: id.clone(),
            status: Status::Idle,
            label: "count things".into(),
            route: "faux/faux-2".into(),
            run: 1,
            settled: Some(Settlement {
                run: 1,
                stop: StopReason::Completed,
                closing: Some("child answer from faux-2".into()),
                diagnostic: None,
            }),
            file: Some(file.clone()),
        }])
    );
    // Delivered once: nothing is left to deliver.
    assert!(!parent.session.deliver_job_notices().await.expect("no turn"));
    assert_eq!(file.parent(), Some(children_dir(h.dir.path()).as_path()));
    let header = std::fs::read_to_string(&file).expect("child file");
    assert!(header.contains(&format!(
        "\"parentSession\":{}",
        serde_json::Value::String(
            parent.session.session_file().expect("parent file").to_string_lossy().into_owned()
        )
    )));
    assert_eq!(
        crate::session::file::find_most_recent_session(h.dir.path(), None),
        parent.session.session_file()
    );
    parent.session.shutdown().await;
}

// Intercom: the child's message reaches its parent as an attributed
// custom message; the parent's message to the idle child starts a second
// run, recorded with its run id, which settles once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn messages_flow_both_ways_and_an_idle_child_runs_again() {
    let h = harness("subagent-intercom");
    let parent_id = Arc::new(Mutex::new(String::new()));
    {
        let parent_id = Arc::clone(&parent_id);
        h.think(Arc::new(move |_, messages| {
            let last = last_user(messages);
            if is_child(messages) {
                if last.contains("sent a message: again") {
                    return Some(faux_assistant_message("second answer"));
                }
                if after_tool(messages) {
                    return Some(faux_assistant_message("first answer"));
                }
                let parent = parent_id.lock().expect("id").clone();
                return Some(call(
                    "send_message",
                    json!({ "agent_id": parent, "message": "PING\nmore" }),
                ));
            }
            if after_tool(messages) {
                return Some(faux_assistant_message("ok"));
            }
            if last.starts_with("Background subagent") && last.contains("first answer") {
                let child = started_child(messages).expect("child id");
                return Some(call(
                    "send_message",
                    json!({ "agent_id": child, "message": "again" }),
                ));
            }
            if last.starts_with("Background subagent") || last.starts_with("Agent ") {
                return Some(faux_assistant_message("ok"));
            }
            Some(call(
                "subagent",
                json!({ "description": "ping", "prompt": "Ping me." }),
            ))
        }));
    }
    let parent = parent(&h, None).await;
    *parent_id.lock().expect("id") = parent.host.parent_id().to_owned();
    if let Err(error) = parent.session.prompt("start", Vec::new()).await {
        panic!("{error}: {:#?}", h.requests.lock().expect("requests"));
    }
    let id = tool_results(&parent.session, "subagent")[0].2["subagent"]["agentId"]
        .as_str()
        .expect("id")
        .to_owned();
    // The message and the first notice reach the parent in its run or a
    // turn they open; its reply starts the child's second run, whose
    // notice follows.
    for _ in 0..8 {
        settled(&parent).await;
        if customs(&parent.session, messages::NOTICE_TYPE).len() == 2 {
            break;
        }
    }
    let sent = customs(&parent.session, messages::MESSAGE_TYPE);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, format!("Agent {id} sent a message: PING\nmore"));
    assert_eq!(sent[0].1["from"], json!(id));
    assert_eq!(sent[0].1["to"], json!(parent.host.parent_id()));
    let delivered = tool_results(&parent.session, "send_message");
    assert_eq!(
        delivered[0].0,
        format!("message delivered to agent {id}; it starts a new run")
    );
    assert_eq!(delivered[0].2["runId"], 2);
    let notices = customs(&parent.session, messages::NOTICE_TYPE);
    assert_eq!(
        notices.len(),
        2,
        "{notices:#?} {:#?}",
        h.requests.lock().expect("requests")
    );
    assert_eq!(notices[1].1["runId"], 2);
    assert!(
        notices[1]
            .0
            .ends_with("Its closing message:\nsecond answer")
    );
    parent.session.shutdown().await;
}

// A foreground call waits and returns the closing text; its run sends no
// notice. A refused route writes nothing and starts nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreground_returns_the_answer_and_refusals_start_nothing() {
    let h = harness("subagent-foreground");
    h.think(Arc::new(|_, messages| {
        if is_child(messages) {
            return Some(faux_assistant_message("forty-two"));
        }
        if after_tool(messages) {
            return Some(faux_assistant_message("done"));
        }
        let last = last_user(messages);
        if last == "refuse" {
            return Some(call(
                "subagent",
                json!({ "description": "x", "prompt": "y", "provider": "faux", "model": "missing" }),
            ));
        }
        Some(call(
            "subagent",
            json!({ "description": "answer", "prompt": "Answer.", "run_in_background": false, "reasoning_effort": "low" }),
        ))
    }));
    let parent = parent(&h, None).await;
    parent
        .session
        .prompt("refuse", Vec::new())
        .await
        .expect("prompt");
    let refused = tool_results(&parent.session, "subagent");
    assert!(refused[0].1);
    assert_eq!(
        refused[0].0,
        "child LLM route \"faux/missing\" is not allowed for this Session; allowed routes: faux/faux-1, faux/faux-2"
    );
    assert!(parent.host.list().is_empty());
    assert!(
        !children_dir(h.dir.path()).exists(),
        "a refused start wrote no child file"
    );
    parent
        .session
        .prompt("ask", Vec::new())
        .await
        .expect("prompt");
    let answered = tool_results(&parent.session, "subagent");
    assert_eq!(answered[1].0, "forty-two");
    assert!(!answered[1].1);
    assert_eq!(answered[1].2["subagent"]["thinkingLevel"], "low");
    assert_eq!(answered[1].2["subagent"]["model"], "faux-1");
    assert!(
        !parent.session.jobs().has_undelivered(),
        "a foreground run sends no notice"
    );
    parent.session.shutdown().await;
}

// Children inherit their parent's settings, model profiles among them, and
// a child on another model gets that model's profile: its `model_notes`,
// not the parent's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_child_on_another_model_gets_that_models_profile() {
    let h = harness("subagent-profile");
    let settings = Settings::from_object(
        json!({
            "modelProfiles": {
                "faux-1": { "prompt": "Parent notes." },
                "faux-2": { "prompt": "Child notes." }
            }
        })
        .as_object()
        .expect("object"),
    );
    *h.parent_settings.lock().expect("settings") = settings.clone();
    *h.child_settings.lock().expect("settings") = settings;
    // Each request's model and its `model_notes` section.
    type Notes = Vec<(String, Option<String>)>;
    let notes: Arc<Mutex<Notes>> = Arc::default();
    let seen = Arc::clone(&notes);
    h.think(Arc::new(move |model, messages| {
        let section = bake_ai::transcript::get_current_system_message(messages)
            .and_then(|system| system.sections)
            .and_then(|sections| {
                sections
                    .0
                    .into_iter()
                    .find(|(name, _)| name == "model_notes")
                    .and_then(|(_, value)| value)
            });
        seen.lock().expect("notes").push((model.to_owned(), section));
        if is_child(messages) {
            return Some(faux_assistant_message("child done"));
        }
        if after_tool(messages) {
            return Some(faux_assistant_message("done"));
        }
        Some(call(
            "subagent",
            json!({ "description": "x", "prompt": "Do.", "provider": "faux", "model": "faux-2", "run_in_background": false }),
        ))
    }));
    let parent = parent(&h, None).await;
    parent
        .session
        .prompt("delegate", Vec::new())
        .await
        .expect("prompt");
    let answered = tool_results(&parent.session, "subagent");
    assert_eq!(answered[0].0, "child done");
    let notes = notes.lock().expect("notes").clone();
    let parent_notes = Some("<model_notes>\nParent notes.\n</model_notes>".to_owned());
    let child_notes = Some("<model_notes>\nChild notes.\n</model_notes>".to_owned());
    assert_eq!(
        notes,
        [
            ("faux-1".to_owned(), parent_notes.clone()),
            ("faux-2".to_owned(), child_notes),
            ("faux-1".to_owned(), parent_notes),
        ]
    );
    parent.session.shutdown().await;
}

// Adjacency and limits: a parent messages only its children, a child only
// its parent, and an empty or oversized message is refused before
// anything is posted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_adjacent_agents_exchange_bounded_messages() {
    let h = harness("subagent-adjacency");
    h.think(Arc::new(|_, _| Some(faux_assistant_message("idle"))));
    let parent = parent(&h, None).await;
    assert_eq!(
        parent
            .host
            .message_child("someone", "hi")
            .await
            .unwrap_err(),
        "unknown agent someone; use list_agents to find a subagent id."
    );
    assert_eq!(
        parent
            .host
            .message_child(parent.host.parent_id(), "hi")
            .await
            .unwrap_err(),
        format!(
            "unknown agent {}; use list_agents to find a subagent id.",
            parent.host.parent_id()
        )
    );
    let link = parent.host.link("child");
    assert!(
        link.send("other", "hi")
            .unwrap_err()
            .starts_with("agent \"other\" is not your parent")
    );
    assert_eq!(
        link.send(parent.host.parent_id(), "").unwrap_err(),
        "invalid message: expected a non-empty string"
    );
    let long = "x".repeat(MAX_MESSAGE_BYTES + 1);
    assert_eq!(
        link.send(parent.host.parent_id(), &long).unwrap_err(),
        format!(
            "message is too long: {} bytes, the most is {MAX_MESSAGE_BYTES}",
            long.len()
        )
    );
    assert!(!parent.session.jobs().has_undelivered());
    assert_eq!(
        CHILD_TOOL_NAMES,
        ["send_message"],
        "a child cannot delegate"
    );
    parent.session.shutdown().await;
    assert_eq!(
        link.send(parent.host.parent_id(), "late").unwrap_err(),
        "direct parent is not live; the message was not delivered"
    );
}

// Interrupting a running child settles its run as stopped, and the
// parent's shutdown stops a running child and awaits it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_and_shutdown_stop_a_running_child() {
    let h = harness("subagent-interrupt");
    let entered = Arc::new(tokio::sync::Notify::new());
    let steps: Vec<FauxResponseStep> = (0..8)
        .map(|_| {
            let entered = Arc::clone(&entered);
            FauxResponseStep::Factory(Arc::new(move |context, options, _, _| {
                let messages = context.messages().to_vec();
                let signal = options.base.signal.clone();
                let entered = Arc::clone(&entered);
                Box::pin(async move {
                    if is_child(&messages) {
                        entered.notify_one();
                        if let Some(signal) = signal {
                            signal.cancelled().await;
                        }
                        return Err("request cancelled".into());
                    }
                    if after_tool(&messages) || last_user(&messages).starts_with("Background") {
                        return Ok(faux_assistant_message("ok"));
                    }
                    Ok(call(
                        "subagent",
                        json!({ "description": "hang", "prompt": "Hang." }),
                    ))
                })
            }))
        })
        .collect();
    h.faux.set_responses(steps);
    let parent = parent(&h, None).await;
    parent
        .session
        .prompt("start", Vec::new())
        .await
        .expect("prompt");
    let id = parent.host.list()[0].id.clone();
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .expect("child request started");
    assert_eq!(parent.host.list()[0].status, Status::Running);
    parent.host.interrupt(&id).expect("interrupt");
    settled(&parent).await;
    let notices = customs(&parent.session, messages::NOTICE_TYPE);
    assert_eq!(notices.len(), 1);
    assert!(notices[0].0.starts_with(&format!(
        "Background subagent {id} was stopped before it finished."
    )));
    assert_eq!(notices[0].1["stopReason"], "aborted");
    // A second run that hangs is stopped by the parent's shutdown, which
    // returns only once the child is shut down.
    h.faux.set_responses(
        (0..4)
            .map(|_| {
                let entered = Arc::clone(&entered);
                FauxResponseStep::Factory(Arc::new(move |_, options, _, _| {
                    let signal = options.base.signal.clone();
                    let entered = Arc::clone(&entered);
                    Box::pin(async move {
                        entered.notify_one();
                        if let Some(signal) = signal {
                            signal.cancelled().await;
                        }
                        Err("request cancelled".into())
                    })
                }))
            })
            .collect(),
    );
    parent
        .host
        .message_child(&id, "go on")
        .await
        .expect("message");
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .expect("second run started");
    tokio::time::timeout(Duration::from_secs(10), parent.session.shutdown())
        .await
        .expect("shutdown awaited the child");
    assert_eq!(parent.session.jobs().live(), 0);
}

// Restart: a run the parent's file records as started with no settlement
// gets one lost-run notice with the next prompt and is not re-run; the
// child is `ready`, and a message resumes it from its file.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_resumed_parent_reports_lost_runs_and_resumes_children() {
    let h = harness("subagent-restart");
    h.think(Arc::new(|_, messages| {
        if is_child(messages) {
            if last_user(messages).contains("sent a message: resume") {
                return Some(faux_assistant_message("resumed answer"));
            }
            // The first run is in flight when the process ends.
            return None;
        }
        let last = last_user(messages);
        if after_tool(messages) || last.starts_with("Background") || last == "next" {
            return Some(faux_assistant_message("ok"));
        }
        Some(call(
            "subagent",
            json!({ "description": "work", "prompt": "Work." }),
        ))
    }));
    let first = parent(&h, None).await;
    first
        .session
        .prompt("start", Vec::new())
        .await
        .expect("prompt");
    let id = first.host.list()[0].id.clone();
    let file = first.session.session_file().expect("parent file");
    // The process ends while the child runs: its run has no settlement in
    // the file.
    while h
        .requests
        .lock()
        .expect("requests")
        .iter()
        .all(|(_, text)| !text.contains("Your parent agent id is"))
    {
        tokio::task::yield_now().await;
    }
    first.session.shutdown().await;
    assert!(customs(&first.session, messages::NOTICE_TYPE).is_empty());
    let store = SessionManager::open(&file, Some(h.dir.path()), None).expect("reopen");
    let resumed = parent(&h, Some(store)).await;
    assert_eq!(resumed.host.list()[0].status, Status::Ready);
    assert_eq!(resumed.host.list()[0].id, id);
    let child_requests = |h: &Harness| {
        h.requests
            .lock()
            .expect("requests")
            .iter()
            .filter(|(_, text)| text.contains("Your parent agent id is"))
            .count()
    };
    let before = child_requests(&h);
    resumed
        .session
        .prompt("next", Vec::new())
        .await
        .expect("prompt");
    let lost = customs(&resumed.session, messages::NOTICE_TYPE);
    assert_eq!(lost.len(), 1);
    assert_eq!(
        lost[0].0,
        format!(
            "Background subagent {id} was interrupted when Bake stopped and did not finish. Send it a message to continue it."
        )
    );
    assert_eq!(lost[0].1["stopReason"], "interrupted");
    assert_eq!(
        child_requests(&h),
        before,
        "nothing was re-run: {:#?}",
        h.requests.lock().expect("requests")
    );
    let run = resumed
        .host
        .message_child(&id, "resume")
        .await
        .expect("resume")
        .run();
    assert_eq!(run, Some(2), "run ids continue past the recorded ones");
    settled(&resumed).await;
    let notices = customs(&resumed.session, messages::NOTICE_TYPE);
    assert!(
        notices[1]
            .0
            .ends_with("Its closing message:\nresumed answer"),
        "{notices:#?} {:#?}",
        h.requests.lock().expect("requests")
    );
    resumed.session.shutdown().await;
    // Reported once: a second resume finds nothing lost.
    let store = SessionManager::open(&file, Some(h.dir.path()), None).expect("reopen");
    assert!(lost_runs(&store).is_empty());
}

// At the limit the least recently used idle child is evicted to `ready`
// and stays continuable from its file. With every child running, a
// background start and a message to a `ready` child wait in the queue and
// run, in order, once slots free; a foreground start waits for a slot
// until its call is dropped; and a full queue refuses with what to do.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_limit_evicts_idle_children_and_queues_when_all_run() {
    let h = harness("subagent-limit");
    h.think(Arc::new(|_, messages| {
        if last_user(messages).contains("sent a message: answer") {
            return Some(faux_assistant_message("answered"));
        }
        if last_user(messages).starts_with("Hang.") {
            return None;
        }
        Some(faux_assistant_message("done"))
    }));
    let parent = parent(&h, None).await;
    let route = parent
        .host
        .resolve(&models::RouteRequest::default())
        .expect("route");
    let mut ids = Vec::new();
    for index in 0..MAX_ACTIVE {
        let started = parent
            .host
            .start(&format!("child {index}"), "Do.", route.clone(), false)
            .await
            .expect("start");
        ids.push(started.id);
    }
    settled(&parent).await;
    let extra = parent
        .host
        .start("one more", "Do.", route.clone(), false)
        .await
        .expect("an idle child makes room");
    let rows = parent.host.list();
    assert_eq!(
        rows[0].status,
        Status::Ready,
        "the oldest idle child was evicted"
    );
    settled(&parent).await;
    // The evicted child resumes from its file.
    parent
        .host
        .message_child(&ids[0], "answer")
        .await
        .expect("resume");
    settled(&parent).await;
    let notices = customs(&parent.session, messages::NOTICE_TYPE);
    assert!(
        notices.iter().any(
            |(text, details)| details["agentId"] == json!(ids[0]) && text.ends_with("answered")
        )
    );
    let _ = extra;
    // Every resident child running.
    let mut hanging = Vec::new();
    for index in 0..MAX_ACTIVE {
        match parent
            .host
            .start(&format!("hang {index}"), "Hang.", route.clone(), false)
            .await
        {
            Ok(started) => hanging.push(started.id),
            Err(error) => panic!("start {index}: {error}"),
        }
        if parent.host.running() == MAX_ACTIVE {
            break;
        }
    }
    let waits = parent
        .host
        .start("waits", "Do.", route.clone(), false)
        .await
        .expect("a background start queues");
    assert!(waits.queued);
    assert!(waits.settled.is_none());
    assert!(
        parent
            .host
            .listing()
            .iter()
            .any(|(row, queued)| row.id == waits.id && *queued && row.status == Status::Starting)
    );
    let ready = parent
        .host
        .list()
        .into_iter()
        .find(|row| row.status == Status::Ready)
        .expect("an evicted child")
        .id;
    let sent = parent
        .host
        .message_child(&ready, "answer")
        .await
        .expect("a message to a ready child queues");
    assert!(
        matches!(sent.delivery, Delivery::Queued(Some(_))),
        "{sent:?}"
    );
    {
        let foreground = parent.host.start("foreground", "Do.", route.clone(), true);
        tokio::pin!(foreground);
        tokio::select! {
            biased;
            _ = &mut foreground => panic!("no slot is free"),
            () = std::future::ready(()) => {}
        }
    }
    assert!(
        parent
            .host
            .list()
            .iter()
            .all(|row| row.label != "foreground"),
        "a dropped foreground wait leaves nothing"
    );
    for index in 2..MAX_QUEUED {
        let started = parent
            .host
            .start(&format!("queued {index}"), "Hang.", route.clone(), false)
            .await
            .expect("queued");
        assert!(started.queued);
    }
    assert_eq!(
        parent.host.start("too many", "Hang.", route, false).await.err(),
        Some(
            "subagent queue is full: 8 subagents are busy and 32 more wait. Wait for a notice that one finished, or do this work yourself."
                .to_owned()
        )
    );
    // A freed slot starts the queue's head; as it settles, the next.
    assert_eq!(parent.host.interrupt(&hanging[0]), Ok(Interrupt::Requested));
    let jobs = parent.session.jobs().clone();
    let mut posted = Vec::new();
    bounded("the queued work ran", async {
        loop {
            let mut events = jobs.subscribe();
            posted.extend(jobs.claim_mail().iter().map(|notice| {
                (
                    notice.details.clone().unwrap_or_default(),
                    bake_ai::utils::text::user_content_text(&notice.content, "\n"),
                )
            }));
            let done = |id: &str, closing: &str| {
                posted
                    .iter()
                    .any(|(details, text): &(serde_json::Value, String)| {
                        details["agentId"] == json!(id) && text.ends_with(closing)
                    })
            };
            if done(&waits.id, "done") && done(&ready, "answered") {
                return;
            }
            events.changed().await;
        }
    })
    .await;
    tokio::time::timeout(Duration::from_secs(10), parent.session.shutdown())
        .await
        .expect("shutdown awaited every child");
}

// A start dropped while its child is built, as an aborted parent turn
// drops its tool call, leaves no record behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_start_leaves_no_child() {
    let h = harness("subagent-dropped");
    let work = WorkGate::default();
    let host = SubagentHost::new(HostOptions {
        parent_id: "parent".into(),
        parent_file: None,
        jobs: JobRegistry::new(work.clone(), crate::jobs::JobLimits::default()),
        work: work.clone(),
        selection: SessionSelection::default(),
        registry: h.runtime.clone(),
        settings: Settings::default(),
        builder: Arc::new(|_| Box::pin(std::future::pending())),
        recovered: Recovered::default(),
    });
    let route = ChildRoute {
        model: h.model("faux-1"),
        thinking_level: None,
    };
    {
        let start = host.start("never", "Never.", route, false);
        tokio::pin!(start);
        // One poll reaches the builder; then the call is dropped.
        tokio::select! {
            biased;
            _ = &mut start => panic!("the builder never finishes"),
            () = std::future::ready(()) => {}
        }
        assert_eq!(host.list()[0].status, Status::Starting);
    }
    assert!(host.list().is_empty());
    work.close().await;
}

// A subagent's tools carry the replay class recovery reads,
// the child's `send_message` included, which a parent never composes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_subagent_tool_has_a_replay_class() {
    use bake_agent::ToolReplay;
    let h = harness("subagent-replay");
    let work = WorkGate::default();
    let host = SubagentHost::new(HostOptions {
        parent_id: "parent".into(),
        parent_file: None,
        jobs: JobRegistry::new(work.clone(), crate::jobs::JobLimits::default()),
        work: work.clone(),
        selection: SessionSelection::default(),
        registry: h.runtime.clone(),
        settings: Settings::default(),
        builder: Arc::new(|_| Box::pin(std::future::pending())),
        recovered: Recovered::default(),
    });
    let classes = |tools: Vec<ToolDefinition>| -> Vec<(String, Option<ToolReplay>)> {
        tools
            .into_iter()
            .map(|definition| (definition.tool.name, definition.tool.replay))
            .collect()
    };
    let class = |name: &str, replay| (name.to_owned(), Some(replay));
    assert_eq!(
        classes(parent_tools(&host, true)),
        [
            class("subagent", ToolReplay::Never),
            class("list_subagent_models", ToolReplay::Safe),
            class("send_message", ToolReplay::Never),
            class("interrupt_agent", ToolReplay::Never),
            class("list_agents", ToolReplay::Safe),
        ]
    );
    assert_eq!(
        classes(child_tools(&host.link("child"))),
        [class("send_message", ToolReplay::Never)]
    );
    work.close().await;
}

// A foreground call honours its abort: Esc on the parent ends the call at
// once with a result naming the child, whose run is stopped and settles
// with no notice to the parent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn aborting_the_parent_ends_a_foreground_call_promptly() {
    let h = harness("subagent-foreground-abort");
    let entered = Arc::new(tokio::sync::Notify::new());
    {
        let entered = Arc::clone(&entered);
        h.think(Arc::new(move |_, messages| {
            if is_child(messages) {
                entered.notify_one();
                return None;
            }
            if after_tool(messages) {
                return Some(faux_assistant_message("done"));
            }
            Some(call(
                "subagent",
                json!({ "description": "hold", "prompt": "Hold.", "run_in_background": false }),
            ))
        }));
    }
    let parent = parent(&h, None).await;
    let running = Arc::clone(&parent.session);
    let prompt = tokio::spawn(async move { running.prompt("delegate", Vec::new()).await });
    bounded("the child's request", entered.notified()).await;
    bounded("the abort", parent.session.abort()).await;
    let _ = bounded("the parent's turn", prompt).await.expect("joined");
    let results = tool_results(&parent.session, "subagent");
    assert_eq!(results.len(), 1, "{results:#?}");
    let (text, is_error, details) = &results[0];
    let id = details["subagent"]["agentId"].as_str().expect("id");
    assert!(is_error);
    assert_eq!(
        text,
        &format!(
            "subagent run was cancelled; agent {id} was stopped and keeps its context. send_message continues it."
        )
    );
    let jobs = parent.session.jobs().clone();
    bounded("the child's run settled", async {
        loop {
            let mut events = jobs.subscribe();
            if parent.host.running() == 0 && jobs.live() == 0 {
                return;
            }
            events.changed().await;
        }
    })
    .await;
    let row = &parent.host.list()[0];
    assert_eq!(row.status, Status::Idle);
    assert_eq!(
        row.settled.as_ref().map(|settled| settled.stop),
        Some(StopReason::Aborted)
    );
    assert!(
        !jobs.has_undelivered(),
        "the call reported the run; no notice follows"
    );
    parent.session.shutdown().await;
}

/// Scripts the model for children that each wait for a permit of `gate`
/// and then answer; the parent acknowledges everything.
fn gated_children(h: &Harness, gate: &Arc<tokio::sync::Semaphore>) {
    let steps = (0..64)
        .map(|_| {
            let gate = Arc::clone(gate);
            FauxResponseStep::Factory(Arc::new(move |context, _, _, _| {
                let messages = context.messages().to_vec();
                let gate = Arc::clone(&gate);
                Box::pin(async move {
                    if is_child(&messages) {
                        gate.acquire().await.expect("gate").forget();
                        return Ok(faux_assistant_message("answer"));
                    }
                    Ok(faux_assistant_message("ok"))
                })
            }))
        })
        .collect();
    h.faux.set_responses(steps);
}

// Children started side by side in one turn each wake the idle parent when
// they finish, one after another, however many wake turns came before:
// their notices carry the depth of the turn that started them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_fourth_child_to_finish_still_wakes_an_idle_parent() {
    let h = harness("subagent-wakes");
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    gated_children(&h, &gate);
    let parent = parent(&h, None).await;
    let route = parent
        .host
        .resolve(&models::RouteRequest::default())
        .expect("route");
    for index in 0..4 {
        parent
            .host
            .start(&format!("child {index}"), "Work.", route.clone(), false)
            .await
            .expect("start");
    }
    let jobs = parent.session.jobs().clone();
    for index in 0..4 {
        gate.add_permits(1);
        bounded("a notice", async {
            loop {
                let mut events = jobs.subscribe();
                if jobs.has_undelivered() {
                    return;
                }
                events.changed().await;
            }
        })
        .await;
        assert!(jobs.notice_would_wake(), "notice {index} wakes the parent");
        assert!(
            bounded("the notice turn", parent.session.deliver_job_notices())
                .await
                .expect("turn"),
            "notice {index} opened a turn"
        );
    }
    assert_eq!(customs(&parent.session, messages::NOTICE_TYPE).len(), 4);
    parent.session.shutdown().await;
}

// A message the parent sent that the child's run ended without reading is
// kept, said in the notice, kept through an eviction, and read first by
// the child's next run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_message_unread_at_settlement_survives_eviction() {
    let h = harness("subagent-unread");
    let entered = Arc::new(tokio::sync::Notify::new());
    {
        let entered = Arc::clone(&entered);
        h.think(Arc::new(move |_, messages| {
            let texts: Vec<String> = messages.iter().filter_map(user_text).collect();
            let sent = |text: &str| texts.iter().any(|line| line.ends_with(text));
            if sent("sent a message: go") {
                return Some(faux_assistant_message(if sent("sent a message: first") {
                    "read first"
                } else {
                    "missed first"
                }));
            }
            if last_user(messages).starts_with("Hold.") {
                entered.notify_one();
                return None;
            }
            Some(faux_assistant_message("done"))
        }));
    }
    let parent = parent(&h, None).await;
    let route = parent
        .host
        .resolve(&models::RouteRequest::default())
        .expect("route");
    let id = parent
        .host
        .start("holder", "Hold.", route.clone(), false)
        .await
        .expect("start")
        .id;
    bounded("the child's request", entered.notified()).await;
    let sent = parent
        .host
        .message_child(&id, "first")
        .await
        .expect("message");
    assert_eq!(sent.delivery, Delivery::Running);
    assert_eq!(parent.host.interrupt(&id), Ok(Interrupt::Requested));
    settled(&parent).await;
    let notices = customs(&parent.session, messages::NOTICE_TYPE);
    assert!(
        notices[0].0.starts_with(&format!(
            "Background subagent {id} was stopped before it finished.\n1 message you sent it is unread; its next run reads it first."
        )),
        "{notices:#?}"
    );
    // Enough children that finish at once evict the holder to `ready`.
    for index in 0..MAX_ACTIVE {
        parent
            .host
            .start(&format!("filler {index}"), "Do.", route.clone(), false)
            .await
            .expect("start");
    }
    settled(&parent).await;
    let row = |id: &str| {
        parent
            .host
            .list()
            .into_iter()
            .find(|row| row.id == id)
            .expect("row")
    };
    assert_eq!(row(&id).status, Status::Ready);
    let sent = parent.host.message_child(&id, "go").await.expect("resume");
    assert!(matches!(sent.delivery, Delivery::Started(_)));
    settled(&parent).await;
    assert_eq!(
        row(&id).settled.and_then(|settled| settled.closing),
        Some("read first".to_owned())
    );
    parent.session.shutdown().await;
}

/// A host over a standalone registry whose children are built by the
/// harness once `release` is notified; `entered` is notified as each
/// build begins. With `job`, each built child starts a background job that
/// runs until `job` is notified or the child stops.
fn gated_host(
    h: &Arc<Harness>,
    entered: &Arc<tokio::sync::Notify>,
    release: Option<Arc<tokio::sync::Notify>>,
    job: Option<Arc<tokio::sync::Notify>>,
) -> (SubagentHost, JobRegistry, WorkGate) {
    let work = WorkGate::default();
    let jobs = JobRegistry::new(work.clone(), crate::jobs::JobLimits::default());
    let inner = builder(h, "parent".into(), None);
    let entered = Arc::clone(entered);
    let gated: ChildBuilder = Arc::new(move |spec| {
        let (inner, entered, release, job) = (
            Arc::clone(&inner),
            Arc::clone(&entered),
            release.clone(),
            job.clone(),
        );
        Box::pin(async move {
            entered.notify_one();
            if let Some(release) = release {
                release.notified().await;
            }
            let session = inner(spec).await?;
            if let Some(finish) = job {
                session
                    .jobs()
                    .start(
                        crate::jobs::JobSpec {
                            kind: "bash",
                            label: "serve".into(),
                            output: crate::jobs::OutputMode::Final,
                        },
                        move |context| async move {
                            tokio::select! {
                                () = finish.notified() => crate::jobs::JobOutcome::completed("served"),
                                () = context.signal().cancelled() => crate::jobs::JobOutcome::killed("stopped"),
                            }
                        },
                    )
                    .map_err(|error| error.to_string())?;
            }
            Ok(session)
        })
    });
    let host = SubagentHost::new(HostOptions {
        parent_id: "parent".into(),
        parent_file: None,
        jobs: jobs.clone(),
        work: work.clone(),
        selection: SessionSelection::default(),
        registry: h.runtime.clone(),
        settings: Settings::default(),
        builder: gated,
        recovered: Recovered::default(),
    });
    (host, jobs, work)
}

/// Waits for the next notice posted to `jobs` and takes it.
async fn next_notice(jobs: &JobRegistry) -> CustomMessage {
    loop {
        let mut events = jobs.subscribe();
        if let Some(notice) = jobs.claim_mail().into_iter().next() {
            return notice;
        }
        events.changed().await;
    }
}

fn notice_text(notice: &CustomMessage) -> String {
    bake_ai::utils::text::user_content_text(&notice.content, "\n")
}

// An interrupt that comes before the child's run has begun still stops
// that run; one for a child with no run says there is nothing to stop.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_interrupt_before_the_run_begins_stops_it() {
    let h = harness("subagent-early-interrupt");
    h.think(Arc::new(|_, _| Some(faux_assistant_message("answered"))));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (host, jobs, work) = gated_host(&h, &entered, Some(Arc::clone(&release)), None);
    let route = ChildRoute {
        model: h.model("faux-1"),
        thinking_level: None,
    };
    let start = {
        let host = host.clone();
        tokio::spawn(async move { host.start("early", "Answer.", route, false).await })
    };
    bounded("the build began", entered.notified()).await;
    let id = host.list()[0].id.clone();
    assert_eq!(host.interrupt(&id), Ok(Interrupt::Requested));
    release.notify_one();
    bounded("the start", start)
        .await
        .expect("joined")
        .expect("started");
    let notice = bounded("the notice", next_notice(&jobs)).await;
    assert!(
        notice_text(&notice).starts_with(&format!(
            "Background subagent {id} was stopped before it finished."
        )),
        "{}",
        notice_text(&notice)
    );
    assert_eq!(
        notice
            .details
            .as_ref()
            .map(|details| details["stopReason"].clone()),
        Some(json!("aborted"))
    );
    assert!(
        h.requests.lock().expect("requests").is_empty(),
        "the stopped run sent no request"
    );
    bounded("the run's end", async {
        loop {
            let mut events = jobs.subscribe();
            if host.running() == 0 && jobs.live() == 0 {
                return;
            }
            events.changed().await;
        }
    })
    .await;
    assert_eq!(host.interrupt(&id), Ok(Interrupt::NotRunning("idle")));
    assert_eq!(
        host.interrupt("nobody"),
        Err("unknown agent nobody; use list_agents to find a subagent id.".to_owned())
    );
    work.close().await;
}

// A child whose turn finished while its own job still runs reports that
// turn after the job wait, naming the job; the job's end then continues
// the child in a run of its own, with its own notice. Virtual time.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_child_with_a_live_job_still_reports_its_turn() {
    let h = harness("subagent-live-job");
    h.think(Arc::new(|_, messages| {
        if last_user(messages).starts_with("background job") {
            return Some(faux_assistant_message("the server finished"));
        }
        Some(faux_assistant_message("started the server"))
    }));
    let entered = Arc::new(tokio::sync::Notify::new());
    let finish = Arc::new(tokio::sync::Notify::new());
    let (host, jobs, work) = gated_host(&h, &entered, None, Some(Arc::clone(&finish)));
    let route = ChildRoute {
        model: h.model("faux-1"),
        thinking_level: None,
    };
    let started = tokio::time::Instant::now();
    let id = host
        .start("server", "Serve.", route, false)
        .await
        .expect("start")
        .id;
    let notice = next_notice(&jobs).await;
    assert!(started.elapsed() >= JOB_WAIT, "the run waited for its job");
    assert_eq!(
        notice_text(&notice),
        format!(
            "Background subagent {id} finished its turn, but its background jobs still run: bash-1. When one finishes it continues, and you are notified again.\nIts closing message:\nstarted the server"
        )
    );
    assert_eq!(host.list()[0].status, Status::Idle);
    assert_eq!(jobs.live(), 0, "a settled run holds nothing live");
    finish.notify_one();
    let notice = next_notice(&jobs).await;
    assert_eq!(
        notice_text(&notice),
        format!(
            "Background subagent {id} finished and will do no further work unless you send it more.\nIts closing message:\nthe server finished"
        )
    );
    assert_eq!(host.list()[0].run, 2);
    work.close().await;
}

// A notice says what its run left: messages it did not read, those lost
// when Bake stopped, and jobs still running. A restart reports the unread
// messages of a run whose notice was never delivered as lost.
#[test]
fn a_notice_names_what_its_run_left_behind() {
    let notice = |stop, leftovers: Leftovers| {
        notice_text(&messages::settlement_notice_with(
            "c", 1, stop, None, &leftovers,
        ))
    };
    assert_eq!(
        notice(
            StopReason::Aborted,
            Leftovers {
                unread: 2,
                ..Leftovers::default()
            }
        ),
        "Background subagent c was stopped before it finished.\n2 messages you sent it are unread; its next run reads them first.\nIt left no closing message."
    );
    assert_eq!(
        notice(
            StopReason::MaxTokens,
            Leftovers {
                jobs: vec!["bash-1".into()],
                ..Leftovers::default()
            }
        ),
        "Background subagent c ran out of room before it finished.\nIts background jobs still run: bash-1. When one finishes it continues, and you are notified again.\nIt left no closing message."
    );
    let mut store =
        SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
            .expect("store");
    store
        .append_message(crate::session::AgentMessage::from_json(
            json!({
                "role": "toolResult",
                "toolCallId": "call-1",
                "toolName": "send_message",
                "content": [{ "type": "text", "text": "message delivered to agent c; it reads it at its next step" }],
                "details": { "agentId": "c", "messageId": "m", "runId": 4 },
                "isError": false,
                "timestamp": 0,
            })
            .as_object()
            .expect("object")
            .clone(),
        ))
        .expect("result");
    store
        .append_custom_entry(
            SETTLED_TYPE,
            Some(json!({ "version": 1, "agentId": "c", "runId": 4, "stopReason": "aborted", "closing": null, "unread": 1 })),
        )
        .expect("fact");
    let lost = lost_runs(&store);
    assert_eq!(lost.len(), 1);
    assert_eq!(
        notice_text(&lost[0]),
        "Background subagent c was stopped before it finished.\n1 message you sent it was unread when Bake stopped and is lost.\nIt left no closing message."
    );
}

#[path = "barrier_tests.rs"]
mod barrier;

#[path = "restart_tests.rs"]
mod restart;
