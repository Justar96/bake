//! Cases from Pi v1.1.0 `test/system-prompt-updates.test.ts`,
//! `test/agent-session-concurrent.test.ts`, and the setup rules of
//! `src/core/sdk.ts`, named after the Pi test each follows. They run the
//! session over `bake-ai`'s faux provider, registered through
//! [`ModelRegistry::add_provider_config`].

use std::sync::Arc;

use bake_agent::AgentToolResult;
use bake_ai::providers::faux::{
    FauxProvider, FauxProviderOptions, FauxResponseStep, faux_assistant_blocks,
    faux_assistant_message, faux_text, faux_tool_call,
};
use bake_ai::utils::text::get_system_message_text;
use bake_ai::{ApiRegistry, StopReason};
use serde_json::json;
use tokio::sync::Notify;

#[cfg(unix)]
mod read_shutdown;

use super::*;
use crate::auth_storage::AuthStorage;
use crate::model_registry::config::ModelConfig;
use crate::session::NewSessionOptions;
use crate::system_prompt::build_system_prompt;
use crate::test_support::TempDir;

pub(super) struct Harness {
    pub(super) faux: Arc<FauxProvider>,
    registry: Arc<ModelRegistry>,
    apis: Arc<ApiRegistry>,
}

pub(super) fn harness() -> Harness {
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
                "models": [{ "id": "faux-1", "reasoning": true }],
            }),
            Some("test-key".into()),
        )
        .expect("faux provider");
    Harness {
        faux,
        registry: Arc::new(registry),
        apis,
    }
}

pub(super) fn options(
    harness: &Harness,
    session: SessionManager,
    cwd: &str,
) -> AgentSessionOptions {
    AgentSessionOptions {
        session,
        registry: Arc::clone(&harness.registry).into(),
        settings_store: None,
        selection: None,
        apis: Arc::clone(&harness.apis),
        settings: Settings::default(),
        model: harness.registry.model("faux", "faux-1"),
        thinking_level: None,
        tools: Vec::new(),
        active_tool_names: Vec::new(),
        tool_prompts: Vec::new(),
        system_prompt: SystemPromptOptions::new(cwd),
        docs_command: "/opt/bake/bin/bake-rs docs".into(),
        plugins: crate::plugins::PluginRegistry::default(),
        work: None,
    }
}

fn entry_kinds(session: &AgentSession) -> Vec<String> {
    lock(&session.session)
        .entries()
        .iter()
        .map(|entry| {
            let kind = entry.entry_type().to_owned();
            match entry.as_json().get("message").and_then(|m| m.get("role")) {
                Some(role) => format!("{kind}:{}", role.as_str().unwrap_or_default()),
                None => kind,
            }
        })
        .collect()
}

fn roles(messages: &[LiveMessage]) -> Vec<String> {
    messages
        .iter()
        .map(|message| match message {
            LiveMessage::Llm(llm) => llm.role().to_owned(),
            LiveMessage::Custom(custom) => custom.role().to_owned(),
        })
        .collect()
}

pub(super) fn new_file_session(dir: &TempDir) -> SessionManager {
    SessionManager::create(
        &dir.path().to_string_lossy(),
        Some(&dir.join("sessions")),
        NewSessionOptions::default(),
    )
    .expect("session")
}

// Bake: shutdown releases the session file for good. A later model change
// is refused: it takes no lock, writes nothing, and leaves the model.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_closes_the_session_file() {
    let dir = TempDir::new("shutdown-closes");
    let cwd = dir.path().to_string_lossy().into_owned();
    let h = harness();
    h.faux
        .set_responses(vec![faux_assistant_message("first").into()]);
    let session = AgentSession::create(options(&h, new_file_session(&dir), &cwd))
        .await
        .expect("session");
    session.prompt("one", Vec::new()).await.expect("one");
    let file = session.session_file().expect("persisted");
    session.shutdown().await;
    let written = std::fs::read(&file).expect("read");
    let model = session.model().expect("model");
    let error = session
        .set_model(model.clone(), false)
        .await
        .expect_err("closed");
    assert_eq!(error, "The session is shut down.");
    let error = session
        .set_thinking_level(ModelThinkingLevel::Low, false)
        .await
        .expect_err("closed");
    assert_eq!(error, "The session is shut down.");
    assert_eq!(session.model(), Some(model));
    assert_eq!(std::fs::read(&file).expect("read"), written);
    // Another writer takes the file at once.
    drop(SessionManager::open(&file, None, None).expect("open"));
}

