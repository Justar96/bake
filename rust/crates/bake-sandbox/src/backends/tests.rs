//! Chain selection, caching, probe parsing, wrap shapes, and fail-closed
//! refusals through the real `confine` path, following `describe('the
//! platform chains')`, `describe('the default landlock probe')`,
//! `describe('probeTimeoutMs config')`, and `describe('the default seatbelt
//! probe')` in `packages/sandbox/sandbox-local/tests/local.spec.ts`. Fake
//! runners are shell scripts run through `/bin/sh`, so no test executes a
//! file it has just written. Real enforcement is in `tests/enforcement.rs`.

use std::path::Path;

use super::*;
use crate::provider::confine;

fn os(list: &[&str]) -> Vec<OsString> {
    list.iter().map(OsString::from).collect()
}

/// An existing canonical workspace: every wrap checks its writable roots.
fn workspace() -> std::path::PathBuf {
    crate::paths::canonical_path(&std::env::temp_dir())
}

fn ro() -> SandboxPolicy {
    SandboxPolicy::new(SandboxMode::ReadOnly, workspace())
}

fn ww() -> SandboxPolicy {
    SandboxPolicy::new(SandboxMode::WorkspaceWrite, workspace())
}

/// Polls a future that never pends, outside any runtime.
fn poll_once<F: std::future::Future>(future: F) -> Option<F::Output> {
    let waker = std::task::Waker::noop();
    let mut context = std::task::Context::from_waker(waker);
    let mut future = std::pin::pin!(future);
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(value) => Some(value),
        std::task::Poll::Pending => None,
    }
}

#[test]
fn an_empty_chain_refuses_without_a_runtime_or_a_probe() {
    let provider = LocalProvider::with_chain(Vec::new());
    for policy in [ro(), ww()] {
        assert_eq!(
            poll_once(provider.confine(&os(&["true"]), &policy)),
            Some(Err(SandboxUnavailable::new(policy.mode, None)))
        );
    }
    // danger-full-access never reaches the provider.
    let open = SandboxPolicy::new(SandboxMode::DangerFullAccess, "/ws");
    assert_eq!(
        poll_once(confine(&provider, &os(&["true"]), &open)),
        Some(Ok(None))
    );
}

#[test]
fn a_sole_landlock_rung_wraps_with_the_current_executable_unprobed() {
    let provider = LocalProvider::with_chain(vec![Runner::Landlock]);
    let Some(Ok(command)) = poll_once(provider.confine(&os(&["bash", "-c", "x"]), &ww())) else {
        panic!("a sole candidate needs no probe");
    };
    let exe = std::env::current_exe().map(OsString::from).ok();
    assert_eq!(Some(command.program.clone()), exe);
    let mut expected = os(&[landlock::HELPER_ARG]);
    expected.extend(profiles::landlock_profile_args(&ww()));
    expected.extend(os(&["--", "bash", "-c", "x"]));
    assert_eq!(command.args, expected);
    assert_eq!(command.enforcement, Enforcement::Full);
    assert_eq!(command.denial_signatures, ["permission denied"]);
    assert_eq!(
        command.runner_failure_rules,
        [RunnerFailureRule {
            allowed_exit_codes: vec![125],
            fatal_signatures: vec!["landlock-run: ".into()],
            informational_lines: vec![
                "landlock-run: partial enforcement (older Landlock ABI)".into()
            ],
        }]
    );
}

#[test]
fn a_multi_rung_chain_outside_a_runtime_refuses_without_caching() {
    let provider = LocalProvider::with_chain(vec![Runner::Bwrap, Runner::Landlock]);
    let Some(Err(error)) = poll_once(provider.confine(&os(&["true"]), &ro())) else {
        panic!("probing outside a runtime must refuse");
    };
    assert_eq!(error.mode, SandboxMode::ReadOnly);
    assert!(error.detail.is_some_and(|detail| detail.contains("Tokio")));
    assert!(provider.verdict.get().is_none());
}

#[test]
fn the_default_provider_is_local() {
    assert_eq!(default_provider().name(), "local");
}

