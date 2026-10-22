//! Pi's `waitForChildProcess` over Bake's owned process tree, and the
//! coding tools' admission into it. Every child a tool spawns (the bash
//! backend, direct exec, grep, and find) is a [`bake_process::OwnedProcess`],
//! spawned through [`spawn`] under a permit of the tool's owner.
//!
//! [`wait_for_process`] is Pi's loop: exit and pipe closure are
//! independent, and post-exit output restarts the 100 ms idle grace. Its
//! policies are the tools': a command that exits by itself is detached,
//! leaving its descendants running as Pi does, and an abort, a timeout, or
//! a caller's stop kills the tree and returns once it is gone.
//!
//! **Admission.** A process is spawned under a permit of its owner, a
//! [`WorkGate`] (the session's, given to the tool that spawns it), taken
//! before anything is created, so the owner's close waits for it; a closing
//! owner refuses the spawn and nothing runs. The permit is the process's
//! claim, so a dropped call's process is settled by a task holding it.
//! Without an owner, only the call awaiting the process owns it.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::process::ExitStatus;
use std::time::Duration;

use bake_ai::AbortSignal;
use bake_process::Claim;
pub(crate) use bake_process::{OwnedProcess, ProcessSpec, TreeState};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;

use crate::owned_work::{Refused, WorkGate};

pub(crate) const EXIT_STDIO_GRACE: Duration = Duration::from_millis(100);
const READ_CHUNK: usize = 64 * 1024;

pub(crate) struct ProcessExit {
    pub status: Option<ExitStatus>,
    pub timed_out: bool,
    pub killed: bool,
    /// What became of the tree: left running after a normal exit, or
    /// killed and gone.
    #[allow(
        dead_code,
        reason = "read by tests; the tools' results do not report it"
    )]
    pub tree: TreeState,
}

/// Spawns `spec` under a permit of `owner`, taken before anything is
/// created. Refused, with nothing spawned, once the owner is closing or
/// outside a Tokio runtime.
pub(crate) async fn spawn(
    spec: &ProcessSpec,
    owner: Option<&WorkGate>,
) -> io::Result<OwnedProcess> {
    if tokio::runtime::Handle::try_current().is_err() {
        return Err(io::Error::other(Refused::NoRuntime));
    }
    let claim = match owner {
        Some(gate) => Some(Box::new(
            gate.enter()
                .ok_or_else(|| io::Error::other(Refused::Closed))?,
        ) as Claim),
        None => None,
    };
    OwnedProcess::spawn(spec, claim).await
}

async fn read_some<R: AsyncRead + Unpin>(
    reader: Option<&mut R>,
    buffer: &mut [u8],
) -> io::Result<usize> {
    match reader {
        Some(reader) => reader.read(buffer).await,
        None => std::future::pending().await,
    }
}

pub(crate) async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

pub(crate) async fn aborted(signal: Option<&AbortSignal>) {
    match signal {
        Some(signal) => signal.cancelled().await,
        None => std::future::pending().await,
    }
}

async fn poll_some(task: Option<&mut Pin<Box<dyn Future<Output = ()> + Send>>>) {
    match task {
        Some(task) => task.await,
        None => std::future::pending().await,
    }
}

async fn wait_exit(process: &mut OwnedProcess, exited: bool) -> io::Result<()> {
    if exited {
        std::future::pending().await
    } else {
        process.wait_exit().await
    }
}

/// Own a spawned child, its optional input, and both output streams.
/// Callbacks run inline and cannot outlive the call. Like Pi, a normal
/// exit leaves descendants running; idle inherited pipes are closed. After
/// an abort or the deadline, the child is killed with its tree and the
/// call returns once the child and its tree are gone
/// ([`OwnedProcess::settle`]). Dropping the future drops the process,
/// which hands it to its owner.
pub(crate) async fn wait_for_process(
    mut shell: OwnedProcess,
    stdin_bytes: Option<Vec<u8>>,
    signal: Option<&AbortSignal>,
    deadline: Option<Instant>,
    mut on_stdout: impl FnMut(&[u8]),
    mut on_stderr: impl FnMut(&[u8]),
) -> io::Result<ProcessExit> {
    let mut stdin_write: Option<Pin<Box<dyn Future<Output = ()> + Send>>> =
        shell.take_stdin().map(|mut stdin| {
            let bytes = stdin_bytes.unwrap_or_default();
            Box::pin(async move {
                // Pi ignores standard-input errors; the shell may exit
                // without reading.
                let _ = stdin.write_all(&bytes).await;
                let _ = stdin.shutdown().await;
            }) as Pin<Box<dyn Future<Output = ()> + Send>>
        });
    let mut stdout = shell.take_stdout();
    let mut stderr = shell.take_stderr();
    let mut out_buffer = vec![0; READ_CHUNK];
    let mut err_buffer = vec![0; READ_CHUNK];
    let mut exited = false;
    let mut grace: Option<Instant> = None;
    let mut timeout_at = deadline;
    let mut abort_pending = signal.is_some();
    let mut timed_out = false;
    let mut killed = false;

    loop {
        if exited && stdout.is_none() && stderr.is_none() {
            break;
        }
        let abort_signal = signal.filter(|_| abort_pending);
        tokio::select! {
            result = wait_exit(&mut shell, exited) => {
                result?;
                exited = true;
                grace = Some(Instant::now() + EXIT_STDIO_GRACE);
            }
            read = read_some(stdout.as_mut(), &mut out_buffer) => match read {
                Ok(count) if count > 0 => {
                    on_stdout(&out_buffer[..count]);
                    if exited {
                        grace = Some(Instant::now() + EXIT_STDIO_GRACE);
                    }
                }
                _ => stdout = None,
            },
            read = read_some(stderr.as_mut(), &mut err_buffer) => match read {
                Ok(count) if count > 0 => {
                    on_stderr(&err_buffer[..count]);
                    if exited {
                        grace = Some(Instant::now() + EXIT_STDIO_GRACE);
                    }
                }
                _ => stderr = None,
            },
            () = poll_some(stdin_write.as_mut()) => stdin_write = None,
            () = sleep_until(grace) => break,
            () = aborted(abort_signal) => {
                abort_pending = false;
                killed = true;
                shell.kill();
            }
            () = sleep_until(timeout_at) => {
                timeout_at = None;
                timed_out = true;
                killed = true;
                shell.kill();
            }
        }
    }
    // Our ends of the pipes close before the tree is awaited: a process
    // that left the tree may hold the other ends.
    drop((stdout, stderr, stdin_write));
    let settled = shell.settle().await?;

    Ok(ProcessExit {
        status: Some(settled.status),
        timed_out,
        killed,
        tree: settled.tree,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    const BOUND: Duration = Duration::from_secs(10);

    // A closing owner refuses the spawn, and nothing runs.
    #[tokio::test]
    async fn a_closing_owner_refuses_the_spawn() {
        let gate = WorkGate::default();
        let closed = gate.close();
        let spec = ProcessSpec {
            program: "sh".into(),
            args: vec!["-c".into(), "exit 0".into()],
            cwd: None,
            env: None,
            stdin: false,
        };
        let refused = spawn(&spec, Some(&gate)).await;
        tokio::time::timeout(BOUND, closed).await.expect("closed");
        let error = refused.expect_err("refused");
        assert_eq!(error.to_string(), Refused::Closed.to_string());
    }
}
