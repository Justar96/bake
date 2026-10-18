//! `/session`, `/name`, `/new`, and `/resume` against real sessions saved
//! in a session directory this test alone owns, with the faux provider.
//! The prompts each session answers, the files they land in, the model and
//! thinking level each comes back with, and what the next request carries
//! are read from the session files and the requests themselves.
//!
//! The round trip follows Pi v1.2.0
//! `packages/coding-agent/src/core/agent-session-runtime.ts`
//! (`newSession`, `switchSession`) and the TypeScript navigation's
//! `apps/tui/packages/app/src/navigation.ts` (busy refusal, a cancelled
//! choice keeping the session). Cases adapted from Pi's
//! `test/suite/agent-session-runtime.test.ts` name the case they follow:
//! "restores model and thinking state from the destination session" in the
//! round trip. Its "settles the active response before session replacement"
//! is not followed: Pi aborts the running turn, while bake-rs refuses the
//! replacement until the turn ends, as the TypeScript navigation did (see
//! the busy case). Its `session_before_switch` cancellation needs
//! extensions, which bake-rs does not have; a user's cancellation is
//! covered by the cancelled-resume case instead.
//!
//! `/tree` and `/fork` follow Pi v1.2.0 `interactive-mode.ts`
//! (`showTreeSelector`, `showUserMessageSelector`), `navigateTree`, and the
//! runtime's `fork`, with Pi's `test/agent-session-branching.test.ts` cases
//! ("should fork from middle of conversation", in-memory forking) read
//! through the driver: the request after navigation, the files, the draft,
//! and the restored model are checked, and no sheet sends a request.

use std::path::{Path, PathBuf};
use std::sync::Mutex as StdMutex;

use bake_ai::Message;
use bake_coding_agent::cli::{Args, SessionFactory};
use bake_tui_view::dialog::{DialogAnswer, DialogKind, DialogRequest, InfoLine};

use super::*;

/// A root with a Bake home, a project, and a session directory, removed
/// when dropped.
struct World {
    root: PathBuf,
}

impl World {
    /// Allocates a fresh root atomically: a random name, created with
    /// `create_dir`, retried only when that name already exists, so no
    /// other process's directory is ever adopted (or later removed).
    fn new(name: &str) -> Self {
        let root = loop {
            let candidate = std::env::temp_dir().join(format!(
                "bake-tui-sessions-{name}-{}",
                bake_coding_agent::session::id::uuid_v4()
            ));
            match std::fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("a fresh test root: {error}"),
            }
        };
        for dir in ["home", "project", "sessions"] {
            std::fs::create_dir(root.join(dir)).expect("dirs");
        }
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn project(&self) -> String {
        self.root.join("project").to_string_lossy().into_owned()
    }

    fn sessions(&self) -> PathBuf {
        self.root.join("sessions")
    }

    /// The session files, by name.
    fn files(&self) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(self.sessions())
            .expect("sessions")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
            .collect();
        files.sort();
        files
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// What each request carried: its model and its messages as JSON.
type Requests = Arc<StdMutex<Vec<(String, String)>>>;

/// Answers every request with `Echo: <last user text>.`, recording it.
fn echo(faux: &FauxProvider, requests: &Requests) {
    let steps = (0..40)
        .map(|_| {
            let requests = Arc::clone(requests);
            FauxResponseStep::from_fn(move |context, _options, _state, model| {
                let last = context
                    .messages()
                    .iter()
                    .rev()
                    .find_map(|message| match message {
                        Message::User(user) => {
                            Some(serde_json::to_string(&user.content).unwrap_or_default())
                        }
                        _ => None,
                    })
                    .unwrap_or_default();
                let messages = serde_json::to_string(context.messages()).unwrap_or_default();
                requests
                    .lock()
                    .expect("requests")
                    .push((model.id.clone(), messages));
                let text: String = serde_json::from_str::<serde_json::Value>(&last)
                    .ok()
                    .and_then(|value| match value {
                        serde_json::Value::String(text) => Some(text),
                        serde_json::Value::Array(blocks) => blocks
                            .first()
                            .and_then(|block| block["text"].as_str().map(str::to_owned)),
                        _ => None,
                    })
                    .unwrap_or_default();
                Ok(faux_assistant_message(&format!("Echo: {text}.")))
            })
        })
        .collect();
    faux.set_responses(steps);
}

/// A port over `manager`'s session in `world`, with a factory for its
/// replacements, the faux models `faux-1` and `faux-2` (which reasons), and
/// `tools`.
fn session_harness(
    world: &World,
    manager: SessionManager,
    tools: Vec<Arc<AgentTool>>,
    gate: Option<commands::CommitGate>,
) -> (Harness, Requests) {
    let (h, requests, _) = panicking_session_harness(world, manager, tools, gate, None);
    (h, requests)
}

/// [`session_harness`], whose driver panics when it refuses the prompt
/// `panic_on`, as a driver bug would. The `Weak` is gone once nothing
/// holds the port's update callback.
fn panicking_session_harness(
    world: &World,
    manager: SessionManager,
    tools: Vec<Arc<AgentTool>>,
    gate: Option<commands::CommitGate>,
    panic_on: Option<u64>,
) -> (Harness, Requests, std::sync::Weak<()>) {
    settings_harness(world, manager, tools, gate, panic_on, Settings::default())
}

/// [`panicking_session_harness`] with the session's `settings`.
fn settings_harness(
    world: &World,
    manager: SessionManager,
    tools: Vec<Arc<AgentTool>>,
    gate: Option<commands::CommitGate>,
    panic_on: Option<u64>,
    settings: Settings,
) -> (Harness, Requests, std::sync::Weak<()>) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let faux = Arc::new(FauxProvider::new(FauxProviderOptions::default()));
    let apis = Arc::new(ApiRegistry::new());
    apis.register(faux.clone());
    let mut registry = ModelRegistry::new(
        ModelConfig::from_value(json!({}), "models.json"),
        AuthStorage::empty(),
    );
    registry
        .add_provider_config(
            "faux",
            json!({
                "baseUrl": "http://127.0.0.1:9/unused",
                "api": faux.model().api,
                "models": [
                    { "id": "faux-1", "contextWindow": 1000 },
                    { "id": "faux-2", "contextWindow": 1000, "reasoning": true },
                ],
            }),
            Some("test-key".into()),
        )
        .expect("faux provider");
    let resumed = !manager.build_session_context().messages.is_empty();
    let model = (!resumed)
        .then(|| registry.model("faux", "faux-1"))
        .flatten();
    let active: Vec<String> = tools.iter().map(|tool| tool.name.clone()).collect();
    let session = runtime
        .block_on(AgentSession::create(AgentSessionOptions {
            session: manager,
            model,
            registry: registry.into(),
            settings_store: None,
            selection: None,
            apis,
            settings,
            thinking_level: None,
            tools,
            active_tool_names: active,
            tool_prompts: Vec::new(),
            system_prompt: SystemPromptOptions::new(world.project()),
            docs_command: "bake-rs docs".into(),
            plugins: bake_coding_agent::plugins::PluginRegistry::default(),
            work: None,
            jobs: None,
        }))
        .expect("agent session");
    let factory = SessionFactory::new(
        Args {
            no_context_files: true,
            no_skills: true,
            no_prompt_templates: true,
            ..Args::default()
        },
        world.home(),
        "bake-rs docs".into(),
        Arc::new(bake_sandbox::provider::UnavailableProvider::new(None)),
        None,
    );
    let requests: Requests = Arc::default();
    echo(&faux, &requests);
    let live = LiveSession {
        session,
        runtime: runtime.handle().clone(),
        prompts: Vec::new(),
        notes: Vec::new(),
        approvals: None,
        home: None,
        factory: Some(factory),
        commit_gate: gate,
    };
    let held = Arc::new(());
    let callback_alive = Arc::downgrade(&held);
    let (sender, updates) = std_mpsc::channel();
    let mut port = LivePort::start(live, move |update| {
        let _held = &held;
        if let RuntimeUpdate::Refused { id, .. } = &update {
            assert_ne!(Some(*id), panic_on, "a driver bug");
        }
        sender.send(update).is_ok()
    });
    port.begin();
    (
        Harness {
            runtime,
            faux,
            port: Some(port),
            updates,
        },
        requests,
        callback_alive,
    )
}

