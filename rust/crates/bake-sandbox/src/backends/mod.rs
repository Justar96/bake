//! The local OS backends, ported from `LocalSandboxProvider` in
//! `packages/sandbox/sandbox-local/src/index.ts`: Linux bubblewrap then
//! Landlock, and macOS Seatbelt. The Windows restricted-token runner and the
//! unqualified AppContainer runner exist for explicit chains only: the
//! first does not confine deletion (see [`windows_acl`]), and the second has
//! no native qualification yet (see [`windows_appcontainer`]), so the
//! Windows default chain is empty. Windows and every other platform refuse
//! by default.
//!
//! | Module | Contents |
//! |---|---|
//! | [`profiles`] | the bubblewrap, Landlock, and Seatbelt profile builders |
//! | [`landlock`] | the Landlock helper `bake` re-executes itself as |
//! | [`windows_acl`] | the Windows restricted-token runner `bake` re-executes itself as |
//! | [`windows_appcontainer`] | the unqualified Windows AppContainer runner and its grant policy |
//!
//! Selection is by platform first and probes second: a chain with one
//! candidate selects it unprobed, and its own refusal at run time is the
//! fail-closed end; a chain with several probes each in order, once, and
//! caches the verdict for the provider's lifetime. When no candidate is
//! usable every confined call fails with [`SandboxUnavailable`].
//!
//! # Runner and root integrity
//!
//! - **Runner paths are fixed when the provider is built.** Bubblewrap is
//!   looked up on `PATH` once, in [`LocalProvider::new`], and the resulting
//!   absolute, canonical path is the program of the probe and every wrap.
//!   Relative and empty `PATH` entries (the current directory) and anything
//!   under `/tmp` or the OS temporary directory are skipped. The Landlock
//!   helper and the Windows runner are the current executable, resolved at
//!   the same moment, and Seatbelt is always `/usr/bin/sandbox-exec`.
//! - **A runner a confined call could replace is refused.** Before each
//!   wrap, the selected runner's program must not lie under any root the
//!   call's policy grants under `workspace-write` (checked for `read-only`
//!   calls too: the file tools and a sibling `workspace-write` call write
//!   those roots). Lying under a root is the same as having an ancestor
//!   directory under it, so a runner whose directory could be renamed is
//!   refused as well.
//! - **A writable root must still be the directory the session resolved.**
//!   Before each wrap every writable root must be a real directory that
//!   canonicalizes to itself, with no symbolic link in any component; a
//!   workspace moved and replaced by a link is refused. The Landlock helper
//!   also opens each granted path without following any link
//!   (`openat2(RESOLVE_NO_SYMLINKS)`, or `O_PATH | O_NOFOLLOW` per component
//!   on kernels before 5.6), so the window between that check and the spawn
//!   cannot redirect its grant.
//! - **Every confined command runs in a new session**: bubblewrap's
//!   `--new-session`, and `setsid` (or `TIOCNOTTY` when the helper already
//!   leads a process group) in the Landlock helper. A confined command
//!   therefore has no controlling terminal shared with Bake, which closes
//!   `TIOCSTI` keystroke injection on kernels that still allow it.

pub mod landlock;
pub mod profiles;
pub mod windows_acl;
pub mod windows_appcontainer;

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use bake_process::{OwnedProcess, ProcessSpec};

use tokio::io::AsyncReadExt;
use tokio::sync::OnceCell;

use crate::diagnostics::RunnerFailureRule;
use crate::mode::SandboxMode;
use crate::paths::{canonical_path, is_path_under};
use crate::policy::SandboxPolicy;
use crate::provider::{
    ConfineFuture, ConfinedCommand, Enforcement, ProbeOwner, SandboxProvider, SandboxUnavailable,
};

/// One rung of a platform chain (`SelectedRunner['runner']`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Runner {
    /// Bubblewrap: a read-only mount namespace with writable binds.
    Bwrap,
    /// The Landlock helper: a self-applied Landlock ruleset.
    Landlock,
    /// Seatbelt through `sandbox-exec` and an SBPL profile.
    Seatbelt,
    /// The Windows restricted-token runner (`windows-acl`).
    WindowsAcl,
    /// The unqualified Windows AppContainer runner
    /// (`windows-appcontainer`).
    WindowsAppContainer,
}