// Pi: "declares the prompt and tools once and reuses them across resume".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declares_the_prompt_once_and_reuses_it_across_resume() {
    let dir = TempDir::new("prompt-once");
    let cwd = dir.path().to_string_lossy().into_owned();
    let h = harness();
    h.faux.set_responses(vec![
        faux_assistant_message("first").into(),
        faux_assistant_message("second").into(),
        faux_assistant_message("third").into(),
    ]);
    let session = AgentSession::create(options(&h, new_file_session(&dir), &cwd))
        .await
        .expect("session");
    session.prompt("one", Vec::new()).await.expect("one");
    session.prompt("two", Vec::new()).await.expect("two");
    assert_eq!(
        roles(&session.messages()),
        ["system", "user", "assistant", "user", "assistant"]
    );
    let head = session.messages()[0]
        .as_system()
        .cloned()
        .expect("system head");
    assert_eq!(head.content, SystemContent::Text(String::new()));
    let names: Vec<String> = head
        .sections
        .as_ref()
        .map(|sections| sections.0.iter().map(|(name, _)| name.clone()).collect())
        .unwrap_or_default();
    assert_eq!(names, ["preamble", "rules", "docs", "run", "environment"]);
    let mut expected = SystemPromptOptions::new(cwd.clone());
    expected.selected_tools = Vec::new();
    expected.docs_command = "/opt/bake/bin/bake-rs docs".into();
    assert_eq!(
        get_system_message_text(&head),
        build_system_prompt(&expected).expect("prompt")
    );
    assert_eq!(
        entry_kinds(&session),
        [
            "model_change",
            "thinking_level_change",
            "message:system",
            "message:user",
            "message:assistant",
            "message:user",
            "message:assistant",
        ]
    );
    let file = session.session_file().expect("persisted");
    session.shutdown().await;

    // Resume: the restored transcript already declares the prompt.
    let reopened = SessionManager::open(&file, None, None).expect("open");
    let resumed = AgentSession::create(AgentSessionOptions {
        model: None,
        ..options(&h, reopened, &cwd)
    })
    .await
    .expect("resumed");
    assert_eq!(resumed.model().map(|m| m.id).as_deref(), Some("faux-1"));
    resumed.prompt("three", Vec::new()).await.expect("three");
    let kinds = entry_kinds(&resumed);
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| *kind == "message:system")
            .count(),
        1
    );
    assert_eq!(kinds.len(), 9, "{kinds:?}");
    assert_eq!(h.faux.call_count(), 3);
    resumed.shutdown().await;
}

// Pi: "opens a transcript without a system message and declares the prompt
// on the first request".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opens_a_transcript_without_a_system_message() {
    let h = harness();
    let mut manager =
        SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
            .expect("session");
    manager
        .append_message(UserMessage {
            content: UserContent::Text("existing".into()),
            timestamp: 1,
        })
        .expect("append");
    let session = AgentSession::create(options(&h, manager, "/tmp"))
        .await
        .expect("session");
    assert_eq!(roles(&session.messages()), ["user"]);
    let context = lock(&session.session).build_session_context();
    assert_eq!(context.messages.len(), 1);
    // An existing session without a thinking entry gets one, as in Pi.
    assert_eq!(
        entry_kinds(&session),
        ["message:user", "thinking_level_change"]
    );
    session.shutdown().await;
}

// Pi: "should throw when prompt() called while streaming".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refuses_a_prompt_while_streaming() {
    let h = harness();
    let release = Arc::new(Notify::new());
    let gate = Arc::clone(&release);
    h.faux
        .set_responses(vec![FauxResponseStep::Factory(Arc::new(
            move |_, _, _, _| {
                let gate = Arc::clone(&gate);
                Box::pin(async move {
                    gate.notified().await;
                    Ok(faux_assistant_message("late"))
                })
            },
        ))]);
    let manager = SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
        .expect("session");
    let session = Arc::new(
        AgentSession::create(options(&h, manager, "/tmp"))
            .await
            .expect("session"),
    );
    let first = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.prompt("First message", Vec::new()).await })
    };
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while !session.agent().is_streaming() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "never started streaming"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(
        session.prompt("Second message", Vec::new()).await,
        Err("Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.".into())
    );
    release.notify_one();
    assert_eq!(first.await.expect("joined"), Ok(()));
    // Pi: "should allow prompt() after previous completes".
    h.faux
        .set_responses(vec![faux_assistant_message("again").into()]);
    assert_eq!(session.prompt("Third", Vec::new()).await, Ok(()));
    session.shutdown().await;
}

