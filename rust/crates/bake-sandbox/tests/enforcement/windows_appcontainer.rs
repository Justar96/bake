//! Real effects through the unqualified AppContainer runner
//! (`backends::windows_appcontainer`), reached through an explicit chain:
//! the default provider still refuses on Windows
//! ([`super::the_default_provider_refuses`]).
//!
//! Every case decides allowed and denied effects from the disk or the
//! process table as this unconfined process sees it, beside an unconfined
//! control where a denial could otherwise come from the fixture. The
//! confined program is a copy of this binary in a read root the run grants
//! ([`Fixture`]), because a container cannot read Cargo's target directory;
//! that copy is the executable read dependency every case exercises.
//!
//! What is asserted here is the runner's contract on the host that runs it,
//! not a release qualification. Compatibility facts the owner needs (a
//! shell, PowerShell, `git`, a remote connection) are printed as
//! `appcontainer-observation:` lines and never asserted, because they depend
//! on the host and its network. The hard-link boundary, a qualification
//! blocker, is measured from the disk and printed as
//! `appcontainer-blocker:` lines for the same reason.
//!
//! Each case owns its roots ([`user_scratch`], removed on drop) and its
//! processes: every runner it starts is a [`Started`], owned through
//! `bake_process::OwnedProcess` in a Job that nests the runner's own, and
//! settled only when that Job's account shows every member ended; pids it
//! learns from a handshake are pinned as [`OwnedPid`]s. Every run creates its own AppContainer profile under
//! `%LOCALAPPDATA%\Packages`. Each case's runs pass its [`Fixture`]'s
//! random [`ProfileTag`], so their profiles are named under it and no other
//! process's profile is listed, counted, or deleted here; the cases still
//! run concurrently, with each other and with anything else on the host.
//!
//! [`a_shared_profile_shares_its_temp_directory`] keeps the evidence that
//! ruled out a profile shared between runs: under one profile, `TEMP` is
//! the profile's own folder for every process.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bake_sandbox::backends::windows_appcontainer::{
    self, AppContainerGrants, HELPER_ARG, NetworkGrant, PROFILE_WRITE_MASK, ProfileTag,
    READ_EXECUTE, WORKSPACE_WRITE_MASK,
};
use bake_sandbox::backends::{LocalProvider, Runner};
use bake_sandbox::{ConfinedCommand, Enforcement, SandboxMode, SandboxPolicy};

use super::mechanisms::{acl, seed_without_delete, token};
use super::{
    BOUND, DENIED, OwnedPid, PROBE_ARG, Run, Scratch, assert_denied, assert_ok, harness, is_image,
    mklink_junction, runtime, signaled_within, text, user_scratch, win32,
};

pub fn tests() -> Vec<harness::Test> {
    let scenarios: &[(&str, fn())] = &[
        (
            "an_explicit_chain_selects_the_unqualified_runner",
            an_explicit_chain_selects_the_unqualified_runner,
        ),
        (
            "read_only_reads_the_workspace_and_denies_effects",
            read_only_reads_the_workspace_and_denies_effects,
        ),
        (
            "workspace_write_allows_effects_inside",
            workspace_write_allows_effects_inside,
        ),
        (
            "workspace_write_denies_effects_outside",
            workspace_write_denies_effects_outside,
        ),
        (
            "read_grants_are_per_root_read_only_and_standing",
            read_grants_are_per_root_read_only_and_standing,
        ),
        (
            "each_run_gets_a_private_temp_directory",
            each_run_gets_a_private_temp_directory,
        ),
        (
            "read_only_profile_storage_denies_every_mutation",
            read_only_profile_storage_denies_every_mutation,
        ),
        ("junction_escapes_are_denied", junction_escapes_are_denied),
        (
            "descendants_end_with_the_command_and_cannot_break_away",
            descendants_end_with_the_command_and_cannot_break_away,
        ),
        (
            "a_killed_runner_takes_its_tree",
            a_killed_runner_takes_its_tree,
        ),
        (
            "loopback_stays_closed_with_and_without_network_grants",
            loopback_stays_closed_with_and_without_network_grants,
        ),
        (
            "runner_failures_grant_nothing_and_clean_up",
            runner_failures_grant_nothing_and_clean_up,
        ),
        (
            "exit_codes_and_output_pass_through",
            exit_codes_and_output_pass_through,
        ),
        (
            "hard_link_aliases_are_measured",
            hard_link_aliases_are_measured,
        ),
        (
            "a_shared_profile_shares_its_temp_directory",
            a_shared_profile_shares_its_temp_directory,
        ),
        (
            "concurrent_fixtures_see_only_their_own_profiles",
            concurrent_fixtures_see_only_their_own_profiles,
        ),
        ("compatibility_observations", compatibility_observations),
    ];
    scenarios
        .iter()
        .map(|(name, scenario)| {
            let scenario = *scenario;
            harness::Test::new(format!("windows-appcontainer::{name}"), scenario)
        })
        .collect()
}

/// A copy of this binary in a root the run grants for reading and
/// execution, the confined program of every case, and the case's own
/// profile namespace.
///
/// Every run a fixture starts passes its [`ProfileTag`], 16 hex digits
/// minted here from the OS random source, so each run profile's name is
/// `bake.run.<tag>.<32 random hex>`. Only this fixture's runs use the tag,
/// and the runner creates every profile exclusively, so a profile named
/// under the tag is one this fixture's runner created: other test
/// processes, CI jobs, and Bake itself never use it, and their profiles are
/// never listed, counted, or deleted here. Where a run reports its
/// profile, cases also check the exact name.
///
/// Every runner it starts is a [`Started`], owned with its whole tree.
/// Created first in each case, the fixture is dropped last, after every
/// `Started` has settled. It then fails the case if a profile under its tag
/// remains, and deletes each one, assertion failures included, but only
/// when every run's tree was shown ended; otherwise it keeps them and
/// reports them, never removing storage under a member that may live.
struct Fixture {
    bin: Scratch,
    probe: PathBuf,
    tag: ProfileTag,
    /// Set by a [`Started`] whose tree's end was not shown.
    unsettled: Arc<AtomicBool>,
}

