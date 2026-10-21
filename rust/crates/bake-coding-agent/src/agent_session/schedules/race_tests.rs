//! Scope 11 race and restart proofs for reminders ([D39]): looks that race
//! on one due reminder, wall-clock jumps forward and back, a daylight-saving
//! gap and overlap through the tools, overdue delivery on resume, and a
//! process killed with `SIGKILL` on either side of a dispatch and after its
//! delivery. Each case counts the durable dispatches and the posted and
//! delivered reminders: a reminder occurrence reaches the model at most
//! once. Time is the test clock, as in the parent module.
//!
//! [D39]: ../../../../../../docs/roadmap/rust-0.4/scope-00/support.md

use std::path::PathBuf;

use bake_agent::{AgentEvent, AgentMessage};

use super::*;
use crate::agent_session::SessionEvent;
use crate::agent_session::job_notices;
use crate::jobs::crash_fixture::{halt, kill_fixture};
use crate::schedules::{Record, Rule};

/// 2100-01-01T00:00:00.000Z: the base of reminders made before the test
/// clock is set, which no real clock makes due.
const BASE: i64 = 4_102_444_800_000;
const MINUTE: i64 = 60_000;
const HOUR: i64 = 3_600_000;
/// 2026-11-01T04:00:00.000Z, midnight in New York on the day it leaves
/// daylight saving time: 01:00 to 02:00 local happens twice.
const FALL_BACK_DAY: i64 = 1_793_505_600_000;

const CHILD_MODE: &str = "BAKE_TEST_REMINDER_CRASH_MODE";
const CHILD_DIR: &str = "BAKE_TEST_REMINDER_CRASH_DIR";

/// A new session file whose own `records` are created, durably, so the
/// file exists before anything else is written.
fn with_reminders(dir: &TempDir, records: Vec<Record>) -> SessionManager {
    let mut session = new_file_session(dir);
    let owner = session.session_id().to_owned();
    session
        .append_durable_group(
            records
                .into_iter()
                .map(|record| GroupEntry::Custom {
                    custom_type: CHANGE_TYPE.into(),
                    data: Some(
                        Change::Create {
                            session_id: owner.clone(),
                            record,
                        }
                        .to_json(),
                    ),
                })
                .collect::<Vec<_>>(),
        )
        .expect("create the reminders");
    session
}

fn one_shot(id: &str, at: i64) -> Record {
    Record {
        id: id.into(),
        rule: Rule::At,
        prompt: format!("remind {id}"),
        scheduled_at: at,
    }
}

fn every(id: &str, seconds: i64, first: i64) -> Record {
    Record {
        id: id.into(),
        rule: Rule::Every { seconds },
        prompt: format!("repeat {id}"),
        scheduled_at: first,
    }
}

/// Takes the reminders the timer posted and has not delivered, as their
/// texts.
fn claim_posted(session: &AgentSession) -> Vec<String> {
    job_notices::claim(session.jobs())
        .into_iter()
        .filter(|message| message.custom_type == REMINDER_TYPE)
        .map(|message| bake_ai::utils::text::user_content_text(&message.content, "\n"))
        .collect()
}

/// Gives the timer time to act on a wake-up that must change nothing.
async fn quiet() {
    tokio::time::sleep(Duration::from_millis(100)).await;
}

/// Moves the clock, waits for the reminder the move makes due, and takes
/// it.
async fn fire(session: &AgentSession, clock: &AtomicI64, to: i64) -> Vec<String> {
    advance(session, clock, to);
    posted(session).await;
    claim_posted(session)
}

/// The operations of the file's dispatches, with each fixed-rate dispatch's
/// decision time.
fn dispatches(session: &AgentSession) -> Vec<String> {
    changes(session)
        .iter()
        .filter(|change| change["operation"] == "dispatch")
        .map(|change| {
            let id = change["id"].as_str().unwrap_or_default();
            match change["acceptedAt"].as_str() {
                Some(at) => format!("{id} {at}"),
                None => id.to_owned(),
            }
        })
        .collect()
}

