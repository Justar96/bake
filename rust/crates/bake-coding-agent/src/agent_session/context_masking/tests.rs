use std::sync::{Arc, Mutex};

use bake_agent::{RunControl, RunLimits, RunOutcome};
use bake_ai::providers::faux::{
    FauxResponseStep, faux_assistant_blocks, faux_assistant_message, faux_tool_call,
};
use bake_ai::{AbortController, StopReason};

use super::*;
use crate::agent_session::tests::{harness, new_file_session, options};
use crate::agent_session::{AgentSession, lock};
use crate::session::{AgentMessage, Fault, FaultKind, NewSessionOptions, SessionManager};
use crate::test_support::TempDir;

fn message(value: Value) -> AgentMessage {
    AgentMessage::from_json(value.as_object().unwrap().clone())
}

fn model_messages(projection: &SessionProjection) -> Vec<bake_ai::Message> {
    crate::session::messages::convert_to_llm(
        &projection
            .messages
            .iter()
            .map(AgentMessage::to_agent)
            .collect::<Vec<_>>(),
    )
}

fn history() -> SessionManager {
    let mut session =
        SessionManager::in_memory(Some("/test"), NewSessionOptions::default(), Vec::new()).unwrap();
    session
        .append_message(message(
            json!({"role":"user","content":"Keep the original task", "timestamp":0}),
        ))
        .unwrap();
    session
}

fn batch(
    session: &mut SessionManager,
    name: &str,
    arguments: Value,
    text: &str,
    is_error: bool,
) -> String {
    let id = format!("call-{}", session.entries().len());
    session
        .append_message(faux_assistant_blocks(
            vec![faux_tool_call(name, arguments, Some(&id))],
            StopReason::ToolUse,
        ))
        .unwrap();
    session
        .append_message(message(json!({
            "role":"toolResult", "toolCallId":id, "toolName":name,
            "content":[{"type":"text","text":text}], "isError":is_error, "timestamp":0,
            "details":{"bake.contextObservation":{"version":1,"kind":name}}
        })))
        .unwrap()
}

fn tail(session: &mut SessionManager) {
    for _ in 0..RETAINED_BATCHES {
        batch(
            session,
            "grep",
            json!({"pattern":"recent"}),
            "A recent result must remain intact.",
            false,
        );
    }
}

fn prepared(session: &mut SessionManager) -> Vec<String> {
    let ids = (0..3)
        .map(|index| {
            batch(
                session,
                "grep",
                json!({"pattern":"noise"}),
                &format!("old observation {index}: {}", "noise ".repeat(200)),
                false,
            )
        })
        .collect();
    tail(session);
    ids
}

fn masked_targets(plan: &Plan) -> Vec<&str> {
    plan.edits
        .iter()
        .map(|edit| match edit {
            GroupEntry::ContextEdit { target_id, .. } => target_id.as_str(),
            _ => panic!("masking emits context replacements only"),
        })
        .collect()
}

#[test]
fn masking_preserves_call_result_identity_recent_batches_and_original_log() {
    let mut session = history();
    let ids = prepared(&mut session);
    let before = session.build_session_projection();
    let plan = plan(&before);
    assert_eq!(masked_targets(&plan), ids);
    assert!(plan.removed_bytes > 3000);
    session.append_group(plan.edits).unwrap();
    let after = session.build_session_projection();
    assert_eq!(before.messages.len(), after.messages.len());
    for (before, after) in before.messages.iter().zip(&after.messages) {
        let mut before = before.as_json().clone();
        let mut after = after.as_json().clone();
        if before.get("role").and_then(Value::as_str) == Some("toolResult") {
            before.remove("content");
            after.remove("content");
        }
        assert_eq!(before, after);
    }
    for id in ids {
        assert!(
            session
                .entry(&id)
                .unwrap()
                .get("message")
                .unwrap()
                .to_string()
                .contains("old observation")
        );
    }
    assert!(
        super::plan(&after).edits.is_empty(),
        "repeating the boundary appends nothing"
    );
}

