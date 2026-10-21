//! A session whose file write the kernel fails with `EIO`, Linux only. A
//! child process re-running this test binary answers a first prompt, then
//! installs a seccomp filter on every thread
//! (`session::manager::kernel_faults`) that fails `write` and `pwrite64`,
//! or `fsync` and `fdatasync`, with `EIO` on the descriptor the session
//! file's next append opens: the lowest free one. The next prompt's write
//! gets `EIO` back from the real system call, with no Rust seam: the
//! session stops before another request and takes no more prompts,
//! without a panic. The parent then checks the file loads, rebuilds a
//! provider-valid request, and is taken over. This shows how the session
//! handles `EIO` from those calls, with the page cache intact; a failing
//! device rarely fails a buffered `write`, and after its `fsync` fails the
//! kernel may drop the dirty pages. The parent bounds the child with a
//! watchdog and kills and reaps it on every path. Where seccomp is
//! unavailable the test skips with a note, unless
//! `BAKE_REQUIRE_KERNEL_FAULTS=1`.

use super::rerun::conversation;
use super::*;
use crate::session::manager::crash_support::TestChild;
use crate::session::manager::kernel_faults::{Calls, EioFilter, lowest_free_fd, skip_unavailable};

const CHILD_DIR: &str = "BAKE_TEST_EIO_SESSION_DIR";
const CHILD_CALLS: &str = "BAKE_TEST_EIO_SESSION_CALLS";
const CHILD_TEST: &str = "agent_session::recovery_tests::run_journal_tests::eio::eio_session_child";
const UNAVAILABLE: &str = "bake-test-seccomp-unavailable";
const CHECKED: &str = "bake-test-eio-session-checked";

fn say(line: &str) {
    println!("{line}");
    let _ = std::io::Write::flush(&mut std::io::stdout());
}

/// Not a test of its own: the child of the test below, run with
/// [`CHILD_DIR`] naming its directory and [`CHILD_CALLS`] the calls to
/// fail (`write` or `sync`).
#[test]
fn eio_session_child() {
    let Some(directory) = std::env::var_os(CHILD_DIR) else {
        return;
    };
    let directory = PathBuf::from(directory);
    let calls = std::env::var(CHILD_CALLS).unwrap();
    let calls = Calls::parse(&calls).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async move {
        let h = harness(200_000);
        h.faux.set_responses(vec![
            faux_assistant_message("one").into(),
            faux_assistant_message("two").into(),
        ]);
        let manager = SessionManager::create(
            &directory.to_string_lossy(),
            Some(&directory.join("sessions")),
            NewSessionOptions::default(),
        )
        .unwrap();
        let session = AgentSession::create(options(&h, manager, Settings::default()))
            .await
            .unwrap();
        session.prompt("first", Vec::new()).await.unwrap();
        let path = lock(&session.session).session_file().unwrap().to_path_buf();
        std::fs::write(
            directory.join("session-path"),
            path.to_string_lossy().as_bytes(),
        )
        .unwrap();
        assert_eq!(h.faux.call_count(), 1);

        let fd = lowest_free_fd();
        let filter = EioFilter {
            calls,
            fds: fd..fd + 1,
            count: None,
        };
        if let Err(error) = filter.install_on_every_thread() {
            say(&format!("{UNAVAILABLE}: {error}"));
            std::process::exit(0);
        }
        let result = session.prompt("second", Vec::new()).await;
        let failure = session.persistence_failure();
        // The kernel's error stopped the session: no request followed the
        // failed write, and no later prompt is accepted.
        assert!(
            failure
                .as_deref()
                .is_some_and(|error| error.contains("os error 5")),
            "{failure:?}, prompt {result:?}"
        );
        assert_eq!(h.faux.call_count(), 1, "a request after the failed write");
        let refused = session
            .prompt("third", Vec::new())
            .await
            .expect_err("a prompt after the failed write");
        assert!(refused.contains("takes no more prompts"), "{refused}");
        assert_eq!(h.faux.call_count(), 1);
        session.shutdown().await;
        say(CHECKED);
        std::process::exit(0);
    });
}

/// The request `manager` rebuilds, as every provider adapter sends it.
fn request_of(manager: &SessionManager) -> Vec<Message> {
    let live: Vec<_> = manager
        .build_session_context()
        .messages
        .iter()
        .map(|message| message.to_agent())
        .collect();
    bake_ai::api::transform_messages::transform_messages(
        &crate::session::messages::convert_to_llm(&live),
        harness(200_000).faux.model(),
        None,
    )
}

/// A prompt whose session write the kernel fails with `EIO` stops the
/// session: a failed `write` leaves the file at the first exchange, a
/// failed sync leaves the second prompt written but unanswered. Either
/// file loads, its next request is provider-valid, and the next writer
/// takes it over.
#[test]
fn a_session_write_failing_with_eio_stops_the_session() {
    for calls in ["write", "sync"] {
        let dir = TempDir::new(&format!("run-journal-eio-{calls}"));
        // Waits at most `PATIENCE` for the child to end, then kills it;
        // dropped on any failure, it kills and reaps the child.
        let mut child = TestChild::try_spawn(
            CHILD_TEST,
            CHILD_DIR,
            &dir.path().to_string_lossy(),
            |command| {
                command.env(CHILD_CALLS, calls);
            },
        )
        .unwrap();
        let (status, output) = child.wait();
        drop(child);
        if let Some(line) = output.iter().find(|line| line.contains(UNAVAILABLE)) {
            skip_unavailable("a seccomp filter", line);
            return;
        }
        assert!(status.success(), "{calls}: {status}: {output:?}");
        assert!(
            output.iter().any(|line| line.contains(CHECKED)),
            "{calls}: {output:?}"
        );

        let path = PathBuf::from(std::fs::read_to_string(dir.join("session-path")).unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with('\n'), "{calls}: a torn line");
        let mut taken = SessionManager::open(&path, None, None).unwrap();
        let expected: &[&str] = match calls {
            "write" => &["user: first", "assistant: "],
            _ => &["user: first", "assistant: ", "user: second"],
        };
        assert_eq!(conversation(&request_of(&taken)), expected, "{calls}");
        taken.append_session_info("after").unwrap();
        drop(taken);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.starts_with(&text), "{calls}");
        assert_eq!(after[text.len()..].lines().count(), 1, "{calls}");
    }
}
