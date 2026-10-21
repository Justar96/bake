//! Bake: a killed tool process is awaited with its process group, whether
//! the call is aborted, times out, or is dropped, with no time limit; a
//! dropped call's tree is settled under its owner (a `WorkGate`), whose
//! close waits for it and only for it; session shutdown waits for the
//! trees of the calls it cancels; a normal exit leaves its background
//! descendants running and never signals them later, and a normal session
//! shutdown leaves them running too (no exit drain in 0.4.0); the leader stays
//! unreaped while its tree can be signalled; and, as pid 1, the owner reaps
//! the group's orphaned zombies itself.
//!
//! Linux only, and a test binary of its own: it makes this process a child
//! subreaper (`PR_SET_CHILD_SUBREAPER`), so a killed descendant whose shell
//! has died is reparented here and stays a zombie, still in its process
//! group, until the test reaps it with `waitpid`. That makes "the group is
//! gone" an event the test releases rather than a race it hopes to win: a
//! call that must wait for the group stays pending until then. Tests run
//! one at a time ([`SERIAL`]), since the reaper's drain is process-wide.
//!
//! Each command reports its descendant's pid through a FIFO the test owns.
//! The pending checks give a broken implementation [`EARLY`] to finish
//! early; a correct one cannot finish before the release at all, so that
//! window can only make a negative control slower to catch, never this
//! test flaky.
#![cfg(target_os = "linux")]

use std::ffi::{CString, OsString};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bake_ai::providers::faux::{
    FauxProvider, FauxProviderOptions, FauxResponseStep, faux_assistant_blocks,
    faux_assistant_message, faux_tool_call,
};
use bake_ai::{AbortController, ApiRegistry, StopReason};
use bake_coding_agent::agent_session::{AgentSession, AgentSessionOptions};
use bake_coding_agent::auth_storage::AuthStorage;
use bake_coding_agent::exec::{ExecOptions, exec_command};
use bake_coding_agent::model_registry::ModelRegistry;
use bake_coding_agent::model_registry::config::ModelConfig;
use bake_coding_agent::owned_work::WorkGate;
use bake_coding_agent::session::{NewSessionOptions, SessionManager};
use bake_coding_agent::settings::Settings;
use bake_coding_agent::system_prompt::SystemPromptOptions;
use bake_coding_agent::tools::bash::{
    BashToolOptions, create_bash_tool, kill_tracked_detached_children,
};
use serde_json::json;
use tokio::io::AsyncBufReadExt;

/// Bounds waits for states the test already caused.
const BOUND: Duration = Duration::from_secs(10);
/// How long a broken implementation gets to finish early.
const EARLY: Duration = Duration::from_millis(500);

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
        let dir = std::env::temp_dir().join(format!("bake-process-reap-{id}"));
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
            .expect("the command reports its descendant")
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

/// `sh -c` script: a background descendant in the shell's group reports
/// its pid, and the shell then becomes `sleep`, so it cannot reap the
/// descendant itself.
fn holding_script(fifo: &Path) -> Vec<OsString> {
    vec![
        "-c".into(),
        format!(
            "sleep 1000 & echo $! > '{}'; exec sleep 1000",
            fifo.display()
        )
        .into(),
    ]
}

fn exec(
    args: Vec<OsString>,
    cwd: PathBuf,
    options: ExecOptions,
) -> tokio::task::JoinHandle<bake_coding_agent::exec::ExecResult> {
    tokio::spawn(async move { exec_command("sh".as_ref(), &args, &cwd, options).await })
}

/// Whether `pid` has been sent SIGKILL, or is dead: the kernel queues a
/// fatal signal on the target during the `kill` call itself, so once that
/// call has returned this sees it, with no window to wait out.
fn signalled_or_gone(pid: libc::pid_t) -> bool {
    let Ok(status) = fs::read_to_string(format!("/proc/{pid}/status")) else {
        return true;
    };
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .map(str::trim)
            .map(str::to_owned)
    };
    if field("State:").is_some_and(|state| state.starts_with('Z') || state.starts_with('X')) {
        return true;
    }
    let sigkill = 1u64 << (libc::SIGKILL - 1);
    ["SigPnd:", "ShdPnd:"].iter().any(|name| {
        field(name)
            .and_then(|mask| u64::from_str_radix(&mask, 16).ok())
            .is_some_and(|mask| mask & sigkill != 0)
    })
}

