//! Plugin vetoes through a real session: the faux model asks for real
//! `write` and `edit` calls under Bake's D24 file tools, and each case
//! checks the files, the approvals, the requests, and the session file on
//! disk. The guard is the `protected-paths` port of Pi v1.3.0's
//! `examples/extensions/protected-paths.ts`; the other plugins are scripted
//! to fail, hang, or block in order.

use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::Duration;

use bake_ai::providers::faux::{
    FauxProvider, FauxProviderOptions, faux_assistant_blocks, faux_assistant_message,
    faux_tool_call,
};
use bake_ai::{ApiRegistry, StopReason};
use bake_sandbox::{EscalationOutcome, SandboxMode, UnavailableProvider};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use super::*;
use crate::auth_storage::AuthStorage;
use crate::model_registry::config::ModelConfig;
use crate::permissions::approval::ApprovalFuture;
use crate::permissions::{
    ApprovalPolicy, ApprovalRequest, Approver, StandingPermissions, ToolPermissions,
    sandbox_file_tools,
};
use crate::plugins::tests::{Scripted, allow, answer, block, fail, hang};
use crate::plugins::{DECISION_ENTRY_TYPE, Plugin, ProtectedPaths, ToolCallVeto};
use crate::session::NewSessionOptions;
use crate::test_support::TempDir;
use crate::tools::index::{ToolsOptions, create_all_tool_definitions};

/// How long a test waits for one readiness, admission, gate, or task
/// transition before it reports that transition missing. Generous: each
/// transition is a callback away, never a deadline under test.
const WAIT: Duration = Duration::from_secs(10);

/// Awaits `future` for up to [`WAIT`]; a miss reports `what` instead of
/// hanging the test.
async fn within<T>(what: &str, future: impl Future<Output = T>) -> Result<T, String> {
    tokio::time::timeout(WAIT, future)
        .await
        .map_err(|_| format!("timed out waiting for {what}"))
}

/// Joins `task` for up to [`WAIT`]. A task that misses it is aborted and
/// awaited, so it does not outlive the test, and the miss is reported.
async fn join<T>(what: &str, mut task: JoinHandle<T>) -> Result<T, String> {
    match tokio::time::timeout(WAIT, &mut task).await {
        Ok(joined) => joined.map_err(|error| format!("{what} task failed: {error}")),
        Err(_) => {
            task.abort();
            let _ = task.await;
            Err(format!("timed out waiting for {what}"))
        }
    }
}

/// Attempts bounded shutdown before reporting the original failure; if
/// cleanup also misses its deadline, reports both failures.
async fn fail_after_shutdown(session: &AgentSession, error: String) -> ! {
    if let Err(cleanup) = within("shutdown after a failure", session.shutdown()).await {
        panic!("{error}; cleanup also failed: {cleanup}");
    }
    panic!("{error}");
}

/// Answers every approval with one outcome and counts the requests.
struct Fixed(EscalationOutcome, Arc<AtomicUsize>);

impl Approver for Fixed {
    fn request(&self, _request: ApprovalRequest, _cancel: Option<AbortSignal>) -> ApprovalFuture {
        self.1.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(self.0))
    }
}

struct Fixture {
    dir: TempDir,
    faux: Arc<FauxProvider>,
    registry: Arc<ModelRegistry>,
    apis: Arc<ApiRegistry>,
    asked: Arc<AtomicUsize>,
    /// Runs `write` and `edit` in parallel batches, as any tool a manifest
    /// lists other than Bake's ordered ones would.
    parallel: bool,
}