impl Runner {
    /// The runner's name.
    pub fn as_str(self) -> &'static str {
        match self {
            Runner::Bwrap => "bwrap",
            Runner::Landlock => "landlock",
            Runner::Seatbelt => "seatbelt",
            Runner::WindowsAcl => "windows-acl",
            Runner::WindowsAppContainer => "windows-appcontainer",
        }
    }

    /// The runner's program as a refusal names it.
    fn program_label(self) -> &'static str {
        match self {
            Runner::Bwrap => "bubblewrap",
            Runner::Landlock => "Landlock helper",
            Runner::Seatbelt => "sandbox-exec",
            Runner::WindowsAcl => "Windows ACL runner",
            Runner::WindowsAppContainer => "Windows AppContainer runner",
        }
    }

    /// The enforcement a rung claims when selected without a probe
    /// (`STATIC_ENFORCEMENT`). Landlock is listed for totality: the Linux
    /// chain reaches it only through its probe, whose report decides. The
    /// Windows runner is partial: deletion, Everyone's grants, and hard links
    /// escape it (see [`windows_acl`]), and no default chain holds it. The
    /// AppContainer runner is partial until a native qualification shows
    /// otherwise; hard links, at least, escape it (see
    /// [`windows_appcontainer`]).
    pub fn static_enforcement(self) -> Enforcement {
        match self {
            Runner::WindowsAcl | Runner::WindowsAppContainer => Enforcement::Partial,
            _ => Enforcement::Full,
        }
    }

    /// The case-insensitive stderr substrings a denied file effect produces
    /// under this runner (`DENIAL_SIGNATURES`).
    pub fn denial_signatures(self) -> Vec<String> {
        let signatures: &[&str] = match self {
            Runner::Bwrap => &["read-only file system"],
            Runner::Landlock => &["permission denied"],
            Runner::Seatbelt => &["operation not permitted"],
            Runner::WindowsAcl => &windows_acl::DENIAL_SIGNATURES,
            Runner::WindowsAppContainer => &windows_appcontainer::DENIAL_SIGNATURES,
        };
        signatures
            .iter()
            .map(|&signature| signature.to_owned())
            .collect()
    }

    /// The runner's own fatal diagnostics (`RUNNER_FAILURE_RULES`). Landlock
    /// has a status-gated contract; bubblewrap and `sandbox-exec` reserve no
    /// status, so theirs are signature-only.
    pub fn runner_failure_rules(self) -> Vec<RunnerFailureRule> {
        let rule = match self {
            Runner::Bwrap => RunnerFailureRule {
                fatal_signatures: vec!["bwrap: ".to_owned()],
                ..RunnerFailureRule::default()
            },
            Runner::Landlock => RunnerFailureRule {
                allowed_exit_codes: vec![landlock::LAUNCHER_FAILURE_EXIT],
                fatal_signatures: vec![format!("{}: ", landlock::LAUNCHER_BIN)],
                informational_lines: vec![landlock::PARTIAL_ENFORCEMENT_LINE.to_owned()],
            },
            Runner::Seatbelt => RunnerFailureRule {
                fatal_signatures: vec!["sandbox-exec: ".to_owned()],
                ..RunnerFailureRule::default()
            },
            Runner::WindowsAcl => RunnerFailureRule {
                allowed_exit_codes: vec![windows_acl::RUNNER_FAILURE_EXIT],
                fatal_signatures: vec![format!("{}: ", windows_acl::RUNNER_SIGNATURE)],
                ..RunnerFailureRule::default()
            },
            Runner::WindowsAppContainer => RunnerFailureRule {
                allowed_exit_codes: vec![windows_appcontainer::RUNNER_FAILURE_EXIT],
                fatal_signatures: vec![format!("{}: ", windows_appcontainer::RUNNER_SIGNATURE)],
                ..RunnerFailureRule::default()
            },
        };
        vec![rule]
    }
}

/// A program and the arguments that precede a runner's profile.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Invocation {
    program: OsString,
    args: Vec<OsString>,
}

impl Invocation {
    fn new(program: impl Into<OsString>, args: impl IntoIterator<Item = OsString>) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().collect(),
        }
    }
}

/// The chain's cached verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Selected(Runner, Enforcement),
    Unavailable,
}

/// The default bound on each functional probe (`probeTimeoutMs`).
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The most probe output read; the Landlock report is one short line.
const PROBE_OUTPUT_LIMIT: u64 = 4096;

/// The system `sandbox-exec`.
pub const SEATBELT_EXEC: &str = "/usr/bin/sandbox-exec";

