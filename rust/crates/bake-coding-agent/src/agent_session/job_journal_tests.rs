//! Crash tests own a re-executed test process and stop it only after a
//! durable-write checkpoint. No producer is replayed during recovery.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bake_ai::providers::faux::faux_assistant_message;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};

use super::super::control_tests::harness;
use super::super::{AgentSession, Settings, job_notices, lock};
use crate::jobs::{JobId, JobLimits, JobOutcome, JobRegistry, JobSpec, OutputMode};
use crate::owned_work::WorkGate;
use crate::session::{NewSessionOptions, SessionManager};
use crate::test_support::TempDir;

const PATIENCE: Duration = Duration::from_secs(20);

async fn terminal(session: &AgentSession, id: JobId) {
    let mut events = session.jobs().subscribe();
    tokio::time::timeout(PATIENCE, async {
        loop {
            if session.jobs().get(id).unwrap().status.is_terminal() {
                break;
            }
            if !events.changed().await {
                break;
            }
        }
    })
    .await
    .expect("job terminal");
}

fn checkpoint(path: &std::path::Path) {
    println!("JOB_CHECKPOINT {}", json!({ "file": path }));
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
    panic!("the crash fixture must be killed at its checkpoint");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn job_journal_crash_host() {
    let Some(root) = std::env::var_os("BAKE_JOB_JOURNAL_TEST_DIR") else {
        return;
    };
    let root = PathBuf::from(root);
    let phase = std::env::var("BAKE_JOB_JOURNAL_TEST_PHASE").unwrap();
    let h = harness();
    let manager = SessionManager::create(
        &root.to_string_lossy(),
        Some(&root),
        NewSessionOptions::default(),
    )
    .unwrap();
    let session = h
        .session(manager, Settings::default(), None, Vec::new())
        .await;
    let path = session.session_file().unwrap();
    let jobs = session.jobs().clone();
    if phase == "observed" {
        session.core.hooks().after_job_observation = Some(Box::new(move || {
            jobs.release_deliveries(&[]);
            assert!(
                jobs.claim_report(jobs.id("bash-1").unwrap()).unwrap(),
                "observation was acknowledged before its durable boundary"
            );
            checkpoint(&path);
        }));
    } else {
        session.core.hooks().after_job_record = Some(Arc::new(move |snapshot| {
            if snapshot.status.is_terminal() == (phase == "terminal") {
                if phase == "terminal" {
                    assert_eq!(
                        jobs.get(snapshot.id).unwrap().status,
                        crate::jobs::JobStatus::Completing,
                        "terminal state was published before journal acknowledgement"
                    );
                }
                checkpoint(&path);
            }
        }));
    }
    let marker = root.join("effect");
    let id = session
        .jobs()
        .start(
            JobSpec {
                kind: "bash",
                label: "write one effect".into(),
                output: OutputMode::Stream,
            },
            move |_| async move {
                let mut effect = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(marker)
                    .unwrap();
                writeln!(effect, "producer ran").unwrap();
                effect.sync_all().unwrap();
                JobOutcome::completed("exit code: 0")
            },
        )
        .unwrap();
    terminal(&session, id).await;
    h.faux
        .append_responses(vec![faux_assistant_message("noted").into()]);
    session.deliver_job_notices().await.unwrap();
    session.shutdown().await;
    panic!("crash checkpoint was not reached");
}

async fn crash_at(phase: &str) {
    let dir = TempDir::new(&format!("journal-crash-{phase}"));
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "agent_session::job_journal::tests::job_journal_crash_host",
            "--nocapture",
        ])
        .env("BAKE_JOB_JOURNAL_TEST_DIR", dir.path())
        .env("BAKE_JOB_JOURNAL_TEST_PHASE", phase)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
    let reached = tokio::time::timeout(PATIENCE, async {
        while let Some(line) = output.next_line().await? {
            if let Some(json) = line.strip_prefix("JOB_CHECKPOINT ") {
                return Ok::<_, std::io::Error>(Some(serde_json::from_str::<Value>(json).unwrap()));
            }
        }
        Ok(None)
    })
    .await;
    // Reap on every path, including a broken checkpoint assertion.
    let _ = child.kill().await;
    let _ = child.wait().await;
    let checkpoint = reached
        .expect("checkpoint deadline")
        .expect("checkpoint stream")
        .expect("checkpoint reached");
    let file = PathBuf::from(checkpoint["file"].as_str().unwrap());
    let reopened = SessionManager::open(&file, None, None).unwrap();
    let jobs = JobRegistry::new(WorkGate::default(), JobLimits::default());
    let notices = job_notices::recover(&reopened, &jobs);
    jobs.close().await;
    assert!(jobs.id("bash-1").is_ok(), "journal reserves the id");
    let effect = std::fs::read_to_string(dir.join("effect")).unwrap_or_default();
    assert_eq!(
        effect,
        if phase == "started" {
            ""
        } else {
            "producer ran\n"
        }
    );
    if phase == "observed" {
        assert!(
            notices.is_empty(),
            "durable observation was delivered twice"
        );
    } else {
        assert_eq!(notices.len(), 1);
        assert_eq!(
            notices[0].details.as_ref().unwrap()["status"],
            if phase == "started" {
                "lost"
            } else {
                "completed"
            }
        );
        let text = bake_ai::utils::text::user_content_text(&notices[0].content, "");
        assert!(
            !text.contains("Read its output"),
            "recovery promised unavailable output"
        );
    }
    let h = harness();
    let session = h
        .session(reopened, Settings::default(), None, Vec::new())
        .await;
    h.faux
        .append_responses(vec![faux_assistant_message("recovered").into()]);
    session.prompt("continue", Vec::new()).await.unwrap();
    assert!(
        session.jobs().list().is_empty(),
        "recovery admitted a producer"
    );
    session.shutdown().await;
    let reopened = SessionManager::open(&file, None, None).unwrap();
    let jobs = JobRegistry::new(WorkGate::default(), JobLimits::default());
    let repeated = job_notices::recover(&reopened, &jobs);
    jobs.close().await;
    assert!(
        repeated.is_empty(),
        "a resumed observation was not acknowledged"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("effect")).unwrap_or_default(),
        effect
    );
}

