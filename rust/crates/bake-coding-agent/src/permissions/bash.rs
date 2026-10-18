//! Pi's bash tool under the sandbox.
//!
//! Ported from Bake's TypeScript `packages/shell/bash-sandbox/src/{index,helpers}.ts`
//! (`SandboxBashExecutor.run`: wrap the exact shell argv through the
//! provider, fail closed when it cannot, classify a runner failure before a
//! denial) and the sandbox parts of `packages/shell/tool-bash/src/index.ts`
//! (`bashDescription`, the `sandbox_permissions` and `justification`
//! parameters, the escalation step) and `render.ts` (`renderResult`'s
//! denial marker and escalation hint).
//!
//! The wrapped tool is Pi's: [`SandboxedBashOperations`] is a
//! [`BashOperations`] that runs Pi's local shell through
//! [`LocalShellOperations::exec_prepared`], so output, truncation, timeout,
//! abort, and process-tree ownership stay Pi's. Each call resolves its
//! policy through the [`PermissionEngine`](super::PermissionEngine) first.
//!
//! Where Pi's text and the sandbox's meet: Pi's description gains the
//! TypeScript sandbox paragraph; Pi's parameters gain the two escalation
//! parameters; a denied run's output gains the TypeScript markers before
//! Pi's exit status, as `renderResult` put them before its exit marker. A
//! runner failure fails the call with the `SandboxUnavailable` text, as a
//! TypeScript foreground run did (the `runner_failed_notice` belonged to
//! background jobs, which Pi does not have).

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use bake_agent::BoxFuture;
use bake_ai::AbortSignal;
use bake_sandbox::escalation::{escalation_hint_marker, sandbox_denial_marker};
use bake_sandbox::{
    ConfinedCommand, SandboxMode, SandboxPolicy, SandboxProvider, SandboxUnavailable,
    classify_runner_failure, matches_signature,
};
use serde_json::Value;

use super::ToolPermissions;
use super::engine::{BlankJustification, ToolCall};
use super::with_escalation_parameters;
use crate::tools::bash::{
    BashDataCallback, BashExecError, BashExecOptions, BashOperations, BashToolOptions,
    LocalShellOperations, PreparedExecError, PreparedSpawn, bash_tool_description_for,
    bash_tool_parameters_for, bash_tool_prompt, create_bash_tool, create_local_bash_operations,
};
use crate::tools::definition::ToolDefinition;

/// The sandbox paragraph `bashDescription` appends when a confining
/// executor is composed: its denial sentences, then, when a wider mode is
/// reachable, [`BASH_SANDBOX_ESCALATION_DESCRIPTION`].
pub const BASH_SANDBOX_DESCRIPTION: &str = " A file sandbox may block commands; trying is safe. `[sandbox: file access denied under <mode> mode]` is a policy denial: do not work around it.";
/// The escalation sentences of `bashDescription`'s sandbox paragraph.
pub const BASH_SANDBOX_ESCALATION_DESCRIPTION: &str = " If a wider mode would help, retry the same command once this turn with the narrowest `sandbox_permissions` and a `justification`; that asks the user, so do not ask in chat. Request wider access up front only after this session was denied the same access. A rejection is final for that command.";
/// `tool-bash`'s `sandbox_permissions` description.
pub const BASH_SANDBOX_PERMISSIONS_DESCRIPTION: &str = "For retrying a denied command.";
/// `tool-bash`'s `justification` description.
pub const BASH_JUSTIFICATION_DESCRIPTION: &str = "One sentence for the user.";

/// How much of a confined run's standard error is kept for classification:
/// this much from its start and this much from its end.
pub const STDERR_CLASSIFY_BYTES: usize = 64 * 1024;

/// Pi's description with the sandbox paragraph, whose escalation sentences
/// appear only when `targets` is not empty, and the background sentence when
/// the tool takes `run_in_background`.
pub fn sandboxed_bash_tool_description(targets: &[SandboxMode], background: bool) -> String {
    let escalation = if targets.is_empty() {
        ""
    } else {
        BASH_SANDBOX_ESCALATION_DESCRIPTION
    };
    format!(
        "{}{BASH_SANDBOX_DESCRIPTION}{escalation}",
        bash_tool_description_for(background)
    )
}

/// Pi's parameters with `sandbox_permissions` (enum `targets`) and
/// `justification`, Pi's alone when `targets` is empty, with
/// `run_in_background` when `background`.
pub fn sandboxed_bash_tool_parameters(targets: &[SandboxMode], background: bool) -> Value {
    with_escalation_parameters(
        bash_tool_parameters_for(background),
        targets,
        BASH_SANDBOX_PERMISSIONS_DESCRIPTION,
        BASH_JUSTIFICATION_DESCRIPTION,
    )
}

/// The start and end of a stream, bounded.
#[derive(Debug, Default)]
struct StderrCapture {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    gap: bool,
}

