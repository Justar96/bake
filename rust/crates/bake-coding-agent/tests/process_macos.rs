//! Bake: the macOS counterpart of `process_reap.rs`. A killed tool process
//! is awaited with its process group, whether the call is aborted, times
//! out, or is dropped, with no time limit, and its leader stays an
//! unreaped zombie until then; a dropped call's tree is settled under its
//! owner (a `WorkGate`), whose close waits for it and only for it; session
//! shutdown waits for the trees of the calls it cancels; a normal exit
//! leaves its background descendants running and never signals them later;
//! the leader stays unreaped while its tree can be signalled; a descendant
//! that leaves the group is neither killed nor awaited; and a leader reaped
//! by anything but its owner suppresses every signal.
//!
//! macOS has no child subreaper, so a killed descendant whose shell has
//! died is reparented to launchd and reaped at once: "the group is gone"
//! would be a race the test hopes to win. Each command instead starts a
//! holder in the background, this binary's [`holder_case`]. The holder
//! leaves the command's process group for one of its own, in the same
//! session, and starts `sleep` as its own child back in the command's
//! group: the group's *member*. A killed member stays a zombie, still in
//! the group (`proc_listpids` lists it, which is what the owner's wait
//! asks), until the holder reaps it, and the holder does so only once the
//! test releases it ([`Holder`]). A call that must wait for the group stays
//! pending until then. On release the holder sends the member `SIGUSR1`
//! and reports how it ended: by `SIGKILL`, the owner's group kill reached
//! it first; by `SIGUSR1`, no kill of the owner's had (a `SIGKILL` sent
//! earlier has killed it, or is pending and delivered first). The holder is
//! outside the group, so it is neither killed nor awaited; a watchdog ends
//! it and its member if the test never releases them.
//!
//! The pending checks give a broken implementation [`EARLY`] to finish
//! early; a correct one cannot finish before the release at all, so that
//! window can only make a negative control slower to catch, never this
//! test flaky. Tests run one at a time ([`SERIAL`]), since the emergency
//! kill reaches every tree in the process.
//!
//! `process_reap.rs`'s pid-1 case has no counterpart: macOS has no PID
//! namespace in which this test could run as pid 1.
#![cfg(target_os = "macos")]

use std::ffi::OsString;
use std::fs;
use std::io;
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

/// Bounds waits for states the test already caused.
const BOUND: Duration = Duration::from_secs(30);
/// How long a broken implementation gets to finish early.
const EARLY: Duration = Duration::from_millis(500);
/// How long a holder waits for its release before it ends its member.
const WATCHDOG: Duration = Duration::from_secs(300);
/// Set, to the case's directory, in a holder's environment.
const HOLDER: &str = "BAKE_PROCESS_MACOS_HOLDER";
/// Set in an escaping holder's environment.
const ESCAPE: &str = "BAKE_PROCESS_MACOS_ESCAPE";
/// `PROC_PGRP_ONLY` from `<sys/proc_info.h>`.
const PROC_PGRP_ONLY: u32 = 2;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let mut random = [0; 16];
        getrandom::fill(&mut random).expect("random");
        let id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let dir = std::env::temp_dir().join(format!("bake-process-macos-{id}"));
        fs::create_dir(&dir).expect("temp dir");
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A holder a command started for this case, as it reported itself.
/// Declared after the case's [`TempDir`], so it is dropped first: dropped
/// unreleased, it releases the holder and waits for its report, so a
/// failing case leaves nothing behind.
struct Holder {
    dir: PathBuf,
    /// The member's pid; 0 for an escaping holder, which has none.
    member: libc::pid_t,
    pid: libc::pid_t,
    /// The command's process group, whose id is its leader's pid.
    group: libc::pid_t,
    released: bool,
}

impl Holder {
    /// Shell words that start a holder for `dir` in the background, its
    /// output in `holder.log`.
    fn start(dir: &Path) -> String {
        format!(
            "{HOLDER}='{}' '{}' --exact holder_case --ignored --nocapture >'{}' 2>&1 &",
            dir.display(),
            exe().display(),
            dir.join("holder.log").display()
        )
    }