/// `Some('Z')` for a zombie, `None` once reaped.
fn state(pid: libc::pid_t) -> Option<char> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.trim_start().chars().next())
}

// An aborted call kills its group and returns only once the descendant left
// in the group is gone, however long that takes: the window is past the
// five-second limit the review found, after which the source gave up.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_aborted_exec_waits_for_its_process_group_without_a_limit() {
    let _serial = SERIAL.lock().await;
    become_subreaper();
    let dir = TempDir::new();
    let (fifo, mut reader) = dir.fifo();
    let abort = AbortController::new();
    let mut call = exec(
        holding_script(&fifo),
        dir.0.clone(),
        ExecOptions {
            signal: Some(abort.signal()),
            ..ExecOptions::default()
        },
    );
    let mut descendant = Descendant::read(&mut reader).await;
    abort.abort();
    let early = tokio::time::timeout(Duration::from_millis(6500), &mut call)
        .await
        .is_ok();
    descendant.reap_killed().await;
    let result = if early {
        None
    } else {
        tokio::time::timeout(BOUND, call)
            .await
            .ok()
            .map(|joined| joined.expect("joined"))
    };
    assert!(!early, "the call returned while its group was alive");
    let result = result.expect("the call returns once the group is gone");
    assert!(result.aborted && result.killed);
}

// A timed-out call does the same.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_timed_out_exec_waits_for_its_process_group() {
    let _serial = SERIAL.lock().await;
    become_subreaper();
    let dir = TempDir::new();
    let (fifo, mut reader) = dir.fifo();
    // The timeout starts the kill; the descendant is reported well before
    // it, and nothing the test checks depends on how long it is.
    let mut call = exec(
        holding_script(&fifo),
        dir.0.clone(),
        ExecOptions {
            timeout: Some(Duration::from_millis(300)),
            ..ExecOptions::default()
        },
    );
    let mut descendant = Descendant::read(&mut reader).await;
    let early = tokio::time::timeout(EARLY + Duration::from_millis(300), &mut call)
        .await
        .is_ok();
    descendant.reap_killed().await;
    let result = if early {
        None
    } else {
        tokio::time::timeout(BOUND, call)
            .await
            .ok()
            .map(|joined| joined.expect("joined"))
    };
    assert!(!early, "the call returned while its group was alive");
    let result = result.expect("the call returns once the group is gone");
    assert!(result.timed_out && result.killed);
}

// A dropped call's tree is settled under its owner: that owner's close
// waits for it, while another owner, which never ran a process, closes at
// once (the process-wide drain the review found waited for both).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_call_is_settled_by_its_own_owner_only() {
    let _serial = SERIAL.lock().await;
    become_subreaper();
    let dir = TempDir::new();
    let (fifo, mut reader) = dir.fifo();
    let owner = WorkGate::default();
    let other = WorkGate::default();
    let call = exec(
        holding_script(&fifo),
        dir.0.clone(),
        ExecOptions {
            owner: Some(owner.clone()),
            ..ExecOptions::default()
        },
    );
    let mut descendant = Descendant::read(&mut reader).await;
    call.abort();
    assert!(
        tokio::time::timeout(BOUND, call)
            .await
            .expect("the call is dropped")
            .is_err_and(|error| error.is_cancelled())
    );
    let unrelated = tokio::time::timeout(EARLY, other.close()).await.is_ok();
    let mut drained = tokio::spawn(owner.close());
    let early = tokio::time::timeout(EARLY, &mut drained).await.is_ok();
    descendant.reap_killed().await;
    let drained = early || tokio::time::timeout(BOUND, drained).await.is_ok();
    assert!(unrelated, "an unrelated owner waited for this call's tree");
    assert!(
        !early,
        "the owner closed while the dropped call's group was alive"
    );
    assert!(drained, "the owner closes once the group is gone");
}