impl StderrCapture {
    fn push(&mut self, data: &[u8]) {
        let room = STDERR_CLASSIFY_BYTES.saturating_sub(self.head.len());
        let (head, rest) = data.split_at(room.min(data.len()));
        self.head.extend_from_slice(head);
        let rest = if rest.len() > STDERR_CLASSIFY_BYTES {
            self.gap = true;
            self.tail.clear();
            &rest[rest.len() - STDERR_CLASSIFY_BYTES..]
        } else {
            rest
        };
        self.tail.extend(rest);
        let excess = self.tail.len().saturating_sub(STDERR_CLASSIFY_BYTES);
        if excess > 0 {
            self.gap = true;
            self.tail.drain(..excess);
        }
    }

    fn text(&self) -> String {
        let mut text = String::from_utf8_lossy(&self.head).into_owned();
        if !self.tail.is_empty() {
            if self.gap {
                // A line split by the gap must not join another.
                text.push('\n');
            }
            let (front, back) = self.tail.as_slices();
            let mut tail = front.to_vec();
            tail.extend_from_slice(back);
            text.push_str(&String::from_utf8_lossy(&tail));
        }
        text
    }
}

const NO_BYTE: u16 = u16::MAX;

/// Pi's local bash, confined: each command's shell argv is wrapped by the
/// provider under one call's policy. A confined mode the provider cannot
/// enforce fails the call with [`SandboxUnavailable`]'s text and runs
/// nothing; `danger-full-access` runs Pi's local shell as it is.
pub struct SandboxedBashOperations {
    local: LocalShellOperations,
    provider: Arc<dyn SandboxProvider>,
    policy: SandboxPolicy,
    escalation_hint: bool,
}

impl std::fmt::Debug for SandboxedBashOperations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxedBashOperations")
            .field("provider", &self.provider.name())
            .field("policy", &self.policy)
            .field("escalation_hint", &self.escalation_hint)
            .finish_non_exhaustive()
    }
}

impl SandboxedBashOperations {
    /// Runs `local`'s shell through `provider` under `policy`. A denial
    /// carries the escalation hint unless [`Self::with_escalation_hint`]
    /// turns it off.
    pub fn new(
        local: LocalShellOperations,
        provider: Arc<dyn SandboxProvider>,
        policy: SandboxPolicy,
    ) -> Self {
        Self {
            local,
            provider,
            policy,
            escalation_hint: true,
        }
    }

    /// Whether a denial carries the escalation hint: only when a wider mode
    /// than the policy's is reachable.
    pub fn with_escalation_hint(mut self, escalation_hint: bool) -> Self {
        self.escalation_hint = escalation_hint;
        self
    }

    async fn run(
        &self,
        command: String,
        cwd: PathBuf,
        options: BashExecOptions,
    ) -> Result<Option<i64>, BashExecError> {
        if !self.policy.mode.is_confined() {
            return self.local.exec(command, cwd, options).await;
        }
        let mode = self.policy.mode;
        let unavailable = |detail: Option<String>| {
            BashExecError::Failed(SandboxUnavailable::new(mode, detail).to_string())
        };
        let capture = Arc::new(Mutex::new(StderrCapture::default()));
        let last_byte = Arc::new(AtomicU16::new(NO_BYTE));
        let facts: Arc<Mutex<Option<ConfinedCommand>>> = Arc::default();
        let on_data = Arc::clone(&options.on_data);
        let tracked: BashDataCallback = {
            let on_data = Arc::clone(&on_data);
            let last_byte = Arc::clone(&last_byte);
            Arc::new(move |data: &[u8]| {
                if let Some(byte) = data.last() {
                    last_byte.store(u16::from(*byte), Ordering::Relaxed);
                }
                on_data(data);
            })
        };
        let on_stderr: BashDataCallback = {
            let capture = Arc::clone(&capture);
            Arc::new(move |data: &[u8]| {
                capture
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(data);
            })
        };
        let provider = Arc::clone(&self.provider);
        let policy = self.policy.clone();
        let recorded = Arc::clone(&facts);
        let outcome = self
            .local
            .exec_prepared(
                command,
                cwd.clone(),
                BashExecOptions {
                    on_data: tracked,
                    ..options
                },
                move |invocation| async move {
                    let argv: Vec<OsString> = invocation.argv;
                    let confined = provider
                        .confine(&argv, &policy)
                        .await
                        .map_err(|error| BashExecError::Failed(error.to_string()))?;
                    *recorded.lock().unwrap_or_else(PoisonError::into_inner) =
                        Some(confined.clone());
                    Ok(PreparedSpawn {
                        program: confined.program,
                        args: confined.args,
                        env: confined.env,
                        stdin: invocation.stdin,
                        on_stderr: Some(on_stderr),
                    })
                },
            )
            .await;
        let exit = match outcome {
            Ok(exit) => exit,
            Err(PreparedExecError {
                error: BashExecError::Failed(message),
                spawn_error: Some(kind),
            }) if is_runner_spawn_failure(kind, &cwd) => {
                // The runner itself could not start (`isRunnerSpawnFailure`).
                return Err(unavailable(Some(format!("Error: {message}"))));
            }
            Err(failure) => return Err(failure.error),
        };
        let Some(facts) = facts.lock().unwrap_or_else(PoisonError::into_inner).take() else {
            return Ok(exit);
        };
        let code = exit.and_then(|code| i32::try_from(code).ok());
        let stderr = capture
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .text();
        // A runner failure outranks a denial: the command did not run.
        if let Some(line) = classify_runner_failure(code, &stderr, &facts.runner_failure_rules) {
            return Err(unavailable(Some(line)));
        }
        if matches_signature(code, &stderr, &facts.denial_signatures) {
            let mut notice = String::new();
            let last = last_byte.load(Ordering::Relaxed);
            if last != NO_BYTE && last != u16::from(b'\n') {
                notice.push('\n');
            }
            notice.push_str(&sandbox_denial_marker(mode));
            if self.escalation_hint {
                notice.push('\n');
                notice.push_str(&escalation_hint_marker("command"));
            }
            on_data(notice.as_bytes());
        }
        Ok(exit)
    }
}