    /// Shell words that start an escaping holder: it starts a session of
    /// its own, has no member, and keeps the command's standard error open.
    fn start_escaping(dir: &Path) -> String {
        format!(
            "{HOLDER}='{}' {ESCAPE}=1 '{}' --exact holder_case --ignored --nocapture >'{}' &",
            dir.display(),
            exe().display(),
            dir.join("holder.log").display()
        )
    }

    /// Waits for the holder in `dir` to report, then checks that its member
    /// is in the command's group and the holder is not.
    async fn started(dir: &Path) -> Self {
        let report = dir.join("report");
        let deadline = Instant::now() + BOUND;
        let text = loop {
            if let Ok(text) = fs::read_to_string(&report) {
                break text;
            }
            if Instant::now() >= deadline {
                let _ = fs::write(dir.join("release"), b"");
                panic!("the holder never reported\n{}", log(dir));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let pids: Vec<libc::pid_t> = text
            .split_whitespace()
            .filter_map(|pid| pid.parse().ok())
            .collect();
        let [member, pid, group] = pids[..] else {
            let _ = fs::write(dir.join("release"), b"");
            panic!("a malformed holder report: {text:?}");
        };
        let holder = Self {
            dir: dir.to_owned(),
            member,
            pid,
            group,
            released: false,
        };
        let listed = group_members(group);
        assert!(
            member == 0 || listed.contains(&member),
            "the member is not in the command's group: {listed:?}"
        );
        assert!(
            !listed.contains(&pid),
            "the holder did not leave the command's group"
        );
        holder
    }

    fn release(&mut self) {
        if !self.released {
            self.released = true;
            fs::write(self.dir.join("release"), b"").expect("release the holder");
        }
    }

    /// Releases the holder and returns its report once it has exited: how
    /// its member ended (`signal N` or `exit N`), or `alive` from an
    /// escaping holder, which a kill would have ended.
    async fn ended_by(&mut self) -> String {
        self.release();
        let status = self.dir.join("status");
        let deadline = Instant::now() + BOUND;
        let ended = loop {
            if let Ok(ended) = fs::read_to_string(&status) {
                break ended;
            }
            assert!(
                Instant::now() < deadline,
                "the holder never reported\n{}",
                log(&self.dir)
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        // The holder is launchd's child by now (its parent, the leader, has
        // exited), so it is reaped as soon as it exits.
        // SAFETY: signal 0 only checks that the pid exists.
        while unsafe { libc::kill(self.pid, 0) } == 0 {
            assert!(Instant::now() < deadline, "the holder did not exit");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        ended
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        // Only reached when a case failed before its release. The holder
        // ends its member and reports; the directory is kept until then.
        let _ = fs::write(self.dir.join("release"), b"");
        let deadline = Instant::now() + BOUND;
        while !self.dir.join("status").exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn exe() -> PathBuf {
    std::env::current_exe().expect("test binary")
}

fn log(dir: &Path) -> String {
    fs::read_to_string(dir.join("holder.log")).unwrap_or_default()
}

fn signal(number: libc::c_int) -> String {
    format!("signal {number}")
}

/// `sh -c` script: starts a holder for `dir`, then runs `then`.
fn holding(dir: &Path, then: &str) -> Vec<OsString> {
    vec!["-c".into(), format!("{} {then}", Holder::start(dir)).into()]
}

fn exec(
    args: Vec<OsString>,
    cwd: PathBuf,
    options: ExecOptions,
) -> tokio::task::JoinHandle<bake_coding_agent::exec::ExecResult> {
    tokio::spawn(async move { exec_command("sh".as_ref(), &args, &cwd, options).await })
}

/// The pids `proc_listpids` lists in process group `group`, zombies
/// included: the query the owner's wait makes. Empty when it fails.
fn group_members(group: libc::pid_t) -> Vec<libc::pid_t> {
    let mut pids = vec![0 as libc::pid_t; 1024];
    let bytes = libc::c_int::try_from(pids.len() * size_of::<libc::pid_t>()).expect("size");
    let group = u32::try_from(group).expect("a group id");
    // SAFETY: `pids` holds `bytes` bytes of writable space; the call fills
    // at most that and returns the bytes it wrote.
    let written =
        unsafe { libc::proc_listpids(PROC_PGRP_ONLY, group, pids.as_mut_ptr().cast(), bytes) };
    let count = usize::try_from(written).unwrap_or(0) / size_of::<libc::pid_t>();
    pids.truncate(count.min(pids.len()));
    pids.retain(|&pid| pid != 0);
    pids
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leader {
    Running,
    /// Exited and unreaped: a zombie, so its pid and group id are pinned.
    Exited,
    Reaped,
}

/// The state of `pid`, a child of this process, observed without reaping
/// it (`waitid` with `WNOWAIT`, as the owner observes it).
fn leader(pid: libc::pid_t) -> Leader {
    let id = libc::id_t::try_from(pid).expect("a pid");
    loop {
        // SAFETY: a zeroed `siginfo_t` is valid output space.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: plain syscall with a valid out pointer; `WNOWAIT` leaves
        // the child waitable.
        let waited = unsafe {
            libc::waitid(
                libc::P_PID,
                id,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if waited == 0 {
            // SAFETY: `waitid` filled a `SIGCHLD` record or left it zeroed.
            let exited = unsafe { info.si_pid() } != 0;
            return if exited {
                Leader::Exited
            } else {
                Leader::Running
            };
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => {}
            Some(libc::ECHILD) => return Leader::Reaped,
            _ => panic!("waitid: {error}"),
        }
    }
}

/// [`leader`], once the leader has stopped running or [`BOUND`] has
/// passed.
async fn leader_after_exit(pid: libc::pid_t) -> Leader {
    let deadline = Instant::now() + BOUND;
    loop {
        let state = leader(pid);
        if state != Leader::Running || Instant::now() >= deadline {
            return state;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The pid a command wrote, newline-terminated, to `file`.
async fn read_pid(file: &Path) -> libc::pid_t {
    let deadline = Instant::now() + BOUND;
    loop {
        if let Some(pid) = fs::read_to_string(file)
            .ok()
            .filter(|text| text.ends_with('\n'))
            .and_then(|text| text.trim().parse().ok())
        {
            return pid;
        }
        assert!(Instant::now() < deadline, "the command never wrote a pid");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

// An aborted call kills its group and returns only once the member left in
// the group is gone, however long that takes: the window is past the
// five-second limit the review found, after which the source gave up.
// Until then the leader is a zombie, pinning the group's id.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_aborted_exec_waits_for_its_process_group_without_a_limit() {
    let _serial = SERIAL.lock().await;
    let dir = TempDir::new();
    let abort = AbortController::new();
    let mut call = exec(
        holding(&dir.0, "exec sleep 1000"),
        dir.0.clone(),
        ExecOptions {
            signal: Some(abort.signal()),
            ..ExecOptions::default()
        },
    );
    let mut holder = Holder::started(&dir.0).await;
    abort.abort();
    let early = tokio::time::timeout(Duration::from_millis(6500), &mut call)
        .await
        .is_ok();
    let pinned = leader_after_exit(holder.group).await;
    let ended = holder.ended_by().await;
    let result = if early {
        None
    } else {
        tokio::time::timeout(BOUND, call)
            .await
            .ok()
            .map(|joined| joined.expect("joined"))
    };
    let reaped = leader(holder.group);
    assert!(!early, "the call returned while its group was alive");
    assert_eq!(
        pinned,
        Leader::Exited,
        "the leader was not a zombie while its group was alive"
    );
    assert_eq!(
        ended,
        signal(libc::SIGKILL),
        "the group kill reached the member"
    );
    let result = result.expect("the call returns once the group is gone");
    assert!(result.aborted && result.killed);
    assert_eq!(
        reaped,
        Leader::Reaped,
        "the leader is reaped once the call returns"
    );
}

/// The timed-out case's limit. A holder is a fresh process of this binary
/// and must be in place before the timeout kills the group, so it gets
/// this long; nothing the case checks depends on the length.
const TIMEOUT: Duration = Duration::from_secs(5);

// A timed-out call does the same.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_timed_out_exec_waits_for_its_process_group() {
    let _serial = SERIAL.lock().await;
    let dir = TempDir::new();
    let begun = Instant::now();
    let mut call = exec(
        holding(&dir.0, "exec sleep 1000"),
        dir.0.clone(),
        ExecOptions {
            timeout: Some(TIMEOUT),
            ..ExecOptions::default()
        },
    );
    let mut holder = Holder::started(&dir.0).await;
    let in_time = begun.elapsed() < TIMEOUT;
    let early = tokio::time::timeout(TIMEOUT + EARLY, &mut call)
        .await
        .is_ok();
    let pinned = leader_after_exit(holder.group).await;
    let ended = holder.ended_by().await;
    let result = if early {
        None
    } else {
        tokio::time::timeout(BOUND, call)
            .await
            .ok()
            .map(|joined| joined.expect("joined"))
    };
    assert!(in_time, "the holder reported only after the timeout");
    assert!(!early, "the call returned while its group was alive");
    assert_eq!(
        pinned,
        Leader::Exited,
        "the leader was not a zombie while its group was alive"
    );
    assert_eq!(
        ended,
        signal(libc::SIGKILL),
        "the group kill reached the member"
    );
    let result = result.expect("the call returns once the group is gone");
    assert!(result.timed_out && result.killed);
}

// A dropped call's tree is settled under its owner: that owner's close
// waits for it, while another owner, which never ran a process, closes at
// once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_call_is_settled_by_its_own_owner_only() {
    let _serial = SERIAL.lock().await;
    let dir = TempDir::new();
    let owner = WorkGate::default();
    let other = WorkGate::default();
    let call = exec(
        holding(&dir.0, "exec sleep 1000"),
        dir.0.clone(),
        ExecOptions {
            owner: Some(owner.clone()),
            ..ExecOptions::default()
        },
    );
    let mut holder = Holder::started(&dir.0).await;
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
    let pinned = leader_after_exit(holder.group).await;
    let ended = holder.ended_by().await;
    let drained = early || tokio::time::timeout(BOUND, drained).await.is_ok();
    let reaped = leader(holder.group);
    assert!(unrelated, "an unrelated owner waited for this call's tree");
    assert!(
        !early,
        "the owner closed while the dropped call's group was alive"
    );
    assert_eq!(
        pinned,
        Leader::Exited,
        "the settling task let the leader go while its group was alive"
    );
    assert_eq!(
        ended,
        signal(libc::SIGKILL),
        "the drop's kill reached the member"
    );
    assert!(drained, "the owner closes once the group is gone");
    assert_eq!(reaped, Leader::Reaped, "the settling task reaps the leader");
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
    assert_eq!(result.error, Some(io::ErrorKind::Other));
    assert!(!marker.exists(), "a refused call ran");
}

// A descendant that left the group (`setsid`) is not owned: an aborted
// call returns without it, after the post-exit grace closes the pipe it
// holds, and the group kill does not reach it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_escaped_descendant_is_neither_killed_nor_awaited() {
    let _serial = SERIAL.lock().await;
    let dir = TempDir::new();
    let abort = AbortController::new();
    let call = exec(
        vec![
            "-c".into(),
            format!("{} exec sleep 1000", Holder::start_escaping(&dir.0)).into(),
        ],
        dir.0.clone(),
        ExecOptions {
            signal: Some(abort.signal()),
            ..ExecOptions::default()
        },
    );
    let mut escaped = Holder::started(&dir.0).await;
    abort.abort();
    let result = tokio::time::timeout(BOUND, call)
        .await
        .expect("the call returns without the escaped descendant")
        .expect("joined");
    let reaped = leader(escaped.group);
    let ended = escaped.ended_by().await;
    assert!(result.aborted && result.killed);
    assert_eq!(
        reaped,
        Leader::Reaped,
        "the leader is reaped once the call returns"
    );
    assert_eq!(
        ended, "alive",
        "the escaped descendant was killed with the group"
    );
}

// A normal exit leaves its background descendant running, as Pi does: the
// call returns with its normal result, and neither a later emergency kill
// nor the owner's close reaches the member, which is still in the group
// (so a group kill would), since the owner disarmed and untracked the tree.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_normal_exit_leaves_its_background_running_and_never_signals_it() {
    let _serial = SERIAL.lock().await;
    let dir = TempDir::new();
    let owner = WorkGate::default();
    // The command exits only once the member is in its group.
    let call = exec(
        holding(
            &dir.0,
            &format!(
                "while [ ! -e '{}' ]; do sleep 0.01; done; echo done",
                dir.0.join("report").display()
            ),
        ),
        dir.0.clone(),
        ExecOptions {
            owner: Some(owner.clone()),
            ..ExecOptions::default()
        },
    );
    let mut holder = Holder::started(&dir.0).await;
    let result = tokio::time::timeout(BOUND, call)
        .await
        .expect("the call returns")
        .expect("joined");
    let reaped = leader(holder.group);
    let in_group = group_members(holder.group).contains(&holder.member);
    kill_tracked_detached_children();
    let closed = tokio::time::timeout(BOUND, owner.close()).await.is_ok();
    // A kill sent at any point above wins over the release's `SIGUSR1`.
    let ended = holder.ended_by().await;
    assert_eq!((result.code, result.stdout.as_str()), (0, "done\n"));
    assert!(!result.killed && !result.aborted && !result.timed_out);
    assert_eq!(
        reaped,
        Leader::Reaped,
        "a normal exit's leader is reaped when the call returns"
    );
    assert!(in_group, "the member left the command's group");
    assert!(closed, "the owner waited for a normal exit's background");
    assert_eq!(
        ended,
        signal(libc::SIGUSR1),
        "a settled tree was signalled later"
    );
}

// While a tree can still be signalled, its leader is not reaped, so its
// group id cannot be reused: after the shell exits, a descendant keeps the
// output pipe busy (so the call is still reading and the tree armed), and
// the shell must stay a zombie. The emergency kill then ends the call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_armed_tree_keeps_its_leader_unreaped() {
    let _serial = SERIAL.lock().await;
    let dir = TempDir::new();
    let file = dir.0.join("leader");
    let mut call = exec(
        vec![
            "-c".into(),
            format!(
                "(while :; do echo x; sleep 0.01; done) & echo $$ > '{}'; exit 0",
                file.display()
            )
            .into(),
        ],
        dir.0.clone(),
        ExecOptions::default(),
    );
    let pid = read_pid(&file).await;
    let seen = leader_after_exit(pid).await;
    // Still reading the writer's output: the call has not settled.
    let pending = tokio::time::timeout(Duration::ZERO, &mut call)
        .await
        .is_err();
    kill_tracked_detached_children();
    let ended = tokio::time::timeout(BOUND, call).await.is_ok();
    let reaped_after = leader(pid);
    assert!(pending, "the call settled before the check");
    assert_eq!(seen, Leader::Exited, "the armed tree's leader was reaped");
    assert!(ended, "the emergency kill ends the call");
    assert_eq!(
        reaped_after,
        Leader::Reaped,
        "the leader is reaped once the call settles"
    );
}

// Shutdown cancels a running bash call by dropping it, and returns only
// once the call's process group is gone: the tool and the session share
// one owner.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_waits_for_a_cancelled_bash_call_group() {
    let _serial = SERIAL.lock().await;
    let dir = TempDir::new();
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
    let command = format!("{} exec sleep 1000", Holder::start(&dir.0));
    faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_blocks(
            vec![faux_tool_call(
                "bash",
                json!({ "command": command }),
                Some("call-1"),
            )],
            StopReason::ToolUse,
        )),
        FauxResponseStep::Message(faux_assistant_message("never reached")),
    ]);
    let registry = Arc::new(registry);
    let cwd = dir.0.to_string_lossy().into_owned();
    let work = WorkGate::default();
    let tool = create_bash_tool(
        &dir.0,
        BashToolOptions {
            spill_directory: Some(dir.0.clone()),
            owner: Some(work.clone()),
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
            docs_command: "bake docs".into(),
            plugins: bake_coding_agent::plugins::PluginRegistry::default(),
            work: Some(work),
            jobs: None,
        })
        .await
        .expect("session"),
    );
    let prompt = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.prompt("run it", Vec::new()).await })
    };
    let mut holder = Holder::started(&dir.0).await;
    let mut shutdown = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.shutdown().await })
    };
    let early = tokio::time::timeout(EARLY, &mut shutdown).await.is_ok();
    let pinned = leader_after_exit(holder.group).await;
    let ended = holder.ended_by().await;
    let finished = early || tokio::time::timeout(BOUND, shutdown).await.is_ok();
    let _ = tokio::time::timeout(BOUND, prompt).await;
    assert!(!early, "shutdown returned while the call's group was alive");
    assert_eq!(
        pinned,
        Leader::Exited,
        "the leader was not a zombie while its group was alive"
    );
    assert_eq!(
        ended,
        signal(libc::SIGKILL),
        "the cancellation's kill reached the member"
    );
    assert!(finished, "shutdown finishes once the group is gone");
}