#[tokio::test]
async fn crash_after_started_durability_precedes_the_producer() {
    crash_at("started").await;
}

#[tokio::test]
async fn crash_after_terminal_durability_precedes_publication() {
    crash_at("terminal").await;
}

#[tokio::test]
async fn crash_after_observation_durability_precedes_acknowledgement() {
    crash_at("observed").await;
}

#[tokio::test]
async fn a_failed_start_record_prevents_the_producer_and_further_prompts() {
    let dir = TempDir::new("journal-start-failure");
    let h = harness();
    let manager = SessionManager::create(
        &dir.path().to_string_lossy(),
        Some(dir.path()),
        NewSessionOptions::default(),
    )
    .unwrap();
    let session = h
        .session(manager, Settings::default(), None, Vec::new())
        .await;
    lock(&session.session).close_writer();
    let marker = dir.join("effect");
    let effect = marker.clone();
    let id = session
        .jobs()
        .start(
            JobSpec {
                kind: "bash",
                label: "must not run".into(),
                output: OutputMode::Stream,
            },
            move |_| async move {
                std::fs::write(effect, "ran").unwrap();
                JobOutcome::completed("done")
            },
        )
        .unwrap();
    assert!(session.jobs().wait_admitted(id).await.is_err());
    let refused = session.prompt("continue", Vec::new()).await;
    session.shutdown().await;
    assert!(!marker.exists());
    assert!(session.jobs().claim_unreported().is_empty());
    assert!(refused.is_err());
    assert_eq!(h.faux.call_count(), 0);
}

#[tokio::test]
async fn a_failed_terminal_write_publishes_no_completion_and_recovers_unknown_outcome() {
    let dir = TempDir::new("journal-terminal-failure");
    let h = harness();
    let manager = SessionManager::create(
        &dir.path().to_string_lossy(),
        Some(dir.path()),
        NewSessionOptions::default(),
    )
    .unwrap();
    let session = h
        .session(manager, Settings::default(), None, Vec::new())
        .await;
    let path = session.session_file().unwrap();
    let file = Arc::clone(&session.session);
    session.core.hooks().after_job_record = Some(Arc::new(move |snapshot| {
        if !snapshot.status.is_terminal() {
            lock(&file).close_writer();
        }
    }));
    let id = session
        .jobs()
        .start(
            JobSpec {
                kind: "bash",
                label: "complete before write failure".into(),
                output: OutputMode::Stream,
            },
            |_| async { JobOutcome::completed("done") },
        )
        .unwrap();
    let result = session.jobs().wait(id, PATIENCE, None).await.unwrap();
    assert_eq!(result.status, crate::jobs::JobStatus::Failed);
    assert!(session.jobs().claim_unreported().is_empty());
    session.shutdown().await;
    let reopened = SessionManager::open(&path, None, None).unwrap();
    let jobs = JobRegistry::new(WorkGate::default(), JobLimits::default());
    let notices = job_notices::recover(&reopened, &jobs);
    jobs.close().await;
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].details.as_ref().unwrap()["status"], "lost");
}

