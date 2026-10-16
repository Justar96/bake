//! Real enforcement through each OS backend: every scenario confines a
//! `sh -c` command through [`LocalProvider`], spawns the confined argv, and
//! inspects the filesystem afterwards.
//!
//! Ported from `packages/sandbox/sandbox-local/tests/{bwrap,landlock,
//! seatbelt}.e2e.ts` and the scope-04 proof list (writes, renames, deletes,
//! symlink escapes, and descendants inside and outside the writable roots),
//! with the helper contract of `native/system/docs/cli-contract.md`.
//!
//! The binary has its own `main` (`harness = false`) so it can also serve as
//! the Landlock helper: a first argument of `__bake-landlock-run` runs
//! [`helper_main`], exactly as `bake-rs` does, and every Landlock test
//! confines through a real self-re-exec of this binary. The small runner
//! below accepts libtest's filter, `--exact`, `--list`, `--ignored`,
//! `--include-ignored`, and `--test-threads`.
//!
//! A backend that is not usable here is skipped with a printed note, except
//! under `CI=true`, which requires Landlock on Linux and Seatbelt on macOS.
//! The Windows scenarios ([`windows`]) are never skipped.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use bake_sandbox::backends::landlock::{HELPER_ARG, helper_main};
use bake_sandbox::backends::{LocalProvider, Runner};
use bake_sandbox::{
    ConfinedCommand, Enforcement, SandboxMode, SandboxPolicy, SandboxProvider,
    classify_runner_failure, default_provider, matches_signature,
};

#[cfg(windows)]
#[path = "enforcement/windows.rs"]
mod windows;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == HELPER_ARG) {
        helper_main(args.into_iter().skip(1));
    }
    if args
        .first()
        .is_some_and(|arg| arg == bake_sandbox::backends::windows_acl::HELPER_ARG)
    {
        bake_sandbox::backends::windows_acl::helper_main(args.into_iter().skip(1));
    }
    #[cfg(windows)]
    if args.first().is_some_and(|arg| arg == windows::PROBE_ARG) {
        windows::probe_main(args.into_iter().skip(1).collect());
    }
    harness::run(tests(), args)
}

/// A scenario run against one backend.
type Scenario = fn(&Backend);

/// Every test: per-runner scenarios, then the runner-independent ones.
fn tests() -> Vec<harness::Test> {
    let scenarios: &[(&str, Scenario)] = &[
        (
            "selects_and_reports_its_dialect",
            selects_and_reports_its_dialect,
        ),
        (
            "read_only_denies_every_write_but_dev_null",
            read_only_denies_every_write_but_dev_null,
        ),
        (
            "read_only_keeps_the_tree_readable",
            read_only_keeps_the_tree_readable,
        ),
        (
            "workspace_write_allows_effects_inside",
            workspace_write_allows_effects_inside,
        ),
        (
            "workspace_write_denies_effects_outside",
            workspace_write_denies_effects_outside,
        ),
        ("symlink_escapes_are_denied", symlink_escapes_are_denied),
        ("descendants_stay_confined", descendants_stay_confined),
        (
            "runner_failures_are_classified",
            runner_failures_are_classified,
        ),
        #[cfg(target_os = "linux")]
        (
            "hard_links_out_of_the_workspace_are_denied",
            hard_links_out_of_the_workspace_are_denied,
        ),
        #[cfg(target_os = "linux")]
        (
            "read_only_leaves_host_dev_shm_untouched",
            read_only_leaves_host_dev_shm_untouched,
        ),
        ("workspace_swap_is_refused", workspace_swap_is_refused),
        #[cfg(target_os = "linux")]
        (
            "confined_commands_get_their_own_session",
            confined_commands_get_their_own_session,
        ),
    ];
    let mut tests = Vec::new();
    for runner in platform_runners() {
        for (name, scenario) in scenarios {
            let scenario = *scenario;
            tests.push(harness::Test::new(
                format!("{}::{name}", runner.as_str()),
                move || with_backend(runner, scenario),
            ));
        }
    }
    #[cfg(target_os = "linux")]
    {
        tests.push(harness::Test::new(
            "bwrap::private_pid_namespace_and_no_proc_root_escape".into(),
            || with_backend(Runner::Bwrap, bwrap_private_pid_namespace),
        ));
        tests.push(harness::Test::new(
            "landlock::helper_cli_contract".into(),
            landlock_helper_cli_contract,
        ));
        tests.push(harness::Test::new(
            "landlock::restricted_bwrap_falls_through_to_landlock".into(),
            restricted_bwrap_falls_through_to_landlock,
        ));
        tests.push(harness::Test::new(
            "bwrap::path_hijack_is_blocked".into(),
            bwrap_path_hijack_is_blocked,
        ));
        tests.push(harness::Test::new(
            "bwrap::a_bwrap_in_a_workspace_path_entry_is_refused".into(),
            bwrap_in_a_workspace_path_entry_is_refused,
        ));
        tests.push(harness::Test::new(
            "landlock::a_helper_inside_the_workspace_is_refused".into(),
            landlock_helper_inside_the_workspace_is_refused,
        ));
    }
    #[cfg(windows)]
    tests.extend(windows::tests());
    tests.push(harness::Test::new(
        "chain::no_usable_backend_refuses".into(),
        no_usable_backend_refuses,
    ));
    tests.push(harness::Test::new(
        "chain::default_provider_confines_or_refuses".into(),
        default_provider_confines_or_refuses,
    ));
    tests
}

