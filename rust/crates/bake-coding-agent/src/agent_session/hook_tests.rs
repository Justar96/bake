//! External hooks through a real session: the faux model asks for real
//! `write` calls, and real `sh` hooks block, add context, continue, time
//! out, and hang. Each case checks the files, the requests the model got,
//! and the session file, whose decision entries precede what they decide.
//! The scenarios follow 0.3's bridge tests
//! (`packages/hooks/hooks-claude-code/tests/bridge.spec.ts`,
//! `packages/hooks/hooks-codex/tests/bridge.spec.ts`).
#![cfg(unix)]

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

use bake_ai::providers::faux::{
    FauxProvider, FauxProviderOptions, FauxResponseStep, faux_assistant_blocks,
    faux_assistant_message, faux_tool_call,
};
use bake_ai::{ApiRegistry, StopReason};
use bake_sandbox::{EscalationOutcome, SandboxMode, UnavailableProvider};
use serde_json::{Value, json};

use super::*;
use crate::auth_storage::AuthStorage;
use crate::hooks::{
    CONTEXT_MESSAGE_TYPE, DECISION_ENTRY_TYPE, HookDialect, HookFile, HookScope, HooksOptions,
    parse_hooks,
};
use crate::model_registry::config::ModelConfig;
use crate::permissions::approval::ApprovalFuture;
use crate::permissions::{ApprovalPolicy, ApprovalRequest, Approver, StandingPermissions};
use crate::session::NewSessionOptions;
use crate::session::messages::convert_to_llm;
use crate::test_support::TempDir;
use crate::tools::index::{ToolsOptions, create_all_tool_definitions};

const WAIT: Duration = Duration::from_secs(20);

async fn within<T>(what: &str, future: impl Future<Output = T>) -> T {
    tokio::time::timeout(WAIT, future)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

/// Answers every approval with one outcome and counts the requests.
struct Fixed(EscalationOutcome, Arc<AtomicUsize>);

impl Approver for Fixed {
    fn request(&self, _request: ApprovalRequest, _cancel: Option<AbortSignal>) -> ApprovalFuture {
        self.1.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(self.0))
    }
}

/// Each request's user and tool-result texts, in order.
type Seen = Arc<Mutex<Vec<Vec<String>>>>;

fn texts(messages: &[Message]) -> Vec<String> {
    let blocks = |blocks: &[UserContentBlock]| -> Vec<String> {
        blocks
            .iter()
            .filter_map(|block| match block {
                UserContentBlock::Text(text) => Some(text.text.clone()),
                UserContentBlock::Image(_) => None,
            })
            .collect()
    };
    messages
        .iter()
        .flat_map(|message| match message {
            Message::User(user) => match &user.content {
                UserContent::Text(text) => vec![text.clone()],
                UserContent::Blocks(content) => blocks(content),
            },
            Message::ToolResult(result) => blocks(&result.content),
            _ => Vec::new(),
        })
        .collect()
}