impl Fixture {
    fn new() -> Self {
        let mut random = [0_u8; 8];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut random)
            .expect("random profile tag");
        let hex: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let tag = ProfileTag::parse(&hex).expect("16 lowercase hex digits");
        let bin = user_scratch("ac-bin");
        let probe = bin.join("probe.exe");
        std::fs::copy(std::env::current_exe().expect("exe"), &probe)
            .expect("copy the test binary into the read root");
        Self {
            bin,
            probe,
            tag,
            unsettled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The bin root, then `reads`, readable; `network` granted.
    fn grants(&self, reads: &[&Path], network: &[NetworkGrant]) -> AppContainerGrants {
        let mut grants = AppContainerGrants::new().with_read_root(self.bin.0.clone());
        for root in reads {
            grants = grants.with_read_root(root.to_path_buf());
        }
        for grant in network {
            grants = grants.with_network(*grant);
        }
        grants
    }

    fn provider(&self, reads: &[&Path], network: &[NetworkGrant]) -> LocalProvider {
        LocalProvider::with_chain(vec![Runner::WindowsAppContainer])
            .with_windows_appcontainer_grants(self.grants(reads, network))
            .with_windows_appcontainer_profile_tag(self.tag.clone())
    }

    /// `[probe, PROBE_ARG, ops...]`.
    fn argv(&self, ops: &[&str]) -> Vec<String> {
        let mut argv = vec![text(&self.probe), PROBE_ARG.to_owned()];
        argv.extend(ops.iter().map(|op| (*op).to_owned()));
        argv
    }

    /// Runs `ops` confined under `policy` with only the bin root granted.
    fn run(&self, policy: &SandboxPolicy, ops: &[&str]) -> Run {
        self.run_with(policy, &[], &[], ops)
    }

    fn run_with(
        &self,
        policy: &SandboxPolicy,
        reads: &[&Path],
        network: &[NetworkGrant],
        ops: &[&str],
    ) -> Run {
        let argv = self.argv(ops);
        self.run_argv(&self.provider(reads, network), &argv, policy)
    }

    /// Confines `argv` through `provider` under `policy` and runs it in the
    /// workspace to its end.
    fn run_argv(&self, provider: &LocalProvider, argv: &[String], policy: &SandboxPolicy) -> Run {
        let (command, started) = self.start_with(provider, argv, policy, false);
        started.output(command)
    }

    /// Confines `ops` under `policy`, with only the bin root granted, and
    /// starts it in the workspace, stdin piped when `stdin`.
    fn start(&self, policy: &SandboxPolicy, ops: &[&str], stdin: bool) -> Started {
        self.start_with(&self.provider(&[], &[]), &self.argv(ops), policy, stdin)
            .1
    }

    fn start_with(
        &self,
        provider: &LocalProvider,
        argv: &[String],
        policy: &SandboxPolicy,
        stdin: bool,
    ) -> (ConfinedCommand, Started) {
        let argv: Vec<OsString> = argv.iter().map(OsString::from).collect();
        let command = runtime()
            .block_on(bake_sandbox::SandboxProvider::confine(
                provider, &argv, policy,
            ))
            .expect("confine");
        let env = (!command.env.is_empty()).then(|| {
            std::env::vars_os()
                .chain(command.env.iter().cloned())
                .collect()
        });
        let spec = bake_process::ProcessSpec {
            program: command.program.clone(),
            args: command.args.clone(),
            cwd: Some(policy.workspace_root.clone()),
            env,
            stdin,
        };
        let started = Started::spawn(&spec, self.unsettled.clone());
        (command, started)
    }

    /// The runner's own arguments for a direct start: `--profile-tag` and
    /// this fixture's tag.
    fn tag_args(&self) -> [String; 2] {
        ["--profile-tag".to_owned(), self.tag.as_str().to_owned()]
    }

    /// The runner started directly in `cwd` with this fixture's tag, `args`
    /// before `--`, and `ops` after. The command inherits `cwd`, which its
    /// container must be able to read, so callers pass the workspace.
    fn start_runner(&self, cwd: &Path, args: &[String], ops: &[&str], stdin: bool) -> Started {
        self.start_runner_command(cwd, args, &self.argv(ops), stdin)
    }

    /// The runner started directly in `cwd` with this fixture's tag, `args`
    /// and the bin root's read grant before `--`, and `command` after.
    fn start_runner_command(
        &self,
        cwd: &Path,
        args: &[String],
        command: &[String],
        stdin: bool,
    ) -> Started {
        let mut argv: Vec<OsString> = vec![HELPER_ARG.into()];
        argv.extend(args.iter().map(OsString::from));
        argv.extend(self.tag_args().iter().map(OsString::from));
        argv.extend([
            "--read".into(),
            self.bin.0.clone().into_os_string(),
            "--".into(),
        ]);
        argv.extend(command.iter().map(OsString::from));
        let spec = bake_process::ProcessSpec {
            program: std::env::current_exe().expect("exe").into_os_string(),
            args: argv,
            cwd: Some(cwd.to_path_buf()),
            env: None,
            stdin,
        };
        Started::spawn(&spec, self.unsettled.clone())
    }

    /// [`Self::start_runner`] run to its end: its exit code and output.
    fn runner(&self, cwd: &Path, args: &[String], ops: &[&str]) -> Ended {
        let ended = self.start_runner(cwd, args, ops, false).settle();
        assert!(
            ended.code.is_some(),
            "the runner did not finish: {}",
            ended.stderr
        );
        ended
    }

    /// The profiles under this fixture's tag: those its runs created and
    /// have not deleted. Empty when `Packages` does not exist.
    fn profiles(&self) -> BTreeSet<String> {
        let prefix = self.tag.moniker_prefix();
        let packages = packages_dir();
        let listing = match std::fs::read_dir(&packages) {
            Ok(listing) => listing,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return BTreeSet::new(),
            Err(error) => panic!("cannot inspect {}: {error}", packages.display()),
        };
        listing
            .filter_map(|entry| {
                let name = entry.expect("a Packages entry").file_name();
                name.to_str()
                    .filter(|name| name.to_ascii_lowercase().starts_with(&prefix))
                    .map(str::to_owned)
            })
            .collect()
    }

    /// Checks that `name` is one of this fixture's run profile names,
    /// `bake.run.<tag>.<32 hex>`, and returns it.
    fn own<'a>(&self, name: &'a str) -> &'a str {
        let random = name
            .strip_prefix(&self.tag.moniker_prefix())
            .unwrap_or_else(|| panic!("{name} is not a profile of this fixture's runs"));
        assert!(
            random.len() == 32 && random.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "{name}"
        );
        name
    }

    /// Deletes profile `name`, which must be one of this fixture's own.
    fn delete_profile(&self, name: &str) -> Result<(), String> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Security::Isolation::DeleteAppContainerProfile;
        let name = self.own(name);
        let units: Vec<u16> = std::ffi::OsStr::new(name)
            .encode_wide()
            .chain([0])
            .collect();
        // SAFETY: a NUL-terminated profile name.
        let status = unsafe { DeleteAppContainerProfile(units.as_ptr()) };
        if status < 0 {
            return Err(format!(
                "DeleteAppContainerProfile({name}): HRESULT {status:#010x}"
            ));
        }
        Ok(())
    }

    /// The run profile whose `LOCALAPPDATA` a confined command reported:
    /// `<Packages>\bake.run.<tag>.<32 hex>\AC`, with [`packages_dir`] as its
    /// parent's parent, named under this fixture's tag. Returns the profile
    /// root and its name.
    fn run_profile(&self, local: &str) -> (PathBuf, String) {
        let local = PathBuf::from(local.trim());
        let root = local.parent().expect("the profile root").to_path_buf();
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .expect("a profile name")
            .to_owned();
        self.own(&name);
        assert_eq!(
            windows_appcontainer::profile_root_for(&local, &name).as_deref(),
            Some(root.as_path()),
            "LOCALAPPDATA {} is not the profile's AC folder",
            local.display()
        );
        assert_eq!(
            folded(root.parent().expect("Packages")),
            folded(&packages_dir()),
            "{}",
            root.display()
        );
        (root, name)
    }

    /// The profile root of one confined `mechanism env-pair` run, after
    /// checking that its `TEMP` and `TMP` are that profile's `AC\Temp` and
    /// its `LOCALAPPDATA` the profile's `AC`.
    fn run_temp(&self, run: &Run) -> PathBuf {
        assert_ok(run, "env-pair");
        let mut lines = run.stdout.lines();
        let (Some(temp), Some(tmp), Some(local)) = (lines.next(), lines.next(), lines.next())
        else {
            panic!("env-pair output {:?}", run.stdout);
        };
        let (root, _) = self.run_profile(local);
        assert_eq!(temp, tmp, "TEMP and TMP");
        assert_eq!(
            folded(Path::new(temp)),
            folded(&root.join("AC").join("Temp")),
            "TEMP is not the run profile's AC\\Temp"
        );
        root
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let left = self.profiles();
        if self.unsettled.load(Ordering::SeqCst) {
            // A run's tree was not shown ended: its profile may still be in
            // use, so nothing is deleted.
            let error = format!(
                "a run's tree was not shown ended; profiles under tag {} kept: {left:?}",
                self.tag
            );
            if std::thread::panicking() {
                eprintln!("{error}");
                return;
            }
            panic!("{error}");
        }
        let failures: Vec<String> = left
            .iter()
            .filter_map(|name| self.delete_profile(name).err())
            .collect();
        if left.is_empty() {
            return;
        }
        let error = format!(
            "runs left AppContainer profiles under tag {}, now deleted: {left:?} {failures:?}",
            self.tag
        );
        if std::thread::panicking() {
            eprintln!("{error}");
        } else {
            panic!("{error}");
        }
    }
}

/// One runner process a [`Fixture`] started, owned through
/// `bake_process::OwnedProcess`. It is born in a kill-on-close Job of its
/// own, which nests the runner's Job, so every process of the run, the
/// confined command and its descendants included, is a member of it.
///
/// Settling it ([`Started::settle`], or its drop, a panic included) waits
/// within [`BOUND`] for the runner's own exit and the pipes' end, then kills
/// that Job and waits within [`BOUND`] until its account shows every member
/// ended (`TreeState::Gone`): observed, never assumed from a kill. When that
/// cannot be shown, it marks its fixture, which then keeps every profile
/// under its tag instead of deleting storage a member may still use. Its
/// pipes are read on its own current-thread runtime; no reader task or
/// thread outlives it.
struct Started {
    runtime: tokio::runtime::Runtime,
    process: Option<bake_process::OwnedProcess>,
    stdin: Option<bake_process::ProcessStdin>,
    stdout: Option<tokio::io::BufReader<bake_process::ProcessStdout>>,
    stderr: Option<bake_process::ProcessStderr>,
    unsettled: Arc<AtomicBool>,
}

/// What a settled [`Started`] reported: its exit code when it exited by
/// itself within the bound, and the rest of its output.
struct Ended {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// The most one output stream a case reads keeps.
const OUTPUT_CAP: u64 = 1024 * 1024;

impl Started {
    fn spawn(spec: &bake_process::ProcessSpec, unsettled: Arc<AtomicBool>) -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let mut process = runtime
            .block_on(async {
                tokio::time::timeout(BOUND, bake_process::OwnedProcess::spawn(spec, None)).await
            })
            .expect("spawn within the bound")
            .expect("spawn the runner");
        Self {
            stdin: process.take_stdin(),
            stdout: process.take_stdout().map(tokio::io::BufReader::new),
            stderr: process.take_stderr(),
            process: Some(process),
            runtime,
            unsettled,
        }
    }