impl Fixture {
    fn new(name: &str) -> Self {
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
                    "models": [{ "id": "faux-1" }],
                }),
                Some("test-key".into()),
            )
            .expect("faux provider");
        let dir = TempDir::new(name);
        std::fs::create_dir_all(dir.join("ws")).expect("ws");
        Self {
            dir,
            faux,
            registry: Arc::new(registry),
            apis,
            asked: Arc::default(),
            parallel: false,
        }
    }

    fn ws(&self) -> std::path::PathBuf {
        self.dir.join("ws")
    }

    fn new_store(&self) -> SessionManager {
        SessionManager::create(
            &self.ws().to_string_lossy(),
            Some(&self.dir.join("sessions")),
            NewSessionOptions::default(),
        )
        .expect("session")
    }

    /// A session over Pi's file tools under D24 in `mode` with the `ask`
    /// policy, approvals answered by `outcome`.
    async fn session(
        &self,
        store: SessionManager,
        mode: SandboxMode,
        outcome: EscalationOutcome,
        plugins: PluginRegistry,
    ) -> AgentSession {
        let ws = self.ws();
        let standing = StandingPermissions::new(
            mode,
            ApprovalPolicy::Ask,
            "test",
            &ws,
            Some("plugin-tests".into()),
        );
        let permissions = ToolPermissions::new(
            Arc::new(standing),
            Arc::new(Fixed(outcome, Arc::clone(&self.asked))),
            Arc::new(UnavailableProvider::new(None)),
        );
        let tools: Vec<Arc<AgentTool>> = sandbox_file_tools(
            create_all_tool_definitions(&ws, &ToolsOptions::default()),
            &ws,
            &permissions,
        )
        .into_iter()
        .filter(|definition| matches!(definition.name(), "write" | "edit"))
        .map(|definition| {
            let mut tool = definition.tool;
            if self.parallel {
                tool.execution_mode = Some(ToolExecutionMode::Parallel);
            }
            Arc::new(tool)
        })
        .collect();
        AgentSession::create(AgentSessionOptions {
            session: store,
            registry: Arc::clone(&self.registry).into(),
            settings_store: None,
            selection: None,
            apis: Arc::clone(&self.apis),
            settings: Settings::default(),
            model: self.registry.model("faux", "faux-1"),
            thinking_level: None,
            active_tool_names: tools.iter().map(|tool| tool.name.clone()).collect(),
            tools,
            tool_prompts: Vec::new(),
            system_prompt: SystemPromptOptions::new(ws.to_string_lossy()),
            docs_command: "bake-rs docs".into(),
            plugins,
        })
        .await
        .expect("session")
    }

    /// The model asks for `calls` (tool, args, id) in one response, then
    /// answers "done".
    fn script(&self, calls: &[(&str, Value, &str)]) {
        self.faux.append_responses(vec![
            faux_assistant_blocks(
                calls
                    .iter()
                    .map(|(tool, args, id)| faux_tool_call(tool, args.clone(), Some(id)))
                    .collect(),
                StopReason::ToolUse,
            )
            .into(),
            faux_assistant_message("done").into(),
        ]);
    }
}

fn registry(plugins: Vec<(Arc<dyn Plugin>, i32)>) -> PluginRegistry {
    let mut registry = PluginRegistry::new();
    for (plugin, order) in plugins {
        registry.register(plugin, order).expect("register");
    }
    registry
}

fn guard() -> PluginRegistry {
    registry(vec![(Arc::new(ProtectedPaths::pi_default()), 0)])
}

/// The session file's entries, read from disk.
fn file_entries(session: &AgentSession) -> Vec<Value> {
    let path = session.session_file().expect("persisted");
    std::fs::read_to_string(path)
        .expect("session file")
        .lines()
        .map(|line| serde_json::from_str(line).expect("json line"))
        .filter(|entry: &Value| entry["type"] != "session")
        .collect()
}

fn decisions(entries: &[Value]) -> Vec<&Value> {
    entries
        .iter()
        .filter(|entry| entry["type"] == "custom" && entry["customType"] == DECISION_ENTRY_TYPE)
        .collect()
}