#[tokio::test]
async fn a_failed_observation_write_releases_its_delivery_and_preserves_terminal_truth() {
    let dir = TempDir::new("journal-observation-failure");
    let h = harness();
    let manager = SessionManager::create(
        &dir.path().to_string_lossy(),
        Some(dir.path()),
        NewSessionOptions::default(),
    )
    .unwrap();
    let session = h
        .session(manager, Settings::default(), None, Vec::new())
        .await;
    let path = session.session_file().unwrap();
    let id = session
        .jobs()
        .start(
            JobSpec {
                kind: "bash",
                label: "complete before observation failure".into(),
                output: OutputMode::Stream,
            },
            |_| async { JobOutcome::completed("done") },
        )
        .unwrap();
    terminal(&session, id).await;
    lock(&session.session).close_writer();
    h.faux
        .append_responses(vec![faux_assistant_message("not reached").into()]);
    let delivery = session.deliver_job_notices().await;
    let released = session.jobs().claim_unreported();
    session.shutdown().await;
    assert!(delivery.is_err());
    assert_eq!(
        released.len(),
        1,
        "failed persistence acknowledged the observation"
    );
    assert_eq!(h.faux.call_count(), 0);
    let reopened = SessionManager::open(&path, None, None).unwrap();
    let jobs = JobRegistry::new(WorkGate::default(), JobLimits::default());
    let notices = job_notices::recover(&reopened, &jobs);
    jobs.close().await;
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].details.as_ref().unwrap()["status"], "completed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn a_dropped_delivery_keeps_its_lease_until_persistence_is_idle() {
    let h = harness();
    let session = Arc::new(
        h.session(
            super::super::control_tests::in_memory(),
            Settings::default(),
            None,
            Vec::new(),
        )
        .await,
    );
    let id = session
        .jobs()
        .start(
            JobSpec {
                kind: "bash",
                label: "complete once".into(),
                output: OutputMode::Final,
            },
            |_| async { JobOutcome::completed("done").with_output("done") },
        )
        .unwrap();
    terminal(&session, id).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let reached = Arc::clone(&entered);
    let (release, held) = std::sync::mpsc::channel();
    session.core.hooks().after_job_observation = Some(Box::new(move || {
        reached.notify_one();
        held.recv_timeout(PATIENCE).expect("listener released");
    }));
    h.faux
        .append_responses(vec![faux_assistant_message("noted").into()]);
    let owner = Arc::clone(&session);
    let delivery = tokio::spawn(async move { owner.deliver_job_notices().await });
    tokio::time::timeout(PATIENCE, entered.notified())
        .await
        .expect("durable observation");
    delivery.abort();
    let _ = delivery.await;
    let still_owned = session.is_streaming();
    let prematurely_released = session.jobs().claim_report(id).unwrap();
    release.send(()).unwrap();
    tokio::time::timeout(PATIENCE, session.core.wait_for_idle())
        .await
        .expect("delivery cleanup");
    // A later read, wait, or kill cannot turn an acknowledged observation
    // back into a reservation that a subsequent aborted turn releases.
    session.jobs().read(id).unwrap();
    session.jobs().wait(id, PATIENCE, None).await.unwrap();
    session.jobs().kill(id, None).unwrap();
    session.jobs().release_deliveries(&[]);
    let repeated = session.jobs().claim_unreported();
    session.shutdown().await;
    assert!(
        still_owned,
        "dropped wrapper released a live persistence listener"
    );
    assert!(!prematurely_released, "live delivery lost its reservation");
    assert!(
        repeated.is_empty(),
        "a durable acknowledgement became reportable again"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_the_owner_during_start_durability_prevents_producer_admission() {
    let h = harness();
    let session = h
        .session(
            super::super::control_tests::in_memory(),
            Settings::default(),
            None,
            Vec::new(),
        )
        .await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let reached = Arc::clone(&entered);
    let (release, held) = std::sync::mpsc::channel();
    let held = std::sync::Mutex::new(held);
    session.core.hooks().after_job_record = Some(Arc::new(move |snapshot| {
        if !snapshot.status.is_terminal() {
            reached.notify_one();
            held.lock()
                .unwrap()
                .recv_timeout(PATIENCE)
                .expect("start write released");
        }
    }));
    let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let effect = Arc::clone(&ran);
    let id = session
        .jobs()
        .start(
            JobSpec {
                kind: "bash",
                label: "must not run".into(),
                output: OutputMode::Stream,
            },
            move |_| async move {
                effect.store(true, std::sync::atomic::Ordering::SeqCst);
                JobOutcome::completed("ran")
            },
        )
        .unwrap();
    tokio::time::timeout(PATIENCE, entered.notified())
        .await
        .expect("start write reached");
    let drain = session.work.close();
    release.send(()).unwrap();
    tokio::time::timeout(PATIENCE, drain)
        .await
        .expect("owner drained");
    let outcome = session.jobs().get(id).unwrap();
    session.shutdown().await;
    assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(outcome.status, crate::jobs::JobStatus::Killed);
}

#[tokio::test]
async fn a_live_custom_job_kind_is_acknowledged_once() {
    let h = harness();
    let session = h
        .session(
            super::super::control_tests::in_memory(),
            Settings::default(),
            None,
            Vec::new(),
        )
        .await;
    let id = session
        .jobs()
        .start(
            JobSpec {
                kind: "custom",
                label: "one custom job".into(),
                output: OutputMode::Final,
            },
            |_| async { JobOutcome::completed("done") },
        )
        .unwrap();
    terminal(&session, id).await;
    h.faux
        .append_responses(vec![faux_assistant_message("noted").into()]);
    assert!(session.deliver_job_notices().await.unwrap());
    assert!(!session.deliver_job_notices().await.unwrap());
    assert!(session.jobs().claim_unreported().is_empty());
    session.shutdown().await;
    assert_eq!(h.faux.call_count(), 1);
}