fn platform_runners() -> Vec<Runner> {
    if cfg!(target_os = "linux") {
        vec![Runner::Bwrap, Runner::Landlock]
    } else if cfg!(target_os = "macos") {
        vec![Runner::Seatbelt]
    } else {
        Vec::new()
    }
}

fn in_ci() -> bool {
    std::env::var_os("CI").is_some_and(|value| value == "true")
}

/// Whether CI must have this runner.
fn required(runner: Runner) -> bool {
    in_ci()
        && match runner {
            Runner::Landlock => cfg!(target_os = "linux"),
            Runner::Seatbelt => cfg!(target_os = "macos"),
            // The Windows scenarios never skip.
            Runner::Bwrap | Runner::WindowsAcl => false,
        }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

/// A provider pinned to one runner, with what it selected.
struct Backend {
    runner: Runner,
    provider: LocalProvider,
    enforcement: Enforcement,
}

/// A path that names no program, for disabling a rung.
const ABSENT: &str = "/nonexistent/bake-sandbox-absent-runner";

/// A two-rung chain whose second rung is disabled, so `runner` is probed.
fn pinned(runner: Runner) -> LocalProvider {
    match runner {
        Runner::Bwrap => LocalProvider::with_chain(vec![Runner::Bwrap, Runner::Landlock])
            .with_landlock_helper(ABSENT, []),
        Runner::Landlock => {
            LocalProvider::with_chain(vec![Runner::Bwrap, Runner::Landlock]).with_bwrap(ABSENT, [])
        }
        Runner::Seatbelt => {
            LocalProvider::with_chain(vec![Runner::Seatbelt, Runner::Bwrap]).with_bwrap(ABSENT, [])
        }
        Runner::WindowsAcl => LocalProvider::with_chain(vec![Runner::WindowsAcl]),
    }
}

fn with_backend(runner: Runner, scenario: Scenario) {
    let provider = pinned(runner);
    match runtime().block_on(provider.selected()) {
        Some((selected, enforcement)) if selected == runner => scenario(&Backend {
            runner,
            provider,
            enforcement,
        }),
        other => {
            assert!(
                !required(runner),
                "CI requires the {} sandbox backend, but its probe selected {other:?}",
                runner.as_str()
            );
            eprintln!(
                "note: skipping: the {} sandbox backend is not usable on this host",
                runner.as_str()
            );
        }
    }
}

/// A directory the test owns, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(base: &Path, label: &str) -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = base.join(format!(
            "bake-enforce-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Self(bake_sandbox::canonical_path(&dir))
    }

    /// Outside every writable root: under Cargo's per-target directory.
    fn outside(label: &str) -> Self {
        Self::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label)
    }

    /// Under the OS temporary directory, a writable root.
    fn temp(label: &str) -> Self {
        Self::new(&std::env::temp_dir(), label)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.join(name);
        std::fs::write(&path, text).expect("seed file");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A workspace and a sibling outside it, both outside the temp roots.
fn workspace_and_outside(mode: SandboxMode) -> (Scratch, Scratch, SandboxPolicy) {
    let workspace = Scratch::outside("ws");
    let outside = Scratch::outside("out");
    let policy = SandboxPolicy::new(mode, &workspace.0);
    assert!(
        !policy.allows_write(&outside.0),
        "the outside directory {} must not be writable under the policy",
        outside.0.display()
    );
    (workspace, outside, policy)
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

/// One confined run's result.
struct Run {
    command: ConfinedCommand,
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Run {
    fn denied(&self) -> bool {
        matches_signature(self.code, &self.stderr, &self.command.denial_signatures)
    }

    fn runner_failure(&self) -> Option<String> {
        classify_runner_failure(self.code, &self.stderr, &self.command.runner_failure_rules)
    }
}

/// Confines `argv` under `policy`, runs it in `cwd` with a 30-second bound,
/// and returns its result.
fn run_argv(
    provider: &dyn SandboxProvider,
    argv: &[&str],
    policy: &SandboxPolicy,
    cwd: &Path,
) -> Run {
    run_argv_with_path(provider, argv, policy, cwd, None)
}

/// [`run_argv`], spawning with `PATH` set to `path` when given.
fn run_argv_with_path(
    provider: &dyn SandboxProvider,
    argv: &[&str],
    policy: &SandboxPolicy,
    cwd: &Path,
    path: Option<&std::ffi::OsStr>,
) -> Run {
    let argv: Vec<OsString> = argv.iter().map(OsString::from).collect();
    runtime().block_on(async {
        let command = provider.confine(&argv, policy).await.expect("confine");
        let mut child = tokio::process::Command::new(&command.program);
        child
            .args(&command.args)
            .envs(command.env.iter().cloned())
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        if let Some(path) = path {
            child.env("PATH", path);
        }
        let output = tokio::time::timeout(std::time::Duration::from_secs(30), child.output())
            .await
            .expect("the confined command finishes")
            .expect("spawn the confined command");
        Run {
            command,
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    })
}

fn sh(backend: &Backend, script: &str, policy: &SandboxPolicy, cwd: &Path) -> Run {
    run_argv(&backend.provider, &["sh", "-c", script], policy, cwd)
}

/// Runs `script` and asserts it failed with this backend's denial dialect,
/// not a runner failure.
fn assert_denied(backend: &Backend, script: &str, policy: &SandboxPolicy, cwd: &Path) {
    let run = sh(backend, script, policy, cwd);
    assert_ne!(run.code, Some(0), "{script}: must fail\n{}", run.stderr);
    assert!(
        run.denied(),
        "{script}: stderr must carry {:?}: {}",
        run.command.denial_signatures,
        run.stderr
    );
    assert_eq!(
        run.runner_failure(),
        None,
        "{script}: a denial is not a runner failure"
    );
}

fn assert_ok(backend: &Backend, script: &str, policy: &SandboxPolicy, cwd: &Path) -> Run {
    let run = sh(backend, script, policy, cwd);
    assert_eq!(run.code, Some(0), "{script}: must succeed\n{}", run.stderr);
    run
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

// ---------------------------------------------------------------------------
// Per-runner scenarios.

fn selects_and_reports_its_dialect(backend: &Backend) {
    let (workspace, _outside, policy) = workspace_and_outside(SandboxMode::ReadOnly);
    let run = assert_ok(backend, "true", &policy, &workspace.0);
    let command = run.command;
    assert_eq!(command.enforcement, backend.enforcement);
    match backend.runner {
        Runner::Bwrap => {
            // Resolved once, absolutely, when the provider was built.
            let program = Path::new(&command.program);
            assert!(program.is_absolute(), "{}", program.display());
            assert_eq!(
                program.file_name().map(|name| name.to_os_string()),
                Some(OsString::from("bwrap"))
            );
            assert_eq!(command.enforcement, Enforcement::Full);
            assert_eq!(command.denial_signatures, ["read-only file system"]);
        }
        Runner::Landlock => {
            assert_eq!(
                Some(command.program),
                std::env::current_exe().ok().map(OsString::from)
            );
            assert_eq!(
                command.args.first().map(|arg| arg.as_os_str()),
                Some(HELPER_ARG.as_ref())
            );
            assert_eq!(command.denial_signatures, ["permission denied"]);
            // The wrap reports what the helper's own probe reports.
            let probe = std::process::Command::new(std::env::current_exe().expect("exe"))
                .args([HELPER_ARG, "--probe"])
                .output()
                .expect("probe");
            let expected = if String::from_utf8_lossy(&probe.stdout).contains("partially enforced")
            {
                Enforcement::Partial
            } else {
                Enforcement::Full
            };
            assert_eq!(backend.enforcement, expected);
            // Under a partial ABI every run reports it, as informational.
            if expected == Enforcement::Partial {
                assert!(
                    run.stderr
                        .contains("partial enforcement (older Landlock ABI)")
                );
            }
        }
        Runner::Seatbelt => {
            assert_eq!(command.program, "/usr/bin/sandbox-exec");
            assert_eq!(command.enforcement, Enforcement::Full);
            assert_eq!(command.denial_signatures, ["operation not permitted"]);
        }
        Runner::WindowsAcl => unreachable!("the Windows runner has its own scenarios"),
    }
}

fn read_only_denies_every_write_but_dev_null(backend: &Backend) {
    let (workspace, _outside, policy) = workspace_and_outside(SandboxMode::ReadOnly);
    let temp = Scratch::temp("ro-tmp");
    let keep = workspace.write("keep.txt", "orig");
    let cwd = &workspace.0;
    assert_denied(backend, "echo hi > new.txt", &policy, cwd);
    assert_denied(
        backend,
        &format!("echo hi > {}", quote(&temp.join("new.txt"))),
        &policy,
        cwd,
    );
    assert_denied(backend, "echo hacked > keep.txt", &policy, cwd);
    let _ = sh(backend, "rm -f keep.txt", &policy, cwd);
    let _ = sh(backend, "mv keep.txt moved.txt", &policy, cwd);
    let _ = sh(backend, "mkdir sub", &policy, cwd);
    assert_eq!(read(&keep).as_deref(), Some("orig"));
    for absent in ["new.txt", "moved.txt", "sub"] {
        assert!(!workspace.join(absent).exists(), "{absent} must not exist");
    }
    assert!(!temp.join("new.txt").exists());
    let run = assert_ok(backend, "echo ok > /dev/null && echo dev-ok", &policy, cwd);
    assert_eq!(run.stdout, "dev-ok\n");
}

fn read_only_keeps_the_tree_readable(backend: &Backend) {
    let (workspace, _outside, policy) = workspace_and_outside(SandboxMode::ReadOnly);
    workspace.write("note.txt", "readable");
    let run = assert_ok(
        backend,
        "ls / > /dev/null && cat note.txt",
        &policy,
        &workspace.0,
    );
    assert_eq!(run.stdout, "readable");
}

fn workspace_write_allows_effects_inside(backend: &Backend) {
    let (workspace, _outside, policy) = workspace_and_outside(SandboxMode::WorkspaceWrite);
    let temp = Scratch::temp("ww-tmp");
    workspace.write("old.txt", "old");
    workspace.write("gone.txt", "gone");
    assert_ok(
        backend,
        &format!(
            "printf new > new.txt && printf over > old.txt && mv old.txt renamed.txt \
             && rm gone.txt && mkdir -p sub/deep && printf deep > sub/deep/f.txt \
             && printf tmp > {}",
            quote(&temp.join("scratch.txt"))
        ),
        &policy,
        &workspace.0,
    );
    assert_eq!(read(&workspace.join("new.txt")).as_deref(), Some("new"));
    assert_eq!(
        read(&workspace.join("renamed.txt")).as_deref(),
        Some("over")
    );
    assert!(!workspace.join("old.txt").exists());
    assert!(!workspace.join("gone.txt").exists());
    assert_eq!(
        read(&workspace.join("sub/deep/f.txt")).as_deref(),
        Some("deep")
    );
    // The temp roots are the host's, as the file tools see them.
    assert_eq!(read(&temp.join("scratch.txt")).as_deref(), Some("tmp"));
}

fn workspace_write_denies_effects_outside(backend: &Backend) {
    let (workspace, outside, policy) = workspace_and_outside(SandboxMode::WorkspaceWrite);
    let victim = outside.write("victim.txt", "orig");
    let mover = workspace.write("mover.txt", "mine");
    let cwd = &workspace.0;
    let out = |name: &str| quote(&outside.join(name));
    assert_denied(
        backend,
        &format!("echo hi > {}", out("new.txt")),
        &policy,
        cwd,
    );
    assert_denied(
        backend,
        &format!("echo hacked > {}", out("victim.txt")),
        &policy,
        cwd,
    );
    let _ = sh(
        backend,
        &format!("rm -f {}", out("victim.txt")),
        &policy,
        cwd,
    );
    let _ = sh(
        backend,
        &format!("mv mover.txt {}", out("moved.txt")),
        &policy,
        cwd,
    );
    let _ = sh(
        backend,
        &format!("mv {} stolen.txt", out("victim.txt")),
        &policy,
        cwd,
    );
    let _ = sh(backend, &format!("mkdir {}", out("sub")), &policy, cwd);
    assert_eq!(read(&victim).as_deref(), Some("orig"));
    assert_eq!(read(&mover).as_deref(), Some("mine"));
    for absent in ["new.txt", "moved.txt", "sub"] {
        assert!(
            !outside.join(absent).exists(),
            "{absent} must not exist outside"
        );
    }
}

fn symlink_escapes_are_denied(backend: &Backend) {
    let (workspace, outside, policy) = workspace_and_outside(SandboxMode::WorkspaceWrite);
    let victim = outside.write("victim.txt", "orig");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside.0, workspace.join("dirlink")).expect("dir link");
        std::os::unix::fs::symlink(&victim, workspace.join("filelink")).expect("file link");
    }
    let cwd = &workspace.0;
    assert_denied(backend, "echo hi > dirlink/new.txt", &policy, cwd);
    assert_denied(backend, "echo hacked > filelink", &policy, cwd);
    // A link the confined command makes itself.
    let _ = sh(
        backend,
        &format!(
            "ln -s {} made && echo hi > made/made.txt",
            quote(&outside.0)
        ),
        &policy,
        cwd,
    );
    assert_eq!(read(&victim).as_deref(), Some("orig"));
    assert!(!outside.join("new.txt").exists());
    assert!(!outside.join("made.txt").exists());
}

fn descendants_stay_confined(backend: &Backend) {
    let (workspace, outside, policy) = workspace_and_outside(SandboxMode::WorkspaceWrite);
    let script = format!(
        "( sh -c 'echo child > {child}' ); \
         sh -c 'echo bg > {bg}' > /dev/null 2>&1 & wait; \
         sh -c 'sh -c \"echo inside > grandchild.txt\"'",
        child = outside.join("child.txt").display(),
        bg = outside.join("bg.txt").display(),
    );
    let _ = sh(backend, &script, &policy, &workspace.0);
    assert!(!outside.join("child.txt").exists());
    assert!(!outside.join("bg.txt").exists());
    assert_eq!(
        read(&workspace.join("grandchild.txt")).as_deref(),
        Some("inside\n")
    );
}

fn runner_failures_are_classified(backend: &Backend) {
    let (workspace, _outside, policy) = workspace_and_outside(SandboxMode::ReadOnly);
    let run = run_argv(
        &backend.provider,
        &["/nonexistent/bake-sandbox-no-such-command"],
        &policy,
        &workspace.0,
    );
    let prefix = match backend.runner {
        Runner::Bwrap => "bwrap: ",
        Runner::Landlock => "landlock-run: ",
        Runner::Seatbelt => "sandbox-exec: ",
        Runner::WindowsAcl => unreachable!("the Windows runner has its own scenarios"),
    };
    let line = run.runner_failure();
    assert!(
        line.as_deref()
            .is_some_and(|line| line.to_lowercase().starts_with(prefix)),
        "exit {:?}, stderr {:?}, classified {line:?}",
        run.code,
        run.stderr
    );
    // A command that fails on its own is not a runner failure.
    let run = sh(
        backend,
        "echo plain failure >&2; exit 3",
        &policy,
        &workspace.0,
    );
    assert_eq!(run.code, Some(3));
    assert_eq!(run.runner_failure(), None);
    assert!(!run.denied());
    // A writable root that does not exist is refused before any runner
    // starts (the helper's own refusal is in `helper_cli_contract`).
    let missing = SandboxPolicy::new(
        SandboxMode::WorkspaceWrite,
        workspace.join("missing-workspace"),
    );
    let error = runtime()
        .block_on(
            backend
                .provider
                .confine(&[OsString::from("true")], &missing),
        )
        .expect_err("a missing root is refused");
    assert!(
        error
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("no longer exists")),
        "{error:?}"
    );
}

/// The reviewer's workspace swap: a `workspace-write` call moves its
/// workspace (under `/tmp`, so the move itself is allowed) and leaves a link
/// to a victim directory in its place; the next call must refuse, not grant
/// the victim. Bubblewrap's bind mount makes the move fail inside the
/// sandbox, so the swap is then made from the host, as any other process
/// could.
fn workspace_swap_is_refused(backend: &Backend) {
    let parent = Scratch::temp("swap");
    let workspace = parent.join("ws");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let victim = Scratch::outside("swap-victim");
    let policy = SandboxPolicy::new(SandboxMode::WorkspaceWrite, &workspace);
    let swap = format!(
        "mv {ws} {gone} && ln -s {victim} {ws}",
        ws = quote(&workspace),
        gone = quote(&parent.join("ws.gone")),
        victim = quote(&victim.0)
    );
    let _ = sh(backend, &swap, &policy, &parent.0);
    let swapped_inside =
        std::fs::symlink_metadata(&workspace).is_ok_and(|meta| meta.file_type().is_symlink());
    if !swapped_inside {
        let _ = std::fs::remove_dir_all(parent.join("ws.gone"));
        std::fs::rename(&workspace, parent.join("ws.gone")).expect("host move");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&victim.0, &workspace).expect("host link");
    }
    let argv: Vec<OsString> = ["sh", "-c", "echo pwn > pwn.txt"]
        .iter()
        .map(OsString::from)
        .collect();
    let refused = runtime().block_on(backend.provider.confine(&argv, &policy));
    let error = refused.expect_err("a swapped workspace must be refused");
    assert_eq!(error.code(), "SANDBOX_UNAVAILABLE");
    assert!(
        error
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("changed since the session started")),
        "{error:?}"
    );
    assert_eq!(
        std::fs::read_dir(&victim.0).map(|dir| dir.count()).ok(),
        Some(0),
        "nothing may reach the victim (swapped inside the sandbox: {swapped_inside})"
    );
}

