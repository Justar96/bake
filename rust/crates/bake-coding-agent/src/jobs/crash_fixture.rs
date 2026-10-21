//! A crash fixture for the scope 11 restart tests of jobs, subagents, and
//! reminders: the test re-executes one of its own fixture tests as a child
//! process, waits until the fixture reports its kill point, and kills it
//! with `SIGKILL`, which bypasses every destructor, shutdown, and flush.
//! The test then reopens what the killed process left in its directory.
//!
//! The pattern is the run journal's (`agent_session::run_journal_tests`);
//! this copy belongs to the orchestration tests so each owns its fixture.

use std::path::Path;
use std::time::Duration;

/// The file a fixture writes once it reaches its kill point.
const READY: &str = "ready";

/// Reports the fixture's kill point to the test, then holds the calling
/// thread there until the test's `SIGKILL`.
pub(crate) fn halt(directory: &Path) -> ! {
    std::fs::write(directory.join(READY), b"").expect("report the kill point");
    loop {
        std::thread::park();
    }
}

/// Runs the fixture test `name` (its full path in this test binary) as a
/// child process with `env`, waits up to a minute for it to report its kill
/// point in `directory` ([`halt`]), and kills it with `SIGKILL`. Fails if
/// the fixture exits first or its exit is anything but that kill.
pub(crate) async fn kill_fixture(name: &str, env: &[(&str, &std::ffi::OsStr)], directory: &Path) {
    let mut child = tokio::process::Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", name, "--nocapture"])
        .envs(env.iter().copied())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn the fixture");
    let watchdog = tokio::time::Instant::now() + Duration::from_secs(60);
    while !directory.join(READY).exists() {
        if let Some(status) = child.try_wait().expect("fixture status") {
            panic!("{name} {env:?}: the fixture exited ({status}) before its kill point");
        }
        assert!(
            tokio::time::Instant::now() < watchdog,
            "{name} {env:?}: the fixture did not reach its kill point"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    child.kill().await.expect("SIGKILL the fixture");
    let status = child.wait().await.expect("reap the fixture");
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            status.signal(),
            Some(libc::SIGKILL),
            "{name} {env:?}: {status}"
        );
    }
    #[cfg(not(unix))]
    assert!(!status.success(), "{name} {env:?}: {status}");
}