    /// The runner's pid, pinned while this owns it.
    fn pid(&self) -> u32 {
        self.process.as_ref().expect("owned").pid()
    }

    /// The next stdout line, without its line break, within [`BOUND`].
    /// Panics at EOF or the bound, after settling.
    fn line(&mut self) -> String {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt};
        let stdout = self.stdout.as_mut().expect("piped stdout");
        let mut line = String::new();
        let read = self.runtime.block_on(async {
            tokio::time::timeout(BOUND, stdout.take(OUTPUT_CAP).read_line(&mut line)).await
        });
        match read {
            Ok(Ok(n)) if n > 0 => line.trim_end_matches(['\r', '\n']).to_owned(),
            other => {
                let ended = self.settle();
                panic!(
                    "no line from the run ({other:?}): exit={:?} stdout={:?} stderr={:?}",
                    ended.code, ended.stdout, ended.stderr
                );
            }
        }
    }

    /// Lines up to and including `last`, all within [`BOUND`]. Panics at
    /// EOF or the bound, after settling.
    fn lines_until(&mut self, last: &str) -> Vec<String> {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt};
        let stdout = self.stdout.as_mut().expect("piped stdout");
        let mut lines = Vec::new();
        let read = self.runtime.block_on(async {
            tokio::time::timeout(BOUND, async {
                loop {
                    let mut line = String::new();
                    if stdout.take(OUTPUT_CAP).read_line(&mut line).await? == 0 {
                        return Ok::<bool, std::io::Error>(false);
                    }
                    let line = line.trim_end_matches(['\r', '\n']).to_owned();
                    let done = line == last;
                    lines.push(line);
                    if done {
                        return Ok(true);
                    }
                }
            })
            .await
        });
        if !matches!(read, Ok(Ok(true))) {
            let ended = self.settle();
            panic!(
                "the run did not print {last:?} ({read:?}): lines={lines:?} exit={:?} stderr={:?}",
                ended.code, ended.stderr
            );
        }
        lines
    }

    /// Closes the runner's stdin.
    fn close_stdin(&mut self) {
        let _guard = self.runtime.enter();
        drop(self.stdin.take());
    }

    /// Reads the rest of both streams until EOF, waits for the runner's own
    /// exit, then kills its Job and requires its account to settle, all
    /// within [`BOUND`] each. Panics, after marking the fixture, when the
    /// tree's end is not shown.
    fn settle(&mut self) -> Ended {
        use tokio::io::AsyncReadExt;
        let _guard = self.runtime.enter();
        drop(self.stdin.take());
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let stdout = self.stdout.take();
        let stderr = self.stderr.take();
        let process = self.process.as_mut().expect("not yet settled");
        let (exited, settled) = self.runtime.block_on(async {
            let _ = tokio::time::timeout(BOUND, async {
                let read_out = async {
                    if let Some(stdout) = stdout {
                        let _ = stdout.take(OUTPUT_CAP).read_to_end(&mut out).await;
                    }
                };
                let read_err = async {
                    if let Some(stderr) = stderr {
                        let _ = stderr.take(OUTPUT_CAP).read_to_end(&mut err).await;
                    }
                };
                tokio::join!(read_out, read_err);
            })
            .await;
            let exited = matches!(
                tokio::time::timeout(BOUND, process.wait_exit()).await,
                Ok(Ok(()))
            );
            process.kill();
            (exited, tokio::time::timeout(BOUND, process.finish()).await)
        });
        let ended = |code| Ended {
            code,
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: String::from_utf8_lossy(&err).into_owned(),
        };
        match settled {
            Ok(Ok(settled)) if settled.tree == bake_process::TreeState::Gone => {
                self.process = None;
                ended(exited.then(|| settled.status.code()).flatten())
            }
            other => {
                self.unsettled.store(true, Ordering::SeqCst);
                let ended = ended(None);
                panic!(
                    "the run's Job did not settle within {BOUND:?} ({other:?}): stdout={:?} stderr={:?}",
                    ended.stdout, ended.stderr
                );
            }
        }
    }

    /// [`Self::settle`] for a run that must exit by itself: its result as a
    /// [`Run`] of `command`.
    fn output(mut self, command: ConfinedCommand) -> Run {
        let ended = self.settle();
        assert!(
            ended.code.is_some(),
            "the confined command did not finish within {BOUND:?}: stdout={:?} stderr={:?}",
            ended.stdout,
            ended.stderr
        );
        Run {
            command,
            code: ended.code,
            stdout: ended.stdout,
            stderr: ended.stderr,
        }
    }
}

impl Drop for Started {
    fn drop(&mut self) {
        if self.process.is_none() {
            return;
        }
        let _guard = self.runtime.enter();
        let process = self.process.as_mut().expect("owned");
        drop(self.stdin.take());
        drop(self.stdout.take());
        drop(self.stderr.take());
        process.kill();
        let settled = self
            .runtime
            .block_on(async { tokio::time::timeout(BOUND, process.finish()).await });
        if matches!(&settled, Ok(Ok(settled)) if settled.tree == bake_process::TreeState::Gone) {
            self.process = None;
            return;
        }
        // Dropped here, under the runtime: the owner kills the tree again.
        self.process = None;
        self.unsettled.store(true, Ordering::SeqCst);
        let error = format!("a run's Job did not settle within {BOUND:?}: {settled:?}");
        if std::thread::panicking() {
            eprintln!("{error}");
        } else {
            panic!("{error}");
        }
    }
}

/// `%LOCALAPPDATA%\Packages` as this unconfined process sees it, where
/// Windows keeps AppContainer profile storage.
fn packages_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA")).join("Packages")
}

/// Lowercase canonical text, for comparing Windows paths that may differ
/// in case or 8.3 spelling.
fn folded(path: &Path) -> String {
    text(&bake_sandbox::canonical_path(path)).to_ascii_lowercase()
}

fn ww(workspace: &Scratch) -> SandboxPolicy {
    SandboxPolicy::new(SandboxMode::WorkspaceWrite, &workspace.0)
}

fn ro(workspace: &Scratch) -> SandboxPolicy {
    SandboxPolicy::new(SandboxMode::ReadOnly, &workspace.0)
}

fn observe(fields: std::fmt::Arguments<'_>) {
    eprintln!("appcontainer-observation: {fields}");
}

/// `root`'s DACL as read back.
fn dacl(root: &Path) -> Vec<acl::Ace> {
    acl::Descriptor::read(root)
        .unwrap_or_else(|error| panic!("read the DACL of {}: {error}", root.display()))
        .dacl
}

/// The ACEs in `after` that are not in `before`, counted as a multiset; it
/// panics if `after` lost any ACE of `before`.
fn added(before: &[acl::Ace], after: &[acl::Ace], root: &Path) -> Vec<acl::Ace> {
    let mut extra = after.to_vec();
    for ace in before {
        let index = extra
            .iter()
            .position(|kept| kept == ace)
            .unwrap_or_else(|| panic!("{} lost {ace:?}: now {after:?}", root.display()));
        extra.swap_remove(index);
    }
    extra
}

fn explicit(mask: u32, sid_prefix: &str, ace: &acl::Ace) -> bool {
    ace.kind == acl::ACCESS_ALLOWED
        && ace.flags == acl::OI_CI
        && ace.mask == mask
        && ace.sid.starts_with(sid_prefix)
}

// ---------------------------------------------------------------------------
// Scenarios.

fn an_explicit_chain_selects_the_unqualified_runner() {
    let fixture = Fixture::new();
    let provider = fixture.provider(&[], &[]);
    assert_eq!(
        runtime().block_on(provider.selected()),
        Some((Runner::WindowsAppContainer, Enforcement::Partial))
    );
    let workspace = user_scratch("ac-ws");
    let run = fixture.run(&ro(&workspace), &["mechanism", "identity"]);
    assert_ok(&run, "identity");
    assert!(
        run.stdout.contains("app_container=1"),
        "the command must run as an AppContainer: {}",
        run.stdout
    );
    assert_eq!(run.command.enforcement, Enforcement::Partial);
    assert_eq!(
        run.command.args.first().map(OsString::as_os_str),
        Some(HELPER_ARG.as_ref())
    );
}