// A call that fails to spawn leaves nothing running, and its owner closes
// at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_spawn_leaves_nothing_to_settle() {
    let _serial = SERIAL.lock().await;
    let dir = TempDir::new();
    let owner = WorkGate::default();
    let result = exec_command(
        "bake-process-macos-no-such-program".as_ref(),
        &[],
        &dir.0,
        ExecOptions {
            owner: Some(owner.clone()),
            ..ExecOptions::default()
        },
    )
    .await;
    assert_eq!(result.code, 1);
    assert_eq!(result.error, Some(io::ErrorKind::NotFound));
    tokio::time::timeout(BOUND, owner.close())
        .await
        .expect("nothing is left to settle");
}

/// Set in the copy of this binary that runs `lost_identity_case`.
const LOST_CASE: &str = "BAKE_PROCESS_MACOS_LOST_IDENTITY";

// Review finding: when something other than the owner reaps the leader
// (`waitid` then reports `ECHILD`), its pid and group id may name other
// processes, so the owner must send no signal at all: not to the group,
// not to the leader, and not through Tokio's kill-on-drop. Runs
// `lost_identity_case` as its own process, which reaps its own child.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lost_leader_identity_suppresses_every_signal() {
    let _serial = SERIAL.lock().await;
    let output = tokio::time::timeout(
        Duration::from_secs(60),
        tokio::process::Command::new(exe())
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
// The member stays in the group, which keeps the group id from being
// reused and makes any group signal visible as a `SIGKILL` on it.
#[tokio::test(flavor = "current_thread")]
#[ignore = "run in its own process by a_lost_leader_identity_suppresses_every_signal"]
async fn lost_identity_case() {
    assert_eq!(std::env::var(LOST_CASE).as_deref(), Ok("1"));
    let dir = TempDir::new();
    let owner = WorkGate::default();
    let mut call = exec(
        holding(&dir.0, "exec sleep 1000"),
        dir.0.clone(),
        ExecOptions {
            owner: Some(owner.clone()),
            ..ExecOptions::default()
        },
    );
    let mut holder = Holder::started(&dir.0).await;
    // This process's own child: killed and reaped here, without yielding.
    // SAFETY: plain syscalls on this process's unreaped child.
    let reaped = unsafe {
        libc::kill(holder.group, libc::SIGKILL);
        libc::waitpid(holder.group, std::ptr::null_mut(), 0)
    };
    assert_eq!(reaped, holder.group, "the fixture reaps its own child");
    let result = tokio::time::timeout(BOUND, &mut call)
        .await
        .expect("the call returns")
        .expect("joined");
    let closed = tokio::time::timeout(BOUND, owner.close()).await.is_ok();
    let ended = holder.ended_by().await;
    assert!(
        result
            .error
            .is_some_and(|kind| kind == io::ErrorKind::Other),
        "the lost identity is reported: {result:?}"
    );
    assert!(closed, "the owner closes once the identity is lost");
    assert_eq!(
        ended,
        signal(libc::SIGUSR1),
        "a signal reached the group after its leader's identity was lost"
    );
}

/// Writes `text` to `name` in `dir` all at once: a reader sees all or
/// nothing.
fn publish(dir: &Path, name: &str, text: &str) {
    let partial = dir.join(format!("{name}.partial"));
    fs::write(&partial, text).expect("write a report");
    fs::rename(&partial, dir.join(name)).expect("publish a report");
}

/// Polls `done` until it holds (`true`) or `deadline` passes (`false`).
fn wait_until(deadline: Instant, mut done: impl FnMut() -> bool) -> bool {
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

// The holder (see the module doc and [`Holder`]): started in the
// background by a case's command, never by the harness. It reports
// `<member> <holder> <group>` in `report`, waits for `release`, then sends
// the member `SIGUSR1`, reaps it, and reports how it ended in `status`. An
// escaping holder starts its own session instead, reports `0` as its
// member, and reports `alive` once released.
#[test]
#[ignore = "started by the commands of the cases above"]
fn holder_case() {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::{Command, Stdio};

    let dir = PathBuf::from(std::env::var_os(HOLDER).expect("started by a case's command"));
    let deadline = Instant::now() + WATCHDOG;
    let released = || wait_until(deadline, || dir.join("release").exists());
    // SAFETY: plain syscall.
    let group = unsafe { libc::getpgrp() };
    let me = libc::pid_t::try_from(std::process::id()).expect("a pid");
    if std::env::var_os(ESCAPE).is_some() {
        // SAFETY: plain syscall; this process leads no group, so it can.
        let session = unsafe { libc::setsid() };
        assert_ne!(session, -1, "setsid: {}", io::Error::last_os_error());
        publish(&dir, "report", &format!("0 {me} {group}"));
        assert!(released(), "never released");
        publish(&dir, "status", "alive");
        return;
    }
    // SAFETY: plain syscall: a group of its own, in the same session.
    let left = unsafe { libc::setpgid(0, 0) };
    assert_eq!(left, 0, "setpgid: {}", io::Error::last_os_error());
    let mut member = Command::new("sleep");
    member
        .arg("1000")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: the closure runs in the forked child before `exec` and calls
    // only `setpgid`, which is async-signal-safe, and reads errno.
    unsafe {
        member.pre_exec(move || {
            if libc::setpgid(0, group) == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut member = member.spawn().expect("start the member");
    let pid = libc::pid_t::try_from(member.id()).expect("a pid");
    publish(&dir, "report", &format!("{pid} {me} {group}"));
    if !released() {
        let _ = member.kill();
        let _ = member.wait();
        panic!("never released");
    }
    // SAFETY: plain syscall on this process's unreaped child.
    unsafe {
        libc::kill(pid, libc::SIGUSR1);
    }
    let status = member.wait().expect("reap the member");
    let ended = match (status.signal(), status.code()) {
        (Some(signal), _) => format!("signal {signal}"),
        (None, Some(code)) => format!("exit {code}"),
        (None, None) => format!("{status:?}"),
    };
    publish(&dir, "status", &ended);
}