#[test]
fn unique_and_latest_read_ranges_errors_unknown_tools_and_edits_stay_visible() {
    let mut session = history();
    let old_read = batch(
        &mut session,
        "read",
        json!({"path":"src/a.rs","offset":1,"limit":20}),
        &"older version ".repeat(30),
        false,
    );
    let protected = [
        batch(
            &mut session,
            "read",
            json!({"path":"unique.txt"}),
            &"unique required fact ".repeat(30),
            false,
        ),
        batch(
            &mut session,
            "read",
            json!({"path":"src/a.rs","offset":1,"limit":20}),
            &"latest source ".repeat(30),
            false,
        ),
        batch(
            &mut session,
            "read",
            json!({"path":"src/a.rs","offset":21,"limit":20}),
            &"different range ".repeat(30),
            false,
        ),
        batch(
            &mut session,
            "read",
            json!({"futurePathField":"unknown.txt"}),
            &"unrecognized read arguments ".repeat(30),
            false,
        ),
        batch(
            &mut session,
            "grep",
            json!({"pattern":"fail"}),
            &"error details ".repeat(30),
            true,
        ),
        batch(
            &mut session,
            "bash",
            json!({"command":"build"}),
            &"test or mutation outcome ".repeat(30),
            false,
        ),
        batch(
            &mut session,
            "mcp__search",
            json!({}),
            &"unknown observation ".repeat(30),
            false,
        ),
    ];
    let edited = batch(
        &mut session,
        "grep",
        json!({}),
        &"unmodified ".repeat(30),
        false,
    );
    session
        .append_context_edit(
            &edited,
            Some(EditContent::Text(
                "Explicit replacement with a required fact ".repeat(30),
            )),
        )
        .unwrap();
    tail(&mut session);
    let plan = plan(&session.build_session_projection());
    assert_eq!(masked_targets(&plan), [old_read]);
    for id in protected.iter().chain(std::iter::once(&edited)) {
        assert!(!masked_targets(&plan).contains(&id.as_str()));
    }
}

#[test]
fn incomplete_ambiguous_and_multimodal_batches_are_not_masked() {
    let mut session = history();
    session
        .append_message(faux_assistant_blocks(
            vec![
                faux_tool_call("grep", json!({}), Some("one")),
                faux_tool_call("grep", json!({}), Some("two")),
            ],
            StopReason::ToolUse,
        ))
        .unwrap();
    session.append_message(message(json!({"role":"toolResult","toolCallId":"one","toolName":"grep","details":{"bake.contextObservation":{"version":1,"kind":"grep"}},"content":[{"type":"text","text":"partial batch ".repeat(100)}],"isError":false,"timestamp":0}))).unwrap();
    session
        .append_message(faux_assistant_blocks(
            vec![faux_tool_call("grep", json!({}), Some("image"))],
            StopReason::ToolUse,
        ))
        .unwrap();
    session.append_message(message(json!({"role":"toolResult","toolCallId":"image","toolName":"grep","details":{"bake.contextObservation":{"version":1,"kind":"grep"}},"content":[{"type":"text","text":"image description ".repeat(100)},{"type":"image","data":"AAAA","mimeType":"image/png"}],"isError":false,"timestamp":0}))).unwrap();
    session
        .append_message(faux_assistant_blocks(
            vec![
                faux_tool_call("grep", json!({}), Some("duplicate")),
                faux_tool_call("grep", json!({}), Some("duplicate")),
            ],
            StopReason::ToolUse,
        ))
        .unwrap();
    session.append_message(message(json!({"role":"toolResult","toolCallId":"duplicate","toolName":"grep","details":{"bake.contextObservation":{"version":1,"kind":"grep"}},"content":[{"type":"text","text":"ambiguous call ".repeat(100)}],"isError":false,"timestamp":0}))).unwrap();
    tail(&mut session);
    assert!(plan(&session.build_session_projection()).edits.is_empty());
}

