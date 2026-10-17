use std::sync::{Arc, Mutex};

use bake_agent::RunOutcome;
use bake_ai::StopReason;
use bake_ai::providers::faux::{
    FauxResponseStep, faux_assistant_blocks, faux_assistant_message, faux_tool_call,
};
use serde_json::{Value, json};

use super::*;
use crate::agent_session::tests::{harness, new_file_session, options};
use crate::agent_session::{AgentSession, SessionEvent, lock};
use crate::session::{Fault, FaultKind, SessionManager};
use crate::test_support::TempDir;

fn create_request() -> Value {
    json!({"operation":"revise","expected":null,"document":{
        "id":"goal-1","kind":"goal","body":"Fix the bug and check the changed files","state":"active"
    }})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tool_revision_is_exposed_after_its_result_and_reopens_identically() {
    let dir = TempDir::new("document-tool-order");
    let h = harness();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let next = Arc::clone(&captured);
    h.faux.set_responses(vec![
        faux_assistant_blocks(
            vec![faux_tool_call(NAME, create_request(), Some("revise-1"))],
            StopReason::ToolUse,
        )
        .into(),
        FauxResponseStep::from_fn(move |context, _, _, _| {
            *next.lock().unwrap() = context.messages().to_vec();
            Ok(faux_assistant_message("The plan is recorded."))
        }),
    ]);
    let mut opts = options(&h, new_file_session(&dir), &dir.path().to_string_lossy());
    opts.settings.task_documents = true;
    opts.active_tool_names.push(NAME.into());
    let session = AgentSession::create(opts).await.unwrap();
    session
        .prompt("Create a goal for the fix", Vec::new())
        .await
        .unwrap();
    let messages = captured.lock().unwrap().clone();
    let result = messages
        .iter()
        .position(|message| message.role() == "toolResult")
        .unwrap();
    // The assistant arguments also contain the body; find the canonical user exposure.
    assert!(messages.iter().skip(result + 1).any(|message| {
        message.role() == "user"
            && serde_json::to_string(message)
                .unwrap()
                .contains("Fix the bug and check the changed files")
    }));
    let stored = lock(&session.session).build_session_context();
    let file = session.session_file().unwrap();
    assert_eq!(
        task_documents::project(&lock(&session.session)).unwrap()["goal-1"]
            .snapshot
            .revision,
        1
    );
    session.shutdown().await;
    let reopened = SessionManager::open(&file, None, None).unwrap();
    let model_context = |context: crate::session::SessionContext| {
        crate::session::messages::convert_to_llm(
            &context
                .messages
                .iter()
                .map(crate::session::AgentMessage::to_agent)
                .collect::<Vec<_>>(),
        )
    };
    assert_eq!(
        model_context(reopened.build_session_context()),
        model_context(stored)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_revisions_return_tool_errors_without_poisoning_the_session() {
    let dir = TempDir::new("document-tool-conflict");
    let h = harness();
    h.faux.set_responses(vec![
        faux_assistant_blocks(
            vec![faux_tool_call(NAME, create_request(), Some("first"))],
            StopReason::ToolUse,
        )
        .into(),
        faux_assistant_blocks(
            vec![faux_tool_call(NAME, create_request(), Some("stale"))],
            StopReason::ToolUse,
        )
        .into(),
        faux_assistant_message("I will reread the current revision.").into(),
        faux_assistant_message("A later prompt still works.").into(),
    ]);
    let mut opts = options(&h, new_file_session(&dir), &dir.path().to_string_lossy());
    opts.settings.task_documents = true;
    opts.active_tool_names.push(NAME.into());
    let session = AgentSession::create(opts).await.unwrap();
    session.prompt("Create a goal", Vec::new()).await.unwrap();
    let errors = session
        .messages()
        .iter()
        .filter(|message| {
            message
                .as_tool_result()
                .is_some_and(|result| result.is_error)
        })
        .count();
    assert_eq!(errors, 1);
    assert!(crate::agent_session::lock_gap(&session.core.gap).is_none());
    assert_eq!(
        task_documents::project(&lock(&session.session)).unwrap()["goal-1"]
            .snapshot
            .revision,
        1
    );
    session.prompt("Continue", Vec::new()).await.unwrap();
    session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_document_write_stops_before_the_next_request() {
    let dir = TempDir::new("document-tool-write-failure");
    let h = harness();
    h.faux.set_responses(vec![
        faux_assistant_blocks(
            vec![faux_tool_call(NAME, create_request(), Some("failed"))],
            StopReason::ToolUse,
        )
        .into(),
        faux_assistant_message("Must not run").into(),
    ]);
    let mut opts = options(&h, new_file_session(&dir), &dir.path().to_string_lossy());
    opts.settings.task_documents = true;
    opts.active_tool_names.push(NAME.into());
    let session = AgentSession::create(opts).await.unwrap();
    let store = Arc::clone(&session.session);
    let _subscription = session.subscribe(move |event| {
        if matches!(
            event,
            SessionEvent::Agent(bake_agent::AgentEvent::ToolExecutionStart { .. })
        ) {
            lock(&store).arm_group_fault(Fault {
                line: 0,
                kind: FaultKind::Refuse,
            });
        }
    });
    let outcome = session
        .prompt_with_control("Create a goal", Vec::new(), Default::default())
        .await
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Failed { .. }), "{outcome:?}");
    assert_eq!(h.faux.call_count(), 1);
    assert!(
        task_documents::project(&lock(&session.session))
            .unwrap()
            .is_empty()
    );
    session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enabling_documents_does_not_override_the_selected_tools() {
    for (enabled, selected, expected) in [
        (false, true, false),
        (true, false, false),
        (true, true, true),
    ] {
        let dir = TempDir::new("document-tool-selection");
        let h = harness();
        h.faux
            .set_responses(vec![FauxResponseStep::from_fn(move |context, _, _, _| {
                assert_eq!(
                    bake_ai::transcript::get_current_tools(context.messages())
                        .iter()
                        .any(|tool| tool.name == NAME),
                    expected
                );
                Ok(faux_assistant_message("done"))
            })]);
        let mut opts = options(&h, new_file_session(&dir), &dir.path().to_string_lossy());
        opts.settings.task_documents = enabled;
        if selected {
            opts.active_tool_names.push(NAME.into());
        }
        let session = AgentSession::create(opts).await.unwrap();
        session.prompt("Continue", Vec::new()).await.unwrap();
        session.shutdown().await;
    }
}

#[test]
fn document_crash_host() {
    use std::io::Write;
    let Some(root) = std::env::var_os("BAKE_DOCUMENT_CRASH_DIR") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let mut session =
        SessionManager::create(&root.to_string_lossy(), Some(&root), Default::default()).unwrap();
    apply(
        &mut session,
        serde_json::from_value(create_request()).unwrap(),
    )
    .unwrap();
    if std::env::var_os("BAKE_DOCUMENT_CRASH_EXPOSE").is_some() {
        task_documents::expose_pending(&mut session).unwrap();
    }
    println!(
        "DOCUMENT_CHECKPOINT {}",
        json!({"file":session.session_file()})
    );
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
    panic!("the parent must kill this process at the durable checkpoint");
}

async fn crash_document(exposed: bool) {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let dir = TempDir::new("document-crash");
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "agent_session::task_documents::tests::document_crash_host",
            "--nocapture",
        ])
        .env("BAKE_DOCUMENT_CRASH_DIR", dir.path())
        .env_remove("BAKE_DOCUMENT_CRASH_EXPOSE")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true);
    if exposed {
        command.env("BAKE_DOCUMENT_CRASH_EXPOSE", "1");
    }
    let mut child = command.spawn().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let reached = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while let Some(line) = lines.next_line().await? {
            if let Some(value) = line.strip_prefix("DOCUMENT_CHECKPOINT ") {
                return Ok::<_, std::io::Error>(Some(
                    serde_json::from_str::<Value>(value).unwrap(),
                ));
            }
        }
        Ok(None)
    })
    .await;
    let _ = child.kill().await;
    let _ = child.wait().await;
    let checkpoint = reached
        .expect("checkpoint deadline")
        .expect("checkpoint output")
        .expect("checkpoint reached");
    let mut reopened = SessionManager::open(
        std::path::Path::new(checkpoint["file"].as_str().unwrap()),
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        task_documents::project(&reopened).unwrap()["goal-1"]
            .snapshot
            .revision,
        1
    );
    assert_eq!(
        task_documents::expose_pending(&mut reopened).unwrap(),
        usize::from(!exposed)
    );
    assert_eq!(task_documents::expose_pending(&mut reopened).unwrap(), 0);
    assert_eq!(reopened.build_session_context().messages.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crash_after_snapshot_repairs_exposure_without_repeating_the_revision() {
    crash_document(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crash_after_exposure_keeps_one_model_visible_copy() {
    crash_document(true).await;
}

// The host owns the native tool with no rewriting hook. Only that host can
// attest success; text from the model is never an evidence input.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_check_evidence_rejects_changed_inputs_and_later_becomes_stale() {
    use crate::task_documents::artifacts::ArtifactSet;
    use crate::task_documents::verification::{
        self, CheckSpec, EvidenceStatus, VerificationTicket,
    };
    use crate::tools::bash::{BashToolOptions, create_bash_tool};
    for changes_input in [false, true] {
        let dir = TempDir::new("document-native-check");
        std::fs::write(dir.join("input.txt"), "ok").unwrap();
        let command = if changes_input {
            "test \"$(cat input.txt)\" = ok && printf changed > input.txt"
        } else {
            "test \"$(cat input.txt)\" = ok"
        };
        let h = harness();
        h.faux.set_responses(vec![
            faux_assistant_blocks(
                vec![faux_tool_call(
                    "bash",
                    json!({"command":command}),
                    Some("check-1"),
                )],
                StopReason::ToolUse,
            )
            .into(),
            faux_assistant_message("The check passed.").into(),
        ]);
        let mut store = new_file_session(&dir);
        apply(
            &mut store,
            serde_json::from_value(create_request()).unwrap(),
        )
        .unwrap();
        let document = task_documents::project(&store).unwrap()["goal-1"].reference();
        let before = ArtifactSet::capture(dir.path(), &["input.txt".into()]).unwrap();
        let ticket = VerificationTicket::prepare(
            &store,
            &document,
            CheckSpec {
                command: command.into(),
            },
            &before,
        )
        .unwrap();
        let work = crate::owned_work::WorkGate::default();
        let mut opts = options(&h, store, &dir.path().to_string_lossy());
        opts.tools.push(Arc::new(create_bash_tool(
            dir.path(),
            BashToolOptions {
                owner: Some(work.clone()),
                ..Default::default()
            },
        )));
        opts.active_tool_names.push("bash".into());
        opts.work = Some(work);
        let session = AgentSession::create(opts).await.unwrap();
        session
            .prompt("Run the admitted check", Vec::new())
            .await
            .unwrap();
        let after = before.recapture(dir.path()).unwrap();
        {
            let mut store = lock(&session.session);
            let result = store
                .entries()
                .into_iter()
                .find(|entry| {
                    entry
                        .get("message")
                        .and_then(|m| m.get("toolCallId"))
                        .and_then(Value::as_str)
                        == Some("check-1")
                })
                .unwrap();
            assert_eq!(
                result.as_json()["message"]["isError"],
                false,
                "native command failed"
            );
            let result_id = result.id().to_owned();
            let evidence = verification::record(&mut store, ticket, &result_id, &after);
            if changes_input {
                assert!(matches!(evidence, Err(DocumentError::Conflict)));
                assert_eq!(
                    verification::status(&store, &document, Some(&after)).unwrap(),
                    EvidenceStatus::Unavailable
                );
            } else {
                evidence.unwrap();
                assert_eq!(
                    verification::status(&store, &document, Some(&after)).unwrap(),
                    EvidenceStatus::Fresh
                );
                std::fs::write(dir.join("input.txt"), "later edit").unwrap();
                let changed = after.recapture(dir.path()).unwrap();
                assert_eq!(
                    verification::status(&store, &document, Some(&changed)).unwrap(),
                    EvidenceStatus::Stale
                );
            }
        }
        session.shutdown().await;
    }
}
