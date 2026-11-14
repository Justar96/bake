//! The host over a stand-in for `bake --mode rpc`: a shell script that
//! answers `get_state`, runs each prompt as one turn, and exits when its
//! input closes, as RPC mode does.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::jobs::JobLimits;
use crate::permissions::ApprovalPolicy;
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
        questions: None,
        standing: Arc::new(StandingPermissions::new(
            SandboxMode::WorkspaceWrite,
            ApprovalPolicy::Ask,
            "workspace-write",
            dir,
            None,
        )),
        model: Arc::new(|| Some("faux/parent".to_owned())),
    })
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
    let mail = jobs.claim_mail();
    assert_eq!(mail.len(), 1);
    assert_eq!(mail[0].custom_type, NOTICE_TYPE);
    let UserContent::Blocks(blocks) = &mail[0].content else {
        panic!()
    };
    let UserContentBlock::Text(text) = &blocks[0] else {
        panic!()
    };
    assert_eq!(
        text.text,
        "[spawn] agent-1, which you started on \"write the tests\", finished a run and is idle. Its last answer:\ndone as agent-1"
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
        assert_eq!(ended.state, SpawnedState::Exited(expected));
        work.close().await;
        std::fs::remove_dir_all(dir).unwrap();
    }
}