// Pi v1.2.0 `steer`, `clearQueue`, and the `message_start` queue
// bookkeeping: a steer queued while a response streams is delivered at the
// next step, persisted, and leaves the queue; `clear_queue` hands back what
// was not delivered and empties the agent's queue too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steering_is_delivered_at_the_next_step_and_clear_queue_returns_the_rest() {
    let dir = TempDir::new("steer-queue");
    let cwd = dir.path().to_string_lossy().into_owned();
    let h = harness();
    let release = Arc::new(Notify::new());
    let gate = Arc::clone(&release);
    h.faux.set_responses(vec![
        FauxResponseStep::Factory(Arc::new(move |_, _, _, _| {
            let gate = Arc::clone(&gate);
            Box::pin(async move {
                gate.notified().await;
                Ok(faux_assistant_message("first"))
            })
        })),
        faux_assistant_message("after steer").into(),
    ]);
    let session = Arc::new(
        AgentSession::create(options(&h, new_file_session(&dir), &cwd))
            .await
            .expect("session"),
    );
    let updates = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = Arc::clone(&updates);
    let subscription = session.subscribe(move |event| {
        if let SessionEvent::QueueUpdate {
            steering,
            follow_up,
        } = event
        {
            seen.lock()
                .unwrap()
                .push((steering.to_vec(), follow_up.to_vec()));
        }
    });
    let run = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.prompt("start", Vec::new()).await })
    };
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    // Steered once the first request is held, so it waits for the next step.
    while h.faux.call_count() < 1 {
        assert!(tokio::time::Instant::now() < deadline, "never requested");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    session.steer("steer me", Vec::new()).await.expect("steer");
    assert_eq!(session.queued_messages().steering, ["steer me"]);
    release.notify_one();
    assert_eq!(run.await.expect("joined"), Ok(()));
    assert!(session.queued_messages().is_empty());
    let users: Vec<String> = session
        .messages()
        .iter()
        .filter_map(|message| message.as_user())
        .map(|user| bake_ai::utils::text::user_content_text(&user.content, ""))
        .collect();
    assert_eq!(users, ["start", "steer me"]);
    assert_eq!(
        bake_ai::utils::text::assistant_text(
            session
                .messages()
                .last()
                .and_then(|m| m.as_assistant())
                .expect("assistant")
        ),
        "after steer"
    );
    // The steer reached the session file.
    let file = std::fs::read_to_string(session.session_file().expect("file")).expect("read");
    assert!(file.contains("steer me"), "{file}");

    // Queued while idle, then cleared: nothing reaches the agent.
    session
        .follow_up("later", Vec::new())
        .await
        .expect("follow up");
    assert!(session.agent().has_queued_messages());
    let cleared = session.clear_queue();
    assert_eq!(cleared.follow_up, ["later"]);
    assert!(!session.agent().has_queued_messages());
    subscription.unsubscribe();
    assert_eq!(
        *updates.lock().unwrap(),
        [
            (vec!["steer me".to_owned()], Vec::new()),
            (Vec::new(), Vec::new()),
            (Vec::new(), vec!["later".to_owned()]),
            (Vec::new(), Vec::new()),
        ]
    );
    session.shutdown().await;
}

// Bake: queue updates reach listeners with no lock held, so a listener may
// read and clear the queue; after shutdown nothing more is queued.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_listeners_may_use_the_queue_and_shutdown_refuses_more() {
    let h = harness();
    let manager = SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
        .expect("session");
    let session = Arc::new(
        AgentSession::create(options(&h, manager, "/tmp"))
            .await
            .expect("session"),
    );
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let subscription = {
        let weak = Arc::downgrade(&session);
        let seen = Arc::clone(&seen);
        session.subscribe(move |event| {
            if let SessionEvent::QueueUpdate { steering, .. } = event {
                seen.lock().unwrap().push(steering.to_vec());
                let Some(session) = weak.upgrade() else {
                    return;
                };
                // Reentrant: reading, then clearing, from inside the update.
                if !session.queued_messages().steering.is_empty() {
                    session.clear_queue();
                }
            }
        })
    };
    let steer = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        session.steer("one", Vec::new()),
    )
    .await
    .expect("no deadlock");
    assert_eq!(steer, Ok(()));
    assert!(session.queued_messages().is_empty());
    assert!(!session.agent().has_queued_messages());
    // The clear announced after the update it ran in.
    assert_eq!(*seen.lock().unwrap(), [vec!["one".to_owned()], Vec::new()]);
    subscription.unsubscribe();
    session.shutdown().await;
    assert_eq!(
        session.steer("late", Vec::new()).await,
        Err("The session is shut down.".into())
    );
    assert_eq!(
        session.follow_up("late", Vec::new()).await,
        Err("The session is shut down.".into())
    );
    assert!(!session.agent().has_queued_messages());
}

