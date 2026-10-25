//! Nested tool calls through a real session: the faux model calls `script`,
//! a test tool that runs other tools through its `ToolContext::nested`
//! executor, as the `codemode` tool will. Each nested call meets the
//! session's whole pipeline: Bake's D24 `write` and `bash` under each
//! permission (allowed, refused, an approval answered yes and no, under
//! `never`, and aborted while pending), the plugins' vetoes, the external
//! hooks before and after the call (a deny, an ask, and a `PostToolUse`
//! feedback), the file mutation queue (a nested write waits behind a held
//! queue, after the batch's direct write to the same file), and the
//! session's work gate (shutdown during a nested write waits for its
//! worker, and the write is refused).

use std::path::PathBuf;
use std::time::Duration;

use bake_agent::{AgentToolResult, NestedToolCallOptions};
use bake_ai::StopReason;
use bake_ai::providers::faux::{faux_assistant_blocks, faux_assistant_message, faux_tool_call};
use bake_sandbox::testing::RecordingProvider;
use bake_sandbox::{EscalationOutcome, SandboxMode, SandboxProvider};
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::tests::{Harness, harness, options};
use super::*;
use crate::owned_work::WorkGate;
use crate::permissions::approval::ApprovalFuture;
use crate::permissions::{
    APPROVAL_CUSTOM_TYPE, ApprovalPolicy, ApprovalRequest, Approver, StandingPermissions,
    ToolPermissions, owned_sandbox_file_tools, sandboxed_bash_tool,
};
use crate::plugins::tests::{Scripted, block};
use crate::session::NewSessionOptions;
use crate::test_support::TempDir;
use crate::tools::bash::BashToolOptions;
use crate::tools::index::{ToolsOptions, create_all_tool_definitions};

const WAIT: Duration = Duration::from_secs(20);

/// A tool that runs `calls` (`{ name, args }`) one after another through
/// its nested-call executor and returns each outcome's id, error flag, and
/// text as JSON.
pub(crate) fn script_tool() -> AgentTool {
    AgentTool::new(
        "script",
        "script",
        "Runs other tools.",
        json!({
            "type": "object",
            "properties": { "calls": { "type": "array" } },
            "required": ["calls"],
        }),
        |_id, args, _signal, _update, ctx| {
            Box::pin(async move {
                let mut outcomes = Vec::new();
                for call in args["calls"].as_array().cloned().unwrap_or_default() {
                    let outcome = ctx
                        .nested()
                        .execute(
                            call["name"].as_str().unwrap_or_default(),
                            call["args"].clone(),
                            NestedToolCallOptions::default(),
                        )
                        .await;
                    outcomes.push(json!({
                        "id": outcome.tool_call.id,
                        "isError": outcome.is_error,
                        "text": text(&outcome.result.content),
                    }));
                }
                Ok(AgentToolResult::text(Value::Array(outcomes).to_string()))
            })
        },
    )
}

