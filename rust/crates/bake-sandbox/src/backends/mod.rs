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
//! | [`landlock`] | the Landlock helper `bake-rs` re-executes itself as |
//! | [`windows_acl`] | the Windows restricted-token runner `bake-rs` re-executes itself as |
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
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::sync::OnceCell;

use crate::diagnostics::RunnerFailureRule;
use crate::mode::SandboxMode;
use crate::paths::{canonical_path, is_path_under};
use crate::policy::SandboxPolicy;
use crate::provider::{
    ConfineFuture, ConfinedCommand, Enforcement, SandboxProvider, SandboxUnavailable,
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
/// caching. A probe is bounded by the probe timeout, and dropping the
/// confine future kills it.
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
    /// capabilities beyond the workspace and its private temp directory.
    /// They are validated on each wrap, and again by the runner, and a wrap
    /// whose grants fail is refused. The default grants nothing.
    pub fn with_windows_appcontainer_grants(
        mut self,
        grants: windows_appcontainer::AppContainerGrants,
    ) -> Self {
        self.appcontainer_grants = grants;
        self
    }

    /// Bounds each functional probe by `timeout`. A zero timeout fails every
    /// probe, so a multi-candidate chain refuses.
    pub fn with_probe_timeout(mut self, timeout: Duration) -> Self {
        self.probe_timeout = timeout;
        self
    }

    /// The selected runner and its enforcement, walking the chain if no call
    /// has yet; `None` when nothing is usable.
    pub async fn selected(&self) -> Option<(Runner, Enforcement)> {
        match self.verdict().await {
            Ok(Verdict::Selected(runner, enforcement)) => Some((runner, enforcement)),
            _ => None,
        }
    }

    async fn verdict(&self) -> Result<Verdict, Option<String>> {
        if let Some(verdict) = self.verdict.get() {
            return Ok(*verdict);
        }
        if self.chain.len() > 1 && tokio::runtime::Handle::try_current().is_err() {
            return Err(Some(
                "probing a sandbox runner needs a Tokio runtime".to_owned(),
            ));
        }
        Ok(*self.verdict.get_or_init(|| self.chain_verdict()).await)
    }

    /// Walks the chain (`chainVerdict`): a sole candidate unprobed, several
    /// probed in order, none usable is unavailable.
    async fn chain_verdict(&self) -> Verdict {
        match self.chain.as_slice() {
            [] => Verdict::Unavailable,
            [sole] => Verdict::Selected(*sole, sole.static_enforcement()),
            chain => {
                for runner in chain {
                    if let Some(enforcement) = self.probe(*runner).await {
                        return Verdict::Selected(*runner, enforcement);
                    }
                }
                Verdict::Unavailable
            }
        }
    }

    /// One rung's functional probe (`probeRunner`): `None` when unusable.
    async fn probe(&self, runner: Runner) -> Option<Enforcement> {
        let probe_policy = SandboxPolicy::new(SandboxMode::ReadOnly, "/");
        match runner {
            Runner::Bwrap => {
                let bwrap = self.bwrap.as_ref()?;
                let mut args = profiles::bwrap_profile_args(&probe_policy);
                args.extend(["--".into(), "true".into()]);
                let (code, _) = run_probe(bwrap, &args, self.probe_timeout, false).await?;
                (code == Some(0)).then_some(Enforcement::Full)
            }
            Runner::Landlock => {
                let helper = self.landlock.as_ref()?;
                let args = [OsString::from("--probe")];
                let (code, stdout) = run_probe(helper, &args, self.probe_timeout, true).await?;
                if code != Some(0) {
                    return None;
                }
                // Only the contract's exact report line counts: a program
                // that merely exits 0 is not a Landlock helper.
                match stdout.as_slice() {
                    out if out == format!("{}\n", landlock::PROBE_FULL_LINE).as_bytes() => {
                        Some(Enforcement::Full)
                    }
                    out if out == format!("{}\n", landlock::PROBE_PARTIAL_LINE).as_bytes() => {
                        Some(Enforcement::Partial)
                    }
                    _ => None,
                }
            }
            Runner::Seatbelt => {
                let mut args = profiles::seatbelt_profile_args(&probe_policy).ok()?;
                args.extend(["--".into(), "true".into()]);
                let (code, _) = run_probe(&self.seatbelt, &args, self.probe_timeout, false).await?;
                (code == Some(0)).then_some(Enforcement::Full)
            }
            Runner::WindowsAcl => {
                // TypeScript's probe: a read-only run, which grants nothing,
                // around `cmd /c exit 0`. Only an override chain probes it.
                let runner = self.windows_acl.as_ref()?;
                let temp = canonical_path(&std::env::temp_dir());
                let mut args = windows_acl::runner_profile_args(
                    windows_acl::RunnerMode::ReadOnly,
                    &temp,
                    &temp,
                );
                args.extend(["--", "cmd", "/c", "exit", "0"].map(OsString::from));
                let (code, _) = run_probe(runner, &args, self.probe_timeout, false).await?;
                (code == Some(0)).then_some(Enforcement::Partial)
            }
            // Never probed: a real launch would create a standing profile
            // and workspace ACE for the probe's own directory. The rung is
            // selectable only as a sole candidate, whose launch failure is
            // the fail-closed end.
            Runner::WindowsAppContainer => None,
        }
    }

    /// The wrapped command for one policy (`confine` and `runnerArgv`).
    async fn wrap(
        &self,
        argv: &[OsString],
        policy: &SandboxPolicy,
    ) -> Result<ConfinedCommand, SandboxUnavailable> {
        let refuse = |detail: Option<String>| SandboxUnavailable::new(policy.mode, detail);
        let (runner, enforcement) = match self.verdict().await.map_err(refuse)? {
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
                    windows_appcontainer_profile(policy, &self.appcontainer_grants)
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

    fn confine<'a>(&'a self, argv: &'a [OsString], policy: &'a SandboxPolicy) -> ConfineFuture<'a> {
        Box::pin(self.wrap(argv, policy))
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
/// temporary directory is never granted; the temp root must lie outside the
/// workspace under both modes, because even a read-only run leaves the
/// package's standing read ACE on the workspace, which every later private
/// directory would inherit. The grants pass their path and disk checks
/// against this host's locations before the wrap is returned.
fn windows_appcontainer_profile(
    policy: &SandboxPolicy,
    grants: &windows_appcontainer::AppContainerGrants,
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
            "the Windows AppContainer runner grants only the workspace and a private temporary directory; extra writable roots are not supported"
                .to_owned(),
        );
    }
    let temp_root = canonical_path(&std::env::temp_dir());
    let workspace = crate::paths::normalize_lexically(&policy.workspace_root);
    windows_acl::check_temp_root_outside_workspace(&workspace, &temp_root)?;
    grants
        .validate_shape(
            &workspace,
            &temp_root,
            &windows_appcontainer::HostRoots::from_env(),
        )
        .and_then(|()| grants.check_on_disk())
        .map_err(|error| error.to_string())?;
    Ok(windows_appcontainer::runner_profile_args(
        mode, &workspace, &temp_root, grants,
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

/// Runs one probe child, bounded by `timeout`: its exit code and, when
/// `capture`, at most [`PROBE_OUTPUT_LIMIT`] bytes of stdout. `None` when it
/// cannot start or times out. A timed-out child is killed and reaped before
/// this returns; a dropped probe kills its child.
async fn run_probe(
    invocation: &Invocation,
    args: &[OsString],
    timeout: Duration,
    capture: bool,
) -> Option<(Option<i32>, Vec<u8>)> {
    if timeout.is_zero() {
        return None;
    }
    let mut command = tokio::process::Command::new(&invocation.program);
    command
        .args(&invocation.args)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(if capture {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .kill_on_drop(true);
    let mut child = command.spawn().ok()?;
    let stdout = child.stdout.take();
    let outcome = tokio::time::timeout(timeout, async {
        let mut output = Vec::new();
        if let Some(stdout) = stdout {
            let _ = stdout
                .take(PROBE_OUTPUT_LIMIT)
                .read_to_end(&mut output)
                .await;
        }
        let status = child.wait().await.ok()?;
        Some((status.code(), output))
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(_) => {
            let _ = child.kill().await;
            None
        }
    }
}

#[cfg(test)]
mod tests;
