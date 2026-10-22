//! Scope 11 barrier proofs for subagents: a background child's run settles
//! just before, during, or just after its parent's running turn is aborted
//! or the parent shuts down, and two children settle in the reverse of
//! their start order. Each case counts the settlement notices the parent's
//! file admits, checks that no task of the parent or a child outlives a
//! shutdown, and resumes the parent from its file to check what a restart
//! reports and that the child continues. Waits are on gates the scripted
//! model holds, never on time.

use tokio::sync::Notify;

use super::*;

/// Gates the scripted model holds: each child's request until its gate
/// opens, and the parent's `hold` request until it is cancelled.
#[derive(Default)]
struct Gates {
    first: Arc<Notify>,
    second: Arc<Notify>,
    /// Notified when the parent's held request starts.
    held: Arc<Notify>,
}

/// Scripts the model. The parent delegates `First.` (and `Second.` for
/// `delegate two`) in the background and waits; its `hold` request runs
/// until cancelled; it acknowledges notices. A child's first run answers
/// once its gate opens, unless cancelled first; a continued child answers
/// at once.
fn script(h: &Harness, gates: &Gates) {
    let steps = (0..64)
        .map(|_| {
            let (first, second, held) = (
                Arc::clone(&gates.first),
                Arc::clone(&gates.second),
                Arc::clone(&gates.held),
            );
            FauxResponseStep::Factory(Arc::new(move |context, options, _, _| {
                let messages = context.messages().to_vec();
                let signal = options.base.signal.clone();
                let (first, second, held) =
                    (Arc::clone(&first), Arc::clone(&second), Arc::clone(&held));
                Box::pin(async move {
                    let cancelled = async {
                        match signal {
                            Some(signal) => signal.cancelled().await,
                            None => std::future::pending().await,
                        }
                    };
                    let last = last_user(&messages);
                    if is_child(&messages) {
                        if last.contains("sent a message: resume") {
                            return Ok(faux_assistant_message("resumed"));
                        }
                        let (gate, answer) = if last.starts_with("First.") {
                            (first, "first answer")
                        } else {
                            (second, "second answer")
                        };
                        tokio::select! {
                            () = gate.notified() => return Ok(faux_assistant_message(answer)),
                            () = cancelled => return Err("request cancelled".into()),
                        }
                    }
                    if last == "hold" {
                        held.notify_one();
                        cancelled.await;
                        return Err("request cancelled".into());
                    }
                    if after_tool(&messages)
                        || last.starts_with("Background subagent")
                        || last == "next"
                    {
                        return Ok(faux_assistant_message("ok"));
                    }
                    let delegate = |prompt: &str| {
                        faux_tool_call(
                            "subagent",
                            json!({ "description": prompt, "prompt": prompt }),
                            None,
                        )
                    };
                    let mut calls = vec![delegate("First.")];
                    if last == "delegate two" {
                        calls.push(delegate("Second."));
                    }
                    Ok(faux_assistant_blocks(calls, Stop::ToolUse))
                })
            }))
        })
        .collect();
    h.faux.set_responses(steps);
}

/// The notices of the parent's file, as their run and stop reason.
fn file_notices(parent: &Parent) -> Vec<(u64, String)> {
    let path = parent.session.session_file().expect("parent file");
    std::fs::read_to_string(path)
        .expect("parent file")
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|entry| entry["customType"] == messages::NOTICE_TYPE)
        .map(|entry| {
            (
                entry["details"]["runId"].as_u64().unwrap_or_default(),
                entry["details"]["stopReason"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            )
        })
        .collect()
}

/// Waits until no child runs.
async fn idle_children(parent: &Parent) {
    let jobs = parent.session.jobs().clone();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let mut events = jobs.subscribe();
            if parent.host.running() == 0 {
                return;
            }
            events.changed().await;
        }
    })
    .await
    .expect("the children settled");
}