struct Fixture {
    dir: TempDir,
    faux: Arc<FauxProvider>,
    registry: Arc<ModelRegistry>,
    apis: Arc<ApiRegistry>,
    seen: Seen,
    asked: Arc<AtomicUsize>,
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
            seen: Arc::default(),
            asked: Arc::default(),
        }
    }

    fn ws(&self) -> PathBuf {
        self.dir.join("ws")
    }

    /// A path for a hook's marker or record, outside the workspace.
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn new_store(&self) -> SessionManager {
        SessionManager::create(
            &self.ws().to_string_lossy(),
            Some(&self.dir.join("sessions")),
            NewSessionOptions::default(),
        )
        .expect("session")
    }

    /// A reply that records its request.
    fn reply(&self, message: bake_ai::AssistantMessage) -> FauxResponseStep {
        let seen = Arc::clone(&self.seen);
        FauxResponseStep::from_fn(move |context, _, _, _| {
            seen.lock().unwrap().push(texts(context.messages()));
            Ok(message.clone())
        })
    }

    /// The model writes `a.txt`, then answers "done".
    fn script_write(&self) {
        self.faux.append_responses(vec![
            self.reply(faux_assistant_blocks(
                vec![faux_tool_call(
                    "write",
                    json!({ "path": "a.txt", "content": "x" }),
                    Some("w1"),
                )],
                StopReason::ToolUse,
            )),
            self.reply(faux_assistant_message("done")),
        ]);
    }

    fn requests(&self) -> Vec<Vec<String>> {
        self.seen.lock().unwrap().clone()
    }

    async fn session(
        &self,
        store: SessionManager,
        files: Vec<HookFile>,
        outcome: EscalationOutcome,
    ) -> AgentSession {
        let ws = self.ws();
        let tools: Vec<Arc<AgentTool>> = create_all_tool_definitions(&ws, &ToolsOptions::default())
            .into_iter()
            .filter(|definition| definition.name() == "write")
            .map(|definition| Arc::new(definition.tool))
            .collect();
        let session = AgentSession::create(AgentSessionOptions {
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
            plugins: PluginRegistry::default(),
            work: None,
            jobs: None,
        })
        .await
        .expect("session");
        session
            .set_hooks(HooksOptions {
                files,
                scope: HookScope::Session,
                standing: Arc::new(StandingPermissions::new(
                    SandboxMode::DangerFullAccess,
                    ApprovalPolicy::Ask,
                    "test",
                    &ws,
                    None,
                )),
                sandbox: Arc::new(UnavailableProvider::new(None)),
                approver: Arc::new(Fixed(outcome, Arc::clone(&self.asked))),
                default_timeout: crate::hooks::DEFAULT_HOOK_TIMEOUT,
            })
            .expect("hooks");
        session
    }
}

fn file(dialect: HookDialect, value: Value) -> HookFile {
    HookFile {
        dialect,
        path: PathBuf::from("hooks.json"),
        groups: parse_hooks(&value, dialect).expect("hooks").groups,
    }
}

/// One `event` hook running `command`, with `matcher`.
fn one(dialect: HookDialect, event: &str, matcher: Option<&str>, hook: Value) -> HookFile {
    let mut group = json!({ "hooks": [hook] });
    if let Some(matcher) = matcher {
        group["matcher"] = matcher.into();
    }
    file(dialect, json!({ "hooks": { event: [group] } }))
}

fn command(text: &str) -> Value {
    json!({ "type": "command", "command": text })
}

/// The session's entries, from memory (a session writes its file only once
/// an assistant message exists).
fn entries(session: &AgentSession) -> Vec<Value> {
    lock(&session.session)
        .entries()
        .iter()
        .map(|entry| Value::Object(entry.as_json().clone()))
        .collect()
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
        .map(|entry| &entry["data"])
        .collect()
}

fn position(entries: &[Value], what: impl Fn(&Value) -> bool) -> usize {
    entries.iter().position(what).expect("entry")
}

fn is_decision(event: &'static str) -> impl Fn(&Value) -> bool {
    move |entry| entry["customType"] == DECISION_ENTRY_TYPE && entry["data"]["event"] == event
}

fn is_tool_result(entry: &Value) -> bool {
    entry["message"]["role"] == "toolResult"
}

/// The `toolResult` entry's text and error flag.
fn tool_result(entries: &[Value]) -> (String, bool) {
    let message = &entries[position(entries, is_tool_result)]["message"];
    let text = message["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (text, message["isError"] == true)
}

fn llm(session: &AgentSession) -> Vec<Message> {
    convert_to_llm(&session.messages())
}

// 0.3: "a UserPromptSubmit hook that exits 2 closes a blocked turn without
// a step". The prompt is refused with the hook's reason, nothing reaches
// the model, and the block is recorded; a steer is refused alike.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_user_prompt_submit_block_refuses_the_prompt() {
    let fx = Fixture::new("hooks-prompt-block");
    let session = fx
        .session(
            fx.new_store(),
            vec![one(
                HookDialect::ClaudeCode,
                "UserPromptSubmit",
                None,
                command("echo 'no secrets' >&2; exit 2"),
            )],
            EscalationOutcome::Rejected,
        )
        .await;
    let refused = session.prompt("my password is hunter2", Vec::new()).await;
    assert_eq!(
        refused,
        Err("Prompt blocked by UserPromptSubmit hook: no secrets".to_owned())
    );
    assert_eq!(fx.faux.call_count(), 0);
    let recorded = entries(&session);
    assert!(
        !recorded
            .iter()
            .any(|entry| entry["message"]["role"] == "user")
    );
    let decisions = decisions(&recorded);
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0]["event"], "UserPromptSubmit");
    assert_eq!(decisions[0]["decision"], "block");
    assert_eq!(decisions[0]["reason"], "no secrets");
    assert_eq!(decisions[0]["hooks"][0]["exitCode"], 2);
    assert_eq!(decisions[0]["hooks"][0]["dialect"], "claude-code");

    let steered = session.steer("also this", Vec::new()).await;
    assert_eq!(
        steered,
        Err("Prompt blocked by UserPromptSubmit hook: no secrets".to_owned())
    );
    assert!(session.queued_messages().is_empty());
    session.shutdown().await;
}