fn text(content: &[UserContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// How the fixture's answerer answers: at once, or never (until the
/// request is withdrawn).
#[derive(Clone, Copy)]
enum Answer {
    Now(EscalationOutcome),
    Pending,
}

/// Records each approval request, then answers.
struct Answerer {
    answer: Answer,
    asked: Arc<Mutex<Vec<ApprovalRequest>>>,
    notify: Arc<Notify>,
}

impl Approver for Answerer {
    fn request(&self, request: ApprovalRequest, _cancel: Option<AbortSignal>) -> ApprovalFuture {
        self.asked.lock().unwrap().push(request);
        self.notify.notify_one();
        match self.answer {
            Answer::Now(outcome) => Box::pin(std::future::ready(outcome)),
            Answer::Pending => Box::pin(std::future::pending()),
        }
    }
}

struct Fixture {
    dir: TempDir,
    h: Harness,
    asked: Arc<Mutex<Vec<ApprovalRequest>>>,
    notify: Arc<Notify>,
    provider: Arc<RecordingProvider>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = TempDir::new(name);
        std::fs::create_dir_all(dir.join("ws")).expect("ws");
        Self {
            dir,
            h: harness(),
            asked: Arc::default(),
            notify: Arc::default(),
            provider: Arc::default(),
        }
    }

    fn ws(&self) -> PathBuf {
        self.dir.join("ws")
    }

    /// The model calls `script` once with `calls`, then answers "done".
    fn script(&self, calls: Value) {
        self.h.faux.append_responses(vec![
            faux_assistant_blocks(
                vec![faux_tool_call(
                    "script",
                    json!({ "calls": calls }),
                    Some("c1"),
                )],
                StopReason::ToolUse,
            )
            .into(),
            faux_assistant_message("done").into(),
        ]);
    }

    /// A persisted session whose tools are `script` and Bake's D24 `write`
    /// and `bash` standing in `mode` under `policy`, all under the
    /// session's work gate, with `plugins` vetoing.
    async fn session(
        &self,
        mode: SandboxMode,
        policy: ApprovalPolicy,
        answer: Answer,
        plugins: PluginRegistry,
    ) -> AgentSession {
        let ws = self.ws();
        let standing = StandingPermissions::new(mode, policy, "test", &ws, None);
        let permissions = ToolPermissions::new(
            Arc::new(standing),
            Arc::new(Answerer {
                answer,
                asked: Arc::clone(&self.asked),
                notify: Arc::clone(&self.notify),
            }),
            Arc::clone(&self.provider) as Arc<dyn SandboxProvider>,
        );
        let work = WorkGate::default();
        let mut tools = vec![Arc::new(script_tool())];
        let bash = sandboxed_bash_tool(
            &ws,
            BashToolOptions {
                spill_directory: Some(self.dir.path().to_path_buf()),
                owner: Some(work.clone()),
                ..BashToolOptions::default()
            },
            &permissions,
        )
        .expect("bash");
        tools.push(Arc::new(bash.tool));
        tools.extend(
            owned_sandbox_file_tools(
                create_all_tool_definitions(&ws, &ToolsOptions::default())
                    .into_iter()
                    .filter(|definition| definition.name() == "write")
                    .collect(),
                &ws,
                &permissions,
                Some(work.clone()),
            )
            .into_iter()
            .map(|definition| Arc::new(definition.tool)),
        );
        let store = SessionManager::create(
            &ws.to_string_lossy(),
            Some(&self.dir.join("sessions")),
            NewSessionOptions::default(),
        )
        .expect("store");
        let mut options = options(&self.h, store, &ws.to_string_lossy());
        options.active_tool_names = tools.iter().map(|tool| tool.name.clone()).collect();
        options.tools = tools;
        options.plugins = plugins;
        options.work = Some(work);
        let session = AgentSession::create(options).await.expect("session");
        session
            .record_approvals(permissions.engine.recorder_slot())
            .expect("record approvals");
        session
    }

    fn asked_ids(&self) -> Vec<String> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.call_id.clone())
            .collect()
    }

    /// The calling tool call each approval request names.
    fn asked_parents(&self) -> Vec<Option<String>> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.parent_tool_call_id.clone())
            .collect()
    }
}

