//! Tests for [`AgentSession::inspect_context`] and [`SessionContextReport`]:
//! direct/codemode-only/inactive tool classification, direct-only `visualize`,
//! model profiles, caller `hidden_tools`, parity with captured provider
//! declarations and section token estimates, and strict non-mutation.

use std::collections::HashMap;
use std::time::Duration;

use bake_ai::TranscriptContext;
use bake_ai::providers::faux::{FauxResponseStep, faux_assistant_message};
use bake_ai::transcript::{get_current_system_message, get_current_tools};
use bake_ai::utils::estimate::estimate_text_tokens;
use serde_json::{Value, json};

use super::control_tests::in_memory;
use super::*;
use crate::test_support::TempDir;

const WAIT: Duration = Duration::from_secs(30);

type Requests = Arc<Mutex<Vec<TranscriptContext>>>;

fn make_tool(name: &str, nested_callable: bool) -> Arc<AgentTool> {
    let mut t = AgentTool::new(
        name,
        name,
        format!("The {name} tool."),
        json!({ "type": "object", "properties": {} }),
        |_, _, _, _, _ctx| Box::pin(async { Ok(bake_agent::AgentToolResult::text("ok")) }),
    );
    t.nested_callable = nested_callable;
    Arc::new(t)
}

fn recording(requests: &Requests) -> FauxResponseStep {
    let requests = Arc::clone(requests);
    FauxResponseStep::from_fn(move |context, _, _, _| {
        requests.lock().unwrap().push(context.clone());
        Ok(faux_assistant_message("done"))
    })
}

async fn send_prompt(session: &AgentSession, text: &str) {
    tokio::time::timeout(WAIT, session.prompt(text, Vec::new()))
        .await
        .expect("prompt timeout")
        .expect("prompt outcome");
}

#[derive(Debug, PartialEq, Eq)]
struct StateSnapshot {
    agent_tools: Vec<String>,
    hidden_declarations: Vec<String>,
    own_descriptions: HashMap<String, String>,
    message_count: usize,
    mcp_started: bool,
}

fn snapshot(session: &AgentSession) -> StateSnapshot {
    StateSnapshot {
        agent_tools: session
            .agent
            .tools()
            .into_iter()
            .map(|tool| tool.name.clone())
            .collect(),
        hidden_declarations: session.hidden_declarations.lock().unwrap().clone(),
        own_descriptions: session.codemode.own_descriptions.lock().unwrap().clone(),
        message_count: session.messages().len(),
        mcp_started: session.mcp.get().is_some(),
    }
}