// 0.3: "a matching PreToolUse hook that exits 2 denies the tool (isError
// result), tool never runs"; Codex's matcher is an unanchored regex, and
// its exit 2 without a reason gives 0.3's fallback text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pre_tool_use_block_denies_the_call_with_the_text_the_model_sees() {
    for (dialect, matcher, hook, text) in [
        (
            HookDialect::ClaudeCode,
            "write",
            "echo 'writes are frozen' >&2; exit 2",
            "writes are frozen",
        ),
        (
            HookDialect::Codex,
            "rit",
            "exit 2",
            "blocked by PreToolUse hook",
        ),
    ] {
        let fx = Fixture::new("hooks-pre-tool");
        fx.script_write();
        let session = fx
            .session(
                fx.new_store(),
                vec![one(dialect, "PreToolUse", Some(matcher), command(hook))],
                EscalationOutcome::Rejected,
            )
            .await;
        session.prompt("go", Vec::new()).await.expect("prompt");
        assert!(!fx.ws().join("a.txt").exists(), "{dialect:?}: never ran");
        let recorded = file_entries(&session);
        assert_eq!(tool_result(&recorded), (text.to_owned(), true));
        let decision = position(&recorded, is_decision("PreToolUse"));
        assert!(
            decision < position(&recorded, is_tool_result),
            "{dialect:?}"
        );
        let data = &recorded[decision]["data"];
        assert_eq!(data["decision"], "deny");
        assert_eq!(data["reason"], text);
        assert_eq!(data["toolCallId"], "w1");
        assert_eq!(data["toolName"], "write");
        assert_eq!(data["hooks"][0]["matcher"], matcher);
        // The model's next request carries the denial.
        let requests = fx.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].last().map(String::as_str), Some(text));
        session.shutdown().await;
    }
    // A matcher that does not select the tool leaves it alone.
    let fx = Fixture::new("hooks-pre-tool-miss");
    fx.script_write();
    let session = fx
        .session(
            fx.new_store(),
            vec![one(
                HookDialect::ClaudeCode,
                "PreToolUse",
                Some("Write"),
                command("exit 2"),
            )],
            EscalationOutcome::Rejected,
        )
        .await;
    session.prompt("go", Vec::new()).await.expect("prompt");
    assert!(fx.ws().join("a.txt").exists());
    assert!(decisions(&file_entries(&session)).is_empty());
    session.shutdown().await;
}

