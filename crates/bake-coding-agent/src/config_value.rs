//! Configuration values that may be shell commands, environment templates,
//! or literals.
//!
//! Ported from Pi `packages/coding-agent/src/core/resolve-config-value.ts`
//! (v1.1.0). API keys and header values in `models.json` and `auth.json`
//! use these forms:
//!
//! - `!command` runs `command` in a shell and uses its trimmed output;
//! - `$NAME` and `${NAME}` interpolate environment variables, and a value
//!   with any unset variable resolves to nothing;
//! - `$$` is a literal `$` and `$!` a literal `!`;
//! - anything else is literal.
//!
//! # Deviations from Pi
//!
//! - **Shell.** On Unix the command runs under `/bin/sh -c`, as Node's
//!   `execSync` does. On Windows it runs under `%ComSpec%` (else `cmd.exe`)
//!   with `/d /s /c`, `execSync`'s default; Pi first tries its configured
//!   Git Bash shell, which Bake does not discover.
//! - **Bounds.** Output beyond [`MAX_COMMAND_OUTPUT`] bytes fails the
//!   command, where Node's `execSync` fails beyond its 1 MiB buffer too. The
//!   10-second timeout ([`COMMAND_TIMEOUT`]) bounds the command: the run
//!   from the spawn until the shell has exited and its output has closed.
//!   It does not bound the call. On Unix a timed-out or cancelled call kills
//!   the shell's process group and returns once that killed group is gone,
//!   with no time limit of its own, as a bash tool call does; a member that
//!   cannot die, such as one in uninterruptible sleep on a hung network
//!   mount, holds the call, and with it any teardown waiting for the call.
//! - **Ownership.** On Unix the shell is an owned process tree
//!   ([`bake_process::OwnedProcess`]): it leads its own process group, and
//!   that group is killed, and awaited until no process is left in it, when
//!   the shell exits, when the timeout passes, when the output overflows,
//!   and on cancellation. A `!command` therefore never leaves a process
//!   running in its group: a background process it started, with its
//!   output redirected or not, is killed as soon as the shell exits. This
//!   differs from the bash tool, whose command's background processes
//!   outlive a normal exit as in Pi, and from Pi's `execSync`, which leaves
//!   such a process running and waits only for the output to close. The
//!   output written before the shell exited is kept, and the command
//!   resolves at once instead of failing at the timeout. Only a descendant
//!   that left the group (`setsid`) outlives the command, and it holds the
//!   read, while it keeps the output open, only until the timeout. The tree
//!   runs on a thread of its own, with a runtime of its own, which is joined
//!   before the call returns; on Linux the shell is also tied to that thread
//!   (`PR_SET_PDEATHSIG`), so a host killed outright takes the shell with
//!   it. Windows keeps a plain child: the shell alone is killed and reaped
//!   on timeout or cancellation, and its reader thread is not joined.
//! - **Blocking.** Resolution blocks for as long as a command runs; async
//!   callers run it on a blocking thread, inside [`with_command_cancel`] so
//!   that their cancellation kills the command. A cancelled command
//!   resolves to nothing and is not cached.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

/// Pi's command timeout.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
/// The largest command output read, in bytes.
pub const MAX_COMMAND_OUTPUT: usize = 1024 * 1024;