// A closing owner refuses a call: nothing is spawned.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closing_owner_refuses_the_call() {
    let _serial = SERIAL.lock().await;
    let dir = TempDir::new();
    let owner = WorkGate::default();
    tokio::time::timeout(BOUND, owner.close())
        .await
        .expect("an idle owner closes");
    let marker = dir.0.join("ran");
    let result = exec(
        vec!["-c".into(), format!("touch '{}'", marker.display()).into()],
        dir.0.clone(),
        ExecOptions {
            owner: Some(owner),
            ..ExecOptions::default()
        },
    )
    .await
    .expect("joined");
    assert_eq!(result.code, 1);
    assert_eq!(result.error, Some(std::io::ErrorKind::Other));
    assert!(!marker.exists(), "a refused call ran");
}

// A descendant that left the group (`setsid`) is not owned: an aborted call
// returns without it, after the post-exit grace closes the pipe it holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_escaped_descendant_is_not_awaited() {
    let _serial = SERIAL.lock().await;
    become_subreaper();
    let dir = TempDir::new();
    let (fifo, mut reader) = dir.fifo();
    let abort = AbortController::new();
    // The escaped process reports itself once it has left the group.
    let call = exec(
        vec![
            "-c".into(),
            format!(
                "setsid sh -c 'echo $$ > \"$0\"; exec sleep 1000' '{}' & exec sleep 1000",
                fifo.display()
            )
            .into(),
        ],
        dir.0.clone(),
        ExecOptions {
            signal: Some(abort.signal()),
            ..ExecOptions::default()
        },
    );
    let mut escaped = Descendant::read(&mut reader).await;
    abort.abort();
    let result = tokio::time::timeout(BOUND, call)
        .await
        .expect("the call returns without the escaped descendant")
        .expect("joined");
    let survived = !signalled_or_gone(escaped.pid);
    // SAFETY: plain syscall; the pid is still this process's to reap.
    unsafe {
        libc::kill(escaped.pid, libc::SIGKILL);
    }
    escaped.reap_killed().await;
    assert!(result.aborted);
    assert!(survived, "the escaped descendant was killed with the group");
}

// A normal exit leaves its background descendant running, as Pi does: the
// call returns with its normal result, and a later emergency kill does not
// reach the descendant, whose owner has disarmed and untracked the tree.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_normal_exit_leaves_its_background_running_and_never_signals_it() {
    let _serial = SERIAL.lock().await;
    become_subreaper();
    let dir = TempDir::new();
    let (fifo, mut reader) = dir.fifo();
    let owner = WorkGate::default();
    let result = exec(
        vec![
            "-c".into(),
            format!(
                "sleep 1000 >/dev/null 2>&1 & echo $! > '{}'; echo done",
                fifo.display()
            )
            .into(),
        ],
        dir.0.clone(),
        ExecOptions {
            owner: Some(owner.clone()),
            ..ExecOptions::default()
        },
    );
    let mut descendant = Descendant::read(&mut reader).await;
    let result = tokio::time::timeout(BOUND, result)
        .await
        .expect("the call returns")
        .expect("joined");
    // Every check is synchronous with the signals it rules out: a SIGKILL
    // sent before it is pending or has killed (`signalled_or_gone`).
    let running_after_return = !signalled_or_gone(descendant.pid);
    kill_tracked_detached_children();
    let running_after_emergency = !signalled_or_gone(descendant.pid);
    let closed = tokio::time::timeout(BOUND, owner.close()).await.is_ok();
    let running_after_close = !signalled_or_gone(descendant.pid);
    // SAFETY: plain syscall on a pid this test owns (an orphan of this
    // subreaper, unreaped).
    unsafe {
        libc::kill(descendant.pid, libc::SIGKILL);
    }
    descendant.reap_killed().await;
    assert_eq!((result.code, result.stdout.as_str()), (0, "done\n"));
    assert!(!result.killed && !result.aborted && !result.timed_out);
    assert!(running_after_return, "a normal exit killed its background");
    assert!(
        running_after_emergency,
        "a settled tree was signalled later"
    );
    assert!(closed, "the owner waited for a normal exit's background");
    assert!(
        running_after_close,
        "closing the owner killed the background"
    );
}