/// The `occurrence_at` of each reminder in a batch's framing.
fn occurrences(batch: &str) -> Vec<String> {
    let json = batch
        .lines()
        .find_map(|line| line.strip_prefix("reminders_json: "))
        .expect("a batch");
    serde_json::from_str::<Vec<Value>>(json)
        .expect("batch json")
        .iter()
        .map(|reminder| {
            reminder["occurrence_at"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

// Duplicate wake-ups: two looks race on the same due one-shot, then on
// the same due fixed-rate batch, each pair held until both have decided
// before either writes. One dispatch is written for each and one reminder
// posted; the look that lost decides again from the file. A second
// dispatch would also corrupt the log, which refuses a dispatch of an
// inactive one-shot and a fixed-rate dispatch before its next target.
// Wake-ups after that change nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn racing_wakeups_dispatch_each_due_reminder_once() {
    let dir = TempDir::new("schedule-race-wakeups");
    let h = harness();
    let clock = Arc::new(AtomicI64::new(START));
    let file = with_reminders(
        &dir,
        vec![
            one_shot("schedule-1", BASE + MINUTE),
            every("schedule-2", 300, BASE + 5 * MINUTE),
        ],
    );
    let session = open(&h, file, &dir, &clock).await;
    quiet().await;
    // Each odd decision waits for the next one, so the two racing looks
    // both decide before either writes. The timer sleeps through the race:
    // the clock is moved without waking it.
    let decided = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&decided);
    session.schedules().set_before_dispatch(Arc::new(move || {
        let mine = counter.fetch_add(1, Ordering::SeqCst) + 1;
        if mine % 2 == 1 {
            let bound = std::time::Instant::now() + Duration::from_secs(10);
            while counter.load(Ordering::SeqCst) == mine && std::time::Instant::now() < bound {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }));
    clock.store(BASE + HOUR, Ordering::SeqCst);
    let looks: Vec<_> = (0..2)
        .map(|_| {
            let core = Arc::clone(&session.core);
            let jobs = session.jobs().clone();
            let schedules = session.schedules();
            tokio::spawn(async move {
                // Each racing look is a tick of its own: it delivers what it
                // finds due until nothing is.
                while let Step::Again = look(&core, &jobs, &schedules).await {}
            })
        })
        .collect();
    for racing in looks {
        racing.await.expect("a look");
    }
    assert!(
        decided.load(Ordering::SeqCst) >= 4,
        "both looks decided twice"
    );
    for _ in 0..8 {
        session.schedules().kick();
    }
    quiet().await;
    assert_eq!(
        dispatches(&session),
        ["schedule-1", "schedule-2 2100-01-01T01:00:00.000Z"]
    );
    let posted = claim_posted(&session);
    assert_eq!(posted.len(), 2, "{posted:#?}");
    assert!(
        posted[0].starts_with("[SCHEDULE REMINDER]\n"),
        "{posted:#?}"
    );
    assert_eq!(occurrences(&posted[1]), ["2100-01-01T01:00:00.000Z"]);
    let folded = crate::schedules::fold(&lock(&session.session)).expect("the log stays valid");
    assert_eq!(
        folded
            .active
            .iter()
            .map(|record| crate::schedules::instant(record.scheduled_at))
            .collect::<Vec<_>>(),
        ["2100-01-01T01:05:00.000Z"]
    );
    session.shutdown().await;
}

// Wall-clock jumps: a jump forward over many fixed-rate occurrences
// delivers only the latest, after the due one-shot; a jump back, to the
// same instant again, or to before the reminders were made delivers
// nothing; the next occurrence is delivered once when the clock reaches it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clock_jumps_forward_and_back_deliver_each_occurrence_at_most_once() {
    let dir = TempDir::new("schedule-race-clock");
    let h = harness();
    let clock = Arc::new(AtomicI64::new(BASE));
    let file = with_reminders(
        &dir,
        vec![
            every("schedule-1", 300, BASE + 5 * MINUTE),
            one_shot("schedule-2", BASE + 10 * MINUTE),
        ],
    );
    let session = open(&h, file, &dir, &clock).await;
    quiet().await;
    assert!(claim_posted(&session).is_empty());

    // Forward an hour: the one-shot first, then one batch whose occurrence
    // is the latest due, not the eleven missed before it.
    advance(&session, &clock, BASE + HOUR + 1);
    tokio::time::timeout(Duration::from_secs(10), async {
        while dispatches(&session).len() < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("both dispatched");
    quiet().await;
    let posted = claim_posted(&session);
    assert_eq!(posted.len(), 2, "{posted:#?}");
    assert!(posted[0].contains("schedule_id_json: \"schedule-2\""));
    assert_eq!(occurrences(&posted[1]), ["2100-01-01T01:00:00.000Z"]);

    for back in [BASE, BASE + HOUR + 1, BASE - 24 * HOUR] {
        advance(&session, &clock, back);
        quiet().await;
        assert!(
            claim_posted(&session).is_empty(),
            "the clock at {} delivered again",
            crate::schedules::instant(back)
        );
    }
    let next = fire(&session, &clock, BASE + HOUR + 5 * MINUTE).await;
    assert_eq!(next.len(), 1);
    assert_eq!(occurrences(&next[0]), ["2100-01-01T01:05:00.000Z"]);
    assert_eq!(
        dispatches(&session),
        [
            "schedule-2",
            "schedule-1 2100-01-01T01:00:00.001Z",
            "schedule-1 2100-01-01T01:05:00.000Z",
        ]
    );
    let folded = crate::schedules::fold(&lock(&session.session)).expect("fold");
    assert_eq!(folded.active.len(), 1);
    assert_eq!(
        crate::schedules::instant(folded.active[0].scheduled_at),
        "2100-01-01T01:10:00.000Z"
    );
    session.shutdown().await;
}

// Daylight saving through the tools: a local time the spring change skips
// is refused and writes nothing; a local time the autumn change repeats
// takes its earlier instant and fires once, not again when the wall clock
// repeats it. A fixed-rate reminder across the change keeps its UTC
// interval.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dst_gap_is_refused_and_an_overlap_fires_once() {
    let dir = TempDir::new("schedule-race-dst");
    let h = harness();
    let clock = Arc::new(AtomicI64::new(FALL_BACK_DAY));
    assert_eq!(
        crate::schedules::instant(FALL_BACK_DAY),
        "2026-11-01T04:00:00.000Z"
    );
    let session = open(&h, new_file_session(&dir), &dir, &clock).await;
    let local = |date: &str, time: &str| json!({ "date": date, "time": time, "time_zone": "America/New_York" });
    h.faux.set_responses(vec![
        call(
            "schedule_create",
            json!({ "prompt": "skipped", "at": local("2027-03-14", "02:30:00") }),
            "gap",
        ),
        call(
            "schedule_create",
            json!({ "prompt": "repeated", "at": local("2026-11-01", "01:30:00") }),
            "overlap",
        ),
        call(
            "schedule_create",
            json!({ "prompt": "hourly", "every_seconds": 3600 }),
            "hourly",
        ),
        faux_assistant_message("set").into(),
    ]);
    session
        .prompt("remind me", Vec::new())
        .await
        .expect("prompt");
    let results = tool_results(&session);
    assert_eq!(
        results[0],
        json!({ "code": "invalid_rule", "message": "The local at time does not exist in the selected time zone." })
    );
    assert_eq!(results[1]["id"], "schedule-1");
    assert_eq!(results[1]["scheduledAt"], "2026-11-01T05:30:00.000Z");
    assert_eq!(results[2]["scheduledAt"], "2026-11-01T05:00:00.000Z");
    assert_eq!(
        changes(&session)
            .iter()
            .filter(|change| change["operation"] == "create")
            .count(),
        2,
        "the refused time wrote nothing"
    );

    // 01:00 EDT, 01:30 EDT, 01:00 EST, 01:30 EST, 02:00 EST.
    let mut delivered = Vec::new();
    for (minutes, fires) in [
        (60, true),
        (90, true),
        (120, true),
        (150, false),
        (180, true),
    ] {
        let at = FALL_BACK_DAY + minutes * MINUTE;
        if fires {
            delivered.extend(fire(&session, &clock, at).await);
        } else {
            advance(&session, &clock, at);
        }
        quiet().await;
        delivered.extend(claim_posted(&session));
    }
    assert_eq!(delivered.len(), 4, "{delivered:#?}");
    assert_eq!(occurrences(&delivered[0]), ["2026-11-01T05:00:00.000Z"]);
    assert!(delivered[1].contains("occurrence_at: 2026-11-01T05:30:00.000Z"));
    assert_eq!(occurrences(&delivered[2]), ["2026-11-01T06:00:00.000Z"]);
    assert_eq!(occurrences(&delivered[3]), ["2026-11-01T07:00:00.000Z"]);
    session.shutdown().await;
}

// Overdue on resume, once: a one-shot that fell due while the session was
// closed is delivered by the resumed session; a second resume, a moved
// clock, and the next prompt deliver it no more.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_overdue_reminder_is_delivered_once_across_resumes() {
    let dir = TempDir::new("schedule-race-overdue");
    let h = harness();
    let clock = Arc::new(AtomicI64::new(BASE + HOUR));
    let file = with_reminders(&dir, vec![one_shot("schedule-1", BASE + MINUTE)]);
    let path = file.session_file().expect("a file").to_path_buf();
    let session = open(&h, file, &dir, &clock).await;
    posted(&session).await;
    h.faux
        .set_responses(vec![faux_assistant_message("Reminded.").into()]);
    assert!(session.deliver_job_notices().await.expect("delivered"));
    assert_eq!(reminders(&session).len(), 1);
    session.shutdown().await;

    for moved in [BASE + HOUR, BASE, BASE + 24 * HOUR] {
        let reopened = SessionManager::open(&path, None, None).expect("reopen");
        let session = open(&h, reopened, &dir, &clock).await;
        advance(&session, &clock, moved);
        quiet().await;
        assert!(!session.jobs().has_undelivered(), "delivered again");
        h.faux
            .set_responses(vec![faux_assistant_message("Nothing due.").into()]);
        session
            .prompt("anything?", Vec::new())
            .await
            .expect("prompt");
        assert_eq!(reminders(&session).len(), 1);
        assert_eq!(dispatches(&session), ["schedule-1"]);
        session.shutdown().await;
    }
}

/// Runs a session with one due reminder to the kill point its mode names
/// and halts there: inside the look that found it due, before its dispatch
/// (`before-dispatch`); once it is dispatched and posted, before a turn
/// takes it (`after-dispatch`, and `every-after-dispatch` for a fixed-rate
/// reminder); or once the turn's reminder message is synced
/// (`after-delivery`).
#[test]
fn reminder_crash_fixture() {
    let Ok(mode) = std::env::var(CHILD_MODE) else {
        return;
    };
    let dir = PathBuf::from(std::env::var_os(CHILD_DIR).expect("fixture directory"));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async move {
        let h = harness();
        let mut session = SessionManager::create(
            &dir.to_string_lossy(),
            Some(&dir.join("sessions")),
            NewSessionOptions::default(),
        )
        .expect("session");
        let owner = session.session_id().to_owned();
        let record = if mode == "every-after-dispatch" {
            every("schedule-1", 300, BASE + 5 * MINUTE)
        } else {
            one_shot("schedule-1", BASE + MINUTE)
        };
        let due = record.scheduled_at;
        session
            .append_durable_group([GroupEntry::Custom {
                custom_type: CHANGE_TYPE.into(),
                data: Some(
                    Change::Create {
                        session_id: owner,
                        record,
                    }
                    .to_json(),
                ),
            }])
            .expect("create");
        let path = session.session_file().expect("a file").to_owned();
        std::fs::write(dir.join("session-path"), path.to_string_lossy().as_bytes())
            .expect("session path");
        let mut opts = options(&h, session, &dir.to_string_lossy());
        opts.settings.schedules = true;
        opts.active_tool_names.extend(NAMES.map(str::to_owned));
        let session = AgentSession::create(opts).await.expect("agent session");
        let halt_in_look = mode == "before-dispatch";
        let clock_dir = dir.clone();
        let now = Arc::new(AtomicI64::new(BASE));
        let read = Arc::clone(&now);
        session.schedules().set_clock(Arc::new(move || {
            let now = read.load(Ordering::SeqCst);
            if halt_in_look && now >= due {
                halt(&clock_dir);
            }
            now
        }));
        let delivered_dir = dir.clone();
        let _subscription = session.subscribe(move |event| {
            if let SessionEvent::Agent(AgentEvent::MessageEnd {
                message: AgentMessage::Custom(custom),
            }) = event
                && custom
                    .as_any()
                    .downcast_ref::<crate::session::messages::CustomMessage>()
                    .is_some_and(|custom| custom.custom_type == REMINDER_TYPE)
            {
                halt(&delivered_dir);
            }
        });
        advance(&session, &now, due);
        posted(&session).await;
        if mode == "after-dispatch" || mode == "every-after-dispatch" {
            halt(&dir);
        }
        h.faux
            .set_responses(vec![faux_assistant_message("Reminded.").into()]);
        let _ = session.deliver_job_notices().await;
        panic!("the fixture did not reach its kill point");
    });
}

/// Kills [`reminder_crash_fixture`] in `mode`; returns its directory and
/// session file.
async fn kill_at(mode: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new(&format!("schedule-kill-{mode}"));
    kill_fixture(
        "agent_session::schedules::tests::race::reminder_crash_fixture",
        &[
            (CHILD_MODE, mode.as_ref()),
            (CHILD_DIR, dir.path().as_os_str()),
        ],
        dir.path(),
    )
    .await;
    let path = PathBuf::from(std::fs::read_to_string(dir.join("session-path")).expect("path"));
    (dir, path)
}

// SIGKILL around a dispatch: killed inside the look, before the dispatch,
// the reminder stays due and the resumed session delivers it once; killed
// after the durable dispatch, before any turn took it, it is lost, never
// repeated; killed once its message is synced, it is not delivered again.
// A fixed-rate reminder killed after its dispatch resumes at its next
// occurrence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_killed_session_delivers_a_reminder_at_most_once() {
    for (mode, posted_on_resume, delivered_before) in [
        ("before-dispatch", true, 0),
        ("after-dispatch", false, 0),
        ("after-delivery", false, 1),
        ("every-after-dispatch", false, 0),
    ] {
        let (dir, path) = kill_at(mode).await;
        let due = if mode == "every-after-dispatch" {
            BASE + 5 * MINUTE
        } else {
            BASE + MINUTE
        };
        let clock = Arc::new(AtomicI64::new(due));
        let h = harness();
        let reopened = SessionManager::open(&path, None, None).expect("the file loads");
        let session = open(&h, reopened, &dir, &clock).await;
        assert_eq!(reminders(&session).len(), delivered_before, "{mode}");
        quiet().await;
        if posted_on_resume {
            posted(&session).await;
        }
        let posted = claim_posted(&session);
        assert_eq!(
            posted.len(),
            usize::from(posted_on_resume),
            "{mode}: {posted:#?}"
        );
        assert_eq!(dispatches(&session).len(), 1, "{mode}");
        if mode == "every-after-dispatch" {
            let next = fire(&session, &clock, BASE + 10 * MINUTE).await;
            assert_eq!(
                occurrences(&next[0]),
                ["2100-01-01T00:10:00.000Z"],
                "{mode}"
            );
            assert_eq!(
                dispatches(&session),
                [
                    "schedule-1 2100-01-01T00:05:00.000Z",
                    "schedule-1 2100-01-01T00:10:00.000Z",
                ]
            );
        }
        h.faux
            .set_responses(vec![faux_assistant_message("ok").into()]);
        session
            .prompt("continue", Vec::new())
            .await
            .expect("prompt");
        assert_eq!(reminders(&session).len(), delivered_before, "{mode}");
        assert_eq!(h.faux.call_count(), 1, "{mode}: only the prompt's request");
        session.shutdown().await;
    }
}