// A listener that clears the queue during an update does not make a later
// listener see the cleared queue before the update it was cleared in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_listener_sees_queue_updates_in_order() {
    let h = harness();
    let manager = SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
        .expect("session");
    let session = Arc::new(
        AgentSession::create(options(&h, manager, "/tmp"))
            .await
            .expect("session"),
    );
    let clearer = {
        let weak = Arc::downgrade(&session);
        session.subscribe(move |event| {
            if let SessionEvent::QueueUpdate { steering, .. } = event
                && let (false, Some(session)) = (steering.is_empty(), weak.upgrade())
            {
                session.clear_queue();
            }
        })
    };
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let watcher = {
        let seen = Arc::clone(&seen);
        session.subscribe(move |event| {
            if let SessionEvent::QueueUpdate { steering, .. } = event {
                seen.lock().unwrap().push(steering.to_vec());
            }
        })
    };
    assert_eq!(session.steer("one", Vec::new()).await, Ok(()));
    assert_eq!(*seen.lock().unwrap(), [vec!["one".to_owned()], Vec::new()]);
    clearer.unsubscribe();
    watcher.unsubscribe();
    session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn message_end_observers_see_the_committed_assistant() {
    let dir = TempDir::new("observer-commit");
    let cwd = dir.path().to_string_lossy().into_owned();
    let h = harness();
    h.faux
        .set_responses(vec![faux_assistant_message("committed answer").into()]);
    let manager = new_file_session(&dir);
    let file = manager.session_file().expect("file path").to_path_buf();
    let session = AgentSession::create(options(&h, manager, &cwd))
        .await
        .expect("session");
    let observations = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&observations);
    session.subscribe(move |event| {
        if let SessionEvent::Agent(AgentEvent::MessageEnd { message }) = event
            && let Some(assistant) = message.as_assistant()
        {
            let written = std::fs::read_to_string(&file)
                .map_err(|error| error.to_string())
                .map(|text| {
                    text.lines().any(|line| {
                        serde_json::from_str::<serde_json::Value>(line)
                            .ok()
                            .and_then(|entry| entry.get("message").cloned())
                            .and_then(|value| serde_json::from_value::<Message>(value).ok())
                            .is_some_and(|stored| stored == Message::Assistant(assistant.clone()))
                    })
                });
            observed.lock().expect("observations").push(written);
        }
    });
    let result = session.prompt("answer", Vec::new()).await;
    session.shutdown().await;
    result.expect("prompt");
    assert_eq!(*observations.lock().expect("observations"), [Ok(true)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panicking_session_observer_does_not_starve_persistence_or_observers() {
    let dir = TempDir::new("observer-panic");
    let cwd = dir.path().to_string_lossy().into_owned();
    let h = harness();
    h.faux
        .set_responses(vec![faux_assistant_message("done").into()]);
    let session = AgentSession::create(options(&h, new_file_session(&dir), &cwd))
        .await
        .expect("session");
    session.subscribe(|event| {
        if matches!(event, SessionEvent::Agent(AgentEvent::MessageEnd { .. })) {
            panic!("broken presentation subscriber");
        }
    });
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    session.subscribe(move |event| {
        if let SessionEvent::Agent(AgentEvent::MessageEnd { message }) = event {
            log.lock()
                .expect("observations")
                .push(message.role().to_owned());
        }
    });
    let result = session.prompt("answer", Vec::new()).await;
    session.shutdown().await;
    result.expect("prompt");
    assert_eq!(h.faux.call_count(), 1);
    assert_eq!(
        *seen.lock().expect("observations"),
        ["system", "user", "assistant"]
    );
    let reopened = SessionManager::open(&session.session_file().expect("file"), None, None)
        .expect("persisted session");
    assert_eq!(reopened.build_session_context().messages.len(), 3);
}

// Pi: "should persist message_end events in order with slow extension
// handlers", with a slow session listener in place of the extension.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persists_message_end_events_in_order() {
    let dir = TempDir::new("persist-order");
    let cwd = dir.path().to_string_lossy().into_owned();
    let h = harness();
    h.faux.set_responses(vec![
        faux_assistant_blocks(
            vec![
                faux_text("calling tool"),
                faux_tool_call("dummy", json!({ "q": "x" }), Some("toolu_1")),
            ],
            StopReason::ToolUse,
        )
        .into(),
        faux_assistant_message("done").into(),
    ]);
    let tool = AgentTool::new(
        "dummy",
        "dummy",
        "Dummy tool",
        json!({ "type": "object", "properties": { "q": { "type": "string" } } }),
        |_, params, _, _| {
            let q = params["q"].as_str().unwrap_or_default().to_owned();
            Box::pin(async move {
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new(format!(
                        "result:{q}"
                    )))],
                    ..AgentToolResult::default()
                })
            })
        },
    );
    let session = AgentSession::create(AgentSessionOptions {
        tools: vec![Arc::new(tool)],
        active_tool_names: vec!["dummy".into(), "missing".into()],
        ..options(&h, new_file_session(&dir), &cwd)
    })
    .await
    .expect("session");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let _subscription = session.subscribe(move |event| {
        if let SessionEvent::Agent(AgentEvent::MessageEnd { message }) = event {
            std::thread::sleep(std::time::Duration::from_millis(5));
            log.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(roles(std::slice::from_ref(message)).concat());
        }
    });
    session.prompt("run", Vec::new()).await.expect("prompt");
    assert_eq!(
        entry_kinds(&session),
        [
            "model_change",
            "thinking_level_change",
            "message:system",
            "message:user",
            "message:assistant",
            "message:toolResult",
            "message:assistant",
        ]
    );
    assert_eq!(
        *seen.lock().unwrap_or_else(PoisonError::into_inner),
        ["system", "user", "assistant", "toolResult", "assistant"]
    );
    // The declaration and the prompt list only the tool that exists.
    let head = session.messages()[0].as_system().cloned().expect("system");
    let declared: Vec<String> = head
        .tools_added
        .unwrap_or_default()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(declared, ["dummy"]);
    session.shutdown().await;
}