/// The text and error flag of the `toolResult` entry for `id`, and its
/// index.
fn tool_result(entries: &[Value], id: &str) -> Option<(usize, String, bool)> {
    entries.iter().enumerate().find_map(|(index, entry)| {
        let message = &entry["message"];
        (entry["type"] == "message"
            && message["role"] == "toolResult"
            && message["toolCallId"] == id)
            .then(|| {
                let text = message["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|block| block["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                (index, text, message["isError"] == true)
            })
    })
}

fn decision_index(entries: &[Value], id: &str) -> usize {
    entries
        .iter()
        .position(|entry| {
            entry["type"] == "custom"
                && entry["customType"] == DECISION_ENTRY_TYPE
                && entry["data"]["toolCallId"] == id
        })
        .expect("decision entry")
}

/// The live transcript's tool-result texts by call id.
fn live_results(messages: &[LiveMessage]) -> Vec<(String, String)> {
    messages
        .iter()
        .filter_map(LiveMessage::as_tool_result)
        .map(|result| {
            let text = result
                .content
                .iter()
                .filter_map(|block| match block {
                    UserContentBlock::Text(text) => Some(text.text.clone()),
                    UserContentBlock::Image(_) => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            (result.tool_call_id.clone(), text)
        })
        .collect()
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

const SECRET: &str = "SECRET=1\n";

fn protected_calls() -> Vec<(&'static str, Value, &'static str)> {
    vec![
        (
            "write",
            json!({ "path": ".env", "content": "pwned" }),
            "w-env",
        ),
        (
            "edit",
            json!({ "path": ".env", "edits": [{ "oldText": "SECRET=1", "newText": "SECRET=0" }] }),
            "e-env",
        ),
        (
            "write",
            json!({ "path": "notes.txt", "content": "kept" }),
            "w-notes",
        ),
    ]
}

// Pi's protected-paths example through a real session under
// danger-full-access, where D24 would allow every write: the guard alone
// keeps `.env` unchanged, each block's decision is written before its
// denial result, the allowed write lands with exactly the model's
// arguments, and a resumed session replays the denials the model saw. The
// control arm, without the guard, shows the same calls change `.env`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_guard_blocks_protected_writes_and_records_each_decision_first() {
    let fx = Fixture::new("plugin-guard");
    let env = fx.ws().join(".env");
    std::fs::write(&env, SECRET).expect("seed .env");

    fx.script(&protected_calls());
    let session = fx
        .session(
            fx.new_store(),
            SandboxMode::DangerFullAccess,
            EscalationOutcome::Rejected,
            guard(),
        )
        .await;
    session.prompt("go", Vec::new()).await.expect("prompt");

    assert_eq!(read(&env).as_deref(), Some(SECRET), "bytes unchanged");
    assert_eq!(read(&fx.ws().join("notes.txt")).as_deref(), Some("kept"));
    assert_eq!(fx.asked.load(Ordering::SeqCst), 0);

    let entries = file_entries(&session);
    assert_eq!(decisions(&entries).len(), 2, "only the blocks are recorded");
    let assistant = entries
        .iter()
        .position(|entry| entry["message"]["role"] == "assistant")
        .expect("assistant");
    for id in ["w-env", "e-env"] {
        let decision = decision_index(&entries, id);
        let (result, text, is_error) = tool_result(&entries, id).expect("denial");
        assert!(assistant < decision && decision < result, "{id}: order");
        assert!(is_error);
        assert_eq!(text, "Path \".env\" is protected");
        let data = &entries[decision]["data"];
        assert_eq!(data["plugin"], "protected-paths");
        assert_eq!(data["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(data["decision"], "block");
        assert_eq!(data["reason"], text);
    }
    assert_eq!(
        tool_result(&entries, "w-notes").map(|(_, _, error)| error),
        Some(false)
    );
    // The persisted calls are the model's, unchanged by any plugin.
    let calls = entries[assistant]["message"]["content"]
        .as_array()
        .expect("content")
        .iter()
        .filter(|block| block["type"] == "toolCall")
        .map(|block| (block["id"].clone(), block["arguments"].clone()))
        .collect::<Vec<_>>();
    let expected: Vec<(Value, Value)> = protected_calls()
        .into_iter()
        .map(|(_, args, id)| (json!(id), args))
        .collect();
    assert_eq!(calls, expected);

    // A resumed session sends the model the same denials, and the decision
    // entries stay out of its context.
    let live = live_results(&session.messages());
    let file = session.session_file().expect("file");
    session.shutdown().await;
    let resumed = fx
        .session(
            SessionManager::open(&file, None, None).expect("open"),
            SandboxMode::DangerFullAccess,
            EscalationOutcome::Rejected,
            PluginRegistry::default(),
        )
        .await;
    assert_eq!(live_results(&resumed.messages()), live);
    assert!(
        live.iter()
            .any(|(id, text)| id == "w-env" && text == "Path \".env\" is protected")
    );
    assert!(
        resumed
            .messages()
            .iter()
            .all(|message| matches!(message, LiveMessage::Llm(_)))
    );
    resumed.shutdown().await;

    // Control: the same calls without the guard rewrite `.env`.
    fx.script(&protected_calls());
    let unguarded = fx
        .session(
            fx.new_store(),
            SandboxMode::DangerFullAccess,
            EscalationOutcome::Rejected,
            PluginRegistry::default(),
        )
        .await;
    unguarded.prompt("go", Vec::new()).await.expect("prompt");
    assert_eq!(read(&env).as_deref(), Some("pwned"));
    assert!(decisions(&file_entries(&unguarded)).is_empty());
    unguarded.shutdown().await;
}

// A plugin's `continue` only lets the call reach the tool: under read-only
// with `ask`, a plain write the guard allows is still denied by D24, an
// escalated one still asks the user, and an escalated write to a protected
// path is blocked before anyone is asked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_allowed_call_still_goes_through_d24() {
    let fx = Fixture::new("plugin-d24");
    let escalate = |path: &str| {
        json!({
            "path": path,
            "content": "x",
            "sandbox_permissions": "workspace-write",
            "justification": "the test needs it",
        })
    };
    fx.script(&[
        (
            "write",
            json!({ "path": "plain.txt", "content": "x" }),
            "plain",
        ),
        ("write", escalate("granted.txt"), "granted"),
        ("write", escalate(".env"), "env"),
    ]);
    let session = fx
        .session(
            fx.new_store(),
            SandboxMode::ReadOnly,
            EscalationOutcome::AllowedOnce,
            guard(),
        )
        .await;
    session.prompt("go", Vec::new()).await.expect("prompt");

    let entries = file_entries(&session);
    let (_, plain, plain_error) = tool_result(&entries, "plain").expect("plain");
    assert!(plain_error);
    assert!(
        plain.starts_with("[sandbox: file access denied under read-only mode]"),
        "{plain}"
    );
    assert_eq!(read(&fx.ws().join("plain.txt")), None);
    assert_eq!(read(&fx.ws().join("granted.txt")).as_deref(), Some("x"));
    assert_eq!(read(&fx.ws().join(".env")), None);
    assert_eq!(fx.asked.load(Ordering::SeqCst), 1, "only `granted` asked");
    let recorded = decisions(&entries);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["data"]["toolCallId"], "env");
    session.shutdown().await;
}

// Plugins run in configured order, then by id; the first block wins and
// later plugins are not asked. Unlisted tools reach no plugin.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_block_in_order_wins_in_a_session() {
    let fx = Fixture::new("plugin-order");
    std::fs::write(fx.ws().join("a.txt"), "a").expect("seed");
    let first = Arc::new(Scripted::new("first", &["write"], allow()));
    let second = Arc::new(Scripted::new("second", &["write"], block("second says no")));
    let third = Arc::new(Scripted::new("third", &["write"], block("third says no")));
    fx.script(&[
        ("write", json!({ "path": "a.txt", "content": "b" }), "w"),
        (
            "edit",
            json!({ "path": "a.txt", "edits": [{ "oldText": "a", "newText": "c" }] }),
            "e",
        ),
    ]);
    let session = fx
        .session(
            fx.new_store(),
            SandboxMode::DangerFullAccess,
            EscalationOutcome::Rejected,
            registry(vec![
                (Arc::clone(&third) as Arc<dyn Plugin>, 2),
                (Arc::clone(&second) as Arc<dyn Plugin>, 1),
                (Arc::clone(&first) as Arc<dyn Plugin>, 0),
            ]),
        )
        .await;
    session.prompt("go", Vec::new()).await.expect("prompt");
    assert_eq!(first.calls(), ["write:w"]);
    assert_eq!(second.calls(), ["write:w"]);
    assert!(third.calls().is_empty());
    // The write was blocked; the unlisted edit ran.
    assert_eq!(read(&fx.ws().join("a.txt")).as_deref(), Some("c"));
    let entries = file_entries(&session);
    let recorded = decisions(&entries);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["data"]["plugin"], "second");
    assert_eq!(
        tool_result(&entries, "w")
            .map(|(_, text, _)| text)
            .as_deref(),
        Some("second says no")
    );
    session.shutdown().await;
}

// A plugin that errs, and one that misses its deadline, block that call and
// every later call to their listed tools, across prompts, without being
// asked again; their unlisted tools still run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_plugins_keep_blocking_their_tools() {
    let fx = Fixture::new("plugin-failed");
    std::fs::write(fx.ws().join("e.txt"), "a").expect("seed");
    let broken = Arc::new(Scripted::new("broken", &["write"], fail("boom")));
    let entered = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let slow = Arc::new(Scripted::new(
        "slow",
        &["edit"],
        hang(Arc::clone(&entered), Arc::clone(&dropped)),
    ));
    let plugins = registry(vec![
        (Arc::clone(&broken) as Arc<dyn Plugin>, 0),
        (Arc::clone(&slow) as Arc<dyn Plugin>, 0),
    ])
    .with_call_timeout(Duration::from_millis(50));
    let edit = |id| {
        (
            "edit",
            json!({ "path": "e.txt", "edits": [{ "oldText": "a", "newText": "b" }] }),
            id,
        )
    };
    fx.script(&[
        ("write", json!({ "path": "w1.txt", "content": "x" }), "w1"),
        edit("e1"),
    ]);
    let session = fx
        .session(
            fx.new_store(),
            SandboxMode::DangerFullAccess,
            EscalationOutcome::Rejected,
            plugins,
        )
        .await;
    session.prompt("one", Vec::new()).await.expect("prompt");
    assert!(
        dropped.load(Ordering::SeqCst),
        "the timed-out check was dropped"
    );
    fx.script(&[
        ("write", json!({ "path": "w2.txt", "content": "x" }), "w2"),
        edit("e2"),
    ]);
    session.prompt("two", Vec::new()).await.expect("prompt");

    assert_eq!(broken.calls(), ["write:w1"]);
    assert_eq!(slow.calls(), ["edit:e1"]);
    assert_eq!(read(&fx.ws().join("w1.txt")), None);
    assert_eq!(read(&fx.ws().join("w2.txt")), None);
    assert_eq!(read(&fx.ws().join("e.txt")).as_deref(), Some("a"));
    let entries = file_entries(&session);
    let failure = |id: &str| entries[decision_index(&entries, id)]["data"]["failure"].clone();
    assert_eq!(
        failure("w1"),
        json!({ "kind": "error", "message": "boom", "earlier": false })
    );
    assert_eq!(failure("w2")["earlier"], true);
    assert_eq!(failure("e1")["kind"], "timeout");
    assert_eq!(failure("e2")["earlier"], true);
    assert_eq!(
        tool_result(&entries, "w2")
            .map(|(_, text, _)| text)
            .as_deref(),
        Some("Plugin \"broken\" failed earlier, blocking execution: boom")
    );
    session.shutdown().await;
}

