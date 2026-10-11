//! Pi's local shell execution: `createLocalShellOperations` and
//! `createLocalBashOperations` from `src/core/tools/bash.ts`, and
//! `waitForChildProcess` from `src/utils/child-process.ts`.
//!
//! One future owns the shell, its pipes, its optional standard-input write,
//! the timeout, and the abort: nothing is spawned beside it. Abort and
//! timeout kill the process tree, then wait as Pi does for the shell to exit
//! and its pipes to settle. Dropping the future before it settles kills the
//! tree; Tokio reaps the killed shell. After the shell exits, Pi keeps
//! reading while output arrives and stops once both pipes close or stay
//! quiet for 100 ms, so a background descendant that holds a pipe open
//! neither hangs the call nor loses output it is still writing
//! (earendil-works/pi#5303). Like Pi, a command that completes normally
//! leaves such descendants running.

use std::ffi::OsString;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use bake_agent::BoxFuture;
use bake_ai::AbortSignal;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::time::Instant;

use super::shell::{CommandTransport, ShellConfig, get_shell_config, get_shell_env};
use super::tree::{ProcessTree, TrackedTree, configure};
use super::{BashExecError, BashExecOptions, BashOperations};
use crate::session::json::js_number_to_string;

/// Pi's `EXIT_STDIO_GRACE_MS`.
pub const EXIT_STDIO_GRACE: Duration = Duration::from_millis(100);
const MAX_TIMEOUT_MS: f64 = 2_147_483_647.0;
const READ_CHUNK: usize = 64 * 1024;

type ResolveShell = Arc<dyn Fn() -> Result<ShellConfig, String> + Send + Sync>;

/// Pi's local shell backend: runs each command in a fresh shell owned by
/// the call. [`BashOperations`] callers can wrap it, for example to
/// refuse or rewrite a spawn.
#[derive(Clone)]
pub struct LocalShellOperations {
    shell_name: String,
    resolve: ResolveShell,
    bin_dir: Option<PathBuf>,
}

impl std::fmt::Debug for LocalShellOperations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalShellOperations")
            .field("shell_name", &self.shell_name)
            .finish_non_exhaustive()
    }
}

impl LocalShellOperations {
    /// Pi's `createLocalShellOperations(shellName, resolveShellConfig)`:
    /// `shell_name` appears in the missing-directory error, and `resolve`
    /// runs for every command.
    pub fn new(
        shell_name: impl Into<String>,
        resolve: impl Fn() -> Result<ShellConfig, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            shell_name: shell_name.into(),
            resolve: Arc::new(resolve),
            bin_dir: None,
        }
    }

    /// The directory [`get_shell_env`] puts first on `PATH` when a call
    /// passes no environment, Pi's `getBinDir()`.
    #[must_use]
    pub fn with_bin_dir(mut self, bin_dir: Option<PathBuf>) -> Self {
        self.bin_dir = bin_dir;
        self
    }
}

/// Pi's `createLocalBashOperations({ shellPath })`.
pub fn create_local_bash_operations(shell_path: Option<PathBuf>) -> LocalShellOperations {
    LocalShellOperations::new("bash", move || get_shell_config(shell_path.as_deref()))
}

impl BashOperations for LocalShellOperations {
    fn exec<'a>(
        &'a self,
        command: String,
        cwd: PathBuf,
        options: BashExecOptions,
    ) -> BoxFuture<'a, Result<Option<i64>, BashExecError>> {
        Box::pin(self.run(command, cwd, options))
    }
}

/// Pi's `resolveTimeoutMs`, in seconds; `Err` holds Pi's message.
pub(super) fn resolve_timeout(timeout: Option<f64>) -> Result<Option<Duration>, String> {
    let Some(timeout) = timeout else {
        return Ok(None);
    };
    if !timeout.is_finite() || timeout <= 0.0 {
        return Err("Invalid timeout: must be a finite number of seconds".to_owned());
    }
    if timeout * 1000.0 > MAX_TIMEOUT_MS {
        return Err(format!(
            "Invalid timeout: maximum is {} seconds",
            js_number_to_string(MAX_TIMEOUT_MS / 1000.0)
        ));
    }
    Ok(Some(Duration::from_secs_f64(timeout)))
}

/// Node's spawn error message, `spawn <file> <code>`, for the codes a shell
/// spawn can meet; other errors keep the OS text.
fn spawn_error(shell: &Path, error: &io::Error) -> String {
    let code = match error.kind() {
        io::ErrorKind::NotFound => "ENOENT".to_owned(),
        io::ErrorKind::PermissionDenied => "EACCES".to_owned(),
        io::ErrorKind::NotADirectory => "ENOTDIR".to_owned(),
        io::ErrorKind::ArgumentListTooLong => "E2BIG".to_owned(),
        io::ErrorKind::OutOfMemory => "ENOMEM".to_owned(),
        _ => error.to_string(),
    };
    format!("spawn {} {code}", shell.display())
}