// Pi `createAgentSession`: an explicit thinking level is clamped to the
// model, a model without reasoning runs with `off`, and a new session
// records both.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_sessions_record_the_model_and_clamped_thinking_level() {
    let h = harness();
    let manager = SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
        .expect("session");
    let mut plain = h.registry.model("faux", "faux-1").expect("model");
    plain.reasoning = false;
    let session = AgentSession::create(AgentSessionOptions {
        model: Some(plain),
        thinking_level: Some(ModelThinkingLevel::High),
        ..options(&h, manager, "/tmp")
    })
    .await
    .expect("session");
    assert_eq!(session.thinking_level(), ModelThinkingLevel::Off);
    let entries = lock(&session.session)
        .entries()
        .iter()
        .map(|entry| entry.as_json().clone())
        .collect::<Vec<_>>();
    assert_eq!(entries[0]["provider"], "faux");
    assert_eq!(entries[0]["modelId"], "faux-1");
    assert_eq!(entries[1]["thinkingLevel"], "off");
    session.shutdown().await;

    // A default thinking level from settings applies to a reasoning model.
    let manager = SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
        .expect("session");
    let settings = Settings {
        default_thinking_level: Some(ModelThinkingLevel::Low),
        ..Settings::default()
    };
    let session = AgentSession::create(AgentSessionOptions {
        settings,
        ..options(&h, manager, "/tmp")
    })
    .await
    .expect("session");
    assert_eq!(session.thinking_level(), ModelThinkingLevel::Low);
    session
        .set_thinking_level(ModelThinkingLevel::High, false)
        .await
        .expect("set");
    assert_eq!(
        entry_kinds(&session),
        [
            "model_change",
            "thinking_level_change",
            "thinking_level_change"
        ]
    );
    // Setting the same level records nothing.
    session
        .set_thinking_level(ModelThinkingLevel::High, false)
        .await
        .expect("set");
    assert_eq!(entry_kinds(&session).len(), 3);
    session.shutdown().await;
}

// Pi `prompt`: no model and no auth fail with Pi's guidance.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompts_need_a_model_with_auth() {
    let empty = Arc::new(ModelRegistry::new(
        ModelConfig::empty(),
        AuthStorage::empty(),
    ));
    let manager = SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
        .expect("session");
    let h = harness();
    let session = AgentSession::create(AgentSessionOptions {
        registry: Arc::clone(&empty).into(),
        model: None,
        ..options(&h, manager, "/tmp")
    })
    .await
    .expect("session");
    assert!(session.model().is_none());
    assert_eq!(
        session.model_fallback_message(),
        Some(format_no_models_available_message("/opt/bake/bin/bake-rs docs").as_str())
    );
    assert_eq!(
        session.prompt("hi", Vec::new()).await,
        Err(format_no_model_selected_message(
            "/opt/bake/bin/bake-rs docs"
        ))
    );
    session.shutdown().await;

    let manager = SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
        .expect("session");
    let session = AgentSession::create(AgentSessionOptions {
        registry: empty.into(),
        ..options(&h, manager, "/tmp")
    })
    .await
    .expect("session");
    assert_eq!(
        session.prompt("hi", Vec::new()).await,
        // Bake names its docs command where Pi names its docs files.
        Err(
            "No API key found for faux.\n\nAdd a provider to models.json in the Bake home, and its API key to models.json or auth.json. See:\n  /opt/bake/bin/bake-rs docs models"
                .into()
        )
    );
    assert_eq!(
        session
            .set_model(h.registry.model("faux", "faux-1").expect("model"), false)
            .await,
        Err("No API key for faux/faux-1".into())
    );
    session.shutdown().await;
}

// Pi `dispose` and `abort`: a shut-down session refuses prompts and leaves
// no run behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abort_and_shutdown_end_the_run() {
    let h = harness();
    let entered = Arc::new(tokio::sync::Notify::new());
    let reached = Arc::clone(&entered);
    // The response waits for the abort, which the faux stream honors.
    h.faux
        .set_responses(vec![FauxResponseStep::Factory(Arc::new(
            move |_, options, _, _| {
                let signal = options.base.signal.clone();
                let reached = Arc::clone(&reached);
                Box::pin(async move {
                    reached.notify_one();
                    signal
                        .expect("provider cancellation signal")
                        .cancelled()
                        .await;
                    Ok(faux_assistant_message("never shown"))
                })
            },
        ))]);
    let manager = SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
        .expect("session");
    let session = Arc::new(
        AgentSession::create(options(&h, manager, "/tmp"))
            .await
            .expect("session"),
    );
    let settled = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&settled);
    let _subscription = session.subscribe(move |event| {
        if let SessionEvent::AgentSettled { aborted, .. } = event {
            log.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(*aborted);
        }
    });
    let run = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.prompt("hang", Vec::new()).await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .expect("provider request started");
    tokio::time::timeout(std::time::Duration::from_secs(5), session.abort())
        .await
        .expect("abort settles");
    let result = run.await.expect("joined");
    assert_eq!(result, Ok(()));
    let last = session
        .messages()
        .last()
        .and_then(|m| m.as_assistant().cloned());
    assert_eq!(last.map(|m| m.stop_reason), Some(StopReason::Aborted));
    assert_eq!(
        *settled.lock().unwrap_or_else(PoisonError::into_inner),
        [true]
    );
    session.shutdown().await;
    assert!(session.prompt("after", Vec::new()).await.is_err());
}