// Context from SessionStart, UserPromptSubmit (Claude Code JSON and Codex
// plain output), and PostToolUse reaches the model where 0.3 put it, each
// written before it is sent; a resumed session rebuilds the same context.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn added_context_reaches_the_model_and_survives_a_resume() {
    let fx = Fixture::new("hooks-context");
    fx.script_write();
    let starts = fx.path("starts");
    let claude = file(
        HookDialect::ClaudeCode,
        json!({ "hooks": {
            "SessionStart": [{ "matcher": "startup", "hooks": [command(&format!(
                r#"echo x >> '{}'; echo '{{"hookSpecificOutput":{{"hookEventName":"SessionStart","additionalContext":"session ctx"}}}}'"#,
                starts.display()
            ))] }],
            "UserPromptSubmit": [{ "hooks": [command(
                r#"echo '{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"ctx-claude"}}'"#
            )] }],
            "PostToolUse": [{ "matcher": "write", "hooks": [command(
                r#"echo '{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"post ctx"}}'"#
            )] }],
        } }),
    );
    let codex = one(
        HookDialect::Codex,
        "UserPromptSubmit",
        None,
        command("echo ctx-codex"),
    );
    let session = fx
        .session(
            fx.new_store(),
            vec![claude, codex],
            EscalationOutcome::Rejected,
        )
        .await;
    session.prompt("hello", Vec::new()).await.expect("prompt");
    assert!(fx.ws().join("a.txt").exists());
    let requests = fx.requests();
    assert_eq!(
        requests[0],
        ["session ctx", "hello", "ctx-claude", "ctx-codex"]
    );
    // The second request repeats the first's, then the write's result,
    // then the PostToolUse context.
    assert_eq!(requests[1][..4], requests[0][..]);
    assert_eq!(requests[1].len(), 6);
    assert_eq!(requests[1][5], "post ctx");

    let recorded = file_entries(&session);
    let is_context = |text: &'static str| {
        move |entry: &Value| {
            entry["type"] == "custom_message"
                && entry["customType"] == CONTEXT_MESSAGE_TYPE
                && entry["content"][0]["text"] == text
        }
    };
    let user = position(&recorded, |entry| entry["message"]["role"] == "user");
    assert!(
        position(&recorded, is_decision("SessionStart"))
            < position(&recorded, is_context("session ctx"))
    );
    assert!(position(&recorded, is_context("session ctx")) < user);
    assert!(position(&recorded, is_decision("UserPromptSubmit")) < user);
    assert!(user < position(&recorded, is_context("ctx-claude")));
    let post = position(&recorded, is_decision("PostToolUse"));
    assert!(post < position(&recorded, is_tool_result));
    assert!(position(&recorded, is_tool_result) < position(&recorded, is_context("post ctx")));
    let data = decisions(&recorded);
    assert_eq!(data[0]["decision"], "context");
    assert_eq!(data[0]["source"], "startup");
    assert_eq!(data[1]["decision"], "context");
    assert_eq!(data[1]["hooks"].as_array().map(Vec::len), Some(2));
    assert_eq!(data[1]["hooks"][1]["dialect"], "codex");

    // SessionStart runs once per session.
    fx.faux
        .append_responses(vec![fx.reply(faux_assistant_message("again"))]);
    session.prompt("more", Vec::new()).await.expect("prompt");
    assert_eq!(std::fs::read_to_string(&starts).expect("starts"), "x\n");

    let live = llm(&session);
    let path = session.session_file().expect("file");
    session.shutdown().await;
    let resumed = fx
        .session(
            SessionManager::open(&path, None, None).expect("open"),
            Vec::new(),
            EscalationOutcome::Rejected,
        )
        .await;
    // The same messages and text; a custom message's time is its entry's.
    let resumed_llm = llm(&resumed);
    assert_eq!(texts(&resumed_llm), texts(&live));
    assert_eq!(
        resumed_llm.iter().map(Message::role).collect::<Vec<_>>(),
        live.iter().map(Message::role).collect::<Vec<_>>()
    );
    resumed.shutdown().await;
}

