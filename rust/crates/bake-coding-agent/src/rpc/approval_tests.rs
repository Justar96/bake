//! Approvals over the RPC protocol (D24): a tool call that escalates the
//! sandbox reaches the client as Pi's `extension_ui_request`, its answer
//! decides the one call, and both are in the session file before the call
//! runs or is refused.
//!
//! The fixture follows `agent_session/approval_tests.rs`, whose rules come
//! from Bake's TypeScript `ApprovalService.request`
//! (`packages/interaction/user-approval/src/index.ts`): an `asked` entry
//! before anyone is asked and one `decided` entry per request. Here the
//! answerer is the RPC client, and the broker is the terminal's
//! ([`crate::permissions::ApprovalBroker`]).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bake_agent::{AgentTool, AgentToolResult};
use bake_ai::providers::faux::{faux_assistant_blocks, faux_assistant_message, faux_tool_call};
use bake_ai::{StopReason, TextContent, UserContentBlock};
use bake_sandbox::{SandboxMode, UnavailableProvider};
use serde_json::{Value, json};

use super::tests::{Input, Output, start};
use crate::agent_session::AgentSession;
use crate::agent_session::tests::{harness, options};
use crate::permissions::{
    APPROVAL_CUSTOM_TYPE, ApprovalBroker, ApprovalPolicy, BlankJustification, StandingPermissions,
    ToolCall, ToolPermissions,
};
use crate::session::{NewSessionOptions, SessionManager};
use crate::test_support::TempDir;

/// The mode the escalating call asks for, from a `read-only` session.
const TARGET: SandboxMode = SandboxMode::WorkspaceWrite;

/// A session whose one tool leaves a marker file once the permission
/// engine resolves its escalating call, with `broker` answering.
async fn effect_session(
    dir: &TempDir,
    broker: &ApprovalBroker,
    h: &crate::agent_session::tests::Harness,
) -> (AgentSession, PathBuf) {
    let ws = dir.join("ws");
    let marker = dir.join("markers").join("ran");
    std::fs::create_dir_all(&ws).expect("ws");
    std::fs::create_dir_all(dir.join("markers")).expect("markers");
    let standing = StandingPermissions::new(
        SandboxMode::ReadOnly,
        ApprovalPolicy::Ask,
        "test",
        &ws,
        Some("rpc-approval-tests".into()),
    );
    let permissions = ToolPermissions::new(
        Arc::new(standing),
        Arc::new(broker.clone()),
        Arc::new(UnavailableProvider::new(None)),
    );
    let tool = {
        let permissions = permissions.clone();
        let marker = marker.clone();
        AgentTool::new(
            "effect",
            "effect",
            "Leaves a marker file.",
            json!({
                "type": "object",
                "properties": {
                    "sandbox_permissions": { "type": "string" },
                    "justification": { "type": "string" },
                },
            }),
            move |id, args, signal, _, _ctx| {
                let permissions = permissions.clone();
                let marker = marker.clone();
                Box::pin(async move {
                    let call = ToolCall {
                        tool_name: "effect",
                        call_id: &id,
                        parent_tool_call_id: None,
                        args: &args,
                        subject: "operation",
                        blank_justification: BlankJustification::Checked,
                    };
                    permissions.engine.resolve(&call, signal.as_ref()).await?;
                    std::fs::write(&marker, "ran").map_err(|error| error.to_string())?;
                    Ok(AgentToolResult {
                        content: vec![UserContentBlock::Text(TextContent::new("ran"))],
                        ..AgentToolResult::default()
                    })
                })
            },
        )
    };
    let store = SessionManager::create(
        &ws.to_string_lossy(),
        Some(&dir.join("sessions")),
        NewSessionOptions::default(),
    )
    .expect("store");
    let mut options = options(h, store, &ws.to_string_lossy());
    options.active_tool_names = vec![tool.name.clone()];
    options.tools = vec![Arc::new(tool)];
    let session = AgentSession::create(options).await.expect("session");
    session
        .record_approvals(permissions.engine.recorder_slot())
        .expect("bind the recorder");
    (session, marker)
}

/// The approval entries the session file holds, in order.
fn approvals(file: &Path) -> Vec<Value> {
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|entry| {
            entry["type"] == json!("custom") && entry["customType"] == APPROVAL_CUSTOM_TYPE
        })
        .map(|entry| entry["data"].clone())
        .collect()
}