// Pi `_rebuildSystemPrompt`: each tool definition's `promptGuidelines`,
// normalized, reach the prompt for the active tools; Bake's prompt lists no
// `promptSnippet`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_prompts_reach_the_system_prompt() {
    let h = harness();
    h.faux
        .set_responses(vec![faux_assistant_message("ok").into()]);
    let tool = AgentTool::new(
        "dummy",
        "dummy",
        "Dummy tool",
        json!({ "type": "object" }),
        |_, _, _, _| Box::pin(async { Ok(AgentToolResult::default()) }),
    );
    let manager = SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
        .expect("session");
    let session = AgentSession::create(AgentSessionOptions {
        tools: vec![Arc::new(tool)],
        active_tool_names: vec!["dummy".into()],
        tool_prompts: vec![
            ToolPrompt {
                name: "dummy".into(),
                snippet: Some("  Run the\n dummy   tool ".into()),
                guidelines: vec![" Use dummy sparingly ".into(), "Use dummy sparingly".into()],
            },
            ToolPrompt {
                name: "inactive".into(),
                snippet: Some("Never listed".into()),
                guidelines: vec!["Never said".into()],
            },
        ],
        ..options(&h, manager, "/tmp")
    })
    .await
    .expect("session");
    session.prompt("hi", Vec::new()).await.expect("prompt");
    let head = session.messages()[0].as_system().cloned().expect("system");
    let text = get_system_message_text(&head);
    assert!(!text.contains("Run the"), "{text}");
    assert_eq!(text.matches("- Use dummy sparingly\n").count(), 1, "{text}");
    assert!(!text.contains("Never"), "{text}");
    session.shutdown().await;
}

// A failed session write ends the run, as Pi's throwing listener does,
// and nothing after it is written, so the file has no gap. The tool
// replaces the session file with a directory, so the tool result's write
// fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_write_aborts_the_run() {
    let dir = TempDir::new("persist-fail");
    let cwd = dir.path().to_string_lossy().into_owned();
    let h = harness();
    h.faux.set_responses(vec![
        faux_assistant_blocks(
            vec![faux_tool_call("dummy", json!({}), Some("toolu_1"))],
            StopReason::ToolUse,
        )
        .into(),
        faux_assistant_message("never requested").into(),
    ]);
    let control = RunControl::default();
    let cancel = control.clone();
    let manager = new_file_session(&dir);
    let file = manager.session_file().expect("file path").to_path_buf();
    let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&runs);
    // The tool's own I/O errors, reported by the assertions below.
    let blocked: Arc<Mutex<Vec<String>>> = Arc::default();
    let tool_errors = Arc::clone(&blocked);
    let tool = AgentTool::new(
        "dummy",
        "dummy",
        "Dummy tool",
        json!({ "type": "object" }),
        move |_, _, _, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            let file = file.clone();
            let cancel = cancel.clone();
            let tool_errors = Arc::clone(&tool_errors);
            Box::pin(async move {
                let outcome = std::fs::remove_file(&file)
                    .map_err(|error| format!("remove {}: {error}", file.display()))
                    .and_then(|()| {
                        std::fs::create_dir_all(&file)
                            .map_err(|error| format!("block {}: {error}", file.display()))
                    });
                if let Err(error) = outcome {
                    tool_errors
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(error);
                }
                cancel.cancel();
                Ok(AgentToolResult::default())
            })
        },
    );
    let session = AgentSession::create(AgentSessionOptions {
        tools: vec![Arc::new(tool)],
        active_tool_names: vec!["dummy".into()],
        ..options(&h, manager, &cwd)
    })
    .await
    .expect("session");
    let completed_results = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = Arc::clone(&completed_results);
    session.subscribe(move |event| {
        if let SessionEvent::Agent(AgentEvent::MessageEnd { message }) = event
            && message.as_tool_result().is_some()
        {
            observed.fetch_add(1, Ordering::SeqCst);
        }
    });
    let settled = Arc::new(Mutex::new(None));
    let observed = Arc::clone(&settled);
    let _settled = session.subscribe(move |event| {
        if let SessionEvent::AgentSettled { outcome, .. } = event {
            *observed.lock().unwrap() = Some((*outcome).clone());
        }
    });
    let result = session
        .prompt_with_control("run", Vec::new(), control.clone())
        .await
        .unwrap();
    assert!(matches!(result, RunOutcome::Failed { .. }), "{result:?}");
    assert_eq!(control.stopped(), Some(result.clone()));
    assert_eq!(*settled.lock().unwrap(), Some(result));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(completed_results.load(Ordering::SeqCst), 0);
    let tool_errors = blocked
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert_eq!(
        tool_errors,
        Vec::<String>::new(),
        "the tool blocks the file"
    );
    assert_eq!(h.faux.call_count(), 1, "the run stops after the failure");
    // Cancellation creates no synthetic model response. The write failure
    // determines settlement, and nothing after the missing result is written.
    assert!(
        session
            .messages()
            .last()
            .is_some_and(|message| message.as_tool_result().is_some())
    );
    let mut written: Vec<String> = std::fs::read_dir(dir.join("sessions"))
        .expect("sessions dir")
        .map(|entry| {
            let name = entry.expect("entry").file_name();
            name.to_string_lossy().into_owned()
        })
        .collect();
    written.sort();
    assert_eq!(written.len(), 2, "{written:?}");
    assert_eq!(format!("{}.lock", written[0]), written[1]);
    // Bake: the file now misses a message, so later prompts and steers are
    // refused instead of writing after the gap.
    for refused in [
        session.prompt("again", Vec::new()).await,
        session.steer("again", Vec::new()).await,
    ] {
        let error = refused.expect_err("refused after a failed write");
        assert!(
            error.starts_with("The session file could not be written"),
            "{error}"
        );
    }
    assert_eq!(h.faux.call_count(), 1);
    session.shutdown().await;
}