/// Hangs on a call to `a.txt` until it is dropped; blocks every other
/// call with "asked again".
fn hang_then_block(entered: &Arc<Notify>, dropped: &Arc<AtomicBool>) -> Arc<Scripted> {
    let hanging = hang(Arc::clone(entered), Arc::clone(dropped));
    Arc::new(Scripted::new(
        "slow",
        &["write"],
        answer(move |tool, args| {
            if args["path"] == "a.txt" {
                hanging(tool, args)
            } else {
                Box::pin(async {
                    Ok(ToolCallVeto::Block {
                        reason: "asked again".into(),
                        terminate: false,
                    })
                })
            }
        }),
    ))
}

// Esc while a plugin checks a call: its future is dropped, the call does
// not run, the loop answers it `Operation aborted`, and nothing is recorded.
// The cancellation itself fails nothing: the next prompt asks the plugin
// again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborting_a_check_cancels_the_call_and_fails_nothing() {
    let fx = Fixture::new("plugin-abort");
    let entered = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let slow = hang_then_block(&entered, &dropped);
    fx.script(&[("write", json!({ "path": "a.txt", "content": "x" }), "w1")]);
    let session = Arc::new(
        fx.session(
            fx.new_store(),
            SandboxMode::DangerFullAccess,
            EscalationOutcome::Rejected,
            registry(vec![(Arc::clone(&slow) as Arc<dyn Plugin>, 0)]),
        )
        .await,
    );
    let running = tokio::spawn({
        let session = Arc::clone(&session);
        async move { session.prompt("go", Vec::new()).await }
    });
    // Every wait is bounded, and the abort and join run whether or not the
    // check started, so a missing transition fails here with no prompt
    // left running; assertions wait until the prompt has ended.
    let started = within("the plugin check to start", entered.notified()).await;
    let aborted = within("the abort", session.abort()).await;
    let prompted = join("the first prompt", running).await;
    if let Err(error) = started.and(aborted).and(prompted.map(drop)) {
        fail_after_shutdown(&session, error).await;
    }
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(read(&fx.ws().join("a.txt")), None);
    let entries = file_entries(&session);
    assert!(
        decisions(&entries).is_empty(),
        "a cancelled call decides nothing"
    );
    let (_, text, is_error) = tool_result(&entries, "w1").expect("result");
    assert!(is_error);
    assert_eq!(text, "Operation aborted");

    fx.faux.set_responses(Vec::new());
    fx.script(&[("write", json!({ "path": "b.txt", "content": "x" }), "w2")]);
    session.prompt("again", Vec::new()).await.expect("prompt");
    assert_eq!(slow.calls(), ["write:w1", "write:w2"], "asked again");
    assert_eq!(read(&fx.ws().join("b.txt")), None);
    let entries = file_entries(&session);
    let recorded = decisions(&entries);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["data"]["toolCallId"], "w2");
    assert_eq!(recorded[0]["data"]["decision"], "block");
    session.shutdown().await;
}