/// Asserts that the parent's gate and every child's are drained: no task,
/// blocking work, or process of the parent or a child is left.
fn nothing_left(h: &Harness, parent: &Parent, case: &str) {
    assert!(
        parent.work.is_closed(),
        "{case}: the parent's work outlived it"
    );
    for gate in h.child_gates.lock().expect("gates").iter() {
        assert!(gate.is_closed(), "{case}: a child's work outlived it");
    }
    assert_eq!(parent.host.running(), 0, "{case}");
    assert_eq!(parent.session.jobs().live(), 0, "{case}");
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParentStop {
    /// Esc: the parent's turn is aborted; the parent stays live.
    Abort,
    /// The parent session shuts down, as a process does when it exits.
    Shutdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Settles {
    Before,
    During,
    After,
}

/// One barrier case: the child settles `when` relative to the parent's
/// `stop`. Returns the parent's file once it is shut down, with the
/// harness.
async fn race(stop: ParentStop, when: Settles) -> (Arc<Harness>, PathBuf, String) {
    let case = format!("{stop:?} {when:?}");
    let h = harness(&format!("subagent-barrier-{stop:?}-{when:?}"));
    let gates = Gates::default();
    script(&h, &gates);
    let parent = parent(&h, None).await;
    bounded("delegate", parent.session.prompt("delegate", Vec::new()))
        .await
        .expect("delegate");
    let id = parent.host.list()[0].id.clone();
    let file = parent.session.session_file().expect("parent file");
    let running = Arc::clone(&parent.session);
    let hold = tokio::spawn(async move { running.prompt("hold", Vec::new()).await });
    tokio::time::timeout(Duration::from_secs(10), gates.held.notified())
        .await
        .expect("the parent's turn is running");
    let settle = || async {
        gates.first.notify_one();
        idle_children(&parent).await;
    };
    let stopping = || async {
        match stop {
            ParentStop::Abort => bounded("abort", parent.session.abort()).await,
            ParentStop::Shutdown => bounded("shutdown", parent.session.shutdown()).await,
        }
    };
    match when {
        Settles::Before => {
            settle().await;
            stopping().await;
        }
        Settles::During => {
            tokio::join!(settle(), stopping());
        }
        Settles::After => {
            stopping().await;
            settle().await;
        }
    }
    // The held turn has ended, whatever it reports.
    let _ = tokio::time::timeout(Duration::from_secs(10), hold)
        .await
        .expect("the held turn ended");
    match stop {
        ParentStop::Abort => {
            // The parent stays live: the notice is delivered once, by the
            // run that took it or the turn the idle owner opens.
            settled(&parent).await;
            assert_eq!(
                file_notices(&parent),
                [(1, "completed".to_owned())],
                "{case}"
            );
            assert!(
                !bounded("delivery", parent.session.deliver_job_notices())
                    .await
                    .expect("nothing left"),
                "{case}"
            );
            bounded("shutdown", parent.session.shutdown()).await;
        }
        ParentStop::Shutdown => {
            // Nothing is delivered after shutdown begins: a settlement
            // posted before it is dropped with the registry's mail, though
            // the file records it first, and a run still going is stopped
            // and records and reports nothing.
            assert!(file_notices(&parent).is_empty(), "{case}");
            assert!(!parent.session.jobs().has_undelivered(), "{case}");
            let recorded = file_settled(&file);
            match when {
                Settles::Before => assert_eq!(recorded, [(1, "completed".to_owned())], "{case}"),
                Settles::During => assert!(
                    recorded.is_empty() || recorded == [(1, "completed".to_owned())],
                    "{case}: {recorded:?}"
                ),
                Settles::After => assert!(recorded.is_empty(), "{case}: {recorded:?}"),
            }
        }
    }
    nothing_left(&h, &parent, &case);
    (h, file, id)
}

// The settlement of a background child's run before, during, and after
// the parent's turn is aborted is admitted once into the parent's file,
// and a resumed parent reports nothing more for it. A run that settled
// before the parent's shutdown has its settlement recorded, though its
// notice was never delivered, and the resumed parent delivers that
// finished notice once; a run the shutdown stopped first is reported once,
// as interrupted; one settling during it is either, as the race fell. In
// every case the resumed parent continues the child as a new run, settled
// once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_child_settling_around_a_parent_stop_is_admitted_once() {
    for stop in [ParentStop::Abort, ParentStop::Shutdown] {
        for when in [Settles::Before, Settles::During, Settles::After] {
            let case = format!("{stop:?} {when:?}");
            let (h, file, id) = race(stop, when).await;
            let store = SessionManager::open(&file, Some(&h.place), None).expect("reopen");
            let lost = lost_runs(&store);
            assert_eq!(
                lost.len(),
                usize::from(stop == ParentStop::Shutdown),
                "{case}: {lost:?}"
            );
            let first = match (stop, when) {
                (ParentStop::Abort, _) | (ParentStop::Shutdown, Settles::Before) => "completed",
                (ParentStop::Shutdown, Settles::After) => "interrupted",
                (ParentStop::Shutdown, Settles::During) => {
                    if file_settled(&file).is_empty() {
                        "interrupted"
                    } else {
                        "completed"
                    }
                }
            };
            if stop == ParentStop::Shutdown {
                let text = bake_ai::utils::text::user_content_text(&lost[0].content, "\n");
                let expected = match first {
                    "completed" => format!(
                        "Background subagent {id} finished and will do no further work unless you send it more.\nIts closing message:\nfirst answer"
                    ),
                    _ => format!(
                        "Background subagent {id} was interrupted when Bake stopped and did not finish. Send it a message to continue it."
                    ),
                };
                assert_eq!(text, expected, "{case}");
            }
            let resumed = parent(&h, Some(store)).await;
            assert_eq!(resumed.host.list()[0].status, Status::Ready, "{case}");
            bounded("the prompt", resumed.session.prompt("next", Vec::new()))
                .await
                .expect("prompt");
            assert_eq!(
                file_notices(&resumed),
                [(1, first.to_owned())],
                "{case}: one notice for run 1"
            );
            let (_, run) = bounded("the message", resumed.host.message_child(&id, "resume"))
                .await
                .expect("continue");
            assert_eq!(run, Some(2), "{case}");
            settled(&resumed).await;
            assert_eq!(
                file_notices(&resumed),
                [(1, first.to_owned()), (2, "completed".to_owned())],
                "{case}"
            );
            bounded("shutdown", resumed.session.shutdown()).await;
            nothing_left(&h, &resumed, &case);
        }
    }
}

// Ordered results: two children started by one parent turn settle in the
// reverse of their start order; their notices reach the parent's file in
// settlement order, each once, with the run each started, and the
// parent's next request carries them in that order.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn notices_are_admitted_in_settlement_order_once_each() {
    let h = harness("subagent-barrier-order");
    let gates = Gates::default();
    script(&h, &gates);
    let parent = parent(&h, None).await;
    bounded(
        "delegate",
        parent.session.prompt("delegate two", Vec::new()),
    )
    .await
    .expect("delegate");
    let started = tool_results(&parent.session, "subagent");
    let ids: Vec<String> = started
        .iter()
        .map(|(_, _, details)| {
            details["subagent"]["agentId"]
                .as_str()
                .expect("id")
                .to_owned()
        })
        .collect();
    assert_eq!(ids.len(), 2);
    let jobs = parent.session.jobs().clone();
    let posted = || async {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let mut events = jobs.subscribe();
                if jobs.has_undelivered() {
                    return;
                }
                events.changed().await;
            }
        })
        .await
        .expect("a notice was posted");
    };
    gates.second.notify_one();
    posted().await;
    assert!(
        bounded("delivery", parent.session.deliver_job_notices())
            .await
            .expect("delivered")
    );
    gates.first.notify_one();
    settled(&parent).await;
    let notices = customs(&parent.session, messages::NOTICE_TYPE);
    let order: Vec<(String, u64)> = notices
        .iter()
        .map(|(_, details)| {
            (
                details["agentId"].as_str().unwrap_or_default().to_owned(),
                details["runId"].as_u64().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(order, [(ids[1].clone(), 2), (ids[0].clone(), 1)]);
    assert!(notices[0].0.ends_with("second answer"));
    assert!(notices[1].0.ends_with("first answer"));
    assert_eq!(
        file_notices(&parent),
        [(2, "completed".to_owned()), (1, "completed".to_owned())]
    );
    bounded("shutdown", parent.session.shutdown()).await;
    nothing_left(&h, &parent, "order");
}

/// Pushes its name onto a log when dropped: when the work holding it ends.
/// The log's order is the order the pushes took its lock, one sequence for
/// every event of the test.
#[cfg(unix)]
struct Ended(Arc<Mutex<Vec<&'static str>>>, &'static str);

#[cfg(unix)]
impl Drop for Ended {
    fn drop(&mut self) {
        self.0.lock().expect("log").push(self.1);
    }
}

/// How long the child's job's producer takes to end after its process
/// has: a parent that does not wait for its child's teardown returns first
/// by this margin, so the log shows it. A correct parent waits however
/// long it is.
#[cfg(unix)]
const SLOW_END: Duration = Duration::from_millis(200);

/// Starts a background job on `jobs` whose producer is a real `sh`
/// process that writes its pid to `pid_file` and sleeps; logs `name`
/// `slow` after the process has ended. Returns the job and the process's
/// pid.
#[cfg(unix)]
async fn sleeping_job(
    jobs: &JobRegistry,
    pid_file: PathBuf,
    log: &Arc<Mutex<Vec<&'static str>>>,
    name: &'static str,
    slow: Duration,
) -> (crate::jobs::JobId, libc::pid_t) {
    let spec = bake_process::ProcessSpec {
        program: "sh".into(),
        args: vec![
            "-c".into(),
            format!("echo $$ > '{}'; exec sleep 1000", pid_file.display()).into(),
        ],
        cwd: None,
        env: None,
        stdin: false,
    };
    let ended = Ended(Arc::clone(log), name);
    let job = jobs
        .start(
            crate::jobs::JobSpec {
                kind: "bash",
                label: "sleep".into(),
                output: crate::jobs::OutputMode::Stream,
            },
            move |context| async move {
                let outcome = crate::jobs::process::run_process(context, spec).await;
                tokio::time::sleep(slow).await;
                drop(ended);
                outcome
            },
        )
        .expect("a job");
    let pid = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(pid) = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the job's process started");
    (job, pid)
}

/// Whether `pid` names no process any more.
#[cfg(unix)]
fn gone(pid: libc::pid_t) -> bool {
    // SAFETY: plain syscall with an integer pid and the null signal; it
    // only probes whether the pid exists.
    let probe = unsafe { libc::kill(pid, 0) };
    (probe, std::io::Error::last_os_error().raw_os_error()) == (-1, Some(libc::ESRCH))
}

// Ownership, bottom-up (scope 11 addition): a parent with a running child
// that has a background job of its own, a background job's process of the
// parent's, and a pending reminder. Esc aborts only the parent's turn; the
// child, both jobs, and the timer go on. The parent's shutdown then stops
// everything below it before it returns: the child's turn, and after it,
// as the child's own shutdown, the child's job's process; the parent's
// job's process; and the timer. Each ending is logged in one sequence,
// checked in the order it happened, and nothing of the parent or the
// child is left running.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_parent_stops_its_children_jobs_and_reminders_bottom_up() {
    use crate::schedules::{CHANGE_TYPE, Change, Record, Rule};
    // 2100-01-01T00:00:00.000Z: no real clock makes the reminder due.
    const FAR: i64 = 4_102_444_800_000;
    let h = harness("subagent-bottom-up");
    h.parent_settings.lock().expect("settings").schedules = true;
    let log: Arc<Mutex<Vec<&'static str>>> = Arc::default();
    let (child_entered, held) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let steps = (0..16)
        .map(|_| {
            let (log, child_entered, held) = (
                Arc::clone(&log),
                Arc::clone(&child_entered),
                Arc::clone(&held),
            );
            FauxResponseStep::Factory(Arc::new(move |context, options, _, _| {
                let messages = context.messages().to_vec();
                let signal = options.base.signal.clone();
                let (log, child_entered, held) = (
                    Arc::clone(&log),
                    Arc::clone(&child_entered),
                    Arc::clone(&held),
                );
                Box::pin(async move {
                    let last = last_user(&messages);
                    let ended = if is_child(&messages) {
                        child_entered.notify_one();
                        Ended(log, "child turn")
                    } else if last == "hold" {
                        held.notify_one();
                        Ended(log, "parent turn")
                    } else if after_tool(&messages) {
                        return Ok(faux_assistant_message("ok"));
                    } else {
                        return Ok(call(
                            "subagent",
                            json!({ "description": "work", "prompt": "Work." }),
                        ));
                    };
                    if let Some(signal) = signal {
                        signal.cancelled().await;
                    }
                    drop(ended);
                    Err("request cancelled".into())
                })
            }))
        })
        .collect();
    h.faux.set_responses(steps);
    let mut store = SessionManager::create("/tmp", Some(&h.place), NewSessionOptions::default())
        .expect("parent store");
    let owner = store.session_id().to_owned();
    store
        .append_durable_group([crate::session::GroupEntry::Custom {
            custom_type: CHANGE_TYPE.into(),
            data: Some(
                Change::Create {
                    session_id: owner,
                    record: Record {
                        id: "schedule-1".into(),
                        rule: Rule::At,
                        prompt: "later".into(),
                        scheduled_at: FAR + 3_600_000,
                    },
                }
                .to_json(),
            ),
        }])
        .expect("a pending reminder");
    let parent = parent(&h, Some(store)).await;
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&reads);
    parent.session.schedules().set_clock(Arc::new(move || {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        FAR
    }));
    let timer_log = Arc::clone(&log);
    parent
        .session
        .schedules()
        .set_timer_ended(Arc::new(move || {
            timer_log.lock().expect("log").push("timer");
        }));
    let reads = || reads.load(std::sync::atomic::Ordering::SeqCst);

    bounded("delegate", parent.session.prompt("delegate", Vec::new()))
        .await
        .expect("delegate");
    tokio::time::timeout(Duration::from_secs(10), child_entered.notified())
        .await
        .expect("the child's turn is running");
    let child_jobs = h.child_jobs.lock().expect("jobs")[0].clone();
    let (child_job, child_pid) = sleeping_job(
        &child_jobs,
        h.place.join("child-job.pid"),
        &log,
        "child job process",
        SLOW_END,
    )
    .await;
    let (job, pid) = sleeping_job(
        parent.session.jobs(),
        h.place.join("job.pid"),
        &log,
        "job process",
        Duration::ZERO,
    )
    .await;
    let running = Arc::clone(&parent.session);
    let hold = tokio::spawn(async move { running.prompt("hold", Vec::new()).await });
    tokio::time::timeout(Duration::from_secs(10), held.notified())
        .await
        .expect("the parent's turn is running");

    // Esc: the parent's turn alone.
    bounded("abort", parent.session.abort()).await;
    let _ = bounded("the held turn", hold).await.expect("the held turn");
    assert_eq!(*log.lock().expect("log"), ["parent turn"]);
    assert_eq!(parent.host.running(), 1, "the child runs on");
    assert_eq!(
        parent.session.jobs().get(job).expect("job").status,
        crate::jobs::JobStatus::Running
    );
    assert_eq!(
        child_jobs.get(child_job).expect("child job").status,
        crate::jobs::JobStatus::Running
    );
    let before = reads();
    parent.session.schedules().kick();
    tokio::time::timeout(Duration::from_secs(10), async {
        while reads() == before {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the timer looks on");

    // Shutdown: everything below the parent ends before it returns, and
    // the child's job ends after the child's turn, by the child's shutdown.
    bounded("shutdown", parent.session.shutdown()).await;
    log.lock().expect("log").push("parent shutdown");
    let ended = log.lock().expect("log").clone();
    let at = |name: &str| {
        let found: Vec<usize> = ended
            .iter()
            .enumerate()
            .filter(|(_, event)| **event == name)
            .map(|(index, _)| index)
            .collect();
        assert_eq!(found.len(), 1, "{name} once: {ended:?}");
        found[0]
    };
    assert_eq!(at("parent turn"), 0, "{ended:?}");
    assert_eq!(at("parent shutdown"), ended.len() - 1, "{ended:?}");
    assert_eq!(ended.len(), 6, "{ended:?}");
    for below in ["child turn", "child job process", "job process", "timer"] {
        assert!(at(below) < at("parent shutdown"), "{below}: {ended:?}");
    }
    assert!(
        at("child turn") < at("child job process"),
        "the child's job ended before its turn: {ended:?}"
    );
    nothing_left(&h, &parent, "bottom-up");
    assert_eq!(
        parent.session.jobs().get(job).expect("job").status,
        crate::jobs::JobStatus::Killed
    );
    assert_eq!(
        child_jobs.get(child_job).expect("child job").status,
        crate::jobs::JobStatus::Killed
    );
    assert!(gone(pid), "the job's process outlived the parent");
    assert!(
        gone(child_pid),
        "the child's job's process outlived the parent"
    );
    assert_eq!(h.faux.call_count(), 4, "delegate, its answer, child, hold");
}