/// Scoped environment values read before the process environment.
pub type ScopedEnv = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum TemplatePart {
    Literal(String),
    Env(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Reference {
    Command(String),
    Template(Vec<TemplatePart>),
}

fn append_literal(parts: &mut Vec<TemplatePart>, value: &str) {
    if value.is_empty() {
        return;
    }
    if let Some(TemplatePart::Literal(previous)) = parts.last_mut() {
        previous.push_str(value);
        return;
    }
    parts.push(TemplatePart::Literal(value.to_owned()));
}

fn is_name_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_name_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn is_env_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.first().is_some_and(|first| is_name_start(*first))
        && bytes.iter().all(|b| is_name_char(*b))
}

/// Pi's `parseConfigValueTemplate`. Every split point is an ASCII byte, so
/// slicing at them stays on character boundaries.
fn parse_template(config: &str) -> Vec<TemplatePart> {
    let bytes = config.as_bytes();
    let mut parts = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let Some(offset) = config[index..].find('$') else {
            append_literal(&mut parts, &config[index..]);
            break;
        };
        let dollar = index + offset;
        append_literal(&mut parts, &config[index..dollar]);
        match bytes.get(dollar + 1) {
            Some(b'$') => {
                append_literal(&mut parts, "$");
                index = dollar + 2;
            }
            Some(b'!') => {
                append_literal(&mut parts, "!");
                index = dollar + 2;
            }
            Some(b'{') => match config[dollar + 2..].find('}') {
                None => {
                    append_literal(&mut parts, "$");
                    index = dollar + 1;
                }
                Some(length) => {
                    let end = dollar + 2 + length;
                    let name = &config[dollar + 2..end];
                    if is_env_name(name) {
                        parts.push(TemplatePart::Env(name.to_owned()));
                    } else {
                        append_literal(&mut parts, &config[dollar..=end]);
                    }
                    index = end + 1;
                }
            },
            Some(first) if is_name_start(*first) => {
                let end = bytes[dollar + 1..]
                    .iter()
                    .position(|b| !is_name_char(*b))
                    .map_or(bytes.len(), |length| dollar + 1 + length);
                parts.push(TemplatePart::Env(config[dollar + 1..end].to_owned()));
                index = end;
            }
            _ => {
                append_literal(&mut parts, "$");
                index = dollar + 1;
            }
        }
    }
    parts
}

fn parse_reference(config: &str) -> Reference {
    if config.starts_with('!') {
        Reference::Command(config.to_owned())
    } else {
        Reference::Template(parse_template(config))
    }
}

/// Pi's `resolveEnvConfigValue`: the scoped value, then the process
/// environment; empty values count as unset.
fn env_value(name: &str, env: Option<&ScopedEnv>) -> Option<String> {
    env.and_then(|env| env.get(name))
        .filter(|value| !value.is_empty())
        .cloned()
        .or_else(|| std::env::var(name).ok().filter(|value| !value.is_empty()))
}

fn template_names(parts: &[TemplatePart]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for part in parts {
        if let TemplatePart::Env(name) = part
            && !names.contains(name)
        {
            names.push(name.clone());
        }
    }
    names
}

fn resolve_template(parts: &[TemplatePart], env: Option<&ScopedEnv>) -> Option<String> {
    let mut resolved = String::new();
    for part in parts {
        match part {
            TemplatePart::Literal(value) => resolved.push_str(value),
            TemplatePart::Env(name) => resolved.push_str(&env_value(name, env)?),
        }
    }
    Some(resolved)
}

/// Pi's `getConfigValueEnvVarNames`: the variables a template names, in
/// first-use order; none for a command.
pub fn config_value_env_var_names(config: &str) -> Vec<String> {
    match parse_reference(config) {
        Reference::Template(parts) => template_names(&parts),
        Reference::Command(_) => Vec::new(),
    }
}

/// Pi's `getMissingConfigValueEnvVarNames`.
pub fn missing_config_value_env_var_names(config: &str, env: Option<&ScopedEnv>) -> Vec<String> {
    config_value_env_var_names(config)
        .into_iter()
        .filter(|name| env_value(name, env).is_none())
        .collect()
}

/// Pi's `isCommandConfigValue`.
pub fn is_command_config_value(config: &str) -> bool {
    config.starts_with('!')
}

/// Pi's `isConfigValueConfigured`: every variable it names is set.
pub fn is_config_value_configured(config: &str, env: Option<&ScopedEnv>) -> bool {
    missing_config_value_env_var_names(config, env).is_empty()
}

thread_local! {
    static COMMAND_CANCEL: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

/// Runs `work` on this thread so that setting `cancel` kills any command it
/// is running and fails the commands it would start.
pub fn with_command_cancel<R>(cancel: Arc<AtomicBool>, work: impl FnOnce() -> R) -> R {
    struct Restore(Option<Arc<AtomicBool>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            COMMAND_CANCEL.with(|slot| *slot.borrow_mut() = previous);
        }
    }
    let previous = COMMAND_CANCEL.with(|slot| slot.borrow_mut().replace(cancel));
    let _restore = Restore(previous);
    work()
}

