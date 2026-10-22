//! A killed tree's PID namespace, on real processes: the init of a
//! namespace a member started, which leads its own session outside the
//! group, is killed with the tree and awaited, so no process of the
//! namespace still runs once the owner returns, also when a `SIGTERM`
//! request ended the init's parent before the kill. The namespaces come
//! from `unshare`, whose init has no `--die-with-parent`: only the owner's
//! own kill can end it in time. As pid 1, the owner also reaps an init that
//! exited while another process's child once it is reparented here.
//! `tests/enforcement.rs` in `bake-sandbox` proves the kill, and the
//! request before it, through bubblewrap.
//!
//! Needs unprivileged user namespaces and `unshare`; without them the case
//! prints why and skips, unless `BAKE_REQUIRE_SANDBOX_BACKENDS` names
//! `bwrap`, which needs the same kernel support.
#![cfg(target_os = "linux")]

use std::time::Duration;

use bake_process::{OwnedProcess, ProcessSpec, TreeState};
use tokio::io::AsyncBufReadExt;

/// Bounds each wait, below cargo-mutants' 20 s test timeout, so an owner
/// that never returns fails the case rather than hanging it.
const BOUND: Duration = Duration::from_secs(10);

/// Whether this run must have user namespaces.
fn required() -> bool {
    std::env::var("BAKE_REQUIRE_SANDBOX_BACKENDS")
        .is_ok_and(|list| list.split(',').map(str::trim).any(|name| name == "bwrap"))
}

/// `ppid`, process group, state, and start time of `pid`.
fn stat(pid: i32) -> Option<(i32, i32, u8, u64)> {
    let stat = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
    let close = stat.iter().rposition(|&byte| byte == b')')?;
    let fields: Vec<&[u8]> = stat[close + 1..]
        .split(u8::is_ascii_whitespace)
        .filter(|field| !field.is_empty())
        .collect();
    let number = |index: usize| -> Option<u64> {
        std::str::from_utf8(fields.get(index)?).ok()?.parse().ok()
    };
    // After the name: state (0), ppid (1), pgrp (2), ..., starttime (19).
    Some((
        i32::try_from(number(1)?).ok()?,
        i32::try_from(number(2)?).ok()?,
        *fields.first()?.first()?,
        number(19)?,
    ))
}

fn pids() -> Vec<i32> {
    std::fs::read_dir("/proc")
        .expect("/proc")
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
        .collect()
}

fn is_nested_init(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/status")).is_ok_and(|status| {
        status.lines().any(|line| {
            line.strip_prefix("NSpid:").is_some_and(|levels| {
                let levels: Vec<&str> = levels.split_whitespace().collect();
                levels.len() > 1 && levels.last() == Some(&"1")
            })
        })
    })
}