fn read_only_reads_the_workspace_and_denies_effects() {
    let fixture = Fixture::new();
    let (workspace, outside) = (user_scratch("ac-ws"), user_scratch("ac-out"));
    let policy = ro(&workspace);
    let inside = workspace.write("readme.txt", "inside");
    let run = fixture.run(&policy, &["read", &text(&inside)]);
    assert_ok(&run, "read the workspace");
    assert_eq!(run.stdout, "inside");
    let created = workspace.join("new.txt");
    assert_denied(
        &fixture.run(&policy, &["write", &text(&created), "x"]),
        "create in the workspace",
    );
    assert!(!created.exists());
    assert_denied(
        &fixture.run(&policy, &["write", &text(&inside), "x"]),
        "overwrite in the workspace",
    );
    assert_denied(
        &fixture.run(&policy, &["win32-delete", "DeleteFileW", &text(&inside)]),
        "delete in the workspace",
    );
    assert_eq!(std::fs::read_to_string(&inside).expect("kept"), "inside");
    let foreign = outside.write("readme.txt", "outside");
    assert_denied(
        &fixture.run(&policy, &["read", &text(&foreign)]),
        "read outside every grant",
    );
    // The control: unconfined, this user reads it.
    assert_eq!(
        std::fs::read_to_string(&foreign).expect("control"),
        "outside"
    );
    // TEMP, TMP, and LOCALAPPDATA are this run's own profile storage, which
    // is gone once the run has ended.
    let root = fixture.run_temp(&fixture.run(&policy, &["mechanism", "env-pair"]));
    assert!(!root.exists(), "{} must be deleted", root.display());
    // That storage is read-only: a write into TEMP is denied. The file
    // would land in the run's own profile, never in a host directory.
    let run = fixture.run(&policy, &["appcontainer", "write-profile-temp"]);
    assert_denied(&run, "read-only writes its TEMP");
}

fn workspace_write_allows_effects_inside() {
    let fixture = Fixture::new();
    let workspace = user_scratch("ac-ws");
    let before = dacl(&workspace.0);
    let policy = ww(&workspace);
    let created = workspace.join("created.txt");
    assert_ok(
        &fixture.run(&policy, &["write", &text(&created), "made"]),
        "create inside",
    );
    assert_eq!(std::fs::read_to_string(&created).expect("made"), "made");
    let renamed = workspace.join("renamed.txt");
    assert_ok(
        &fixture.run(&policy, &["rename", &text(&created), &text(&renamed)]),
        "rename inside",
    );
    assert!(!created.exists() && renamed.exists());
    let dir = workspace.join("sub");
    assert_ok(
        &fixture.run(&policy, &["mkdir", &text(&dir)]),
        "mkdir inside",
    );
    assert!(dir.is_dir());
    assert_ok(
        &fixture.run(&policy, &["win32-delete", "DeleteFileW", &text(&renamed)]),
        "delete inside",
    );
    assert!(!renamed.exists());
    assert_ok(
        &fixture.run(
            &policy,
            &["spawn", "write", &text(&workspace.join("child.txt")), "x"],
        ),
        "a descendant writes inside",
    );
    assert!(workspace.join("child.txt").exists());
    // The standing grants: every earlier ACE kept, plus the read and write
    // capabilities' ACEs, explicit and inheritable. No package SID is
    // granted: each run has its own.
    let extra = added(&before, &dacl(&workspace.0), &workspace.0);
    assert_eq!(extra.len(), 2, "{extra:?}");
    assert!(
        extra
            .iter()
            .any(|ace| explicit(READ_EXECUTE, "S-1-15-3-", ace)),
        "{extra:?}"
    );
    assert!(
        extra
            .iter()
            .any(|ace| explicit(WORKSPACE_WRITE_MASK, "S-1-15-3-", ace)),
        "{extra:?}"
    );
    // Repeated runs reuse them.
    assert_ok(&fixture.run(&policy, &["exit", "0"]), "a second run");
    assert_eq!(added(&before, &dacl(&workspace.0), &workspace.0).len(), 2);
}

fn workspace_write_denies_effects_outside() {
    let fixture = Fixture::new();
    let (workspace, outside) = (user_scratch("ac-ws"), user_scratch("ac-out"));
    let policy = ww(&workspace);
    let user = win32::user_sid().expect("this process's user SID");
    let written = outside.join("new.txt");
    assert_denied(
        &fixture.run(&policy, &["write", &text(&written), "x"]),
        "create outside",
    );
    assert!(!written.exists());
    let victim = outside.write("victim.txt", "keep");
    assert_denied(
        &fixture.run(&policy, &["win32-delete", "DeleteFileW", &text(&victim)]),
        "delete outside",
    );
    assert_denied(
        &fixture.run(&policy, &["win32-delete", "posix", &text(&victim)]),
        "POSIX disposition outside",
    );
    assert_eq!(std::fs::read_to_string(&victim).expect("kept"), "keep");
    // The parent route: the file grants no DELETE, the parent grants
    // FILE_DELETE_CHILD; unconfined, a control copy is deletable.
    let control = seed_without_delete(&outside, "control.txt", &user);
    assert!(
        super::unconfined(&["win32-delete", "DeleteFileW", &text(&control)])
            .status
            .success()
            && !control.exists(),
        "the parent-route control must delete unconfined"
    );
    let parent_route = seed_without_delete(&outside, "parent.txt", &user);
    assert_denied(
        &fixture.run(
            &policy,
            &["win32-delete", "DeleteFileW", &text(&parent_route)],
        ),
        "delete outside through the parent",
    );
    assert!(parent_route.exists());
    // Renames in both directions.
    let into = workspace.join("moved-in.txt");
    assert_denied(
        &fixture.run(&policy, &["rename", &text(&victim), &text(&into)]),
        "rename outside → workspace",
    );
    assert!(victim.exists() && !into.exists());
    let own = workspace.write("own.txt", "own");
    let out = outside.join("moved-out.txt");
    assert_denied(
        &fixture.run(&policy, &["rename", &text(&own), &text(&out)]),
        "rename workspace → outside",
    );
    assert!(own.exists() && !out.exists());
    // Another workspace, holding its own standing grants after a run there.
    let foreign = user_scratch("ac-foreign");
    assert_ok(
        &fixture.run(&ww(&foreign), &["exit", "0"]),
        "a run in the other workspace",
    );
    let theirs = foreign.write("theirs.txt", "theirs");
    assert_denied(
        &fixture.run(&policy, &["win32-delete", "DeleteFileW", &text(&theirs)]),
        "delete in another workspace",
    );
    assert_denied(
        &fixture.run(&policy, &["write", &text(&theirs), "x"]),
        "write in another workspace",
    );
    assert_eq!(std::fs::read_to_string(&theirs).expect("kept"), "theirs");
    // Descendants stay confined.
    let child = outside.join("child.txt");
    let run = fixture.run(&policy, &["spawn", "write", &text(&child), "x"]);
    assert_eq!(run.code, Some(DENIED), "{}", run.stderr);
    assert!(!child.exists());
}

fn read_grants_are_per_root_read_only_and_standing() {
    let fixture = Fixture::new();
    let workspace = user_scratch("ac-ws");
    let (granted, other) = (user_scratch("ac-read"), user_scratch("ac-other"));
    let doc = granted.write("doc.txt", "granted");
    let hidden = other.write("doc.txt", "other");
    let before = dacl(&granted.0);
    let other_before = dacl(&other.0);
    let policy = ww(&workspace);
    let run = fixture.run_with(&policy, &[&granted.0], &[], &["read", &text(&doc)]);
    assert_ok(&run, "read a granted root");
    assert_eq!(run.stdout, "granted");
    assert_denied(
        &fixture.run_with(&policy, &[&granted.0], &[], &["read", &text(&hidden)]),
        "read a root this run does not grant",
    );
    for ops in [
        vec!["write".to_owned(), text(&doc), "x".to_owned()],
        vec![
            "win32-delete".to_owned(),
            "DeleteFileW".to_owned(),
            text(&doc),
        ],
        vec![
            "write".to_owned(),
            text(&granted.join("new.txt")),
            "x".to_owned(),
        ],
    ] {
        let ops: Vec<&str> = ops.iter().map(String::as_str).collect();
        assert_denied(
            &fixture.run_with(&policy, &[&granted.0], &[], &ops),
            "an effect in a read root",
        );
    }
    assert_eq!(std::fs::read_to_string(&doc).expect("kept"), "granted");
    assert!(!granted.join("new.txt").exists());
    // A standing grant on that root does not reach a run that omits it.
    assert_denied(
        &fixture.run(&policy, &["read", &text(&doc)]),
        "read a previously granted root without its grant",
    );
    // One explicit read ACE, every earlier ACE kept, and none added again.
    let after = dacl(&granted.0);
    let extra = added(&before, &after, &granted.0);
    assert_eq!(extra.len(), 1, "{extra:?}");
    assert!(explicit(READ_EXECUTE, "S-1-15-3-", &extra[0]), "{extra:?}");
    // An ACE added by someone else between runs survives the next run.
    let unrelated = token::Sid::parse("S-1-5-21-1-2-3-4242").expect("SID");
    acl::grant(&granted.0, &unrelated, READ_EXECUTE).expect("an unrelated grant");
    let with_unrelated = dacl(&granted.0);
    assert_ok(
        &fixture.run_with(&policy, &[&granted.0], &[], &["read", &text(&doc)]),
        "read again",
    );
    assert_eq!(dacl(&granted.0), with_unrelated);
    assert_eq!(
        dacl(&other.0),
        other_before,
        "an ungranted root must be untouched"
    );
}