/// The local process-sandbox provider (`LocalSandboxProvider`).
///
/// Probing spawns runners through Tokio: the first confined call on a
/// multi-candidate chain must be polled inside a Tokio runtime with its I/O
/// and time drivers enabled. Outside any runtime the call refuses without
/// caching. A probe is bounded by the probe timeout; it runs as an owned
/// process tree under a claim of the call's [`ProbeOwner`], and is killed
/// and settled when it ends, times out, or is dropped with the confine
/// future, a dropped one by a task holding that claim, so the owner's
/// teardown waits for it (see `run_probe`). A walk whose owner is closing
/// refuses the call and caches nothing.
#[derive(Debug)]
pub struct LocalProvider {
    chain: Vec<Runner>,
    /// Why the chain is empty, when the platform has no backend.
    no_backend_detail: Option<String>,
    /// `None` when no `bwrap` was found on `PATH` when the provider was built.
    bwrap: Option<Invocation>,
    /// `None` when the current executable could not be resolved.
    landlock: Option<Invocation>,
    seatbelt: Invocation,
    /// `None` when the current executable could not be resolved.
    windows_acl: Option<Invocation>,
    /// `None` when the current executable could not be resolved.
    windows_appcontainer: Option<Invocation>,
    /// The AppContainer runner's read and network grants.
    appcontainer_grants: windows_appcontainer::AppContainerGrants,
    /// The AppContainer runner's observation tag; `None` in Bake.
    appcontainer_profile_tag: Option<windows_appcontainer::ProfileTag>,
    probe_timeout: Duration,
    verdict: OnceCell<Verdict>,
}

impl Default for LocalProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalProvider {
    /// The provider for this platform (`PLATFORM_CHAINS`): bubblewrap then
    /// Landlock on Linux, Seatbelt on macOS, and nothing elsewhere, Windows
    /// included ([`windows_acl::DEFAULT_REFUSAL`]). The Landlock helper and
    /// the Windows runner of an explicit chain are the current executable,
    /// re-run with [`landlock::HELPER_ARG`] or [`windows_acl::HELPER_ARG`].
    ///
    /// Building one searches `PATH` and resolves the current executable; the
    /// first confined call on a multi-candidate chain probes. Build it once
    /// per process and share it (see [`default_provider`]).
    pub fn new() -> Self {
        let (chain, detail) = platform_chain();
        let mut provider = Self::with_chain(chain);
        provider.no_backend_detail = detail;
        provider
    }

    /// A provider walking `chain` in place of the platform's: the test seam
    /// TypeScript called `internals.chain`. An empty chain refuses every
    /// confined call. Bubblewrap is looked up on the process `PATH` now.
    pub fn with_chain(chain: Vec<Runner>) -> Self {
        let exe = std::env::current_exe().ok();
        let landlock = exe
            .clone()
            .map(|exe| Invocation::new(exe, [OsString::from(landlock::HELPER_ARG)]));
        let windows_acl = exe
            .clone()
            .map(|exe| Invocation::new(exe, [OsString::from(windows_acl::HELPER_ARG)]));
        let windows_appcontainer =
            exe.map(|exe| Invocation::new(exe, [OsString::from(windows_appcontainer::HELPER_ARG)]));
        let path = std::env::var_os("PATH");
        Self {
            chain,
            no_backend_detail: None,
            bwrap: bwrap_on_path(path.as_deref()),
            landlock,
            seatbelt: Invocation::new(SEATBELT_EXEC, []),
            windows_acl,
            windows_appcontainer,
            appcontainer_grants: windows_appcontainer::AppContainerGrants::new(),
            appcontainer_profile_tag: None,
            probe_timeout: DEFAULT_PROBE_TIMEOUT,
            verdict: OnceCell::new(),
        }
    }

    /// Runs bubblewrap as `program` with `args` before its profile.
    pub fn with_bwrap(
        mut self,
        program: impl Into<OsString>,
        args: impl IntoIterator<Item = OsString>,
    ) -> Self {
        self.bwrap = Some(Invocation::new(program, args));
        self
    }

    /// Looks bubblewrap up again on `path`, a `PATH`-style list, instead of
    /// the process `PATH`; the lookup rules are [`LocalProvider::new`]'s.
    pub fn with_search_path(mut self, path: impl AsRef<OsStr>) -> Self {
        self.bwrap = bwrap_on_path(Some(path.as_ref()));
        self
    }