/// A session whose single `write` to `a.txt` a plugin blocks, with `hook`
/// installed by `install` before the prompt; returns the session file's
/// entries after the prompt.
async fn abort_around_admission(
    name: &str,
    install: impl FnOnce(&AgentSession, Arc<Agent>),
) -> (Fixture, Vec<Value>) {
    let fx = Fixture::new(name);
    fx.script(&[("write", json!({ "path": "a.txt", "content": "x" }), "w1")]);
    let session = fx
        .session(
            fx.new_store(),
            SandboxMode::DangerFullAccess,
            EscalationOutcome::Rejected,
            registry(vec![(
                Arc::new(Scripted::new("guard", &["write"], block("no"))),
                0,
            )]),
        )
        .await;
    install(&session, session.core.agent().expect("agent"));
    let _ = session.prompt("go", Vec::new()).await;
    assert_eq!(read(&fx.ws().join("a.txt")), None);
    assert_eq!(fx.faux.call_count(), 1, "no request after the abort");
    let entries = file_entries(&session);
    session.shutdown().await;
    (fx, entries)
}

// Cancellation that lands after dispatch but before the decision write is
// admitted under the session lock: nothing is recorded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_abort_before_the_decision_is_admitted_records_nothing() {
    let (_fx, entries) = abort_around_admission("plugin-abort-pre", |session, agent| {
        session.core.hooks().before_plugin_decision_admission = Some(Box::new(move || {
            agent.abort();
        }));
    })
    .await;
    assert!(decisions(&entries).is_empty());
    let (_, text, _) = tool_result(&entries, "w1").expect("result");
    assert_eq!(text, "Operation aborted");
}

