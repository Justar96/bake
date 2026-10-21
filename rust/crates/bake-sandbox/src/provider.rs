//! The provider seam, from `SandboxProvider`, `ConfinedArgv`,
//! `SandboxEnforcement`, and `SandboxUnavailableError` in
//! `packages/sandbox/sandbox/src/index.ts`.

use std::ffi::OsString;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

use crate::diagnostics::RunnerFailureRule;
use crate::mode::SandboxMode;
use crate::policy::SandboxPolicy;

/// How completely a backend enforces a policy's file effects on this host.
/// Callers that need an absolute boundary must not treat `Partial` as `Full`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Enforcement {
    /// Every promised file effect is governed.
    Full,
    /// Some effect escapes (an older kernel ABI, hard links, and so on).
    Partial,
}

impl Enforcement {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Enforcement::Full => "full",
            Enforcement::Partial => "partial",
        }
    }
}

/// A command wrapped for confinement: what to spawn in place of the caller's
/// argv, and how to read the run afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConfinedCommand {
    /// The program to spawn: a runner such as `bwrap`, or the caller's own
    /// program for a backend that confines in other ways.
    pub program: OsString,
    /// Its arguments, ending with the caller's argv.
    pub args: Vec<OsString>,
    /// Environment variables the runner needs, set on top of the caller's.
    pub env: Vec<(OsString, OsString)>,
    /// How completely the backend enforces the policy.
    pub enforcement: Enforcement,
    /// This backend's denial dialect: the case-insensitive stderr substrings
    /// a denied file effect produces under it (`EROFS` text under read-only
    /// binds, `EACCES` under Landlock, `EPERM` under Seatbelt). Match a failed
    /// run against exactly these, never a union across backends.
    pub denial_signatures: Vec<String>,
    /// Evidence that the runner itself failed, checked before denials: a
    /// runner failure means the command never ran.
    pub runner_failure_rules: Vec<RunnerFailureRule>,
}

impl ConfinedCommand {
    /// A command for `program` and `args` with no extra environment.
    pub fn new(
        program: impl Into<OsString>,
        args: Vec<OsString>,
        enforcement: Enforcement,
    ) -> Self {
        Self {
            program: program.into(),
            args,
            env: Vec::new(),
            enforcement,
            denial_signatures: Vec::new(),
            runner_failure_rules: Vec::new(),
        }
    }

    /// The same command with `signatures` as its denial dialect.
    pub fn with_denial_signatures(mut self, signatures: impl IntoIterator<Item = String>) -> Self {
        self.denial_signatures.extend(signatures);
        self
    }

    /// The same command with `rules` as its runner-failure evidence.
    pub fn with_runner_failure_rules(
        mut self,
        rules: impl IntoIterator<Item = RunnerFailureRule>,
    ) -> Self {
        self.runner_failure_rules.extend(rules);
        self
    }

    /// The same command with `key=value` set for the runner.
    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// The full argv: the program, then its arguments.
    pub fn argv(&self) -> Vec<OsString> {
        std::iter::once(self.program.clone())
            .chain(self.args.iter().cloned())
            .collect()
    }
}

/// The error code a refused confinement carries through tool results.
pub const SANDBOX_UNAVAILABLE: &str = "SANDBOX_UNAVAILABLE";

/// A confined mode was requested but nothing on this host can enforce it.
/// The command must not run (`SandboxUnavailableError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxUnavailable {
    /// The mode that could not be enforced.
    pub mode: SandboxMode,
    /// The runner's own failure text, when a runner was tried.
    pub detail: Option<String>,
}

impl SandboxUnavailable {
    /// The error for `mode`, with an optional runner detail.
    pub fn new(mode: SandboxMode, detail: Option<String>) -> Self {
        Self { mode, detail }
    }

    /// [`SANDBOX_UNAVAILABLE`].
    pub fn code(&self) -> &'static str {
        SANDBOX_UNAVAILABLE
    }
}

impl fmt::Display for SandboxUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "sandbox mode \"{}\" is requested but no sandbox backend is usable on this host; \
refusing to run the command unconfined. Install bubblewrap or run a Landlock-enforcing \
kernel (Linux), ensure sandbox-exec is usable (macOS), or ensure the ACL \
restricted-token runner can start (Windows) — otherwise switch the consumer to \
danger-full-access.",
            self.mode
        )?;
        if let Some(detail) = &self.detail {
            write!(f, " Runner failure: {detail}")?;
        }
        Ok(())
    }
}

impl std::error::Error for SandboxUnavailable {}

/// The future [`SandboxProvider::confine`] returns. Dropping it cancels the
/// provider's work (probing a runner, for example); a process that work
/// started is then settled under the call's [`ProbeOwner`].
pub type ConfineFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ConfinedCommand, SandboxUnavailable>> + Send + 'a>>;

