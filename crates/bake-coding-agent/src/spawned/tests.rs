//! The host over a stand-in for `bake --mode rpc`: a shell script that
//! answers `get_state`, runs each prompt as one turn, and exits when its
//! input closes, as RPC mode does.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::jobs::JobLimits;
use crate::permissions::ApprovalPolicy;
use crate::questions::QuestionBroker;
use bake_sandbox::SandboxMode;

const AGENT: &str = r#"#!/bin/sh
printf '%s\n' "$*" > "$0.args"
while IFS= read -r line; do
  case "$line" in
    *get_state*) echo '{"type":"response","id":"bake-spawn-state","command":"get_state","success":true,"data":{"sessionId":"child-1","model":{"provider":"faux","id":"m"}}}' ;;
    *'"abort"'*) echo '{"type":"agent_settled","aborted":true,"outcome":{"kind":"cancelled"}}' ;;
    *'"prompt"'*)
      echo '{"type":"agent_start"}'
      echo '{"type":"tool_execution_start","toolName":"bash","toolCallId":"c1"}'
      echo '{"type":"tool_execution_end","toolName":"bash","toolCallId":"c1"}'
      echo '{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"done as '"$BAKE_TELECOM_CALLSIGN"'"}]}}'
      echo '{"type":"agent_settled","aborted":false,"outcome":{"kind":"finished_turn"}}' ;;
  esac
done
"#;

fn temp() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bs-{}", crate::session::id::uuid_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn host(dir: &std::path::Path, work: &WorkGate, jobs: &JobRegistry) -> SpawnHost {
    host_running(dir, work, jobs, AGENT)
}

fn host_running(
    dir: &std::path::Path,
    work: &WorkGate,
    jobs: &JobRegistry,
    script: &str,
) -> SpawnHost {
    host_with(dir, work, jobs, script, None, STOP_GRACE)
}

