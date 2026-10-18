//! The test kills a running fixture process with `SIGKILL` at each durable
//! checkpoint ([`super::kill_at`]): before a model request (a new session's
//! first prompt, a recoverable run's first request, and the requests a
//! retry, a follow-up, a steer, a compaction, or a job notice opens), after
//! an assistant's tool call is synced and before the tool's first effect,
//! during an effectful and a read-only tool, and before the next step. Each
//! case reopens the file and checks what it holds, what the next request
//! is, and which effects ran, counted in files across both processes.

use super::rerun::{NOTICE, conversation};
use super::*;
use bake_agent::ToolReplay;

/// The file's entries in order: run-journal records as their kind, call,
/// and class; messages as their role and call; other conversation entries
/// as their type.
fn on_disk(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let entry: Value = serde_json::from_str(line).unwrap();
            let event = &entry["data"]["event"];
            let parts = match entry["type"].as_str()? {
                "custom" if entry["customType"] == "bake.run.v1" => {
                    [&event["kind"], &event["tool_call_id"], &event["replay"]]
                }
                "message" => [
                    &entry["message"]["role"],
                    &entry["message"]["toolCallId"],
                    &Value::Null,
                ],
                "custom_message" => [&entry["customType"], &Value::Null, &Value::Null],
                "compaction" | "context_edit" => [&entry["type"], &Value::Null, &Value::Null],
                _ => return None,
            };
            Some(
                parts
                    .iter()
                    .filter_map(|part| part.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        })
        .collect()
}

/// The effects `name` ran, in both processes.
fn effects(killed: &Killed, name: &str) -> usize {
    std::fs::read_to_string(killed.dir.join(name)).map_or(0, |text| text.lines().count())
}

/// Answers the next request with a final message and keeps the request.
fn answering(h: &Harness) -> Arc<Mutex<Vec<TranscriptContext>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    h.faux
        .set_responses(vec![capture(&seen, faux_assistant_message("done"))]);
    seen
}

fn request(seen: &Mutex<Vec<TranscriptContext>>) -> Vec<String> {
    conversation(seen.lock().unwrap()[0].messages())
}

/// The killed session, reopened with the fixture's tools, which now run in
/// this process and count into the same files.
async fn reopen(h: &Harness, killed: &Killed) -> AgentSession {
    AgentSession::create(AgentSessionOptions {
        tools: vec![
            fixture_tool(
                "effect",
                ToolReplay::Never,
                killed.dir.path(),
                Hold::Never,
                None,
            ),
            fixture_tool(
                "probe",
                ToolReplay::Safe,
                killed.dir.path(),
                Hold::Never,
                None,
            ),
        ],
        active_tool_names: vec!["effect".into(), "probe".into()],
        ..options(
            h,
            SessionManager::open(&killed.path, None, None).unwrap(),
            Settings::default(),
        )
    })
    .await
    .unwrap()
}

/// Inspects and resumes the killed run: its disposition before, and the
/// resumption's outcome.
async fn resume(
    session: &AgentSession,
    killed: &Killed,
) -> (RunRecoveryDisposition, Result<RunOutcome, String>) {
    let run_id = killed.run_id.as_deref().unwrap();
    let before = session.inspect_run(run_id).unwrap().disposition;
    let resumed = session
        .resume_run(run_id)
        .await
        .map(|result| result.outcome);
    (before, resumed)
}

const INPUT: [&str; 6] = [
    "started", "system", "message", "user", "message", "admitted",
];

