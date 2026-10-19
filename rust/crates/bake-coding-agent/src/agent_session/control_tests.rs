//! Model, thinking-level, and settings changes: Pi v1.2.0
//! `agent-session.ts` `setModel`, `setThinkingLevel`, `setSteeringMode`,
//! `setFollowUpMode`, and `_installAgentNextTurnRefresh` (revision
//! `06a466935536f8c9bcacb2d5efc0fc021be1bdff`), with Bake's serialization,
//! log-first writes, and shutdown ownership. Each test owns its session,
//! temporary home, and faux provider; waits are on barriers, notifications,
//! and observed state, never on time.

use std::sync::Arc;

use bake_agent::AgentToolResult;
use bake_ai::providers::faux::{
    FauxProvider, FauxProviderOptions, FauxResponseStep, faux_assistant_blocks,
    faux_assistant_message, faux_tool_call,
};
use bake_ai::{ApiRegistry, StopReason};
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::*;
use crate::auth_storage::AuthStorage;
use crate::model_registry::config::ModelConfig;
use crate::session::NewSessionOptions;
use crate::test_support::TempDir;

pub(super) struct Harness {
    pub(super) faux: Arc<FauxProvider>,
    runtime: ModelRuntime,
    apis: Arc<ApiRegistry>,
}

pub(super) fn harness() -> Harness {
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
                    { "id": "plain" },
                ],
            }),
            Some("test-key".into()),
        )
        .expect("faux provider");
    Harness {
        faux,
        runtime: ModelRuntime::new(registry),
        apis,
    }
}

impl Harness {
    pub(super) fn model(&self, id: &str) -> Model {
        self.runtime.snapshot().model("faux", id).expect("model")
    }

    pub(super) async fn session(
        &self,
        session: SessionManager,
        settings: Settings,
        store: Option<SettingsStore>,
        tools: Vec<Arc<AgentTool>>,
    ) -> AgentSession {
        let active = tools.iter().map(|tool| tool.name.clone()).collect();
        AgentSession::create(AgentSessionOptions {
            session,
            registry: self.runtime.clone(),
            apis: Arc::clone(&self.apis),
            settings,
            settings_store: store,
            selection: None,
            model: Some(self.model("faux-1")),
            thinking_level: Some(ModelThinkingLevel::High),
            tools,
            active_tool_names: active,
            tool_prompts: Vec::new(),
            system_prompt: crate::system_prompt::SystemPromptOptions::new("/tmp"),
            docs_command: "bake-rs docs".into(),
            plugins: crate::plugins::PluginRegistry::default(),
            work: None,
            jobs: None,
        })
        .await
        .expect("session")
    }
}

pub(super) fn in_memory() -> SessionManager {
    SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
        .expect("session")
}

/// The model and thinking-level entries, in log order.
fn changes(session: &AgentSession) -> Vec<String> {
    lock(&session.session)
        .entries()
        .iter()
        .filter_map(|entry| {
            let json = entry.as_json();
            match entry.entry_type() {
                "model_change" => Some(format!("model {}", json["modelId"].as_str()?)),
                "thinking_level_change" => {
                    Some(format!("thinking {}", json["thinkingLevel"].as_str()?))
                }
                _ => None,
            }
        })
        .collect()
}

fn settings_json(home: &TempDir) -> Value {
    serde_json::from_str(&std::fs::read_to_string(home.join("settings.json")).expect("settings"))
        .expect("json")
}