/// The owner of the processes a provider starts while it confines one call,
/// such as a runner probe: the caller's own owner of its work (a session's,
/// for one), so that its teardown also waits for them.
///
/// Each process is spawned under a claim from [`Self::admit`], held until
/// that process has been settled (it and its whole tree are gone): by the
/// call itself, or, when the call's future is dropped mid-probe, by a task
/// that settles the dropped probe while it holds the claim
/// ([`bake_process::OwnedProcess`]). A call with no owner owns its probes
/// only while it runs: dropped, a probe is killed and not awaited.
pub trait ProbeOwner: Send + Sync {
    /// A claim for one process, or `None` once the owner is closing, which
    /// refuses the process: the call is refused, and nothing is cached. The
    /// refusal's detail is diagnostic; a caller should see its owner closing
    /// and report its own cancellation rather than the refusal.
    fn admit(&self) -> Option<bake_process::Claim>;
}

/// A process-confinement backend. Implementations must return an enforcing
/// command or refuse: returning the caller's argv unconfined is forbidden.
/// A backend may probe runners (once, then cache), and reports the
/// enforcement it achieves for each wrap.
pub trait SandboxProvider: Send + Sync {
    /// A short name for diagnostics, such as `bwrap` or `seatbelt`.
    fn name(&self) -> &str;

    /// Wraps `argv`, the exact program and arguments the caller is about to
    /// spawn (a shell consumer passes `["bash", "-c", command]`), to run
    /// confined under `policy`, whose mode is confining. Any process the
    /// provider starts for the call runs under `owner` ([`ProbeOwner`]).
    /// Use [`confine`] to let `danger-full-access` through unwrapped.
    fn confine<'a>(
        &'a self,
        argv: &'a [OsString],
        policy: &'a SandboxPolicy,
        owner: Option<&'a dyn ProbeOwner>,
    ) -> ConfineFuture<'a>;
}

/// What to spawn for `argv` under `policy`: `None` for `danger-full-access`,
/// which runs the caller's argv as it is, else the provider's wrapped
/// command or its refusal. Processes the provider starts run under `owner`.
pub async fn confine(
    provider: &dyn SandboxProvider,
    argv: &[OsString],
    policy: &SandboxPolicy,
    owner: Option<&dyn ProbeOwner>,
) -> Result<Option<ConfinedCommand>, SandboxUnavailable> {
    if !policy.mode.is_confined() {
        return Ok(None);
    }
    provider.confine(argv, policy, owner).await.map(Some)
}

/// A provider for hosts with no usable backend: every confined call fails
/// closed with [`SandboxUnavailable`] carrying `detail`.
#[derive(Debug, Clone)]
pub struct UnavailableProvider {
    detail: Option<String>,
}

impl UnavailableProvider {
    /// A provider that refuses with `detail`, such as why no backend exists.
    pub fn new(detail: Option<String>) -> Self {
        Self { detail }
    }
}

impl SandboxProvider for UnavailableProvider {
    fn name(&self) -> &str {
        "unavailable"
    }

    fn confine<'a>(
        &'a self,
        _argv: &'a [OsString],
        policy: &'a SandboxPolicy,
        _owner: Option<&'a dyn ProbeOwner>,
    ) -> ConfineFuture<'a> {
        let error = SandboxUnavailable::new(policy.mode, self.detail.clone());
        Box::pin(async move { Err(error) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<F: Future>(future: F) -> F::Output {
        // The futures here never pend.
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        let mut future = std::pin::pin!(future);
        loop {
            if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut context) {
                return value;
            }
        }
    }

    #[test]
    fn the_unavailable_message_is_the_typescript_one() {
        let error = SandboxUnavailable::new(SandboxMode::WorkspaceWrite, Some("bwrap: no".into()));
        assert_eq!(
            error.to_string(),
            "sandbox mode \"workspace-write\" is requested but no sandbox backend is usable on this host; refusing to run the command unconfined. Install bubblewrap or run a Landlock-enforcing kernel (Linux), ensure sandbox-exec is usable (macOS), or ensure the ACL restricted-token runner can start (Windows) — otherwise switch the consumer to danger-full-access. Runner failure: bwrap: no"
        );
        assert_eq!(error.code(), "SANDBOX_UNAVAILABLE");
    }

    #[test]
    fn full_access_skips_the_provider_and_others_fail_closed() {
        let provider = UnavailableProvider::new(None);
        let argv = vec![OsString::from("true")];
        let root = std::env::temp_dir();
        let open = SandboxPolicy::new(SandboxMode::DangerFullAccess, &root);
        assert_eq!(block_on(confine(&provider, &argv, &open, None)), Ok(None));
        let confined = SandboxPolicy::new(SandboxMode::ReadOnly, &root);
        assert_eq!(
            block_on(confine(&provider, &argv, &confined, None)),
            Err(SandboxUnavailable::new(SandboxMode::ReadOnly, None))
        );
    }
}
