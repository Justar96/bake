//! The live port against a real [`AgentSession`] over `bake-ai`'s faux
//! provider: what the composer's requests do and the updates the view
//! receives, in order. Each test owns its session (in memory) and Tokio
//! runtime; waits are on received updates, never on time alone.

use std::sync::Arc;
use std::sync::mpsc::{self as std_mpsc, Receiver};
use std::time::Duration;

use bake_agent::{AgentTool, AgentToolResult};
use bake_ai::providers::faux::{
    FauxProvider, FauxProviderOptions, FauxResponseStep, faux_assistant_blocks,
    faux_assistant_message, faux_text, faux_tool_call,
};
use bake_ai::{ApiRegistry, StopReason};
use bake_coding_agent::agent_session::AgentSessionOptions;
use bake_coding_agent::auth_storage::AuthStorage;
use bake_coding_agent::model_registry::ModelRegistry;
use bake_coding_agent::model_registry::config::ModelConfig;
use bake_coding_agent::session::{NewSessionOptions, SessionManager};
use bake_coding_agent::settings::Settings;
use bake_coding_agent::system_prompt::SystemPromptOptions;
use bake_tui_view::paste::{Image, ImageData};
use bake_tui_view::runtime::{Pending, Prompt, Submission, Target};
use bake_tui_view::transcript::CallState;
use serde_json::json;
use tokio::sync::Notify;

use super::*;

/// How long a test waits for an update before failing.
const PATIENCE: Duration = Duration::from_secs(20);

struct Harness {
    runtime: tokio::runtime::Runtime,
    faux: Arc<FauxProvider>,
    port: Option<LivePort>,
    updates: Receiver<RuntimeUpdate>,
}

fn harness(tools: Vec<Arc<AgentTool>>, prompts: Vec<(String, Vec<ImageContent>)>) -> Harness {
    harness_with_key(tools, prompts, Some("test-key"))
}

fn harness_with_key(
    tools: Vec<Arc<AgentTool>>,
    prompts: Vec<(String, Vec<ImageContent>)>,
    key: Option<&str>,
) -> Harness {
    let (runtime, faux, live) = setup(tools, prompts, key);
    let (sender, updates) = std_mpsc::channel();
    let mut port = LivePort::start(live, move |update| sender.send(update).is_ok());
    port.begin();
    Harness {
        runtime,
        faux,
        port: Some(port),
        updates,
    }
}

fn setup(
    tools: Vec<Arc<AgentTool>>,
    prompts: Vec<(String, Vec<ImageContent>)>,
    key: Option<&str>,
) -> (tokio::runtime::Runtime, Arc<FauxProvider>, LiveSession) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
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
                "models": [
                    { "id": "faux-1", "contextWindow": 1000 },
                    { "id": "faux-2", "contextWindow": 1000, "reasoning": true },
                ],
            }),
            key.map(str::to_owned),
        )
        .expect("faux provider");
    let registry = Arc::new(registry);
    let active: Vec<String> = tools.iter().map(|tool| tool.name.clone()).collect();
    let session = runtime
        .block_on(AgentSession::create(AgentSessionOptions {
            session: SessionManager::in_memory(
                Some("/tmp"),
                NewSessionOptions::default(),
                Vec::new(),
            )
            .expect("session"),
            model: registry.model("faux", "faux-1"),
            registry: registry.into(),
            settings_store: None,
            selection: None,
            apis,
            settings: Settings::default(),
            thinking_level: None,
            tools,
            active_tool_names: active,
            tool_prompts: Vec::new(),
            system_prompt: SystemPromptOptions::new("/tmp"),
            docs_command: "bake-rs docs".into(),
            plugins: bake_coding_agent::plugins::PluginRegistry::default(),
            work: None,
            jobs: None,
        }))
        .expect("agent session");
    let live = LiveSession {
        session,
        runtime: runtime.handle().clone(),
        prompts,
        notes: Vec::new(),
        approvals: None,
        home: None,
        factory: None,
        commit_gate: None,
    };
    (runtime, faux, live)
}

impl Harness {
    fn port(&self) -> &LivePort {
        self.port.as_ref().expect("port")
    }

    fn submit(&self, submission: Submission) {
        self.port().request(Request::Submit(submission.into()));
    }

    fn submit_id(&self, id: u64, submission: Submission) {
        self.port().request(Request::Submit(Prompt {
            id,
            submission,
            images: Vec::new(),
        }));
    }

