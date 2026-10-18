//! Bake: session shutdown stops a running background `bash` job and returns
//! only once the job's process group is gone and the job is terminal; then
//! nothing more happens (no notice turn reaches the model) and a new job is
//! refused.
//!
//! Linux only, and a test binary of its own, as `process_reap.rs` is: it
//! makes this process a child subreaper (`PR_SET_CHILD_SUBREAPER`), so the
//! job's killed descendant, whose shell has died, is reparented here and
//! stays a zombie in the job's process group until the test reaps it. A
//! shutdown that must wait for the group stays pending until then; the
//! [`EARLY`] window only gives a broken implementation time to finish
//! early, so it can make a negative control slower to catch, never this
//! test flaky.
#![cfg(target_os = "linux")]

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bake_ai::providers::faux::{
    FauxProvider, FauxProviderOptions, FauxResponseStep, faux_assistant_blocks,
    faux_assistant_message, faux_tool_call,
};
use bake_ai::{ApiRegistry, StopReason};
use bake_coding_agent::agent_session::{AgentSession, AgentSessionOptions};
use bake_coding_agent::auth_storage::AuthStorage;
use bake_coding_agent::jobs::{
    JobLimits, JobOutcome, JobRegistry, JobSpec, JobStatus, OutputMode, StartError, StopCause,
};
use bake_coding_agent::model_registry::ModelRegistry;
use bake_coding_agent::model_registry::config::ModelConfig;
use bake_coding_agent::owned_work::WorkGate;
use bake_coding_agent::session::{NewSessionOptions, SessionManager};
use bake_coding_agent::settings::Settings;
use bake_coding_agent::system_prompt::SystemPromptOptions;
use bake_coding_agent::tools::bash::{BashToolOptions, create_bash_tool};
use serde_json::json;
use tokio::io::AsyncBufReadExt;

/// Bounds waits for states the test already caused.
const BOUND: Duration = Duration::from_secs(10);
/// How long a broken implementation gets to finish early.
const EARLY: Duration = Duration::from_millis(500);