fn host_with(
    dir: &std::path::Path,
    work: &WorkGate,
    jobs: &JobRegistry,
    script: &str,
    questions: Option<QuestionBroker>,
    stop_grace: Duration,
) -> SpawnHost {
    let program = dir.join("bake");
    std::fs::write(&program, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    SpawnHost::new(HostOptions {
        program,
        cwd: dir.to_path_buf(),
        station: None,
        jobs: jobs.clone(),
        work: work.clone(),
        questions,
        standing: Arc::new(StandingPermissions::new(
            SandboxMode::WorkspaceWrite,
            ApprovalPolicy::Ask,
            "workspace-write",
            dir,
            None,
        )),
        model: Arc::new(|| Some("faux/parent".to_owned())),
        stop_grace,
    })
}

/// Polls `check` until it holds, as `until` does for one agent.
async fn eventually(what: &str, check: impl Fn() -> bool) {
    for _ in 0..3000 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never reached: {what}");
}

/// The text of each notice posted to the parent, oldest first.
fn notices(jobs: &JobRegistry) -> Vec<String> {
    jobs.claim_mail()
        .into_iter()
        .map(|message| {
            assert_eq!(message.custom_type, NOTICE_TYPE);
            match message.content {
                UserContent::Blocks(blocks) => blocks
                    .into_iter()
                    .filter_map(|block| match block {
                        UserContentBlock::Text(text) => Some(text.text),
                        _ => None,
                    })
                    .collect(),
                UserContent::Text(text) => text,
            }
        })
        .collect()
}

/// Calls the spawn tool `name` over `host`; returns its text.
async fn call(host: &SpawnHost, name: &str, args: serde_json::Value) -> Result<String, String> {
    let tool = tools(host)
        .into_iter()
        .find(|definition| definition.name() == name)
        .expect("a spawn tool")
        .tool;
    let result = (tool.execute)(
        "call".into(),
        args,
        None,
        bake_agent::AgentToolUpdateCallback::ignore(),
        bake_agent::ToolContext::detached(),
    )
    .await?;
    Ok(result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect())
}

/// Waits for `n` notices and returns them; none more are waiting then.
async fn told(jobs: &JobRegistry, n: usize) -> Vec<String> {
    let mut seen = Vec::new();
    for _ in 0..3000 {
        seen.extend(notices(jobs));
        if seen.len() >= n {
            assert_eq!(seen.len(), n, "{seen:?}");
            return seen;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{n} notices never came: {seen:?}");
}

fn state_of(host: &SpawnHost, id: u64) -> SpawnedState {
    host.list()
        .into_iter()
        .find(|agent| agent.id == id)
        .map(|agent| agent.state)
        .unwrap_or(SpawnedState::Exited(None))
}

async fn until(host: &SpawnHost, check: impl Fn(&SpawnedAgent) -> bool) -> SpawnedAgent {
    for _ in 0..500 {
        if let Some(agent) = host.list().into_iter().next()
            && check(&agent)
        {
            return agent;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never reached: {:?}", host.list());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_runs_reports_to_its_parent_and_stops() {
    let dir = temp();
    let work = WorkGate::default();
    let jobs = JobRegistry::new(work.clone(), JobLimits::default());
    let host = host(&dir, &work, &jobs);

    let started = host.spawn("write the tests", None, Origin::Model).unwrap();
    assert_eq!(started.name, "agent-1");
    assert_eq!(started.model.as_deref(), Some("faux/parent"));
    let idle = until(&host, |agent| agent.runs == 1).await;
    assert_eq!(idle.state, SpawnedState::Idle);
    assert_eq!(idle.session_id.as_deref(), Some("child-1"));
    assert_eq!(idle.last_line.as_deref(), Some("done as agent-1"));

    let args = std::fs::read_to_string(dir.join("bake.args")).unwrap();
    assert_eq!(
        args.trim(),
        "--mode rpc --sandbox workspace-write --approval ask --name agent-1 --model faux/parent"
    );
    assert_eq!(
        told(&jobs, 1).await,
        [
            "[spawn] agent-1, which you started on \"write the tests\", finished a run and is idle. Its last answer:\ndone as agent-1"
        ]
    );

    // A person's message starts another run.
    host.message(started.id, "and the docs").unwrap();
    until(&host, |agent| agent.runs == 2).await;

    host.stop(started.id).unwrap();
    let exited = until(&host, |agent| !agent.state.is_live()).await;
    assert_eq!(exited.state, SpawnedState::Exited(None));
    assert!(host.message(started.id, "late").is_err());
    host.clear_exited();
    assert!(host.list().is_empty());

    work.close().await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_session_ending_ends_its_agents() {
    let dir = temp();
    let work = WorkGate::default();
    let jobs = JobRegistry::new(work.clone(), JobLimits::default());
    let host = host(&dir, &work, &jobs);
    host.spawn("one", None, Origin::Model).unwrap();
    host.spawn("two", None, Origin::Model).unwrap();
    assert_eq!(host.list()[1].name, "agent-2");
    until(&host, |agent| agent.runs == 1).await;
    tokio::time::timeout(Duration::from_secs(30), work.close())
        .await
        .expect("the agents are awaited");
    assert!(
        host.list().iter().all(|agent| !agent.state.is_live()),
        "{:?}",
        host.list()
    );
    assert!(
        host.spawn("three", None, Origin::Model).is_err(),
        "a closed session starts nothing"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_task_is_required_and_bounded() {
    let dir = temp();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _guard = runtime.enter();
    let work = WorkGate::default();
    let jobs = JobRegistry::new(work.clone(), JobLimits::default());
    let host = host(&dir, &work, &jobs);
    assert!(host.spawn("  ", None, Origin::Model).is_err());
    assert!(
        host.spawn(
            &"x".repeat(crate::telecom::wire::TEXT_CAP + 1),
            None,
            Origin::Model
        )
        .is_err()
    );
    assert!(
        host.find("nobody")
            .unwrap_err()
            .contains("none was started")
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_that_cannot_start_says_why() {
    // As bake does with no usable model: a notice on standard error over
    // two lines, then status 1.
    const REFUSES: &str = "#!/bin/sh\nprintf 'No models available. Add a provider.\\n  See: bake docs models\\n' >&2\nexit 1\n";
    // A clean exit with something on standard error is not a failure.
    const QUIET: &str = "#!/bin/sh\necho 'a warning' >&2\nexit 0\n";
    for (script, expected) in [
        (
            REFUSES,
            Some("No models available. Add a provider. See: bake docs models".to_owned()),
        ),
        (QUIET, None),
    ] {
        let dir = temp();
        let work = WorkGate::default();
        let jobs = JobRegistry::new(work.clone(), JobLimits::default());
        let host = host_running(&dir, &work, &jobs, script);
        host.spawn("anything", None, Origin::Model).unwrap();
        let ended = until(&host, |agent| !agent.state.is_live()).await;
        assert_eq!(ended.state, SpawnedState::Exited(expected.clone()));
        // The parent's model is told once how the agent ended.
        let told = match expected {
            Some(reason) => {
                format!("[spawn] agent-1, which you started on \"anything\", exited: {reason}")
            }
            None => "[spawn] agent-1, which you started on \"anything\", exited.".to_owned(),
        };
        assert_eq!(notices(&jobs), [told]);
        work.close().await;
        assert!(notices(&jobs).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_that_ignores_the_end_of_its_input_is_killed_after_the_grace() {
    // Its output stays open past the end of its input, as a hung shutdown
    // keeps it.
    const WEDGED: &str =
        "#!/bin/sh\nIFS= read -r line\necho '{\"type\":\"agent_start\"}'\nexec sleep 600\n";
    let dir = temp();
    let work = WorkGate::default();
    let jobs = JobRegistry::new(work.clone(), JobLimits::default());
    let host = host_with(&dir, &work, &jobs, WEDGED, None, Duration::from_millis(200));
    let stopped = host.spawn("hang", None, Origin::Model).unwrap();
    let closed = host.spawn("hang too", None, Origin::Model).unwrap();
    eventually("both work", || {
        host.list()
            .iter()
            .all(|agent| agent.state == SpawnedState::Working)
    })
    .await;

    host.stop(stopped.id).unwrap();
    // Stopping, it takes no more commands.
    assert_eq!(
        host.message(stopped.id, "late").unwrap_err(),
        "agent-1 is exiting"
    );
    eventually("the stopped agent is killed", || {
        state_of(&host, stopped.id) == SpawnedState::Exited(None)
    })
    .await;

    // Closing the session waits only the grace for the other.
    tokio::time::timeout(Duration::from_secs(30), work.close())
        .await
        .expect("closing never waits on a wedged agent");
    assert_eq!(state_of(&host, closed.id), SpawnedState::Exited(None));
    // The parent stopped both, so neither is reported.
    assert!(notices(&jobs).is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn commands_during_one_long_slowly_written_event_lose_nothing() {
    // About 100 KB of one event line, written in pieces.
    const SLOW: &str = r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *bake-spawn-task*)
      echo '{"type":"agent_start"}'
      : > "$0.writing"
      printf '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"'
      i=0
      while [ $i -lt 40 ]; do
        printf '%02560d' 0
        sleep 0.02
        i=$((i+1))
      done
      printf '"}}\n'
      echo '{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"long done"}]}}'
      echo '{"type":"agent_settled","aborted":false,"outcome":{"kind":"finished_turn"}}' ;;
  esac
done
"#;
    let dir = temp();
    let work = WorkGate::default();
    let jobs = JobRegistry::new(work.clone(), JobLimits::default());
    let host = host_running(&dir, &work, &jobs, SLOW);
    let agent = host.spawn("write slowly", None, Origin::Model).unwrap();
    let writing = dir.join("bake.writing");
    eventually("the long line starts", || writing.exists()).await;
    // Each command wins the driver's select while the line is half read.
    for n in 0..10 {
        host.message(agent.id, &format!("steer {n}")).unwrap();
        host.interrupt(agent.id).unwrap();
        tokio::task::yield_now().await;
    }
    let told = told(&jobs, 1).await;
    let idle = until(&host, |agent| agent.runs == 1).await;
    assert_eq!(idle.last_line.as_deref(), Some("long done"));
    assert!(told[0].ends_with("Its last answer:\nlong done"), "{told:?}");
    work.close().await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_exited_agents_question_is_withdrawn() {
    const ASKS: &str = r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *bake-spawn-task*)
      echo '{"type":"agent_start"}'
      echo '{"type":"extension_ui_request","id":"q1","method":"select","title":"Pick","options":["a","b"]}' ;;
    *bake-spawn-msg*) exit 0 ;;
  esac
done
"#;
    let dir = temp();
    let work = WorkGate::default();
    let jobs = JobRegistry::new(work.clone(), JobLimits::default());
    let broker = QuestionBroker::new();
    assert!(broker.connect(Box::new(|_| true)));
    let host = host_with(&dir, &work, &jobs, ASKS, Some(broker.clone()), STOP_GRACE);
    let agent = host.spawn("pick", None, Origin::Model).unwrap();
    eventually("the question is forwarded", || broker.pending().len() == 1).await;
    assert_eq!(broker.pending()[0].from.as_deref(), Some("agent-1"));
    assert_eq!(state_of(&host, agent.id), SpawnedState::Asking);

    // The agent exits with its question unanswered.
    host.message(agent.id, "bye").unwrap();
    until(&host, |agent| !agent.state.is_live()).await;
    assert!(broker.pending().is_empty(), "{:?}", broker.pending());
    assert_eq!(
        notices(&jobs),
        ["[spawn] agent-1, which you started on \"pick\", exited before its run finished."]
    );

    // A later question is shown and answered.
    let asking = tokio::spawn({
        let broker = broker.clone();
        async move {
            broker
                .ask(
                    "c2",
                    None,
                    vec![crate::questions::Question {
                        question: "next?".into(),
                        header: String::new(),
                        options: Vec::new(),
                        multi_select: false,
                    }],
                    None,
                )
                .await
        }
    });
    eventually("the next question is shown", || broker.pending().len() == 1).await;
    let shown = broker.pending()[0].generation;
    assert!(broker.answer(
        shown,
        crate::questions::QuestionAnswer::Answered(vec![vec!["yes".into()]])
    ));
    assert!(asking.await.unwrap().is_ok());
    work.close().await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn past_the_cap_an_agent_queues_and_starts_when_a_slot_frees() {
    let dir = temp();
    let work = WorkGate::default();
    let jobs = JobRegistry::new(work.clone(), JobLimits::default());
    let host = host(&dir, &work, &jobs);
    let running: Vec<SpawnedAgent> = (0..MAX_LIVE)
        .map(|n| {
            host.spawn(&format!("task {n}"), None, Origin::Model)
                .unwrap()
        })
        .collect();
    assert!(
        running
            .iter()
            .all(|agent| agent.state == SpawnedState::Starting)
    );
    told(&jobs, MAX_LIVE).await;

    assert_eq!(
        call(&host, "spawn_agent", serde_json::json!({ "task": "the seventh" })).await,
        Ok("queued agent-7: 6 agents are running; it starts when one exits or is stopped (stop_agent)".to_owned())
    );
    let waiting = host.find("agent-7").unwrap();
    assert_eq!(waiting.state, SpawnedState::Queued);
    assert_eq!(host.running(), MAX_LIVE);

    // A run that settles while one waits says how to free a slot.
    host.message(running[0].id, "more").unwrap();
    let told = told(&jobs, 1).await;
    assert!(
        told[0].contains("finished a run and is idle; 1 agent is queued, and stopping this one (stop_agent) if its work is done lets it start. Its last answer:"),
        "{told:?}"
    );

    // Stopping one starts the queued agent before the stopped one exits.
    host.stop(running[0].id).unwrap();
    eventually("the seventh runs", || {
        host.list()
            .iter()
            .any(|agent| agent.id == waiting.id && agent.runs == 1)
    })
    .await;

    // Stopping a queued agent cancels it, untold.
    let cancelled = host.spawn("the eighth", None, Origin::Model).unwrap();
    assert_eq!(cancelled.state, SpawnedState::Queued);
    assert_eq!(
        call(
            &host,
            "stop_agent",
            serde_json::json!({ "agent": cancelled.name, "interrupt": true })
        )
        .await,
        Err(format!(
            "stop_agent: {} is queued and has no run to stop",
            cancelled.name
        ))
    );
    assert_eq!(
        call(
            &host,
            "stop_agent",
            serde_json::json!({ "agent": cancelled.name })
        )
        .await,
        Ok(format!("{} was queued and will not start", cancelled.name))
    );
    eventually("the eighth is cancelled", || {
        state_of(&host, cancelled.id) == SpawnedState::Exited(None)
    })
    .await;

    // The queue is bounded, and its refusal says what to do.
    eventually("one finished", || {
        host.list()
            .iter()
            .any(|agent| agent.id == waiting.id && agent.runs == 1)
    })
    .await;
    for n in 0..MAX_QUEUED {
        let queued = host
            .spawn(&format!("queued {n}"), None, Origin::Model)
            .unwrap();
        assert_eq!(queued.state, SpawnedState::Queued);
    }
    let refused = host.spawn("one too many", None, Origin::Model).unwrap_err();
    assert_eq!(
        refused,
        format!(
            "6 agents are running and {MAX_QUEUED} are queued; stop one (stop_agent), or wait for their notices, before starting another"
        )
    );

    tokio::time::timeout(Duration::from_secs(60), work.close())
        .await
        .expect("queued agents end with the session");
    assert!(host.list().iter().all(|agent| !agent.state.is_live()));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_working_agent_holds_the_parents_live_count_until_its_notice() {
    let dir = temp();
    let work = WorkGate::default();
    let jobs = JobRegistry::new(work.clone(), JobLimits::default());
    let host = host(&dir, &work, &jobs);
    let agent = host.spawn("count", None, Origin::Model).unwrap();
    assert_eq!(jobs.live(), 1, "held from the spawn");
    eventually("the run is reported", || jobs.live() == 0).await;
    // Posted before the hold is released.
    assert_eq!(notices(&jobs).len(), 1);

    host.message(agent.id, "again").unwrap();
    eventually("the second run is reported", || {
        state_of(&host, agent.id) == SpawnedState::Idle
            && host.list()[0].runs == 2
            && jobs.live() == 0
    })
    .await;
    assert_eq!(notices(&jobs).len(), 1);
    work.close().await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_task_is_reported_once() {
    const BUSY: &str = r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *bake-spawn-task*) echo '{"type":"response","id":"bake-spawn-task","command":"prompt","success":false,"error":"busy"}' ;;
  esac
done
"#;
    let dir = temp();
    let work = WorkGate::default();
    let jobs = JobRegistry::new(work.clone(), JobLimits::default());
    let host = host_running(&dir, &work, &jobs, BUSY);
    let agent = host.spawn("x", None, Origin::Model).unwrap();
    until(&host, |agent| {
        agent.state == SpawnedState::Failed("busy".into())
    })
    .await;
    assert_eq!(
        notices(&jobs),
        ["[spawn] agent-1, which you started on \"x\", could not start its task: busy"]
    );
    eventually("the hold is released", || jobs.live() == 0).await;
    host.stop(agent.id).unwrap();
    work.close().await;
    assert!(notices(&jobs).is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_message_is_sent_again_once_as_a_follow_up() {
    // Refuses the first two messages, and the second one's follow-up too.
    const REFUSES: &str = r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *get_state*) ;;
    *'"bake-spawn-msg-1"'*|*'"bake-spawn-msg-2"'*|*'"bake-spawn-msg-2-again"'*)
      id=$(printf '%s' "$line" | sed 's/.*"id":"\([^"]*\)".*/\1/')
      cmd=prompt
      case "$line" in *follow_up*) cmd=follow_up ;; esac
      echo '{"type":"response","id":"'"$id"'","command":"'"$cmd"'","success":false,"error":"compacting"}' ;;
    *)
      echo '{"type":"agent_start"}'
      echo '{"type":"agent_settled","aborted":false,"outcome":{"kind":"finished_turn"}}' ;;
  esac
done
"#;
    let dir = temp();
    let work = WorkGate::default();
    let jobs = JobRegistry::new(work.clone(), JobLimits::default());
    let host = host_running(&dir, &work, &jobs, REFUSES);
    let agent = host.spawn("x", None, Origin::Person).unwrap();
    until(&host, |agent| agent.runs == 1).await;
    host.message(agent.id, "one").unwrap();
    until(&host, |agent| agent.runs == 2).await;
    host.message(agent.id, "two").unwrap();
    until(&host, |agent| {
        agent.state == SpawnedState::Failed("your message was not delivered: compacting".into())
    })
    .await;
    work.close().await;
    std::fs::remove_dir_all(dir).unwrap();
}