fn each_run_gets_a_private_temp_directory() {
    let fixture = Fixture::new();
    let workspace = user_scratch("ac-ws");
    let policy = ww(&workspace);
    let mut a = fixture.start(
        &policy,
        &["appcontainer", "write-profile-temp-and-wait"],
        true,
    );
    let held = PathBuf::from(a.line());
    let a_temp = held.parent().expect("A's TEMP").to_path_buf();
    let a_root = a_temp
        .parent()
        .and_then(Path::parent)
        .expect("A's profile root")
        .to_path_buf();
    fixture.run_profile(&text(&a_root.join("AC")));
    assert_eq!(folded(&a_temp), folded(&a_root.join("AC").join("Temp")));
    // The control: this user reads A's file while A still runs.
    assert_eq!(std::fs::read_to_string(&held).expect("A's file"), "held");
    // B, on the same workspace and alive at the same time, holds another
    // package SID and so another profile.
    let intruder = a_temp.join("from-b.txt");
    assert_denied(
        &fixture.run(&policy, &["write", &text(&intruder), "x"]),
        "B creates in A's TEMP",
    );
    assert_denied(
        &fixture.run(&policy, &["read", &text(&held)]),
        "B reads A's file",
    );
    assert_denied(
        &fixture.run(&policy, &["win32-delete", "DeleteFileW", &text(&held)]),
        "B deletes A's file",
    );
    assert!(!intruder.exists());
    assert_eq!(std::fs::read_to_string(&held).expect("kept"), "held");
    let b = fixture.run(&policy, &["spawn", "appcontainer", "write-profile-temp"]);
    assert_ok(&b, "B's descendant writes B's TEMP");
    let b_temp = PathBuf::from(b.stdout.trim())
        .parent()
        .expect("B's TEMP")
        .to_path_buf();
    assert_ne!(folded(&b_temp), folded(&a_temp));
    let b_root = b_temp
        .parent()
        .and_then(Path::parent)
        .expect("B's profile root")
        .to_path_buf();
    fixture.run_profile(&text(&b_root.join("AC")));
    assert!(!b_root.exists(), "{} must be deleted", b_root.display());
    // Under workspace-write, too, TEMP and TMP are the profile's AC\Temp
    // and LOCALAPPDATA its AC.
    let c_root = fixture.run_temp(&fixture.run(&policy, &["mechanism", "env-pair"]));
    assert!(!c_root.exists(), "{} must be deleted", c_root.display());
    assert!(a_root.exists(), "A's profile must outlive B and C");
    a.close_stdin();
    let ended = a.settle();
    assert_eq!(ended.code, Some(0), "A: {}", ended.stderr);
    assert!(!a_root.exists(), "{} must be deleted", a_root.display());
}

fn junction_escapes_are_denied() {
    let fixture = Fixture::new();
    let (workspace, outside) = (user_scratch("ac-ws"), user_scratch("ac-out"));
    let link = workspace.join("escape");
    mklink_junction(&link, &outside.0);
    let outside_before = dacl(&outside.0);
    let policy = ww(&workspace);
    // The first run grants the workspace after the junction exists, so the
    // propagation meets it.
    let through = link.join("new.txt");
    assert_denied(
        &fixture.run(&policy, &["write", &text(&through), "x"]),
        "write through a junction",
    );
    assert!(!outside.join("new.txt").exists());
    let target = outside.write("victim.txt", "keep");
    assert_denied(
        &fixture.run(
            &policy,
            &[
                "win32-delete",
                "DeleteFileW",
                &text(&link.join("victim.txt")),
            ],
        ),
        "delete through a junction",
    );
    assert_denied(
        &fixture.run(&policy, &["read", &text(&link.join("victim.txt"))]),
        "read through a junction",
    );
    assert_eq!(std::fs::read_to_string(&target).expect("kept"), "keep");
    // Direct evidence that propagation stopped at the junction: the
    // target's own DACL gained nothing.
    assert_eq!(
        dacl(&outside.0),
        outside_before,
        "the workspace grant propagated through the junction"
    );
    // The same inside a read root.
    let read = user_scratch("ac-read");
    let inner = read.join("escape");
    mklink_junction(&inner, &outside.0);
    let run = fixture.run_with(
        &ro(&workspace),
        &[&read.0],
        &[],
        &["read", &text(&inner.join("victim.txt"))],
    );
    assert_denied(&run, "read through a junction in a read root");
    assert_eq!(
        dacl(&outside.0),
        outside_before,
        "the read grant propagated through the junction"
    );
    // A read root that is itself a junction is refused before any grant.
    let parent = user_scratch("ac-link-parent");
    let junction = parent.join("j");
    mklink_junction(&junction, &outside.0);
    let grants = fixture.grants(&[&junction], &[]);
    let error = grants
        .check_on_disk()
        .expect_err("a junction read root must be refused");
    assert!(
        matches!(
            error,
            windows_appcontainer::GrantError::NotADirectory { .. }
        ),
        "{error}"
    );
}

fn descendants_end_with_the_command_and_cannot_break_away() {
    let fixture = Fixture::new();
    let workspace = user_scratch("ac-ws");
    // The mechanism probe's orphan inherits its standard handles rather than
    // opening `NUL`, which an AppContainer may not be able to open.
    let run = fixture.run(&ww(&workspace), &["mechanism", "orphan"]);
    assert_ok(&run, "orphan a grandchild");
    let pid: u32 = run
        .stdout
        .trim()
        .strip_prefix("grandchild ")
        .and_then(|pid| pid.parse().ok())
        .unwrap_or_else(|| panic!("grandchild pid in {:?}", run.stdout));
    // The runner exits only after its Job is empty, so the grandchild is
    // gone, or its pid already names another program; it is only opened,
    // never signalled.
    {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        };
        // SAFETY: plain call; null means no such process.
        let raw = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        if !raw.is_null() {
            // SAFETY: a fresh handle owned here.
            let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
            assert!(
                signaled_within(handle.as_raw_handle(), Duration::ZERO)
                    || !is_image(handle.as_raw_handle(), &fixture.probe),
                "grandchild {pid} outlived the run"
            );
        }
    }
    let run = fixture.run(&ww(&workspace), &["mechanism", "breakaway"]);
    assert_eq!(
        run.code,
        Some(DENIED),
        "a breakaway must be refused: {}",
        run.stderr
    );
}

fn a_killed_runner_takes_its_tree() {
    let fixture = Fixture::new();
    let workspace = user_scratch("ac-ws");
    let mut started = fixture.start_runner(
        &workspace.0,
        &[
            "--workspace".into(),
            text(&workspace.0),
            "--mode".into(),
            "workspace-write".into(),
        ],
        &["appcontainer", "orphan-and-park"],
        false,
    );
    let line = started.line();
    let mut words = line.trim_end().splitn(6, ' ');
    let (
        Some("grandchild"),
        Some(grandchild),
        Some("child"),
        Some(direct),
        Some("local"),
        Some(local),
    ) = (
        words.next(),
        words.next(),
        words.next(),
        words.next(),
        words.next(),
        words.next(),
    )
    else {
        panic!("handshake {line:?}");
    };
    // The run's own profile, by the name its command reported.
    let (root, name) = fixture.run_profile(local);
    // Both are opened while the runner still holds its Job, so neither pid
    // can have been reused.
    let grandchild = OwnedPid::open_image(grandchild.parse().expect("pid"), &fixture.probe);
    let direct = OwnedPid::open_image(direct.parse().expect("pid"), &fixture.probe);
    // The runner alone is killed: its pid is pinned by its owner.
    let runner = OwnedPid::open_image(started.pid(), &std::env::current_exe().expect("exe"));
    assert!(grandchild.running() && direct.running() && runner.running());
    runner.terminate();
    assert!(
        runner.exited_within(BOUND),
        "the runner exits within the bound"
    );
    // Its Job closes with it, and kill-on-close ends the tree: observed on
    // handles pinned before the kill.
    assert!(direct.exited_within(BOUND), "the child outlived its runner");
    assert!(
        grandchild.exited_within(BOUND),
        "the grandchild outlived its runner"
    );
    // The owner's account must also show every member of the run ended
    // before the profile is touched.
    let ended = started.settle();
    assert_eq!(ended.code, Some(1), "the killed runner's status");
    // The documented residue: exactly the killed runner's profile, by the
    // name its command reported. Its tree has exited, so this case deletes
    // it, as no runner does yet.
    assert!(
        root.is_dir(),
        "the killed runner's profile {}",
        root.display()
    );
    assert_eq!(fixture.profiles(), BTreeSet::from([name.clone()]));
    observe(format_args!("case=killed-runner-residue profile={name}"));
    fixture
        .delete_profile(&name)
        .expect("delete the killed runner's profile");
    assert!(!root.exists(), "{} must be deleted", root.display());
}

