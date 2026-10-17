//! Pi's local shell execution: `createLocalShellOperations` and
//! `createLocalBashOperations` from `src/core/tools/bash.ts`, and
//! `waitForChildProcess` from `src/utils/child-process.ts`.
//!
//! One future owns the shell, its pipes, its optional standard-input write,
//! the timeout, and the abort: nothing is spawned beside it. Abort and
//! timeout kill the process tree, then wait for the shell to exit, its pipes
//! to settle, and its whole tree to be gone ([`bake_process`]).
//! Dropping the future before it settles kills the tree and hands the shell
//! to its owner, the [`WorkGate`] given by
//! [`LocalShellOperations::with_owner`], whose close then waits for it. After the shell exits, Pi keeps
//! reading while output arrives and stops once both pipes close or stay
//! quiet for 100 ms, so a background descendant that holds a pipe open
//! neither hangs the call nor loses output it is still writing
//! (earendil-works/pi#5303). Like Pi, a command that completes normally
//! leaves such descendants running.
//!
//! [`LocalShellOperations::exec_prepared`] is Bake's seam for confinement:
//! it hands Pi's shell argv to a caller that may wrap it (a sandbox runner)
//! and spawns the result under the same ownership, timeout, and abort.

use std::ffi::OsString;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;

use bake_agent::BoxFuture;
use bake_ai::AbortSignal;
use tokio::time::Instant;

use super::shell::{CommandTransport, ShellConfig, get_shell_config, get_shell_env};
use super::{BashExecError, BashExecOptions, BashOperations};
use crate::child_process::{self, ProcessSpec, aborted, sleep_until, wait_for_process};
use crate::owned_work::WorkGate;
use crate::session::json::js_number_to_string;

/// Pi's `EXIT_STDIO_GRACE_MS`.
pub const EXIT_STDIO_GRACE: Duration = crate::child_process::EXIT_STDIO_GRACE;
const MAX_TIMEOUT_MS: f64 = 2_147_483_647.0;

type ResolveShell = Arc<dyn Fn() -> Result<ShellConfig, String> + Send + Sync>;

/// Pi's local shell backend: runs each command in a fresh shell owned by
/// the call. [`BashOperations`] callers can wrap it, for example to
/// refuse or rewrite a spawn.
#[derive(Clone)]
pub struct LocalShellOperations {
    shell_name: String,
    resolve: ResolveShell,
    bin_dir: Option<PathBuf>,
    owner: Option<WorkGate>,
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
            owner: None,
        }
    }

    /// The directory [`get_shell_env`] puts first on `PATH` when a call
    /// passes no environment, Pi's `getBinDir()`.
    #[must_use]
    pub fn with_bin_dir(mut self, bin_dir: Option<PathBuf>) -> Self {
        self.bin_dir = bin_dir;
        self
    }

    /// The owner of every shell this spawns: its close refuses later
    /// commands and waits for each running or dropped command's whole
    /// tree. Pass the session's gate ([`crate::agent_session::AgentSessionOptions::work`]).
    /// `None`, the default, leaves a dropped command killed but not
    /// awaited.
    #[must_use]
    pub fn with_owner(mut self, owner: Option<WorkGate>) -> Self {
        self.owner = owner;
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

/// The shell invocation Pi's local backend would spawn for one command:
/// the shell's argv, ending with the command unless the shell reads it
/// from standard input, and those standard-input bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellInvocation {
    /// The shell and its arguments.
    pub argv: Vec<OsString>,
    /// The command, for a shell that reads it from standard input.
    pub stdin: Option<Vec<u8>>,
}

/// What to spawn in place of a [`ShellInvocation`], such as the same argv
/// wrapped by a sandbox runner.
pub struct PreparedSpawn {
    /// The program.
    pub program: OsString,
    /// Its arguments.
    pub args: Vec<OsString>,
    /// Variables set on top of the call's environment.
    pub env: Vec<(OsString, OsString)>,
    /// Written to standard input, which is then closed; `None` gives the
    /// process no standard input.
    pub stdin: Option<Vec<u8>>,
    /// Also receives each standard-error chunk, after `on_data`.
    pub on_stderr: Option<super::BashDataCallback>,
}

impl std::fmt::Debug for PreparedSpawn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedSpawn")
            .field("program", &self.program)
            .field("args", &self.args)
            .finish_non_exhaustive()
    }
}

impl PreparedSpawn {
    /// The invocation itself, unwrapped.
    pub fn unwrapped(invocation: ShellInvocation) -> Self {
        let mut argv = invocation.argv.into_iter();
        Self {
            program: argv.next().unwrap_or_default(),
            args: argv.collect(),
            env: Vec::new(),
            stdin: invocation.stdin,
            on_stderr: None,
        }
    }
}