fn command_cancelled() -> bool {
    command_cancel().is_some_and(|flag| flag.load(Ordering::SeqCst))
}

/// This thread's [`with_command_cancel`] flag, if any.
fn command_cancel() -> Option<Arc<AtomicBool>> {
    COMMAND_CANCEL.with(|slot| slot.borrow().clone())
}

/// How a command ended.
enum CommandOutcome {
    /// It ran: its output, or `None` on failure.
    Ran(Option<String>),
    /// [`with_command_cancel`]'s flag stopped it.
    Cancelled,
}

impl CommandOutcome {
    fn value(self) -> Option<String> {
        match self {
            Self::Ran(value) => value,
            Self::Cancelled => None,
        }
    }
}

fn command_cache() -> &'static Mutex<HashMap<String, Option<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Pi's `resolveConfigValue`: commands are cached for the process's
/// lifetime, failures included; environment values are read every time.
pub fn resolve_config_value(config: &str, env: Option<&ScopedEnv>) -> Option<String> {
    match parse_reference(config) {
        Reference::Command(command) => {
            if let Some(cached) = command_cache()
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&command)
            {
                return cached.clone();
            }
            let result = match execute_command(&command[1..]) {
                CommandOutcome::Ran(result) => result,
                CommandOutcome::Cancelled => return None,
            };
            command_cache()
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(command, result.clone());
            result
        }
        Reference::Template(parts) => resolve_template(&parts, env),
    }
}

/// Pi's `resolveConfigValueUncached`: a command runs on every call.
pub fn resolve_config_value_uncached(config: &str, env: Option<&ScopedEnv>) -> Option<String> {
    match parse_reference(config) {
        Reference::Command(command) => execute_command(&command[1..]).value(),
        Reference::Template(parts) => resolve_template(&parts, env),
    }
}

/// Pi's `resolveConfigValueOrThrow`: an uncached resolution, or Pi's error
/// text naming the command or the missing variables.
pub fn resolve_config_value_or_err(
    config: &str,
    description: &str,
    env: Option<&ScopedEnv>,
) -> Result<String, String> {
    if let Some(value) = resolve_config_value_uncached(config, env) {
        return Ok(value);
    }
    match parse_reference(config) {
        Reference::Command(command) => Err(format!(
            "Failed to resolve {description} from shell command: {}",
            &command[1..]
        )),
        Reference::Template(_) => {
            let missing = missing_config_value_env_var_names(config, env);
            match missing.as_slice() {
                [one] => Err(format!(
                    "Failed to resolve {description} from environment variable: {one}"
                )),
                [] => Err(format!("Failed to resolve {description}")),
                many => Err(format!(
                    "Failed to resolve {description} from environment variables: {}",
                    many.join(", ")
                )),
            }
        }
    }
}

/// Pi's `resolveHeadersOrThrow`: every header resolved, or the first
/// failure. `None` for no headers.
pub fn resolve_headers_or_err(
    headers: Option<&[(String, String)]>,
    description: &str,
    env: Option<&ScopedEnv>,
) -> Result<Option<Vec<(String, String)>>, String> {
    let Some(headers) = headers else {
        return Ok(None);
    };
    let mut resolved = Vec::with_capacity(headers.len());
    for (key, value) in headers {
        resolved.push((
            key.clone(),
            resolve_config_value_or_err(value, &format!("{description} header \"{key}\""), env)?,
        ));
    }
    Ok((!resolved.is_empty()).then_some(resolved))
}

/// Pi's `clearConfigValueCache`.
pub fn clear_config_value_cache() {
    command_cache()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
}

/// Runs `command` in the shell with Pi's timeout and returns its trimmed
/// output, or `None` when it fails, times out, prints nothing, or prints
/// more than [`MAX_COMMAND_OUTPUT`] bytes. A cancellation kills and reaps
/// it.
fn execute_command(command: &str) -> CommandOutcome {
    if command_cancelled() {
        return CommandOutcome::Cancelled;
    }
    run_command(command, COMMAND_TIMEOUT, command_cancel())
}