/// The init and every descendant of it now, each with its start time.
fn namespace(init: i32) -> Vec<(i32, u64)> {
    let table: Vec<(i32, i32, u64)> = pids()
        .into_iter()
        .filter_map(|pid| stat(pid).map(|(ppid, _, _, start)| (pid, ppid, start)))
        .collect();
    let mut members: Vec<(i32, u64)> = table
        .iter()
        .filter(|(pid, _, _)| *pid == init)
        .map(|&(pid, _, start)| (pid, start))
        .collect();
    let mut index = 0;
    while let Some(&(parent, _)) = members.get(index) {
        members.extend(
            table
                .iter()
                .filter(|(_, ppid, _)| *ppid == parent)
                .map(|&(pid, _, start)| (pid, start)),
        );
        index += 1;
    }
    members
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_killed_tree_ends_and_awaits_a_pid_namespace_outside_its_group() {
    let spec = ProcessSpec {
        program: "unshare".into(),
        args: [
            "-Urpf",
            "setsid",
            "sh",
            "-c",
            "sleep 1000 & sleep 1000 & (sleep 1000; :) & echo ready; wait",
        ]
        .map(Into::into)
        .to_vec(),
        cwd: None,
        env: None,
        stdin: false,
    };
    let mut process = match OwnedProcess::spawn(&spec, None).await {
        Ok(process) => process,
        Err(error) => {
            assert!(!required(), "unshare is required: {error}");
            eprintln!("note: skipping: unshare could not start: {error}");
            return;
        }
    };
    let leader = i32::try_from(process.pid()).expect("pid");
    let mut stdout = tokio::io::BufReader::new(process.take_stdout().expect("stdout"));
    let mut line = String::new();
    let _ = tokio::time::timeout(BOUND, stdout.read_line(&mut line)).await;
    if line != "ready\n" {
        let _ = process.settle().await;
        assert!(!required(), "user namespaces are required");
        eprintln!("note: skipping: unprivileged user namespaces are unavailable here");
        return;
    }
    let init = pids()
        .into_iter()
        .find(|&pid| stat(pid).is_some_and(|(ppid, _, _, _)| ppid == leader) && is_nested_init(pid))
        .expect("the namespace init");
    let (_, init_group, _, _) = stat(init).expect("init stat");
    assert_ne!(
        init_group, leader,
        "the init must lead a session of its own"
    );
    let members = namespace(init);
    // The init shell, three sleeps, and a subshell.
    assert!(members.len() >= 5, "{members:?}");
    process.kill();
    let settled = tokio::time::timeout(BOUND, process.settle())
        .await
        .expect("the owner returns")
        .expect("settled");
    assert_eq!(settled.tree, TreeState::Gone);
    let survivors: Vec<(i32, u64)> = members
        .into_iter()
        .filter(|&(pid, start)| {
            stat(pid).is_some_and(|(_, _, state, now)| now == start && state != b'Z')
        })
        .collect();
    if let Some(&(pid, _)) = survivors.first() {
        // Ends the namespace, so a failure leaves nothing behind.
        // SAFETY: plain syscall on a process this test started, its pid
        // and start time just checked.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
    assert!(
        survivors.is_empty(),
        "processes of the killed tree's PID namespace outlived its owner: {survivors:?}"
    );
}

/// Whether `pid` is still listed, running or a zombie.
fn listed(pid: i32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

// Review finding: a `SIGTERM` request that ends the namespace's parent
// must not lose the namespace. The leader is the init's parent and dies of
// the request, which reparents the init out of the tree, so only the pin
// the request took first lets the later kill end the namespace and the
// settle await it, as `bake-mcp` stops a server (terminate, then kill).
// `unshare -Urp` without `-f` makes the leader shell's first child the
// init.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_terminated_tree_still_ends_and_awaits_its_pid_namespace() {
    let spec = ProcessSpec {
        program: "unshare".into(),
        args: [
            "-Urp",
            "sh",
            "-c",
            "setsid sh -c 'sleep 1000 & sleep 1000 & echo ready; wait' & wait",
        ]
        .map(Into::into)
        .to_vec(),
        cwd: None,
        env: None,
        stdin: false,
    };
    let mut process = match OwnedProcess::spawn(&spec, None).await {
        Ok(process) => process,
        Err(error) => {
            assert!(!required(), "unshare is required: {error}");
            eprintln!("note: skipping: unshare could not start: {error}");
            return;
        }
    };
    let leader = i32::try_from(process.pid()).expect("pid");
    let mut stdout = tokio::io::BufReader::new(process.take_stdout().expect("stdout"));
    let mut line = String::new();
    let _ = tokio::time::timeout(BOUND, stdout.read_line(&mut line)).await;
    if line != "ready\n" {
        let _ = process.settle().await;
        assert!(!required(), "user namespaces are required");
        eprintln!("note: skipping: unprivileged user namespaces are unavailable here");
        return;
    }
    let init = pids()
        .into_iter()
        .find(|&pid| stat(pid).is_some_and(|(ppid, _, _, _)| ppid == leader) && is_nested_init(pid))
        .expect("the namespace init");
    let members = namespace(init);
    // The init shell and two sleeps.
    assert!(members.len() >= 3, "{members:?}");
    process.terminate();
    tokio::time::timeout(BOUND, process.wait_exit())
        .await
        .expect("the leader dies of the request")
        .expect("exited");
    let reparented = stat(init).is_some_and(|(ppid, _, _, _)| ppid != leader);
    assert!(reparented, "the request ended the init's parent");
    process.kill();
    let settled = tokio::time::timeout(BOUND, process.settle())
        .await
        .expect("the owner returns")
        .expect("settled");
    assert_eq!(settled.tree, TreeState::Gone);
    let survivors: Vec<(i32, u64)> = members
        .into_iter()
        .filter(|&(pid, start)| {
            stat(pid).is_some_and(|(_, _, state, now)| now == start && state != b'Z')
        })
        .collect();
    if stat(init).is_some_and(|(_, _, state, _)| state != b'Z') {
        // Ends the namespace, so a failure leaves nothing behind.
        // SAFETY: plain syscall on the init this test started, still
        // running, so its pid is not reused.
        unsafe {
            libc::kill(init, libc::SIGKILL);
        }
    }
    assert!(
        survivors.is_empty(),
        "a terminated, then killed, tree left its PID namespace running: {survivors:?}"
    );
}

/// Set in the copy of this binary that runs as pid 1 of a new PID
/// namespace.
const PID1_CASE: &str = "BAKE_PROCESS_PID1_NAMESPACE";
/// Set in the copy of this binary that is the init's parent there.
const PARENT_HELPER: &str = "BAKE_PROCESS_PID1_NAMESPACE_PARENT";

// Review finding: as pid 1 (a container without an init), an exited
// namespace init that is still another process's child must stay pinned
// until it is reaped, here once that parent dies. Runs `pid1_case` as pid 1
// of an unprivileged user and PID namespace (`unshare -Urpf --mount-proc`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn as_pid_1_an_adopted_init_is_reaped_once_reparented() {
    let usable = tokio::process::Command::new("unshare")
        .args(["-Urpf", "--mount-proc", "true"])
        .kill_on_drop(true)
        .output()
        .await
        .is_ok_and(|output| output.status.success());
    if !usable {
        assert!(!required(), "user and PID namespaces are required");
        eprintln!("note: skipping: unprivileged user namespaces are unavailable here");
        return;
    }
    let exe = std::env::current_exe().expect("test binary");
    let output = tokio::time::timeout(
        Duration::from_secs(60),
        tokio::process::Command::new("unshare")
            .args(["-Urpf", "--mount-proc", "--"])
            .arg(&exe)
            .args(["--exact", "pid1_case", "--ignored", "--nocapture"])
            .env(PID1_CASE, "1")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("the namespaced case ends")
    .expect("ran");
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && out.contains("test pid1_case ... ok"),
        "pid 1 case failed: {:?}\n{out}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

// The tree's leader shell starts `pid1_parent_helper` (P), which starts a
// PID namespace whose init (I) leads its own session. The request pins I
// while P is in the group; P then leaves the group, outliving the kill,
// and exits 2 s later without reaping I. So the settle first sees I exited
// while it is P's child (`ECHILD`), and I is reparented here only after.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "run as pid 1 by as_pid_1_an_adopted_init_is_reaped_once_reparented"]
async fn pid1_case() {
    use tokio::io::AsyncWriteExt;

    assert_eq!(std::env::var(PID1_CASE).as_deref(), Ok("1"));
    assert_eq!(std::process::id(), 1, "runs as pid 1");
    let exe = std::env::current_exe().expect("test binary");
    let mut env: Vec<_> = std::env::vars_os().collect();
    env.push((PARENT_HELPER.into(), "1".into()));
    let spec = ProcessSpec {
        program: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            // Through fd 3: dash gives an asynchronous command `/dev/null`
            // as its input before applying `<&0`, so P would read EOF and
            // leave the group at once.
            "exec 3<&0; \"$0\" --exact pid1_parent_helper --ignored --nocapture <&3 3<&- & wait"
                .into(),
            exe.into_os_string(),
        ],
        cwd: None,
        env: Some(env),
        stdin: true,
    };
    let mut process = OwnedProcess::spawn(&spec, None).await.expect("spawned");
    let leader = i32::try_from(process.pid()).expect("pid");
    let mut stdin = process.take_stdin().expect("stdin");
    let mut stdout = tokio::io::BufReader::new(process.take_stdout().expect("stdout"));
    tokio::time::timeout(BOUND, async {
        let mut line = String::new();
        while line != "ready\n" {
            line.clear();
            assert_ne!(stdout.read_line(&mut line).await.expect("read"), 0, "eof");
        }
    })
    .await
    .expect("the init reports");
    let parent = pids()
        .into_iter()
        .find(|&pid| stat(pid).is_some_and(|(ppid, _, _, _)| ppid == leader))
        .expect("the init's parent");
    let init = pids()
        .into_iter()
        .find(|&pid| stat(pid).is_some_and(|(ppid, _, _, _)| ppid == parent) && is_nested_init(pid))
        .expect("the namespace init");
    assert_eq!(
        stat(parent).map(|(_, group, _, _)| group),
        Some(leader),
        "P starts in the group"
    );
    process.terminate();
    stdin.write_all(b"leave\n").await.expect("write");
    let deadline = std::time::Instant::now() + BOUND;
    while stat(parent).is_some_and(|(_, group, _, _)| group != parent) {
        assert!(
            std::time::Instant::now() < deadline,
            "P never left the group"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::timeout(BOUND, process.wait_exit())
        .await
        .expect("the leader dies of the request")
        .expect("exited");
    process.kill();
    let settled = tokio::time::timeout(BOUND, process.settle())
        .await
        .expect("the owner returns")
        .expect("settled");
    assert_eq!(settled.tree, TreeState::Gone);
    // P is this process's child by now (its parent died) and is reaped
    // here once it exits, which may be after a settle that returned early.
    let reaped = tokio::time::timeout(
        BOUND,
        tokio::task::spawn_blocking(move || {
            // SAFETY: plain syscall on P, this process's child.
            unsafe { libc::waitpid(parent, std::ptr::null_mut(), 0) }
        }),
    )
    .await
    .expect("P exits")
    .expect("joined");
    assert_eq!(reaped, parent);
    let leaked = listed(init);
    if leaked {
        // SAFETY: plain syscall on a zombie child of this process.
        unsafe {
            libc::waitpid(init, std::ptr::null_mut(), libc::WNOHANG);
        }
    }
    assert!(
        !leaked,
        "the settled tree left its init {init} a zombie of pid 1"
    );
}

// P of `pid1_case`: ignores the request, starts the namespace, leaves the
// group on its input's first line, and exits 2 s later without reaping
// its init.
#[test]
#[ignore = "started by pid1_case"]
fn pid1_parent_helper() {
    use std::io::BufRead;

    assert_eq!(std::env::var(PARENT_HELPER).as_deref(), Ok("1"));
    // SAFETY: plain syscalls with integer arguments; only this process's
    // disposition and namespace for its children change.
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
        assert_eq!(libc::unshare(libc::CLONE_NEWPID), 0, "unshare");
    }
    // SAFETY: the child calls only async-signal-safe functions and never
    // returns.
    let child = unsafe { libc::fork() };
    if child == 0 {
        let ready = b"ready\n";
        // SAFETY: async-signal-safe syscalls on a valid buffer.
        unsafe {
            libc::setsid();
            libc::write(1, ready.as_ptr().cast(), ready.len());
            loop {
                libc::pause();
            }
        }
    }
    assert!(child > 0, "fork");
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    // SAFETY: plain syscall; this process is not a session leader.
    assert_eq!(unsafe { libc::setpgid(0, 0) }, 0, "setpgid");
    std::thread::sleep(Duration::from_secs(2));
    std::process::exit(0);
}