/// The outcomes `script` reported, as `(id, is_error, text)`.
fn outcomes(session: &AgentSession) -> Vec<(String, bool, String)> {
    let messages = session.messages();
    let result = messages
        .iter()
        .find_map(|message| message.as_tool_result().filter(|r| r.tool_call_id == "c1"))
        .expect("script's result");
    let reported: Value = serde_json::from_str(&text(&result.content)).expect("outcomes");
    reported
        .as_array()
        .unwrap()
        .iter()
        .map(|outcome| {
            (
                outcome["id"].as_str().unwrap().to_owned(),
                outcome["isError"].as_bool().unwrap(),
                outcome["text"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// The session file's entries.
fn file_entries(session: &AgentSession) -> Vec<Value> {
    std::fs::read_to_string(session.session_file().expect("persisted"))
        .expect("session file")
        .lines()
        .map(|line| serde_json::from_str(line).expect("json line"))
        .collect()
}

/// The approval entries among `entries`: each request's kind and call id,
/// each answer's kind and outcome.
fn approval_entries(entries: &[Value]) -> Vec<(String, String)> {
    entries
        .iter()
        .filter(|entry| entry["customType"] == APPROVAL_CUSTOM_TYPE)
        .map(|entry| {
            let data = &entry["data"];
            let kind = data["kind"].as_str().unwrap_or_default().to_owned();
            let fact = if kind == "asked" {
                &data["callId"]
            } else {
                &data["outcome"]
            };
            (kind, fact.as_str().unwrap_or_default().to_owned())
        })
        .collect()
}

fn write(path: &str) -> Value {
    json!({ "name": "write", "args": { "path": path, "content": "x" } })
}

fn escalated_write(path: &str) -> Value {
    json!({ "name": "write", "args": {
        "path": path,
        "content": "x",
        "sandbox_permissions": "workspace-write",
        "justification": "the script writes its output",
    } })
}

fn denial(mode: &str) -> String {
    format!(
        "[sandbox: file access denied under {mode} mode]\n[sandbox: escalation available — retry this exact operation once with sandbox_permissions (the narrowest wider mode that suffices) + justification; the approval prompt asks the user]"
    )
}

fn refusal(outcome: EscalationOutcome) -> String {
    outcome
        .refusal("operation", SandboxMode::WorkspaceWrite)
        .expect("a refusal")
}

async fn prompt(session: &AgentSession) {
    tokio::time::timeout(WAIT, session.prompt("go", Vec::new()))
        .await
        .expect("prompt settles")
        .expect("prompt");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nested_writes_meet_each_permission_and_approval_answer() {
    struct Case {
        name: &'static str,
        mode: SandboxMode,
        policy: ApprovalPolicy,
        answer: Answer,
        call: Value,
        /// Whether the answerer is asked.
        asked: bool,
        /// Whether a request and its answer are recorded.
        recorded: bool,
        result: Result<(), String>,
    }
    let wrote = Ok(());
    let cases = [
        Case {
            name: "danger-full-access",
            mode: SandboxMode::DangerFullAccess,
            policy: ApprovalPolicy::Ask,
            answer: Answer::Now(EscalationOutcome::Rejected),
            call: write("a.txt"),
            asked: false,
            recorded: false,
            result: wrote.clone(),
        },
        Case {
            name: "workspace-write",
            mode: SandboxMode::WorkspaceWrite,
            policy: ApprovalPolicy::Ask,
            answer: Answer::Now(EscalationOutcome::Rejected),
            call: write("a.txt"),
            asked: false,
            recorded: false,
            result: wrote.clone(),
        },
        Case {
            name: "read-only refuses",
            mode: SandboxMode::ReadOnly,
            policy: ApprovalPolicy::Ask,
            answer: Answer::Now(EscalationOutcome::AllowedOnce),
            call: write("a.txt"),
            asked: false,
            recorded: false,
            result: Err(denial("read-only")),
        },
        Case {
            name: "read-only, approved",
            mode: SandboxMode::ReadOnly,
            policy: ApprovalPolicy::Ask,
            answer: Answer::Now(EscalationOutcome::AllowedOnce),
            call: escalated_write("a.txt"),
            asked: true,
            recorded: true,
            result: wrote.clone(),
        },
        Case {
            name: "read-only, rejected",
            mode: SandboxMode::ReadOnly,
            policy: ApprovalPolicy::Ask,
            answer: Answer::Now(EscalationOutcome::Rejected),
            call: escalated_write("a.txt"),
            asked: true,
            recorded: true,
            result: Err(refusal(EscalationOutcome::Rejected)),
        },
        Case {
            name: "read-only under never",
            mode: SandboxMode::ReadOnly,
            policy: ApprovalPolicy::Never,
            answer: Answer::Now(EscalationOutcome::AllowedOnce),
            call: escalated_write("a.txt"),
            asked: false,
            recorded: true,
            result: Err(refusal(EscalationOutcome::Rejected)),
        },
    ];
    for case in cases {
        let fx = Fixture::new("nested-permissions");
        fx.script(json!([case.call]));
        let session = fx
            .session(
                case.mode,
                case.policy,
                case.answer,
                PluginRegistry::default(),
            )
            .await;
        prompt(&session).await;
        let outcomes = outcomes(&session);
        let entries = file_entries(&session);
        session.shutdown().await;
        let name = case.name;
        assert_eq!(outcomes.len(), 1, "{name}");
        let (id, is_error, text) = &outcomes[0];
        assert_eq!(id, "c1/1", "{name}");
        match &case.result {
            Ok(()) => {
                assert!(!is_error, "{name}: {text}");
                assert_eq!(
                    std::fs::read_to_string(fx.ws().join("a.txt"))
                        .ok()
                        .as_deref(),
                    Some("x"),
                    "{name}"
                );
            }
            Err(expected) => {
                assert!(is_error, "{name}");
                assert_eq!(text, expected, "{name}");
                assert!(!fx.ws().join("a.txt").exists(), "{name}");
            }
        }
        // The prompt names the nested call and the script that made it,
        // and the request and its answer are on disk under the call's id.
        let expected_asks: Vec<String> = if case.asked {
            vec!["c1/1".into()]
        } else {
            Vec::new()
        };
        assert_eq!(fx.asked_ids(), expected_asks, "{name}");
        let expected_parents: Vec<Option<String>> = if case.asked {
            vec![Some("c1".into())]
        } else {
            Vec::new()
        };
        assert_eq!(fx.asked_parents(), expected_parents, "{name}");
        let recorded = if case.recorded {
            let outcome = if case.result.is_ok() {
                EscalationOutcome::AllowedOnce
            } else {
                EscalationOutcome::Rejected
            };
            vec![
                ("asked".to_owned(), "c1/1".to_owned()),
                ("decided".to_owned(), outcome.as_str().to_owned()),
            ]
        } else {
            Vec::new()
        };
        assert_eq!(approval_entries(&entries), recorded, "{name}");
        // Only the model's call is in the transcript; its result records
        // the nested call.
        let results: Vec<_> = entries
            .iter()
            .filter(|entry| entry["message"]["role"] == "toolResult")
            .collect();
        assert_eq!(results.len(), 1, "{name}");
        let nested = &results[0]["message"]["nestedCalls"];
        assert_eq!(nested["calls"][0]["id"], "c1/1", "{name}");
        assert_eq!(
            nested["calls"][0]["status"],
            if case.result.is_ok() { "ok" } else { "error" },
            "{name}"
        );
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nested_bash_runs_under_the_sandbox_and_its_approvals() {
    let bash = |command: &str| json!({ "name": "bash", "args": { "command": command } });
    let escalated = json!({ "name": "bash", "args": {
        "command": "printf escalated",
        "sandbox_permissions": "workspace-write",
        "justification": "the script needs it",
    } });
    // Full access runs unconfined; workspace-write confines through the
    // provider; an approved escalation confines to the granted mode, and a
    // rejected one runs nothing.
    let cases = [
        (
            SandboxMode::DangerFullAccess,
            EscalationOutcome::Rejected,
            bash("printf ok"),
            Ok("ok"),
            vec![],
        ),
        (
            SandboxMode::WorkspaceWrite,
            EscalationOutcome::Rejected,
            bash("printf ok"),
            Ok("ok"),
            vec![SandboxMode::WorkspaceWrite],
        ),
        (
            SandboxMode::ReadOnly,
            EscalationOutcome::AllowedOnce,
            escalated.clone(),
            Ok("escalated"),
            vec![SandboxMode::WorkspaceWrite],
        ),
        (
            SandboxMode::ReadOnly,
            EscalationOutcome::Rejected,
            escalated,
            Err(()),
            vec![],
        ),
    ];
    for (mode, answer, call, expected, confined) in cases {
        if crate::tools::bash::shell::get_shell_config(None).is_err() {
            return;
        }
        let fx = Fixture::new("nested-bash");
        fx.script(json!([call]));
        let session = fx
            .session(
                mode,
                ApprovalPolicy::Ask,
                Answer::Now(answer),
                PluginRegistry::default(),
            )
            .await;
        prompt(&session).await;
        let outcomes = outcomes(&session);
        session.shutdown().await;
        let (id, is_error, text) = &outcomes[0];
        assert_eq!(id, "c1/1");
        match expected {
            Ok(output) => {
                assert!(!is_error, "{mode:?}: {text}");
                assert_eq!(text, output);
            }
            Err(()) => {
                assert!(is_error);
                assert_eq!(fx.asked_ids(), ["c1/1"]);
            }
        }
        assert_eq!(fx.provider.modes(), confined, "{mode:?}");
    }
}

/// An approval still pending when the run is aborted is withdrawn: the
/// nested call is refused as cancelled, nothing is written, and the caller's
/// result waits for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborting_while_a_nested_approval_is_pending_cancels_it() {
    let fx = Fixture::new("nested-pending");
    fx.script(json!([escalated_write("a.txt")]));
    let session = Arc::new(
        fx.session(
            SandboxMode::ReadOnly,
            ApprovalPolicy::Ask,
            Answer::Pending,
            PluginRegistry::default(),
        )
        .await,
    );
    let notified = fx.notify.notified();
    let running = tokio::spawn({
        let session = Arc::clone(&session);
        async move { session.prompt("go", Vec::new()).await }
    });
    tokio::time::timeout(WAIT, notified)
        .await
        .expect("the approval is asked");
    assert_eq!(fx.asked_ids(), ["c1/1"]);
    session.abort().await;
    let _ = tokio::time::timeout(WAIT, running)
        .await
        .expect("prompt settles");
    let entries = file_entries(&session);
    let outcomes = outcomes(&session);
    session.shutdown().await;
    assert_eq!(
        outcomes,
        [(
            "c1/1".to_owned(),
            true,
            refusal(EscalationOutcome::Cancelled)
        )]
    );
    assert!(!fx.ws().join("a.txt").exists());
    assert_eq!(
        approval_entries(&entries),
        [
            ("asked".to_owned(), "c1/1".to_owned()),
            (
                "decided".to_owned(),
                EscalationOutcome::Cancelled.as_str().to_owned()
            ),
        ]
    );
}

/// A plugin's veto applies to a nested call by its own id; the caller is
/// not one of the tools it watches.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugin_vetoes_a_nested_call() {
    let fx = Fixture::new("nested-veto");
    fx.script(json!([write("a.txt")]));
    let guard = Arc::new(Scripted::new(
        "guard",
        &["write"],
        block("no writes from scripts"),
    ));
    let mut plugins = PluginRegistry::new();
    plugins
        .register(Arc::clone(&guard) as Arc<dyn crate::plugins::Plugin>, 0)
        .expect("register");
    let session = fx
        .session(
            SandboxMode::WorkspaceWrite,
            ApprovalPolicy::Ask,
            Answer::Now(EscalationOutcome::Rejected),
            plugins,
        )
        .await;
    prompt(&session).await;
    let outcomes = outcomes(&session);
    let entries = file_entries(&session);
    session.shutdown().await;
    assert_eq!(guard.calls(), ["write:c1/1"]);
    assert_eq!(
        outcomes,
        [("c1/1".to_owned(), true, "no writes from scripts".to_owned())]
    );
    assert!(!fx.ws().join("a.txt").exists());
    let decision = entries
        .iter()
        .find(|entry| entry["customType"] == crate::plugins::DECISION_ENTRY_TYPE)
        .expect("the veto is recorded");
    assert_eq!(decision["data"]["toolCallId"], "c1/1");
    assert_eq!(decision["data"]["plugin"], "guard");
}

/// Installs `hooks` (a hooks file of `dialect`) on `session`, asking
/// `approver` for a hook's `ask`.
#[cfg(unix)]
fn install_hooks(
    session: &AgentSession,
    fx: &Fixture,
    dialect: crate::hooks::HookDialect,
    hooks: &Value,
    approver: Arc<dyn Approver>,
) {
    use crate::hooks::{HookFile, HookScope, HooksOptions, parse_hooks};
    session
        .set_hooks(HooksOptions {
            files: vec![HookFile {
                dialect,
                path: PathBuf::from("hooks.json"),
                groups: parse_hooks(hooks, dialect).expect("hooks").groups,
            }],
            scope: HookScope::Session,
            standing: Arc::new(StandingPermissions::new(
                SandboxMode::DangerFullAccess,
                ApprovalPolicy::Ask,
                "test",
                fx.ws(),
                None,
            )),
            sandbox: Arc::new(bake_sandbox::UnavailableProvider::new(None)),
            approver,
            default_timeout: crate::hooks::DEFAULT_HOOK_TIMEOUT,
        })
        .expect("hooks");
}

/// Claude Code and Codex `PreToolUse` hooks see a nested call by its own
/// id; a deny refuses it and is recorded with the hook's handler id.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hooks_deny_a_nested_call_and_record_it() {
    use crate::hooks::HookDialect;
    for dialect in [HookDialect::ClaudeCode, HookDialect::Codex] {
        let fx = Fixture::new("nested-hooks");
        fx.script(json!([write("a.txt")]));
        let session = fx
            .session(
                SandboxMode::WorkspaceWrite,
                ApprovalPolicy::Ask,
                Answer::Now(EscalationOutcome::Rejected),
                PluginRegistry::default(),
            )
            .await;
        // As 0.3's bridge tests: Claude Code reports the hook's stderr,
        // Codex its own text.
        let (command, expected) = match dialect {
            HookDialect::ClaudeCode => ("echo 'no nested writes' >&2; exit 2", "no nested writes"),
            HookDialect::Codex => ("exit 2", "blocked by PreToolUse hook"),
        };
        let hooks = json!({ "hooks": { "PreToolUse": [{
            "matcher": "write",
            "hooks": [{ "type": "command", "command": command }],
        }] } });
        install_hooks(
            &session,
            &fx,
            dialect,
            &hooks,
            Arc::new(crate::permissions::NoApprover),
        );
        prompt(&session).await;
        let outcomes = outcomes(&session);
        let entries = file_entries(&session);
        session.shutdown().await;
        assert_eq!(
            outcomes,
            [("c1/1".to_owned(), true, expected.to_owned())],
            "{dialect:?}"
        );
        assert!(!fx.ws().join("a.txt").exists());
        let decisions: Vec<_> = entries
            .iter()
            .filter(|entry| entry["customType"] == crate::hooks::DECISION_ENTRY_TYPE)
            .map(|entry| &entry["data"])
            .collect();
        // The `script` call matched no hook; the nested `write` did.
        assert_eq!(decisions.len(), 1, "{dialect:?}");
        assert_eq!(decisions[0]["toolCallId"], "c1/1");
        assert_eq!(decisions[0]["decision"], "deny");
        assert!(decisions[0]["hooks"][0]["handlerId"].is_string());
    }
}

/// A `PostToolUse` hook sees a nested call by its own id after it ran, and
/// its feedback replaces the nested result the script gets; a `PreToolUse`
/// ask about a nested call names the script that made it.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hooks_after_and_asking_about_a_nested_call() {
    use crate::hooks::HookDialect;
    let fx = Fixture::new("nested-post-hook");
    fx.script(json!([write("a.txt")]));
    let session = fx
        .session(
            SandboxMode::WorkspaceWrite,
            ApprovalPolicy::Ask,
            Answer::Now(EscalationOutcome::Rejected),
            PluginRegistry::default(),
        )
        .await;
    let hooks = json!({ "hooks": { "PostToolUse": [{
        "matcher": "write",
        "hooks": [{ "type": "command", "command": "echo 'bad output' >&2; exit 2" }],
    }] } });
    install_hooks(
        &session,
        &fx,
        HookDialect::ClaudeCode,
        &hooks,
        Arc::new(crate::permissions::NoApprover),
    );
    prompt(&session).await;
    let reported = outcomes(&session);
    let entries = file_entries(&session);
    session.shutdown().await;
    assert!(fx.ws().join("a.txt").exists(), "the nested write ran");
    assert_eq!(
        reported,
        [("c1/1".to_owned(), true, "bad output".to_owned())]
    );
    let decisions: Vec<_> = entries
        .iter()
        .filter(|entry| entry["customType"] == crate::hooks::DECISION_ENTRY_TYPE)
        .map(|entry| &entry["data"])
        .collect();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0]["event"], "PostToolUse");
    assert_eq!(decisions[0]["toolCallId"], "c1/1");
    assert_eq!(decisions[0]["decision"], "block");

    let fx = Fixture::new("nested-ask-hook");
    fx.script(json!([write("a.txt")]));
    let session = fx
        .session(
            SandboxMode::WorkspaceWrite,
            ApprovalPolicy::Ask,
            Answer::Now(EscalationOutcome::Rejected),
            PluginRegistry::default(),
        )
        .await;
    let hooks = json!({ "hooks": { "PreToolUse": [{
        "matcher": "write",
        "hooks": [{ "type": "command", "command": r#"echo '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask","permissionDecisionReason":"scripted write"}}'"# }],
    }] } });
    install_hooks(
        &session,
        &fx,
        HookDialect::ClaudeCode,
        &hooks,
        Arc::new(Answerer {
            answer: Answer::Now(EscalationOutcome::Rejected),
            asked: Arc::clone(&fx.asked),
            notify: Arc::clone(&fx.notify),
        }),
    );
    prompt(&session).await;
    let reported = outcomes(&session);
    session.shutdown().await;
    assert!(!fx.ws().join("a.txt").exists());
    assert!(reported[0].1, "{reported:?}");
    assert_eq!(fx.asked_ids(), ["c1/1"]);
    assert_eq!(fx.asked_parents(), [Some("c1".to_owned())]);
}

/// Holds `path`'s place in the process-wide file mutation queue until the
/// returned sender is dropped; `held` resolves once it is held.
fn hold_mutation_queue(
    path: PathBuf,
) -> (
    std::sync::mpsc::Sender<()>,
    tokio::sync::oneshot::Receiver<()>,
    std::thread::JoinHandle<()>,
) {
    let (release, released) = std::sync::mpsc::channel::<()>();
    let (entered, held) = tokio::sync::oneshot::channel();
    let holder = std::thread::spawn(move || {
        let _ = crate::tools::file_mutation_queue::with_file_mutation_queue(&path, || {
            let _ = entered.send(());
            let _ = released.recv();
            Ok(())
        });
    });
    (release, held, holder)
}

/// A nested write takes its place in the file mutation queue as a direct
/// write does: with the file's queue held elsewhere, neither the batch's
/// direct write nor the script's nested write to the same file runs; once
/// released, they write in the batch's order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_nested_write_waits_in_the_file_mutation_queue_behind_a_direct_write() {
    let fx = Fixture::new("nested-mutation-queue");
    let target = fx.ws().join("a.txt");
    fx.h.faux.append_responses(vec![
        faux_assistant_blocks(
            vec![
                faux_tool_call(
                    "write",
                    json!({ "path": "a.txt", "content": "direct" }),
                    Some("c0"),
                ),
                faux_tool_call(
                    "script",
                    json!({ "calls": [{ "name": "write", "args": { "path": "a.txt", "content": "nested" } }] }),
                    Some("c1"),
                ),
            ],
            StopReason::ToolUse,
        )
        .into(),
        faux_assistant_message("done").into(),
    ]);
    let session = Arc::new(
        fx.session(
            SandboxMode::WorkspaceWrite,
            ApprovalPolicy::Ask,
            Answer::Now(EscalationOutcome::Rejected),
            PluginRegistry::default(),
        )
        .await,
    );
    let (release, held, holder) = hold_mutation_queue(target.clone());
    tokio::time::timeout(WAIT, held)
        .await
        .expect("the queue is held")
        .expect("holder");
    let running = tokio::spawn({
        let session = Arc::clone(&session);
        async move { session.prompt("go", Vec::new()).await }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!target.exists(), "a write ran while the queue was held");
    drop(release);
    tokio::time::timeout(WAIT, running)
        .await
        .expect("prompt settles")
        .expect("prompt task")
        .expect("prompt");
    holder.join().expect("holder thread");
    let outcomes = outcomes(&session);
    let direct = session
        .messages()
        .iter()
        .find_map(|message| {
            message
                .as_tool_result()
                .filter(|result| result.tool_call_id == "c0")
                .map(|result| (result.is_error, text(&result.content)))
        })
        .expect("the direct write's result");
    session.shutdown().await;
    assert_eq!(direct, (false, "Successfully wrote to a.txt".to_owned()));
    assert_eq!(
        outcomes,
        [(
            "c1/1".to_owned(),
            false,
            "Successfully wrote to a.txt".to_owned()
        )]
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "nested");
}

/// Shutting the session down while a nested write's worker is parked in
/// the file's mutation queue waits for that worker, owned by the session's
/// work gate, and the write, seeing the abort, leaves no file.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_during_a_nested_write_waits_for_its_worker() {
    let fx = Fixture::new("nested-shutdown");
    let target = fx.ws().join("a.txt");
    fx.script(json!([write("a.txt")]));
    let (entered, ready) = tokio::sync::oneshot::channel();
    // Dropped on every path out of the test, so a failed case never leaves
    // the worker parked.
    let (release, held) = std::sync::mpsc::channel::<()>();
    crate::permissions::fs::seam::on_after_check(&target, move || {
        let _ = entered.send(());
        let _ = held.recv();
    });
    let session = Arc::new(
        fx.session(
            SandboxMode::WorkspaceWrite,
            ApprovalPolicy::Ask,
            Answer::Now(EscalationOutcome::Rejected),
            PluginRegistry::default(),
        )
        .await,
    );
    let running = tokio::spawn({
        let session = Arc::clone(&session);
        async move { session.prompt("go", Vec::new()).await }
    });
    tokio::time::timeout(WAIT, ready)
        .await
        .expect("the nested write is in its worker")
        .expect("seam");
    let shutdown = tokio::spawn({
        let session = Arc::clone(&session);
        async move { session.shutdown().await }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let waited = !shutdown.is_finished();
    drop(release);
    tokio::time::timeout(WAIT, shutdown)
        .await
        .expect("shutdown returns")
        .expect("shutdown task");
    let _ = tokio::time::timeout(WAIT, running)
        .await
        .expect("prompt settles");
    assert!(
        waited,
        "shutdown returned while the nested write's worker ran"
    );
    assert!(!target.exists(), "the aborted nested write wrote the file");
}