/// The outcome of an exit `status` with `output`: Pi's trimmed value, or
/// `None` for a failure, an overflow, or nothing printed.
fn ran(success: bool, output: &[u8]) -> CommandOutcome {
    if !success || output.len() > MAX_COMMAND_OUTPUT {
        return CommandOutcome::Ran(None);
    }
    let text = String::from_utf8_lossy(output);
    let trimmed = text.trim();
    CommandOutcome::Ran((!trimmed.is_empty()).then(|| trimmed.to_owned()))
}

/// Runs `command` under `/bin/sh -c` as an owned process tree, on a thread
/// with a runtime of its own, and joins that thread (see the module docs).
#[cfg(unix)]
fn run_command(
    command: &str,
    timeout: Duration,
    cancel: Option<Arc<AtomicBool>>,
) -> CommandOutcome {
    let spec = bake_process::ProcessSpec {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), command.into()],
        cwd: None,
        env: None,
        stdin: false,
    };
    // Its own thread keeps the call synchronous wherever it is made, a
    // Tokio worker included, and outlives the shell it is tied to.
    let worker = std::thread::Builder::new()
        .name("bake-config-command".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?;
            Some(runtime.block_on(run_owned(spec, timeout, cancel)))
        });
    match worker.map(std::thread::JoinHandle::join) {
        Ok(Ok(Some(outcome))) => outcome,
        _ => CommandOutcome::Ran(None),
    }
}

/// How often a running command checks its cancellation flag.
#[cfg(unix)]
const CANCEL_POLL: Duration = Duration::from_millis(5);

/// [`run_command`]'s body: spawns the shell in its own group, reads its
/// output (and discards its standard error) until it has exited and the
/// output has closed, killing the group as soon as the shell exits, and
/// settles the tree before it returns, whatever ended the run.
#[cfg(unix)]
async fn run_owned(
    spec: bake_process::ProcessSpec,
    timeout: Duration,
    cancel: Option<Arc<AtomicBool>>,
) -> CommandOutcome {
    use tokio::io::AsyncReadExt;

    let deadline = tokio::time::Instant::now() + timeout;
    #[cfg(target_os = "linux")]
    let spawned = bake_process::OwnedProcess::spawn_tied_to_thread(&spec, None).await;
    #[cfg(not(target_os = "linux"))]
    let spawned = bake_process::OwnedProcess::spawn(&spec, None).await;
    let Ok(mut shell) = spawned else {
        return CommandOutcome::Ran(None);
    };
    let mut stdout = shell.take_stdout();
    let mut stderr = shell.take_stderr();
    let mut output = Vec::new();
    let mut chunk = vec![0; 64 * 1024];
    let mut discard = vec![0; 64 * 1024];
    let mut exited = false;
    let mut cancelled = false;
    let mut finished = false;
    loop {
        if exited && stdout.is_none() {
            finished = true;
            break;
        }
        tokio::select! {
            read = async { stdout.as_mut()?.read(&mut chunk).await.ok() },
                if stdout.is_some() =>
            {
                match read {
                    Some(count) if count > 0 => {
                        output.extend_from_slice(&chunk[..count]);
                        if output.len() > MAX_COMMAND_OUTPUT {
                            break;
                        }
                    }
                    _ => stdout = None,
                }
            }
            read = async { stderr.as_mut()?.read(&mut discard).await.ok() },
                if stderr.is_some() =>
            {
                if !matches!(read, Some(count) if count > 0) {
                    stderr = None;
                }
            }
            waited = shell.wait_exit(), if !exited => {
                if waited.is_err() {
                    break;
                }
                exited = true;
                // The shell is done: whatever it left in its group goes now,
                // so the output closes once what was written is read.
                shell.kill();
            }
            () = tokio::time::sleep_until(deadline) => break,
            () = tokio::time::sleep(CANCEL_POLL), if cancel.is_some() => {
                if cancel.as_ref().is_some_and(|flag| flag.load(Ordering::SeqCst)) {
                    cancelled = true;
                    break;
                }
            }
        }
    }
    shell.kill();
    // Our ends close before the tree is awaited: a process that left the
    // group may hold the others.
    drop((stdout, stderr));
    // Unbounded, as the bash tool's wait for a killed tree is: the timeout
    // bounds the command, and the call returns once the group is gone.
    let settled = shell.settle().await;
    if cancelled {
        return CommandOutcome::Cancelled;
    }
    match settled {
        Ok(settled) if finished => ran(settled.status.success(), &output),
        _ => CommandOutcome::Ran(None),
    }
}