// Pi "setModel persists only with persist" and "per-model thinking level
// wins on a switch": the session records the change and the level the
// model's settings choose; the default is saved only when asked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changes_follow_pi_and_save_defaults_only_when_asked() {
    let home = TempDir::new("control-defaults");
    let h = harness();
    let settings = Settings::from_object(
        json!({ "modelThinkingLevels": { "faux/faux-2": "low" } })
            .as_object()
            .expect("object"),
    );
    let session = h
        .session(
            in_memory(),
            settings,
            Some(SettingsStore::new(home.path())),
            Vec::new(),
        )
        .await;
    assert_eq!(
        session.set_model(h.model("faux-2"), false).await,
        Ok(DefaultSave::NotRequested)
    );
    assert_eq!(session.thinking_level(), ModelThinkingLevel::Low);
    assert!(
        !home.join("settings.json").exists(),
        "saved without persist"
    );
    assert_eq!(
        session.selection().get(),
        Selected {
            model: Some(h.model("faux-2")),
            thinking_level: ModelThinkingLevel::Low,
        }
    );
    // A model without reasoning clamps the level to off.
    assert_eq!(
        session.set_model(h.model("plain"), true).await,
        Ok(DefaultSave::Saved)
    );
    assert_eq!(session.thinking_level(), ModelThinkingLevel::Off);
    assert_eq!(settings_json(&home)["defaultModel"], "plain");
    assert_eq!(settings_json(&home)["defaultProvider"], "faux");
    assert_eq!(
        session.available_thinking_levels(),
        [ModelThinkingLevel::Off]
    );
    session
        .set_model(h.model("faux-1"), false)
        .await
        .expect("faux-1");
    // Without a per-model level, the current one (off) carries over.
    assert_eq!(session.thinking_level(), ModelThinkingLevel::Off);
    assert_eq!(
        session
            .set_thinking_level(ModelThinkingLevel::Medium, true)
            .await,
        Ok(DefaultSave::Saved)
    );
    assert_eq!(settings_json(&home)["defaultThinkingLevel"], "medium");
    assert_eq!(
        session.settings().default_thinking_level,
        Some(ModelThinkingLevel::Medium)
    );
    // The saved default now chooses the level of the next switch.
    session
        .set_model(h.model("faux-2"), false)
        .await
        .expect("faux-2");
    assert_eq!(session.thinking_level(), ModelThinkingLevel::Low);
    // The same level again records nothing.
    let before = changes(&session).len();
    session
        .set_thinking_level(ModelThinkingLevel::Low, false)
        .await
        .expect("same");
    assert_eq!(changes(&session).len(), before);
    assert_eq!(
        changes(&session),
        [
            "model faux-1",
            "thinking high",
            "model faux-2",
            "thinking low",
            "model plain",
            "thinking off",
            "model faux-1",
            "thinking medium",
            "model faux-2",
            "thinking low",
        ]
    );
    // Queue modes and auto-compaction apply now and are saved.
    assert_eq!(
        session.set_steering_mode(QueueMode::All).await,
        Ok(DefaultSave::Saved)
    );
    assert_eq!(session.steering_mode(), QueueMode::All);
    session
        .set_follow_up_mode(QueueMode::All)
        .await
        .expect("follow-up");
    assert_eq!(session.follow_up_mode(), QueueMode::All);
    session
        .set_auto_compaction_enabled(false)
        .await
        .expect("compaction");
    assert!(!session.auto_compaction_enabled());
    let saved = settings_json(&home);
    assert_eq!(saved["steeringMode"], "all");
    assert_eq!(saved["followUpMode"], "all");
    assert_eq!(saved["compaction"]["enabled"], false);
    session.shutdown().await;
}

// Bake: a default that cannot be saved leaves the session changed, and is
// reported apart from a session failure; prompts still run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_default_save_leaves_the_session_changed() {
    let home = TempDir::new("control-save-fails");
    // A directory where settings.json should be: every write is refused.
    std::fs::create_dir_all(home.join("settings.json")).expect("dir");
    let h = harness();
    h.faux
        .set_responses(vec![faux_assistant_message("still running").into()]);
    let session = h
        .session(
            in_memory(),
            Settings::default(),
            Some(SettingsStore::new(home.path())),
            Vec::new(),
        )
        .await;
    let saved = session
        .set_model(h.model("faux-2"), true)
        .await
        .expect("session change");
    assert!(matches!(saved, DefaultSave::Failed(_)), "{saved:?}");
    assert_eq!(session.model(), Some(h.model("faux-2")));
    session.prompt("hi", Vec::new()).await.expect("prompt");
    session.shutdown().await;
}

// Bake: the log is written first. A change whose session write fails is
// not applied; the session keeps the model its file records and takes no
// more prompts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_session_write_changes_nothing_and_stops_prompts() {
    let dir = TempDir::new("control-write-fails");
    let h = harness();
    h.faux
        .set_responses(vec![faux_assistant_message("first").into()]);
    let session = h
        .session(file_session(&dir), Settings::default(), None, Vec::new())
        .await;
    session.prompt("one", Vec::new()).await.expect("one");
    let file = session.session_file().expect("file");
    std::fs::remove_file(&file).expect("remove");
    std::fs::create_dir_all(&file).expect("block");
    let error = session
        .set_model(h.model("faux-2"), false)
        .await
        .expect_err("write fails");
    assert!(error.contains("takes no more prompts"), "{error}");
    assert_eq!(session.model(), Some(h.model("faux-1")));
    assert_eq!(session.selection().get().model, Some(h.model("faux-1")));
    let refused = session
        .prompt("two", Vec::new())
        .await
        .expect_err("refused");
    assert!(refused.contains("could not be written"), "{refused}");
    session.shutdown().await;
}

pub(super) fn file_session(dir: &TempDir) -> SessionManager {
    SessionManager::create(
        &dir.path().to_string_lossy(),
        Some(&dir.join("sessions")),
        NewSessionOptions::default(),
    )
    .expect("session")
}