#[tokio::test]
async fn a_custom_marker_saved_while_masking_is_off_is_not_trusted_after_reopen() {
    let dir = TempDir::new("masking-provenance-reopen");
    let h = harness();
    let original_text = "custom observations must remain ".repeat(100);
    let returned = original_text.clone();
    let tool = bake_agent::AgentTool::new(
        "grep",
        "Custom grep",
        "A custom override",
        json!({"type":"object","properties":{}}),
        move |_, _, _, _| {
            let text = returned.clone();
            Box::pin(async move {
                Ok(bake_agent::AgentToolResult {
                    details: Some(json!({
                        "bake.contextObservation":{"version":1,"kind":"grep"},
                        "customDetail":"preserved"
                    })),
                    ..bake_agent::AgentToolResult::text(text)
                })
            })
        },
    );
    let mut responses: Vec<FauxResponseStep> = (0..RETAINED_BATCHES + 2)
        .map(|index| {
            faux_assistant_blocks(
                vec![faux_tool_call(
                    "grep",
                    json!({}),
                    Some(&format!("custom-{index}")),
                )],
                StopReason::ToolUse,
            )
            .into()
        })
        .collect();
    responses.push(faux_assistant_message("Saved the observations").into());
    h.faux.set_responses(responses);
    let mut first = options(&h, new_file_session(&dir), &dir.path().to_string_lossy());
    assert!(!first.settings.context_masking);
    first.tools = vec![Arc::new(tool)];
    first.active_tool_names = vec!["grep".into()];
    let session = AgentSession::create(first).await.unwrap();
    assert_eq!(
        session
            .prompt_with_control("Gather observations", Vec::new(), RunControl::default())
            .await
            .unwrap(),
        RunOutcome::FinishedTurn
    );
    let file = session.session_file().unwrap();
    session.shutdown().await;

    let reopened = SessionManager::open(&file, None, None).unwrap();
    let mut second = options(&h, reopened, &dir.path().to_string_lossy());
    second.settings.context_masking = true;
    let session = AgentSession::create(second).await.unwrap();
    h.faux
        .set_responses(vec![faux_assistant_message("Continued").into()]);
    assert_eq!(
        session
            .prompt_with_control("Continue", Vec::new(), RunControl::default())
            .await
            .unwrap(),
        RunOutcome::FinishedTurn
    );
    session.shutdown().await;
    let reopened = SessionManager::open(&file, None, None).unwrap();
    let projection = reopened.build_session_projection();
    let results: Vec<_> = projection
        .messages
        .iter()
        .filter(|message| message.role() == Some("toolResult"))
        .collect();
    assert_eq!(results.len(), RETAINED_BATCHES + 2);
    for result in results {
        assert_eq!(
            result.as_json()["content"],
            json!([{"type":"text","text":original_text}]),
            "enabling masking trusted a marker supplied by a custom tool"
        );
        assert_eq!(
            result.as_json()["details"],
            json!({"customDetail":"preserved"})
        );
    }
}

#[tokio::test]
async fn a_native_read_is_attested_but_a_custom_override_cannot_attest_itself() {
    for native in [false, true] {
        let dir = TempDir::new("masking-tool-provenance");
        std::fs::write(dir.join("fact.txt"), "original read fact ".repeat(100)).unwrap();
        let h = harness();
        h.faux.set_responses(vec![
            faux_assistant_blocks(
                vec![faux_tool_call(
                    "read",
                    json!({"path":"fact.txt"}),
                    Some("actual-read"),
                )],
                StopReason::ToolUse,
            )
            .into(),
            faux_assistant_message("The observation is recorded").into(),
        ]);
        let tool = if native {
            crate::tools::read::create_read_tool(
                dir.path(),
                crate::tools::read::ReadToolOptions::default(),
            )
        } else {
            bake_agent::AgentTool::new(
                "read",
                "Custom read",
                "A custom override",
                json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
                |_, _, _, _| {
                    Box::pin(async {
                        Ok(bake_agent::AgentToolResult {
                            details: Some(
                                json!({"bake.contextObservation":{"version":1,"kind":"read"}}),
                            ),
                            ..bake_agent::AgentToolResult::text("custom observation ".repeat(100))
                        })
                    })
                },
            )
        };
        assert_eq!(tool.read_only, native);
        let mut options = options(&h, new_file_session(&dir), &dir.path().to_string_lossy());
        options.settings.context_masking = true;
        options.tools = vec![Arc::new(tool)];
        options.active_tool_names = vec!["read".into()];
        let session = AgentSession::create(options).await.unwrap();
        assert_eq!(
            session
                .prompt_with_control("Read the file", Vec::new(), RunControl::default())
                .await
                .unwrap(),
            RunOutcome::FinishedTurn
        );
        let original = {
            let mut manager = lock(&session.session);
            let original = manager
                .entries()
                .iter()
                .find(|entry| {
                    entry
                        .get("message")
                        .and_then(|message| message.get("toolCallId"))
                        .and_then(Value::as_str)
                        == Some("actual-read")
                })
                .unwrap()
                .id()
                .to_owned();
            assert_eq!(
                manager
                    .entry(&original)
                    .unwrap()
                    .get("message")
                    .unwrap()
                    .get("details")
                    .and_then(|details| details.get(PROVENANCE))
                    .is_some(),
                native
            );
            batch(
                &mut manager,
                "read",
                json!({"path":"fact.txt"}),
                &"latest required fact ".repeat(100),
                false,
            );
            tail(&mut manager);
            original
        };
        session.core.mask_context(None).await.unwrap();
        let projected = lock(&session.session).build_session_projection();
        let result = projected
            .entries
            .iter()
            .find(|entry| entry.source_entry.id() == original)
            .unwrap();
        assert_eq!(
            result.messages[0].as_json().get("content")
                == Some(&json!([{"type":"text","text":REPLACEMENT}])),
            native
        );
        session.shutdown().await;
    }
}