fn assert_matches_captured(
    report: &SessionContextReport,
    context: &TranscriptContext,
    exact_tool_order: bool,
) {
    let tools = get_current_tools(context.messages());
    let mut actual_names: Vec<String> = tools.iter().map(|tool| tool.name.clone()).collect();
    if exact_tool_order {
        assert_eq!(report.direct_tools, actual_names);
    } else {
        let mut expected_names = report.direct_tools.clone();
        expected_names.sort();
        actual_names.sort();
        assert_eq!(expected_names, actual_names);
    }
    let expected_schema_tokens = if tools.is_empty() {
        0
    } else {
        let serialized = serde_json::to_string(&tools).expect("serialize tools");
        usize::try_from(estimate_text_tokens(&serialized)).unwrap_or(usize::MAX)
    };
    assert_eq!(report.tool_schema_tokens, expected_schema_tokens);

    let captured_sections: Vec<(String, usize)> = get_current_system_message(context.messages())
        .and_then(|system| system.sections)
        .map(|sections| {
            sections
                .0
                .into_iter()
                .map(|(name, body)| {
                    let body = body.unwrap_or_default();
                    (
                        name,
                        usize::try_from(estimate_text_tokens(&body)).unwrap_or(usize::MAX),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(report.sections, captured_sections);
}

fn base_options(h: &super::tests::Harness, dir: &TempDir) -> AgentSessionOptions {
    super::tests::options(h, in_memory(), &dir.path().to_string_lossy())
}

fn settings(value: Value) -> Settings {
    Settings::from_object(value.as_object().expect("object"))
}

#[tokio::test]
async fn direct_mode_matches_captured_provider_request_and_does_not_mutate() {
    let dir = TempDir::new("inspect-context-direct");
    let h = super::tests::harness();
    let requests: Requests = Arc::default();
    h.faux.set_responses(vec![recording(&requests)]);

    let mut options = base_options(&h, &dir);
    options.tools = vec![
        make_tool("read", true),
        make_tool("bash", true),
        make_tool("visualize", false),
        make_tool("grep", true),
        make_tool("ls", true),
    ];
    options.active_tool_names = ["read", "bash", "visualize", "codemode"]
        .map(str::to_owned)
        .to_vec();

    let session = AgentSession::create(options).await.expect("session");
    let before = snapshot(&session);

    let report = session.inspect_context().expect("inspect");
    assert_eq!(
        report.direct_tools,
        ["read", "bash", "visualize", "codemode"]
    );
    assert_eq!(report.codemode_tools, ["read", "bash"]);
    assert_eq!(report.inactive_tools, ["grep", "ls"]);
    assert!(report.tool_schema_tokens > 0);
    assert!(!report.sections.is_empty());
    assert_eq!(snapshot(&session), before);
    assert_eq!(session.inspect_context().expect("inspect again"), report);
    assert_eq!(snapshot(&session), before);

    send_prompt(&session, "hello").await;
    let captured = requests.lock().unwrap().clone();
    assert_eq!(captured.len(), 1);
    assert_matches_captured(&report, &captured[0], true);

    session.shutdown().await;
}

#[tokio::test]
async fn codemode_only_and_model_profile_switch_match_captured_provider_request() {
    let h = super::control_tests::harness();
    let requests: Requests = Arc::default();
    h.faux
        .set_responses(vec![recording(&requests), recording(&requests)]);

    let session = h
        .session(
            in_memory(),
            settings(json!({
                "modelProfiles": {
                    "faux-1": {
                        "tools": ["+codemode", "-ls"],
                        "codemode": { "mode": "only" },
                        "prompt": "Batch reads through codemode."
                    },
                    "faux-2": {
                        "tools": ["+codemode", "-grep"]
                    }
                }
            })),
            None,
            vec![
                make_tool("read", true),
                make_tool("ls", true),
                make_tool("grep", true),
                make_tool("visualize", false),
            ],
        )
        .await;

    let report_1 = session.inspect_context().expect("inspect faux-1");
    assert_eq!(report_1.direct_tools, ["visualize", "codemode"]);
    assert_eq!(report_1.codemode_tools, ["read", "grep"]);
    assert_eq!(report_1.inactive_tools, ["ls"]);
    assert!(
        report_1
            .sections
            .iter()
            .any(|(name, tokens)| name == "model_notes" && *tokens > 0)
    );

    send_prompt(&session, "first").await;

    session
        .set_model(h.model("faux-2"), false)
        .await
        .expect("switch to faux-2");
    let before = snapshot(&session);

    let report_2 = session.inspect_context().expect("inspect faux-2");
    assert_eq!(
        report_2.direct_tools,
        ["read", "ls", "visualize", "codemode"]
    );
    assert_eq!(report_2.codemode_tools, ["read", "ls"]);
    assert_eq!(report_2.inactive_tools, ["grep"]);
    assert!(
        !report_2
            .sections
            .iter()
            .any(|(name, _)| name == "model_notes")
    );
    assert_eq!(snapshot(&session), before);

    send_prompt(&session, "second").await;
    let captured = requests.lock().unwrap().clone();
    assert_eq!(captured.len(), 2);
    assert_matches_captured(&report_1, &captured[0], true);
    assert_matches_captured(&report_2, &captured[1], false);

    session.shutdown().await;
}

#[tokio::test]
async fn inactive_codemode_leaves_codemode_tools_empty_and_lists_codemode_inactive() {
    let dir = TempDir::new("inspect-context-no-codemode");
    let h = super::tests::harness();
    let requests: Requests = Arc::default();
    h.faux.set_responses(vec![recording(&requests)]);

    let mut options = base_options(&h, &dir);
    options.tools = vec![make_tool("read", true), make_tool("grep", true)];
    options.active_tool_names = vec!["read".into()];

    let session = AgentSession::create(options).await.expect("session");
    let report = session.inspect_context().expect("inspect");
    assert_eq!(report.direct_tools, ["read"]);
    assert!(report.codemode_tools.is_empty());
    assert_eq!(report.inactive_tools, ["grep", "codemode"]);

    send_prompt(&session, "hi").await;
    let captured = requests.lock().unwrap().clone();
    assert_eq!(captured.len(), 1);
    assert_matches_captured(&report, &captured[0], true);

    session.shutdown().await;
}

#[tokio::test]
async fn caller_hidden_tools_affects_rules_section_without_suppressing_provider_declarations() {
    let dir = TempDir::new("inspect-context-caller-hidden");
    let h = super::tests::harness();
    let requests: Requests = Arc::default();
    h.faux.set_responses(vec![recording(&requests)]);

    let mut options = base_options(&h, &dir);
    options.tools = vec![make_tool("read", true), make_tool("bash", true)];
    options.active_tool_names = ["read", "bash"].map(str::to_owned).to_vec();
    options.tool_prompts = vec![ToolPrompt {
        name: "read".into(),
        snippet: None,
        guidelines: vec!["Prefer read over shell cat for every single file inspection.".into()],
    }];
    options.system_prompt.hidden_tools = vec!["read".into()];

    let session = AgentSession::create(options).await.expect("session");
    let report = session.inspect_context().expect("inspect");
    assert_eq!(report.direct_tools, ["read", "bash"]);

    send_prompt(&session, "check").await;
    let captured = requests.lock().unwrap().clone();
    assert_eq!(captured.len(), 1);
    assert_matches_captured(&report, &captured[0], true);

    session.shutdown().await;
}