/// `isRunnerSpawnFailure`: the prepared program failed to spawn with an
/// executable error (`EACCES`, `ENOENT`) from a usable working directory.
fn is_runner_spawn_failure(kind: std::io::ErrorKind, cwd: &Path) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
    ) && cwd.is_dir()
}

impl BashOperations for SandboxedBashOperations {
    fn exec<'a>(
        &'a self,
        command: String,
        cwd: PathBuf,
        options: BashExecOptions,
    ) -> BoxFuture<'a, Result<Option<i64>, BashExecError>> {
        Box::pin(self.run(command, cwd, options))
    }
}

/// Pi's bash tool definition under the permission engine: each call
/// resolves its policy (the escalation step included), then runs Pi's tool
/// over [`SandboxedBashOperations`] with that policy. `options.operations`
/// must be `None`: custom operations cannot be confined.
pub fn create_sandboxed_bash_tool_definition(
    cwd: &Path,
    options: BashToolOptions,
    permissions: &ToolPermissions,
) -> Result<ToolDefinition, String> {
    if options.operations.is_some() {
        return Err(
            "permission: a sandboxed bash tool runs Pi's local shell; custom BashOperations cannot be confined"
                .into(),
        );
    }
    let prompt = bash_tool_prompt();
    let mut tool = create_bash_tool(cwd, options.clone());
    let cwd = cwd.to_path_buf();
    let permissions = permissions.clone();
    let background = options.jobs.is_some();
    tool.description = sandboxed_bash_tool_description(permissions.escalation_targets, background);
    tool.parameters = sandboxed_bash_tool_parameters(permissions.escalation_targets, background);
    tool.execute = Arc::new(
        move |id: String, args: Value, signal: Option<AbortSignal>, on_update| {
            let cwd = cwd.clone();
            let options = options.clone();
            let permissions = permissions.clone();
            Box::pin(async move {
                let call = ToolCall {
                    tool_name: "bash",
                    call_id: &id,
                    args: &args,
                    subject: "command",
                    blank_justification: BlankJustification::IgnoredAlone,
                };
                let policy = permissions.engine.resolve(&call, signal.as_ref()).await?;
                let local = create_local_bash_operations(options.shell_path.clone())
                    .with_bin_dir(options.bin_dir.clone())
                    .with_owner(options.owner.clone());
                let hint = permissions.offers_escalation_beyond(policy.mode);
                let operations = SandboxedBashOperations::new(local, permissions.provider, policy)
                    .with_escalation_hint(hint);
                let tool = create_bash_tool(
                    cwd,
                    BashToolOptions {
                        operations: Some(Arc::new(operations)),
                        ..options
                    },
                );
                (tool.execute)(id, args, signal, on_update).await
            }) as BoxFuture<'static, _>
        },
    );
    Ok(ToolDefinition {
        tool,
        prompt_snippet: prompt.snippet,
        prompt_guidelines: (!prompt.guidelines.is_empty()).then_some(prompt.guidelines),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_capture_keeps_both_ends() {
        let mut capture = StderrCapture::default();
        capture.push(b"start\n");
        assert_eq!(capture.text(), "start\n");
        let filler = vec![b'x'; STDERR_CLASSIFY_BYTES * 3];
        capture.push(&filler);
        capture.push(b"\nend: Read-only file system\n");
        let text = capture.text();
        assert!(text.starts_with("start\n"));
        assert!(text.ends_with("\nend: Read-only file system\n"));
        assert!(text.len() <= STDERR_CLASSIFY_BYTES * 2 + 1);
    }
}