#[test]
fn complete_parallel_results_keep_their_recorded_order_and_whole_recent_batches() {
    let mut session = history();
    session
        .append_message(faux_assistant_blocks(
            vec![
                faux_tool_call("grep", json!({}), Some("first")),
                faux_tool_call("find", json!({}), Some("second")),
            ],
            StopReason::ToolUse,
        ))
        .unwrap();
    let mut ids = Vec::new();
    for (id, name) in [("second", "find"), ("first", "grep")] {
        ids.push(session.append_message(message(json!({"role":"toolResult","toolCallId":id,"toolName":name,"details":{"bake.contextObservation":{"version":1,"kind":name}},"content":[{"type":"text","text":"old parallel result ".repeat(100)}],"isError":false,"timestamp":0}))).unwrap());
    }
    tail(&mut session);
    let projection = session.build_session_projection();
    let plan = plan(&projection);
    assert_eq!(masked_targets(&plan), ids);
    session.append_group(plan.edits).unwrap();
    let masked = session.build_session_projection();
    let results: Vec<_> = masked
        .messages
        .iter()
        .filter_map(|message| message.as_json().get("toolCallId"))
        .collect();
    assert_eq!(results[0], "second");
    assert_eq!(results[1], "first");
    let converted = model_messages(&masked);
    let repaired = bake_ai::api::transform_messages::transform_messages(
        &converted,
        &bake_ai::providers::faux::faux_model("faux-1"),
        None,
    );
    assert_eq!(
        repaired.len(),
        converted.len(),
        "masking needs no synthetic result repair"
    );
}

#[test]
fn context_masking_setting_requires_explicit_true() {
    assert!(!crate::settings::Settings::default().context_masking);
    for value in [
        Value::Null,
        json!(false),
        json!("true"),
        json!(1),
        json!({}),
    ] {
        let settings = crate::settings::Settings::from_object(
            json!({"contextMasking":value}).as_object().unwrap(),
        );
        assert!(!settings.context_masking);
    }
    assert!(
        crate::settings::Settings::from_object(json!({"contextMasking":true}).as_object().unwrap())
            .context_masking
    );
}

