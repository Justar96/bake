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
//! processes ([`Owned`], [`OwnedPid`], and the runner's own Job, killed and awaited on
//! drop). Every workspace a run uses is an [`AcWorkspace`], which also
//! deletes the standing AppContainer profile the runner created for it,
//! because the runner never removes profiles and each case's workspace is
//! fresh.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use bake_sandbox::backends::windows_appcontainer::{
    self, AppContainerGrants, HELPER_ARG, NetworkGrant, PRIVATE_TEMP_PREFIX, READ_EXECUTE,
    WORKSPACE_WRITE_MASK,
};
use bake_sandbox::backends::{LocalProvider, Runner};
use bake_sandbox::{Enforcement, SandboxMode, SandboxPolicy};

use super::mechanisms::{acl, seed_without_delete, token};
use super::{
    BOUND, DENIED, Owned, OwnedPid, PROBE_ARG, Run, Scratch, assert_denied, assert_ok, entries,
    harness, is_image, mklink_junction, run_argv, runtime, signaled_within, text, user_scratch,
    win32,
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
/// execution: the confined program of every case.
struct Fixture {
    bin: Scratch,
    probe: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let bin = user_scratch("ac-bin");
        let probe = bin.join("probe.exe");
        std::fs::copy(std::env::current_exe().expect("exe"), &probe)
            .expect("copy the test binary into the read root");
        Self { bin, probe }
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
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        run_argv(
            &self.provider(reads, network),
            &refs,
            policy,
            &policy.workspace_root,
        )
    }

    /// The runner started directly in `cwd` with `args` before `--` and
    /// `ops` after. The command inherits `cwd`, which its container must be
    /// able to read, so callers pass the workspace.
    fn runner(&self, cwd: &Path, args: &[String], ops: &[&str]) -> std::process::Output {
        std::process::Command::new(std::env::current_exe().expect("exe"))
            .current_dir(cwd)
            .arg(HELPER_ARG)
            .args(args)
            .args(["--read".to_owned(), text(&self.bin.0), "--".to_owned()])
            .args(self.argv(ops))
            .stdin(Stdio::null())
            .output()
            .expect("runner")
    }
}

/// A workspace root this test owns, with the per-user AppContainer profile
/// the runner creates for it. Dropped after the case's runs are awaited, it
/// deletes that profile (absent when no run reached profile creation), then
/// the directory.
struct AcWorkspace {
    scratch: Scratch,
}

impl std::ops::Deref for AcWorkspace {
    type Target = Scratch;

    fn deref(&self) -> &Scratch {
        &self.scratch
    }
}

impl Drop for AcWorkspace {
    fn drop(&mut self) {
        use windows_sys::Win32::Security::Isolation::DeleteAppContainerProfile;
        let name = windows_appcontainer::profile_name(&text(&self.scratch.0));
        let units: Vec<u16> = name.encode_utf16().chain([0]).collect();
        // SAFETY: a NUL-terminated name. DeleteAppContainerProfile documents
        // success for an absent profile too, so every failure is a cleanup
        // failure, including a case refused before profile creation.
        let status = unsafe { DeleteAppContainerProfile(units.as_ptr()) };
        if status < 0 {
            let error = format!("profile cleanup failed: name={name} hresult={status:#010x}");
            if std::thread::panicking() {
                eprintln!("{error}");
            } else {
                panic!("{error}");
            }
        }
    }
}

fn ac_workspace(label: &str) -> AcWorkspace {
    AcWorkspace {
        scratch: user_scratch(label),
    }
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
    let workspace = ac_workspace("ac-ws");
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
    let (workspace, outside) = (ac_workspace("ac-ws"), user_scratch("ac-out"));
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
    // Read-only has no private TEMP.
    let run = fixture.run(&policy, &["env", "TEMP"]);
    assert_ok(&run, "read-only TEMP");
    assert_eq!(run.stdout.trim(), std::env::var("TEMP").unwrap_or_default());
}