    /// Runs the Landlock helper as `program` with `args` before its grants;
    /// a Bake binary needs `[HELPER_ARG]`.
    pub fn with_landlock_helper(
        mut self,
        program: impl Into<OsString>,
        args: impl IntoIterator<Item = OsString>,
    ) -> Self {
        self.landlock = Some(Invocation::new(program, args));
        self
    }

    /// Runs Seatbelt as `program` with `args` before its profile.
    pub fn with_seatbelt(
        mut self,
        program: impl Into<OsString>,
        args: impl IntoIterator<Item = OsString>,
    ) -> Self {
        self.seatbelt = Invocation::new(program, args);
        self
    }

    /// Runs the Windows runner as `program` with `args` before its profile;
    /// a Bake binary needs `[windows_acl::HELPER_ARG]`.
    pub fn with_windows_acl_runner(
        mut self,
        program: impl Into<OsString>,
        args: impl IntoIterator<Item = OsString>,
    ) -> Self {
        self.windows_acl = Some(Invocation::new(program, args));
        self
    }

    /// Runs the Windows AppContainer runner as `program` with `args` before
    /// its profile; a Bake binary needs `[windows_appcontainer::HELPER_ARG]`.
    pub fn with_windows_appcontainer_runner(
        mut self,
        program: impl Into<OsString>,
        args: impl IntoIterator<Item = OsString>,
    ) -> Self {
        self.windows_appcontainer = Some(Invocation::new(program, args));
        self
    }

    /// Gives every AppContainer wrap `grants`: read roots and network
    /// capabilities beyond the workspace and the run's own profile storage.
    /// They are validated on each wrap, and again by the runner, and a wrap
    /// whose grants fail is refused. The default grants nothing.
    pub fn with_windows_appcontainer_grants(
        mut self,
        grants: windows_appcontainer::AppContainerGrants,
    ) -> Self {
        self.appcontainer_grants = grants;
        self
    }

    /// Passes `tag` to every AppContainer wrap, so each run's profile name
    /// starts with [`ProfileTag::moniker_prefix`](windows_appcontainer::ProfileTag::moniker_prefix).
    /// It only lets the caller attribute its own runs' profiles; creation
    /// stays exclusive and random. Bake never sets one; the enforcement
    /// tests do.
    pub fn with_windows_appcontainer_profile_tag(
        mut self,
        tag: windows_appcontainer::ProfileTag,
    ) -> Self {
        self.appcontainer_profile_tag = Some(tag);
        self
    }

    /// Bounds each functional probe by `timeout`. A zero timeout fails every
    /// probe, so a multi-candidate chain refuses.
    pub fn with_probe_timeout(mut self, timeout: Duration) -> Self {
        self.probe_timeout = timeout;
        self
    }

    /// The selected runner and its enforcement, walking the chain if no call
    /// has yet, with its probes under `owner`; `None` when nothing is
    /// usable, or when the walk was refused because `owner` is closing.
    pub async fn selected(&self, owner: Option<&dyn ProbeOwner>) -> Option<(Runner, Enforcement)> {
        match self.verdict(owner).await {
            Ok(Verdict::Selected(runner, enforcement)) => Some((runner, enforcement)),
            _ => None,
        }
    }

    async fn verdict(&self, owner: Option<&dyn ProbeOwner>) -> Result<Verdict, Option<String>> {
        if let Some(verdict) = self.verdict.get() {
            return Ok(*verdict);
        }
        if self.chain.len() > 1 && tokio::runtime::Handle::try_current().is_err() {
            return Err(Some(
                "probing a sandbox runner needs a Tokio runtime".to_owned(),
            ));
        }
        // A refused walk caches nothing: the next call, under an owner of
        // its own, walks again.
        self.verdict
            .get_or_try_init(|| self.chain_verdict(owner))
            .await
            .copied()
            .map_err(|OwnerClosing| Some(OWNER_CLOSING.to_owned()))
    }

    /// Walks the chain (`chainVerdict`): a sole candidate unprobed, several
    /// probed in order, none usable is unavailable.
    async fn chain_verdict(&self, owner: Option<&dyn ProbeOwner>) -> Result<Verdict, OwnerClosing> {
        match self.chain.as_slice() {
            [] => Ok(Verdict::Unavailable),
            [sole] => Ok(Verdict::Selected(*sole, sole.static_enforcement())),
            chain => {
                for runner in chain {
                    if let Some(enforcement) = self.probe(*runner, owner).await? {
                        return Ok(Verdict::Selected(*runner, enforcement));
                    }
                }
                Ok(Verdict::Unavailable)
            }
        }
    }