fn become_subreaper() {
    // SAFETY: plain prctl with integer arguments; it only changes how this
    // process's orphaned descendants are reparented.
    let set = unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };
    assert_eq!(set, 0, "PR_SET_CHILD_SUBREAPER");
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let mut random = [0; 16];
        getrandom::getrandom(&mut random).expect("random");
        let id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let dir = std::env::temp_dir().join(format!("bake-session-job-reap-{id}"));
        fs::create_dir(&dir).expect("temp dir");
        Self(dir)
    }

    /// A FIFO in the directory, opened for reading without waiting for a
    /// writer.
    fn fifo(
        &self,
    ) -> (
        PathBuf,
        tokio::io::BufReader<tokio::net::unix::pipe::Receiver>,
    ) {
        let path = self.0.join("pid.fifo");
        let c_path = CString::new(path.as_os_str().as_bytes()).expect("path");
        // SAFETY: `c_path` is a valid NUL-terminated path for the call.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0, "mkfifo");
        let receiver = tokio::net::unix::pipe::OpenOptions::new()
            .read_write(true)
            .open_receiver(&path)
            .expect("open fifo");
        (path, tokio::io::BufReader::new(receiver))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A descendant pid the test owns: dropped unreaped, it is killed and
/// reaped, so a failing test leaves nothing behind.
struct Descendant {
    pid: libc::pid_t,
    reaped: bool,
}

impl Descendant {
    async fn read(reader: &mut tokio::io::BufReader<tokio::net::unix::pipe::Receiver>) -> Self {
        let mut line = String::new();
        tokio::time::timeout(BOUND, reader.read_line(&mut line))
            .await
            .expect("the job reports its descendant")
            .expect("read fifo");
        let pid = line.trim().parse().expect("a pid");
        Self { pid, reaped: false }
    }

    /// Reaps the descendant, which must have been killed by `SIGKILL`; it
    /// is a zombie of this subreaper, or about to be one.
    async fn reap_killed(&mut self) {
        let pid = self.pid;
        let status = tokio::time::timeout(
            BOUND,
            tokio::task::spawn_blocking(move || {
                let mut status = 0;
                // SAFETY: plain syscall; `status` is a valid out pointer.
                let reaped = unsafe { libc::waitpid(pid, &mut status, 0) };
                (reaped, status)
            }),
        )
        .await
        .expect("the descendant dies")
        .expect("joined");
        self.reaped = true;
        assert_eq!(status.0, pid, "reaped here, as an orphan");
        assert!(
            libc::WIFSIGNALED(status.1) && libc::WTERMSIG(status.1) == libc::SIGKILL,
            "the group kill reached the descendant"
        );
    }
}

impl Drop for Descendant {
    fn drop(&mut self) {
        if !self.reaped {
            // SAFETY: plain syscalls on a pid this test still owns: it is
            // this process's child, or not yet reaped, so not reused.
            unsafe {
                // Not a child (yet): `waitpid` fails at once.
                libc::kill(self.pid, libc::SIGKILL);
                libc::waitpid(self.pid, std::ptr::null_mut(), 0);
            }
        }
    }
}

// The job outlives the turn that started it; shutdown stops it as a
// shutdown and waits for its group, then refuses another job.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_stops_a_background_job_and_waits_for_its_group() {
    become_subreaper();
    let dir = TempDir::new();
    let (fifo, mut reader) = dir.fifo();
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
                "models": [{ "id": "faux-1" }],
            }),
            Some("test-key".into()),
        )
        .expect("faux provider");
    // The shell becomes `sleep`, so it cannot reap its own descendant.
    let command = format!(
        "sleep 1000 & echo $! > '{}'; exec sleep 1000",
        fifo.display()
    );
    faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_blocks(
            vec![faux_tool_call(
                "bash",
                json!({ "command": command, "run_in_background": true }),
                Some("call-1"),
            )],
            StopReason::ToolUse,
        )),
        FauxResponseStep::Message(faux_assistant_message("started")),
    ]);
    let registry = Arc::new(registry);
    let cwd = dir.0.to_string_lossy().into_owned();
    let work = WorkGate::default();
    let jobs = JobRegistry::new(work.clone(), JobLimits::default());
    let tool = create_bash_tool(
        &dir.0,
        BashToolOptions {
            spill_directory: Some(dir.0.clone()),
            owner: Some(work.clone()),
            jobs: Some(jobs.clone()),
            ..BashToolOptions::default()
        },
    );
    let session = SessionManager::in_memory(Some(&cwd), NewSessionOptions::default(), Vec::new())
        .expect("session");
    let session = Arc::new(
        AgentSession::create(AgentSessionOptions {
            session,
            registry: Arc::clone(&registry).into(),
            settings_store: None,
            selection: None,
            apis,
            settings: Settings::default(),
            model: registry.model("faux", "faux-1"),
            thinking_level: None,
            tools: vec![Arc::new(tool)],
            active_tool_names: vec!["bash".into()],
            tool_prompts: Vec::new(),
            system_prompt: SystemPromptOptions::new(&cwd),
            docs_command: "bake-rs docs".into(),
            plugins: bake_coding_agent::plugins::PluginRegistry::default(),
            work: Some(work.clone()),
            jobs: Some(jobs.clone()),
        })
        .await
        .expect("session"),
    );
    tokio::time::timeout(BOUND, session.prompt("start it", Vec::new()))
        .await
        .expect("the turn ends")
        .expect("prompt");
    let mut descendant = Descendant::read(&mut reader).await;
    let [job] = <[_; 1]>::try_from(session.jobs().list()).expect("one job");
    assert_eq!(job.status, JobStatus::Running, "the job outlives its turn");
    let calls = faux.call_count();

    let mut shutdown = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.shutdown().await })
    };
    let early = tokio::time::timeout(EARLY, &mut shutdown).await.is_ok();
    // Before the reap, which an early return can outrun; the descendant's
    // guard cleans up.
    assert!(!early, "shutdown returned while the job's group was alive");
    descendant.reap_killed().await;
    let finished = tokio::time::timeout(BOUND, shutdown).await.is_ok();
    assert!(finished, "shutdown finishes once the group is gone");

    let ended = session.jobs().get(job.id).expect("kept");
    assert_eq!(
        (ended.status, ended.stop),
        (JobStatus::Killed, Some(StopCause::Shutdown))
    );
    assert!(work.is_closed(), "the session's gate drained");
    assert_eq!(faux.call_count(), calls, "a turn ran after shutdown");
    let refused = session
        .jobs()
        .start(
            JobSpec {
                kind: "bash",
                label: "late".into(),
                output: OutputMode::Final,
            },
            |_| async { JobOutcome::completed("") },
        )
        .map(|_| ());
    assert_eq!(refused, Err(StartError::Closed));
}