// Cancellation that lands once the decision write is admitted: the owned
// entry is written, but the loop, which checks the signal after the hook,
// answers the call `Operation aborted`. This is the case tested here in
// which a decision has no matching denial result; a crash or shutdown
// before the result is written, or a failed result write, leaves the same
// trace.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_abort_after_the_decision_is_admitted_keeps_it_but_reports_the_abort() {
    let (_fx, entries) = abort_around_admission("plugin-abort-post", |session, agent| {
        session.core.hooks().plugin_decision_admitted = Some(Box::new(move || {
            agent.abort();
        }));
    })
    .await;
    let decision = decision_index(&entries, "w1");
    assert_eq!(entries[decision]["data"]["decision"], "block");
    let (result, text, _) = tool_result(&entries, "w1").expect("result");
    assert!(decision < result);
    assert_eq!(text, "Operation aborted");
}

// Shutdown while a plugin checks a call: shutdown aborts the call before it
// drops the run, so the check is dropped, nothing is recorded, the call
// never ran, and nothing calls the plugin afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_during_a_check_leaves_nothing_running() {
    let fx = Fixture::new("plugin-shutdown");
    let entered = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let slow = hang_then_block(&entered, &dropped);
    fx.script(&[("write", json!({ "path": "a.txt", "content": "x" }), "w1")]);
    let session = Arc::new(
        fx.session(
            fx.new_store(),
            SandboxMode::DangerFullAccess,
            EscalationOutcome::Rejected,
            registry(vec![(Arc::clone(&slow) as Arc<dyn Plugin>, 0)]),
        )
        .await,
    );
    let running = tokio::spawn({
        let session = Arc::clone(&session);
        async move { session.prompt("go", Vec::new()).await }
    });
    // Bounded waits; shutdown and the join run whether or not the check
    // started, and assertions wait until the prompt has ended.
    let started = within("the plugin check to start", entered.notified()).await;
    let shut = within("shutdown", session.shutdown()).await;
    let dropped_by_shutdown = dropped.load(Ordering::SeqCst);
    let prompted = join("the prompt", running).await;
    if let Err(error) = started.and(shut).and(prompted.map(drop)) {
        fail_after_shutdown(&session, error).await;
    }
    assert!(dropped_by_shutdown, "dropped before shutdown returned");
    let settled = std::fs::read(session.session_file().expect("file")).expect("bytes");
    assert!(decisions(&file_entries(&session)).is_empty());
    assert!(session.prompt("later", Vec::new()).await.is_err());
    assert_eq!(slow.calls(), ["write:w1"]);
    assert_eq!(read(&fx.ws().join("a.txt")), None);
    assert_eq!(
        std::fs::read(session.session_file().expect("file")).expect("bytes"),
        settled,
        "nothing is written after shutdown"
    );
}