// Bake: a change whose write fails while a response streams stops that
// run: the held response's tool call never runs, no further request is
// made, and nothing is written past the gap, although the file is
// writable again by then.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_change_write_stops_the_running_turn() {
    let dir = TempDir::new("control-write-fails-mid-run");
    let h = harness();
    h.faux
        .set_responses(vec![faux_assistant_message("first").into()]);
    let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tool = {
        let ran = Arc::clone(&ran);
        Arc::new(AgentTool::new(
            "noop",
            "noop",
            "Counts its calls",
            json!({ "type": "object" }),
            move |_, _, _, _, _ctx| {
                ran.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(AgentToolResult::default()) })
            },
        ))
    };
    let session = Arc::new(
        h.session(file_session(&dir), Settings::default(), None, vec![tool])
            .await,
    );
    session
        .prompt("one", Vec::new())
        .await
        .expect("flush the file");
    let file = session.session_file().expect("file");
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    {
        let (started, release) = (started.clone(), release.clone());
        h.faux.set_responses(vec![
            FauxResponseStep::Factory(Arc::new(move |_, _, _, _| {
                let (started, release) = (started.clone(), release.clone());
                Box::pin(async move {
                    started.notify_one();
                    release.notified().await;
                    Ok(faux_assistant_blocks(
                        vec![faux_tool_call("noop", json!({}), Some("call-1"))],
                        StopReason::ToolUse,
                    ))
                })
            })),
            faux_assistant_message("never requested").into(),
        ]);
    }
    let run = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.prompt("two", Vec::new()).await })
    };
    started.notified().await;
    let calls_before = h.faux.call_count();
    // The change's append fails; the file comes back writable afterwards.
    let written = std::fs::read(&file).expect("read");
    std::fs::remove_file(&file).expect("remove");
    std::fs::create_dir_all(&file).expect("block");
    let error = session
        .set_model(h.model("faux-2"), false)
        .await
        .expect_err("write fails");
    assert!(error.contains("stopped"), "{error}");
    std::fs::remove_dir(&file).expect("unblock");
    std::fs::write(&file, &written).expect("restore");
    release.notify_one();
    let _ = run.await.expect("join");
    assert_eq!(
        ran.load(Ordering::SeqCst),
        0,
        "the tool ran after the failure"
    );
    assert_eq!(
        h.faux.call_count(),
        calls_before,
        "a request followed the failure"
    );
    assert_eq!(
        std::fs::read(&file).expect("read"),
        written,
        "written past the gap"
    );
    assert_eq!(session.model(), Some(h.model("faux-1")));
    session.shutdown().await;
}

// Bake: a default written whose directory flush failed is reported as
// written but not durable, not as a plain success.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unsynced_save_is_reported_as_such() {
    let home = TempDir::new("control-unsynced");
    let h = harness();
    let store = SettingsStore::new(home.path()).with_fault(crate::json_store::Fault::DirectorySync);
    let session = h
        .session(in_memory(), Settings::default(), Some(store), Vec::new())
        .await;
    let saved = session
        .set_model(h.model("faux-2"), true)
        .await
        .expect("change");
    assert!(matches!(saved, DefaultSave::Unsynced(_)), "{saved:?}");
    assert!(saved.is_written());
    assert_eq!(settings_json(&home)["defaultModel"], "faux-2");
    session.shutdown().await;
}

// Bake: a caller that stops waiting does not abandon its change. The
// change finishes on its own task, holding its turn, so a later change
// lands after it in the log, and shutdown waits for its save.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_caller_does_not_abandon_its_change() {
    let home = TempDir::new("control-dropped");
    let h = harness();
    let session = Arc::new(
        h.session(
            in_memory(),
            Settings::default(),
            Some(SettingsStore::new(home.path())),
            Vec::new(),
        )
        .await,
    );
    let held = crate::json_store::JsonFileStore::new(home.join("settings.json"), 1 << 20)
        .lock()
        .expect("hold the settings lock");
    let caller = {
        let (session, model) = (Arc::clone(&session), h.model("faux-2"));
        tokio::spawn(async move { session.set_model(model, true).await })
    };
    while session.model() != Some(h.model("faux-2")) {
        tokio::task::yield_now().await;
    }
    caller.abort();
    let _ = caller.await;
    let later = {
        let (session, model) = (Arc::clone(&session), h.model("plain"));
        tokio::spawn(async move { session.set_model(model, false).await })
    };
    drop(held);
    later.await.expect("join").expect("later change");
    session.shutdown().await;
    assert_eq!(settings_json(&home)["defaultModel"], "faux-2");
    let models: Vec<String> = changes(&session)
        .into_iter()
        .filter(|change| change.starts_with("model "))
        .collect();
    assert_eq!(models, ["model faux-1", "model faux-2", "model plain"]);
    assert_eq!(session.model(), Some(h.model("plain")));
}