    /// One rung's functional probe (`probeRunner`): `None` when unusable.
    async fn probe(
        &self,
        runner: Runner,
        owner: Option<&dyn ProbeOwner>,
    ) -> Result<Option<Enforcement>, OwnerClosing> {
        let probe = Probe {
            timeout: self.probe_timeout,
            owner,
        };
        let probe_policy = SandboxPolicy::new(SandboxMode::ReadOnly, "/");
        match runner {
            Runner::Bwrap => {
                let Some(bwrap) = self.bwrap.as_ref() else {
                    return Ok(None);
                };
                let mut args = profiles::bwrap_profile_args(&probe_policy);
                args.extend(["--".into(), "true".into()]);
                let ran = probe.run(bwrap, &args, false).await?;
                Ok(ran.and_then(|(code, _)| (code == Some(0)).then_some(Enforcement::Full)))
            }
            Runner::Landlock => {
                let Some(helper) = self.landlock.as_ref() else {
                    return Ok(None);
                };
                let args = [OsString::from("--probe")];
                let ran = probe.run(helper, &args, true).await?;
                Ok(ran.and_then(|(code, stdout)| {
                    if code != Some(0) {
                        return None;
                    }
                    // Only the contract's exact report line counts: a
                    // program that merely exits 0 is not a Landlock helper.
                    match stdout.as_slice() {
                        out if out == format!("{}\n", landlock::PROBE_FULL_LINE).as_bytes() => {
                            Some(Enforcement::Full)
                        }
                        out if out == format!("{}\n", landlock::PROBE_PARTIAL_LINE).as_bytes() => {
                            Some(Enforcement::Partial)
                        }
                        _ => None,
                    }
                }))
            }
            Runner::Seatbelt => {
                let Ok(mut args) = profiles::seatbelt_profile_args(&probe_policy) else {
                    return Ok(None);
                };
                args.extend(["--".into(), "true".into()]);
                let ran = probe.run(&self.seatbelt, &args, false).await?;
                Ok(ran.and_then(|(code, _)| (code == Some(0)).then_some(Enforcement::Full)))
            }
            Runner::WindowsAcl => {
                // TypeScript's probe: a read-only run, which grants nothing,
                // around `cmd /c exit 0`. Only an override chain probes it.
                let Some(runner) = self.windows_acl.as_ref() else {
                    return Ok(None);
                };
                let temp = canonical_path(&std::env::temp_dir());
                let mut args = windows_acl::runner_profile_args(
                    windows_acl::RunnerMode::ReadOnly,
                    &temp,
                    &temp,
                );
                args.extend(["--", "cmd", "/c", "exit", "0"].map(OsString::from));
                let ran = probe.run(runner, &args, false).await?;
                Ok(ran.and_then(|(code, _)| (code == Some(0)).then_some(Enforcement::Partial)))
            }
            // Never probed: a real launch would leave standing workspace
            // ACEs on the probe's own directory. The rung is
            // selectable only as a sole candidate, whose launch failure is
            // the fail-closed end.
            Runner::WindowsAppContainer => Ok(None),
        }
    }

