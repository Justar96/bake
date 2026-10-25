//! Running one command hook, after Bake 0.3's
//! `packages/hooks/hook-protocol/src/runner.ts`, over the session's owned
//! processes and its D24 sandbox instead of 0.3's shell executor.
//!
//! The command runs as `bash -c <command>` (Pi's shell resolution,
//! [`get_shell_config`]) in the session's working directory, with Bake's
//! credential-scrubbed environment and the dialect's variables over it.
//! Before it is spawned the shell's argv is wrapped by the session's
//! sandbox provider for the standing sandbox mode read then, as an MCP
//! stdio server is admitted ([`crate::mcp`]); a mode that no backend can
//! enforce leaves the hook unrun. The process is spawned under a permit of
//! the session's [`WorkGate`], so shutdown waits until it and its tree are
//! settled, and it is killed with its tree on the caller's abort or at its
//! timeout. The payload is written to its standard input, which is then
//! closed. Each output stream keeps its first [`MAX_HOOK_OUTPUT_BYTES`],
//! 0.3's executor default.
//!
//! Every failure is an outcome, never a panic or an error to the caller:
//! a hook that cannot be started, times out, or is killed has no exit code
//! and is a non-blocking error, with the reason on its standard error.

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bake_ai::AbortSignal;
use bake_sandbox::SandboxProvider;
use bake_sandbox::provider::confine;

use super::codec::{HookOutput, parse_hook_output};
use super::config::CommandHook;
use crate::child_process::{self, ProcessSpec, wait_for_process};
use crate::owned_work::WorkGate;
use crate::permissions::StandingPermissions;
use crate::tools::bash::{CommandTransport, get_shell_config};

/// The most bytes kept of each output stream (0.3's `maxOutputBytes`).
pub const MAX_HOOK_OUTPUT_BYTES: usize = 64_000;

/// The timeout of a hook that sets none: 10 minutes, as in Claude Code,
/// Codex, and 0.3 (`DEFAULT_HOOK_TIMEOUT_MS`).
pub const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(600);

/// Where and how a session's hooks run.
#[derive(Clone)]
pub struct HookProcesses {
    /// The session's owner of its work.
    pub owner: WorkGate,
    /// The session's standing permission: its sandbox mode when a hook
    /// starts confines that hook.
    pub standing: Arc<StandingPermissions>,
    /// The provider that confines it.
    pub sandbox: Arc<dyn SandboxProvider>,
    /// The timeout of a hook that sets none.
    pub default_timeout: Duration,
}

impl std::fmt::Debug for HookProcesses {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookProcesses")
            .field("standing", &self.standing)
            .field("sandbox", &self.sandbox.name())
            .field("default_timeout", &self.default_timeout)
            .finish_non_exhaustive()
    }
}

/// One hook's outcome and how long it ran.
#[derive(Debug, Clone)]
pub struct HookRun {
    pub output: HookOutput,
    pub duration_ms: u64,
    /// Killed at its timeout.
    pub timed_out: bool,
}

/// Bounded capture of one stream.
struct Capture(Vec<u8>);