/// A pseudo-terminal pair for the session test: the master end, and the
/// slave end that becomes the controlling terminal of an "agent" shell.
#[cfg(target_os = "linux")]
fn open_pty() -> (std::fs::File, std::fs::File) {
    use std::os::fd::FromRawFd;
    // SAFETY: plain libc calls on a descriptor this function owns.
    unsafe {
        let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
        assert!(master >= 0, "posix_openpt");
        let master = std::fs::File::from_raw_fd(master);
        use std::os::fd::AsRawFd;
        assert_eq!(libc::grantpt(master.as_raw_fd()), 0);
        assert_eq!(libc::unlockpt(master.as_raw_fd()), 0);
        let mut name = [0 as libc::c_char; 128];
        assert_eq!(
            libc::ptsname_r(master.as_raw_fd(), name.as_mut_ptr(), name.len()),
            0
        );
        let slave = libc::open(
            name.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        );
        assert!(slave >= 0, "open the pty slave");
        (master, std::fs::File::from_raw_fd(slave))
    }
}

/// Runs `argv` from a shell that leads its own session with a terminal as
/// its controlling terminal, as Bake's own shell would, and returns stdout.
/// With `job_control`, the shell puts the command in its own process group,
/// so the Landlock helper cannot `setsid` and must give up the terminal.
#[cfg(target_os = "linux")]
fn run_on_a_terminal(argv: &[OsString], job_control: bool) -> String {
    use std::os::unix::process::CommandExt;
    let (_master, slave) = open_pty();
    let script = if job_control {
        "set -m; \"$@\"; exit $?"
    } else {
        "\"$@\"; exit $?"
    };
    let mut command = std::process::Command::new("sh");
    command
        .args(["-c", script, "sh"])
        .args(argv)
        .stdin(slave.try_clone().expect("slave"))
        .stderr(std::process::Stdio::null());
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = command.output().expect("run on a terminal");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A confined command has no controlling terminal shared with Bake, so it
/// cannot `TIOCSTI` keystrokes into it: bubblewrap's `--new-session`, and
/// the helper's `setsid` or `TIOCNOTTY`.
#[cfg(target_os = "linux")]
fn confined_commands_get_their_own_session(backend: &Backend) {
    let probe: Vec<OsString> = [
        "sh",
        "-c",
        "if (exec 3</dev/tty) 2>/dev/null; then echo tty; else echo none; fi",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    // The control: unconfined, the command shares the terminal.
    assert_eq!(run_on_a_terminal(&probe, false), "tty");
    let (_workspace, _outside, policy) = workspace_and_outside(SandboxMode::ReadOnly);
    let command = runtime()
        .block_on(backend.provider.confine(&probe, &policy))
        .expect("confine");
    let mut argv = vec![command.program.clone()];
    argv.extend(command.args.iter().cloned());
    for job_control in [false, true] {
        assert_eq!(
            run_on_a_terminal(&argv, job_control),
            "none",
            "{} (job control {job_control}) kept the controlling terminal",
            backend.runner.as_str()
        );
    }
}

/// The reviewer's PATH hijack: a writable directory on `PATH` ahead of the
/// real bubblewrap. A `workspace-write` call plants a fake `bwrap` there;
/// the next `read-only` call must still run the bubblewrap resolved when
/// the provider was built, and stay confined.
#[cfg(target_os = "linux")]
fn bwrap_path_hijack_is_blocked() {
    let bin = Scratch::temp("hijack-bin");
    let path = std::env::join_paths(std::iter::once(bin.0.clone()).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .expect("PATH");
    let provider = LocalProvider::with_chain(vec![Runner::Bwrap, Runner::Landlock])
        .with_landlock_helper(ABSENT, [])
        .with_search_path(&path);
    let Some((Runner::Bwrap, enforcement)) = runtime().block_on(provider.selected()) else {
        eprintln!("note: skipping: bubblewrap is not usable on this host");
        return;
    };
    let backend = Backend {
        runner: Runner::Bwrap,
        provider,
        enforcement,
    };
    let (workspace, outside, _) = workspace_and_outside(SandboxMode::WorkspaceWrite);
    let fake = bin.join("bwrap");
    let plant = format!(
        "printf '#!/bin/sh\\nwhile [ \"$1\" != -- ]; do shift; done; shift; exec \"$@\"\\n' > {fake} && chmod +x {fake}",
        fake = quote(&fake)
    );
    // Every spawn sees the hijacked PATH, as the whole Bake process would.
    let spawn = |script: &str, policy: &SandboxPolicy| {
        run_argv_with_path(
            &backend.provider,
            &["sh", "-c", script],
            policy,
            &workspace.0,
            Some(&path),
        )
    };
    let ww = SandboxPolicy::new(SandboxMode::WorkspaceWrite, &workspace.0);
    let planted = spawn(&plant, &ww);
    assert_eq!(planted.code, Some(0), "{}", planted.stderr);
    assert!(
        fake.exists(),
        "the plant must succeed for the test to mean anything"
    );
    let target = outside.join("path_hijack");
    let ro = SandboxPolicy::new(SandboxMode::ReadOnly, &workspace.0);
    let run = spawn(&format!("echo pwn > {}", quote(&target)), &ro);
    assert_ne!(Path::new(&run.command.program), fake.as_path());
    assert!(run.denied(), "{:?} {}", run.code, run.stderr);
    assert!(!target.exists(), "the read-only call ran unconfined");
}

/// A fake `bwrap` already in a `PATH` entry inside the workspace when the
/// provider is built is found, but never wrapped: the workspace is writable
/// by the session's calls and file tools.
#[cfg(target_os = "linux")]
fn bwrap_in_a_workspace_path_entry_is_refused() {
    let (workspace, _outside, _) = workspace_and_outside(SandboxMode::WorkspaceWrite);
    let bin = workspace.join("node_modules/.bin");
    std::fs::create_dir_all(&bin).expect("bin");
    let fake = bin.join("bwrap");
    std::fs::write(&fake, "#!/bin/sh\nexit 0\n").expect("fake");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let provider = LocalProvider::with_chain(vec![Runner::Bwrap])
        .with_search_path(std::env::join_paths([bin.clone(), "/usr/bin".into()]).expect("PATH"));
    let argv = [OsString::from("true")];
    for mode in [SandboxMode::ReadOnly, SandboxMode::WorkspaceWrite] {
        let policy = SandboxPolicy::new(mode, &workspace.0);
        let error = runtime()
            .block_on(provider.confine(&argv, &policy))
            .expect_err("a runner in the workspace is refused");
        assert!(
            error
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("bubblewrap")
                    && detail.contains("lies under the writable root")),
            "{error:?}"
        );
    }
}

/// The Landlock helper (the current executable) copied into the workspace,
/// as when a developer runs `target/debug/bake-rs` inside the Bake repo:
/// every mode refuses rather than run a helper a confined command could
/// overwrite.
#[cfg(target_os = "linux")]
fn landlock_helper_inside_the_workspace_is_refused() {
    let (workspace, _outside, _) = workspace_and_outside(SandboxMode::WorkspaceWrite);
    let helper = workspace.join("bake-rs");
    std::fs::copy(std::env::current_exe().expect("exe"), &helper).expect("copy");
    let provider = LocalProvider::with_chain(vec![Runner::Landlock])
        .with_landlock_helper(helper.clone(), [OsString::from(HELPER_ARG)]);
    let argv = [OsString::from("true")];
    for mode in [SandboxMode::ReadOnly, SandboxMode::WorkspaceWrite] {
        let policy = SandboxPolicy::new(mode, &workspace.0);
        let error = runtime()
            .block_on(provider.confine(&argv, &policy))
            .expect_err("a helper in the workspace is refused");
        assert!(
            error
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("Landlock helper")),
            "{error:?}"
        );
    }
}

#[cfg(target_os = "linux")]
fn hard_links_out_of_the_workspace_are_denied(backend: &Backend) {
    let (workspace, outside, policy) = workspace_and_outside(SandboxMode::WorkspaceWrite);
    let victim = outside.write("victim.txt", "orig");
    let _ = sh(
        backend,
        &format!("ln {} hard.txt && echo hacked > hard.txt", quote(&victim)),
        &policy,
        &workspace.0,
    );
    assert_eq!(read(&victim).as_deref(), Some("orig"));
}

#[cfg(target_os = "linux")]
fn read_only_leaves_host_dev_shm_untouched(backend: &Backend) {
    let Ok(meta) = std::fs::metadata("/dev/shm") else {
        eprintln!("note: skipping: no /dev/shm");
        return;
    };
    assert!(meta.is_dir());
    let (workspace, _outside, policy) = workspace_and_outside(SandboxMode::ReadOnly);
    let target = PathBuf::from(format!(
        "/dev/shm/bake-enforce-{}-{}",
        backend.runner.as_str(),
        std::process::id()
    ));
    let _ = std::fs::remove_file(&target);
    let _ = sh(
        backend,
        &format!("echo hi > {}", quote(&target)),
        &policy,
        &workspace.0,
    );
    let leaked = target.exists();
    let _ = std::fs::remove_file(&target);
    assert!(!leaked, "a read-only command wrote the host's /dev/shm");
}

#[cfg(target_os = "linux")]
fn bwrap_private_pid_namespace(backend: &Backend) {
    // From `bwrap.e2e.ts`: a private PID namespace, and no write through
    // procfs root links.
    for mode in [SandboxMode::ReadOnly, SandboxMode::WorkspaceWrite] {
        let (workspace, outside, policy) = workspace_and_outside(mode);
        let host = std::fs::read_link("/proc/self/ns/pid").expect("host pid ns");
        let run = assert_ok(backend, "readlink /proc/self/ns/pid", &policy, &workspace.0);
        assert_ne!(run.stdout.trim(), "");
        assert_ne!(Path::new(run.stdout.trim()), host);
        let target = outside.join("escaped.txt");
        let run = sh(
            backend,
            &format!("printf escaped > /proc/1/root{}", target.display()),
            &policy,
            &workspace.0,
        );
        assert_ne!(run.code, Some(0));
        assert!(!target.exists());
    }
    // Descendants stay observable and controllable inside the namespace.
    let (workspace, _outside, policy) = workspace_and_outside(SandboxMode::ReadOnly);
    assert_ok(
        backend,
        "sleep 30 & child=$!; kill -0 \"$child\" && kill \"$child\"; wait \"$child\"; status=$?; test \"$status\" -ge 128",
        &policy,
        &workspace.0,
    );
}

// ---------------------------------------------------------------------------
// Runner-independent scenarios.

#[cfg(target_os = "linux")]
fn landlock_helper_cli_contract() {
    let exe = std::env::current_exe().expect("exe");
    let helper = |args: &[&str]| {
        std::process::Command::new(&exe)
            .arg(HELPER_ARG)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run helper")
    };
    let usage = helper(&["--ro", "/"]);
    assert_eq!(usage.status.code(), Some(125));
    assert_eq!(
        String::from_utf8_lossy(&usage.stderr),
        "landlock-run: usage error: missing `-- <argv>...` command\n"
    );
    let usage = helper(&["--probe", "--ro", "/"]);
    assert_eq!(
        String::from_utf8_lossy(&usage.stderr),
        "landlock-run: usage error: --probe takes no other arguments\n"
    );
    let usage = helper(&["--bogus"]);
    assert_eq!(
        String::from_utf8_lossy(&usage.stderr),
        "landlock-run: usage error: unknown argument: --bogus\n"
    );
    let probe = helper(&["--probe"]);
    if probe.status.code() != Some(0) {
        assert!(
            !required(Runner::Landlock),
            "CI requires Landlock: {probe:?}"
        );
        assert_eq!(probe.status.code(), Some(125));
        assert_eq!(
            String::from_utf8_lossy(&probe.stderr),
            "landlock-run: landlock is not enforced by this kernel (ABI unsupported or disabled)\n"
        );
        eprintln!("note: skipping the enforcing half: Landlock is not enforced here");
        return;
    }
    let report = String::from_utf8_lossy(&probe.stdout).into_owned();
    assert!(
        report == "landlock: fully enforced\n"
            || report == "landlock: partially enforced (older ABI)\n",
        "{report:?}"
    );
    // A granted path reached through a link fails closed: the helper never
    // follows one, in any component.
    let linked = Scratch::outside("helper-link");
    std::fs::create_dir_all(linked.join("real/sub")).expect("real");
    std::os::unix::fs::symlink(linked.join("real"), linked.join("link")).expect("link");
    for path in [linked.join("link"), linked.join("link/sub")] {
        let path = path.to_string_lossy().into_owned();
        let followed = helper(&["--ro", "/", "--rw", &path, "--", "true"]);
        assert_eq!(followed.status.code(), Some(125), "{followed:?}");
        assert_eq!(
            String::from_utf8_lossy(&followed.stderr),
            format!(
                "landlock-run: cannot open rule path: {path}: Too many levels of symbolic links\n"
            )
        );
    }
    let unopenable = helper(&["--ro", "/", "--rw", "/nonexistent/bake-root", "--", "true"]);
    assert_eq!(unopenable.status.code(), Some(125));
    assert_eq!(
        String::from_utf8_lossy(&unopenable.stderr),
        "landlock-run: cannot open rule path: /nonexistent/bake-root: No such file or directory\n"
    );
    let exec = helper(&["--ro", "/", "--", "/nonexistent/bake-command"]);
    assert_eq!(exec.status.code(), Some(125));
    assert!(
        String::from_utf8_lossy(&exec.stderr)
            .ends_with("landlock-run: exec failed: No such file or directory\n"),
        "{exec:?}"
    );
    // After the exec the command's own status passes through, 125 included.
    let passthrough = helper(&["--ro", "/", "--", "sh", "-c", "exit 125"]);
    assert_eq!(passthrough.status.code(), Some(125));
    assert!(!String::from_utf8_lossy(&passthrough.stderr).contains("landlock-run: exec"));
}

#[cfg(target_os = "linux")]
fn restricted_bwrap_falls_through_to_landlock() {
    // A bubblewrap that starts and refuses, as under Ubuntu 24.04's
    // AppArmor user-namespace restriction.
    let scratch = Scratch::outside("fake-bwrap");
    let script = scratch.write(
        "bwrap",
        "echo 'bwrap: setting up uid map: Permission denied' >&2\nexit 1\n",
    );
    let provider = LocalProvider::with_chain(vec![Runner::Bwrap, Runner::Landlock])
        .with_bwrap("/bin/sh", [script.into_os_string()]);
    match runtime().block_on(provider.selected()) {
        Some((Runner::Landlock, enforcement)) => {
            let backend = Backend {
                runner: Runner::Landlock,
                provider,
                enforcement,
            };
            let (workspace, outside, policy) = workspace_and_outside(SandboxMode::WorkspaceWrite);
            assert_denied(
                &backend,
                &format!("echo hi > {}", quote(&outside.join("x"))),
                &policy,
                &workspace.0,
            );
            assert!(!outside.join("x").exists());
        }
        None => {
            assert!(!required(Runner::Landlock), "CI requires Landlock");
            eprintln!("note: skipping: Landlock is not usable on this host");
        }
        Some(other) => panic!("a refusing bwrap must not be selected: {other:?}"),
    }
}

fn no_usable_backend_refuses() {
    let rt = runtime();
    let argv = [OsString::from("true")];
    let policy = SandboxPolicy::new(SandboxMode::WorkspaceWrite, std::env::temp_dir());
    let empty = LocalProvider::with_chain(Vec::new());
    let error = rt
        .block_on(empty.confine(&argv, &policy))
        .expect_err("empty chain");
    assert_eq!(error.mode, SandboxMode::WorkspaceWrite);
    assert_eq!(error.code(), "SANDBOX_UNAVAILABLE");
    let nothing =
        LocalProvider::with_chain(vec![Runner::Bwrap, Runner::Landlock, Runner::Seatbelt])
            .with_bwrap(ABSENT, [])
            .with_landlock_helper(ABSENT, [])
            .with_seatbelt(ABSENT, []);
    let error = rt
        .block_on(nothing.confine(&argv, &policy))
        .expect_err("no backend");
    assert_eq!(error.detail, None);
    assert!(error.to_string().starts_with(
        "sandbox mode \"workspace-write\" is requested but no sandbox backend is usable on this host; refusing to run the command unconfined."
    ));
}

fn default_provider_confines_or_refuses() {
    let provider: Arc<dyn SandboxProvider> = default_provider();
    let workspace = Scratch::outside("default");
    let policy = SandboxPolicy::new(SandboxMode::ReadOnly, &workspace.0);
    let script: &[&str] = if cfg!(windows) {
        &["cmd", "/c", "echo hi > denied.txt"]
    } else {
        &["sh", "-c", "echo hi > denied.txt"]
    };
    let argv: Vec<OsString> = script.iter().map(OsString::from).collect();
    match runtime().block_on(provider.confine(&argv, &policy)) {
        // Windows has no default backend: its runner leaves deletion
        // unconfined.
        Ok(_) if cfg!(windows) => panic!("the Windows default provider must refuse"),
        Ok(_) => {
            let run = run_argv(provider.as_ref(), script, &policy, &workspace.0);
            assert_ne!(run.code, Some(0));
            assert!(run.denied(), "{}", run.stderr);
            assert!(!workspace.join("denied.txt").exists());
        }
        #[cfg(windows)]
        Err(error) => assert_eq!(
            error.detail.as_deref(),
            Some(bake_sandbox::backends::windows_acl::DEFAULT_REFUSAL),
            "{error}"
        ),
        #[cfg(not(windows))]
        Err(error) => {
            assert!(!in_ci(), "CI requires a backend here: {error}");
            eprintln!("note: the default provider refused here: {error}");
        }
    }
}

mod harness {
    //! A minimal libtest stand-in: this binary's `main` must stay free to
    //! serve as the Landlock helper. It runs filters (`--exact`, `--skip`),
    //! `--list`, and `--test-threads`; no test here is `#[ignore]`d.
    use std::ffi::OsString;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::process::ExitCode;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub struct Test {
        name: String,
        run: Box<dyn Fn() + Send + Sync>,
    }

    impl Test {
        pub fn new(name: String, run: impl Fn() + Send + Sync + 'static) -> Self {
            Self {
                name,
                run: Box::new(run),
            }
        }
    }

    pub fn run(tests: Vec<Test>, args: Vec<OsString>) -> ExitCode {
        let mut filters = Vec::new();
        let mut skips: Vec<String> = Vec::new();
        let mut exact = false;
        let mut list = false;
        let mut only_ignored = false;
        let mut threads = std::thread::available_parallelism().map_or(1, usize::from);
        let mut args = args
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned());
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--exact" => exact = true,
                "--list" => list = true,
                "--ignored" => only_ignored = true,
                "--test-threads" => {
                    threads = args.next().and_then(|n| n.parse().ok()).unwrap_or(threads);
                }
                other if other.starts_with("--test-threads=") => {
                    threads = other["--test-threads=".len()..].parse().unwrap_or(threads);
                }
                "--skip" => skips.extend(args.next()),
                // libtest options that take a value this runner ignores.
                "--format" | "--color" | "--logfile" | "--shuffle-seed" | "-Z" => {
                    args.next();
                }
                other if other.starts_with('-') => {}
                other => filters.push(other.to_owned()),
            }
        }
        let selected: Vec<Test> = tests
            .into_iter()
            .filter(|test| !skips.iter().any(|skip| test.name.contains(skip.as_str())))
            .filter(|test| {
                filters.is_empty()
                    || filters.iter().any(|filter| {
                        if exact {
                            test.name == *filter
                        } else {
                            test.name.contains(filter.as_str())
                        }
                    })
            })
            .collect();
        if list {
            for test in &selected {
                println!("{}: test", test.name);
            }
            return ExitCode::SUCCESS;
        }
        // No test here is ignored.
        let selected = if only_ignored { Vec::new() } else { selected };
        println!("\nrunning {} tests", selected.len());
        let next = AtomicUsize::new(0);
        let failures = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for _ in 0..threads.max(1) {
                scope.spawn(|| {
                    while let Some(test) = selected.get(next.fetch_add(1, Ordering::Relaxed)) {
                        let passed = catch_unwind(AssertUnwindSafe(|| (test.run)())).is_ok();
                        println!(
                            "test {} ... {}",
                            test.name,
                            if passed { "ok" } else { "FAILED" }
                        );
                        if !passed {
                            failures
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .push(test.name.clone());
                        }
                    }
                });
            }
        });
        let failures = failures
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let passed = selected.len() - failures.len();
        if failures.is_empty() {
            println!("\ntest result: ok. {passed} passed; 0 failed\n");
            ExitCode::SUCCESS
        } else {
            println!("\nfailures:");
            for name in &failures {
                println!("    {name}");
            }
            println!(
                "\ntest result: FAILED. {passed} passed; {} failed\n",
                failures.len()
            );
            ExitCode::from(101)
        }
    }
}