    /// Every update up to and including the first that `last` accepts.
    fn until(&self, last: impl Fn(&RuntimeUpdate) -> bool) -> Vec<RuntimeUpdate> {
        let mut seen = Vec::new();
        loop {
            let update = self
                .updates
                .recv_timeout(PATIENCE)
                .unwrap_or_else(|_| panic!("no matching update; saw {seen:#?}"));
            let done = last(&update);
            seen.push(update);
            if done {
                return seen;
            }
        }
    }

    /// Waits until the provider has taken `count` requests.
    fn wait_for_requests(&self, count: u64) {
        let deadline = std::time::Instant::now() + PATIENCE;
        while self.faux.call_count() < count {
            assert!(
                std::time::Instant::now() < deadline,
                "the request never started"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn until_turn_end(&self) -> Vec<RuntimeUpdate> {
        self.until(|update| matches!(update, RuntimeUpdate::TurnEnded(_)))
    }

    /// Stops the port, which shuts the session down, then checks that no
    /// update arrives afterwards.
    fn stop(mut self) {
        self.port.take().expect("port").stop().expect("stop");
        while let Ok(update) = self.updates.try_recv() {
            assert!(
                !matches!(update, RuntimeUpdate::TurnStarted { .. }),
                "a turn started during shutdown"
            );
        }
        drop(self.runtime);
    }
}

fn committed(updates: &[RuntimeUpdate]) -> Vec<Row> {
    updates
        .iter()
        .filter_map(|update| match update {
            RuntimeUpdate::Commit(rows) => Some(rows.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn echo_tool() -> Arc<AgentTool> {
    Arc::new(AgentTool::new(
        "echo",
        "Echo",
        "Echoes",
        json!({"type": "object", "properties": {}}),
        |_, _, _, _| Box::pin(async { Ok(AgentToolResult::text("echoed\nline two")) }),
    ))
}

#[test]
fn a_prompt_streams_its_answer_commits_it_once_and_reports_the_status() {
    let h = harness(Vec::new(), Vec::new());
    let first = h.until(|update| matches!(update, RuntimeUpdate::Status(_)));
    assert_eq!(
        first,
        [RuntimeUpdate::Status(SessionStatus {
            model: Some("faux/faux-1".into()),
            thinking: None,
            context: None,
        })]
    );
    h.faux
        .set_responses(vec![faux_assistant_message("Hello there.").into()]);
    h.submit(Submission::FollowUp("hi".into()));
    let updates = h.until_turn_end();
    assert!(matches!(
        updates
            .iter()
            .find(|u| !matches!(u, RuntimeUpdate::Pending(_))),
        Some(RuntimeUpdate::TurnStarted { .. })
    ));
    assert_eq!(
        committed(&updates),
        [Row::User("hi".into()), Row::Answer("Hello there.".into())]
    );
    assert_eq!(
        updates.last(),
        Some(&RuntimeUpdate::TurnEnded(Outcome::Completed))
    );
    // A second prompt is a new turn in the same session.
    h.faux
        .set_responses(vec![faux_assistant_message("Again.").into()]);
    h.submit(Submission::FollowUp("more".into()));
    let updates = h.until_turn_end();
    assert_eq!(
        committed(&updates),
        [Row::User("more".into()), Row::Answer("Again.".into())]
    );
    h.stop();
}

/// A background job that settles while the session is idle opens a turn
/// of its own: the notice shows as an information row, and the model's
/// answer follows it.
#[test]
fn an_idle_job_notice_opens_a_turn_that_shows_it() {
    let (runtime, faux, live) = setup(Vec::new(), Vec::new(), Some("test-key"));
    let jobs = live.session.jobs().clone();
    let (sender, updates) = std_mpsc::channel();
    let mut port = LivePort::start(live, move |update| sender.send(update).is_ok());
    port.begin();
    let h = Harness {
        runtime,
        faux,
        port: Some(port),
        updates,
    };
    h.until(|update| matches!(update, RuntimeUpdate::Status(_)));
    h.faux
        .set_responses(vec![faux_assistant_message("Noted.").into()]);
    let _guard = h.runtime.enter();
    jobs.start(
        bake_coding_agent::jobs::JobSpec {
            kind: "bash",
            label: "make".into(),
            output: bake_coding_agent::jobs::OutputMode::Stream,
        },
        |_| async { bake_coding_agent::jobs::JobOutcome::completed("exit code: 0") },
    )
    .expect("admitted");
    let updates = h.until_turn_end();
    assert_eq!(
        committed(&updates),
        [
            Row::Info(
                "background job bash-1 (bash: make) finished [status: completed, exit code: 0]. Read its output with job_output.".into()
            ),
            Row::Answer("Noted.".into()),
        ]
    );
    assert_eq!(
        updates.last(),
        Some(&RuntimeUpdate::TurnEnded(Outcome::Completed))
    );
    assert_eq!(h.faux.call_count(), 1);
    drop(_guard);
    h.stop();
}

#[test]
fn a_tool_call_runs_and_settles_in_the_turn_it_belongs_to() {
    let h = harness(vec![echo_tool()], Vec::new());
    h.faux.set_responses(vec![
        faux_assistant_blocks(
            vec![
                faux_text("Calling."),
                faux_tool_call("echo", json!({}), Some("call-1")),
            ],
            StopReason::ToolUse,
        )
        .into(),
        faux_assistant_message("Done.").into(),
    ]);
    h.submit(Submission::FollowUp("use the tool".into()));
    let updates = h.until_turn_end();
    // The call shows running while the tool runs, then settles.
    assert!(updates.iter().any(|update| matches!(
        update,
        RuntimeUpdate::Live(rows) if rows.iter().any(|row| matches!(row, Row::Call { state: CallState::Running, .. }))
    )));
    assert!(updates.contains(&RuntimeUpdate::Phase("running echo".into())));
    assert_eq!(
        committed(&updates),
        [
            Row::User("use the tool".into()),
            Row::Answer("Calling.".into()),
            Row::Call {
                tool: "Echo".into(),
                argument: String::new(),
                state: CallState::Done,
                summary: None,
                output: vec!["echoed".into(), "line two".into()],
            },
            Row::Answer("Done.".into()),
        ]
    );
    h.stop();
}

#[test]
fn command_line_prompts_run_in_order_and_wait_in_the_panel() {
    let h = harness(
        Vec::new(),
        vec![("first".into(), Vec::new()), ("second".into(), Vec::new())],
    );
    h.faux.set_responses(vec![
        faux_assistant_message("one").into(),
        faux_assistant_message("two").into(),
    ]);
    let updates = h.until_turn_end();
    assert!(updates.contains(&RuntimeUpdate::Pending(vec![Pending {
        text: "second".into(),
        target: Target::NextTurn,
    }])));
    assert_eq!(
        committed(&updates),
        [Row::User("first".into()), Row::Answer("one".into())]
    );
    let updates = h.until_turn_end();
    assert_eq!(
        committed(&updates),
        [Row::User("second".into()), Row::Answer("two".into())]
    );
    h.stop();
}

/// A response that waits for `gate`, or for its request to be aborted,
/// before answering `text`.
fn gated(gate: &Arc<Notify>, text: &'static str) -> FauxResponseStep {
    let gate = Arc::clone(gate);
    FauxResponseStep::Factory(Arc::new(move |_, options, _, _| {
        let gate = Arc::clone(&gate);
        let signal = options.base.signal.clone();
        Box::pin(async move {
            match signal {
                Some(signal) => tokio::select! {
                    () = gate.notified() => {}
                    () = signal.cancelled() => {}
                },
                None => gate.notified().await,
            }
            Ok(faux_assistant_message(text))
        })
    }))
}

/// A tool that runs until `gate` opens.
fn gate_tool(gate: &Arc<Notify>) -> Arc<AgentTool> {
    let gate = Arc::clone(gate);
    Arc::new(AgentTool::new(
        "gate",
        "Gate",
        "Waits",
        json!({"type": "object", "properties": {}}),
        move |_, _, signal, _| {
            let gate = Arc::clone(&gate);
            Box::pin(async move {
                match signal {
                    Some(signal) => tokio::select! {
                        () = gate.notified() => Ok(AgentToolResult::text("opened")),
                        () = signal.cancelled() => Err("cancelled".into()),
                    },
                    None => {
                        gate.notified().await;
                        Ok(AgentToolResult::text("opened"))
                    }
                }
            })
        },
    ))
}

#[test]
fn a_steer_joins_the_running_turn_at_its_next_step_and_leaves_the_panel() {
    let gate = Arc::new(Notify::new());
    let h = harness(vec![gate_tool(&gate)], Vec::new());
    h.faux.set_responses(vec![
        faux_assistant_blocks(
            vec![faux_tool_call("gate", json!({}), Some("call-1"))],
            StopReason::ToolUse,
        )
        .into(),
        faux_assistant_message("steered").into(),
    ]);
    h.submit(Submission::FollowUp("start".into()));
    h.until(|update| update == &RuntimeUpdate::Phase("running gate".into()));
    h.submit(Submission::Steer("turn left".into()));
    h.until(|update| {
        update
            == &RuntimeUpdate::Pending(vec![Pending {
                text: "turn left".into(),
                target: Target::NextStep,
            }])
    });
    gate.notify_one();
    let updates = h.until_turn_end();
    assert!(updates.contains(&RuntimeUpdate::Pending(Vec::new())));
    let rows = committed(&updates);
    assert!(
        matches!(&rows[..], [Row::Call { state: CallState::Done, .. }, Row::User(steer), Row::Answer(answer)]
            if steer == "turn left" && answer == "steered"),
        "{rows:#?}"
    );
    h.stop();
}

#[test]
fn a_cancel_during_a_tool_returns_waiting_prompts_to_the_draft_and_interrupts_the_turn() {
    let gate = Arc::new(Notify::new());
    let h = harness(vec![gate_tool(&gate)], Vec::new());
    h.faux.set_responses(vec![
        faux_assistant_blocks(
            vec![faux_tool_call("gate", json!({}), Some("call-1"))],
            StopReason::ToolUse,
        )
        .into(),
    ]);
    h.submit(Submission::FollowUp("long".into()));
    h.until(|update| update == &RuntimeUpdate::Phase("running gate".into()));
    h.submit(Submission::Steer("queued".into()));
    h.until(|update| matches!(update, RuntimeUpdate::Pending(items) if !items.is_empty()));
    h.port().request(Request::Cancel);
    let updates = h.until_turn_end();
    assert!(updates.contains(&RuntimeUpdate::Returned(vec!["queued".into()])));
    assert!(
        committed(&updates).iter().any(|row| matches!(
            row,
            Row::Call {
                state: CallState::Failed,
                ..
            }
        )),
        "{updates:#?}"
    );
    assert_eq!(
        updates.last(),
        Some(&RuntimeUpdate::TurnEnded(Outcome::Interrupted))
    );
    // The dropped steer does not run afterwards.
    h.faux
        .set_responses(vec![faux_assistant_message("fresh").into()]);
    h.submit(Submission::FollowUp("next".into()));
    let updates = h.until_turn_end();
    assert_eq!(
        committed(&updates),
        [Row::User("next".into()), Row::Answer("fresh".into())]
    );
    h.stop();
}

#[test]
fn a_cancel_while_the_response_streams_interrupts_it() {
    let h = harness(Vec::new(), Vec::new());
    let gate = Arc::new(Notify::new());
    h.faux.set_responses(vec![gated(&gate, "never")]);
    h.submit(Submission::FollowUp("long".into()));
    h.until(|update| matches!(update, RuntimeUpdate::Commit(rows) if rows == &[Row::User("long".into())]));
    h.wait_for_requests(1);
    h.port().request(Request::Cancel);
    let updates = h.until_turn_end();
    assert_eq!(
        updates.last(),
        Some(&RuntimeUpdate::TurnEnded(Outcome::Interrupted))
    );
    h.stop();
}

#[test]
fn quitting_mid_turn_shuts_the_session_down_before_stop_returns() {
    let h = harness(Vec::new(), Vec::new());
    let gate = Arc::new(Notify::new());
    h.faux.set_responses(vec![gated(&gate, "never")]);
    h.submit(Submission::FollowUp("long".into()));
    h.until(|update| matches!(update, RuntimeUpdate::Commit(rows) if rows == &[Row::User("long".into())]));
    // `stop` returns only after the session's shutdown, and no turn
    // starts or ends after it.
    h.stop();
}

#[test]
fn pi_commands_are_refused_and_the_draft_comes_back() {
    let h = harness(Vec::new(), Vec::new());
    h.until(|update| matches!(update, RuntimeUpdate::Status(_)));
    h.submit_id(4, Submission::FollowUp("/clone gpt".into()));
    let updates = h.until(|update| matches!(update, RuntimeUpdate::Refused { .. }));
    assert!(matches!(
        updates.last(),
        Some(RuntimeUpdate::Refused { id: 4, reason }) if reason.starts_with("/clone is not available")
    ));
    assert!(!updates.iter().any(|u| matches!(
        u,
        RuntimeUpdate::TurnStarted { .. } | RuntimeUpdate::Commit(_)
    )));
    h.stop();
}

#[test]
fn a_prompt_without_an_api_key_is_refused_before_the_agent_takes_it() {
    let h = harness_with_key(Vec::new(), Vec::new(), None);
    h.submit_id(9, Submission::FollowUp("hello".into()));
    let updates = h.until_turn_end();
    assert!(
        updates.iter().any(|u| matches!(
            u,
            RuntimeUpdate::Refused { id: 9, reason } if reason.starts_with("No API key found")
        )),
        "{updates:#?}"
    );
    assert!(committed(&updates).is_empty(), "{updates:#?}");
    assert_eq!(
        updates.last(),
        Some(&RuntimeUpdate::TurnEnded(Outcome::Failed))
    );
    h.stop();
}

#[test]
fn a_prompt_sent_after_a_cancel_waits_for_its_own_turn_and_the_abort() {
    let h = harness(Vec::new(), Vec::new());
    let gate = Arc::new(Notify::new());
    h.faux.set_responses(vec![
        gated(&gate, "never"),
        faux_assistant_message("second turn").into(),
    ]);
    h.submit_id(1, Submission::FollowUp("long".into()));
    h.until(|update| update == &RuntimeUpdate::Admitted(1));
    h.wait_for_requests(1);
    h.port().request(Request::Cancel);
    // Sent before the cancelled turn ended, as a steer would be.
    h.submit_id(2, Submission::Steer("after".into()));
    let updates = h.until_turn_end();
    assert_eq!(
        updates.last(),
        Some(&RuntimeUpdate::TurnEnded(Outcome::Interrupted))
    );
    assert!(!updates.contains(&RuntimeUpdate::Admitted(2)));
    // It runs as the next turn, which the earlier abort does not stop.
    let updates = h.until_turn_end();
    assert!(
        updates.contains(&RuntimeUpdate::Admitted(2)),
        "{updates:#?}"
    );
    assert_eq!(
        committed(&updates),
        [Row::User("after".into()), Row::Answer("second turn".into())]
    );
    assert_eq!(
        updates.last(),
        Some(&RuntimeUpdate::TurnEnded(Outcome::Completed))
    );
    h.stop();
}

#[test]
fn a_prompt_the_session_refuses_returns_to_the_draft_with_its_error() {
    let h = harness(Vec::new(), Vec::new());
    // A provider failure is admitted: the prompt is in the transcript and
    // the turn fails with the error.
    h.faux
        .set_responses(vec![FauxResponseStep::from_fn(|_, _, _, _| {
            Err("provider exploded".into())
        })]);
    h.submit(Submission::FollowUp("boom".into()));
    let updates = h.until_turn_end();
    assert_eq!(
        updates.last(),
        Some(&RuntimeUpdate::TurnEnded(Outcome::Failed))
    );
    assert!(
        committed(&updates)
            .iter()
            .any(|row| matches!(row, Row::Error(message) if message.contains("provider exploded"))),
        "{updates:#?}"
    );
    assert!(
        !updates
            .iter()
            .any(|u| matches!(u, RuntimeUpdate::Returned(_)))
    );
    h.stop();
}

#[test]
fn images_go_with_an_idle_prompt() {
    let h = harness(Vec::new(), Vec::new());
    let seen = Arc::new(std::sync::Mutex::new(0usize));
    let count = Arc::clone(&seen);
    h.faux
        .set_responses(vec![FauxResponseStep::from_fn(move |context, _, _, _| {
            let images = context
                .messages()
                .iter()
                .filter_map(|message| match message {
                    bake_ai::Message::User(user) => Some(user),
                    _ => None,
                })
                .flat_map(|user| match &user.content {
                    bake_ai::UserContent::Blocks(blocks) => blocks.clone(),
                    bake_ai::UserContent::Text(_) => Vec::new(),
                })
                .filter(|block| matches!(block, bake_ai::UserContentBlock::Image(_)))
                .count();
            *count.lock().unwrap() = images;
            Ok(faux_assistant_message("I see it."))
        })]);
    // A 1×1 PNG.
    let png = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==",
    )
    .expect("png");
    h.port().request(Request::Submit(Prompt {
        id: 3,
        submission: Submission::FollowUp("look [Image #1]".into()),
        images: vec![Image {
            name: "dot.png".into(),
            media_type: "image/png",
            bytes: png.len() as u64,
            size: Some((1, 1)),
            data: ImageData::new(png),
        }],
    }));
    let updates = h.until_turn_end();
    assert_eq!(*seen.lock().unwrap(), 1);
    assert_eq!(
        committed(&updates)[0],
        Row::User("look [Image #1] (+1 image)".into())
    );
    h.stop();
}

#[test]
fn a_port_that_never_began_runs_nothing_and_shuts_the_session_down() {
    let (runtime, faux, live) = setup(Vec::new(), vec![("hello".into(), Vec::new())], Some("k"));
    let (sender, updates) = std_mpsc::channel();
    let port = LivePort::start(live, move |update| sender.send(update).is_ok());
    port.stop().expect("stop");
    assert_eq!(faux.call_count(), 0);
    assert_eq!(updates.try_iter().count(), 0);
    drop(runtime);
}

#[test]
fn a_loop_that_stops_taking_updates_stops_the_driver() {
    let (runtime, faux, live) = setup(Vec::new(), Vec::new(), Some("k"));
    let gate = Arc::new(Notify::new());
    faux.set_responses(vec![gated(&gate, "never")]);
    let (sender, updates) = std_mpsc::channel();
    let mut port = LivePort::start(live, move |update| sender.send(update).is_ok());
    port.begin();
    port.request(Request::Submit(Submission::FollowUp("long".into()).into()));
    let deadline = std::time::Instant::now() + PATIENCE;
    while faux.call_count() < 1 {
        assert!(
            std::time::Instant::now() < deadline,
            "the request never started"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    drop(updates);
    // The next update fails to send; the driver shuts the session down,
    // ending the held request, with the port still open.
    port.request(Request::Cancel);
    assert!(port.ended_within(PATIENCE), "the driver kept running");
    port.stop().expect("stop");
    drop(runtime);
}

#[test]
fn a_driver_panic_still_shuts_the_session_down_and_is_reported() {
    let (runtime, faux, live) = setup(Vec::new(), vec![("hello".into(), Vec::new())], Some("k"));
    faux.set_responses(vec![faux_assistant_message("never").into()]);
    let mut port = LivePort::start(live, |update| {
        assert!(!matches!(update, RuntimeUpdate::Status(_)), "a driver bug");
        true
    });
    port.begin();
    let error = port.stop().expect_err("the panic is reported");
    assert!(error.to_string().contains("driver failed"), "{error}");
    assert_eq!(faux.call_count(), 0);
    drop(runtime);
}

#[test]
fn commands_are_refused_but_skills_templates_and_paths_are_not() {
    let templates = ["review".to_owned()];
    assert!(refused_command("/quit", &templates).is_some_and(|r| r.contains("not available")));
    assert!(refused_command("  /clone x", &templates).is_some());
    assert!(
        refused_command("/goal ship it", &templates).is_some_and(|r| r.contains("not a command"))
    );
    assert_eq!(refused_command("/review src", &templates), None);
    assert_eq!(refused_command("/skill:review src", &templates), None);
    assert_eq!(
        refused_command("/home/me/notes.txt is broken", &templates),
        None
    );
    assert_eq!(refused_command("plain", &templates), None);
}

// ---- Approvals (D24) ----
//
// The session's real sandboxed `write` tool under a read-only session asks
// the broker to escalate to workspace-write; what the view answers decides
// whether the file appears. Fixtures follow
// `apps/tui/packages/app/tests/interactions.spec.ts` and
// `packages/interaction/user-approval/tests/approval.spec.ts`.

use bake_coding_agent::permissions::{
    ApprovalBroker, ApprovalPolicy, StandingPermissions, ToolPermissions,
    fs::create_sandboxed_write_tool_definition,
};
use bake_sandbox::SandboxMode;
use bake_sandbox::provider::UnavailableProvider;
use bake_tui_view::runtime::{Approval, ApprovalAnswer};

/// A project directory this test alone owns, removed when dropped.
struct Project(std::path::PathBuf);

impl Project {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "bake-tui-approval-{name}-{}-{}",
            std::process::id(),
            bake_ai::now_ms()
        ));
        std::fs::create_dir(&dir).expect("a fresh project directory");
        Self(dir)
    }

    fn file(&self, name: &str) -> std::path::PathBuf {
        self.0.join(name)
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A started port whose `write` tool asks `broker` under `policy`.
fn approval_harness(project: &Project, policy: ApprovalPolicy) -> (Harness, ApprovalBroker) {
    let broker = ApprovalBroker::new();
    let permissions = ToolPermissions::new(
        Arc::new(StandingPermissions::new(
            SandboxMode::ReadOnly,
            policy,
            "custom",
            &project.0,
            None,
        )),
        Arc::new(broker.clone()),
        Arc::new(UnavailableProvider::new(None)),
    );
    let write = Arc::new(create_sandboxed_write_tool_definition(&project.0, &permissions).tool);
    let (runtime, faux, mut live) = setup(vec![write], Vec::new(), Some("test-key"));
    live.approvals = Some(broker.clone());
    let (sender, updates) = std_mpsc::channel();
    let mut port = LivePort::start(live, move |update| sender.send(update).is_ok());
    port.begin();
    let harness = Harness {
        runtime,
        faux,
        port: Some(port),
        updates,
    };
    (harness, broker)
}

/// A write of `name` that asks to escalate, as call `call_id`.
fn escalating_write(project: &Project, name: &str, call_id: &str) -> FauxResponseStep {
    faux_assistant_blocks(
        vec![faux_tool_call(
            "write",
            json!({
                "path": project.file(name).to_string_lossy(),
                "content": format!("{name} body"),
                "sandbox_permissions": "workspace-write",
                "justification": format!("create {name}"),
            }),
            Some(call_id),
        )],
        StopReason::ToolUse,
    )
    .into()
}

impl Harness {
    /// Waits for the queue to list exactly one request, and returns it.
    fn displayed(&self) -> Approval {
        let updates = self
            .until(|update| matches!(update, RuntimeUpdate::Approvals(queue) if queue.len() == 1));
        let Some(RuntimeUpdate::Approvals(queue)) = updates.last() else {
            unreachable!()
        };
        queue[0].clone()
    }

    fn answer(&self, generation: u64, answer: ApprovalAnswer) {
        self.port().request(Request::Answer { generation, answer });
    }
}

fn committed_text(updates: &[RuntimeUpdate]) -> String {
    format!("{:?}", committed(updates))
}

#[test]
fn an_approval_shows_the_call_and_allows_only_that_call_once() {
    let project = Project::new("once");
    let (h, broker) = approval_harness(&project, ApprovalPolicy::Ask);
    // Two escalating writes with the same call id, one after the other.
    h.faux.set_responses(vec![
        escalating_write(&project, "first.txt", "dup"),
        escalating_write(&project, "second.txt", "dup"),
        faux_assistant_message("Done.").into(),
    ]);
    h.submit(Submission::FollowUp("write two files".into()));
    let first = h.displayed();
    assert_eq!(
        (first.tool.as_str(), first.call_id.as_str()),
        ("write", "dup")
    );
    assert_eq!(
        first.reason,
        "escalate sandbox to workspace-write: create first.txt"
    );
    // The arguments show what would run; the escalation pair is the reason.
    assert_eq!(
        first.arguments,
        format!(
            "path: {}\ncontent: first.txt body",
            project.file("first.txt").display()
        )
    );
    assert!(
        !project.file("first.txt").exists(),
        "the write ran before its answer"
    );
    // A stale or unknown generation settles nothing.
    h.answer(first.generation + 100, ApprovalAnswer::AllowOnce);
    assert_eq!(broker.pending().len(), 1);
    h.answer(first.generation, ApprovalAnswer::AllowOnce);
    let second = h.displayed();
    assert_eq!(second.call_id, "dup");
    assert_ne!(second.generation, first.generation);
    assert_eq!(
        std::fs::read_to_string(project.file("first.txt")).unwrap(),
        "first.txt body"
    );
    // The first prompt's answer, sent again, cannot approve the second.
    h.answer(first.generation, ApprovalAnswer::AllowOnce);
    assert_eq!(broker.pending()[0].generation, second.generation);
    h.answer(second.generation, ApprovalAnswer::Reject);
    let updates = h.until_turn_end();
    assert!(!project.file("second.txt").exists(), "a rejected write ran");
    assert!(
        committed_text(&updates)
            .contains("the user rejected escalating this operation to \\\"workspace-write\\\""),
        "{updates:#?}"
    );
    h.stop();
    assert!(broker.is_closed());
}

#[test]
fn the_never_policy_rejects_without_showing_a_prompt() {
    let project = Project::new("never");
    let (h, _broker) = approval_harness(&project, ApprovalPolicy::Never);
    h.faux.set_responses(vec![
        escalating_write(&project, "never.txt", "call-1"),
        faux_assistant_message("Done.").into(),
    ]);
    h.submit(Submission::FollowUp("write".into()));
    let updates = h.until_turn_end();
    assert!(
        !updates
            .iter()
            .any(|u| matches!(u, RuntimeUpdate::Approvals(queue) if !queue.is_empty())),
        "a prompt was shown: {updates:#?}"
    );
    assert!(!project.file("never.txt").exists());
    assert!(committed_text(&updates).contains("the user rejected escalating"));
    h.stop();
}

#[test]
fn cancelling_the_turn_withdraws_the_prompt_and_a_late_answer_does_nothing() {
    let project = Project::new("cancel");
    let (h, broker) = approval_harness(&project, ApprovalPolicy::Ask);
    h.faux
        .set_responses(vec![escalating_write(&project, "late.txt", "call-1")]);
    h.submit(Submission::FollowUp("write".into()));
    let shown = h.displayed();
    h.port().request(Request::Cancel);
    h.until(|update| update == &RuntimeUpdate::Approvals(Vec::new()));
    h.until_turn_end();
    h.answer(shown.generation, ApprovalAnswer::AllowOnce);
    assert!(broker.pending().is_empty());
    assert!(
        !project.file("late.txt").exists(),
        "a withdrawn request ran"
    );
    h.stop();
    assert!(!project.file("late.txt").exists());
}

#[test]
fn an_answer_that_withdraws_the_prompt_reports_it_cancelled() {
    let project = Project::new("esc");
    let (h, _broker) = approval_harness(&project, ApprovalPolicy::Ask);
    h.faux.set_responses(vec![
        escalating_write(&project, "esc.txt", "call-1"),
        faux_assistant_message("Done.").into(),
    ]);
    h.submit(Submission::FollowUp("write".into()));
    let shown = h.displayed();
    h.answer(shown.generation, ApprovalAnswer::Cancel);
    let updates = h.until_turn_end();
    assert!(!project.file("esc.txt").exists());
    assert!(
        committed_text(&updates)
            .contains("approval for escalating to \\\"workspace-write\\\" was cancelled"),
        "{updates:#?}"
    );
    h.stop();
}

#[test]
fn stopping_with_a_prompt_waiting_cancels_it_and_shuts_down_without_its_effect() {
    let project = Project::new("stop");
    let (h, broker) = approval_harness(&project, ApprovalPolicy::Ask);
    h.faux
        .set_responses(vec![escalating_write(&project, "stop.txt", "call-1")]);
    h.submit(Submission::FollowUp("write".into()));
    let shown = h.displayed();
    // `stop` returns only once the session has shut down.
    h.stop();
    assert!(broker.is_closed());
    assert!(!broker.answer(
        shown.generation,
        bake_coding_agent::permissions::ApprovalAnswer::AllowOnce
    ));
    assert!(
        !project.file("stop.txt").exists(),
        "the write ran after the session stopped"
    );
}

#[test]
fn arguments_show_every_character_that_would_run() {
    let text = super::arguments_text(&json!({
        "command": "printf 'a\\tb'\u{1b}[2J\u{202e}txt",
        "content": "one\r\ntwo",
        "sandbox_permissions": "danger-full-access",
        "justification": "why",
        "limit": 3,
    }));
    assert_eq!(
        text,
        "command: printf 'a\\tb'\\u{1b}[2J\\u{202e}txt\ncontent:\n  one\\u{d}\n  two\nlimit: 3"
    );
}

// Bake: `/compact`'s bar result reads only its own cancellation as an
// interruption. A failed write that a late Esc followed is a failure: the
// command reports the write failure, not `CANCELLED`, and the bar agrees.
#[test]
fn only_the_commands_own_cancellation_interrupts_its_work() {
    assert_eq!(work_outcome(&Ok(commands::Done::Quiet)), Outcome::Completed);
    assert_eq!(
        work_outcome(&Err(commands::CANCELLED.into())),
        Outcome::Interrupted
    );
    let write_failed = "Compaction failed: The compaction could not be written to the session file (disk full), so the session stopped and takes no more prompts. Your draft is kept.";
    assert_eq!(work_outcome(&Err(write_failed.into())), Outcome::Failed);
}

mod commands_live;
mod sessions_live;