/// The shell, killed with its tree when dropped before it settles.
struct OwnedShell {
    child: Child,
    tree: Option<TrackedTree>,
    settled: bool,
}

impl OwnedShell {
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

impl Drop for OwnedShell {
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

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

async fn aborted(signal: Option<&AbortSignal>) {
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

/// Pi's exit code: the shell's code, else 128 plus the signal that killed
/// it. Windows codes are unsigned, as Node reports them.
fn exit_code(status: ExitStatus) -> i64 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.code().map_or_else(
            || status.signal().map_or(1, |signal| 128 + i64::from(signal)),
            i64::from,
        )
    }
    #[cfg(windows)]
    {
        status
            .code()
            .map_or(1, |code| i64::from(u32::from_ne_bytes(code.to_ne_bytes())))
    }
    #[cfg(not(any(unix, windows)))]
    {
        status.code().map_or(1, i64::from)
    }
}

impl LocalShellOperations {
    async fn run(
        &self,
        command: String,
        cwd: PathBuf,
        options: BashExecOptions,
    ) -> Result<Option<i64>, BashExecError> {
        let BashExecOptions {
            on_data,
            signal,
            timeout,
            env,
        } = options;
        let timeout_duration = resolve_timeout(timeout).map_err(BashExecError::Failed)?;
        if signal.as_ref().is_some_and(AbortSignal::aborted) {
            return Err(BashExecError::Aborted);
        }
        let config = (self.resolve)().map_err(BashExecError::Failed)?;
        if std::fs::metadata(&cwd).is_err() {
            return Err(BashExecError::Failed(format!(
                "Working directory does not exist: {}\nCannot execute {} commands.",
                cwd.display(),
                self.shell_name
            )));
        }

        let from_stdin = config.command_transport == CommandTransport::Stdin;
        let mut process = Command::new(&config.shell);
        process.args(&config.args);
        if !from_stdin {
            process.arg(&command);
        }
        let env: Vec<(OsString, OsString)> =
            env.unwrap_or_else(|| get_shell_env(self.bin_dir.as_deref()));
        process
            .current_dir(&cwd)
            .env_clear()
            .envs(env)
            .stdin(if from_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        configure(&mut process);
        let child = process
            .spawn()
            .map_err(|error| BashExecError::Failed(spawn_error(&config.shell, &error)))?;
        let tree = ProcessTree::adopt(&child).map(TrackedTree::new);
        let mut shell = OwnedShell {
            child,
            tree,
            settled: false,
        };

        let mut stdin_write: Option<Pin<Box<dyn Future<Output = ()> + Send>>> =
            shell.child.stdin.take().map(|mut stdin| {
                let bytes = command.into_bytes();
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
        let mut timeout_at = timeout_duration.map(|duration| Instant::now() + duration);
        let mut abort_pending = signal.is_some();
        let mut timed_out = false;

        loop {
            if status.is_some() && stdout.is_none() && stderr.is_none() {
                break;
            }
            let exited = status.is_some();
            let abort_signal = signal.as_ref().filter(|_| abort_pending);
            tokio::select! {
                result = wait_child(&mut shell.child, exited) => match result {
                    Ok(exit) => {
                        status = Some(exit);
                        grace = Some(Instant::now() + EXIT_STDIO_GRACE);
                    }
                    Err(error) => return Err(BashExecError::Failed(error.to_string())),
                },
                read = read_some(stdout.as_mut(), &mut out_buffer) => match read {
                    Ok(count) if count > 0 => {
                        on_data(&out_buffer[..count]);
                        if exited {
                            grace = Some(Instant::now() + EXIT_STDIO_GRACE);
                        }
                    }
                    _ => stdout = None,
                },
                read = read_some(stderr.as_mut(), &mut err_buffer) => match read {
                    Ok(count) if count > 0 => {
                        on_data(&err_buffer[..count]);
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
                    shell.kill(exited);
                }
                () = sleep_until(timeout_at) => {
                    timeout_at = None;
                    timed_out = true;
                    shell.kill(exited);
                }
            }
        }
        shell.settled = true;
        drop(shell);

        if signal.as_ref().is_some_and(AbortSignal::aborted) {
            return Err(BashExecError::Aborted);
        }
        if timed_out {
            return Err(BashExecError::TimedOut(timeout.unwrap_or_default()));
        }
        Ok(status.map(exit_code))
    }
}