// While a tree can still be signalled, its leader is not reaped, so its
// group id cannot be reused: after the shell exits, a descendant keeps the
// output pipe busy (so the call is still reading and the tree armed), and
// the shell must stay a zombie. The emergency kill then ends the call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_armed_tree_keeps_its_leader_unreaped() {
    let _serial = SERIAL.lock().await;
    become_subreaper();
    let dir = TempDir::new();
    let (fifo, mut reader) = dir.fifo();
    let mut call = exec(
        vec![
            "-c".into(),
            format!(
                "(while :; do echo x; sleep 0.01; done) & echo $! > '{0}'; echo $$ > '{0}'; exit 0",
                fifo.display()
            )
            .into(),
        ],
        dir.0.clone(),
        ExecOptions::default(),
    );
    let mut writer = Descendant::read(&mut reader).await;
    let leader = Descendant::read(&mut reader).await;
    // The leader is not this test's child; it is the call's to reap.
    let leader = std::mem::ManuallyDrop::new(leader);
    let deadline = Instant::now() + BOUND;
    let seen = loop {
        match state(leader.pid) {
            Some('Z') => break "zombie",
            None => break "reaped",
            Some(_) => {}
        }
        assert!(Instant::now() < deadline, "the leader never exited");
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    // Still reading the writer's output: the call has not settled.
    let pending = tokio::time::timeout(Duration::ZERO, &mut call)
        .await
        .is_err();
    kill_tracked_detached_children();
    writer.reap_killed().await;
    let ended = tokio::time::timeout(BOUND, call).await.is_ok();
    let reaped_after = state(leader.pid).is_none();
    assert!(pending, "the call settled before the check");
    assert_eq!(seen, "zombie", "the armed tree's leader was reaped");
    assert!(ended, "the emergency kill ends the call");
    assert!(reaped_after, "the leader is reaped once the call settles");
}

/// A session whose model calls `bash` once with `command`, owned by one
/// gate shared by the session and its tool, and then answers "done".
async fn bash_session(dir: &Path, command: String) -> Arc<AgentSession> {
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
    faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_blocks(
            vec![faux_tool_call(
                "bash",
                json!({ "command": command }),
                Some("call-1"),
            )],
            StopReason::ToolUse,
        )),
        FauxResponseStep::Message(faux_assistant_message("done")),
    ]);
    let registry = Arc::new(registry);
    let cwd = dir.to_string_lossy().into_owned();
    let work = WorkGate::default();
    let tool = create_bash_tool(
        dir,
        BashToolOptions {
            spill_directory: Some(dir.to_path_buf()),
            owner: Some(work.clone()),
            ..BashToolOptions::default()
        },
    );
    let session = SessionManager::in_memory(Some(&cwd), NewSessionOptions::default(), Vec::new())
        .expect("session");
    Arc::new(
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
            docs_command: "bake docs".into(),
            plugins: bake_coding_agent::plugins::PluginRegistry::default(),
            work: Some(work),
            jobs: None,
        })
        .await
        .expect("session"),
    )
}

// Shutdown cancels a running bash call by dropping it, and returns only
// once the call's process group is gone: the tool and the session share
// one owner.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_waits_for_a_cancelled_bash_call_group() {
    let _serial = SERIAL.lock().await;
    become_subreaper();
    let dir = TempDir::new();
    let (fifo, mut reader) = dir.fifo();
    let command = format!(
        "sleep 1000 & echo $! > '{}'; exec sleep 1000",
        fifo.display()
    );
    let session = bash_session(&dir.0, command).await;
    let prompt = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.prompt("run it", Vec::new()).await })
    };
    let mut descendant = Descendant::read(&mut reader).await;
    let mut shutdown = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.shutdown().await })
    };
    let early = tokio::time::timeout(EARLY, &mut shutdown).await.is_ok();
    descendant.reap_killed().await;
    let finished = early || tokio::time::timeout(BOUND, shutdown).await.is_ok();
    let _ = tokio::time::timeout(BOUND, prompt).await;
    assert!(!early, "shutdown returned while the call's group was alive");
    assert!(finished, "shutdown finishes once the group is gone");
}

