//! The Linux orphan drain (`bake_process::subreaper`) and a leader tied to
//! its spawning thread (`OwnedProcess::spawn_tied_to_thread`), on real
//! processes.
//!
//! A test binary of its own: [`subreaper::enable`] is process wide, and the
//! drain kills and reaps every child of this process, so the cases run one
//! at a time ([`SERIAL`]) and nothing else here spawns.
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bake_process::{OwnedProcess, ProcessSpec, TreeState, subreaper};
use tokio::io::AsyncReadExt;

/// Bounds waits for states the test already caused.
const BOUND: Duration = Duration::from_secs(10);

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The parent pid of `pid`, or `None` once it is gone (reaped).
fn parent_of(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// Waits, bounded, until `pid` no longer exists: killed and reaped.
fn assert_gone(pid: i32) {
    let deadline = Instant::now() + BOUND;
    while parent_of(pid).is_some() {
        assert!(Instant::now() < deadline, "process {pid} survived");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn me() -> i32 {
    i32::try_from(std::process::id()).expect("pid")
}

/// Runs `script` under `sh`, reads the pids it prints (one per line, then
/// it exits), and reaps the shell, leaving its background descendants
/// orphaned.
fn orphans(script: &str, count: usize) -> Vec<i32> {
    let mut shell = Command::new("sh")
        .args(["-c", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn sh");
    let mut lines = BufReader::new(shell.stdout.take().expect("stdout")).lines();
    let pids = (0..count)
        .map(|_| {
            lines
                .next()
                .expect("a pid line")
                .expect("read")
                .trim()
                .parse()
                .expect("a pid")
        })
        .collect();
    assert!(shell.wait().expect("reap sh").success());
    pids
}

// With the subreaper enabled, a background process whose shell exited is
// reparented here, and the drain kills and reaps it, and then its own
// child, which is reparented here only when the first is killed.
#[test]
fn the_drain_kills_and_reaps_adopted_descendants() {
    let _serial = serial();
    subreaper::enable().expect("PR_SET_CHILD_SUBREAPER");
    assert!(subreaper::enabled());
    let pids = orphans(
        "sleep 1000 </dev/null >/dev/null 2>&1 & echo $!; \
         sh -c 'sleep 1000 & echo $!; wait' </dev/null 2>&1 & \
         echo $!",
        3,
    );
    // The first line is the outer shell's first sleep; the inner shell
    // and its sleep print in either order.
    let deadline = Instant::now() + BOUND;
    while parent_of(pids[0]) != Some(me()) {
        assert!(Instant::now() < deadline, "the orphan was not adopted");
        std::thread::sleep(Duration::from_millis(5));
    }
    let drained = subreaper::drain_adopted(BOUND);
    assert_eq!(drained.remaining, 0, "{drained:?}");
    assert!(drained.reaped >= 3, "{drained:?}");
    for pid in pids {
        assert_gone(pid);
    }
    // Nothing left: a second drain finds no child.
    assert_eq!(
        subreaper::drain_adopted(Duration::ZERO),
        subreaper::Drained::default()
    );
}

// As a subreaper, a killed tree's group members are reparented here when
// their shell dies, and stay zombies in the group unless reaped: the
// owner's wait reaps them, as it does as pid 1, or it would never end.
#[test]
fn a_killed_tree_settles_while_this_process_is_its_subreaper() {
    let _serial = serial();
    subreaper::enable().expect("PR_SET_CHILD_SUBREAPER");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let spec = ProcessSpec {
            program: "sh".into(),
            args: vec![
                "-c".into(),
                "sleep 1000 & sleep 1000 & echo $!; wait".into(),
            ],
            cwd: None,
            env: None,
            stdin: false,
        };
        let mut process = OwnedProcess::spawn(&spec, None).await.expect("spawn");
        let mut stdout = process.take_stdout().expect("stdout");
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        while byte[0] != b'\n' {
            tokio::time::timeout(BOUND, stdout.read_exact(&mut byte))
                .await
                .expect("the shell reports its child")
                .expect("read");
            line.push(byte[0]);
        }
        let child: i32 = String::from_utf8_lossy(&line)
            .trim()
            .parse()
            .expect("a pid");
        process.kill();
        let settled = tokio::time::timeout(BOUND, process.settle())
            .await
            .expect("a killed tree settles under a subreaper")
            .expect("settled");
        assert_eq!(settled.tree, TreeState::Gone);
        assert_gone(child);
    });
}

/// Spawns `sleep 1000` from a thread of its own, on that thread's runtime,
/// and lets the thread end with the process still owned by nobody: the
/// owner is forgotten, so only the kernel can end the leader.
fn spawn_and_end_the_thread(tied: bool) -> i32 {
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async move {
            let spec = ProcessSpec {
                program: "sleep".into(),
                args: vec!["1000".into()],
                cwd: None,
                env: None,
                stdin: false,
            };
            let process = if tied {
                OwnedProcess::spawn_tied_to_thread(&spec, None).await
            } else {
                OwnedProcess::spawn(&spec, None).await
            }
            .expect("spawn");
            let pid = i32::try_from(process.pid()).expect("pid");
            std::mem::forget(process);
            pid
        })
    })
    .join()
    .expect("the spawning thread")
}

/// Reaps the child `pid`, returning the signal that ended it, if any.
fn reap(pid: i32) -> Option<i32> {
    let mut status = 0;
    // SAFETY: plain syscall on a child of this process; `status` is a
    // valid out pointer.
    let reaped = unsafe { libc::waitpid(pid, &mut status, 0) };
    assert_eq!(reaped, pid, "waitpid");
    libc::WIFSIGNALED(status).then(|| libc::WTERMSIG(status))
}

// A leader tied to its spawning thread is killed by the kernel when that
// thread ends, which is what a killed host's threads do; an untied one,
// the control, outlives the thread.
#[test]
fn a_tied_leader_dies_with_its_spawning_thread() {
    let _serial = serial();
    let tied = spawn_and_end_the_thread(true);
    assert_eq!(reap(tied), Some(libc::SIGKILL));
    let untied = spawn_and_end_the_thread(false);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(parent_of(untied), Some(me()), "the control must still run");
    // SAFETY: plain syscall on an unreaped child of this process.
    unsafe {
        libc::kill(untied, libc::SIGKILL);
    }
    assert_eq!(reap(untied), Some(libc::SIGKILL));
}