#[test]
fn the_default_provider_is_built_once_and_shared() {
    assert!(Arc::ptr_eq(&default_provider(), &default_provider()));
}

#[cfg(unix)]
#[test]
fn bwrap_lookup_skips_relative_and_temporary_entries() {
    // `/bin/sh` stands in for bwrap: the lookup rules are name-independent.
    // A relative entry that climbs to `/bin` from any working directory.
    let relative_bin = format!("{}bin", "../".repeat(64));
    assert_eq!(find_program("sh", Some(OsStr::new(&relative_bin))), None);
    assert_eq!(find_program("sh", Some(OsStr::new(""))), None);
    // An executable under the temporary directory is never trusted.
    let dir = workspace().join(format!("bake-lookup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let fake = dir.join("sh");
    std::fs::write(&fake, "").expect("fake");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let path =
        std::env::join_paths([dir.clone(), relative_bin.into(), "/bin".into()]).expect("join");
    let found = find_program("sh", Some(&path));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(found, std::fs::canonicalize("/bin/sh").ok());
    assert_eq!(find_program("sh", None), None);
    assert_eq!(
        find_program("bake-no-such-program", Some(OsStr::new("/bin"))),
        None
    );
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[test]
fn platforms_without_a_backend_refuse_with_a_detail() {
    let provider = default_provider();
    let Some(Err(error)) = poll_once(provider.confine(&os(&["cmd", "/c", "exit", "0"]), &ww()))
    else {
        panic!("no backend must refuse");
    };
    assert_eq!(error.mode, SandboxMode::WorkspaceWrite);
    let detail = error.detail.unwrap_or_default();
    #[cfg(not(windows))]
    assert!(detail.contains("no "), "{detail}");
    // The restricted-token runner leaves deletion unconfined.
    #[cfg(windows)]
    assert_eq!(detail, windows_acl::DEFAULT_REFUSAL);
}

/// A workspace beside the test binary's directory: outside the temporary
/// directory, and not an ancestor of the runner.
struct BesideExe(std::path::PathBuf);

impl BesideExe {
    fn new(label: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let exe = std::env::current_exe().expect("exe");
        let base = exe.parent().and_then(Path::parent).expect("target dir");
        let dir = base.join(format!(
            "bake-acl-wrap-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        Self(crate::paths::canonical_path(&dir))
    }
}

impl Drop for BesideExe {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_sole_windows_acl_rung_wraps_with_the_current_executable_unprobed() {
    let provider = LocalProvider::with_chain(vec![Runner::WindowsAcl]);
    let workspace = BesideExe::new("sole");
    let temp = crate::paths::canonical_path(&std::env::temp_dir());
    for mode in [SandboxMode::ReadOnly, SandboxMode::WorkspaceWrite] {
        let policy = SandboxPolicy::new(mode, &workspace.0);
        let Some(Ok(command)) = poll_once(provider.confine(&os(&["cmd", "/c", "x"]), &policy))
        else {
            panic!("a sole candidate needs no probe");
        };
        assert_eq!(
            Some(command.program.clone()),
            std::env::current_exe().map(OsString::from).ok()
        );
        let mut expected = vec![OsString::from(windows_acl::HELPER_ARG)];
        expected.extend([
            "--workspace".into(),
            workspace.0.clone().into_os_string(),
            "--temp".into(),
            temp.clone().into_os_string(),
            "--mode".into(),
            mode.to_string().into(),
            "--".into(),
        ]);
        expected.extend(os(&["cmd", "/c", "x"]));
        assert_eq!(command.args, expected);
        assert_eq!(command.enforcement, Enforcement::Partial);
        assert_eq!(
            command.denial_signatures,
            [
                "access is denied",
                "access to the path",
                "permission denied",
                "operation not permitted"
            ]
        );
        assert_eq!(
            command.runner_failure_rules,
            [RunnerFailureRule {
                allowed_exit_codes: vec![127],
                fatal_signatures: vec!["windows-acl-run: ".into()],
                informational_lines: Vec::new(),
            }]
        );
    }
}

#[test]
fn the_windows_acl_rung_refuses_what_it_cannot_grant_distinctly() {
    let provider = LocalProvider::with_chain(vec![Runner::WindowsAcl]);
    // A workspace holding the temporary directory would pass its standing
    // capability to every private temp directory.
    let Some(Err(error)) = poll_once(provider.confine(&os(&["x"]), &ww())) else {
        panic!("a workspace containing the temp root must be refused");
    };
    assert!(
        error.detail.as_deref().is_some_and(
            |detail| detail.starts_with("Windows ACL temp root must be outside the workspace")
        ),
        "{error}"
    );
    // Read-only grants nothing, so the same workspace is fine there.
    assert!(matches!(
        poll_once(provider.confine(&os(&["x"]), &ro())),
        Some(Ok(_))
    ));
    // Extra roots have no capability of their own.
    let workspace = BesideExe::new("extra");
    let extra = BesideExe::new("extra-root");
    let policy = SandboxPolicy::new(SandboxMode::WorkspaceWrite, &workspace.0)
        .with_extra_writable_roots([extra.0.clone()]);
    let Some(Err(error)) = poll_once(provider.confine(&os(&["x"]), &policy)) else {
        panic!("extra roots must be refused");
    };
    assert!(
        error
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("extra writable roots")),
        "{error}"
    );
    // The runner is the current executable: a workspace holding it is refused.
    let exe = std::env::current_exe().expect("exe");
    let around_exe = crate::paths::canonical_path(exe.parent().expect("deps dir"));
    let policy = SandboxPolicy::new(SandboxMode::WorkspaceWrite, &around_exe);
    let Some(Err(error)) = poll_once(provider.confine(&os(&["x"]), &policy)) else {
        panic!("a runner under the workspace must be refused");
    };
    assert!(
        error
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("Windows ACL runner")),
        "{error}"
    );
}

#[test]
fn a_sole_appcontainer_rung_wraps_its_grants_unprobed() {
    let read = BesideExe::new("ac-read");
    let grants = windows_appcontainer::AppContainerGrants::new()
        .with_read_root(read.0.clone())
        .with_network(windows_appcontainer::NetworkGrant::InternetClient);
    let provider = LocalProvider::with_chain(vec![Runner::WindowsAppContainer])
        .with_windows_appcontainer_grants(grants);
    let workspace = BesideExe::new("ac-sole");
    for mode in [SandboxMode::ReadOnly, SandboxMode::WorkspaceWrite] {
        let policy = SandboxPolicy::new(mode, &workspace.0);
        let Some(Ok(command)) = poll_once(provider.confine(&os(&["cmd", "/c", "x"]), &policy))
        else {
            panic!("a sole candidate needs no probe");
        };
        assert_eq!(
            Some(command.program.clone()),
            std::env::current_exe().map(OsString::from).ok()
        );
        let mut expected = vec![OsString::from(windows_appcontainer::HELPER_ARG)];
        expected.extend([
            "--workspace".into(),
            workspace.0.clone().into_os_string(),
            "--mode".into(),
            mode.to_string().into(),
            "--read".into(),
            read.0.clone().into_os_string(),
            "--network".into(),
            "internet-client".into(),
            "--".into(),
        ]);
        expected.extend(os(&["cmd", "/c", "x"]));
        assert_eq!(command.args, expected);
        assert_eq!(command.enforcement, Enforcement::Partial);
        assert_eq!(
            command.runner_failure_rules,
            [RunnerFailureRule {
                allowed_exit_codes: vec![127],
                fatal_signatures: vec!["windows-appcontainer-run: ".into()],
                informational_lines: Vec::new(),
            }]
        );
        assert_eq!(
            command.denial_signatures,
            Runner::WindowsAcl.denial_signatures()
        );
    }
}

#[test]
fn the_appcontainer_rung_refuses_unsafe_policies_and_grants() {
    let refused = |provider: &LocalProvider, policy: &SandboxPolicy, expected: &str| {
        let Some(Err(error)) = poll_once(provider.confine(&os(&["x"]), policy)) else {
            panic!("{expected}: must be refused");
        };
        assert!(
            error
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains(expected)),
            "{expected}: {error}"
        );
    };
    let plain = LocalProvider::with_chain(vec![Runner::WindowsAppContainer]);
    // No temp root is involved: a workspace holding the ambient temporary
    // directory wraps under both modes.
    for policy in [ro(), ww()] {
        assert!(
            matches!(poll_once(plain.confine(&os(&["x"]), &policy)), Some(Ok(_))),
            "{}",
            policy.workspace_root.display()
        );
    }
    let workspace = BesideExe::new("ac-policy");
    let extra = BesideExe::new("ac-extra");
    let policy = SandboxPolicy::new(SandboxMode::WorkspaceWrite, &workspace.0)
        .with_extra_writable_roots([extra.0.clone()]);
    refused(&plain, &policy, "extra writable roots");
    let exe = std::env::current_exe().expect("exe");
    let around_exe = crate::paths::canonical_path(exe.parent().expect("deps dir"));
    let policy = SandboxPolicy::new(SandboxMode::ReadOnly, &around_exe);
    refused(&plain, &policy, "Windows AppContainer runner");
    let policy = SandboxPolicy::new(SandboxMode::ReadOnly, &workspace.0);
    for (grants, expected) in [
        (
            windows_appcontainer::AppContainerGrants::new()
                .with_network(windows_appcontainer::NetworkGrant::Loopback),
            "loopback network access is unsupported",
        ),
        (
            windows_appcontainer::AppContainerGrants::new().with_read_root(workspace.0.clone()),
            "overlaps the workspace",
        ),
        (
            windows_appcontainer::AppContainerGrants::new().with_read_root(extra.0.join("missing")),
            "is unusable",
        ),
    ] {
        let provider = LocalProvider::with_chain(vec![Runner::WindowsAppContainer])
            .with_windows_appcontainer_grants(grants);
        refused(&provider, &policy, expected);
    }
}

#[test]
fn a_multi_candidate_chain_never_probes_the_appcontainer_rung() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let provider = LocalProvider::with_chain(vec![
        Runner::WindowsAppContainer,
        Runner::WindowsAppContainer,
    ]);
    assert_eq!(runtime.block_on(provider.selected()), None);
}

#[cfg(unix)]
mod fake_runners {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A directory this test owns, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "bake-backends-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create scratch dir");
            Self(dir)
        }

        /// A `/bin/sh` script with `body`; returns its path.
        fn script(&self, name: &str, body: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, body).expect("write script");
            path
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn quote(path: &Path) -> String {
        format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
    }

    /// A fake runner that appends a line to `count` and exits `status`,
    /// printing `stdout` first.
    fn counting(scratch: &Scratch, name: &str, stdout: &str, status: i32) -> (PathBuf, PathBuf) {
        let count = scratch.path(&format!("{name}.count"));
        let script = scratch.script(
            name,
            &format!(
                "echo probe >> {}\nprintf '%s' '{stdout}'\nexit {status}\n",
                quote(&count)
            ),
        );
        (script, count)
    }

    fn calls(count: &Path) -> usize {
        std::fs::read_to_string(count)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    const FULL: &str = "landlock: fully enforced\n";
    const PARTIAL: &str = "landlock: partially enforced (older ABI)\n";

    fn linux(
        scratch: &Scratch,
        bwrap_status: i32,
        landlock_report: &str,
        landlock_status: i32,
    ) -> (LocalProvider, PathBuf, PathBuf, PathBuf) {
        let (bwrap, bwrap_count) = counting(scratch, "bwrap", "", bwrap_status);
        let (helper, helper_count) =
            counting(scratch, "landlock", landlock_report, landlock_status);
        let provider = LocalProvider::with_chain(vec![Runner::Bwrap, Runner::Landlock])
            .with_bwrap("/bin/sh", [bwrap.into_os_string()])
            .with_landlock_helper("/bin/sh", [helper.clone().into_os_string()]);
        (provider, bwrap_count, helper_count, helper)
    }

    #[tokio::test]
    async fn linux_probes_bwrap_first_and_wraps_with_its_dialect() {
        let scratch = Scratch::new();
        let (provider, bwrap_count, helper_count, _) = linux(&scratch, 0, FULL, 0);
        let command = provider
            .confine(&os(&["true"]), &ro())
            .await
            .expect("bwrap");
        let mut expected = vec![scratch.path("bwrap").into_os_string()];
        expected.extend(profiles::bwrap_profile_args(&ro()));
        expected.extend(os(&["--", "true"]));
        assert_eq!(command.program, "/bin/sh");
        assert_eq!(command.args, expected);
        assert_eq!(command.enforcement, Enforcement::Full);
        assert_eq!(command.denial_signatures, ["read-only file system"]);
        assert_eq!(
            command.runner_failure_rules,
            [RunnerFailureRule {
                fatal_signatures: vec!["bwrap: ".into()],
                ..RunnerFailureRule::default()
            }]
        );
        assert_eq!((calls(&bwrap_count), calls(&helper_count)), (1, 0));
    }

    #[tokio::test]
    async fn a_failing_bwrap_probe_falls_through_to_landlock() {
        // A host whose user namespaces are restricted: bwrap starts and
        // refuses, and the chain must reach Landlock, never run unconfined.
        let scratch = Scratch::new();
        let (provider, bwrap_count, helper_count, helper) = linux(&scratch, 1, FULL, 0);
        let command = provider
            .confine(&os(&["bash", "-c", "echo hi"]), &ww())
            .await
            .expect("landlock");
        let mut expected = vec![helper.into_os_string()];
        expected.extend(profiles::landlock_profile_args(&ww()));
        expected.extend(os(&["--", "bash", "-c", "echo hi"]));
        assert_eq!(command.args, expected);
        assert_eq!(command.enforcement, Enforcement::Full);
        assert_eq!(command.denial_signatures, ["permission denied"]);
        assert_eq!(
            command.runner_failure_rules,
            Runner::Landlock.runner_failure_rules()
        );
        assert_eq!((calls(&bwrap_count), calls(&helper_count)), (1, 1));
    }

    #[tokio::test]
    async fn a_missing_bwrap_falls_through_too() {
        let scratch = Scratch::new();
        let (provider, _, _, _) = linux(&scratch, 0, PARTIAL, 0);
        let provider = provider.with_bwrap(scratch.path("absent-bwrap"), []);
        assert_eq!(
            provider.selected().await,
            Some((Runner::Landlock, Enforcement::Partial))
        );
    }

    #[tokio::test]
    async fn a_partial_probe_report_is_partial_enforcement() {
        let scratch = Scratch::new();
        let (provider, _, _, _) = linux(&scratch, 1, PARTIAL, 0);
        let command = provider
            .confine(&os(&["true"]), &ro())
            .await
            .expect("landlock");
        assert_eq!(command.enforcement, Enforcement::Partial);
    }

    #[tokio::test]
    async fn only_the_contract_report_lines_count_as_landlock() {
        // Stricter than TypeScript, which read any zero exit as full.
        for (report, status) in [
            ("", 0),
            ("landlock: fully enforced", 0),
            ("ok\n", 0),
            (FULL, 125),
        ] {
            let scratch = Scratch::new();
            let (provider, _, _, _) = linux(&scratch, 1, report, status);
            assert_eq!(
                provider.confine(&os(&["true"]), &ro()).await,
                Err(SandboxUnavailable::new(SandboxMode::ReadOnly, None)),
                "{report:?} {status}"
            );
        }
    }

    #[tokio::test]
    async fn the_verdict_is_cached_for_the_provider_lifetime() {
        let scratch = Scratch::new();
        let (provider, bwrap_count, _, _) = linux(&scratch, 0, FULL, 0);
        provider
            .confine(&os(&["true"]), &ro())
            .await
            .expect("first");
        provider
            .confine(&os(&["true"]), &ww())
            .await
            .expect("second");
        assert_eq!(calls(&bwrap_count), 1);
    }

    #[tokio::test]
    async fn the_unavailable_verdict_is_cached_and_structured() {
        let scratch = Scratch::new();
        let (provider, bwrap_count, helper_count, _) = linux(&scratch, 1, "", 125);
        for _ in 0..2 {
            let error = provider
                .confine(&os(&["true"]), &ro())
                .await
                .expect_err("refused");
            assert_eq!(error, SandboxUnavailable::new(SandboxMode::ReadOnly, None));
            assert_eq!(error.code(), "SANDBOX_UNAVAILABLE");
        }
        assert_eq!((calls(&bwrap_count), calls(&helper_count)), (1, 1));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_first_calls_probe_once() {
        let scratch = Scratch::new();
        let (provider, bwrap_count, helper_count, _) = linux(&scratch, 1, FULL, 0);
        let provider = Arc::new(provider);
        let tasks: Vec<_> = (0..16)
            .map(|index| {
                let provider = Arc::clone(&provider);
                tokio::spawn(async move {
                    let policy = if index % 2 == 0 { ro() } else { ww() };
                    provider
                        .confine(&os(&["true"]), &policy)
                        .await
                        .map(|c| c.enforcement)
                })
            })
            .collect();
        for task in tasks {
            assert_eq!(task.await.expect("join"), Ok(Enforcement::Full));
        }
        assert_eq!((calls(&bwrap_count), calls(&helper_count)), (1, 1));
    }

    #[tokio::test]
    async fn a_sole_seatbelt_rung_is_selected_unprobed() {
        let scratch = Scratch::new();
        let (exec, count) = counting(&scratch, "sandbox-exec", "", 1);
        let provider = LocalProvider::with_chain(vec![Runner::Seatbelt])
            .with_seatbelt("/bin/sh", [exec.clone().into_os_string()]);
        let command = provider
            .confine(&os(&["bash", "-c", "echo hi"]), &ro())
            .await
            .expect("seatbelt");
        let mut expected = vec![exec.into_os_string()];
        expected.extend(profiles::seatbelt_profile_args(&ro()).unwrap_or_default());
        expected.extend(os(&["--", "bash", "-c", "echo hi"]));
        assert_eq!(command.args, expected);
        assert_eq!(command.enforcement, Enforcement::Full);
        assert_eq!(command.denial_signatures, ["operation not permitted"]);
        assert_eq!(
            command.runner_failure_rules,
            [RunnerFailureRule {
                fatal_signatures: vec!["sandbox-exec: ".into()],
                ..RunnerFailureRule::default()
            }]
        );
        assert_eq!(calls(&count), 0);
        // The product's macOS rung runs the system sandbox-exec.
        let product = LocalProvider::with_chain(vec![Runner::Seatbelt]);
        let command = product
            .confine(&os(&["true"]), &ro())
            .await
            .expect("seatbelt");
        assert_eq!(command.program, SEATBELT_EXEC);
    }

    #[tokio::test]
    async fn a_multi_rung_chain_probes_seatbelt_like_any_rung() {
        for (status, expected) in [(0, Some((Runner::Seatbelt, Enforcement::Full))), (1, None)] {
            let scratch = Scratch::new();
            let (bwrap, _) = counting(&scratch, "bwrap", "", 1);
            let (exec, count) = counting(&scratch, "sandbox-exec", "", status);
            let provider = LocalProvider::with_chain(vec![Runner::Bwrap, Runner::Seatbelt])
                .with_bwrap("/bin/sh", [bwrap.into_os_string()])
                .with_seatbelt("/bin/sh", [exec.into_os_string()]);
            assert_eq!(provider.selected().await, expected);
            assert_eq!(calls(&count), 1);
        }
    }

    #[tokio::test]
    async fn the_probe_timeout_bounds_a_slow_runner_and_reaps_it() {
        let scratch = Scratch::new();
        let pid_file = scratch.path("pid");
        let slow = scratch.script(
            "slow",
            &format!("echo $$ > {}\nexec sleep 30\n", quote(&pid_file)),
        );
        let provider = LocalProvider::with_chain(vec![Runner::Bwrap, Runner::Landlock])
            .with_bwrap(scratch.path("absent"), [])
            .with_landlock_helper("/bin/sh", [slow.into_os_string()])
            .with_probe_timeout(Duration::from_secs(1));
        let started = std::time::Instant::now();
        assert!(provider.confine(&os(&["true"]), &ro()).await.is_err());
        assert!(started.elapsed() < Duration::from_secs(20));
        // The killed probe was awaited: its pid no longer names a process.
        if let Some(pid) = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|text| text.trim().parse::<libc::pid_t>().ok())
        {
            // SAFETY: signal 0 only checks for existence.
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            assert!(!alive, "probe {pid} outlived its timeout");
        }
    }

    #[tokio::test]
    async fn a_zero_probe_timeout_refuses() {
        let scratch = Scratch::new();
        let (provider, _, _, _) = linux(&scratch, 0, FULL, 0);
        let provider = provider.with_probe_timeout(Duration::ZERO);
        assert!(provider.confine(&os(&["true"]), &ro()).await.is_err());
    }

    /// Dropping a confine future mid-probe kills the probe. A FIFO gates the
    /// drop on the runner having started.
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropping_a_probe_kills_its_runner() {
        let scratch = Scratch::new();
        let fifo = scratch.path("gate");
        let pid_file = scratch.path("pid");
        let fifo_c = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).expect("path");
        // SAFETY: `fifo_c` is a valid path string.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        let gated = scratch.script(
            "gated",
            &format!(
                "echo $$ > {pid}\necho go > {fifo}\nexec sleep 30\n",
                pid = quote(&pid_file),
                fifo = quote(&fifo)
            ),
        );
        let provider = Arc::new(
            LocalProvider::with_chain(vec![Runner::Bwrap, Runner::Landlock])
                .with_bwrap("/bin/sh", [gated.into_os_string()]),
        );
        let mut task = {
            let provider = Arc::clone(&provider);
            tokio::spawn(async move { provider.confine(&os(&["true"]), &ro()).await.is_ok() })
        };
        let gate = fifo.clone();
        let mut reader = tokio::task::spawn_blocking(move || std::fs::read_to_string(gate));
        tokio::select! {
            opened = &mut reader => {
                opened.expect("join").expect("gate");
            }
            finished = &mut task => {
                // No probe ran: open the FIFO's write end so the blocked
                // reader returns and the runtime can shut down.
                drop(std::fs::OpenOptions::new().write(true).open(&fifo));
                let _ = reader.await;
                panic!("the confine finished without probing: {finished:?}");
            }
        }
        let pid: libc::pid_t = std::fs::read_to_string(&pid_file)
            .expect("pid")
            .trim()
            .parse()
            .expect("pid");
        task.abort();
        assert!(task.await.is_err_and(|error| error.is_cancelled()));
        assert!(
            provider.verdict.get().is_none(),
            "a dropped walk caches nothing"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let state = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok();
            let running = state.as_deref().is_some_and(|stat| {
                stat.rsplit_once(") ")
                    .is_some_and(|(_, rest)| !rest.starts_with('Z') && !rest.starts_with('X'))
            });
            if !running {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "probe {pid} survived its drop"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// The runner-location and root-integrity refusals, through `confine`.
#[cfg(unix)]
mod integrity {
    use super::*;
    use std::path::PathBuf;

    struct Dir(PathBuf);

    impl Dir {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = workspace().join(format!(
                "bake-integrity-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("dir");
            Self(dir)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn refusal(provider: &LocalProvider, policy: &SandboxPolicy) -> String {
        match poll_once(provider.confine(&os(&["true"]), policy)) {
            Some(Err(error)) => {
                assert_eq!(error.mode, policy.mode);
                error.detail.unwrap_or_default()
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_helper_inside_the_workspace_is_refused_in_every_mode() {
        let dir = Dir::new("helper");
        let helper = dir.0.join("bake");
        std::fs::write(&helper, "").expect("helper");
        let provider = LocalProvider::with_chain(vec![Runner::Landlock])
            .with_landlock_helper(helper.clone(), [OsString::from(landlock::HELPER_ARG)]);
        for mode in [SandboxMode::ReadOnly, SandboxMode::WorkspaceWrite] {
            let detail = refusal(&provider, &SandboxPolicy::new(mode, &dir.0));
            assert!(
                detail.contains("Landlock helper")
                    && detail.contains("lies under the writable root"),
                "{detail}"
            );
        }
        // An extra root over the helper's directory is refused the same way.
        let elsewhere = Dir::new("helper-ws");
        let policy = SandboxPolicy::new(SandboxMode::ReadOnly, &elsewhere.0)
            .with_extra_writable_roots([dir.0.clone()]);
        assert!(refusal(&provider, &policy).contains("lies under"));
        // The temporary directories are writable roots of every
        // `workspace-write` call, so a helper there is refused whatever the
        // workspace.
        assert!(
            refusal(
                &provider,
                &SandboxPolicy::new(SandboxMode::ReadOnly, &elsewhere.0)
            )
            .contains("lies under")
        );
    }

    #[test]
    fn a_bwrap_inside_the_workspace_is_refused() {
        let dir = Dir::new("bwrap");
        let bwrap = dir.0.join("bin/bwrap");
        std::fs::create_dir_all(dir.0.join("bin")).expect("bin");
        std::fs::write(&bwrap, "").expect("bwrap");
        let provider = LocalProvider::with_chain(vec![Runner::Bwrap]).with_bwrap(bwrap, []);
        let detail = refusal(
            &provider,
            &SandboxPolicy::new(SandboxMode::ReadOnly, &dir.0),
        );
        assert!(detail.contains("bubblewrap"), "{detail}");
        let relative = LocalProvider::with_chain(vec![Runner::Bwrap]).with_bwrap("bwrap", []);
        assert!(refusal(&relative, &ro()).contains("not an absolute path"));
    }

    #[test]
    fn a_missing_bwrap_on_a_sole_rung_is_refused_with_a_detail() {
        let provider =
            LocalProvider::with_chain(vec![Runner::Bwrap]).with_search_path("/nonexistent");
        assert!(refusal(&provider, &ro()).contains("not found on PATH"));
    }

    #[test]
    fn a_writable_root_that_changed_is_refused() {
        let dir = Dir::new("roots");
        let real = dir.0.join("real");
        std::fs::create_dir_all(real.join("sub")).expect("real");
        std::os::unix::fs::symlink(&real, dir.0.join("link")).expect("link");
        std::fs::write(dir.0.join("file"), "").expect("file");
        let provider =
            LocalProvider::with_chain(vec![Runner::Landlock]).with_landlock_helper("/bin/true", []);
        let cases = [
            (dir.0.join("missing"), "no longer exists"),
            (dir.0.join("link"), "symbolic link"),
            (dir.0.join("link/sub"), "resolves to"),
            (dir.0.join("file"), "no longer a directory"),
            (PathBuf::from("relative-ws"), ""),
        ];
        for (root, why) in cases {
            for policy in [
                SandboxPolicy::new(SandboxMode::WorkspaceWrite, &root),
                SandboxPolicy::new(SandboxMode::WorkspaceWrite, &real)
                    .with_extra_writable_roots([root.clone()]),
            ] {
                let detail = refusal(&provider, &policy);
                assert!(
                    detail.contains("changed since the session started") && detail.contains(why),
                    "{}: {detail}",
                    root.display()
                );
            }
        }
        // A read-only call has no writable roots to check.
        let read_only = SandboxPolicy::new(SandboxMode::ReadOnly, dir.0.join("link"));
        assert!(matches!(
            poll_once(provider.confine(&os(&["true"]), &read_only)),
            Some(Ok(_))
        ));
        let intact = SandboxPolicy::new(SandboxMode::WorkspaceWrite, &real);
        assert!(matches!(
            poll_once(provider.confine(&os(&["true"]), &intact)),
            Some(Ok(_))
        ));
    }
}