fn persisted(world: &World) -> SessionManager {
    SessionManager::create(
        &world.project(),
        Some(&world.sessions()),
        NewSessionOptions::default(),
    )
    .expect("session")
}

impl Harness {
    fn next_dialog(&self) -> DialogRequest {
        let seen = self.until(|update| matches!(update, RuntimeUpdate::Dialog(Some(_))));
        match seen.last() {
            Some(RuntimeUpdate::Dialog(Some(request))) => request.clone(),
            _ => unreachable!(),
        }
    }

    fn choose_answer(&self, id: u64, answer: DialogAnswer) {
        self.port().request(Request::Dialog { id, answer });
    }

    fn ended(&self, id: u64) -> Vec<RuntimeUpdate> {
        self.until(|update| {
            matches!(update, RuntimeUpdate::Admitted(admitted) if *admitted == id)
                || matches!(update, RuntimeUpdate::Refused { id: refused, .. } if *refused == id)
        })
    }

    /// Sends `text` as prompt `id` and waits for its turn to end.
    fn turn(&self, id: u64, text: &str) -> Vec<RuntimeUpdate> {
        self.submit_id(id, Submission::FollowUp(text.into()));
        self.until_turn_end()
    }

    /// Runs the command `text` as prompt `id`, and waits for its status.
    fn command(&self, id: u64, text: &str) -> Vec<RuntimeUpdate> {
        self.submit_id(id, Submission::FollowUp(text.into()));
        self.settled_command(id)
    }

    /// Waits for command `id` to end, and for its status when admitted.
    fn settled_command(&self, id: u64) -> Vec<RuntimeUpdate> {
        let mut updates = self.ended(id);
        if matches!(updates.last(), Some(RuntimeUpdate::Admitted(_))) {
            updates.extend(self.until(|u| matches!(u, RuntimeUpdate::Status(_))));
        }
        updates
    }
}

fn refused(updates: &[RuntimeUpdate]) -> Option<String> {
    updates.iter().find_map(|update| match update {
        RuntimeUpdate::Refused { reason, .. } => Some(reason.clone()),
        _ => None,
    })
}

fn replaced(updates: &[RuntimeUpdate]) -> Option<Vec<Row>> {
    updates.iter().find_map(|update| match update {
        RuntimeUpdate::Replaced(rows) => Some(rows.clone()),
        _ => None,
    })
}

fn last_status(updates: &[RuntimeUpdate]) -> Option<SessionStatus> {
    updates.iter().rev().find_map(|update| match update {
        RuntimeUpdate::Status(status) => Some(status.clone()),
        _ => None,
    })
}

/// The saved file a replacement's opening rows name.
fn saved_file(rows: &[Row]) -> PathBuf {
    rows.iter()
        .find_map(|row| match row {
            Row::Info(text) => text.strip_prefix("Session saved to ").map(PathBuf::from),
            _ => None,
        })
        .expect("a saved session")
}

/// The texts of the file's messages with `role`.
fn texts(file: &Path, role: &str) -> Vec<String> {
    std::fs::read_to_string(file)
        .expect("session file")
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|entry| entry["type"] == "message" && entry["message"]["role"] == role)
        .map(|entry| match &entry["message"]["content"] {
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Array(blocks) => blocks
                .iter()
                .filter_map(|block| block["text"].as_str())
                .collect(),
            _ => String::new(),
        })
        .collect()
}

fn entries(file: &Path, kind: &str) -> Vec<serde_json::Value> {
    std::fs::read_to_string(file)
        .expect("session file")
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|entry| entry["type"] == kind)
        .collect()
}

/// The `/resume` list's value for the session whose label is `label`.
fn choose(dialog: &DialogRequest, label: &str) -> DialogAnswer {
    let DialogKind::List(list) = &dialog.kind else {
        panic!("{dialog:?}");
    };
    let item = list
        .items
        .iter()
        .find(|item| item.label == label)
        .unwrap_or_else(|| panic!("{label} in {list:#?}"));
    DialogAnswer::Chose {
        value: item.value.clone(),
        save_default: false,
    }
}

