//! A sandboxed `write` or `edit` survives the hard abort of its Agent task:
//! the session's work gate waits for its blocking worker, which then sees
//! the abort and leaves the file as it was. The sandbox's test seam
//! ([`crate::permissions::fs::seam`]) holds the worker inside the file's
//! mutation queue, after the containment check and before the file is
//! touched.

use std::task::Poll;
use std::time::Duration;

use bake_sandbox::{SandboxMode, UnavailableProvider};
use serde_json::Value;

use super::*;
use crate::owned_work::WorkGate;
use crate::permissions::fs::seam;
use crate::permissions::{
    ApprovalPolicy, NoApprover, StandingPermissions, ToolPermissions, owned_sandbox_file_tools,
};
use crate::tools::index::{ToolsOptions, create_tool_definition};

async fn hard_shutdown_keeps_sandboxed_mutation_owned(name: &str, args: Value, original: &str) {
    let dir = TempDir::new(&format!("owned-sandboxed-{name}"));
    let cwd = dir.path().to_string_lossy().into_owned();
    let target = dir.join("target.txt");
    std::fs::write(&target, original).unwrap();
    let (entered, ready) = tokio::sync::oneshot::channel();
    // Dropped on every path out of this function, so a failed case never
    // leaves the worker parked for the runtime to wait on.
    let (release, held) = std::sync::mpsc::channel::<()>();
    seam::on_after_check(&target, move || {
        let _ = entered.send(());
        let _ = held.recv();
    });
    let h = harness();
    h.faux.set_responses(vec![
        faux_assistant_blocks(
            vec![faux_tool_call(name, args, Some("call_1"))],
            StopReason::ToolUse,
        )
        .into(),
    ]);
    let work = WorkGate::default();
    let permissions = ToolPermissions::new(
        Arc::new(StandingPermissions::new(
            SandboxMode::WorkspaceWrite,
            ApprovalPolicy::Never,
            "workspace-write",
            dir.path(),
            None,
        )),
        Arc::new(NoApprover),
        Arc::new(UnavailableProvider::new(None)),
    );
    let unsandboxed = create_tool_definition(name, dir.path(), &ToolsOptions::default()).unwrap();
    let definition = owned_sandbox_file_tools(
        vec![unsandboxed],
        dir.path(),
        &permissions,
        Some(work.clone()),
    )
    .pop()
    .unwrap();
    let session = Arc::new(
        AgentSession::create(AgentSessionOptions {
            work: Some(work.clone()),
            tools: vec![Arc::new(definition.tool)],
            active_tool_names: vec![name.into()],
            ..options(&h, new_file_session(&dir), &cwd)
        })
        .await
        .unwrap(),
    );
    let owner = Arc::clone(&session);
    let prompt = tokio::spawn(async move { owner.prompt("change target", Vec::new()).await });
    tokio::time::timeout(Duration::from_secs(10), ready)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), session.agent.shutdown())
        .await
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(10), prompt)
        .await
        .unwrap()
        .unwrap();
    let mut drained = Box::pin(work.close());
    let waiting =
        std::future::poll_fn(|cx| Poll::Ready(drained.as_mut().poll(cx).is_pending())).await;
    drop(release);
    if waiting {
        tokio::time::timeout(Duration::from_secs(10), drained)
            .await
            .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(10), session.shutdown())
        .await
        .unwrap();
    assert!(
        waiting,
        "{name}: the gate drained while the mutation's worker was still running"
    );
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        original,
        "{name}: the aborted call changed the file"
    );
}

#[tokio::test]
async fn hard_agent_shutdown_keeps_sandboxed_write_owned_until_the_worker_returns() {
    hard_shutdown_keeps_sandboxed_mutation_owned(
        "write",
        json!({"path": "target.txt", "content": "replaced\n"}),
        "original\n",
    )
    .await;
}

#[tokio::test]
async fn hard_agent_shutdown_keeps_sandboxed_edit_owned_until_the_worker_returns() {
    hard_shutdown_keeps_sandboxed_mutation_owned(
        "edit",
        json!({"path": "target.txt", "edits": [{"oldText": "alpha", "newText": "beta"}]}),
        "alpha\n",
    )
    .await;
}
