//! `/share`'s export and `/bug`'s bundle and record ([`super::share`]):
//! Pi v1.3.0 `exportSessionToHtml`'s refusals, `buildBundle`, and
//! `recordInSession`, whose entry never reaches the model.

use std::sync::{Arc, Mutex};

use bake_ai::providers::faux::{FauxResponseStep, faux_assistant_message};
use serde_json::{Value, json};

use super::control_tests::{file_session, harness, in_memory};
use super::share::{BugReportRequest, EXPORT_IN_MEMORY, EXPORT_NOTHING_YET};
use super::*;
use crate::bug_report::{BUG_REPORT_ENTRY_TYPE, Delivery};
use crate::test_support::TempDir;

fn request(include_session: bool) -> BugReportRequest<'static> {
    BugReportRequest {
        hint: Some("the edit failed"),
        include_session,
        global_settings: Some(json!({"cliproxyapi": {"apiKey": "sk-hidden"}})),
        environment: json!({"version": "test"}),
        home_dir: Some("/home/someone".into()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unsaved_or_unwritten_session_is_not_exported() {
    let h = harness();
    let memory = h
        .session(in_memory(), Settings::default(), None, Vec::new())
        .await;
    assert_eq!(
        memory.export_html().expect_err("in memory"),
        EXPORT_IN_MEMORY
    );
    memory.shutdown().await;

    let dir = TempDir::new("share-unwritten");
    let fresh = h
        .session(file_session(&dir), Settings::default(), None, Vec::new())
        .await;
    // The file is written with the first message, as Pi's is.
    assert!(!fresh.session_file().expect("saved").exists());
    assert_eq!(
        fresh.export_html().expect_err("unwritten"),
        EXPORT_NOTHING_YET
    );
    fresh.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_export_carries_the_saved_session_its_prompt_and_tools() {
    let dir = TempDir::new("share-export");
    let h = harness();
    h.faux
        .set_responses(vec![faux_assistant_message("an answer").into()]);
    let tool = Arc::new(AgentTool::new(
        "noop",
        "noop",
        "Does nothing",
        json!({ "type": "object" }),
        |_, _, _, _| Box::pin(async { Ok(bake_agent::AgentToolResult::default()) }),
    ));
    let session = h
        .session(file_session(&dir), Settings::default(), None, vec![tool])
        .await;
    session
        .prompt("share me", Vec::new())
        .await
        .expect("prompt");
    let html = session.export_html().expect("export");
    let start = html
        .find(r#"<script id="session-data" type="application/json">"#)
        .expect("data")
        + r#"<script id="session-data" type="application/json">"#.len();
    let end = start + html[start..].find("</script>").expect("end");
    use base64::Engine;
    let data: Value = serde_json::from_slice(
        &base64::engine::general_purpose::STANDARD
            .decode(&html[start..end])
            .expect("base64"),
    )
    .expect("json");
    assert_eq!(data["header"]["id"], session.session_id());
    assert_eq!(data["leafId"].as_str(), session.leaf_id().as_deref());
    assert_eq!(data["systemPrompt"], session.agent().system_prompt());
    assert_eq!(data["tools"][0]["name"], "noop");
    let text = data["entries"].to_string();
    assert!(text.contains("share me") && text.contains("an answer"));
    session.shutdown().await;
}

// Pi `buildBundle`: the branch is included only when asked, and the
// metadata is redacted either way.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_bundle_includes_the_branch_only_when_asked() {
    let h = harness();
    h.faux
        .set_responses(vec![faux_assistant_message("reply").into()]);
    let session = h
        .session(in_memory(), Settings::default(), None, Vec::new())
        .await;
    session
        .prompt("question", Vec::new())
        .await
        .expect("prompt");
    let without = session.bug_report_bundle(request(false));
    assert!(without.session_jsonl.is_none());
    assert_eq!(without.metadata["hint"], "the edit failed");
    assert_eq!(
        without.metadata["session"]["messageCount"],
        session.messages().len()
    );
    assert_eq!(without.metadata["model"]["id"], "faux-1");
    assert_eq!(without.metadata["provider"]["authConfigured"], true);
    assert!(!without.metadata.to_string().contains("sk-hidden"));
    let with = session.bug_report_bundle(request(true));
    let jsonl = with.session_jsonl.as_deref().expect("branch");
    assert!(jsonl.contains("question") && jsonl.contains("reply"));
    assert!(jsonl.contains("\"customType\":\"pi.share\""));
    assert_eq!(with.metadata["session"]["cwd"], "/tmp");
    // The issue's report never lists the directory.
    assert_eq!(with.cwd, "/tmp");
    assert_eq!(with.home_dir.as_deref(), Some("/home/someone"));
    assert!(with.public_metadata()["session"].get("cwd").is_none());
    session.shutdown().await;
}

// Pi `recordInSession`, and Bake: the record is written through the
// session's write boundary and never reaches the model's context.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_record_is_written_but_never_sent_to_the_model() {
    let h = harness();
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let capture = |reply: &'static str| {
        let seen = Arc::clone(&seen);
        FauxResponseStep::from_fn(move |context, _, _, _| {
            seen.lock()
                .expect("seen")
                .push(serde_json::to_string(context).expect("context"));
            Ok(faux_assistant_message(reply))
        })
    };
    h.faux.set_responses(vec![capture("one"), capture("two")]);
    let session = h
        .session(in_memory(), Settings::default(), None, Vec::new())
        .await;
    session.prompt("first", Vec::new()).await.expect("first");
    let before = lock(&session.session)
        .build_session_context()
        .messages
        .len();
    let bundle = session.bug_report_bundle(request(false));
    session
        .record_bug_report(bundle.entry_data(Delivery::Issue {
            url: "https://github.com/Justar96/bake/issues/7",
        }))
        .await
        .expect("recorded");
    let recorded: Vec<Value> = lock(&session.session)
        .entries()
        .iter()
        .filter(|entry| {
            entry.get("customType").and_then(Value::as_str) == Some(BUG_REPORT_ENTRY_TYPE)
        })
        .map(|entry| entry.get("data").cloned().unwrap_or(Value::Null))
        .collect();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["delivery"], "issue");
    assert_eq!(recorded[0]["hint"], "the edit failed");
    let context = lock(&session.session).build_session_context();
    assert_eq!(context.messages.len(), before);
    let live = session.messages();
    let converted = crate::session::messages::convert_to_llm(&live);
    assert!(!format!("{converted:?}").contains("issues/7"));
    session.prompt("second", Vec::new()).await.expect("second");
    let requests = seen.lock().expect("seen").clone();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].contains("second"));
    assert!(!requests[1].contains(BUG_REPORT_ENTRY_TYPE));
    assert!(!requests[1].contains("issues/7"));
    assert!(!requests[1].contains("the edit failed"));
    session.shutdown().await;
    assert!(session.record_bug_report(json!({})).await.is_err());
}