/// A tool whose result is empty.
fn noop_tool() -> Arc<AgentTool> {
    Arc::new(AgentTool::new(
        "noop",
        "noop",
        "Does nothing",
        json!({ "type": "object" }),
        |_, _, _, _, _ctx| Box::pin(async { Ok(AgentToolResult::default()) }),
    ))
}

// Pi `_installAgentNextTurnRefresh`: a change made while a response streams
// leaves that response's model and level alone and reaches the run's next
// request, after the tool call, with the model and level of one change.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_change_during_a_streaming_response_reaches_the_next_request() {
    let h = harness();
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    type Seen = Vec<(String, Option<bake_ai::ThinkingLevel>)>;
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let record =
        |seen: &Arc<Mutex<Vec<_>>>, model: &Model, options: &bake_ai::SimpleStreamOptions| {
            seen.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((model.id.clone(), options.reasoning));
        };
    {
        let (started, release, seen) = (started.clone(), release.clone(), seen.clone());
        let seen_after = seen.clone();
        h.faux.set_responses(vec![
            FauxResponseStep::Factory(Arc::new(move |_, options, _, model| {
                record(&seen, model, options);
                let (started, release) = (started.clone(), release.clone());
                Box::pin(async move {
                    started.notify_one();
                    release.notified().await;
                    Ok(faux_assistant_blocks(
                        vec![faux_tool_call("noop", json!({}), Some("call-1"))],
                        StopReason::ToolUse,
                    ))
                })
            })),
            FauxResponseStep::from_fn(move |_, options, _, model| {
                record(&seen_after, model, options);
                Ok(faux_assistant_message("done"))
            }),
        ]);
    }
    let session = Arc::new(
        h.session(in_memory(), Settings::default(), None, vec![noop_tool()])
            .await,
    );
    let run = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.prompt("go", Vec::new()).await })
    };
    started.notified().await;
    session
        .set_model(h.model("faux-2"), false)
        .await
        .expect("switch");
    session
        .set_thinking_level(ModelThinkingLevel::Low, false)
        .await
        .expect("level");
    release.notify_one();
    run.await.expect("join").expect("run");
    assert_eq!(
        *seen.lock().unwrap_or_else(PoisonError::into_inner),
        [
            ("faux-1".to_owned(), Some(bake_ai::ThinkingLevel::High)),
            ("faux-2".to_owned(), Some(bake_ai::ThinkingLevel::Low)),
        ]
    );
    // Each response records the model that wrote it.
    let models: Vec<String> = session
        .messages()
        .iter()
        .filter_map(|message| message.as_assistant().map(|a| a.model.clone()))
        .collect();
    assert_eq!(models, ["faux-1", "faux-2"]);
    session.shutdown().await;
}

// Bake: concurrent changes run one at a time, so the last change in the log
// is the model the session runs with.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_changes_keep_log_order_and_state_together() {
    let h = harness();
    for _ in 0..20 {
        let session = Arc::new(
            h.session(in_memory(), Settings::default(), None, Vec::new())
                .await,
        );
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let tasks: Vec<_> = ["faux-2", "plain"]
            .into_iter()
            .map(|id| {
                let (session, barrier, model) =
                    (Arc::clone(&session), Arc::clone(&barrier), h.model(id));
                tokio::spawn(async move {
                    barrier.wait().await;
                    session.set_model(model, false).await
                })
            })
            .collect();
        for task in tasks {
            task.await.expect("join").expect("change");
        }
        let last_model = changes(&session)
            .into_iter()
            .rev()
            .find_map(|change| change.strip_prefix("model ").map(str::to_owned));
        assert_eq!(last_model, session.model().map(|model| model.id));
        assert_eq!(session.selection().get().model, session.model());
        session.shutdown().await;
    }
}

// Bake: shutdown awaits a change in flight, here one blocked saving its
// default behind another holder of the settings lock, and refuses later
// ones.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_awaits_a_change_in_flight() {
    let home = TempDir::new("control-shutdown");
    let h = harness();
    let session = Arc::new(
        h.session(
            in_memory(),
            Settings::default(),
            Some(SettingsStore::new(home.path())),
            Vec::new(),
        )
        .await,
    );
    let held = crate::json_store::JsonFileStore::new(home.join("settings.json"), 1 << 20)
        .lock()
        .expect("hold the settings lock");
    let change = {
        let (session, model) = (Arc::clone(&session), h.model("faux-2"));
        tokio::spawn(async move { session.set_model(model, true).await })
    };
    // The change has applied and is now saving behind the held lock.
    while session.model() != Some(h.model("faux-2")) {
        tokio::task::yield_now().await;
    }
    let shutdown = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.shutdown().await })
    };
    assert!(
        !change.is_finished(),
        "the save finished behind a held lock"
    );
    drop(held);
    shutdown.await.expect("shutdown");
    // Shutdown returned only after the change's save, so it is on disk.
    assert_eq!(settings_json(&home)["defaultModel"], "faux-2");
    assert_eq!(change.await.expect("join"), Ok(DefaultSave::Saved));
    assert_eq!(
        session.set_model(h.model("faux-1"), false).await,
        Err("The session is shut down.".into())
    );
}