/// Why [`LocalShellOperations::exec_prepared`] failed.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedExecError {
    /// Pi's error for the call.
    pub error: BashExecError,
    /// The I/O error kind when spawning the prepared program failed.
    pub spawn_error: Option<io::ErrorKind>,
}

impl From<BashExecError> for PreparedExecError {
    fn from(error: BashExecError) -> Self {
        Self {
            error,
            spawn_error: None,
        }
    }
}

impl LocalShellOperations {
    /// The shell invocation for `command`, resolved as each call resolves it.
    pub fn shell_invocation(&self, command: &str) -> Result<ShellInvocation, String> {
        let config = (self.resolve)()?;
        Ok(invocation(&config, command))
    }

    async fn run(
        &self,
        command: String,
        cwd: PathBuf,
        options: BashExecOptions,
    ) -> Result<Option<i64>, BashExecError> {
        self.exec_prepared(command, cwd, options, |invocation| {
            std::future::ready(Ok(PreparedSpawn::unwrapped(invocation)))
        })
        .await
        .map_err(|failure| failure.error)
    }

    /// Runs `command` as [`BashOperations::exec`] does, but spawns what
    /// `prepare` makes of Pi's shell invocation. Pi's checks (timeout,
    /// abort, shell, working directory) run first; `prepare` then races the
    /// abort signal and the call's timeout, which keeps counting through
    /// the spawned process. Dropping `prepare`'s future cancels it.
    pub async fn exec_prepared<F, Fut>(
        &self,
        command: String,
        cwd: PathBuf,
        options: BashExecOptions,
        prepare: F,
    ) -> Result<Option<i64>, PreparedExecError>
    where
        F: FnOnce(ShellInvocation) -> Fut,
        Fut: Future<Output = Result<PreparedSpawn, BashExecError>>,
    {
        let BashExecOptions {
            on_data,
            signal,
            timeout,
            env,
        } = options;
        let timeout_duration = resolve_timeout(timeout).map_err(BashExecError::Failed)?;
        let deadline = timeout_duration.map(|duration| Instant::now() + duration);
        if signal.as_ref().is_some_and(AbortSignal::aborted) {
            return Err(BashExecError::Aborted.into());
        }
        let config = (self.resolve)().map_err(BashExecError::Failed)?;
        if std::fs::metadata(&cwd).is_err() {
            return Err(BashExecError::Failed(format!(
                "Working directory does not exist: {}\nCannot execute {} commands.",
                cwd.display(),
                self.shell_name
            ))
            .into());
        }

        let prepared = {
            let prepare = prepare(invocation(&config, &command));
            tokio::select! {
                prepared = prepare => prepared?,
                () = aborted(signal.as_ref()) => return Err(BashExecError::Aborted.into()),
                () = sleep_until(deadline) => {
                    return Err(BashExecError::TimedOut(timeout.unwrap_or_default()).into());
                }
            }
        };
        let PreparedSpawn {
            program,
            args,
            env: extra_env,
            stdin: stdin_bytes,
            on_stderr,
        } = prepared;
        let mut env: Vec<(OsString, OsString)> =
            env.unwrap_or_else(|| get_shell_env(self.bin_dir.as_deref()));
        env.extend(extra_env);
        let spec = ProcessSpec {
            program: program.clone(),
            args,
            cwd: Some(cwd.clone()),
            env: Some(env),
            stdin: stdin_bytes.is_some(),
        };
        let child = child_process::spawn(&spec, self.owner.as_ref())
            .await
            .map_err(|error| PreparedExecError {
                error: BashExecError::Failed(spawn_error(Path::new(&program), &error)),
                spawn_error: Some(error.kind()),
            })?;
        let exit = wait_for_process(
            child,
            stdin_bytes,
            signal.as_ref(),
            deadline,
            |data| on_data(data),
            |data| {
                on_data(data);
                if let Some(on_stderr) = &on_stderr {
                    on_stderr(data);
                }
            },
        )
        .await
        .map_err(|error| BashExecError::Failed(error.to_string()))?;

        if signal.as_ref().is_some_and(AbortSignal::aborted) {
            return Err(BashExecError::Aborted.into());
        }
        if exit.timed_out {
            return Err(BashExecError::TimedOut(timeout.unwrap_or_default()).into());
        }
        Ok(exit.status.map(exit_code))
    }
}

/// Pi's argv for `command` under `config`.
fn invocation(config: &ShellConfig, command: &str) -> ShellInvocation {
    let from_stdin = config.command_transport == CommandTransport::Stdin;
    let mut argv: Vec<OsString> = vec![config.shell.clone().into_os_string()];
    argv.extend(config.args.iter().map(OsString::from));
    if !from_stdin {
        argv.push(command.into());
    }
    ShellInvocation {
        argv,
        stdin: from_stdin.then(|| command.as_bytes().to_vec()),
    }
}