// Pi `AgentSessionRuntime.newSession`/`switchSession` and Pi's "restores
// model and thinking state from the destination session": `/new` starts a
// second file with the live model and level; `/resume` restores the first
// session's transcript, model, and level; each prompt lands in its own
// session's file, and the resumed request carries only its own history.
#[test]
fn new_and_resume_round_trip_without_mixing_transcripts() {
    let world = World::new("round-trip");
    let manager = persisted(&world);
    let first_file = manager.session_file().expect("file").to_path_buf();
    let first_id = manager.session_id().to_owned();
    let (h, requests) = session_harness(&world, manager, Vec::new(), None);
    h.turn(1, "alpha");
    h.command(2, "/model faux/faux-2");
    h.command(3, "/thinking high");
    let updates = h.command(4, "/new");
    let rows = replaced(&updates).expect("replaced");
    assert_eq!(rows.first(), Some(&Row::Info("New session started".into())));
    let second_file = saved_file(&rows);
    assert_ne!(second_file, first_file);
    // /new keeps the live model and level, though neither was saved.
    let status = last_status(&updates).expect("status");
    assert_eq!(status.model.as_deref(), Some("faux/faux-2"));
    assert_eq!(status.thinking.as_deref(), Some("high"));
    h.turn(5, "beta");
    // faux-1 does not reason, so the status shows no level; the resumed
    // session must bring both its model and its level back.
    let status = last_status(&h.command(6, "/model faux/faux-1")).expect("status");
    assert_eq!(status.model.as_deref(), Some("faux/faux-1"));
    assert_eq!(status.thinking, None);
    // /resume lists both, the running one current, and takes the id.
    h.submit_id(7, Submission::FollowUp("/resume".into()));
    let dialog = h.next_dialog();
    let DialogKind::List(list) = &dialog.kind else {
        panic!("{dialog:?}");
    };
    assert_eq!(list.items.len(), 2, "{list:#?}");
    assert_eq!(list.items[list.current.expect("current")].label, "beta");
    assert!(list.items.iter().any(|item| item.value == first_id));
    h.choose_answer(dialog.id, choose(&dialog, "alpha"));
    let mut updates = h.ended(7);
    updates.extend(h.until(|u| matches!(u, RuntimeUpdate::Status(_))));
    let rows = replaced(&updates).expect("replaced");
    assert!(rows.contains(&Row::User("alpha".into())), "{rows:#?}");
    assert!(
        rows.contains(&Row::Answer("Echo: alpha.".into())),
        "{rows:#?}"
    );
    assert!(!rows.contains(&Row::User("beta".into())), "{rows:#?}");
    let status = last_status(&updates).expect("status");
    assert_eq!(status.model.as_deref(), Some("faux/faux-2"));
    assert_eq!(status.thinking.as_deref(), Some("high"));
    let before = requests.lock().expect("requests").len();
    h.turn(8, "gamma");
    let (model, sent) = requests.lock().expect("requests")[before].clone();
    assert_eq!(model, "faux-2");
    assert!(sent.contains("alpha") && sent.contains("gamma"), "{sent}");
    assert!(!sent.contains("beta"), "{sent}");
    h.stop();
    assert_eq!(texts(&first_file, "user"), ["alpha", "gamma"]);
    assert_eq!(texts(&second_file, "user"), ["beta"]);
    // No panel action reached the model: one request per prompt.
    assert_eq!(h_requests(&requests), 3);
    // The writer locks were released on shutdown.
    SessionManager::open(&first_file, None, None).expect("first file free");
    SessionManager::open(&second_file, None, None).expect("second file free");
}

fn h_requests(requests: &Requests) -> usize {
    requests.lock().expect("requests").len()
}

// TS navigation `unavailable`: a running turn refuses `/new`, `/resume`,
// `/tree`, `/fork`, `/clone`, and `/compact` with the draft kept, and the turn goes on in its own session.
#[test]
fn a_running_turn_refuses_a_replacement_and_keeps_the_draft() {
    let world = World::new("busy");
    let manager = persisted(&world);
    let file = manager.session_file().expect("file").to_path_buf();
    let release = Arc::new(Notify::new());
    let held = Arc::clone(&release);
    let wait = Arc::new(AgentTool::new(
        "wait",
        "Wait",
        "Waits",
        json!({"type": "object", "properties": {}}),
        move |_, _, _, _| {
            let held = Arc::clone(&held);
            Box::pin(async move {
                held.notified().await;
                Ok(AgentToolResult::text("waited"))
            })
        },
    ));
    let (h, _requests) = session_harness(&world, manager, vec![wait], None);
    h.faux.set_responses(vec![
        faux_assistant_blocks(
            vec![faux_tool_call("wait", json!({}), Some("call_1"))],
            StopReason::ToolUse,
        )
        .into(),
        faux_assistant_message("Done waiting.").into(),
    ]);
    h.submit_id(1, Submission::FollowUp("hold".into()));
    h.until(|u| matches!(u, RuntimeUpdate::Live(rows) if rows.iter().any(|r| matches!(r, Row::Call { .. }))));
    for (id, text) in [
        (2, "/new"),
        (3, "/resume"),
        (4, "/tree"),
        (5, "/fork"),
        (6, "/compact"),
        (7, "/clone"),
    ] {
        h.submit_id(id, Submission::FollowUp(text.into()));
        let updates = h.ended(id);
        let reason = refused(&updates).expect("refused");
        assert!(reason.contains("turn is running"), "{reason}");
    }
    release.notify_one();
    let updates = h.until_turn_end();
    assert!(matches!(
        updates.last(),
        Some(RuntimeUpdate::TurnEnded(Outcome::Completed))
    ));
    h.stop();
    assert_eq!(texts(&file, "user"), ["hold"]);
    assert_eq!(world.files(), [file]);
}

// TS navigation: a cancelled choice keeps the session; Pi's selector: no
// other saved session means nothing to resume. The draft comes back each
// time, and later prompts land in the same file.
#[test]
fn a_cancelled_or_empty_resume_keeps_the_session_and_draft() {
    let world = World::new("cancel");
    let manager = persisted(&world);
    let file = manager.session_file().expect("file").to_path_buf();
    let (h, requests) = session_harness(&world, manager, Vec::new(), None);
    h.submit_id(1, Submission::FollowUp("/resume".into()));
    let updates = h.ended(1);
    assert!(
        refused(&updates)
            .expect("refused")
            .contains("No other saved session")
    );
    h.turn(2, "one");
    let other = persisted(&world);
    let mut other = other;
    other
        .append_message(bake_coding_agent::session::AgentMessage::from_json(
            json!({"role": "user", "content": "elsewhere", "timestamp": 1})
                .as_object()
                .expect("object")
                .clone(),
        ))
        .expect("append");
    drop(other);
    h.submit_id(3, Submission::FollowUp("/resume".into()));
    let dialog = h.next_dialog();
    h.choose_answer(dialog.id, DialogAnswer::Cancel);
    let updates = h.ended(3);
    assert_eq!(refused(&updates).as_deref(), Some(commands::CANCELLED));
    assert!(replaced(&updates).is_none());
    h.turn(4, "two");
    h.stop();
    assert_eq!(texts(&file, "user"), ["one", "two"]);
    assert_eq!(h_requests(&requests), 2);
}