/// The entries a reader finds in `file`, which the session holds.
fn file_entries(file: &std::path::Path) -> Vec<crate::session::SessionEntry> {
    crate::session::load_entries_from_file(file)
        .expect("load")
        .into_iter()
        .filter_map(|entry| entry.as_entry().cloned())
        .collect()
}

/// A file session that has written its first exchange.
async fn written_session(h: &Harness, dir: &TempDir) -> (AgentSession, std::path::PathBuf) {
    h.faux
        .set_responses(vec![faux_assistant_message("first").into()]);
    let session = h
        .session(file_session(dir), Settings::default(), None, Vec::new())
        .await;
    session.prompt("one", Vec::new()).await.expect("flush");
    let file = session.session_file().expect("file");
    (session, file)
}

// Bake: a model change is one group, the model entry then its level. When
// the level's line fails after the model's line is written, the session
// applies what the file holds, the new model with the previous level
// clamped to it, reports the torn line, stops, and writes nothing more.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_half_written_model_change_applies_what_the_file_holds() {
    use crate::session::{Fault, FaultKind};
    let dir = TempDir::new("control-half-written");
    let h = harness();
    let (session, file) = written_session(&h, &dir).await;
    let before = std::fs::read(&file).expect("read");
    lock(&session.session).arm_group_fault(Fault {
        line: 1,
        kind: FaultKind::Tear(12),
    });
    let error = session
        .set_model(h.model("plain"), false)
        .await
        .expect_err("partial");
    assert!(
        error.contains("was recorded, but its thinking level"),
        "{error}"
    );
    assert!(error.contains("1 of 2 grouped entries written"), "{error}");
    assert!(error.contains("may be partly written"), "{error}");
    assert_eq!(session.model(), Some(h.model("plain")));
    assert_eq!(session.thinking_level(), ModelThinkingLevel::Off);
    assert_eq!(
        session.selection().get(),
        Selected {
            model: Some(h.model("plain")),
            thinking_level: ModelThinkingLevel::Off,
        }
    );
    // The file: the model line, then a torn fragment of the level's.
    let after = std::fs::read(&file).expect("read");
    let added = String::from_utf8(after[before.len()..].to_vec()).expect("utf-8");
    let (model_line, torn) = added.split_once('\n').expect("model line");
    assert!(
        model_line.contains(r#""type":"model_change""#),
        "{model_line}"
    );
    assert_eq!(torn.len(), 12, "{torn}");
    // The session holds what a reader finds, and a reader projects the new
    // model with the level the file last recorded.
    let read = file_entries(&file);
    let ids = |entries: Vec<&crate::session::SessionEntry>| -> Vec<String> {
        entries.iter().map(|entry| entry.id().to_owned()).collect()
    };
    assert_eq!(
        ids(read.iter().collect()),
        ids(lock(&session.session).entries())
    );
    let context = crate::session::build_session_context(&read, crate::session::LeafSelector::Last);
    assert_eq!(
        context.model.map(|model| model.model_id),
        Some("plain".into())
    );
    assert_eq!(context.thinking_level, "high");
    assert_eq!(
        changes(&session),
        ["model faux-1", "thinking high", "model plain"]
    );
    // Nothing more is written: not another change, not a prompt.
    assert!(
        session
            .set_thinking_level(ModelThinkingLevel::Low, false)
            .await
            .is_err()
    );
    assert!(session.prompt("hi", Vec::new()).await.is_err());
    session.shutdown().await;
    assert_eq!(std::fs::read(&file).expect("read"), after);
}