// Shutdown while a request authenticates through a slow `!command` ends
// the request at once and leaves no request task behind.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_ends_a_request_waiting_for_auth() {
    let h = harness();
    let registry = Arc::new(ModelRegistry::new(
        ModelConfig::from_value(
            json!({
                "providers": {
                    "slow": {
                        "baseUrl": "http://127.0.0.1:9/unused",
                        "api": h.faux.model().api,
                        "apiKey": "!sleep 3; echo slow-key",
                        "models": [{ "id": "slow-1" }],
                    },
                },
            }),
            "models.json",
        ),
        AuthStorage::empty(),
    ));
    let manager = SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
        .expect("session");
    let session = Arc::new(
        AgentSession::create(AgentSessionOptions {
            model: registry.model("slow", "slow-1"),
            registry: registry.into(),
            ..options(&h, manager, "/tmp")
        })
        .await
        .expect("session"),
    );
    let run = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.prompt("hang", Vec::new()).await })
    };
    while !session.agent().is_streaming() {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let started = std::time::Instant::now();
    tokio::time::timeout(std::time::Duration::from_secs(2), session.shutdown())
        .await
        .expect("shutdown does not wait for the command");
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert!(session.work.is_closed(), "every request task is gone");
    let _ = run.await;
    assert_eq!(h.faux.call_count(), 0);
}

#[test]
fn block_images_replaces_runs_of_images() {
    let image = || {
        UserContentBlock::Image(ImageContent {
            data: "abc".into(),
            mime_type: "image/png".into(),
        })
    };
    let messages = block_images(vec![Message::User(UserMessage {
        content: UserContent::Blocks(vec![
            UserContentBlock::Text(TextContent::new("look")),
            image(),
            image(),
        ]),
        timestamp: 0,
    })]);
    let Message::User(user) = &messages[0] else {
        panic!("user message");
    };
    assert_eq!(
        user.content,
        UserContent::Blocks(vec![
            UserContentBlock::Text(TextContent::new("look")),
            UserContentBlock::Text(TextContent::new(IMAGE_READING_DISABLED)),
        ])
    );
}

// The prompt's round-trip rule says calls in one response run in order, so
// the tools that change files or run commands are sequential; reads and
// tools that set their own mode keep it.
#[test]
fn tools_that_change_files_or_run_commands_run_in_order() {
    let tool = |name: &str, mode: Option<ToolExecutionMode>| {
        let mut tool = AgentTool::new(name, name, "", json!({ "type": "object" }), |_, _, _, _| {
            Box::pin(async { Ok(AgentToolResult::default()) })
        });
        tool.execution_mode = mode;
        Arc::new(tool)
    };
    for name in ["bash", "edit", "write"] {
        assert_eq!(
            run_in_order(tool(name, None)).execution_mode,
            Some(ToolExecutionMode::Sequential),
            "{name}"
        );
    }
    for name in ["read", "grep", "find", "ls"] {
        assert_eq!(
            run_in_order(tool(name, None)).execution_mode,
            None,
            "{name}"
        );
    }
    assert_eq!(
        run_in_order(tool("bash", Some(ToolExecutionMode::Parallel))).execution_mode,
        Some(ToolExecutionMode::Parallel)
    );
}

// Bake `packages/llm/llm-pi-ai/src/config.ts` `maxToolArgumentWhitespace`:
// the setting fills each request's bound unless the request sets one.
#[test]
fn the_tool_argument_whitespace_setting_reaches_each_request() {
    let h = harness();
    let model = h.registry.model("faux", "faux-1").expect("faux model");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let inner: bake_agent::StreamFn = {
        let seen = Arc::clone(&seen);
        Arc::new(move |_, _, options| {
            seen.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(options.base.max_tool_argument_whitespace);
            Ok(bake_ai::assistant_message_channel().1)
        })
    };
    let context = bake_ai::TranscriptContext::default();
    let unset = with_tool_argument_whitespace(Arc::clone(&inner), None);
    let set = with_tool_argument_whitespace(inner, Some(64));
    let _ = unset(&model, &context, bake_ai::SimpleStreamOptions::default());
    let _ = set(&model, &context, bake_ai::SimpleStreamOptions::default());
    let mut explicit = bake_ai::SimpleStreamOptions::default();
    explicit.base.max_tool_argument_whitespace = Some(7);
    let _ = set(&model, &context, explicit);
    assert_eq!(
        *seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        [None, Some(64), Some(7)]
    );
}