// 0.3: "a Stop hook (exit 2) forces the turn to continue with the reason
// as steering"; the second Stop sees `stop_hook_active` and lets the run
// settle. A `continue: false` overrides a Stop block.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_block_continues_the_run_until_the_hook_lets_it_stop() {
    let fx = Fixture::new("hooks-stop");
    let marker = fx.path("stopped-once");
    let payloads = fx.path("payloads");
    let hook = format!(
        "cat >> '{payloads}'; echo >> '{payloads}'; if [ -e '{marker}' ]; then exit 0; fi; touch '{marker}'; echo 'keep going' >&2; exit 2",
        payloads = payloads.display(),
        marker = marker.display(),
    );
    fx.faux.append_responses(vec![
        fx.reply(faux_assistant_message("first")),
        fx.reply(faux_assistant_message("second")),
    ]);
    let session = fx
        .session(
            fx.new_store(),
            vec![one(HookDialect::Codex, "Stop", None, command(&hook))],
            EscalationOutcome::Rejected,
        )
        .await;
    session.prompt("go", Vec::new()).await.expect("prompt");
    let requests = fx.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].last().map(String::as_str), Some("keep going"));
    let seen: Vec<Value> = std::fs::read_to_string(&payloads)
        .expect("payloads")
        .lines()
        .map(|line| serde_json::from_str(line).expect("payload"))
        .collect();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0]["hook_event_name"], "Stop");
    assert_eq!(seen[0]["stop_hook_active"], false);
    assert_eq!(seen[1]["stop_hook_active"], true);
    assert_eq!(seen[0]["last_assistant_message"], Value::Null);

    let recorded = file_entries(&session);
    let first = position(&recorded, is_decision("Stop"));
    assert_eq!(recorded[first]["data"]["decision"], "continue");
    assert_eq!(recorded[first]["data"]["reason"], "keep going");
    assert_eq!(recorded[first + 1]["customType"], CONTEXT_MESSAGE_TYPE);
    let data = decisions(&recorded);
    assert_eq!(data.len(), 2);
    assert_eq!(data[1]["decision"], "pass");
    assert_eq!(data[1]["stopHookActive"], true);
    session.shutdown().await;

    let fx = Fixture::new("hooks-stop-overridden");
    fx.faux
        .append_responses(vec![fx.reply(faux_assistant_message("only"))]);
    let session = fx
        .session(
            fx.new_store(),
            vec![one(
                HookDialect::ClaudeCode,
                "Stop",
                None,
                command(r#"echo '{"decision":"block","reason":"more","continue":false}'"#),
            )],
            EscalationOutcome::Rejected,
        )
        .await;
    session.prompt("go", Vec::new()).await.expect("prompt");
    assert_eq!(fx.faux.call_count(), 1);
    let data = file_entries(&session);
    let data = decisions(&data);
    assert_eq!(data[0]["decision"], "pass");
    assert_eq!(data[0]["hooks"][0]["decision"], "block");
    session.shutdown().await;
}

// A Claude Code PreToolUse `continue: false` denies the call with its stop
// reason and ends the run after the batch; a PostToolUse exit 2 replaces
// the result with its feedback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_tool_hooks_halt_the_run_and_replace_results() {
    let fx = Fixture::new("hooks-halt");
    fx.script_write();
    let session = fx
        .session(
            fx.new_store(),
            vec![one(
                HookDialect::ClaudeCode,
                "PreToolUse",
                None,
                command(r#"echo '{"continue":false,"stopReason":"budget spent"}'"#),
            )],
            EscalationOutcome::Rejected,
        )
        .await;
    session.prompt("go", Vec::new()).await.expect("prompt");
    assert_eq!(fx.faux.call_count(), 1, "no request after the halt");
    assert!(!fx.ws().join("a.txt").exists());
    let recorded = file_entries(&session);
    assert_eq!(tool_result(&recorded), ("budget spent".to_owned(), true));
    let data = decisions(&recorded);
    assert_eq!(data[0]["decision"], "deny");
    assert_eq!(data[0]["halt"], true);
    session.shutdown().await;

    let fx = Fixture::new("hooks-post-block");
    fx.script_write();
    let session = fx
        .session(
            fx.new_store(),
            vec![one(
                HookDialect::ClaudeCode,
                "PostToolUse",
                Some("write"),
                command("echo 'bad output' >&2; exit 2"),
            )],
            EscalationOutcome::Rejected,
        )
        .await;
    session.prompt("go", Vec::new()).await.expect("prompt");
    assert!(fx.ws().join("a.txt").exists(), "the tool ran");
    let recorded = file_entries(&session);
    assert_eq!(tool_result(&recorded), ("bad output".to_owned(), true));
    assert!(position(&recorded, is_decision("PostToolUse")) < position(&recorded, is_tool_result));
    assert_eq!(
        fx.requests()[1].last().map(String::as_str),
        Some("bad output")
    );
    session.shutdown().await;
}