fn workspace_write_allows_effects_inside() {
    let fixture = Fixture::new();
    let workspace = ac_workspace("ac-ws");
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
    // The standing grants: every earlier ACE kept, plus the package's read
    // ACE and the write capability's ACE, explicit and inheritable.
    let extra = added(&before, &dacl(&workspace.0), &workspace.0);
    assert_eq!(extra.len(), 2, "{extra:?}");
    assert!(
        extra
            .iter()
            .any(|ace| explicit(READ_EXECUTE, "S-1-15-2-", ace)),
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
    let (workspace, outside) = (ac_workspace("ac-ws"), user_scratch("ac-out"));
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
    let foreign = ac_workspace("ac-foreign");
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
    let workspace = ac_workspace("ac-ws");
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
    let workspace = ac_workspace("ac-ws");
    let policy = ww(&workspace);
    let temp_root = bake_sandbox::canonical_path(&std::env::temp_dir());
    let is_private = |path: &Path| {
        path.parent() == Some(temp_root.as_path())
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(PRIVATE_TEMP_PREFIX))
    };
    let argv: Vec<OsString> = fixture
        .argv(&["write-temp-and-wait"])
        .into_iter()
        .map(OsString::from)
        .collect();
    let command = runtime()
        .block_on(bake_sandbox::SandboxProvider::confine(
            &fixture.provider(&[], &[]),
            &argv,
            &policy,
        ))
        .expect("confine");
    let mut a = Owned::spawn(
        std::process::Command::new(&command.program)
            .args(&command.args)
            .current_dir(&workspace.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    );
    let held = PathBuf::from(a.first_line().trim());
    let a_temp = held.parent().expect("A's TEMP").to_path_buf();
    assert!(is_private(&a_temp), "{}", a_temp.display());
    assert_eq!(std::fs::read_to_string(&held).expect("A's file"), "held");
    // B, on the same workspace, holds another temp capability.
    let intruder = a_temp.join("from-b.txt");
    assert_denied(
        &fixture.run(&policy, &["write", &text(&intruder), "x"]),
        "B creates in A's TEMP",
    );
    assert_denied(
        &fixture.run(&policy, &["read", &text(&held)]),
        "B reads A's file",
    );
    assert!(!intruder.exists());
    let b = fixture.run(&policy, &["spawn", "write-temp"]);
    assert_ok(&b, "B's descendant writes B's TEMP");
    let b_temp = PathBuf::from(b.stdout.trim())
        .parent()
        .expect("B's TEMP")
        .to_path_buf();
    assert!(
        is_private(&b_temp) && b_temp != a_temp,
        "{}",
        b_temp.display()
    );
    assert!(!b_temp.exists(), "{} must be removed", b_temp.display());
    drop(a.0.stdin.take());
    let status = a.wait_within(BOUND).expect("A exits within the bound");
    assert!(status.success(), "{status}");
    assert!(!a_temp.exists(), "{} must be removed", a_temp.display());
    // Under a temp root this test owns, nothing is left and no ACE stands.
    let root = user_scratch("ac-temp-root");
    let before = dacl(&root.0);
    let output = fixture.runner(
        &workspace.0,
        &[
            "--workspace".into(),
            text(&workspace.0),
            "--temp".into(),
            text(&root.0),
            "--mode".into(),
            "workspace-write".into(),
        ],
        &["spawn", "write-temp"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(entries(&root.0).is_empty(), "the private directory remains");
    assert_eq!(dacl(&root.0), before);
}

fn junction_escapes_are_denied() {
    let fixture = Fixture::new();
    let (workspace, outside) = (ac_workspace("ac-ws"), user_scratch("ac-out"));
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
    let workspace = ac_workspace("ac-ws");
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
    let workspace = ac_workspace("ac-ws");
    let root = user_scratch("ac-killed-root");
    let mut runner = Owned::spawn(
        std::process::Command::new(std::env::current_exe().expect("exe"))
            .arg(HELPER_ARG)
            .args(["--workspace", &text(&workspace.0), "--temp", &text(&root.0)])
            .args(["--mode", "workspace-write", "--read", &text(&fixture.bin.0)])
            .arg("--")
            .args(fixture.argv(&["mechanism", "orphan-and-park"]))
            .current_dir(&workspace.0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    );
    let line = runner.first_line();
    let mut words = line.split_whitespace();
    let (Some("grandchild"), Some(grandchild), Some("child"), Some(direct)) =
        (words.next(), words.next(), words.next(), words.next())
    else {
        panic!("handshake {line:?}");
    };
    // Both are opened while the runner still holds its Job, so neither pid
    // can have been reused.
    let grandchild = OwnedPid::open_image(grandchild.parse().expect("pid"), &fixture.probe);
    let direct = OwnedPid::open_image(direct.parse().expect("pid"), &fixture.probe);
    assert!(grandchild.running() && direct.running());
    runner.0.kill().expect("kill the runner");
    runner
        .wait_within(BOUND)
        .expect("the runner exits within the bound");
    assert!(direct.exited_within(BOUND), "the child outlived its runner");
    assert!(
        grandchild.exited_within(BOUND),
        "the grandchild outlived its runner"
    );
    // The documented residue: one private directory, removed with `root`.
    assert_eq!(entries(&root.0).len(), 1);
}

fn loopback_stays_closed_with_and_without_network_grants() {
    let fixture = Fixture::new();
    let workspace = ac_workspace("ac-ws");
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
    let (workspace, outside) = (ac_workspace("ac-ws"), user_scratch("ac-out"));
    let root = user_scratch("ac-fail-root");
    let before_ws = dacl(&workspace.0);
    let before_out = dacl(&outside.0);
    let base = |mode: &str| {
        vec![
            "--workspace".to_owned(),
            text(&workspace.0),
            "--temp".to_owned(),
            text(&root.0),
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
    ];
    for (args, expected) in cases {
        let output = fixture.runner(&workspace.0, &args, &["exit", "0"]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
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
    assert!(entries(&root.0).is_empty());
    // A program that cannot start: a runner failure, its temp removed.
    let missing = workspace.join("no-such-program.exe");
    let output = std::process::Command::new(std::env::current_exe().expect("exe"))
        .current_dir(&workspace.0)
        .arg(HELPER_ARG)
        .args(base("workspace-write"))
        .args(["--read", &text(&fixture.bin.0), "--", &text(&missing)])
        .stdin(Stdio::null())
        .output()
        .expect("runner");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(windows_appcontainer::RUNNER_FAILURE_EXIT),
        "{stderr}"
    );
    assert!(stderr.contains("CreateProcessW"), "{stderr}");
    assert!(
        entries(&root.0).is_empty(),
        "the private directory must be removed after a failed spawn"
    );
}

fn exit_codes_and_output_pass_through() {
    let fixture = Fixture::new();
    let workspace = ac_workspace("ac-ws");
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
    let (workspace, outside) = (ac_workspace("ac-ws"), user_scratch("ac-out"));
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

fn compatibility_observations() {
    let fixture = Fixture::new();
    let workspace = ac_workspace("ac-ws");
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
        let run = run_argv(&provider, argv, &ww(&workspace), &workspace.0);
        observe(format_args!(
            "case=program argv={argv:?} exit={:?} stdout={:?} stderr={:?}",
            run.code,
            run.stdout.trim(),
            run.stderr.trim()
        ));
    }
    let _ = std::io::stderr().flush();
}
