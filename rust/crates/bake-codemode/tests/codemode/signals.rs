//! A handled signal that lands on a thread must not end the process.
//!
//! With `Config::macos_use_mach_ports` on, Wasmtime 49 runs a thread that
//! receives Mach exception messages with `MACH_RCV_INTERRUPT` and aborts the
//! process when a signal interrupts the receive (`mach_msg failed with
//! 268451845 (10004005)`, `MACH_RCV_INTERRUPTED`). The bake-coding-agent suite
//! hit it on macOS while codemode scripts ran beside tests that spawn child
//! processes. The engine therefore uses Unix signal handling on every
//! platform, and this test signals every thread of the process directly,
//! which aborted the test binary before that change.

use std::time::Duration;

use serde_json::json;

use crate::harness::{engine, run};

const SCRIPT: &str = "let s = 0; for (let i = 0; i < 2e5; i++) s += i; return s";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_handled_signal_on_any_thread_never_stops_the_engine() {
    engine();
    assert_eq!(run(SCRIPT).await.value(), Some(&json!(19_999_900_000_u64)));
    // A handled signal interrupts the system call of the thread it lands
    // on; an ignored one is discarded.
    let mut usr1 = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())
        .expect("a SIGUSR1 handler");
    let drain = tokio::spawn(async move { while usr1.recv().await.is_some() {} });
    for _ in 0..5 {
        signal_every_thread(libc::SIGUSR1);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(run(SCRIPT).await.value(), Some(&json!(19_999_900_000_u64)));
    drain.abort();
}

/// Sends `signal` to each thread of this process.
#[cfg(target_os = "macos")]
fn signal_every_thread(signal: libc::c_int) {
    let mut threads: libc::thread_act_array_t = std::ptr::null_mut();
    let mut count: libc::mach_msg_type_number_t = 0;
    // SAFETY: `task_threads` fills `threads` with `count` thread ports of
    // this task, which are only read here; the array is released with
    // `vm_deallocate` as its documentation requires.
    unsafe {
        #[allow(
            deprecated,
            reason = "libc's mach_task_self; mach2 is not a dependency"
        )]
        let task = libc::mach_task_self();
        assert_eq!(libc::task_threads(task, &mut threads, &mut count), 0);
        for index in 0..count as usize {
            let thread = libc::pthread_from_mach_thread_np(*threads.add(index));
            if thread != 0 {
                libc::pthread_kill(thread, signal);
            }
        }
        libc::vm_deallocate(
            task,
            threads as libc::vm_address_t,
            count as libc::vm_size_t * std::mem::size_of::<libc::thread_act_t>(),
        );
    }
}

/// Sends `signal` to each thread of this process.
#[cfg(target_os = "linux")]
fn signal_every_thread(signal: libc::c_int) {
    let Ok(entries) = std::fs::read_dir("/proc/self/task") else {
        return;
    };
    // SAFETY: `getpid` and `syscall(SYS_tgkill, ..)` with this process's id
    // and its own thread ids have no memory effects.
    let pid = unsafe { libc::getpid() };
    for entry in entries.flatten() {
        if let Some(tid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<libc::c_int>().ok())
        {
            unsafe { libc::syscall(libc::SYS_tgkill, pid, tid, signal) };
        }
    }
}

/// Elsewhere the process-directed signal is the closest equivalent.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn signal_every_thread(signal: libc::c_int) {
    // SAFETY: `kill` with this process's own id has no memory effects.
    unsafe { libc::kill(libc::getpid(), signal) };
}