// A process a bash command puts in the background on purpose outlives the
// command's normal exit and the session's normal shutdown, as in Pi: for
// 0.4.0 no exit drain kills it. The shutdown does not wait for it, and
// nothing signals it; the test kills and reaps it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_normal_shutdown_leaves_an_intentionally_backgrounded_process_running() {
    let _serial = SERIAL.lock().await;
    become_subreaper();
    let dir = TempDir::new();
    let (fifo, mut reader) = dir.fifo();
    let command = format!(
        "sleep 1000 </dev/null >/dev/null 2>&1 & echo $! > '{}'; echo started",
        fifo.display()
    );
    let session = bash_session(&dir.0, command).await;
    let prompt = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.prompt("run it", Vec::new()).await })
    };
    let mut descendant = Descendant::read(&mut reader).await;
    let prompted = tokio::time::timeout(BOUND, prompt).await;
    let shut_down = tokio::time::timeout(BOUND, session.shutdown())
        .await
        .is_ok();
    let running_after_shutdown = !signalled_or_gone(descendant.pid);
    // SAFETY: plain syscall on a pid this test owns: an orphan of this
    // subreaper, unreaped.
    unsafe {
        libc::kill(descendant.pid, libc::SIGKILL);
    }
    descendant.reap_killed().await;
    assert!(
        matches!(prompted, Ok(Ok(Ok(())))),
        "the turn ends normally: {prompted:?}"
    );
    assert!(shut_down, "the shutdown does not wait for the background");
    assert!(
        running_after_shutdown,
        "a normal shutdown killed an intentionally backgrounded process"
    );
}

// A call that fails to spawn leaves nothing running, and its owner closes
// at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_spawn_leaves_nothing_to_settle() {
    let _serial = SERIAL.lock().await;
    let dir = TempDir::new();
    let owner = WorkGate::default();
    let result = exec_command(
        "bake-process-reap-no-such-program".as_ref(),
        &[],
        &dir.0,
        ExecOptions {
            owner: Some(owner.clone()),
            ..ExecOptions::default()
        },
    )
    .await;
    assert_eq!(result.code, 1);
    assert_eq!(result.error, Some(std::io::ErrorKind::NotFound));
    tokio::time::timeout(BOUND, owner.close())
        .await
        .expect("nothing is left to settle");
}

/// Set in the copy of this binary that runs as pid 1 of a new PID
/// namespace.
const PID1_CASE: &str = "BAKE_PROCESS_REAP_PID1";

