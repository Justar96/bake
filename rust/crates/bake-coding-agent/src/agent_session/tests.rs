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

use super::*;
use crate::auth_storage::AuthStorage;
use crate::model_registry::config::ModelConfig;
use crate::session::NewSessionOptions;
use crate::system_prompt::{DocsPaths, build_system_prompt};
use crate::test_support::TempDir;

struct Harness {
    faux: Arc<FauxProvider>,
    registry: Arc<ModelRegistry>,
    apis: Arc<ApiRegistry>,
}

fn harness() -> Harness {
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

fn docs() -> DocsPaths {
    DocsPaths {
        readme: "/opt/pi/README.md".into(),
        docs: "/opt/pi/docs".into(),
        examples: "/opt/pi/examples".into(),
    }
}

fn options(harness: &Harness, session: SessionManager, cwd: &str) -> AgentSessionOptions {
    AgentSessionOptions {
        session,
        registry: Arc::clone(&harness.registry),
        apis: Arc::clone(&harness.apis),
        settings: Settings::default(),
        model: harness.registry.model("faux", "faux-1"),
        thinking_level: None,
        tools: Vec::new(),
        active_tool_names: Vec::new(),
        tool_prompts: Vec::new(),
        system_prompt: SystemPromptOptions::new(cwd, docs()),
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

fn new_file_session(dir: &TempDir) -> SessionManager {
    SessionManager::create(
        &dir.path().to_string_lossy(),
        Some(&dir.join("sessions")),
        NewSessionOptions::default(),
    )
    .expect("session")
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
    assert_eq!(names, ["preamble", "tools", "rules", "docs", "cwd"]);
    let mut expected = SystemPromptOptions::new(cwd.clone(), docs());
    expected.selected_tools = Vec::new();
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
        .set_thinking_level(ModelThinkingLevel::High)
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
        .set_thinking_level(ModelThinkingLevel::High)
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
        registry: Arc::clone(&empty),
        model: None,
        ..options(&h, manager, "/tmp")
    })
    .await
    .expect("session");
    assert!(session.model().is_none());
    assert_eq!(
        session.model_fallback_message(),
        Some(format_no_models_available_message("/opt/pi/docs").as_str())
    );
    assert_eq!(
        session.prompt("hi", Vec::new()).await,
        Err(format_no_model_selected_message("/opt/pi/docs"))
    );
    session.shutdown().await;

    let manager = SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
        .expect("session");
    let session = AgentSession::create(AgentSessionOptions {
        registry: empty,
        ..options(&h, manager, "/tmp")
    })
    .await
    .expect("session");
    assert_eq!(
        session.prompt("hi", Vec::new()).await,
        // Pi joins the docs paths with the platform separator.
        Err(format!(
            "No API key found for faux.\n\nUse /login to log into a provider via OAuth or API key. See:\n  {}\n  {}",
            std::path::Path::new("/opt/pi/docs")
                .join("providers.md")
                .display(),
            std::path::Path::new("/opt/pi/docs")
                .join("models.md")
                .display()
        ))
    );
    assert_eq!(
        session
            .set_model(h.registry.model("faux", "faux-1").expect("model"))
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
    // The response waits for the abort, which the faux stream honors.
    h.faux
        .set_responses(vec![FauxResponseStep::Factory(Arc::new(
            |_, options, _, _| {
                let signal = options.base.signal.clone();
                Box::pin(async move {
                    while !signal.as_ref().is_some_and(bake_ai::AbortSignal::aborted) {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
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
        if let SessionEvent::AgentSettled { aborted } = event {
            log.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(*aborted);
        }
    });
    let run = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.prompt("hang", Vec::new()).await })
    };
    while !session.agent().is_streaming() {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
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

// Pi `_rebuildSystemPrompt`: each tool definition's `promptSnippet` and
// `promptGuidelines`, normalized, reach the prompt for the active tools.
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
    assert!(
        text.contains("<tools>\n- dummy: Run the dummy tool\n"),
        "{text}"
    );
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
    let manager = new_file_session(&dir);
    let file = manager.session_file().expect("file path").to_path_buf();
    let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&runs);
    let tool = AgentTool::new(
        "dummy",
        "dummy",
        "Dummy tool",
        json!({ "type": "object" }),
        move |_, _, _, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            let file = file.clone();
            Box::pin(async move {
                std::fs::remove_file(&file).expect("the file was written");
                std::fs::create_dir_all(&file).expect("block the file");
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
    let result = session.prompt("run", Vec::new()).await;
    assert!(result.is_err(), "{result:?}");
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(h.faux.call_count(), 1, "the run stops after the failure");
    // The run ends as aborted after the tool result, and nothing more is
    // written: the blocked path is all the session directory holds.
    let last = session
        .messages()
        .last()
        .and_then(|m| m.as_assistant().cloned());
    assert_eq!(last.map(|m| m.stop_reason), Some(StopReason::Aborted));
    let written = std::fs::read_dir(dir.join("sessions"))
        .expect("sessions dir")
        .count();
    assert_eq!(written, 1);
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
            registry,
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
