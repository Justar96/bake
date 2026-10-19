//! The schedule tools and timer through a session. Cases from Bake's
//! TypeScript `packages/schedule/schedule/tests/{tools,runtime}.spec.ts`:
//! tool results and errors, delivery at most once with the dispatch
//! durable first, overdue delivery on resume, a clock moved back, and a
//! fork that adopts nothing; and Bake's shutdown during a look. Time is a test clock the timer is woken for,
//! so no case waits on the wall clock.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use bake_ai::StopReason;
use bake_ai::providers::faux::{
    FauxResponseStep, faux_assistant_blocks, faux_assistant_message, faux_tool_call,
};
use serde_json::{Value, json};

use super::*;
use crate::agent_session::tests::{harness, new_file_session, options};
use crate::agent_session::{AgentSession, lock};
use crate::schedules::REMINDER_TYPE;
use crate::session::{NewSessionOptions, SessionManager};
use crate::test_support::TempDir;

const START: i64 = 1_785_931_200_000; // 2026-08-05T12:00:00.000Z

async fn open(
    h: &crate::agent_session::tests::Harness,
    session: SessionManager,
    dir: &TempDir,
    clock: &Arc<AtomicI64>,
) -> AgentSession {
    let mut opts = options(h, session, &dir.path().to_string_lossy());
    opts.settings.schedules = true;
    opts.active_tool_names.extend(NAMES.map(str::to_owned));
    let session = AgentSession::create(opts).await.expect("session");
    let read = Arc::clone(clock);
    session
        .schedules()
        .set_clock(Arc::new(move || read.load(Ordering::SeqCst)));
    session
}

/// Moves the clock and wakes the timer.
fn advance(session: &AgentSession, clock: &AtomicI64, to: i64) {
    clock.store(to, Ordering::SeqCst);
    session.schedules().kick();
}

/// Waits until the timer has posted a reminder.
async fn posted(session: &AgentSession) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !session.jobs().has_undelivered() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("a reminder was posted");
}

/// The schedule changes in the file, as their data.
fn changes(session: &AgentSession) -> Vec<Value> {
    lock(&session.session)
        .entries()
        .iter()
        .filter(|entry| {
            entry.entry_type() == "custom"
                && entry.get("customType").and_then(Value::as_str) == Some(CHANGE_TYPE)
        })
        .map(|entry| entry.get("data").cloned().unwrap_or_default())
        .collect()
}

