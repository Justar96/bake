//! One program run to its end under a time limit, as an owned process tree.

use std::io;
use std::time::Duration;

use bake_process::{OwnedProcess, ProcessSpec};
use tokio::io::{AsyncRead, AsyncReadExt};

/// How long the pipes may stay open after the tree was killed.
const PIPE_GRACE: Duration = Duration::from_secs(5);

/// What a finished program left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    /// The exit code; `None` when a signal ended it.
    pub code: Option<i32>,
    /// Whether the time limit ended it.
    pub timed_out: bool,
    /// Standard output, up to the byte limit.
    pub stdout: Vec<u8>,
    /// Standard error, up to the byte limit.
    pub stderr: Vec<u8>,
    /// Whether output past the byte limit was dropped.
    pub truncated: bool,
}

async fn drain(mut pipe: impl AsyncRead + Unpin, limit: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut truncated = false;
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                let room = limit.saturating_sub(kept.len());
                truncated |= count > room;
                kept.extend_from_slice(&chunk[..count.min(room)]);
            }
        }
    }
    (kept, truncated)
}

async fn collect(reader: tokio::task::JoinHandle<(Vec<u8>, bool)>) -> (Vec<u8>, bool) {
    tokio::pin!(reader);
    match tokio::time::timeout(PIPE_GRACE, &mut reader).await {
        Ok(Ok(read)) => read,
        // A process that left the tree still holds the pipe.
        _ => {
            reader.abort();
            (Vec::new(), true)
        }
    }
}

/// Runs `spec` until it exits or `limit` passes, then kills whatever is left
/// of its tree and waits for it, so nothing the program started outlives the
/// call. Each of its output streams is kept up to `max_output` bytes.
pub async fn run(spec: &ProcessSpec, limit: Duration, max_output: usize) -> io::Result<Finished> {
    let mut process = OwnedProcess::spawn(spec, None).await?;
    let stdout = process.take_stdout();
    let stderr = process.take_stderr();
    let out = tokio::spawn(async move {
        match stdout {
            Some(pipe) => drain(pipe, max_output).await,
            None => (Vec::new(), false),
        }
    });
    let err = tokio::spawn(async move {
        match stderr {
            Some(pipe) => drain(pipe, max_output).await,
            None => (Vec::new(), false),
        }
    });
    let timed_out = tokio::select! {
        waited = process.wait_exit() => {
            waited?;
            false
        }
        () = tokio::time::sleep(limit) => true,
    };
    process.kill();
    let settled = process.settle().await?;
    let (stdout, out_truncated) = collect(out).await;
    let (stderr, err_truncated) = collect(err).await;
    Ok(Finished {
        code: settled.status.code(),
        timed_out,
        stdout,
        stderr,
        truncated: out_truncated || err_truncated,
    })
}

/// The last `max` bytes of `bytes` as text, for an error message.
pub fn tail(bytes: &[u8], max: usize) -> String {
    let start = bytes.len().saturating_sub(max);
    String::from_utf8_lossy(bytes.get(start..).unwrap_or_default())
        .trim()
        .to_owned()
}