// Shutdown while an admitted decision write is in progress: shutdown waits
// for it, so the owned entry lands before shutdown returns and nothing is
// written afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_waits_for_an_admitted_decision_write() {
    let fx = Fixture::new("plugin-shutdown-admitted");
    fx.script(&[("write", json!({ "path": "a.txt", "content": "x" }), "w1")]);
    let session = Arc::new(
        fx.session(
            fx.new_store(),
            SandboxMode::DangerFullAccess,
            EscalationOutcome::Rejected,
            registry(vec![(
                Arc::new(Scripted::new("guard", &["write"], block("no"))),
                0,
            )]),
        )
        .await,
    );
    let admitted = Arc::new(Notify::new());
    let (release, released) = std::sync::mpsc::channel::<()>();
    session.core.hooks().plugin_decision_admitted = Some(Box::new({
        let admitted = Arc::clone(&admitted);
        // Bounded, so the blocking write cannot outlive the test even if
        // no release is sent.
        move || {
            admitted.notify_one();
            let _ = released.recv_timeout(WAIT);
        }
    }));
    let running = tokio::spawn({
        let session = Arc::clone(&session);
        async move { session.prompt("go", Vec::new()).await }
    });
    // Every wait is bounded, and on every path the write is released, then
    // shutdown and the prompt are joined, before anything is asserted.
    let admission = within("the decision write's admission", admitted.notified()).await;
    let shutting = tokio::spawn({
        let session = Arc::clone(&session);
        async move { session.shutdown().await }
    });
    // Shutdown has reached its work gate once the gate admits nothing.
    let gated = within("shutdown to close the work gate", async {
        while session.work.enter().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    let waited = !shutting.is_finished();
    let _ = release.send(());
    drop(release);
    let shut = join("shutdown", shutting).await;
    let prompted = join("the prompt", running).await;
    if let Err(error) = admission.and(gated).and(shut).and(prompted.map(drop)) {
        fail_after_shutdown(&session, error).await;
    }
    assert!(waited, "shutdown waits for the write");
    let settled = std::fs::read(session.session_file().expect("file")).expect("bytes");
    let entries = file_entries(&session);
    let assistant = entries
        .iter()
        .position(|entry| entry["message"]["role"] == "assistant")
        .expect("assistant");
    assert!(
        assistant < decision_index(&entries, "w1"),
        "the entry landed"
    );
    assert_eq!(read(&fx.ws().join("a.txt")), None);
    assert!(session.prompt("later", Vec::new()).await.is_err());
    assert_eq!(
        std::fs::read(session.session_file().expect("file")).expect("bytes"),
        settled,
        "nothing is written after shutdown"
    );
}

// Integration of the plugin veto with the session's work gate: a decision
// write that has started but is not yet admitted under the session lock is
// the gate's blocking work. Shutdown aborts the run, whose hook future is
// dropped, but the write keeps its gate permit, so the gate is not drained
// while it runs; the write then sees the abort at its admission and records
// nothing. The session lock alone would not show this: the write has not
// taken it yet.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_owns_a_decision_write_before_its_admission() {
    let fx = Fixture::new("plugin-shutdown-pre-admission");
    fx.script(&[("write", json!({ "path": "a.txt", "content": "x" }), "w1")]);
    let session = Arc::new(
        fx.session(
            fx.new_store(),
            SandboxMode::DangerFullAccess,
            EscalationOutcome::Rejected,
            registry(vec![(
                Arc::new(Scripted::new("guard", &["write"], block("no"))),
                0,
            )]),
        )
        .await,
    );
    let entered = Arc::new(Notify::new());
    let (release, released) = std::sync::mpsc::channel::<()>();
    session.core.hooks().before_plugin_decision_admission = Some(Box::new({
        let entered = Arc::clone(&entered);
        // Bounded, so the blocking write cannot outlive the test even if
        // no release is sent.
        move || {
            entered.notify_one();
            let _ = released.recv_timeout(WAIT);
        }
    }));
    let running = tokio::spawn({
        let session = Arc::clone(&session);
        async move { session.prompt("go", Vec::new()).await }
    });
    // Every wait is bounded, and on every path the write is released, then
    // shutdown and the prompt are joined, before anything is asserted.
    let started = within("the decision write's start", entered.notified()).await;
    let shutting = tokio::spawn({
        let session = Arc::clone(&session);
        async move { session.shutdown().await }
    });
    // Once the prompt has returned, the run is gone and the decision write
    // is the only work the session could still own.
    let prompted = join("the prompt", running).await;
    let owned = session.work.is_closing() && !session.work.is_closed();
    let _ = release.send(());
    drop(release);
    let shut = join("shutdown", shutting).await;
    if let Err(error) = started.and(prompted.map(drop)).and(shut) {
        fail_after_shutdown(&session, error).await;
    }
    assert!(owned, "the gate does not own the decision write");
    let entries = file_entries(&session);
    assert!(
        decisions(&entries).is_empty(),
        "nothing admitted after the abort"
    );
    assert_eq!(read(&fx.ws().join("a.txt")), None);
}

// A block with `terminate`, as Pi's `terminate`: the batch's only call is
// blocked and the agent stops without another model request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_terminating_block_ends_the_run() {
    let fx = Fixture::new("plugin-terminate");
    let stop = Arc::new(Scripted::new(
        "stop",
        &["write"],
        answer(|_, _| {
            Box::pin(async {
                Ok(ToolCallVeto::Block {
                    reason: "stop here".into(),
                    terminate: true,
                })
            })
        }),
    ));
    fx.script(&[("write", json!({ "path": "a.txt", "content": "x" }), "w1")]);
    let session = fx
        .session(
            fx.new_store(),
            SandboxMode::DangerFullAccess,
            EscalationOutcome::Rejected,
            registry(vec![(stop as Arc<dyn Plugin>, 0)]),
        )
        .await;
    session.prompt("go", Vec::new()).await.expect("prompt");
    assert_eq!(
        fx.faux.call_count(),
        1,
        "terminated without a second request"
    );
    assert_eq!(read(&fx.ws().join("a.txt")), None);
    let entries = file_entries(&session);
    let decision = decision_index(&entries, "w1");
    assert_eq!(entries[decision]["data"]["terminate"], true);
    let (result, text, _) = tool_result(&entries, "w1").expect("result");
    assert!(decision < result);
    assert_eq!(text, "stop here");
    session.shutdown().await;
}