fn loopback_stays_closed_with_and_without_network_grants() {
    let fixture = Fixture::new();
    let workspace = user_scratch("ac-ws");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let address = listener.local_addr().expect("listener address").to_string();
    // The control: the destination is reachable unconfined.
    let control = super::unconfined(&["mechanism", "connect", &address]);
    assert!(
        control.status.success(),
        "the unconfined loopback control must connect: {}",
        String::from_utf8_lossy(&control.stderr)
    );
    for network in [
        &[][..],
        &[
            NetworkGrant::InternetClient,
            NetworkGrant::PrivateNetworkClientServer,
        ][..],
    ] {
        let run = fixture.run_with(
            &ww(&workspace),
            &[],
            network,
            &["mechanism", "connect", &address],
        );
        assert_eq!(
            run.runner_failure(),
            None,
            "a refused connection is not a runner failure: {}",
            run.stderr
        );
        assert_ne!(
            run.code,
            Some(0),
            "loopback must stay closed with {network:?}"
        );
        observe(format_args!(
            "case=loopback-connect network={network:?} exit={:?} stderr={:?}",
            run.code,
            run.stderr.trim()
        ));
    }
    drop(listener);
    // A loopback grant is refused before anything runs.
    let error = runtime()
        .block_on(bake_sandbox::SandboxProvider::confine(
            &fixture.provider(&[], &[NetworkGrant::Loopback]),
            &[OsString::from("cmd")],
            &ww(&workspace),
        ))
        .expect_err("a loopback grant must be refused");
    assert!(
        error
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("loopback network access is unsupported")),
        "{error}"
    );
    // A remote destination, recorded only: the host's network decides it.
    let remote = "1.1.1.1:443";
    let control = super::unconfined(&["mechanism", "connect", remote]);
    observe(format_args!(
        "case=remote-connect-control exit={:?} stderr={:?}",
        control.status.code(),
        String::from_utf8_lossy(&control.stderr).trim()
    ));
    for network in [&[][..], &[NetworkGrant::InternetClient][..]] {
        let run = fixture.run_with(
            &ww(&workspace),
            &[],
            network,
            &["mechanism", "connect", remote],
        );
        observe(format_args!(
            "case=remote-connect network={network:?} exit={:?} stderr={:?}",
            run.code,
            run.stderr.trim()
        ));
    }
}

fn runner_failures_grant_nothing_and_clean_up() {
    let fixture = Fixture::new();
    let (workspace, outside) = (user_scratch("ac-ws"), user_scratch("ac-out"));
    let before_ws = dacl(&workspace.0);
    let before_out = dacl(&outside.0);
    let base = |mode: &str| {
        vec![
            "--workspace".to_owned(),
            text(&workspace.0),
            "--mode".to_owned(),
            mode.to_owned(),
        ]
    };
    let with = |extra: &[String]| {
        let mut args = base("workspace-write");
        args.extend(extra.iter().cloned());
        args
    };
    let cases: Vec<(Vec<String>, &str)> = vec![
        (
            with(&["--network".into(), "loopback".into()]),
            "loopback network access is unsupported",
        ),
        (
            with(&["--read".into(), text(&workspace.0)]),
            "overlaps the workspace",
        ),
        (
            with(&["--read".into(), text(&outside.join("missing"))]),
            "is unusable",
        ),
        (
            with(&[
                "--read".into(),
                text(&outside.0),
                "--read".into(),
                text(&outside.0),
            ]),
            "more than once",
        ),
        (base("everything"), "unknown mode"),
        (
            with(&["--temp".into(), text(&outside.0)]),
            "unknown argument: --temp",
        ),
    ];
    for (args, expected) in cases {
        let output = fixture.runner(&workspace.0, &args, &["exit", "0"]);
        let stderr = &output.stderr;
        assert_eq!(
            output.code,
            Some(windows_appcontainer::RUNNER_FAILURE_EXIT),
            "{expected}: {stderr}"
        );
        assert!(
            stderr.starts_with("windows-appcontainer-run: ") && stderr.contains(expected),
            "{expected}: {stderr}"
        );
    }
    assert_eq!(
        dacl(&workspace.0),
        before_ws,
        "a refused run granted the workspace"
    );
    assert_eq!(
        dacl(&outside.0),
        before_out,
        "a refused run granted a read root"
    );
    // The fixture's tag is fresh, so any profile under it is a refused
    // run's.
    assert_eq!(
        fixture.profiles(),
        BTreeSet::new(),
        "a refused run created a profile"
    );
    // A program that cannot start, in both modes: a runner failure from
    // `CreateProcessW`, which the runner reaches only after it created the
    // tagged profile, and that profile deleted again.
    let missing = workspace.join("no-such-program.exe");
    for mode in ["workspace-write", "read-only"] {
        let output = fixture
            .start_runner_command(&workspace.0, &base(mode), &[text(&missing)], false)
            .settle();
        let stderr = &output.stderr;
        assert_eq!(
            output.code,
            Some(windows_appcontainer::RUNNER_FAILURE_EXIT),
            "{mode}: {stderr}"
        );
        assert!(stderr.contains("CreateProcessW"), "{mode}: {stderr}");
        assert!(!stderr.contains("cleanup:"), "{mode}: {stderr}");
        assert_eq!(
            fixture.profiles(),
            BTreeSet::new(),
            "{mode}: the profile must be deleted after a failed spawn"
        );
    }
}

fn exit_codes_and_output_pass_through() {
    let fixture = Fixture::new();
    let workspace = user_scratch("ac-ws");
    for code in [0, 1, 3, 127] {
        let run = fixture.run(&ww(&workspace), &["exit", &code.to_string()]);
        assert_eq!(run.code, Some(code), "{}", run.stderr);
        if code == 127 {
            assert_eq!(run.runner_failure(), None, "the command's own 127");
        }
    }
    let run = fixture.run(&ro(&workspace), &["echo", "out", "err"]);
    assert_ok(&run, "echo");
    assert_eq!(run.stdout.trim(), "out");
    assert_eq!(run.stderr.trim(), "err");
}

/// Measures the documented hard-link boundary, a qualification blocker: an
/// outside file hard-linked into a workspace or read root before the first
/// grant shares one descriptor with its alias. The outcome is decided from
/// the disk and printed as an `appcontainer-blocker:` line; the case asserts
/// only that each report agrees with the disk, so it records whichever way
/// Windows behaves. Enabling confinement needs this line read natively.
fn hard_link_aliases_are_measured() {
    let fixture = Fixture::new();
    let (workspace, outside) = (user_scratch("ac-ws"), user_scratch("ac-out"));
    let victim = outside.write("victim.txt", "keep");
    std::fs::hard_link(&victim, workspace.join("alias.txt")).expect("an unconfined hard link");
    let before = dacl(&victim);
    let run = fixture.run(&ww(&workspace), &["write", &text(&victim), "changed"]);
    let changed = std::fs::read_to_string(&victim).expect("victim") == "changed";
    let consistent = match run.code {
        Some(0) => changed,
        Some(DENIED) => !changed,
        _ => panic!("neither success nor a denial: {}", run.stderr),
    };
    assert!(
        consistent,
        "the report contradicts the disk: {}",
        run.stderr
    );
    let widened = !added(&before, &dacl(&victim), &victim).is_empty();
    eprintln!(
        "appcontainer-blocker: case=hardlink-alias-workspace acl_widened={widened} outside_write={}",
        if changed { "effect" } else { "denied" }
    );
    let read = user_scratch("ac-read");
    let secret = outside.write("secret.txt", "secret");
    std::fs::hard_link(&secret, read.join("alias.txt")).expect("an unconfined hard link");
    let run = fixture.run_with(&ro(&workspace), &[&read.0], &[], &["read", &text(&secret)]);
    let observed = match run.code {
        Some(0) => {
            assert_eq!(run.stdout, "secret", "{}", run.stderr);
            "allowed"
        }
        Some(DENIED) => "denied",
        _ => panic!("neither success nor a denial: {}", run.stderr),
    };
    eprintln!("appcontainer-blocker: case=hardlink-alias-read-root outside_read={observed}");
}

