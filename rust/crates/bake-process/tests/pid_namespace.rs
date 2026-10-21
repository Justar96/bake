//! A killed tree's PID namespace, on real processes: the init of a
//! namespace a member started, which leads its own session outside the
//! group, is killed with the tree and awaited, so no process of the
//! namespace still runs once the owner returns. The namespace comes from
//! `unshare -Urpf setsid`, whose init has no `--die-with-parent`: only the
//! owner's own kill can end it in time. `tests/enforcement.rs` in
//! `bake-sandbox` proves the same through bubblewrap.
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