/// The delivered reminder texts, in log order.
fn reminders(session: &AgentSession) -> Vec<String> {
    lock(&session.session)
        .entries()
        .iter()
        .filter(|entry| {
            entry.entry_type() == "custom_message"
                && entry.get("customType").and_then(Value::as_str) == Some(REMINDER_TYPE)
        })
        .map(|entry| {
            entry.as_json()["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

/// The tool results of the last run, as JSON.
fn tool_results(session: &AgentSession) -> Vec<Value> {
    lock(&session.session)
        .entries()
        .iter()
        .filter_map(|entry| {
            let message = entry.get("message")?;
            (message.get("role")?.as_str()? == "toolResult").then(|| {
                serde_json::from_str(message["content"][0]["text"].as_str().unwrap_or("null"))
                    .unwrap_or(Value::Null)
            })
        })
        .collect()
}

fn call(name: &str, arguments: Value, id: &str) -> FauxResponseStep {
    faux_assistant_blocks(
        vec![faux_tool_call(name, arguments, Some(id))],
        StopReason::ToolUse,
    )
    .into()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_tools_create_list_and_delete_with_0_3_results() {
    let dir = TempDir::new("schedule-tools");
    let h = harness();
    let clock = Arc::new(AtomicI64::new(START));
    let session = open(&h, new_file_session(&dir), &dir, &clock).await;
    h.faux.set_responses(vec![
        call("schedule_create", json!({"prompt": "  check logs ", "after_seconds": 60}), "c1"),
        call("schedule_create", json!({"prompt": "x", "after_seconds": 60, "every_seconds": 300}), "c2"),
        call("schedule_create", json!({"prompt": "x", "every_seconds": 60}), "c3"),
        call(
            "schedule_create",
            json!({"prompt": "meet", "at": {"date": "2026-08-06", "time": "09:00:00", "time_zone": "Asia/Shanghai"}}),
            "c4",
        ),
        call("schedule_create", json!({"prompt": "x", "at": "2026-08-05T11:00:00Z"}), "c5"),
        call("schedule_list", json!({}), "l1"),
        call("schedule_delete", json!({"id": "schedule-1"}), "d1"),
        call("schedule_delete", json!({"id": "schedule-1"}), "d2"),
        call("schedule_delete", json!({"id": " schedule-2"}), "d3"),
        faux_assistant_message("done").into(),
    ]);
    session
        .prompt("remind me", Vec::new())
        .await
        .expect("prompt");
    let results = tool_results(&session);
    assert_eq!(
        results[0],
        json!({"id": "schedule-1", "kind": "after", "prompt": "check logs", "afterSeconds": 60,
            "scheduledAt": "2026-08-05T12:01:00.000Z", "state": "scheduled", "deliveryMode": "session-local"})
    );
    assert_eq!(results[1]["code"], "invalid_selector");
    assert_eq!(results[2]["code"], "frequency_too_high");
    assert_eq!(results[3]["scheduledAt"], "2026-08-06T01:00:00.000Z");
    assert_eq!(results[3]["id"], "schedule-2");
    assert_eq!(results[4]["code"], "not_future");
    assert_eq!(
        results[5]
            .as_array()
            .expect("a list")
            .iter()
            .map(|view| view["id"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        ["schedule-1", "schedule-2"]
    );
    assert_eq!(results[6], json!({"id": "schedule-1", "deleted": true}));
    assert_eq!(
        results[7],
        json!({"id": "schedule-1", "deleted": false, "code": "schedule_not_found"})
    );
    assert_eq!(results[8]["code"], "invalid_rule");
    let written = changes(&session);
    assert_eq!(
        written
            .iter()
            .map(|change| change["operation"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        ["create", "create", "delete"],
        "only valid calls write"
    );
    assert_eq!(written[0]["sessionId"], session.session_id());
    session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_due_reminder_is_dispatched_then_delivered_once_and_survives_no_restart() {
    let dir = TempDir::new("schedule-delivery");
    let h = harness();
    let clock = Arc::new(AtomicI64::new(START));
    let session = open(&h, new_file_session(&dir), &dir, &clock).await;
    h.faux.set_responses(vec![
        call(
            "schedule_create",
            json!({"prompt": "stretch", "after_seconds": 300}),
            "c1",
        ),
        faux_assistant_message("Reminder set.").into(),
    ]);
    session
        .prompt("remind me", Vec::new())
        .await
        .expect("prompt");

    // A clock moved back, or not yet at the target, fires nothing.
    advance(&session, &clock, START - 3_600_000);
    advance(&session, &clock, START + 299_999);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!session.jobs().has_undelivered());

    let seen = Arc::new(Mutex::new(Vec::new()));
    let context = Arc::clone(&seen);
    h.faux
        .set_responses(vec![FauxResponseStep::from_fn(move |request, _, _, _| {
            *context.lock().unwrap() = request.messages().to_vec();
            Ok(faux_assistant_message("Time to stretch."))
        })]);
    advance(&session, &clock, START + 300_000);
    posted(&session).await;
    // Durable before it is posted.
    assert_eq!(
        changes(&session).last().expect("a dispatch"),
        &json!({"version": 1, "operation": "dispatch", "id": "schedule-1"})
    );
    assert!(
        session.deliver_job_notices().await.expect("delivered"),
        "an idle session opens a turn"
    );
    let sent = serde_json::to_string(&*seen.lock().unwrap()).unwrap();
    assert!(sent.contains("[SCHEDULE REMINDER]"), "{sent}");
    assert!(
        sent.contains("reminder_prompt_json: \\\"stretch\\\""),
        "{sent}"
    );
    assert_eq!(reminders(&session).len(), 1);

    // Later looks, and a reopened session, deliver nothing more.
    advance(&session, &clock, START + 900_000);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!session.jobs().has_undelivered());
    let file = session.session_file().expect("a file").to_path_buf();
    session.shutdown().await;
    let reopened = SessionManager::open(&file, None, None).expect("reopen");
    let session = open(&h, reopened, &dir, &clock).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!session.jobs().has_undelivered());
    session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reminder_due_while_closed_is_overdue_and_delivered_on_resume() {
    let dir = TempDir::new("schedule-overdue");
    let h = harness();
    let clock = Arc::new(AtomicI64::new(START));
    let mut file_session = new_file_session(&dir);
    let owner = file_session.session_id().to_owned();
    let record = |id: &str, seconds: i64| crate::schedules::Record {
        id: id.into(),
        rule: crate::schedules::Rule::Every { seconds },
        prompt: format!("check {id}"),
        scheduled_at: START - 3_600_000,
    };
    for created in [record("schedule-1", 300), record("schedule-2", 600)] {
        file_session
            .append_custom_entry(
                CHANGE_TYPE,
                Some(
                    Change::Create {
                        session_id: owner.clone(),
                        record: created,
                    }
                    .to_json(),
                ),
            )
            .expect("create");
    }
    let session = open(&h, file_session, &dir, &clock).await;
    posted(&session).await;
    h.faux
        .set_responses(vec![faux_assistant_message("Both checked.").into()]);
    assert!(session.deliver_job_notices().await.expect("delivered"));
    let delivered = reminders(&session);
    assert_eq!(
        delivered.len(),
        1,
        "one batch for every overdue fixed-rate reminder"
    );
    assert!(delivered[0].starts_with("[SCHEDULE REMINDER BATCH]"));
    assert!(
        delivered[0].contains("\"occurrence_at\":\"2026-08-05T12:00:00.000Z\""),
        "{}",
        delivered[0]
    );
    // Each advances past the decision, keeping its anchor.
    let folded = crate::schedules::fold(&lock(&session.session)).expect("fold");
    assert_eq!(
        folded
            .active
            .iter()
            .map(|record| crate::schedules::instant(record.scheduled_at))
            .collect::<Vec<_>>(),
        ["2026-08-05T12:05:00.000Z", "2026-08-05T12:10:00.000Z"]
    );
    session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fork_keeps_the_history_and_delivers_none_of_it() {
    let dir = TempDir::new("schedule-fork");
    let h = harness();
    let clock = Arc::new(AtomicI64::new(START));
    let mut parent = new_file_session(&dir);
    parent
        .append_custom_entry(
            CHANGE_TYPE,
            Some(
                Change::Create {
                    session_id: parent.session_id().to_owned(),
                    record: schedules::create_after("schedule-1".into(), "ping", 60, START)
                        .expect("record"),
                }
                .to_json(),
            ),
        )
        .expect("create");
    let leaf = parent.leaf_id().expect("a leaf").to_owned();
    let fork = parent.branched_copy(Some(&leaf)).expect("fork");
    let session = open(&h, fork, &dir, &clock).await;
    advance(&session, &clock, START + 3_600_000);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!session.jobs().has_undelivered());
    h.faux.set_responses(vec![
        call("schedule_list", json!({}), "l1"),
        call(
            "schedule_create",
            json!({"prompt": "own", "after_seconds": 60}),
            "c1",
        ),
        faux_assistant_message("ok").into(),
    ]);
    session.prompt("list", Vec::new()).await.expect("prompt");
    let results = tool_results(&session);
    assert_eq!(results[0], json!([]));
    assert_eq!(results[1]["id"], "schedule-2", "the copied id stays used");
    session.shutdown().await;
}

// Bake: shutdown during a look waits for it, and the reminder the look
// found due is neither dispatched nor posted; the timer has ended once
// shutdown returns. The look is held in the test clock, which it reads
// with the session's I/O permit held.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_waits_for_a_look_and_nothing_is_delivered_after() {
    // 2100-01-01T00:00:00.000Z: no real clock reaches it, so only the
    // test clock makes the reminder due.
    const FAR: i64 = 4_102_444_800_000;
    let dir = TempDir::new("schedule-shutdown");
    let h = harness();
    let mut file_session = new_file_session(&dir);
    let owner = file_session.session_id().to_owned();
    file_session
        .append_custom_entry(
            CHANGE_TYPE,
            Some(
                Change::Create {
                    session_id: owner,
                    record: schedules::create_after("schedule-1".into(), "ping", 60, FAR)
                        .expect("record"),
                }
                .to_json(),
            ),
        )
        .expect("create");
    let mut opts = options(&h, file_session, &dir.path().to_string_lossy());
    opts.settings.schedules = true;
    opts.active_tool_names.extend(NAMES.map(str::to_owned));
    let session = AgentSession::create(opts).await.expect("session");
    let due = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (entered, ready) = tokio::sync::oneshot::channel();
    // Dropped on every path out of the test, so the look is never left
    // held for the runtime to wait on.
    let (release, held) = std::sync::mpsc::channel::<()>();
    let hold = Mutex::new(Some((entered, held)));
    let reading = Arc::clone(&due);
    session.schedules().set_clock(Arc::new(move || {
        if !reading.load(Ordering::SeqCst) {
            return FAR;
        }
        let taken = hold
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some((entered, held)) = taken {
            let _ = entered.send(());
            let _ = held.recv();
        }
        FAR + 3_600_000
    }));
    due.store(true, Ordering::SeqCst);
    session.schedules().kick();
    tokio::time::timeout(Duration::from_secs(10), ready)
        .await
        .expect("the timer looks")
        .expect("entered");

    let mut shutdown = std::pin::pin!(session.shutdown());
    // The first poll refuses new work and closes the gate.
    let _ = std::future::poll_fn(|cx| Poll::Ready(shutdown.as_mut().poll(cx))).await;
    let mut drained = std::pin::pin!(session.work.close());
    let waiting =
        std::future::poll_fn(|cx| Poll::Ready(drained.as_mut().poll(cx).is_pending())).await;
    drop(release);
    tokio::time::timeout(Duration::from_secs(10), shutdown)
        .await
        .expect("shutdown finishes once the look ends");
    assert!(waiting, "the gate drained during the timer's look");
    assert!(session.work.is_closed(), "the timer outlived shutdown");
    assert_eq!(changes(&session).len(), 1, "a dispatch was written");
    assert!(reminders(&session).is_empty());
    assert!(!session.jobs().has_undelivered(), "a reminder was posted");
    assert_eq!(h.faux.call_count(), 0, "a turn ran");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_setting_gates_the_tools_and_reserves_their_names() {
    let dir = TempDir::new("schedule-gate");
    let h = harness();
    let session = AgentSession::create(options(
        &h,
        new_file_session(&dir),
        &dir.path().to_string_lossy(),
    ))
    .await
    .expect("session");
    assert!(
        session
            .agent
            .state()
            .tools
            .iter()
            .all(|tool| !NAMES.contains(&tool.name.as_str()))
    );
    session.shutdown().await;
    let mut opts = options(
        &h,
        SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
            .expect("session"),
        "/tmp",
    );
    opts.settings.schedules = true;
    opts.tools.push(Arc::new(bake_agent::AgentTool::new(
        "schedule_list",
        "x",
        "x",
        json!({"type": "object"}),
        |_, _, _, _, _ctx| Box::pin(async { Ok(bake_agent::AgentToolResult::default()) }),
    )));
    let refused = AgentSession::create(opts).await.expect_err("refused");
    assert_eq!(
        refused,
        "The schedule tool names are reserved when schedules is enabled."
    );
}