/// The evidence that each run needs its own profile, not a runner case:
/// two processes of one AppContainer profile, launched directly as the
/// first native run's per-workspace profile was, with no write grant beyond
/// their output directory. CI 39035318623 recorded the second process
/// reading the first one's file. Each reports its `TEMP`; the first writes a file there
/// and the second reads it back. Each step is decided from the disk and
/// printed as an `appcontainer-defect:` line; the case asserts only that
/// the reports agree with the disk and that `TEMP` is the profile's.
fn a_shared_profile_shares_its_temp_directory() {
    use super::mechanisms::{CONTAINER_WORKSPACE, Profile, launch};
    // Created exclusively (an existing name fails) and deleted on drop,
    // after both processes are reaped.
    let profile = Profile::create();
    let output = user_scratch("ac-defect-out");
    let bin = user_scratch("ac-defect-bin");
    let sid = token::Sid::parse(&profile.sid_text).expect("package SID");
    acl::grant(&output.0, &sid, CONTAINER_WORKSPACE).expect("output grant");
    acl::grant(&bin.0, &sid, READ_EXECUTE).expect("bin grant");
    let exe = bin.join("probe.exe");
    std::fs::copy(std::env::current_exe().expect("exe"), &exe).expect("copy the test binary");
    let first = launch(
        None,
        Some(profile.sid),
        &exe,
        &["appcontainer", "write-profile-temp"],
        &output.0,
    );
    assert_eq!(first.code, Some(0), "write-temp: {}", first.stderr);
    let written = PathBuf::from(first.stdout.trim());
    assert_eq!(
        std::fs::read_to_string(&written).ok().as_deref(),
        Some("t"),
        "the first process's TEMP file {}",
        written.display()
    );
    let temp = written.parent().expect("TEMP").to_path_buf();
    eprintln!(
        "appcontainer-defect: case=profile-temp-written path={} host_temp={}",
        temp.display(),
        std::env::var("TEMP").unwrap_or_default()
    );
    assert!(
        text(&temp)
            .to_ascii_lowercase()
            .contains(&format!("\\packages\\{}", "bake.probe.")),
        "TEMP is not the profile's folder: {}",
        temp.display()
    );
    let second = launch(
        None,
        Some(profile.sid),
        &exe,
        &["read", &text(&written)],
        &output.0,
    );
    let read = match second.code {
        Some(0) => {
            assert_eq!(second.stdout, "t", "{}", second.stderr);
            "allowed"
        }
        Some(DENIED) => "denied",
        _ => panic!("neither success nor a denial: {}", second.stderr),
    };
    eprintln!("appcontainer-defect: case=profile-temp-second-process read={read}");
}

// ---------------------------------------------------------------------------
// The read-only profile attack.

/// The rights the deny pass must take from the package, each attempted as
/// an open of every object under the profile root: an open for a right is
/// that right's access check.
const ATTACK_RIGHTS: [(&str, u32); 8] = [
    ("write-data", 0x0002),
    ("append-data", 0x0004),
    ("write-ea", 0x0010),
    ("delete-child", 0x0040),
    ("write-attributes", 0x0100),
    ("delete", 0x0001_0000),
    ("write-dac", 0x0004_0000),
    ("write-owner", 0x0008_0000),
];

/// The rights a read needs, attempted on the same objects: the control
/// that shows a denial is the deny pass's, not a blanket refusal.
const CONTROL_RIGHTS: [(&str, u32); 2] =
    [("read-control", 0x0002_0000), ("read-attributes", 0x0080)];

const _: () = {
    let mut all = 0;
    let mut index = 0;
    while index < ATTACK_RIGHTS.len() {
        all |= ATTACK_RIGHTS[index].1;
        index += 1;
    }
    assert!(all == PROFILE_WRITE_MASK);
};

/// The probe operations behind `PROBE_ARG appcontainer ...`, with the same
/// exit convention as [`super::probe_main`].
pub fn probe(ops: &[&str]) -> std::io::Result<i32> {
    match ops {
        // Every mutation of every object under this process's profile
        // root, one line each, then `done`; then held until stdin closes,
        // so the test can read the disk while the profile still exists.
        ["profile-attack-and-wait"] => {
            use std::io::Read;
            profile_attack()?;
            println!("done");
            std::io::stdout().flush()?;
            let _ = std::io::stdin().read_to_end(&mut Vec::new());
            Ok(0)
        }
        // A parked descendant, then this process parks too, after a
        // handshake naming both and this process's LOCALAPPDATA, its
        // profile's `AC`: a confined tree, and a profile, for a runner to
        // lose.
        ["orphan-and-park"] => {
            let child = std::process::Command::new(std::env::current_exe()?)
                .args([PROBE_ARG, "park"])
                .spawn()?;
            println!(
                "grandchild {} child {} local {}",
                child.id(),
                std::process::id(),
                std::env::var("LOCALAPPDATA").unwrap_or_default()
            );
            std::io::stdout().flush()?;
            loop {
                std::thread::park();
            }
        }
        // A write of `probe.txt` into TEMP, only once TEMP is shown to be
        // this process's profile storage, so a TEMP Windows did not reroute
        // is never written; the path is printed.
        ["write-profile-temp"] => {
            let path = profile_temp()?.join("probe.txt");
            std::fs::write(&path, "t").map(|()| {
                println!("{}", path.display());
                0
            })
        }
        // The same for `held.txt`, reported, then held until stdin closes,
        // so a second run can act while this one still owns it.
        ["write-profile-temp-and-wait"] => {
            use std::io::Read;
            let path = profile_temp()?.join("held.txt");
            std::fs::write(&path, "held")?;
            println!("{}", path.display());
            std::io::stdout().flush()?;
            let _ = std::io::stdin().read_to_end(&mut Vec::new());
            Ok(0)
        }
        other => panic!("unknown appcontainer operation {other:?}"),
    }
}

/// TEMP, when it is `LOCALAPPDATA\Temp` and `LOCALAPPDATA` is an `AC`
/// folder under `Packages\<profile>`: an AppContainer profile's own
/// storage, never a host directory. Otherwise an error the case reports.
fn profile_temp() -> std::io::Result<PathBuf> {
    let var = |name: &str| PathBuf::from(std::env::var_os(name).unwrap_or_default());
    let (temp, local) = (var("TEMP"), var("LOCALAPPDATA"));
    let is = |path: Option<&Path>, name: &str| {
        path.and_then(Path::file_name)
            .and_then(|found| found.to_str())
            .is_some_and(|found| found.eq_ignore_ascii_case(name))
    };
    let packages = local.parent().and_then(Path::parent);
    let profile = text(&temp).eq_ignore_ascii_case(&text(&local.join("Temp")))
        && is(Some(&local), "AC")
        && is(packages, "Packages");
    if !profile {
        return Err(std::io::Error::other(format!(
            "TEMP {} is not profile storage (LOCALAPPDATA {})",
            temp.display(),
            local.display()
        )));
    }
    Ok(temp)
}

fn verdict(result: std::io::Result<()>) -> String {
    match result {
        Ok(()) => "allowed".to_owned(),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => "denied".to_owned(),
        Err(error) => format!("error:{}", error.raw_os_error().unwrap_or(-1)),
    }
}

/// The confined side: walks the profile root (`LOCALAPPDATA`'s parent)
/// and `LOCALAPPDATA` itself, in case the root cannot be listed, at most
/// `MAX_PROFILE_OBJECTS` objects and no reparse point, and attempts each
/// mutation on each object. Prints `root <path>`, then `attempt <kind>
/// <verdict> <path>` and `control <kind> <verdict> <path>` lines, and
/// `walk <kind> <verdict> <path>` for a directory it cannot list.
fn profile_attack() -> std::io::Result<()> {
    use std::os::windows::fs::MetadataExt;
    let local = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default());
    let root = local
        .parent()
        .ok_or_else(|| std::io::Error::other("LOCALAPPDATA has no parent"))?
        .to_path_buf();
    println!("root {}", root.display());
    let mut pending = vec![local, root];
    let mut visited = BTreeSet::new();
    while let Some(path) = pending.pop() {
        if !visited.insert(path.clone()) {
            continue;
        }
        if visited.len() > windows_appcontainer::MAX_PROFILE_OBJECTS {
            return Err(std::io::Error::other("too many profile objects"));
        }
        // Above `AC` the container may not read even attributes; the test
        // records such a line as unreached instead of failing the walk.
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) => {
                println!("walk inspect {} {}", verdict(Err(error)), path.display());
                continue;
            }
        };
        if meta.file_attributes() & 0x400 != 0 {
            println!("walk reparse-point skipped {}", path.display());
            continue;
        }
        let shown = path.display();
        for (kind, mask) in CONTROL_RIGHTS {
            let result = win32::open(&format!("{mask:x}"), &text(&path));
            println!("control {kind} {} {shown}", verdict(result));
        }
        for (kind, mask) in ATTACK_RIGHTS {
            let result = win32::open(&format!("{mask:x}"), &text(&path));
            println!("attempt {kind} {} {shown}", verdict(result));
        }
        let renamed = path.with_extension("bake-attack");
        let result = std::fs::rename(&path, &renamed);
        println!("attempt rename {} {shown}", verdict(result));
        if meta.is_dir() {
            let file = path.join("bake-attack-file");
            let result = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&file)
                .map(drop);
            println!("attempt create-file {} {shown}", verdict(result));
            let result = std::fs::create_dir(path.join("bake-attack-dir"));
            println!("attempt create-dir {} {shown}", verdict(result));
            match std::fs::read_dir(&path) {
                Ok(listing) => {
                    for entry in listing {
                        pending.push(entry?.path());
                    }
                }
                Err(error) => println!("walk list {} {shown}", verdict(Err(error))),
            }
        } else {
            let result = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .and_then(|mut file| file.write_all(b"x"));
            println!("attempt append {} {shown}", verdict(result));
            let result = win32::delete("DeleteFileW", &text(&path));
            println!("attempt delete-file {} {shown}", verdict(result));
        }
    }
    Ok(())
}