// A decision that cannot be written stops the run: neither the blocked call
// nor the next call in the batch runs, no further request is sent, the
// prompt reports the failure, and later prompts are refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unwritable_decision_stops_the_run() {
    let fx = Fixture::new("plugin-unwritable");
    std::fs::write(fx.ws().join(".env"), SECRET).expect("seed");
    fx.script(&protected_calls());
    let session = fx
        .session(
            fx.new_store(),
            SandboxMode::DangerFullAccess,
            EscalationOutcome::Rejected,
            guard(),
        )
        .await;
    session.core.hooks().fail_plugin_decision = true;
    let error = session
        .prompt("go", Vec::new())
        .await
        .expect_err("the run stops");
    assert!(error.contains("injected failure"), "{error}");
    assert_eq!(fx.faux.call_count(), 1, "no request after the failure");
    assert_eq!(read(&fx.ws().join(".env")).as_deref(), Some(SECRET));
    assert_eq!(read(&fx.ws().join("notes.txt")), None);
    let entries = file_entries(&session);
    assert!(decisions(&entries).is_empty());
    assert!(
        tool_result(&entries, "w-env").is_none(),
        "no result past the gap"
    );
    assert!(session.prompt("again", Vec::new()).await.is_err());
    session.shutdown().await;
}

fn parallel_calls() -> Vec<(&'static str, Value, &'static str)> {
    vec![
        (
            "write",
            json!({ "path": "first.txt", "content": "x" }),
            "w-first",
        ),
        (
            "write",
            json!({ "path": ".env", "content": "pwned" }),
            "w-env",
        ),
        (
            "write",
            json!({ "path": "last.txt", "content": "x" }),
            "w-last",
        ),
    ]
}

// A parallel batch: calls before and after a block are prepared and then
// run together, and the block's decision still precedes its denial.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_parallel_batch_runs_its_allowed_calls_around_a_block() {
    let mut fx = Fixture::new("plugin-parallel");
    fx.parallel = true;
    std::fs::write(fx.ws().join(".env"), SECRET).expect("seed");
    fx.script(&parallel_calls());
    let session = fx
        .session(
            fx.new_store(),
            SandboxMode::DangerFullAccess,
            EscalationOutcome::Rejected,
            guard(),
        )
        .await;
    session.prompt("go", Vec::new()).await.expect("prompt");
    assert_eq!(read(&fx.ws().join(".env")).as_deref(), Some(SECRET));
    assert_eq!(read(&fx.ws().join("first.txt")).as_deref(), Some("x"));
    assert_eq!(read(&fx.ws().join("last.txt")).as_deref(), Some("x"));
    let entries = file_entries(&session);
    let decision = decision_index(&entries, "w-env");
    let (result, text, _) = tool_result(&entries, "w-env").expect("denial");
    assert!(decision < result);
    assert_eq!(text, "Path \".env\" is protected");
    session.shutdown().await;
}

// A parallel batch whose decision cannot be written: the call prepared
// before the block is deferred until the batch is prepared, and the abort
// the failed write raises stops it there; no later call is prepared and no
// further request is sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unwritable_decision_stops_a_parallel_batch() {
    let mut fx = Fixture::new("plugin-parallel-unwritable");
    fx.parallel = true;
    std::fs::write(fx.ws().join(".env"), SECRET).expect("seed");
    fx.script(&parallel_calls());
    let session = fx
        .session(
            fx.new_store(),
            SandboxMode::DangerFullAccess,
            EscalationOutcome::Rejected,
            guard(),
        )
        .await;
    session.core.hooks().fail_plugin_decision = true;
    let error = session
        .prompt("go", Vec::new())
        .await
        .expect_err("the run stops");
    assert!(error.contains("injected failure"), "{error}");
    assert_eq!(fx.faux.call_count(), 1, "no request after the failure");
    assert_eq!(read(&fx.ws().join(".env")).as_deref(), Some(SECRET));
    assert_eq!(read(&fx.ws().join("first.txt")), None, "prepared, not run");
    assert_eq!(read(&fx.ws().join("last.txt")), None);
    assert!(decisions(&file_entries(&session)).is_empty());
    assert!(session.prompt("again", Vec::new()).await.is_err());
    session.shutdown().await;
}