// Pi v1.2.0 test/suite/agent-session-queue.test.ts: multiple steering and
// follow-up messages in one-at-a-time/all mode, and queue removal before
// message_start. A held tool makes queue admission deterministic.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pi_queue_modes_and_pending_message_start() {
    use bake_agent::{AgentEvent, AgentTool, QueueMode};
    use bake_ai::utils::text::{assistant_text, user_content_text};

    for mode in [QueueMode::OneAtATime, QueueMode::All] {
        for steer in [true, false] {
            let dir = TempDir::new("pi-queue-modes");
            let cwd = dir.path().to_string_lossy().into_owned();
            let h = harness();
            let started = Arc::new(Notify::new());
            let release = Arc::new(Notify::new());
            let tool = {
                let started = Arc::clone(&started);
                let release = Arc::clone(&release);
                Arc::new(AgentTool::new(
                    "wait",
                    "Wait",
                    "Wait for release",
                    json!({"type": "object", "properties": {}}),
                    move |_, _, signal, _| {
                        let started = Arc::clone(&started);
                        let release = Arc::clone(&release);
                        Box::pin(async move {
                            started.notify_one();
                            if let Some(signal) = signal {
                                tokio::select! {
                                    () = release.notified() => {},
                                    () = signal.cancelled() => return Err("cancelled".into()),
                                }
                            } else {
                                release.notified().await;
                            }
                            Ok(AgentToolResult::text("released"))
                        })
                    },
                ))
            };
            let mut responses = vec![
                faux_assistant_blocks(
                    vec![faux_tool_call("wait", json!({}), Some("wait-1"))],
                    StopReason::ToolUse,
                )
                .into(),
            ];
            let mut expected = vec!["user:start", "assistant:"];
            if !steer {
                responses.push(faux_assistant_message("original complete").into());
                expected.push("assistant:original complete");
            }
            if mode == QueueMode::All {
                responses.push(faux_assistant_message("batch").into());
                expected.extend(["user:one", "user:two", "assistant:batch"]);
            } else {
                responses.extend([
                    faux_assistant_message("handled one").into(),
                    faux_assistant_message("handled two").into(),
                ]);
                expected.extend([
                    "user:one",
                    "assistant:handled one",
                    "user:two",
                    "assistant:handled two",
                ]);
            }
            h.faux.set_responses(responses);
            let mut opts = options(&h, new_file_session(&dir), &cwd);
            opts.settings.steering_mode = mode;
            opts.settings.follow_up_mode = mode;
            opts.tools = vec![tool];
            opts.active_tool_names = vec!["wait".into()];
            let session = Arc::new(AgentSession::create(opts).await.expect("session"));
            let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
            let subscription = {
                let weak = Arc::downgrade(&session);
                let seen = Arc::clone(&seen);
                session.subscribe(move |event| {
                    if let SessionEvent::Agent(AgentEvent::MessageStart { message }) = event
                        && let Some(user) = message.as_user()
                        && ["one", "two"].contains(&user_content_text(&user.content, "").as_str())
                        && let Some(session) = weak.upgrade()
                    {
                        seen.lock().unwrap().push(session.queued_messages());
                    }
                })
            };
            let run = async {
                let (result, ()) = tokio::join!(session.prompt("start", Vec::new()), async {
                    started.notified().await;
                    for text in ["one", "two"] {
                        if steer {
                            session.steer(text, Vec::new()).await.expect("steer");
                        } else {
                            session
                                .follow_up(text, Vec::new())
                                .await
                                .expect("follow up");
                        }
                    }
                    release.notify_one();
                });
                result
            };
            let result = tokio::time::timeout(std::time::Duration::from_secs(10), run).await;
            session.shutdown().await;
            result.expect("queue run settled").expect("prompt");
            subscription.unsubscribe();
            let actual: Vec<String> = session
                .messages()
                .iter()
                .filter_map(|message| {
                    if let Some(user) = message.as_user() {
                        Some(format!("user:{}", user_content_text(&user.content, "")))
                    } else {
                        message
                            .as_assistant()
                            .map(|assistant| format!("assistant:{}", assistant_text(assistant)))
                    }
                })
                .collect();
            assert_eq!(actual, expected, "mode={mode:?}, steer={steer}");
            let remaining: Vec<Vec<String>> = seen
                .lock()
                .unwrap()
                .iter()
                .map(|queued| {
                    if steer {
                        queued.steering.clone()
                    } else {
                        queued.follow_up.clone()
                    }
                })
                .collect();
            assert_eq!(
                remaining,
                [vec!["two".to_owned()], vec![]],
                "mode={mode:?}, steer={steer}"
            );
            assert!(session.queued_messages().is_empty());
        }
    }
}