#[test]
fn unmarked_imported_read_results_are_not_inferred_from_their_names() {
    let mut session = history();
    session
        .append_message(faux_assistant_blocks(
            vec![faux_tool_call("grep", json!({}), Some("unmarked"))],
            StopReason::ToolUse,
        ))
        .unwrap();
    session.append_message(message(json!({"role":"toolResult","toolCallId":"unmarked","toolName":"grep","content":[{"type":"text","text":"imported observation ".repeat(100)}],"isError":false,"timestamp":0}))).unwrap();
    tail(&mut session);
    assert!(plan(&session.build_session_projection()).edits.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_context_uses_durable_edits_only_when_enabled_and_reopens_identically() {
    for enabled in [false, true] {
        let dir = TempDir::new("masking-request");
        let h = harness();
        let mut manager = new_file_session(&dir);
        prepared(&mut manager);
        let expected_bytes = plan(&manager.build_session_projection()).removed_bytes;
        let file = manager.session_file().unwrap().to_owned();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let capture = Arc::clone(&captured);
        let check_file = file.clone();
        h.faux
            .set_responses(vec![FauxResponseStep::from_fn(move |context, _, _, _| {
                let read = SessionManager::in_memory(
                    None,
                    NewSessionOptions::default(),
                    crate::session::load_entries_from_file(&check_file).unwrap(),
                )
                .unwrap();
                let logged = crate::session::messages::convert_to_llm(
                    &read
                        .build_session_projection()
                        .messages
                        .iter()
                        .map(AgentMessage::to_agent)
                        .collect::<Vec<_>>(),
                );
                assert_eq!(context.messages(), logged);
                *capture.lock().unwrap() = context.messages().to_vec();
                Ok(faux_assistant_message("Finished"))
            })]);
        let mut options = options(&h, manager, &dir.path().to_string_lossy());
        options.settings.context_masking = enabled;
        let session = AgentSession::create(options).await.unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&events);
        let _subscription = session.subscribe(move |event| {
            if matches!(event, SessionEvent::ContextMasking { .. }) {
                recorded
                    .lock()
                    .unwrap()
                    .push(crate::json_event::session_event_json(event));
            }
        });
        let outcome = session
            .prompt_with_control(
                "Continue",
                Vec::new(),
                RunControl::new(RunLimits {
                    max_model_calls: Some(1),
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
        assert_eq!(outcome, RunOutcome::FinishedTurn);
        assert_eq!(h.faux.call_count(), 1, "masking spends no summary call");
        let events = events.lock().unwrap().clone();
        assert_eq!(events.len(), usize::from(enabled));
        if enabled {
            assert_eq!(events[0]["type"], "context_masking");
            assert_eq!(events[0]["maskedResults"], 3);
            assert_eq!(events[0]["removedBytes"], expected_bytes);
            assert_eq!(events[0]["retainedRecentBatches"], RETAINED_BATCHES);
            assert!(events[0]["sourceLeafId"].is_string());
        }
        assert_eq!(
            serde_json::to_string(&*captured.lock().unwrap())
                .unwrap()
                .contains(REPLACEMENT),
            enabled
        );
        let stored = lock(&session.session).build_session_projection();
        session.shutdown().await;
        assert_eq!(
            model_messages(
                &SessionManager::open(&file, None, None)
                    .unwrap()
                    .build_session_projection()
            ),
            model_messages(&stored)
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_partial_masking_write_stops_before_dispatch_and_preserves_pairs_on_reopen() {
    let dir = TempDir::new("masking-write-failure");
    let h = harness();
    let mut manager = new_file_session(&dir);
    prepared(&mut manager);
    let mut options = options(&h, manager, &dir.path().to_string_lossy());
    options.settings.context_masking = true;
    let session = AgentSession::create(options).await.unwrap();
    let control = RunControl::default();
    let cancelling = control.clone();
    lock(&session.session).arm_group_fault(Fault {
        line: 1,
        kind: FaultKind::Hold(0, Box::new(move || cancelling.cancel())),
    });
    let outcome = session
        .prompt_with_control("Continue", Vec::new(), control)
        .await
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Failed { .. }), "{outcome:?}");
    assert_eq!(h.faux.call_count(), 0);
    let file = session.session_file().unwrap();
    session.shutdown().await;
    let reopened = SessionManager::open(&file, None, None).unwrap();
    assert_eq!(
        reopened
            .entries()
            .iter()
            .filter(|entry| entry.entry_type() == "context_edit")
            .count(),
        1
    );
    assert_eq!(
        batches(&reopened.build_session_projection()).len(),
        RETAINED_BATCHES + 3
    );
}

#[tokio::test]
async fn masking_precedes_threshold_compaction_and_retains_the_foreground_fallback() {
    for (enabled, name, expected_compaction) in [
        (false, "grep", true),
        (true, "grep", false),
        (true, "read", true),
    ] {
        let dir = TempDir::new("masking-foreground-fallback");
        let h = harness();
        let mut manager = new_file_session(&dir);
        batch(
            &mut manager,
            name,
            json!({"path":"required.txt","pattern":"noise"}),
            &"A large observation with relevant data. ".repeat(3000),
            false,
        );
        tail(&mut manager);
        let mut options = options(&h, manager, &dir.path().to_string_lossy());
        options.settings.context_masking = enabled;
        options.settings.compaction = Some(json!({"reserveTokens":1024,"keepRecentTokens":256}));
        options.model.as_mut().unwrap().context_window = 8192;
        h.faux.set_responses(vec![
            faux_assistant_message("Summary of the original task and observations").into(),
            faux_assistant_message("Retained turn prefix").into(),
        ]);
        let session = AgentSession::create(options).await.unwrap();
        assert_eq!(session.agent.model().context_window, 8192);
        let control = RunControl::new(RunLimits {
            max_model_calls: Some(3),
            ..Default::default()
        });
        *session.core.run_control.lock().unwrap() = Some(control.clone());
        let update = session
            .core
            .compact_before_next_response(Vec::new(), None)
            .await;
        assert!(update.is_some(), "the loop receives the new projection");
        assert_eq!(
            lock(&session.session)
                .entries()
                .iter()
                .any(|entry| entry.entry_type() == "compaction"),
            expected_compaction
        );
        assert_eq!(h.faux.call_count() > 0, expected_compaction);
        assert_eq!(control.model_calls(), h.faux.call_count());
        assert_eq!(control.stopped(), None);
        session.shutdown().await;
    }
}

#[tokio::test]
async fn task_document_exposure_and_masking_share_the_request_boundary() {
    let dir = TempDir::new("masking-document-composition");
    let h = harness();
    let mut manager = new_file_session(&dir);
    prepared(&mut manager);
    let document = serde_json::from_value(
        json!({"id":"plan","kind":"plan","body":"Preserve this exact task plan","state":"active"}),
    )
    .unwrap();
    crate::task_documents::revise_snapshot(&mut manager, None, document).unwrap();
    h.faux
        .set_responses(vec![FauxResponseStep::from_fn(|context, _, _, _| {
            let request = serde_json::to_string(&context.messages()).unwrap();
            assert!(request.contains("Preserve this exact task plan"));
            assert!(request.contains(REPLACEMENT));
            Ok(faux_assistant_message("Both policies applied"))
        })]);
    let mut options = options(&h, manager, &dir.path().to_string_lossy());
    options.settings.context_masking = true;
    options.settings.task_documents = true;
    let session = AgentSession::create(options).await.unwrap();
    assert_eq!(
        session
            .prompt_with_control("Continue", Vec::new(), RunControl::default())
            .await
            .unwrap(),
        RunOutcome::FinishedTurn
    );
    assert_eq!(h.faux.call_count(), 1);
    session.shutdown().await;
}

struct Release(Option<tokio::sync::oneshot::Sender<()>>);

impl Release {
    fn release(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

impl Drop for Release {
    fn drop(&mut self) {
        self.release();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_or_dropped_waiters_cannot_abandon_an_admitted_masking_commit() {
    for after in [false, true] {
        let dir = TempDir::new("masking-cancel");
        let h = harness();
        let mut manager = new_file_session(&dir);
        prepared(&mut manager);
        let mut options = options(&h, manager, &dir.path().to_string_lossy());
        options.settings.context_masking = true;
        let session = Arc::new(AgentSession::create(options).await.unwrap());
        let file = session.session_file().unwrap();
        let published = Arc::new(Mutex::new(0));
        let observed = Arc::clone(&published);
        let _subscription = session.subscribe(move |event| {
            if let SessionEvent::ContextMasking { masked_results, .. } = event {
                *observed.lock().unwrap() += *masked_results;
            }
        });
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let mut release = Release(Some(release));
        let hook: Box<dyn FnOnce() + Send> = Box::new(move || {
            let _ = entered.send(());
            let _ = released.blocking_recv();
        });
        if after {
            session.core.hooks().after_context_masking = Some(hook);
        } else {
            session.core.hooks().before_context_masking = Some(hook);
        }
        let controller = AbortController::new();
        let applying = {
            let core = Arc::clone(&session.core);
            let signal = controller.signal();
            tokio::spawn(async move { core.mask_context(Some(signal)).await })
        };
        tokio::time::timeout(std::time::Duration::from_secs(20), ready)
            .await
            .unwrap()
            .unwrap();
        applying.abort();
        assert!(applying.await.unwrap_err().is_cancelled());
        controller.abort();
        assert!(
            session
                .prompt("Cannot overtake the commit", Vec::new())
                .await
                .unwrap_err()
                .contains("context")
        );
        assert_eq!(*published.lock().unwrap(), 0);
        release.release();
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            session.core.wait_for_idle(),
        )
        .await
        .unwrap();
        assert_eq!(*published.lock().unwrap(), if after { 3 } else { 0 });
        assert_eq!(h.faux.call_count(), 0);
        let live = lock(&session.session).build_session_projection();
        assert_eq!(
            serde_json::to_string(&live.messages)
                .unwrap()
                .contains(REPLACEMENT),
            after
        );
        session.shutdown().await;
        assert_eq!(
            model_messages(
                &SessionManager::open(&file, None, None)
                    .unwrap()
                    .build_session_projection()
            ),
            model_messages(&live)
        );
    }
}

#[path = "crash_tests.rs"]
mod crash;
