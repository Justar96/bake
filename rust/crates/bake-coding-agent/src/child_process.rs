//! Pi's `waitForChildProcess` over Bake's owned process tree. The bash
//! backend and direct exec use this one loop: exit and pipe closure are
//! independent, and post-exit output restarts the 100 ms idle grace.
//! Cancellation kills the tree; the future awaits the immediate child.
//! Drop kills without waiting, so callers needing awaited teardown must
//! signal cancellation and await the future instead of dropping it.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::process::ExitStatus;
use std::time::Duration;

use bake_ai::AbortSignal;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Child;
use tokio::time::Instant;

use crate::tools::bash::tree::{ProcessTree, TrackedTree};

pub(crate) const EXIT_STDIO_GRACE: Duration = Duration::from_millis(100);
const READ_CHUNK: usize = 64 * 1024;

pub(crate) struct ProcessExit {
    pub status: Option<ExitStatus>,
    pub timed_out: bool,
    pub killed: bool,
}

/// The child, killed with its tree when dropped before it settles.
struct OwnedChild {
    child: Child,
    tree: Option<TrackedTree>,
    settled: bool,
}

impl OwnedChild {
    fn kill(&mut self, exited: bool) {
        if let Some(tree) = &self.tree {
            tree.kill();
        }
        // Pi falls back to the shell's own pid. Tokio refuses to signal a
        // shell it has reaped, so this cannot reach a reused pid.
        if !exited {
            let _ = self.child.start_kill();
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.settled {
            self.kill(false);
        }
        // Dropping the guard untracks the tree.
        self.tree = None;
    }
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

async fn wait_child(child: &mut Child, exited: bool) -> io::Result<ExitStatus> {
    if exited {
        std::future::pending().await
    } else {
        child.wait().await
    }
}

/// Own a just-spawned child, its optional input, and both output streams.
/// Callbacks run inline and cannot outlive the call. Like Pi, a normal
/// exit leaves descendants running; idle inherited pipes are closed.
pub(crate) async fn wait_for_process(
    child: Child,
    stdin_bytes: Option<Vec<u8>>,
    signal: Option<&AbortSignal>,
    deadline: Option<Instant>,
    mut on_stdout: impl FnMut(&[u8]),
    mut on_stderr: impl FnMut(&[u8]),
) -> io::Result<ProcessExit> {
    let tree = ProcessTree::adopt(&child).map(TrackedTree::new);
    let mut shell = OwnedChild {
        child,
        tree,
        settled: false,
    };

    let mut stdin_write: Option<Pin<Box<dyn Future<Output = ()> + Send>>> =
        shell.child.stdin.take().map(|mut stdin| {
            let bytes = stdin_bytes.unwrap_or_default();
            Box::pin(async move {
                // Pi ignores standard-input errors; the shell may exit
                // without reading.
                let _ = stdin.write_all(&bytes).await;
                let _ = stdin.shutdown().await;
            }) as Pin<Box<dyn Future<Output = ()> + Send>>
        });
    let mut stdout = shell.child.stdout.take();
    let mut stderr = shell.child.stderr.take();
    let mut out_buffer = vec![0; READ_CHUNK];
    let mut err_buffer = vec![0; READ_CHUNK];
    let mut status: Option<ExitStatus> = None;
    let mut grace: Option<Instant> = None;
    let mut timeout_at = deadline;
    let mut abort_pending = signal.is_some();
    let mut timed_out = false;
    let mut killed = false;

    loop {
        if status.is_some() && stdout.is_none() && stderr.is_none() {
            break;
        }
        let exited = status.is_some();
        let abort_signal = signal.filter(|_| abort_pending);
        tokio::select! {
            result = wait_child(&mut shell.child, exited) => match result {
                Ok(exit) => {
                    status = Some(exit);
                    grace = Some(Instant::now() + EXIT_STDIO_GRACE);
                }
                Err(error) => return Err(error),
            },
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
                shell.kill(exited);
            }
            () = sleep_until(timeout_at) => {
                timeout_at = None;
                timed_out = true;
                killed = true;
                shell.kill(exited);
            }
        }
    }
    shell.settled = true;
    drop(shell);

    Ok(ProcessExit {
        status,
        timed_out,
        killed,
    })
}
