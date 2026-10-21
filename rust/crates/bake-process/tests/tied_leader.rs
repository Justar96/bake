//! A leader tied to its spawning thread
//! (`OwnedProcess::spawn_tied_to_thread`), on real processes.
//!
//! A test binary of its own: each case forgets an owner on purpose, so
//! that only the kernel can end its leader, and reaps that leader itself;
//! no other test may see those trees.
#![cfg(target_os = "linux")]

use std::time::Duration;

use bake_process::{OwnedProcess, ProcessSpec};

/// The parent pid of `pid`, or `None` once it is gone (reaped).
fn parent_of(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

fn me() -> i32 {
    i32::try_from(std::process::id()).expect("pid")
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