// Bake: when the level's line is written in full but its sync fails, the
// session still applies only the known synced prefix, the new model with
// the previous level clamped to it, and halts; an immediate reader of the
// file sees the unsynced level line as well, so it is not claimed equal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unsynced_level_line_is_not_applied() {
    use crate::session::{Fault, FaultKind};
    let dir = TempDir::new("control-unsynced-level");
    let h = harness();
    h.faux
        .set_responses(vec![faux_assistant_message("first").into()]);
    // A default level, so switching to faux-2 moves the level from high.
    let settings = Settings::from_object(
        json!({"defaultThinkingLevel": "low"})
            .as_object()
            .expect("object"),
    );
    let session = h
        .session(file_session(&dir), settings, None, Vec::new())
        .await;
    session.prompt("one", Vec::new()).await.expect("flush");
    let file = session.session_file().expect("file");
    assert_eq!(session.thinking_level(), ModelThinkingLevel::High);
    lock(&session.session).arm_group_fault(Fault {
        line: 1,
        kind: FaultKind::FailSync,
    });
    let error = session
        .set_model(h.model("faux-2"), false)
        .await
        .expect_err("fails");
    assert!(
        error.contains("1 of 2 grouped entries written; the next was written but not synced"),
        "{error}"
    );
    assert_eq!(session.model(), Some(h.model("faux-2")));
    assert_eq!(session.thinking_level(), ModelThinkingLevel::High);
    let after = std::fs::read(&file).expect("read");
    let read = file_entries(&file);
    let held = lock(&session.session).entries().len();
    assert_eq!(read.len(), held + 1);
    assert_eq!(
        read.last().map(crate::session::SessionEntry::entry_type),
        Some("thinking_level_change")
    );
    let context = crate::session::build_session_context(&read, crate::session::LeafSelector::Last);
    assert_eq!(context.thinking_level, "low");
    assert!(session.prompt("two", Vec::new()).await.is_err());
    session.shutdown().await;
    assert_eq!(std::fs::read(&file).expect("read"), after);
}

// Bake: when the model's own line fails, the change is not applied,
// whether nothing of it was written or it was written without a sync,
// which a reader may see but a crash may lose; the report says which.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_change_whose_first_line_fails_is_not_applied() {
    use crate::session::{Fault, FaultKind};
    for (name, kind, state, lines) in [
        ("refused", FaultKind::Refuse, "was not written", 0),
        (
            "unsynced",
            FaultKind::FailSync,
            "was written but not synced",
            1,
        ),
    ] {
        let dir = TempDir::new(&format!("control-first-line-{name}"));
        let h = harness();
        let (session, file) = written_session(&h, &dir).await;
        let before = std::fs::read(&file).expect("read");
        lock(&session.session).arm_group_fault(Fault { line: 0, kind });
        let error = session
            .set_model(h.model("plain"), false)
            .await
            .expect_err("fails");
        assert!(
            error.starts_with("The model change could not be written"),
            "{error}"
        );
        assert!(
            error.contains(&format!("0 of 2 grouped entries written; the next {state}")),
            "{error}"
        );
        assert_eq!(session.model(), Some(h.model("faux-1")));
        assert_eq!(session.thinking_level(), ModelThinkingLevel::High);
        assert_eq!(session.selection().get().model, Some(h.model("faux-1")));
        let after = std::fs::read(&file).expect("read");
        assert_eq!(
            after[before.len()..]
                .iter()
                .filter(|b| **b == b'\n')
                .count(),
            lines
        );
        assert_eq!(
            file_entries(&file).len(),
            lock(&session.session).entries().len() + lines
        );
        assert!(session.prompt("two", Vec::new()).await.is_err());
        session.shutdown().await;
        assert_eq!(std::fs::read(&file).expect("read"), after);
    }
}

// Pi's `createAgentSession` records a new session's model and level. A
// session file without a conversation is already written, so the two
// entries are written then, as one group, in order; a group that fails
// part-way fails the creation, and the file keeps the written prefix.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_sessions_model_and_level_are_one_group() {
    use crate::session::{Fault, FaultKind};
    let h = harness();
    for fail in [false, true] {
        let dir = TempDir::new(&format!("control-initial-group-{fail}"));
        let file = dir.join("empty.jsonl");
        std::fs::write(&file, b"").expect("empty file");
        let mut manager = SessionManager::open(&file, None, None).expect("open");
        let header = std::fs::read(&file).expect("read");
        if fail {
            manager.arm_group_fault(Fault {
                line: 1,
                kind: FaultKind::Refuse,
            });
        }
        let created = AgentSession::create(AgentSessionOptions {
            session: manager,
            registry: h.runtime.clone(),
            apis: Arc::clone(&h.apis),
            settings: Settings::default(),
            settings_store: None,
            selection: None,
            model: Some(h.model("faux-1")),
            thinking_level: Some(ModelThinkingLevel::High),
            tools: Vec::new(),
            active_tool_names: Vec::new(),
            tool_prompts: Vec::new(),
            system_prompt: crate::system_prompt::SystemPromptOptions::new("/tmp"),
            docs_command: "bake-rs docs".into(),
            plugins: Default::default(),
            work: None,
            jobs: None,
        })
        .await;
        let read = file_entries(&file);
        let types: Vec<&str> = read.iter().map(|entry| entry.entry_type()).collect();
        assert!(std::fs::read(&file).expect("read").starts_with(&header));
        match created {
            Ok(session) => {
                assert!(!fail);
                assert_eq!(types, ["model_change", "thinking_level_change"]);
                assert_eq!(read[1].parent_id(), Some(read[0].id()));
                session.shutdown().await;
            }
            Err(error) => {
                assert!(fail, "{error}");
                assert!(error.contains("1 of 2 grouped entries written"), "{error}");
                assert_eq!(types, ["model_change"]);
            }
        }
    }
}