    /// The wrapped command for one policy (`confine` and `runnerArgv`).
    async fn wrap(
        &self,
        argv: &[OsString],
        policy: &SandboxPolicy,
        owner: Option<&dyn ProbeOwner>,
    ) -> Result<ConfinedCommand, SandboxUnavailable> {
        let refuse = |detail: Option<String>| SandboxUnavailable::new(policy.mode, detail);
        let (runner, enforcement) = match self.verdict(owner).await.map_err(refuse)? {
            Verdict::Selected(runner, enforcement) => (runner, enforcement),
            Verdict::Unavailable => return Err(refuse(self.no_backend_detail.clone())),
        };
        // A root that is no longer the directory the session resolved would
        // redirect the grant to wherever it now leads.
        check_writable_roots(&policy.writable_roots()).map_err(|detail| refuse(Some(detail)))?;
        let (invocation, profile) = match runner {
            Runner::Bwrap => {
                let Some(bwrap) = self.bwrap.as_ref() else {
                    return Err(refuse(Some(
                        "bubblewrap (bwrap) was not found on PATH when the sandbox provider was built"
                            .to_owned(),
                    )));
                };
                (bwrap, profiles::bwrap_profile_args(policy))
            }
            Runner::Landlock => {
                let Some(helper) = self.landlock.as_ref() else {
                    return Err(refuse(Some(
                        "the Landlock helper (the current executable) could not be resolved"
                            .to_owned(),
                    )));
                };
                (helper, profiles::landlock_profile_args(policy))
            }
            Runner::WindowsAcl => {
                let Some(runner) = self.windows_acl.as_ref() else {
                    return Err(refuse(Some(
                        "the Windows ACL runner (the current executable) could not be resolved"
                            .to_owned(),
                    )));
                };
                (
                    runner,
                    windows_acl_profile(policy).map_err(|detail| refuse(Some(detail)))?,
                )
            }
            Runner::WindowsAppContainer => {
                let Some(runner) = self.windows_appcontainer.as_ref() else {
                    return Err(refuse(Some(
                        "the Windows AppContainer runner (the current executable) could not be resolved"
                            .to_owned(),
                    )));
                };
                (
                    runner,
                    windows_appcontainer_profile(
                        policy,
                        &self.appcontainer_grants,
                        self.appcontainer_profile_tag.as_ref(),
                    )
                    .map_err(|detail| refuse(Some(detail)))?,
                )
            }
            Runner::Seatbelt => match profiles::seatbelt_profile_args(policy) {
                Ok(profile) => (&self.seatbelt, profile),
                Err(root) => {
                    return Err(refuse(Some(format!(
                        "sandbox-exec cannot express the non-UTF-8 writable root {}",
                        root.display()
                    ))));
                }
            },
        };
        check_runner_location(runner, &invocation.program, policy)
            .map_err(|detail| refuse(Some(detail)))?;
        let mut args = invocation.args.clone();
        args.extend(profile);
        args.push("--".into());
        args.extend(argv.iter().cloned());
        Ok(
            ConfinedCommand::new(invocation.program.clone(), args, enforcement)
                .with_denial_signatures(runner.denial_signatures())
                .with_runner_failure_rules(runner.runner_failure_rules()),
        )
    }
}

impl SandboxProvider for LocalProvider {
    fn name(&self) -> &str {
        "local"
    }

    fn confine<'a>(
        &'a self,
        argv: &'a [OsString],
        policy: &'a SandboxPolicy,
        owner: Option<&'a dyn ProbeOwner>,
    ) -> ConfineFuture<'a> {
        Box::pin(self.wrap(argv, policy, owner))
    }
}

/// The Windows runner's profile for `policy`. Only the workspace and a
/// private directory under the OS temporary directory become writable:
/// [`SandboxPolicy::writable_roots`]'s temporary-directory entry is never
/// granted (a standing ACE there would reach every later private directory
/// and the user's other temporary files), and extra writable roots are
/// refused, because the runner has no capability of their own to grant them
/// and granting them the workspace's would widen every later call on that
/// workspace.
fn windows_acl_profile(policy: &SandboxPolicy) -> Result<Vec<OsString>, String> {
    let mode = match policy.mode {
        SandboxMode::ReadOnly => windows_acl::RunnerMode::ReadOnly,
        SandboxMode::WorkspaceWrite => windows_acl::RunnerMode::WorkspaceWrite,
        SandboxMode::DangerFullAccess => {
            return Err("danger-full-access is not a confined mode".to_owned());
        }
    };
    if mode == windows_acl::RunnerMode::WorkspaceWrite && !policy.extra_writable_roots.is_empty() {
        return Err(
            "the Windows ACL runner grants only the workspace and a private temporary directory; extra writable roots are not supported"
                .to_owned(),
        );
    }
    let temp_root = canonical_path(&std::env::temp_dir());
    let workspace = crate::paths::normalize_lexically(&policy.workspace_root);
    if mode == windows_acl::RunnerMode::WorkspaceWrite {
        windows_acl::check_temp_root_outside_workspace(&workspace, &temp_root)?;
    }
    Ok(windows_acl::runner_profile_args(
        mode, &workspace, &temp_root,
    ))
}