// As pid 1 (a container without an init), the owner reaps the killed
// group's zombies reparented to it, so an aborted call still returns. Runs
// this binary's `pid1_case` as pid 1 of an unprivileged user and PID
// namespace (`unshare -Urpf --mount-proc`); a host without unprivileged
// namespaces fails here rather than skipping.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn as_pid_1_the_owner_reaps_the_groups_orphans() {
    let _serial = SERIAL.lock().await;
    let exe = std::env::current_exe().expect("test binary");
    let mut child = tokio::process::Command::new("unshare")
        .args(["-Urpf", "--mount-proc", "--"])
        .arg(&exe)
        .args(["--exact", "pid1_case", "--ignored", "--nocapture"])
        .env(PID1_CASE, "1")
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("unshare");
    let mut stdout = child.stdout.take().expect("stdout");
    let mut stderr = child.stderr.take().expect("stderr");
    let output = tokio::time::timeout(Duration::from_secs(60), async {
        use tokio::io::AsyncReadExt;
        let (mut out, mut err) = (String::new(), String::new());
        let (_, _, status) = tokio::join!(
            stdout.read_to_string(&mut out),
            stderr.read_to_string(&mut err),
            child.wait()
        );
        (status, out, err)
    })
    .await;
    let (status, out, err) = output.expect("the namespaced case ends");
    let status = status.expect("waited");
    assert!(
        status.success() && out.contains("test pid1_case ... ok"),
        "pid 1 case failed: {status:?}\n{out}\n{err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "run as pid 1 by as_pid_1_the_owner_reaps_the_groups_orphans"]
async fn pid1_case() {
    assert_eq!(std::env::var(PID1_CASE).as_deref(), Ok("1"));
    assert_eq!(std::process::id(), 1, "runs as pid 1");
    let dir = TempDir::new();
    let (fifo, mut reader) = dir.fifo();
    let abort = AbortController::new();
    let owner = WorkGate::default();
    let call = exec(
        holding_script(&fifo),
        dir.0.clone(),
        ExecOptions {
            signal: Some(abort.signal()),
            owner: Some(owner.clone()),
            ..ExecOptions::default()
        },
    );
    let descendant = Descendant::read(&mut reader).await;
    // Reaped by the owner, not by this test.
    let descendant = std::mem::ManuallyDrop::new(descendant);
    abort.abort();
    let result = tokio::time::timeout(BOUND, call)
        .await
        .expect("the call returns: pid 1 reaps the orphaned zombie")
        .expect("joined");
    tokio::time::timeout(BOUND, owner.close())
        .await
        .expect("the owner closes");
    assert!(result.aborted && result.killed);
    assert_eq!(state(descendant.pid), None, "the orphan was reaped");
}

/// Set in the copy of this binary that runs `lost_identity_case`.
const LOST_CASE: &str = "BAKE_PROCESS_REAP_LOST_IDENTITY";

// Review finding: when something other than the owner reaps the leader
// (`waitid` then reports `ECHILD`), its pid and group id may name other
// processes, so the owner must send no signal at all: not to the group,
// not to the leader, and not through Tokio's kill-on-drop. Runs
// `lost_identity_case` as its own process, which reaps its own child.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lost_leader_identity_suppresses_every_signal() {
    let _serial = SERIAL.lock().await;
    let exe = std::env::current_exe().expect("test binary");
    let output = tokio::time::timeout(
        Duration::from_secs(60),
        tokio::process::Command::new(&exe)
            .args(["--exact", "lost_identity_case", "--ignored", "--nocapture"])
            .env(LOST_CASE, "1")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("the case ends")
    .expect("ran");
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && out.contains("test lost_identity_case ... ok"),
        "lost-identity case failed: {:?}\n{out}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

// On one thread, the reap below runs to completion before the owner's
// task can poll again, so the owner first learns of the exit as `ECHILD`.
// The descendant stays in the group, which keeps the group id from being
// reused and makes any group signal visible as a SIGKILL on it.
#[tokio::test(flavor = "current_thread")]
#[ignore = "run in its own process by a_lost_leader_identity_suppresses_every_signal"]
async fn lost_identity_case() {
    assert_eq!(std::env::var(LOST_CASE).as_deref(), Ok("1"));
    become_subreaper();
    let dir = TempDir::new();
    let (fifo, mut reader) = dir.fifo();
    let owner = WorkGate::default();
    let mut call = exec(
        vec![
            "-c".into(),
            format!(
                "sleep 1000 & echo $! > '{0}'; echo $$ > '{0}'; exec sleep 1000",
                fifo.display()
            )
            .into(),
        ],
        dir.0.clone(),
        ExecOptions {
            owner: Some(owner.clone()),
            ..ExecOptions::default()
        },
    );
    let mut descendant = Descendant::read(&mut reader).await;
    let leader = Descendant::read(&mut reader).await;
    // This process's own child: killed and reaped here, without yielding.
    let leader = std::mem::ManuallyDrop::new(leader);
    // SAFETY: plain syscalls on this process's unreaped child.
    let reaped = unsafe {
        libc::kill(leader.pid, libc::SIGKILL);
        libc::waitpid(leader.pid, std::ptr::null_mut(), 0)
    };
    assert_eq!(reaped, leader.pid, "the fixture reaps its own child");
    let result = tokio::time::timeout(BOUND, &mut call)
        .await
        .expect("the call returns")
        .expect("joined");
    let closed = tokio::time::timeout(BOUND, owner.close()).await.is_ok();
    // Any signal the owner sent has been queued by now (see
    // `signalled_or_gone`).
    let untouched = !signalled_or_gone(descendant.pid);
    // SAFETY: plain syscall; the descendant is this subreaper's orphan.
    unsafe {
        libc::kill(descendant.pid, libc::SIGKILL);
    }
    descendant.reap_killed().await;
    assert!(
        result
            .error
            .is_some_and(|kind| kind == std::io::ErrorKind::Other),
        "the lost identity is reported: {result:?}"
    );
    assert!(closed, "the owner closes once the identity is lost");
    assert!(
        untouched,
        "a signal reached the group after its leader's identity was lost"
    );
}