fn input_then(rest: &[&str]) -> Vec<String> {
    INPUT
        .iter()
        .chain(rest)
        .map(|entry| (*entry).to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_before_the_first_request_keeps_the_prompt() {
    // An ordinary prompt: its user message creates the file, as Pi's does.
    let killed = kill_at("first-prompt").await;
    assert_eq!(on_disk(&killed.path), ["system", "user"]);
    let listed = SessionManager::list(
        &killed.dir.path().to_string_lossy(),
        Some(&killed.dir.join("sessions")),
        None,
        None,
    )
    .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].first_message, "perform effect");
    let h = harness(200_000);
    let seen = answering(&h);
    let session = reopen(&h, &killed).await;
    session.prompt("continue", Vec::new()).await.unwrap();
    session.shutdown().await;
    assert_eq!(request(&seen), ["user: perform effect", "user: continue"]);
    assert_eq!(effects(&killed, "effect") + effects(&killed, "probe"), 0);

    // A recoverable run: its acknowledged input and debit precede it.
    let killed = kill_at("admission").await;
    assert_eq!(on_disk(&killed.path), INPUT);
    let h = harness(200_000);
    let seen = answering(&h);
    let session = reopen(&h, &killed).await;
    let (before, resumed) = resume(&session, &killed).await;
    session.shutdown().await;
    assert_eq!(before, RunRecoveryDisposition::Resumable);
    assert_eq!(resumed.unwrap(), RunOutcome::FinishedTurn);
    assert_eq!(request(&seen), ["user: perform effect", NOTICE]);
    assert_eq!(effects(&killed, "effect") + effects(&killed, "probe"), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_after_a_synced_tool_call_and_before_its_effect_runs_nothing() {
    // An ordinary prompt: the next one answers the call with Pi's
    // synthetic result, which every provider adapter sends.
    let killed = kill_at("plain-assistant").await;
    assert_eq!(on_disk(&killed.path), ["system", "user", "assistant"]);
    let h = harness(200_000);
    let seen = answering(&h);
    let session = reopen(&h, &killed).await;
    session.prompt("continue", Vec::new()).await.unwrap();
    session.shutdown().await;
    let sent = bake_ai::api::transform_messages::transform_messages(
        seen.lock().unwrap()[0].messages(),
        h.faux.model(),
        None,
    );
    assert_eq!(
        conversation(&sent),
        [
            "user: perform effect",
            "assistant: call",
            "result call (error): No result provided",
            "user: continue",
        ]
    );
    assert_eq!(effects(&killed, "effect"), 0);

    // A recoverable run, before and after the call's dispatch record: an
    // effectful call needs reconciliation, and nothing is requested or run.
    for (mode, rest, reason) in [
        (
            "assistant",
            &["assistant", "message"][..],
            "incomplete tool exchange",
        ),
        (
            "pre-effect",
            &["assistant", "message", "dispatch call"][..],
            "external effect",
        ),
    ] {
        let killed = kill_at(mode).await;
        assert_eq!(on_disk(&killed.path), input_then(rest), "{mode}");
        let h = harness(200_000);
        let _seen = answering(&h);
        let session = reopen(&h, &killed).await;
        let (before, resumed) = resume(&session, &killed).await;
        session.shutdown().await;
        assert!(
            matches!(&before, RunRecoveryDisposition::NeedsReconciliation { reason: why } if why.contains(reason)),
            "{mode}: {before:?}"
        );
        assert!(resumed.unwrap_err().contains(reason), "{mode}");
        assert_eq!(h.faux.call_count(), 0, "{mode}");
        assert_eq!(effects(&killed, "effect"), 0, "{mode}");
        assert_eq!(on_disk(&killed.path), input_then(rest), "{mode}");
    }

    // A `Safe` call runs again, once, before the resumed request.
    let killed = kill_at("pre-read").await;
    assert_eq!(
        on_disk(&killed.path),
        input_then(&["assistant", "message", "dispatch call safe"])
    );
    let h = harness(200_000);
    let seen = answering(&h);
    let session = reopen(&h, &killed).await;
    let (before, resumed) = resume(&session, &killed).await;
    session.shutdown().await;
    assert_eq!(before, RunRecoveryDisposition::Resumable);
    assert_eq!(resumed.unwrap(), RunOutcome::FinishedTurn);
    assert_eq!(
        request(&seen),
        [
            "user: perform effect",
            "assistant: call",
            "result call: probe done",
            NOTICE
        ]
    );
    assert_eq!(effects(&killed, "probe"), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_during_a_tool_keeps_its_effect_and_reruns_only_a_read() {
    let dispatched = input_then(&["assistant", "message", "dispatch call"]);
    let killed = kill_at("dispatch").await;
    assert_eq!(on_disk(&killed.path), dispatched);
    let h = harness(200_000);
    let _seen = answering(&h);
    let session = reopen(&h, &killed).await;
    let (before, resumed) = resume(&session, &killed).await;
    session.shutdown().await;
    assert!(matches!(
        before,
        RunRecoveryDisposition::NeedsReconciliation { .. }
    ));
    assert!(resumed.is_err());
    assert_eq!(h.faux.call_count(), 0);
    assert_eq!(effects(&killed, "effect"), 1);
    assert_eq!(on_disk(&killed.path), dispatched);

    let killed = kill_at("read").await;
    assert_eq!(
        on_disk(&killed.path),
        input_then(&["assistant", "message", "dispatch call safe"])
    );
    let h = harness(200_000);
    let seen = answering(&h);
    let session = reopen(&h, &killed).await;
    let (before, resumed) = resume(&session, &killed).await;
    session.shutdown().await;
    assert_eq!(before, RunRecoveryDisposition::Resumable);
    assert_eq!(resumed.unwrap(), RunOutcome::FinishedTurn);
    assert_eq!(
        request(&seen),
        [
            "user: perform effect",
            "assistant: call",
            "result call: probe done",
            NOTICE
        ]
    );
    assert_eq!(effects(&killed, "probe"), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_at_each_later_request_keeps_its_input() {
    // Retried, followed up, steered, compacted, and opened by a job notice.
    let tool_step = ["assistant", "message", "dispatch call", "toolResult call"];
    let cases: [(&str, Vec<&str>, Vec<&str>, usize); 5] = [
        (
            "retry",
            vec!["assistant", "message", "context_edit", "admitted"],
            vec!["user: perform effect", NOTICE],
            0,
        ),
        (
            "follow-up",
            vec!["assistant", "message", "user", "message", "admitted"],
            vec![
                "user: perform effect",
                "assistant: ",
                "user: then this",
                NOTICE,
            ],
            0,
        ),
        (
            "steer",
            [&tool_step[..], &["message", "user", "message", "admitted"]].concat(),
            vec![
                "user: perform effect",
                "assistant: call",
                "result call: effect done",
                "user: also this",
                NOTICE,
            ],
            1,
        ),
        (
            "compaction",
            [
                &tool_step[..],
                &["message", "admitted", "compaction", "admitted"],
            ]
            .concat(),
            vec![],
            1,
        ),
        (
            "job-notice",
            vec![
                "assistant",
                "message",
                "bake.job.notice",
                "message",
                "admitted",
            ],
            vec![
                "user: perform effect",
                "assistant: ",
                "user: background job bash-1 (bash: fixture job) finished \
                 [status: completed, fixture job done]. Read its output with job_output.",
                NOTICE,
            ],
            0,
        ),
    ];
    for (mode, rest, expected, ran) in cases {
        let killed = kill_at(mode).await;
        assert_eq!(on_disk(&killed.path), input_then(&rest), "{mode}");
        let h = harness(200_000);
        let seen = answering(&h);
        let session = reopen(&h, &killed).await;
        let (before, resumed) = resume(&session, &killed).await;
        session.shutdown().await;
        assert_eq!(before, RunRecoveryDisposition::Resumable, "{mode}");
        assert_eq!(resumed.unwrap(), RunOutcome::FinishedTurn, "{mode}");
        let sent = request(&seen);
        if mode == "compaction" {
            assert!(sent[0].contains("Compacted"), "{sent:?}");
            assert_eq!(sent.last().map(String::as_str), Some(NOTICE));
        } else {
            assert_eq!(sent, expected, "{mode}");
        }
        assert_eq!(effects(&killed, "effect"), ran, "{mode}");
    }
}