/// The AppContainer runner's profile for `policy` and `grants`. As for the
/// restricted-token runner, extra writable roots are refused and the ambient
/// temporary directory is never granted: each run's `TEMP` is its own
/// AppContainer profile's storage, which the runner creates and deletes.
/// The grants pass their path and disk checks against this host's
/// locations before the wrap is returned.
fn windows_appcontainer_profile(
    policy: &SandboxPolicy,
    grants: &windows_appcontainer::AppContainerGrants,
    tag: Option<&windows_appcontainer::ProfileTag>,
) -> Result<Vec<OsString>, String> {
    let mode = match policy.mode {
        SandboxMode::ReadOnly => windows_acl::RunnerMode::ReadOnly,
        SandboxMode::WorkspaceWrite => windows_acl::RunnerMode::WorkspaceWrite,
        SandboxMode::DangerFullAccess => {
            return Err("danger-full-access is not a confined mode".to_owned());
        }
    };
    if !policy.extra_writable_roots.is_empty() {
        return Err(
            "the Windows AppContainer runner grants only the workspace and its own profile storage; extra writable roots are not supported"
                .to_owned(),
        );
    }
    let workspace = crate::paths::normalize_lexically(&policy.workspace_root);
    grants
        .validate_shape(&workspace, &windows_appcontainer::HostRoots::from_env())
        .and_then(|()| grants.check_on_disk())
        .map_err(|error| error.to_string())?;
    Ok(windows_appcontainer::runner_profile_args(
        mode, &workspace, grants, tag,
    ))
}

/// This platform's chain, and why it is empty when it is.
fn platform_chain() -> (Vec<Runner>, Option<String>) {
    if cfg!(target_os = "linux") {
        (vec![Runner::Bwrap, Runner::Landlock], None)
    } else if cfg!(target_os = "macos") {
        (vec![Runner::Seatbelt], None)
    } else if cfg!(windows) {
        // The runner leaves deletion unconfined; never select it by default.
        (Vec::new(), Some(windows_acl::DEFAULT_REFUSAL.to_owned()))
    } else {
        (
            Vec::new(),
            Some(format!(
                "this build has no sandbox backend for {}",
                std::env::consts::OS
            )),
        )
    }
}

/// The provider Bake confines commands with on this host: a
/// [`LocalProvider`] for the platform chain.
///
/// It is built once per process, on the first call, and every call returns
/// the same shared provider: `PATH` is searched and the current executable
/// resolved at that moment, and the probe verdict is cached for the rest of
/// the process. Cloning the returned [`Arc`] is the cheap way to hand it to
/// each consumer; never build a provider per call.
pub fn default_provider() -> Arc<dyn SandboxProvider> {
    static DEFAULT: OnceLock<Arc<LocalProvider>> = OnceLock::new();
    DEFAULT
        .get_or_init(|| Arc::new(LocalProvider::new()))
        .clone()
}

/// Bubblewrap on `path`, resolved once (see the module docs): the first
/// absolute entry holding an executable regular file named `bwrap`, by its
/// canonical path, skipping candidates under `/tmp` or the OS temporary
/// directory, which any `workspace-write` call may write.
fn bwrap_on_path(path: Option<&OsStr>) -> Option<Invocation> {
    find_program("bwrap", path).map(|program| Invocation::new(program, []))
}

fn find_program(name: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    let temp_roots = [
        canonical_path(Path::new("/tmp")),
        canonical_path(&std::env::temp_dir()),
    ];
    for dir in std::env::split_paths(path?) {
        if !dir.is_absolute() {
            continue;
        }
        let Ok(program) = std::fs::canonicalize(dir.join(name)) else {
            continue;
        };
        if !is_executable_file(&program)
            || temp_roots.iter().any(|root| is_path_under(&program, root))
        {
            continue;
        }
        return Some(program);
    }
    None
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

/// Refuses unless every writable root is still the directory the session
/// resolved: it exists, is a directory and not a link, and canonicalizes to
/// itself, so no component anywhere in it is a link.
fn check_writable_roots(roots: &[PathBuf]) -> Result<(), String> {
    for root in roots {
        let changed = |why: String| {
            format!(
                "writable root {} changed since the session started: {why}",
                root.display()
            )
        };
        match std::fs::symlink_metadata(root) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(changed("it no longer exists".to_owned()));
            }
            Err(error) => return Err(changed(format!("it cannot be inspected: {error}"))),
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(changed("it is now a symbolic link".to_owned()));
            }
            Ok(meta) if !meta.is_dir() => {
                return Err(changed("it is no longer a directory".to_owned()));
            }
            Ok(_) => {}
        }
        let resolved = canonical_path(root);
        if resolved != *root {
            return Err(changed(format!(
                "it now resolves to {} through a symbolic link",
                resolved.display()
            )));
        }
    }
    Ok(())
}