/// Writes a session file holding one exchange, with `id`, started in
/// `cwd`, at `name`.
fn fixture(world: &World, name: &str, id: &str, cwd: &str, text: &str) -> PathBuf {
    let path = world.sessions().join(name);
    let lines = [
        json!({"type": "session", "version": 3, "id": id, "timestamp": "2026-01-01T00:00:00.000Z", "cwd": cwd}),
        json!({"type": "message", "id": "a1", "parentId": null, "timestamp": "2026-01-01T00:00:01.000Z",
            "message": {"role": "user", "content": text, "timestamp": 1_767_225_601_000_i64}}),
        json!({"type": "message", "id": "a2", "parentId": "a1", "timestamp": "2026-01-01T00:00:02.000Z",
            "message": {"role": "assistant", "content": [{"type": "text", "text": format!("Echo: {text}.")}],
                "api": "faux", "provider": "faux", "model": "faux-1", "stopReason": "stop", "timestamp": 1_767_225_602_000_i64,
                "usage": {"input": 4, "output": 2, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 6,
                    "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}}}}),
    ];
    let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
    std::fs::write(&path, body).expect("fixture");
    path
}

// Bake: a choice is an id, checked again when it opens. A file removed
// after listing, two files holding one id, and a session from another
// directory are refused; the running session and its draft stay.
#[test]
fn a_resume_whose_choice_no_longer_holds_is_refused() {
    let world = World::new("invalid");
    let manager = persisted(&world);
    let file = manager.session_file().expect("file").to_path_buf();
    let (h, _requests) = session_harness(&world, manager, Vec::new(), None);
    h.turn(1, "mine");
    let gone = fixture(
        &world,
        "gone.jsonl",
        "01aaaaaa-gone",
        &world.project(),
        "gone",
    );
    h.submit_id(2, Submission::FollowUp("/resume".into()));
    let dialog = h.next_dialog();
    std::fs::remove_file(&gone).expect("remove");
    h.choose_answer(dialog.id, choose(&dialog, "gone"));
    let reason = refused(&h.ended(2)).expect("refused");
    assert!(reason.contains("no longer listed"), "{reason}");
    assert!(!gone.exists(), "a removed session was recreated");
    // Two copies of one session.
    fixture(
        &world,
        "copy-a.jsonl",
        "01bbbbbb-twin",
        &world.project(),
        "twin",
    );
    fixture(
        &world,
        "copy-b.jsonl",
        "01bbbbbb-twin",
        &world.project(),
        "twin",
    );
    h.submit_id(3, Submission::FollowUp("/resume".into()));
    let dialog = h.next_dialog();
    h.choose_answer(dialog.id, choose(&dialog, "twin"));
    let reason = refused(&h.ended(3)).expect("refused");
    assert!(reason.contains("2 session files hold"), "{reason}");
    // A file that stops being a session after listing.
    let broken = fixture(
        &world,
        "broken.jsonl",
        "01cccccc-bad",
        &world.project(),
        "broken",
    );
    h.submit_id(4, Submission::FollowUp("/resume".into()));
    let dialog = h.next_dialog();
    std::fs::write(&broken, "not json\n").expect("corrupt");
    h.choose_answer(dialog.id, choose(&dialog, "broken"));
    let updates = h.ended(4);
    assert!(refused(&updates).is_some(), "{updates:#?}");
    assert!(replaced(&updates).is_none());
    h.turn(5, "still mine");
    h.stop();
    assert_eq!(texts(&file, "user"), ["mine", "still mine"]);
}

/// Writes a session file whose later assistant message, `later`, sits on
/// a branch the file's leaf has left: the leaf is a final user message,
/// `main line`, after the first exchange, named `name` when given.
fn searchable(
    world: &World,
    file: &str,
    id: &str,
    cwd: &str,
    later: &str,
    name: Option<&str>,
) -> PathBuf {
    let path = fixture(world, file, id, cwd, "first ask");
    let usage = json!({"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2,
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}});
    let mut lines = vec![
        json!({"type": "message", "id": "b1", "parentId": "a2", "timestamp": "2026-01-01T00:00:03.000Z",
            "message": {"role": "toolResult", "toolCallId": "t", "toolName": "read",
                "content": [{"type": "text", "text": "tool-only okapi"}], "isError": false, "timestamp": 1_767_225_603_000_i64}}),
        json!({"type": "message", "id": "b2", "parentId": "a1", "timestamp": "2026-01-01T00:00:04.000Z",
            "message": {"role": "assistant", "content": [{"type": "text", "text": later}],
                "api": "faux", "provider": "faux", "model": "faux-1", "stopReason": "stop",
                "timestamp": 1_767_225_604_000_i64, "usage": usage}}),
        json!({"type": "message", "id": "c1", "parentId": "a2", "timestamp": "2026-01-01T00:00:05.000Z",
            "message": {"role": "user", "content": "main line", "timestamp": 1_767_225_605_000_i64}}),
    ];
    if let Some(name) = name {
        lines.push(json!({"type": "session_info", "id": "n1", "parentId": "c1",
            "timestamp": "2026-01-01T00:00:06.000Z", "name": name}));
    }
    let mut body = std::fs::read_to_string(&path).expect("fixture");
    for line in lines {
        body.push_str(&format!("{line}\n"));
    }
    std::fs::write(&path, body).expect("fixture");
    path
}

/// Every session file's bytes, by path.
fn snapshot(world: &World) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files: Vec<(PathBuf, Vec<u8>)> = std::fs::read_dir(world.sessions())
        .expect("sessions")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .map(|path| {
            let bytes = std::fs::read(&path).expect("read");
            (path, bytes)
        })
        .collect();
    files.sort();
    files
}

// Pi session-selector-search.ts over Pi's listing: `/resume` searches every
// user and assistant message of this directory's sessions, a branch the
// leaf has left included, tool results not; phrases and `re:` patterns work, a
// bad pattern lists nothing and chooses nothing, and Ctrl+N keeps only
// named sessions. Searching, toggling, and cancelling write nothing and
// send nothing; the chosen id opens as before.
#[test]
fn resume_searches_every_message_and_opens_the_chosen_id() {
    use bake_tui_view::dialog::Dialog;
    use bake_tui_view::keys::{Key, KeyInput, Mods};
    let world = World::new("search");
    let manager = persisted(&world);
    let file = manager.session_file().expect("file").to_path_buf();
    let (h, requests) = session_harness(&world, manager, Vec::new(), None);
    h.turn(1, "mine");
    let project = world.project();
    searchable(
        &world,
        "target.jsonl",
        "01dddddd-target",
        &project,
        "Zebra\n\n  crossing notes",
        None,
    );
    searchable(
        &world,
        "named.jsonl",
        "01eeeeee-named",
        &project,
        "plain words",
        Some("Release prep"),
    );
    let foreign = world.root.join("elsewhere");
    std::fs::create_dir_all(&foreign).expect("foreign dir");
    searchable(
        &world,
        "foreign.jsonl",
        "01ffffff-foreign",
        foreign.to_str().expect("utf-8"),
        "zebra crossing notes",
        Some("Foreign"),
    );
    let before = snapshot(&world);
    let sent = h_requests(&requests);
    let key = |dialog: &mut Dialog, key: Key, mods: Mods| dialog.key(KeyInput { key, mods }, true);
    let values = |dialog: &Dialog| -> Vec<String> {
        let DialogKind::List(list) = &dialog.request.kind else {
            unreachable!()
        };
        dialog
            .rows()
            .iter()
            .map(|&i| list.items[i].value.clone())
            .collect()
    };

    // A cancelled search changes nothing.
    h.submit_id(2, Submission::FollowUp("/resume".into()));
    let mut dialog = Dialog::open(h.next_dialog());
    assert_eq!(
        values(&dialog).len(),
        3,
        "the foreign session is not listed"
    );
    dialog.paste("\"zebra crossing\"");
    assert_eq!(values(&dialog), ["01dddddd-target"]);
    let answer = key(&mut dialog, Key::Esc, Mods::NONE).expect("cancel");
    h.choose_answer(dialog.request.id, answer);
    assert_eq!(refused(&h.ended(2)).as_deref(), Some(commands::CANCELLED));

    h.submit_id(3, Submission::FollowUp("/resume".into()));
    let mut dialog = Dialog::open(h.next_dialog());
    dialog.paste("\"tool-only okapi\"");
    assert!(values(&dialog).is_empty(), "tool results are not searched");
    dialog.set_filter("re:(");
    assert!(values(&dialog).is_empty());
    assert!(
        dialog
            .search
            .as_ref()
            .is_some_and(|view| view.error.is_some())
    );
    assert_eq!(key(&mut dialog, Key::Enter, Mods::NONE), None);
    dialog.set_filter("re:CROSSING\\s+NOTES");
    assert_eq!(values(&dialog), ["01dddddd-target"]);
    dialog.set_filter("");
    key(&mut dialog, Key::Char('n'), Mods::CTRL);
    assert_eq!(values(&dialog), ["01eeeeee-named"]);
    key(&mut dialog, Key::Char('s'), Mods::CTRL);
    key(&mut dialog, Key::Char('n'), Mods::CTRL);
    dialog.set_filter("zebra");
    assert_eq!(values(&dialog), ["01dddddd-target"]);
    assert_eq!(snapshot(&world), before, "searching wrote nothing");
    assert_eq!(h_requests(&requests), sent, "searching sent nothing");
    let answer = key(&mut dialog, Key::Enter, Mods::NONE).expect("chosen");
    assert_eq!(
        answer,
        DialogAnswer::Chose {
            value: "01dddddd-target".into(),
            save_default: false,
        }
    );
    h.choose_answer(dialog.request.id, answer);
    let rows = replaced(&h.ended(3)).expect("replaced");
    assert!(rows.contains(&Row::User("first ask".into())), "{rows:#?}");
    assert!(rows.contains(&Row::User("main line".into())), "{rows:#?}");
    // The match was on the branch the leaf left, which does not replay.
    assert!(
        !rows
            .iter()
            .any(|row| format!("{row:?}").contains("crossing")),
        "{rows:#?}"
    );
    assert_eq!(h_requests(&requests), sent);
    h.stop();
    assert_eq!(texts(&file, "user"), ["mine"]);
}

// Bake: the factory refuses a session whose header names another
// directory, even in this one's session directory, so the tools never
// work in a workspace the terminal does not show.
#[test]
fn a_session_from_another_directory_is_not_opened() {
    let world = World::new("cwd");
    let manager = persisted(&world);
    let (h, _requests) = session_harness(&world, manager, Vec::new(), None);
    let session = Arc::new(h.runtime.block_on(async {
        AgentSession::create(AgentSessionOptions {
            session: persisted(&world),
            model: None,
            registry: ModelRegistry::new(
                ModelConfig::from_value(json!({}), "models.json"),
                AuthStorage::empty(),
            )
            .into(),
            settings_store: None,
            selection: None,
            apis: Arc::new(ApiRegistry::new()),
            settings: Settings::default(),
            thinking_level: None,
            tools: Vec::new(),
            active_tool_names: Vec::new(),
            tool_prompts: Vec::new(),
            system_prompt: SystemPromptOptions::new(world.project()),
            docs_command: "bake-rs docs".into(),
            plugins: bake_coding_agent::plugins::PluginRegistry::default(),
            work: None,
            jobs: None,
        })
        .await
        .expect("session")
    }));
    let elsewhere = world.root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("dir");
    let path = fixture(
        &world,
        "other.jsonl",
        "01dddddd-else",
        &elsewhere.to_string_lossy(),
        "far",
    );
    let factory = SessionFactory::new(
        Args::default(),
        world.home(),
        "bake-rs docs".into(),
        Arc::new(bake_sandbox::provider::UnavailableProvider::new(None)),
        None,
    );
    let result = h.runtime.block_on(factory.replace(
        &session,
        bake_coding_agent::cli::SessionTarget::Open {
            path,
            id: "01dddddd-else".into(),
        },
    ));
    let Err(error) = result else {
        panic!("opened a session from another directory");
    };
    assert!(error.contains("started in"), "{error}");
    h.runtime.block_on(session.shutdown());
    h.stop();
}

// Pi `handleNameCommand` and `handleSessionCommand`: `/name` records a
// normalized name; `/session` is a read-only sheet of the log's facts that
// closes without a note, a draft, or a request.
#[test]
fn name_and_session_info_read_and_write_only_the_log() {
    let world = World::new("info");
    let manager = persisted(&world);
    let file = manager.session_file().expect("file").to_path_buf();
    let id = manager.session_id().to_owned();
    let (h, requests) = session_harness(&world, manager, Vec::new(), None);
    h.turn(1, "hello");
    let updates = h.command(2, "/name  Release   notes ");
    assert!(
        committed(&updates).contains(&Row::Info(
            "Session name set: Release   notes (normalized from \"Release   notes \")".into()
        )) || committed(&updates).contains(&Row::Info("Session name set: Release   notes".into())),
        "{updates:#?}"
    );
    h.submit_id(3, Submission::FollowUp("/session".into()));
    let dialog = h.next_dialog();
    let DialogKind::Info(info) = &dialog.kind else {
        panic!("{dialog:?}");
    };
    let field = |name: &str| {
        info.lines.iter().find_map(|line| match line {
            InfoLine::Field { label, value } if label == name => Some(value.clone()),
            _ => None,
        })
    };
    assert_eq!(field("Name").as_deref(), Some("Release   notes"));
    assert_eq!(field("ID"), Some(id));
    assert_eq!(field("File"), Some(file.display().to_string()));
    assert_eq!(field("User").as_deref(), Some("1"));
    assert_eq!(field("Assistant").as_deref(), Some("1"));
    h.choose_answer(dialog.id, DialogAnswer::Cancel);
    let updates = h.ended(3);
    assert!(
        matches!(updates.last(), Some(RuntimeUpdate::Admitted(3))),
        "{updates:#?}"
    );
    assert!(committed(&updates).is_empty(), "{updates:#?}");
    // `/name` with no argument asks, starting from the current name.
    h.submit_id(4, Submission::FollowUp("/name".into()));
    let dialog = h.next_dialog();
    let DialogKind::Field(field) = &dialog.kind else {
        panic!("{dialog:?}");
    };
    assert_eq!(field.initial, "Release   notes");
    h.choose_answer(dialog.id, DialogAnswer::Cancel);
    assert_eq!(refused(&h.ended(4)).as_deref(), Some(commands::CANCELLED));
    h.stop();
    let names: Vec<String> = entries(&file, "session_info")
        .iter()
        .filter_map(|entry| entry["name"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(names, ["Release   notes"]);
    assert_eq!(h_requests(&requests), 1);
}

// Pi `newSession` with an in-memory manager: under `--no-session`, `/new`
// starts another in-memory session and writes no file, and `/resume` is
// refused.
#[test]
fn no_session_stays_in_memory() {
    let world = World::new("memory");
    let manager = SessionManager::in_memory(
        Some(&world.project()),
        NewSessionOptions::default(),
        Vec::new(),
    )
    .expect("memory");
    let id = manager.session_id().to_owned();
    let (h, _requests) = session_harness(&world, manager, Vec::new(), None);
    h.turn(1, "hi");
    h.submit_id(2, Submission::FollowUp("/resume".into()));
    assert!(
        refused(&h.ended(2))
            .expect("refused")
            .contains("--no-session")
    );
    let updates = h.command(3, "/new");
    let rows = replaced(&updates).expect("replaced");
    assert!(
        rows.contains(&Row::Info("Session not saved (--no-session)".into())),
        "{rows:#?}"
    );
    h.submit_id(4, Submission::FollowUp("/session".into()));
    let dialog = h.next_dialog();
    let DialogKind::Info(info) = &dialog.kind else {
        panic!("{dialog:?}");
    };
    assert!(!info.lines.contains(&InfoLine::Field {
        label: "ID".into(),
        value: id
    }));
    h.choose_answer(dialog.id, DialogAnswer::Cancel);
    h.ended(4);
    h.stop();
    assert!(world.files().is_empty());
}

// Bake: an Esc that reaches the driver after the replacement was built
// and checked, but before the driver commits it, discards it: the running
// session stays, and the draft comes back. The gate opens only once the
// driver has taken the Esc.
#[test]
fn an_esc_at_the_commit_boundary_discards_the_replacement() {
    let world = World::new("boundary");
    let manager = persisted(&world);
    let file = manager.session_file().expect("file").to_path_buf();
    let (reached_tx, reached) = std_mpsc::channel();
    let gate = commands::CommitGate {
        reached: reached_tx,
        release: commands::Release::Aborted,
    };
    let (h, _requests) = session_harness(&world, manager, Vec::new(), Some(gate));
    h.turn(1, "kept");
    let target = fixture(
        &world,
        "target.jsonl",
        "01eeeeee-target",
        &world.project(),
        "target",
    );
    h.submit_id(2, Submission::FollowUp("/resume".into()));
    let dialog = h.next_dialog();
    h.choose_answer(dialog.id, choose(&dialog, "target"));
    reached.recv_timeout(PATIENCE).expect("built and held");
    h.choose_answer(dialog.id, DialogAnswer::Cancel);
    let updates = h.ended(2);
    assert_eq!(refused(&updates).as_deref(), Some(commands::CANCELLED));
    assert!(replaced(&updates).is_none());
    h.turn(3, "after");
    h.stop();
    assert_eq!(texts(&file, "user"), ["kept", "after"]);
    assert_eq!(texts(&target, "user"), ["target"]);
    // The discarded replacement released the target's writer lock.
    SessionManager::open(&target, None, None).expect("target free");
}

// Bake: stopping the port while a built replacement is held shuts the
// replacement down as well as the running session: neither file stays
// locked, and nothing outlives the port.
#[test]
fn stopping_during_a_held_replacement_shuts_both_sessions_down() {
    let world = World::new("stop");
    let manager = persisted(&world);
    let file = manager.session_file().expect("file").to_path_buf();
    let (reached_tx, reached) = std_mpsc::channel();
    let gate = commands::CommitGate {
        reached: reached_tx,
        release: commands::Release::Aborted,
    };
    let (h, _requests) = session_harness(&world, manager, Vec::new(), Some(gate));
    h.turn(1, "mine");
    let target = fixture(
        &world,
        "held.jsonl",
        "01ffffff-held",
        &world.project(),
        "held",
    );
    h.submit_id(2, Submission::FollowUp("/resume".into()));
    let dialog = h.next_dialog();
    h.choose_answer(dialog.id, choose(&dialog, "held"));
    reached.recv_timeout(PATIENCE).expect("built and held");
    h.stop();
    SessionManager::open(&file, None, None).expect("running file free");
    SessionManager::open(&target, None, None).expect("target free");
    assert_eq!(texts(&target, "user"), ["held"]);
}

// Bake: after `/new`, the session a panicking driver's handler shuts down
// is the one in use, not the one the port started with: both files are
// free when `stop` returns, and nothing holds the update callback.
#[test]
fn a_driver_panic_after_new_shuts_down_the_session_in_use() {
    let world = World::new("panic");
    let manager = persisted(&world);
    let first = manager.session_file().expect("file").to_path_buf();
    let (mut h, _requests, callback_alive) =
        panicking_session_harness(&world, manager, Vec::new(), None, Some(99));
    h.turn(1, "first");
    let rows = replaced(&h.command(2, "/new")).expect("replaced");
    let second = saved_file(&rows);
    h.turn(3, "second");
    // Held: another process cannot take the file the session writes.
    assert!(SessionManager::open(&second, None, None).is_err());
    h.submit_id(99, Submission::FollowUp("/copy".into()));
    let error = h
        .port
        .take()
        .expect("port")
        .stop()
        .expect_err("the panic is reported");
    assert!(error.to_string().contains("driver failed"), "{error}");
    SessionManager::open(&second, None, None).expect("the session in use was shut down");
    SessionManager::open(&first, None, None).expect("the first session was shut down");
    assert!(
        callback_alive.upgrade().is_none(),
        "a task outlived the port"
    );
    assert_eq!(texts(&second, "user"), ["second"]);
}

/// Answers `/tree`'s "Summarize branch?" with "No summary", returning
/// the prompt.
fn no_summary(h: &Harness) -> DialogRequest {
    let prompt = h.next_dialog();
    let DialogKind::List(list) = &prompt.kind else {
        panic!("{prompt:?}");
    };
    assert_eq!(list.title, "Summarize branch?");
    h.choose_answer(prompt.id, choose(&prompt, "No summary"));
    prompt
}

fn restored(updates: &[RuntimeUpdate]) -> Option<String> {
    updates.iter().find_map(|update| match update {
        RuntimeUpdate::Restored(text) => Some(text.clone()),
        _ => None,
    })
}

// Pi `showTreeSelector` and `navigateTree` on a user message: the sheet
// marks the leaf current and the path with `•`; choosing an earlier user
// message moves the leaf to its parent in the same file, replays that
// path, and puts the message back in the draft without sending it. The
// next prompt carries the new path and branches in the file.
#[test]
fn tree_moves_the_leaf_and_the_next_prompt_branches_in_the_same_file() {
    let world = World::new("tree");
    let manager = persisted(&world);
    let file = manager.session_file().expect("file").to_path_buf();
    let (h, requests) = session_harness(&world, manager, Vec::new(), None);
    h.turn(1, "alpha");
    h.turn(2, "beta");
    h.submit_id(3, Submission::FollowUp("/tree".into()));
    let dialog = h.next_dialog();
    let DialogKind::List(list) = &dialog.kind else {
        panic!("{dialog:?}");
    };
    let current = list.current.expect("current");
    assert_eq!(list.items[current].label, "• assistant: Echo: beta.");
    assert!(list.filterable);
    h.choose_answer(dialog.id, choose(&dialog, "• user: beta"));
    no_summary(&h);
    let mut updates = h.ended(3);
    updates.extend(h.until(|u| matches!(u, RuntimeUpdate::Status(_))));
    let rows = replaced(&updates).expect("replaced");
    assert_eq!(
        rows.first(),
        Some(&Row::Info("Navigated to selected point".into()))
    );
    assert!(
        rows.contains(&Row::Answer("Echo: alpha.".into())),
        "{rows:#?}"
    );
    assert!(!rows.contains(&Row::User("beta".into())), "{rows:#?}");
    assert_eq!(restored(&updates).as_deref(), Some("beta"));
    assert_eq!(h_requests(&requests), 2, "navigation sent no request");
    h.turn(4, "gamma");
    let (_, sent) = requests.lock().expect("requests")[2].clone();
    assert!(sent.contains("alpha") && sent.contains("gamma"), "{sent}");
    assert!(!sent.contains("beta"), "{sent}");
    h.stop();
    assert_eq!(world.files(), std::slice::from_ref(&file), "the same file");
    assert_eq!(texts(&file, "user"), ["alpha", "beta", "gamma"]);
    let messages = entries(&file, "message");
    let parent_of = |text: &str| {
        messages
            .iter()
            .find(|entry| entry["message"]["content"] == text)
            .map(|entry| entry["parentId"].clone())
    };
    // beta and gamma are siblings under alpha's answer.
    assert_eq!(parent_of("beta"), parent_of("gamma"));
}

// Pi: choosing the current point changes nothing; a cancelled sheet keeps
// the leaf and gives the draft back; neither sends a request.
#[test]
fn tree_at_the_leaf_or_cancelled_changes_nothing() {
    let world = World::new("tree-noop");
    let manager = persisted(&world);
    let file = manager.session_file().expect("file").to_path_buf();
    let (h, requests) = session_harness(&world, manager, Vec::new(), None);
    h.turn(1, "alpha");
    h.submit_id(2, Submission::FollowUp("/tree".into()));
    let dialog = h.next_dialog();
    h.choose_answer(dialog.id, choose(&dialog, "• assistant: Echo: alpha."));
    let updates = h.settled_command(2);
    assert!(replaced(&updates).is_none());
    assert!(updates.contains(&RuntimeUpdate::Commit(vec![Row::Info(
        "Already at this point".into()
    )])));
    h.submit_id(3, Submission::FollowUp("/tree".into()));
    let dialog = h.next_dialog();
    h.choose_answer(dialog.id, DialogAnswer::Cancel);
    assert_eq!(refused(&h.ended(3)).as_deref(), Some(commands::CANCELLED));
    h.submit_id(4, Submission::FollowUp("/tree now".into()));
    assert!(
        refused(&h.ended(4))
            .expect("refused")
            .contains("no argument")
    );
    h.turn(5, "beta");
    h.stop();
    assert_eq!(texts(&file, "user"), ["alpha", "beta"]);
    assert_eq!(h_requests(&requests), 2);
}

// Pi "should fork from middle of conversation" and `fork` restoring the
// fork's recorded model: the fork is a new file in the same directory that
// names the source as its parent and holds the path before the chosen
// message; the source file is unchanged; the message is back in the
// draft; later prompts land in the fork.
#[test]
fn fork_starts_a_new_file_and_leaves_the_source_unchanged() {
    let world = World::new("fork");
    let manager = persisted(&world);
    let source = manager.session_file().expect("file").to_path_buf();
    let (h, requests) = session_harness(&world, manager, Vec::new(), None);
    h.turn(1, "alpha");
    h.command(2, "/model faux/faux-2");
    h.command(3, "/thinking high");
    h.turn(4, "beta");
    // A later change, on the source's path only.
    h.command(5, "/model faux/faux-1");
    let before = std::fs::read(&source).expect("source");
    h.submit_id(6, Submission::FollowUp("/fork".into()));
    let dialog = h.next_dialog();
    let DialogKind::List(list) = &dialog.kind else {
        panic!("{dialog:?}");
    };
    assert_eq!(list.items.len(), 2);
    assert_eq!(list.selected, Some(1), "the latest message first");
    assert_eq!(list.current, None);
    h.choose_answer(dialog.id, choose(&dialog, "beta"));
    let mut updates = h.ended(6);
    updates.extend(h.until(|u| matches!(u, RuntimeUpdate::Status(_))));
    let rows = replaced(&updates).expect("replaced");
    assert_eq!(
        rows.first(),
        Some(&Row::Info("Forked to new session".into()))
    );
    let forked = saved_file(&rows);
    assert_ne!(forked, source);
    assert_eq!(forked.parent(), source.parent());
    assert!(rows.contains(&Row::User("alpha".into())), "{rows:#?}");
    assert!(!rows.contains(&Row::User("beta".into())), "{rows:#?}");
    assert_eq!(restored(&updates).as_deref(), Some("beta"));
    let status = last_status(&updates).expect("status");
    assert_eq!(status.model.as_deref(), Some("faux/faux-2"));
    assert_eq!(status.thinking.as_deref(), Some("high"));
    assert_eq!(std::fs::read(&source).expect("source"), before);
    let header = entries(&forked, "session");
    assert_eq!(
        header[0]["parentSession"].as_str(),
        Some(source.to_string_lossy().as_ref())
    );
    let sent_before = h_requests(&requests);
    assert_eq!(sent_before, 2, "the sheet sent no request");
    h.turn(7, "gamma");
    let (model, sent) = requests.lock().expect("requests")[2].clone();
    assert_eq!(model, "faux-2");
    assert!(sent.contains("alpha") && sent.contains("gamma"), "{sent}");
    assert!(!sent.contains("beta"), "{sent}");
    h.stop();
    assert_eq!(std::fs::read(&source).expect("source"), before);
    assert_eq!(texts(&forked, "user"), ["alpha", "gamma"]);
    SessionManager::open(&source, None, None).expect("source free");
    SessionManager::open(&forked, None, None).expect("fork free");
}

// Pi "should support in-memory forking in --no-session mode".
#[test]
fn fork_under_no_session_stays_in_memory() {
    let world = World::new("fork-memory");
    let manager = SessionManager::in_memory(
        Some(&world.project()),
        NewSessionOptions::default(),
        Vec::new(),
    )
    .expect("memory");
    let (h, _requests) = session_harness(&world, manager, Vec::new(), None);
    h.turn(1, "alpha");
    h.turn(2, "beta");
    h.submit_id(3, Submission::FollowUp("/fork".into()));
    let dialog = h.next_dialog();
    h.choose_answer(dialog.id, choose(&dialog, "alpha"));
    let mut updates = h.ended(3);
    updates.extend(h.until(|u| matches!(u, RuntimeUpdate::Status(_))));
    let rows = replaced(&updates).expect("replaced");
    assert!(
        rows.contains(&Row::Info("Session not saved (--no-session)".into())),
        "{rows:#?}"
    );
    assert!(
        !rows.iter().any(|row| matches!(row, Row::User(_))),
        "{rows:#?}"
    );
    assert_eq!(restored(&updates).as_deref(), Some("alpha"));
    h.stop();
    assert!(world.files().is_empty());
}

// Bake: an Esc at the fork's commit boundary discards the built fork: the
// running session stays and keeps writing its file, the draft comes back,
// and the fork's file, written when it was built, is unlocked.
#[test]
fn an_esc_at_the_fork_commit_boundary_keeps_the_source_session() {
    let world = World::new("fork-boundary");
    let manager = persisted(&world);
    let source = manager.session_file().expect("file").to_path_buf();
    let (reached_tx, reached) = std_mpsc::channel();
    let gate = commands::CommitGate {
        reached: reached_tx,
        release: commands::Release::Aborted,
    };
    let (h, _requests) = session_harness(&world, manager, Vec::new(), Some(gate));
    h.turn(1, "alpha");
    h.turn(2, "beta");
    h.submit_id(3, Submission::FollowUp("/fork".into()));
    let dialog = h.next_dialog();
    h.choose_answer(dialog.id, choose(&dialog, "beta"));
    reached.recv_timeout(PATIENCE).expect("built and held");
    h.choose_answer(dialog.id, DialogAnswer::Cancel);
    let updates = h.ended(3);
    assert_eq!(refused(&updates).as_deref(), Some(commands::CANCELLED));
    assert!(replaced(&updates).is_none());
    assert!(restored(&updates).is_none());
    h.turn(4, "gamma");
    h.stop();
    assert_eq!(texts(&source, "user"), ["alpha", "beta", "gamma"]);
    for file in world.files() {
        SessionManager::open(&file, None, None).expect("unlocked");
    }
}

// Pi `handleCloneCommand` and runtime `fork(leafId, { position: "at" })`:
// the clone keeps the model and thinking level of its path (the /fork test
// shows them restored from the path rather than inherited). It is a new
// file in the same directory that names the source as its parent and
// holds the whole current path; the source file is unchanged; nothing
// comes back into the draft; later prompts land in the clone.
#[test]
fn clone_copies_the_current_path_into_a_new_file() {
    let world = World::new("clone");
    let manager = persisted(&world);
    let source = manager.session_file().expect("file").to_path_buf();
    let (h, requests) = session_harness(&world, manager, Vec::new(), None);
    h.turn(1, "alpha");
    h.command(2, "/model faux/faux-2");
    h.command(3, "/thinking high");
    h.turn(4, "beta");
    let before = std::fs::read(&source).expect("source");
    let updates = h.command(5, "/clone");
    let rows = replaced(&updates).expect("replaced");
    assert_eq!(
        rows.first(),
        Some(&Row::Info("Cloned to new session".into()))
    );
    let cloned = saved_file(&rows);
    assert_ne!(cloned, source);
    assert_eq!(cloned.parent(), source.parent());
    assert!(rows.contains(&Row::User("alpha".into())), "{rows:#?}");
    assert!(rows.contains(&Row::User("beta".into())), "{rows:#?}");
    assert!(restored(&updates).is_none());
    let status = last_status(&updates).expect("status");
    assert_eq!(status.model.as_deref(), Some("faux/faux-2"));
    assert_eq!(status.thinking.as_deref(), Some("high"));
    assert_eq!(std::fs::read(&source).expect("source"), before);
    let header = entries(&cloned, "session");
    assert_eq!(
        header[0]["parentSession"].as_str(),
        Some(source.to_string_lossy().as_ref())
    );
    assert_eq!(h_requests(&requests), 2, "cloning sent no request");
    h.turn(6, "gamma");
    let (model, sent) = requests.lock().expect("requests")[2].clone();
    assert_eq!(model, "faux-2");
    assert!(
        sent.contains("alpha") && sent.contains("beta") && sent.contains("gamma"),
        "{sent}"
    );
    h.stop();
    assert_eq!(std::fs::read(&source).expect("source"), before);
    assert_eq!(texts(&cloned, "user"), ["alpha", "beta", "gamma"]);
    SessionManager::open(&source, None, None).expect("source free");
    SessionManager::open(&cloned, None, None).expect("clone free");
}

// Pi "duplicates the current active branch in-memory when forking at the
// current position", through `/clone` under `--no-session`.
#[test]
fn clone_under_no_session_stays_in_memory() {
    let world = World::new("clone-memory");
    let manager = SessionManager::in_memory(
        Some(&world.project()),
        NewSessionOptions::default(),
        Vec::new(),
    )
    .expect("memory");
    let (h, _requests) = session_harness(&world, manager, Vec::new(), None);
    h.turn(1, "alpha");
    h.turn(2, "beta");
    let updates = h.command(3, "/clone");
    let rows = replaced(&updates).expect("replaced");
    assert!(
        rows.contains(&Row::Info("Session not saved (--no-session)".into())),
        "{rows:#?}"
    );
    assert!(rows.contains(&Row::User("alpha".into())), "{rows:#?}");
    assert!(rows.contains(&Row::User("beta".into())), "{rows:#?}");
    assert!(restored(&updates).is_none());
    h.stop();
    assert!(world.files().is_empty());
}

// Pi "reports why an unflushed session cannot be forked": an unsaved
// session with entries refuses, the draft kept. Pi's "nothing to clone"
// case is unreachable here, since every live session writes its thinking
// level when it starts.
#[test]
fn clone_without_a_saved_leaf_changes_nothing() {
    let world = World::new("clone-empty");
    let manager = persisted(&world);
    let file = manager.session_file().expect("file").to_path_buf();
    let (h, _requests) = session_harness(&world, manager, Vec::new(), None);
    let updates = h.command(1, "/clone");
    assert_eq!(
        refused(&updates).as_deref(),
        Some(
            "Could not clone the session: This session has not been saved yet. Send a message before cloning or forking it.; your draft is kept."
        )
    );
    assert!(replaced(&updates).is_none());
    h.turn(2, "alpha");
    h.stop();
    assert_eq!(texts(&file, "user"), ["alpha"]);
    assert_eq!(world.files(), [file]);
}

// Bake: an Esc after the `/tree` choice, before the session moves the
// leaf, moves nothing: the transcript and draft come back as they were and
// the next prompt continues from the old leaf.
#[test]
fn an_esc_before_the_tree_commit_moves_nothing() {
    let world = World::new("tree-boundary");
    let manager = persisted(&world);
    let file = manager.session_file().expect("file").to_path_buf();
    let (reached_tx, reached) = std_mpsc::channel();
    let gate = commands::CommitGate {
        reached: reached_tx,
        release: commands::Release::Aborted,
    };
    let (h, requests) = session_harness(&world, manager, Vec::new(), Some(gate));
    h.turn(1, "alpha");
    h.turn(2, "beta");
    h.submit_id(3, Submission::FollowUp("/tree".into()));
    let dialog = h.next_dialog();
    h.choose_answer(dialog.id, choose(&dialog, "• user: beta"));
    let prompt = no_summary(&h);
    reached.recv_timeout(PATIENCE).expect("chosen and held");
    h.choose_answer(prompt.id, DialogAnswer::Cancel);
    let updates = h.ended(3);
    assert_eq!(refused(&updates).as_deref(), Some(commands::CANCELLED));
    assert!(replaced(&updates).is_none());
    assert!(restored(&updates).is_none());
    h.turn(4, "gamma");
    let (_, sent) = requests.lock().expect("requests")[2].clone();
    assert!(sent.contains("beta") && sent.contains("gamma"), "{sent}");
    h.stop();
    assert_eq!(texts(&file, "user"), ["alpha", "beta", "gamma"]);
}

#[path = "compact_live.rs"]
mod compact_live;

#[path = "tree_live.rs"]
mod tree_live;
