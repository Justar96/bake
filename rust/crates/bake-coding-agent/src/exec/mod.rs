//! Pi's `src/core/exec.ts` (v1.1.0; see `NOTICE`): direct command execution
//! with separate stdout and stderr, timeout, and cancellation.
//!
//! This low-level utility performs no authorization or sandbox selection.
//! A caller must authorize and, when required, confine its argv before
//! calling it. No tool or extension calls this module yet.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;
use std::process::ExitStatus;
use std::time::Duration;

use bake_ai::AbortSignal;
use tokio::time::Instant;

use crate::child_process::{self, ProcessSpec, wait_for_process};
use crate::owned_work::WorkGate;

/// Pi's exec options. The working directory is the required `cwd`
/// argument, as in Pi; its unused `options.cwd` member is omitted.
#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    /// Cancellation before spawn prevents execution. Later cancellation
    /// kills the process tree and awaits the immediate child's exit and
    /// then the whole tree's, with no time limit.
    pub signal: Option<AbortSignal>,
    /// Elapsed runtime limit; zero and `None` disable it, as in Pi.
    pub timeout: Option<Duration>,
    /// The owner of the process ([`crate::owned_work::WorkGate`]): a
    /// closing owner refuses the call (an `Other` I/O error, nothing run),
    /// and its close waits for the process tree, the call's own or one a
    /// dropped call left. `None` leaves a dropped call's tree killed but
    /// not awaited.
    pub owner: Option<WorkGate>,
}

/// Pi's exec result, with independent termination facts for Bake callers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecResult {
    /// Unbounded captured output. Each OS read decodes UTF-8 lossily, as
    /// Pi's `Buffer.toString()` does; a split code point can be replaced.
    pub stdout: String,
    /// Captured separately, with the same decoding as stdout.
    pub stderr: String,
    /// Pi's code: the exit code, zero for signal termination, or one when
    /// spawning or waiting fails. Inspect the other facts before success.
    pub code: i64,
    /// Whether this call requested termination, including a pre-abort.
    pub killed: bool,
    /// The timeout fired, independent of the code or cancellation.
    pub timed_out: bool,
    /// Cancellation was requested before this call settled.
    pub aborted: bool,
    /// The Unix signal that ended the child, if any; `None` on Windows.
    pub signal: Option<i32>,
    /// An OS or invalid-timeout failure, separate from an exit code of 1.
    pub error: Option<io::ErrorKind>,
}

/// Execute an argv without a shell, with closed stdin. The child inherits
/// the parent's environment except credential-shaped names (`KEY`,
/// `PASSWORD`, `SECRET`, `TOKEN`, case-insensitive), per Bake's defensive
/// rule and `packages/subprocess/subprocess/src/index.ts`'s
/// `SENSITIVE_ENV_PATTERN`. This heuristic cannot identify every secret.
///
/// Like Pi, output has no memory limit. After exit, pipes close at EOF or
/// after 100 ms without output; each chunk restarts that grace. Normally
/// completed commands may leave descendants running. Cancellation uses
/// Bake's bash process-group/job termination, immediately killing the
/// tree rather than Pi's SIGTERM of only the direct child. Descendants
/// that escape that group/job are outside its ownership.
///
/// To await teardown, abort and then await this future: it returns once
/// the killed child and every process left in its tree are gone
/// ([`bake_process`] has the rules and the exceptions). Dropping it
/// kills the tree; with [`ExecOptions::owner`] the process moves to a task
/// that settles it and the owner's close waits for that, and without one
/// the tree is killed but not awaited. OS failures are returned in the
/// result, as Pi returns a code of 1 instead of rejecting its promise.
pub async fn exec_command(
    command: &OsStr,
    args: &[OsString],
    cwd: &Path,
    options: ExecOptions,
) -> ExecResult {
    let mut result = ExecResult {
        stdout: String::new(),
        stderr: String::new(),
        code: 0,
        killed: false,
        timed_out: false,
        aborted: false,
        signal: None,
        error: None,
    };
    if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
        result.killed = true;
        result.aborted = true;
        return result;
    }
    let deadline = match options.timeout.filter(|duration| !duration.is_zero()) {
        Some(duration) => match Instant::now().checked_add(duration) {
            Some(deadline) => Some(deadline),
            None => {
                result.code = 1;
                result.error = Some(io::ErrorKind::InvalidInput);
                return result;
            }
        },
        None => None,
    };
    let spec = ProcessSpec {
        program: command.to_os_string(),
        args: args.to_vec(),
        cwd: Some(cwd.to_path_buf()),
        env: Some(scrub_environment(std::env::vars_os()).collect()),
        stdin: false,
    };
    let exit = match child_process::spawn(&spec, options.owner.as_ref()).await {
        Ok(child) => {
            wait_for_process(
                child,
                None,
                options.signal.as_ref(),
                deadline,
                |bytes| result.stdout.push_str(&String::from_utf8_lossy(bytes)),
                |bytes| result.stderr.push_str(&String::from_utf8_lossy(bytes)),
            )
            .await
        }
        Err(error) => Err(error),
    };
    result.aborted = options.signal.as_ref().is_some_and(AbortSignal::aborted);
    match exit {
        Ok(exit) => {
            result.code = exit.status.map_or(0, code);
            result.killed = exit.killed;
            result.timed_out = exit.timed_out;
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                result.signal = exit.status.and_then(|status| status.signal());
            }
        }
        Err(error) => {
            result.code = 1;
            result.error = Some(error.kind());
        }
    }
    result
}

fn code(status: ExitStatus) -> i64 {
    #[cfg(windows)]
    {
        status
            .code()
            .map_or(0, |code| i64::from(u32::from_ne_bytes(code.to_ne_bytes())))
    }
    #[cfg(not(windows))]
    {
        status.code().map_or(0, i64::from)
    }
}

fn scrub_environment(
    env: impl IntoIterator<Item = (OsString, OsString)>,
) -> impl Iterator<Item = (OsString, OsString)> {
    env.into_iter().filter(|(name, _)| {
        let name = name.to_string_lossy().to_ascii_uppercase();
        !["KEY", "PASSWORD", "SECRET", "TOKEN"]
            .iter()
            .any(|pattern| name.contains(pattern))
    })
}

#[cfg(test)]
mod tests;