// Bake: the gap is recorded where writes are serialized. A message write
// queued behind a failing change entry, and run before the change goes on,
// is refused, so nothing lands after the failed entry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_queued_behind_a_failed_change_is_refused() {
    let dir = TempDir::new("control-queued-write");
    let h = harness();
    h.faux
        .set_responses(vec![faux_assistant_message("first").into()]);
    let session = h
        .session(file_session(&dir), Settings::default(), None, Vec::new())
        .await;
    session
        .prompt("one", Vec::new())
        .await
        .expect("flush the file");
    let file = session.session_file().expect("file");
    let written = std::fs::read(&file).expect("read");
    let (session_lock, gap) = (
        Arc::clone(&session.core.session),
        Arc::clone(&session.core.gap),
    );
    let (queued_tx, queued_rx) = std::sync::mpsc::channel();
    session.core.hooks().fail_model_entry = Some(Box::new(move || {
        // Under the session lock, as the entry fails: a message write
        // starts and waits for the lock.
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            started_tx.send(()).expect("started");
            let message = LiveMessage::from(bake_ai::UserMessage {
                content: bake_ai::UserContent::Text("late".into()),
                timestamp: 1,
            });
            persist_after_gap_check(&session_lock, &gap, &message)
        });
        started_rx
            .recv_timeout(PATIENCE)
            .expect("the writer started");
        Box::new(move || {
            // Before the change goes on, the queued write has run.
            queued_tx
                .send(writer.join().expect("writer"))
                .expect("send");
        })
    }));
    let error = session
        .set_model(h.model("faux-2"), false)
        .await
        .expect_err("the entry fails");
    assert!(error.contains("stopped"), "{error}");
    assert_eq!(
        queued_rx.recv_timeout(PATIENCE).expect("queued"),
        Ok(Persisted::AfterGap)
    );
    assert_eq!(std::fs::read(&file).expect("read"), written);
    assert_eq!(session.model(), Some(h.model("faux-1")));
    // Recovery and compaction writes take the same boundary.
    assert!(session.core.with_session(|_| ()).await.is_err());
    session.shutdown().await;
}

// Bake: the run stops at the failed write itself. While the change's own
// continuation is held, the held response is released with a tool call:
// the tool never runs, no further request is made, and the run ends, all
// before the change goes on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_change_write_stops_the_run_before_the_change_goes_on() {
    let dir = TempDir::new("control-stop-at-boundary");
    let h = harness();
    h.faux
        .set_responses(vec![faux_assistant_message("first").into()]);
    let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tool = {
        let ran = Arc::clone(&ran);
        Arc::new(AgentTool::new(
            "noop",
            "noop",
            "Counts its calls",
            json!({ "type": "object" }),
            move |_, _, _, _, _ctx| {
                ran.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(AgentToolResult::default()) })
            },
        ))
    };
    let session = Arc::new(
        h.session(file_session(&dir), Settings::default(), None, vec![tool])
            .await,
    );
    session
        .prompt("one", Vec::new())
        .await
        .expect("flush the file");
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    {
        let (started, release) = (started.clone(), release.clone());
        h.faux.set_responses(vec![
            FauxResponseStep::Factory(Arc::new(move |_, _, _, _| {
                let (started, release) = (started.clone(), release.clone());
                Box::pin(async move {
                    started.notify_one();
                    release.notified().await;
                    Ok(faux_assistant_blocks(
                        vec![faux_tool_call("noop", json!({}), Some("call-1"))],
                        StopReason::ToolUse,
                    ))
                })
            })),
            faux_assistant_message("never requested").into(),
        ]);
    }
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let run = {
        let session = Arc::clone(&session);
        tokio::spawn(async move {
            let result = session.prompt("two", Vec::new()).await;
            let _ = done_tx.send(());
            result
        })
    };
    started.notified().await;
    let calls_before = h.faux.call_count();
    let (seen_tx, seen_rx) = std::sync::mpsc::channel();
    {
        let (ran, faux) = (Arc::clone(&ran), h.faux.clone());
        session.core.hooks().fail_model_entry = Some(Box::new(move || {
            Box::new(move || {
                // The change's continuation is held here until the run,
                // released now, has ended.
                release.notify_one();
                done_rx.recv_timeout(PATIENCE).expect("the run ended");
                seen_tx
                    .send((ran.load(Ordering::SeqCst), faux.call_count()))
                    .expect("send");
            })
        }));
    }
    session
        .set_model(h.model("faux-2"), false)
        .await
        .expect_err("the entry fails");
    let (tools, calls) = seen_rx.recv_timeout(PATIENCE).expect("seen");
    assert_eq!(tools, 0, "the tool ran after the failure");
    assert_eq!(calls, calls_before, "a request followed the failure");
    assert!(run.await.expect("join").is_err());
    session.shutdown().await;
}