/// The package SID Windows derives for profile `name`, as text.
fn package_sid(name: &str) -> String {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::FreeSid;
    use windows_sys::Win32::Security::Isolation::DeriveAppContainerSidFromAppContainerName;
    let units: Vec<u16> = std::ffi::OsStr::new(name)
        .encode_wide()
        .chain([0])
        .collect();
    let mut sid = std::ptr::null_mut();
    // SAFETY: a NUL-terminated name and a valid out pointer.
    let status = unsafe { DeriveAppContainerSidFromAppContainerName(units.as_ptr(), &mut sid) };
    assert!(
        status >= 0 && !sid.is_null(),
        "DeriveAppContainerSidFromAppContainerName({name}): HRESULT {status:#010x}"
    );
    let text = win32::sid_text(sid);
    // SAFETY: documented as freed with `FreeSid`, and freed once.
    unsafe { FreeSid(sid) };
    text
}

/// Every object under `root`, the root included, as this unconfined
/// process lists it: bounded, and failing on a reparse point.
fn profile_objects(root: &Path) -> Vec<PathBuf> {
    use std::os::windows::fs::MetadataExt;
    let mut objects = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        assert!(
            objects.len() < windows_appcontainer::MAX_PROFILE_OBJECTS,
            "the profile {} is too large",
            root.display()
        );
        let meta = std::fs::symlink_metadata(&path)
            .unwrap_or_else(|error| panic!("inspect {}: {error}", path.display()));
        assert_eq!(
            meta.file_attributes() & 0x400,
            0,
            "{} is a reparse point",
            path.display()
        );
        if meta.is_dir() {
            for entry in std::fs::read_dir(&path)
                .unwrap_or_else(|error| panic!("list {}: {error}", path.display()))
            {
                pending.push(entry.expect("a profile entry").path());
            }
        }
        objects.push(path);
    }
    objects
}

/// A read-only run attacks every object of its own profile while this
/// test reads the disk: every mutation is denied, every read control
/// allowed, nothing was created, renamed, or removed, and every object,
/// launch-created ones included, leaves the package no allow ACE with a
/// write right.
/// The profile is gone once the run ends.
fn read_only_profile_storage_denies_every_mutation() {
    let fixture = Fixture::new();
    let workspace = user_scratch("ac-ws");
    let mut child = fixture.start(
        &ro(&workspace),
        &["appcontainer", "profile-attack-and-wait"],
        true,
    );
    let lines = child.lines_until("done");
    let root_line = lines
        .first()
        .and_then(|line| line.strip_prefix("root "))
        .unwrap_or_else(|| panic!("no root line: {lines:?}"));
    let (root, name) = fixture.run_profile(&text(&Path::new(root_line).join("AC")));
    let package = package_sid(&name);
    let ac = folded(&root.join("AC"));
    // The confined report.
    let mut attempted = BTreeSet::new();
    let mut failures = Vec::new();
    let mut controls_allowed = 0;
    for line in &lines[1..lines.len() - 1] {
        let mut words = line.splitn(4, ' ');
        let (Some(what), Some(_kind), Some(verdict), Some(path)) =
            (words.next(), words.next(), words.next(), words.next())
        else {
            panic!("report line {line:?}");
        };
        match (what, verdict) {
            ("attempt", "denied") => {
                attempted.insert(folded(Path::new(path)));
            }
            ("control", "allowed") => controls_allowed += 1,
            // Above `AC`, the container may lack read access; what it
            // cannot open it cannot change either.
            ("control" | "walk", _) if !folded(Path::new(path)).starts_with(&ac) => {
                observe(format_args!("case=read-only-profile outside-ac={line:?}"));
            }
            _ => failures.push(line.clone()),
        }
    }
    // The disk, read while the profile still exists.
    let objects = profile_objects(&root);
    let names: Vec<String> = objects
        .iter()
        .map(|path| text(path.strip_prefix(&root).expect("under the root")))
        .collect();
    observe(format_args!(
        "case=read-only-profile objects={} paths={names:?}",
        objects.len()
    ));
    for path in &objects {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if name.contains("bake-attack") {
            failures.push(format!("created {}", path.display()));
        }
        if !attempted.contains(&folded(path)) {
            // An object the container could not reach above `AC` is
            // recorded; inside `AC` it is a failure of this measurement.
            if folded(path).starts_with(&ac) {
                failures.push(format!("not attacked from inside: {}", path.display()));
            } else {
                observe(format_args!(
                    "case=read-only-profile unreached={}",
                    path.display()
                ));
            }
        }
        // GENERIC_ALL and GENERIC_WRITE, which an inherit-only ACE may hold.
        let writes = dacl(path).iter().any(|ace| {
            ace.kind == acl::ACCESS_ALLOWED
                && ace.mask & (PROFILE_WRITE_MASK | 0x1000_0000 | 0x4000_0000) != 0
                && ace.sid == package
        });
        if writes {
            failures.push(format!(
                "{package} still holds a write right: {} {}",
                path.display(),
                win32::sddl(path)
            ));
        }
    }
    for expected in [root.clone(), root.join("AC"), root.join("AC").join("Temp")] {
        if !objects.iter().any(|path| folded(path) == folded(&expected)) {
            failures.push(format!("missing {}", expected.display()));
        }
    }
    assert!(
        controls_allowed > 0,
        "no read control was allowed: {lines:?}"
    );
    child.close_stdin();
    let ended = child.settle();
    assert!(
        failures.is_empty(),
        "{failures:#?}\nstderr: {}",
        ended.stderr
    );
    assert_eq!(ended.code, Some(0), "{}", ended.stderr);
    assert!(!root.exists(), "{} must be deleted", root.display());
}

/// Two fixtures, each holding a live run at the same time, as two test
/// processes would: each lists exactly its own run's profile, by the name
/// that run reported, and neither's tag prefix matches the other's name.
fn concurrent_fixtures_see_only_their_own_profiles() {
    let (first, second) = (Fixture::new(), Fixture::new());
    assert_ne!(first.tag, second.tag);
    let workspace = user_scratch("ac-ws");
    let held = |fixture: &Fixture| {
        let mut run = fixture.start(
            &ww(&workspace),
            &["appcontainer", "write-profile-temp-and-wait"],
            true,
        );
        let path = PathBuf::from(run.line());
        let root = path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .expect("the profile root")
            .to_path_buf();
        let (_, name) = fixture.run_profile(&text(&root.join("AC")));
        (run, name)
    };
    let (mut a, a_name) = held(&first);
    let (mut b, b_name) = held(&second);
    assert_eq!(first.profiles(), BTreeSet::from([a_name.clone()]));
    assert_eq!(second.profiles(), BTreeSet::from([b_name.clone()]));
    assert!(!b_name.starts_with(&first.tag.moniker_prefix()));
    assert!(!a_name.starts_with(&second.tag.moniker_prefix()));
    for run in [&mut a, &mut b] {
        run.close_stdin();
        let ended = run.settle();
        assert_eq!(ended.code, Some(0), "{}", ended.stderr);
    }
    assert!(first.profiles().is_empty() && second.profiles().is_empty());
}

fn compatibility_observations() {
    let fixture = Fixture::new();
    let workspace = user_scratch("ac-ws");
    let provider = fixture.provider(&[], &[]);
    for argv in [
        &["cmd", "/d", "/c", "echo ok"][..],
        &[
            "powershell",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Write-Output ok",
        ][..],
        &[
            "pwsh",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Write-Output ok",
        ][..],
        &["git", "--version"][..],
    ] {
        let argv: Vec<String> = argv.iter().map(|arg| (*arg).to_owned()).collect();
        let run = fixture.run_argv(&provider, &argv, &ww(&workspace));
        observe(format_args!(
            "case=program argv={argv:?} exit={:?} stdout={:?} stderr={:?}",
            run.code,
            run.stdout.trim(),
            run.stderr.trim()
        ));
    }
    let _ = std::io::stderr().flush();
}