impl Capture {
    fn push(&mut self, data: &[u8]) {
        let room = MAX_HOOK_OUTPUT_BYTES.saturating_sub(self.0.len());
        self.0.extend_from_slice(&data[..data.len().min(room)]);
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0).into_owned()
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// A hook that did not run to an exit: no exit code, `reason` as its
/// standard error.
fn not_run(started: Instant, reason: String, timed_out: bool) -> HookRun {
    HookRun {
        output: parse_hook_output(None, "", &reason, None),
        duration_ms: elapsed_ms(started),
        timed_out,
    }
}

/// Runs `hook` in `cwd` with `stdin` and `env` set over the scrubbed
/// environment, decoding its outcome against `expected_event`. `signal`
/// kills it.
pub async fn run_hook(
    processes: &HookProcesses,
    hook: &CommandHook,
    cwd: &Path,
    env: &[(OsString, OsString)],
    stdin: Vec<u8>,
    expected_event: &str,
    signal: Option<&AbortSignal>,
) -> HookRun {
    let started = Instant::now();
    if signal.is_some_and(AbortSignal::aborted) {
        return not_run(started, "the hook was cancelled".to_owned(), false);
    }
    let shell = match get_shell_config(None) {
        Ok(shell) => shell,
        Err(error) => return not_run(started, error, false),
    };
    if shell.command_transport == CommandTransport::Stdin {
        return not_run(
            started,
            format!(
                "{} reads its command from standard input, which carries a hook's payload",
                shell.shell.display()
            ),
            false,
        );
    }
    let mut argv: Vec<OsString> = vec![shell.shell.into_os_string()];
    argv.extend(shell.args.into_iter().map(OsString::from));
    argv.push(hook.command.clone().into());
    let policy = processes.standing.policy();
    let confined = match confine(
        processes.sandbox.as_ref(),
        &argv,
        &policy,
        Some(&processes.owner),
    )
    .await
    {
        Ok(confined) => confined,
        // A closing owner refuses the probe: the session is shutting down,
        // so the hook is cancelled rather than refused.
        Err(_) if processes.owner.is_closing() => {
            return not_run(started, "the hook was cancelled".to_owned(), false);
        }
        Err(unavailable) => {
            return not_run(
                started,
                format!("the hook was not run: {unavailable}"),
                false,
            );
        }
    };
    let mut environment: Vec<(OsString, OsString)> =
        bake_process::environment::scrub_environment(std::env::vars_os()).collect();
    environment.extend(env.iter().cloned());
    let (program, args) = match confined {
        None => {
            let mut argv = argv.into_iter();
            let program = argv.next().unwrap_or_default();
            (program, argv.collect())
        }
        Some(confined) => {
            environment.extend(confined.env);
            (confined.program, confined.args)
        }
    };
    let spec = ProcessSpec {
        program,
        args,
        cwd: Some(cwd.to_path_buf()),
        env: Some(environment),
        stdin: true,
    };
    let timeout = hook.timeout.unwrap_or(processes.default_timeout);
    let deadline = tokio::time::Instant::now().checked_add(timeout);
    let child = match child_process::spawn(&spec, Some(&processes.owner)).await {
        Ok(child) => child,
        Err(error) => return not_run(started, format!("the hook could not start: {error}"), false),
    };
    let mut stdout = Capture(Vec::new());
    let mut stderr = Capture(Vec::new());
    let exit = wait_for_process(
        child,
        Some(stdin),
        signal,
        deadline,
        |data| stdout.push(data),
        |data| stderr.push(data),
    )
    .await;
    let exit = match exit {
        Ok(exit) => exit,
        Err(error) => return not_run(started, format!("the hook failed: {error}"), false),
    };
    if exit.timed_out {
        let mut reason = stderr.text();
        if !reason.trim().is_empty() {
            reason.push('\n');
        }
        reason.push_str(&format!(
            "the hook timed out after {} s",
            timeout.as_secs_f64()
        ));
        return HookRun {
            output: parse_hook_output(None, &stdout.text(), &reason, None),
            duration_ms: elapsed_ms(started),
            timed_out: true,
        };
    }
    // A kill, by the caller's abort, or a death by a signal has no code.
    let code = if exit.killed {
        None
    } else {
        exit.status.and_then(|status| status.code())
    };
    HookRun {
        output: parse_hook_output(code, &stdout.text(), &stderr.text(), Some(expected_event)),
        duration_ms: elapsed_ms(started),
        timed_out: false,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use bake_ai::AbortController;
    use bake_sandbox::{SandboxMode, UnavailableProvider};

    use super::*;
    use crate::hooks::codec::HookDecision;
    use crate::permissions::ApprovalPolicy;
    use crate::test_support::TempDir;

    fn processes(dir: &Path, mode: SandboxMode, timeout: Duration) -> HookProcesses {
        HookProcesses {
            owner: WorkGate::default(),
            standing: Arc::new(StandingPermissions::new(
                mode,
                ApprovalPolicy::Never,
                "test",
                dir,
                None,
            )),
            sandbox: Arc::new(UnavailableProvider::new(None)),
            default_timeout: timeout,
        }
    }

    fn command(text: &str) -> CommandHook {
        CommandHook {
            command: text.into(),
            timeout: None,
        }
    }

    async fn run(
        processes: &HookProcesses,
        dir: &Path,
        text: &str,
        env: &[(OsString, OsString)],
        stdin: &str,
    ) -> HookRun {
        run_hook(
            processes,
            &command(text),
            dir,
            env,
            stdin.as_bytes().to_vec(),
            "PreToolUse",
            None,
        )
        .await
    }

    #[tokio::test]
    async fn the_payload_env_and_cwd_reach_the_hook() {
        let dir = TempDir::new("hook-runner-io");
        let hooks = processes(
            dir.path(),
            SandboxMode::DangerFullAccess,
            DEFAULT_HOOK_TIMEOUT,
        );
        let env = [(
            OsString::from("CLAUDE_PROJECT_DIR"),
            OsString::from("/proj"),
        )];
        let ran = run(
            &hooks,
            dir.path(),
            r#"read -r line; printf '%s|%s|%s' "$line" "$CLAUDE_PROJECT_DIR" "$(pwd -P)""#,
            &env,
            "{\"a\":1}\n",
        )
        .await;
        let cwd = std::fs::canonicalize(dir.path()).expect("cwd");
        assert_eq!(ran.output.exit_code, Some(0));
        assert_eq!(
            ran.output.stdout,
            format!("{{\"a\":1}}|/proj|{}", cwd.display())
        );
    }

    #[tokio::test]
    async fn credential_variables_are_scrubbed() {
        let dir = TempDir::new("hook-runner-scrub");
        let hooks = processes(
            dir.path(),
            SandboxMode::DangerFullAccess,
            DEFAULT_HOOK_TIMEOUT,
        );
        // PATH is never credential-shaped; a name with KEY is.
        let ran = run(&hooks, dir.path(), "env", &[], "").await;
        assert!(ran.output.stdout.contains("PATH="));
        assert!(!ran.output.stdout.lines().any(|line| {
            line.split('=')
                .next()
                .is_some_and(|name| name.to_ascii_uppercase().contains("KEY"))
        }));
    }

    #[tokio::test]
    async fn exit_codes_decode_as_the_protocol_says() {
        let dir = TempDir::new("hook-runner-exit");
        let hooks = processes(
            dir.path(),
            SandboxMode::DangerFullAccess,
            DEFAULT_HOOK_TIMEOUT,
        );
        let ran = run(&hooks, dir.path(), "echo nope >&2; exit 2", &[], "").await;
        assert_eq!(ran.output.decision, Some(HookDecision::Block));
        assert_eq!(ran.output.reason.as_deref(), Some("nope"));
        let ran = run(&hooks, dir.path(), "echo warn >&2; exit 1", &[], "").await;
        assert_eq!((ran.output.exit_code, ran.output.decision), (Some(1), None));
        let ran = run(
            &hooks,
            dir.path(),
            r#"echo '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny"}}'"#,
            &[],
            "",
        )
        .await;
        assert_eq!(ran.output.decision, Some(HookDecision::Deny));
        // Another event's block is discarded.
        let ran = run(
            &hooks,
            dir.path(),
            r#"echo '{"hookSpecificOutput":{"hookEventName":"Stop","permissionDecision":"deny"}}'"#,
            &[],
            "",
        )
        .await;
        assert_eq!(ran.output.decision, None);
        assert_eq!(ran.output.hook_event_name.as_deref(), Some("Stop"));
    }

    #[tokio::test]
    async fn a_timeout_kills_the_hook_without_an_exit_code() {
        let dir = TempDir::new("hook-runner-timeout");
        let hooks = processes(
            dir.path(),
            SandboxMode::DangerFullAccess,
            Duration::from_millis(200),
        );
        let ran = run(&hooks, dir.path(), "echo started; exec sleep 30", &[], "").await;
        assert!(ran.timed_out);
        assert_eq!((ran.output.exit_code, ran.output.decision), (None, None));
        assert!(
            ran.output
                .stderr
                .ends_with("the hook timed out after 0.2 s")
        );
        assert!(ran.duration_ms < 10_000, "{}", ran.duration_ms);
        // A per-hook timeout, in seconds in the file, wins.
        let hook = CommandHook {
            command: "exit 0".into(),
            timeout: Some(Duration::from_secs(5)),
        };
        let ran = run_hook(&hooks, &hook, dir.path(), &[], Vec::new(), "Stop", None).await;
        assert_eq!(ran.output.exit_code, Some(0));
    }

    #[tokio::test]
    async fn an_abort_kills_the_hook() {
        let dir = TempDir::new("hook-runner-abort");
        let hooks = processes(
            dir.path(),
            SandboxMode::DangerFullAccess,
            DEFAULT_HOOK_TIMEOUT,
        );
        let controller = AbortController::new();
        let signal = controller.signal();
        let hook = command("exec sleep 30");
        let started = Instant::now();
        let run = run_hook(
            &hooks,
            &hook,
            dir.path(),
            &[],
            Vec::new(),
            "Stop",
            Some(&signal),
        );
        tokio::pin!(run);
        tokio::select! {
            _ = &mut run => panic!("the hook ended before the abort"),
            () = tokio::time::sleep(Duration::from_millis(100)) => controller.abort(),
        }
        let ran = run.await;
        assert_eq!(ran.output.exit_code, None);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn output_past_the_limit_is_dropped() {
        let dir = TempDir::new("hook-runner-limit");
        let hooks = processes(
            dir.path(),
            SandboxMode::DangerFullAccess,
            DEFAULT_HOOK_TIMEOUT,
        );
        let ran = run(
            &hooks,
            dir.path(),
            "head -c 200000 /dev/zero | tr '\\0' a",
            &[],
            "",
        )
        .await;
        assert_eq!(ran.output.stdout.len(), MAX_HOOK_OUTPUT_BYTES);
    }

    #[tokio::test]
    async fn a_mode_no_backend_enforces_leaves_the_hook_unrun() {
        let dir = TempDir::new("hook-runner-confine");
        let hooks = processes(
            dir.path(),
            SandboxMode::WorkspaceWrite,
            DEFAULT_HOOK_TIMEOUT,
        );
        let marker = dir.join("ran");
        let ran = run(
            &hooks,
            dir.path(),
            &format!("touch '{}'", marker.display()),
            &[],
            "",
        )
        .await;
        assert_eq!(ran.output.exit_code, None);
        assert!(ran.output.stderr.starts_with("the hook was not run: "));
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn a_closed_owner_refuses_the_hook() {
        let dir = TempDir::new("hook-runner-closed");
        let hooks = processes(
            dir.path(),
            SandboxMode::DangerFullAccess,
            DEFAULT_HOOK_TIMEOUT,
        );
        hooks.owner.close().await;
        let ran = run(&hooks, dir.path(), "exit 0", &[], "").await;
        assert_eq!(ran.output.exit_code, None);
        assert!(ran.output.stderr.starts_with("the hook could not start: "));
    }
}