/// The longest a test waits on a barrier before it fails instead of hanging.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(30);

/// The names `session_info_changed` announced, in order.
fn name_events(session: &AgentSession) -> (SessionSubscription, Arc<Mutex<Vec<Option<String>>>>) {
    let names: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
    let seen = Arc::clone(&names);
    let subscription = session.subscribe(move |event| {
        if let SessionEvent::SessionInfoChanged(name) = event {
            seen.lock().expect("names").push(name.clone());
        }
    });
    (subscription, names)
}

// Pi test/suite/regressions/5996-session-name-newlines.test.ts "filters
// newlines when AgentSession.setSessionName is called" and
// 3686-session-name-event.test.ts "emits session_info_changed when
// AgentSession.setSessionName is called". The extension cases are not
// ported: bake-rs has no extensions.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_name_is_normalized_recorded_and_announced() {
    let h = harness();
    let session = h
        .session(in_memory(), Settings::default(), None, Vec::new())
        .await;
    let (subscription, names) = name_events(&session);
    let named = session
        .set_session_name("hello\nworld\r\nagain")
        .await
        .expect("named");
    assert_eq!(named.as_deref(), Some("hello world again"));
    assert_eq!(session.session_name().as_deref(), Some("hello world again"));
    session
        .set_session_name("hello world")
        .await
        .expect("named");
    assert_eq!(
        *names.lock().expect("names"),
        [
            Some("hello world again".to_owned()),
            Some("hello world".to_owned())
        ]
    );
    subscription.unsubscribe();
    session.shutdown().await;
    // After shutdown a name is refused, not kept in memory.
    assert!(session.set_session_name("late").await.is_err());
    assert_eq!(session.session_name().as_deref(), Some("hello world"));
}

// Pi `getSessionStats`: counts by role and tool calls, and token and cost
// totals from every recorded usage, read from the log; a malformed usage
// counts as absent rather than failing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_stats_read_the_log() {
    let h = harness();
    let usage = json!({"input": 10, "output": 5, "cacheRead": 20, "cacheWrite": 2,
        "totalTokens": 37, "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0.25}});
    let message = |value: Value| {
        crate::session::AgentMessage::from_json(value.as_object().expect("object").clone())
    };
    let mut manager = in_memory();
    manager
        .append_message(message(
            json!({"role": "user", "content": "go", "timestamp": 1}),
        ))
        .expect("user");
    manager
        .append_message(message(json!({"role": "assistant", "content": [
            {"type": "text", "text": "ok"},
            {"type": "toolCall", "id": "c1", "name": "read", "arguments": {}},
        ], "api": "faux", "provider": "faux", "model": "faux-1", "usage": usage,
            "stopReason": "toolUse", "timestamp": 2})))
        .expect("assistant");
    manager
        .append_message(message(
            json!({"role": "toolResult", "toolCallId": "c1", "toolName": "read",
            "content": [{"type": "text", "text": "x"}], "isError": false, "timestamp": 3}),
        ))
        .expect("tool result");
    manager
        .append_message(message(json!({"role": "assistant", "content": [{"type": "text", "text": "done"}],
            "api": "faux", "provider": "faux", "model": "faux-1", "usage": {"input": "bad", "cost": null},
            "stopReason": "stop", "timestamp": 4})))
        .expect("assistant");
    let session = h
        .session(manager, Settings::default(), None, Vec::new())
        .await;
    let stats = session.session_stats();
    assert_eq!(
        (
            stats.total_messages,
            stats.user_messages,
            stats.assistant_messages,
            stats.tool_calls,
            stats.tool_results
        ),
        (4, 1, 2, 1, 1)
    );
    assert_eq!(
        (
            stats.input,
            stats.output,
            stats.cache_read,
            stats.cache_write
        ),
        (10, 5, 20, 2)
    );
    assert_eq!(stats.total_tokens(), 37);
    assert!((stats.cost - 0.25).abs() < 1e-9);
    assert_eq!(stats.session_file, None);
    assert_eq!(stats.session_id, session.session_id());
    session.shutdown().await;
}