/// Runs one prompt whose response asks for an escalating `effect` call and
/// returns what the client was asked, the marker's existence, and the
/// session file's approval entries once the loop has ended.
struct Case {
    out: Output,
    input: Input,
    marker: PathBuf,
    file: PathBuf,
    mode: tokio::task::JoinHandle<i32>,
    /// Removed when the case ends, with the session file it holds.
    _dir: TempDir,
}

async fn case(name: &str, broker: ApprovalBroker) -> Case {
    let dir = TempDir::new(name);
    let h = harness();
    h.faux.set_responses(vec![
        faux_assistant_blocks(
            vec![faux_tool_call(
                "effect",
                json!({
                    "sandbox_permissions": TARGET.as_str(),
                    "justification": "to write the file",
                }),
                Some("call-1"),
            )],
            StopReason::ToolUse,
        )
        .into(),
        faux_assistant_message("done").into(),
    ]);
    let (session, marker) = effect_session(&dir, &broker, &h).await;
    let file = session.session_file().expect("persisted");
    let (input, pipe) = Input::new();
    let out = Output(Arc::new(Mutex::new(Vec::new())));
    let mode = start(session, pipe, &out, Some(broker));
    input.send(json!({ "id": "p", "type": "prompt", "message": "write it" }));
    out.wait_for_response("p").await;
    Case {
        out,
        input,
        marker,
        file,
        mode,
        _dir: dir,
    }
}

/// Waits for the approval request and returns its id.
async fn asked(out: &Output) -> String {
    out.wait_for("the approval request", |out| {
        !out.of_kind("extension_ui_request").is_empty()
    })
    .await;
    let request = out.of_kind("extension_ui_request").remove(0);
    assert_eq!(request["method"], json!("confirm"), "{request}");
    assert_eq!(
        request["approval"]["toolName"],
        json!("effect"),
        "{request}"
    );
    assert_eq!(request["approval"]["callId"], json!("call-1"), "{request}");
    assert!(
        request["approval"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("workspace-write")),
        "{request}"
    );
    request["id"].as_str().expect("an id").to_owned()
}

// Bake (D24): `confirmed: true` lets the one call run, and the session
// file holds `asked` then `decided` with `allowed-once` before it did.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_approved_call_runs_after_its_answer_is_written() {
    let case = case("rpc-approve", ApprovalBroker::new()).await;
    let id = asked(&case.out).await;
    case.input.send(json!({
        "type": "extension_ui_response",
        "id": id,
        "confirmed": true,
    }));
    case.out
        .wait_for("the settled turn", |out| {
            !out.of_kind("agent_settled").is_empty()
        })
        .await;
    case.input.close();
    assert_eq!(case.mode.await.expect("the loop ends"), 0);
    assert!(case.marker.exists(), "the approved call did not run");
    let approvals = approvals(&case.file);
    assert_eq!(approvals.len(), 2, "{approvals:?}");
    assert_eq!(approvals[0]["kind"], json!("asked"));
    assert_eq!(approvals[0]["toolName"], json!("effect"));
    assert_eq!(approvals[1]["kind"], json!("decided"));
    assert_eq!(approvals[1]["outcome"], json!("allowed-once"));
    assert_eq!(approvals[0]["id"], approvals[1]["id"]);
}

// Bake (D24): `confirmed: false` refuses the call, which leaves no effect.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_denied_call_is_refused_and_recorded() {
    let case = case("rpc-deny", ApprovalBroker::new()).await;
    let id = asked(&case.out).await;
    case.input.send(json!({
        "type": "extension_ui_response",
        "id": id,
        "confirmed": false,
    }));
    case.out
        .wait_for("the settled turn", |out| {
            !out.of_kind("agent_settled").is_empty()
        })
        .await;
    case.input.close();
    assert_eq!(case.mode.await.expect("the loop ends"), 0);
    assert!(!case.marker.exists(), "the denied call ran");
    let approvals = approvals(&case.file);
    assert_eq!(approvals.len(), 2, "{approvals:?}");
    assert_eq!(approvals[1]["outcome"], json!("rejected"));
}

// Bake (D24): a client that disconnects without answering leaves the call
// refused, never run, and the refusal is recorded.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disconnected_client_leaves_the_call_refused() {
    let case = case("rpc-disconnect", ApprovalBroker::new()).await;
    asked(&case.out).await;
    case.input.close();
    assert_eq!(case.mode.await.expect("the loop ends"), 0);
    assert!(!case.marker.exists(), "the unanswered call ran");
    let approvals = approvals(&case.file);
    assert_eq!(approvals.len(), 2, "{approvals:?}");
    assert_eq!(approvals[1]["outcome"], json!("cancelled"));
}