/// Refuses a runner program a confined call or a file tool could replace:
/// one under any root `policy` grants under `workspace-write`, whatever the
/// call's own mode. A program is under a root exactly when one of its
/// ancestor directories is, so this also covers a directory that could be
/// renamed out from under it.
fn check_runner_location(
    runner: Runner,
    program: &OsStr,
    policy: &SandboxPolicy,
) -> Result<(), String> {
    let program = Path::new(program);
    let label = runner.program_label();
    if !program.is_absolute() {
        return Err(format!(
            "the {label} {} is not an absolute path; refusing to run it",
            program.display()
        ));
    }
    let program = canonical_path(program);
    let roots = policy
        .clone()
        .with_mode(SandboxMode::WorkspaceWrite)
        .writable_roots();
    if let Some(root) = roots.iter().find(|root| is_path_under(&program, root)) {
        return Err(format!(
            "the {label} {} lies under the writable root {}, where a confined command could replace it; refusing to run it",
            program.display(),
            root.display()
        ));
    }
    Ok(())
}

/// Why a walk was refused: its owner is closing, so no probe may start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OwnerClosing;

/// The refusal detail of a walk whose owner is closing: diagnostic only.
/// A caller that passes an owner sees the owner closing and reports its own
/// cancellation instead, so this never reaches a model.
const OWNER_CLOSING: &str = "the call's owner is closing, so no sandbox runner was probed";

/// One walk's probe settings: the bound on each probe and its owner.
struct Probe<'a> {
    timeout: Duration,
    owner: Option<&'a dyn ProbeOwner>,
}

impl Probe<'_> {
    /// Runs one probe child, bounded by the timeout: its exit code and,
    /// when `capture`, at most [`PROBE_OUTPUT_LIMIT`] bytes of stdout; its
    /// stderr is read and discarded. `Ok(None)` when it cannot start or
    /// times out; [`OwnerClosing`] when the owner refuses it, before
    /// anything is spawned.
    ///
    /// The probe is an owned process tree ([`bake_process::OwnedProcess`])
    /// in a group of its own, spawned under the owner's claim, and whether
    /// it exits, times out, or is dropped, its whole tree is killed and
    /// settled: an exited or timed-out probe before this returns, and a
    /// dropped one by a task on the runtime that polled it, which holds
    /// the claim until the tree is gone, so the owner's teardown waits for
    /// it. A probe leaves no process, and no zombie, behind. With no owner
    /// a dropped probe is killed and not awaited.
    async fn run(
        &self,
        invocation: &Invocation,
        args: &[OsString],
        capture: bool,
    ) -> Result<Option<(Option<i32>, Vec<u8>)>, OwnerClosing> {
        if self.timeout.is_zero() {
            return Ok(None);
        }
        let claim = match self.owner {
            Some(owner) => Some(owner.admit().ok_or(OwnerClosing)?),
            None => None,
        };
        let spec = ProcessSpec {
            program: invocation.program.clone(),
            args: invocation.args.iter().chain(args).cloned().collect(),
            cwd: None,
            env: None,
            stdin: false,
        };
        let Ok(mut probe) = OwnedProcess::spawn(&spec, claim).await else {
            return Ok(None);
        };
        let stdout = probe.take_stdout();
        let stderr = probe.take_stderr();
        let ran = tokio::time::timeout(self.timeout, async {
            let output = async {
                let mut output = Vec::new();
                if let Some(stdout) = stdout {
                    if capture {
                        let _ = stdout
                            .take(PROBE_OUTPUT_LIMIT)
                            .read_to_end(&mut output)
                            .await;
                    } else {
                        let mut stdout = stdout;
                        let _ = tokio::io::copy(&mut stdout, &mut tokio::io::sink()).await;
                    }
                }
                output
            };
            let discarded = async {
                if let Some(mut stderr) = stderr {
                    let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
                }
            };
            let (output, ()) = tokio::join!(output, discarded);
            probe.wait_exit().await.ok().map(|()| output)
        })
        .await;
        // Exited or not, nothing the probe started may outlive it.
        probe.kill();
        let Ok(settled) = probe.settle().await else {
            return Ok(None);
        };
        Ok(ran
            .ok()
            .flatten()
            .map(|output| (settled.status.code(), output)))
    }
}

#[cfg(test)]
mod tests;