#[cfg(windows)]
fn shell_command(command: &str) -> std::process::Command {
    let shell = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
    use std::os::windows::process::CommandExt;

    // Node's `execSync` passes `/d /s /c "<command>"` verbatim
    // (`windowsVerbatimArguments`): `cmd /s` strips the outer quotes and
    // runs the rest as typed. `arg` would escape inner quotes by MSVC
    // rules, which `cmd` does not undo.
    let mut process = std::process::Command::new(shell);
    process
        .args(["/d", "/s", "/c"])
        .raw_arg(format!("\"{command}\""));
    process
}

/// The Windows runner: a plain child whose reader thread is not joined
/// (see the module docs).
#[cfg(windows)]
fn run_command(
    command: &str,
    timeout: Duration,
    cancel: Option<Arc<AtomicBool>>,
) -> CommandOutcome {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::Instant;

    let Ok(mut child) = shell_command(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return CommandOutcome::Ran(None);
    };
    let deadline = Instant::now() + timeout;
    let (sender, receiver) = std::sync::mpsc::channel();
    if let Some(mut stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut output = Vec::new();
            let limit = u64::try_from(MAX_COMMAND_OUTPUT).unwrap_or(u64::MAX) + 1;
            let read = (&mut stdout).take(limit).read_to_end(&mut output);
            // The receiver may have given up; the result is then unused.
            let _ = sender.send(read.ok().map(|_| output));
        });
    } else {
        drop(sender);
    }
    let is_cancelled = || {
        cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::SeqCst))
    };
    let mut cancelled = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if is_cancelled() => {
                cancelled = true;
                break None;
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => break None,
        }
    };
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        return if cancelled {
            CommandOutcome::Cancelled
        } else {
            CommandOutcome::Ran(None)
        };
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    let Some(output) = receiver.recv_timeout(remaining).ok().flatten() else {
        return CommandOutcome::Ran(None);
    };
    ran(status.success(), &output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::time::Instant;

    fn scoped(pairs: &[(&str, &str)]) -> ScopedEnv {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    // Pi `resolve-config-value.test.ts`: "resolves literals, environment
    // templates, and escapes", with the variables scoped instead of set in
    // the process environment, which a parallel test must not mutate.
    #[test]
    fn resolves_literals_templates_and_escapes() {
        let env = scoped(&[
            ("BAKE_TEST_CONFIG_LEFT", "left"),
            ("BAKE_TEST_CONFIG_RIGHT", "right"),
        ]);
        let env = Some(&env);
        assert_eq!(
            resolve_config_value("literal-key", env).as_deref(),
            Some("literal-key")
        );
        assert_eq!(
            resolve_config_value("$BAKE_TEST_CONFIG_LEFT", env).as_deref(),
            Some("left")
        );
        assert_eq!(
            resolve_config_value("${BAKE_TEST_CONFIG_LEFT}_$BAKE_TEST_CONFIG_RIGHT", env)
                .as_deref(),
            Some("left_right")
        );
        assert_eq!(
            resolve_config_value("$$BAKE_TEST_CONFIG_LEFT", env).as_deref(),
            Some("$BAKE_TEST_CONFIG_LEFT")
        );
        assert_eq!(
            resolve_config_value("$!literal-$BAKE_TEST_CONFIG_RIGHT", env).as_deref(),
            Some("!literal-right")
        );
        assert_eq!(
            resolve_config_value("${not a name}x", env).as_deref(),
            Some("${not a name}x")
        );
        assert_eq!(
            resolve_config_value("cost $5 ${", env).as_deref(),
            Some("cost $5 ${")
        );
        assert_eq!(resolve_config_value("é$", env).as_deref(), Some("é$"));
        assert_eq!(resolve_config_value("$BAKE_TEST_CONFIG_UNSET_X", env), None);
    }

    #[test]
    fn names_and_missing_variables() {
        assert_eq!(
            config_value_env_var_names("$A-${B}-$A"),
            vec!["A".to_owned(), "B".to_owned()]
        );
        assert!(config_value_env_var_names("!echo $A").is_empty());
        let env = scoped(&[("BAKE_TEST_PRESENT", "x")]);
        assert!(is_config_value_configured("$BAKE_TEST_PRESENT", Some(&env)));
        assert_eq!(
            resolve_config_value_or_err("$BAKE_TEST_MISSING_1", "API key", Some(&env)),
            Err("Failed to resolve API key from environment variable: BAKE_TEST_MISSING_1".into())
        );
        assert_eq!(
            resolve_config_value_or_err(
                "$BAKE_TEST_MISSING_1$BAKE_TEST_MISSING_2",
                "API key",
                Some(&env)
            ),
            Err("Failed to resolve API key from environment variables: BAKE_TEST_MISSING_1, BAKE_TEST_MISSING_2".into())
        );
    }

    // Pi: "uses credential-scoped environment before process.env". `PATH`
    // is set in every test process, so no variable is mutated.
    #[test]
    fn scoped_environment_comes_first() {
        let env = scoped(&[("PATH", "scoped")]);
        assert_eq!(
            resolve_config_value("$PATH", Some(&env)).as_deref(),
            Some("scoped")
        );
        assert_ne!(
            resolve_config_value("$PATH", None).as_deref(),
            Some("scoped")
        );
    }

    // Pi: "executes shell commands and trims their output" and "returns
    // undefined when command resolution fails".
    #[cfg(unix)]
    #[test]
    fn runs_commands_and_trims_output() {
        assert_eq!(
            resolve_config_value_uncached("!echo '  spaced-key  '", None).as_deref(),
            Some("spaced-key")
        );
        assert_eq!(
            resolve_config_value_uncached("!printf 'line1\\nline2'", None).as_deref(),
            Some("line1\nline2")
        );
        assert_eq!(
            resolve_config_value_uncached("!echo 'hello world' | tr ' ' '-'", None).as_deref(),
            Some("hello-world")
        );
        for failing in ["!exit 1", "!nonexistent-command-12345", "!printf ''"] {
            assert_eq!(
                resolve_config_value_uncached(failing, None),
                None,
                "{failing}"
            );
        }
        assert_eq!(
            resolve_config_value_or_err("!exit 1", "API key", None),
            Err("Failed to resolve API key from shell command: exit 1".into())
        );
    }

    #[cfg(windows)]
    #[test]
    fn runs_commands_under_cmd() {
        assert_eq!(
            resolve_config_value_uncached("!echo spaced-key ", None).as_deref(),
            Some("spaced-key")
        );
        assert_eq!(resolve_config_value_uncached("!exit 1", None), None);
        // Quotes reach `cmd` as typed, as Node's verbatim arguments pass them.
        assert_eq!(
            resolve_config_value_uncached("!echo \"quoted key\"", None).as_deref(),
            Some("\"quoted key\"")
        );
    }

    // Bake: a cancelled command is killed at once, resolves to nothing, and
    // is not cached, so a later resolution runs it again.
    #[cfg(unix)]
    #[test]
    fn cancelled_commands_are_killed_and_not_cached() {
        let cancel = Arc::new(AtomicBool::new(false));
        let setter = {
            let cancel = Arc::clone(&cancel);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                cancel.store(true, Ordering::SeqCst);
            })
        };
        let command = "!sleep 5; echo late-cancel-key";
        let started = Instant::now();
        let value =
            with_command_cancel(Arc::clone(&cancel), || resolve_config_value(command, None));
        let _ = setter.join();
        assert_eq!(value, None);
        assert!(started.elapsed() < Duration::from_secs(2), "killed at once");
        assert!(
            !command_cache()
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains_key(command)
        );
        // Already cancelled: nothing starts.
        let started = Instant::now();
        assert_eq!(
            with_command_cancel(cancel, || resolve_config_value_uncached(command, None)),
            None
        );
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(!command_cancelled(), "the flag is scoped to the call");
    }

    // Pi: "caches successful and failed commands until explicitly cleared"
    // and "uncached resolution executes a command on every call".
    #[cfg(unix)]
    #[test]
    fn caches_commands_until_cleared() {
        let dir = crate::test_support::TempDir::new("config-value");
        let counter = dir.join("counter");
        std::fs::write(&counter, "0").unwrap_or_default();
        let path = counter.display();
        let success = format!("!count=$(cat '{path}'); echo $((count + 1)) > '{path}'; echo value");
        let read = || {
            std::fs::read_to_string(&counter)
                .unwrap_or_default()
                .trim()
                .to_owned()
        };
        assert_eq!(
            resolve_config_value(&success, None).as_deref(),
            Some("value")
        );
        assert_eq!(
            resolve_config_value(&success, None).as_deref(),
            Some("value")
        );
        assert_eq!(read(), "1");
        command_cache()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&success);
        assert_eq!(
            resolve_config_value(&success, None).as_deref(),
            Some("value")
        );
        assert_eq!(read(), "2");
        let failure = format!("!count=$(cat '{path}'); echo $((count + 1)) > '{path}'; exit 1");
        assert_eq!(resolve_config_value(&failure, None), None);
        assert_eq!(resolve_config_value(&failure, None), None);
        assert_eq!(read(), "3");
        assert_eq!(
            resolve_config_value_uncached(&success, None).as_deref(),
            Some("value")
        );
        assert_eq!(
            resolve_config_value_uncached(&success, None).as_deref(),
            Some("value")
        );
        assert_eq!(read(), "5");
    }

    /// Whether `/proc` still lists `pid`, running or a zombie.
    #[cfg(target_os = "linux")]
    fn listed(pid: &str) -> bool {
        std::path::Path::new(&format!("/proc/{}", pid.trim())).exists()
    }

    /// Reads the pid a command wrote to `path`.
    #[cfg(target_os = "linux")]
    fn written_pid(path: &std::path::Path) -> String {
        std::fs::read_to_string(path).expect("the command wrote its pid")
    }

    // Bake: a background process that holds the output is killed and
    // reaped once the shell exits; the value the shell printed resolves at
    // once instead of failing at the timeout.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_background_holder_of_the_output_is_killed_at_completion() {
        let dir = crate::test_support::TempDir::new("config-value-holder");
        let pid = dir.join("pid");
        let command = format!("!sleep 1000 & echo $! > '{}'; echo held-key", pid.display());
        let started = Instant::now();
        assert_eq!(
            resolve_config_value_uncached(&command, None).as_deref(),
            Some("held-key")
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "resolved at once"
        );
        let pid = written_pid(&pid);
        assert!(
            !listed(&pid),
            "the background sleep {pid} outlived the call"
        );
    }

    // Bake: at the timeout the shell and everything in its group, a
    // process holding the output included, are killed and reaped before
    // the call returns, and nothing resolves.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_timed_out_command_and_its_holder_are_killed_and_reaped() {
        let dir = crate::test_support::TempDir::new("config-value-timeout");
        let pids = dir.join("pids");
        let script = format!(
            "sleep 1000 & echo \"$$ $!\" > '{}'; echo partial; exec sleep 1000",
            pids.display()
        );
        let started = Instant::now();
        let outcome = run_command(&script, Duration::from_millis(500), None);
        assert!(matches!(outcome, CommandOutcome::Ran(None)));
        assert!(started.elapsed() < Duration::from_secs(5));
        for pid in written_pid(&pids).split_whitespace() {
            assert!(!listed(pid), "{pid} outlived the timed-out call");
        }
    }

    // Bake: a cancellation kills and reaps the whole group, not only the
    // shell.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_cancelled_command_and_its_descendants_are_killed_and_reaped() {
        let dir = crate::test_support::TempDir::new("config-value-cancel");
        let pids = dir.join("pids");
        let script = format!(
            "sleep 1000 </dev/null >/dev/null 2>&1 & echo \"$$ $!\" > '{0}.tmp'; mv '{0}.tmp' '{0}'; wait",
            pids.display()
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let setter = {
            let cancel = Arc::clone(&cancel);
            let pids = pids.clone();
            std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(10);
                while !pids.exists() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                cancel.store(true, Ordering::SeqCst);
            })
        };
        let outcome = run_command(&script, COMMAND_TIMEOUT, Some(cancel));
        let _ = setter.join();
        assert!(matches!(outcome, CommandOutcome::Cancelled));
        for pid in written_pid(&pids).split_whitespace() {
            assert!(!listed(pid), "{pid} outlived the cancelled call");
        }
    }

    // Bake: a command already cancelled never starts: its side effect
    // does not happen, and nothing is cached.
    #[cfg(unix)]
    #[test]
    fn an_already_cancelled_command_never_starts() {
        let dir = crate::test_support::TempDir::new("config-value-precancel");
        let marker = dir.join("ran");
        let command = format!("!touch '{}'; echo started-key", marker.display());
        let cancel = Arc::new(AtomicBool::new(true));
        assert_eq!(
            with_command_cancel(cancel, || resolve_config_value(&command, None)),
            None
        );
        assert!(!marker.exists(), "a cancelled command ran");
        assert!(
            !command_cache()
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains_key(&command)
        );
    }

    // Bake: a descendant that left the shell's group (`setsid`) and keeps
    // the output open holds the read until the timeout, and then nothing
    // resolves, though the shell exited with a value. The group is killed
    // as soon as the shell exits, so the shell waits for the holder to
    // have left it: a shell that exits at once, as dash does, takes
    // `setsid` with it before it escapes.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_escaped_holder_of_the_output_fails_the_command_at_the_timeout() {
        let dir = crate::test_support::TempDir::new("config-value-escaped");
        let pid = dir.join("pid");
        let script = format!(
            "setsid sh -c 'echo $$ > \"$0\"; exec sleep 1000' '{0}' & \
             while [ ! -s '{0}' ]; do sleep 0.01; done; echo escaped-key",
            pid.display()
        );
        let outcome = run_command(&script, Duration::from_secs(2), None);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !pid.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let escaped: Option<libc::pid_t> = std::fs::read_to_string(&pid)
            .ok()
            .and_then(|text| text.trim().parse().ok());
        if let Some(escaped) = escaped {
            // SAFETY: plain syscall on the process this test's command
            // started; it left the group and nothing else kills it.
            unsafe {
                libc::kill(escaped, libc::SIGKILL);
            }
        }
        assert!(escaped.is_some(), "the escaped holder never started");
        assert!(matches!(outcome, CommandOutcome::Ran(None)));
    }

    // Bake: standard error is read and discarded, so a command that
    // writes more to it than a pipe holds still resolves.
    #[cfg(unix)]
    #[test]
    fn a_command_writing_much_to_stderr_resolves() {
        assert_eq!(
            resolve_config_value_uncached("!head -c 300000 /dev/zero >&2; echo noisy-key", None)
                .as_deref(),
            Some("noisy-key")
        );
    }

    // Bake: a resolution made on a Tokio worker thread runs on a thread of
    // its own, so it neither panics nor needs the caller's runtime.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_command_resolves_from_inside_a_runtime() {
        assert_eq!(
            resolve_config_value_uncached("!echo runtime-key", None).as_deref(),
            Some("runtime-key")
        );
    }

    #[cfg(unix)]
    #[test]
    fn oversized_output_fails() {
        let command = format!(
            "!head -c {} /dev/zero | tr '\\0' a",
            MAX_COMMAND_OUTPUT + 10
        );
        assert_eq!(resolve_config_value_uncached(&command, None), None);
    }
}