// 0.3: "a PreToolUse permissionDecision:ask fails closed without an
// approval service"; here it asks the session's approver, and only an
// approval lets the call run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claude_ask_runs_the_call_only_when_approved() {
    let ask = command(
        r#"echo '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask","permissionDecisionReason":"prod"}}'"#,
    );
    for (outcome, ran, decision) in [
        (EscalationOutcome::Rejected, false, "deny"),
        (EscalationOutcome::Unavailable, false, "deny"),
        (EscalationOutcome::AllowedOnce, true, "allow"),
    ] {
        let fx = Fixture::new("hooks-ask");
        fx.script_write();
        let session = fx
            .session(
                fx.new_store(),
                vec![one(
                    HookDialect::ClaudeCode,
                    "PreToolUse",
                    None,
                    ask.clone(),
                )],
                outcome,
            )
            .await;
        session.prompt("go", Vec::new()).await.expect("prompt");
        assert_eq!(fx.asked.load(Ordering::SeqCst), 1);
        assert_eq!(fx.ws().join("a.txt").exists(), ran, "{outcome:?}");
        let recorded = file_entries(&session);
        let data = decisions(&recorded);
        assert_eq!(data[0]["decision"], decision);
        assert_eq!(data[0]["approval"], outcome.as_str());
        if !ran {
            let (text, error) = tool_result(&recorded);
            assert!(error && text.ends_with(": prod"), "{text}");
        }
        session.shutdown().await;
    }
}

// A hook past its timeout is killed and is a non-blocking error: the call
// runs, and the record says it timed out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_timed_out_hook_is_killed_and_does_not_block() {
    let fx = Fixture::new("hooks-timeout");
    fx.script_write();
    let session = fx
        .session(
            fx.new_store(),
            vec![one(
                HookDialect::ClaudeCode,
                "PreToolUse",
                None,
                json!({ "type": "command", "command": "exec sleep 30", "timeout": 0.3 }),
            )],
            EscalationOutcome::Rejected,
        )
        .await;
    let started = Instant::now();
    session.prompt("go", Vec::new()).await.expect("prompt");
    assert!(started.elapsed() < Duration::from_secs(15));
    assert!(fx.ws().join("a.txt").exists());
    let recorded = file_entries(&session);
    let data = decisions(&recorded);
    assert_eq!(data[0]["decision"], "pass");
    assert_eq!(data[0]["hooks"][0]["timedOut"], true);
    assert!(data[0]["hooks"][0].get("exitCode").is_none());
    session.shutdown().await;
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

async fn wait_for_pid(path: &Path) -> i32 {
    within("the hook to start", async {
        loop {
            if let Some(pid) = std::fs::read_to_string(path)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
}

// Shutdown kills a hook that is still running and returns only once the
// hook is gone; the call it was deciding never runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_awaits_a_slow_hook() {
    let fx = Fixture::new("hooks-shutdown");
    fx.script_write();
    let pid_file = fx.path("pid");
    let session = Arc::new(
        fx.session(
            fx.new_store(),
            vec![one(
                HookDialect::ClaudeCode,
                "PreToolUse",
                None,
                command(&format!(
                    "echo $$ > '{}'; exec sleep 60",
                    pid_file.display()
                )),
            )],
            EscalationOutcome::Rejected,
        )
        .await,
    );
    let prompt = tokio::spawn({
        let session = Arc::clone(&session);
        async move { session.prompt("go", Vec::new()).await }
    });
    let pid = wait_for_pid(&pid_file).await;
    assert!(alive(pid));
    let started = Instant::now();
    within("shutdown", session.shutdown()).await;
    assert!(!alive(pid), "the hook is gone when shutdown returns");
    assert!(started.elapsed() < Duration::from_secs(15));
    let _ = within("the prompt", prompt).await;
    assert!(!fx.ws().join("a.txt").exists());

    // Esc does the same to a prompt's hook: the prompt ends cancelled.
    let fx = Fixture::new("hooks-abort");
    let pid_file = fx.path("pid");
    let session = Arc::new(
        fx.session(
            fx.new_store(),
            vec![one(
                HookDialect::Codex,
                "UserPromptSubmit",
                None,
                command(&format!(
                    "echo $$ > '{}'; exec sleep 60",
                    pid_file.display()
                )),
            )],
            EscalationOutcome::Rejected,
        )
        .await,
    );
    let prompt = tokio::spawn({
        let session = Arc::clone(&session);
        async move { session.prompt("go", Vec::new()).await }
    });
    let pid = wait_for_pid(&pid_file).await;
    within("abort", session.abort()).await;
    assert_eq!(within("the prompt", prompt).await.expect("join"), Ok(()));
    assert!(!alive(pid));
    assert_eq!(fx.faux.call_count(), 0);
    assert!(decisions(&entries(&session)).is_empty());
    session.shutdown().await;
}
