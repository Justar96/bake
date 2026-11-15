//! `EIO` returned by the kernel, through a seccomp filter, from the
//! session writer's write and sync system calls. A child process
//! re-running this test binary opens the session, then installs a seccomp
//! filter (`kernel_faults`) that fails `write` and `pwrite64`, or `fsync`
//! and `fdatasync`, with `EIO` on every descriptor from the lowest free
//! one up: the session file the next append opens, or a migration's
//! temporary file. A group's line is chosen by its
//! length, which the filter matches against the write's byte count. No
//! Rust seam is involved: the writer gets `EIO` back from the real system
//! call. This shows how the writer handles that error, not how a failing
//! device behaves. A device rarely fails a buffered `write`, which only
//! copies into the page cache; and after a real `fsync` failure the
//! kernel may drop the dirty pages, so a reader may no longer see bytes
//! the write put there, while the filter leaves the page cache intact.
//!
//! The child checks the error the writer returns (`SessionError::Io` or
//! `GroupWriteError`, naming `EIO`) and that the manager keeps what Pi
//! keeps, without a panic; the parent checks the bytes left, that the
//! file loads as Pi's loader loads it, rebuilds a provider-valid request,
//! and is taken over by the next writer. Where seccomp is unavailable the
//! tests skip with a note, unless `BAKE_REQUIRE_KERNEL_FAULTS=1`.
//!
//! Each test owns its temporary directory and the child processes it
//! starts; the filter is the child's alone.

use std::io::Write;
use std::path::{Path, PathBuf};

use bake_ai::Message;
use serde_json::Value;

use super::*;
use crate::session::manager::crash_support::{
    CHILD_WATCHDOG_EXIT, Conversation, PATIENCE, TestChild, context_of, conversation, entries_of,
    file_session, lines, listed, migrated, names, provider_problem, read, request, take_over,
    temps, tool_result, tool_turn, v2_session,
};
use crate::session::manager::kernel_faults::{Calls, EioFilter, lowest_free_fd, skip_unavailable};
use crate::session::{EditContent, FailedLine};
use crate::test_support::TempDir;

const CHILD_ENV: &str = "BAKE_TEST_EIO_WRITER";
const CHILD_TEST: &str = "session::manager::seccomp_write_tests::eio_writer_child";
const UNAVAILABLE: &str = "bake-test-seccomp-unavailable";
const CHECKED: &str = "bake-test-eio-checked";

/// A group whose lines all differ in length, so a filter matching a byte
/// count fails exactly one of them: two tool results replaced with text of
/// different lengths, then a model change.
fn redaction(ids: &Conversation) -> Vec<GroupEntry> {
    vec![
        GroupEntry::context_edit(
            ids.results[0].clone(),
            Some(EditContent::Text("first result, redacted".into())),
        ),
        GroupEntry::context_edit(
            ids.results[1].clone(),
            Some(EditContent::Text("second".into())),
        ),
        GroupEntry::model_change("faux", "faux-2"),
    ]
}

fn say(mark: &str) {
    println!("{mark}");
    let _ = std::io::stdout().flush();
}

/// Not a test of its own: the child of the tests below, which re-run this
/// test binary with [`CHILD_ENV`] set to `what|calls|count|file`: what to
/// write (`single`, `group`, or `migrate`), the calls to fail (`write` or
/// `sync`), the byte count a failed write must have (`0` for any), and,
/// for a group, the index of the line expected to fail.
#[test]
fn eio_writer_child() {
    let Some(spec) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    let spec = spec.to_string_lossy().into_owned();
    let parts: Vec<&str> = spec.splitn(5, '|').collect();
    let [what, calls, count, line, file] = parts[..] else {
        panic!("bad spec {spec}");
    };
    let calls = Calls::parse(calls).expect("calls");
    let count: u32 = count.parse().expect("count");
    let line: usize = line.parse().expect("line");
    let file = Path::new(file);
    std::thread::spawn(|| {
        std::thread::sleep(PATIENCE * 2);
        std::process::exit(CHILD_WATCHDOG_EXIT);
    });
    let arm = || {
        let filter = EioFilter {
            calls,
            fds: lowest_free_fd()..u32::MAX,
            count: (count > 0).then_some(count),
        };
        if let Err(error) = filter.install() {
            say(&format!("{UNAVAILABLE}: {error}"));
            std::process::exit(0);
        }
    };
    match what {
        "single" => {
            let mut manager = SessionManager::open(file, None, None).expect("child open");
            let before = manager.entries().len();
            arm();
            let error = manager
                .append_message(tool_result("c2"))
                .expect_err("an append the kernel fails");
            assert_eio(&error, file);
            // As in Pi, the entry stays in memory as the leaf, and the
            // owner, told of the failure, stops writing.
            let entries = manager.entries();
            assert_eq!(entries.len(), before + 1);
            assert_eq!(manager.leaf_id(), Some(entries[before].id()));
        }
        "group" => {
            let mut manager = SessionManager::open(file, None, None).expect("child open");
            let ids = Conversation::of(&manager);
            let before = manager.entries().len();
            arm();
            let result = manager.append_group(redaction(&ids));
            let Err(SessionError::GroupWrite(error)) = result else {
                panic!("not a group write error: {result:?}");
            };
            let failed = match calls {
                Calls::Write => FailedLine::NotWritten,
                Calls::Sync => FailedLine::Unsynced,
            };
            assert_eq!(
                (error.written.len(), error.len, error.failed),
                (line, 3, failed)
            );
            assert_eq!(error.path, file);
            assert_eq!(error.source.raw_os_error(), Some(libc::EIO), "{error}");
            // The manager keeps the synced lines alone.
            let entries = manager.entries();
            let kept: Vec<&str> = entries[before..].iter().map(|entry| entry.id()).collect();
            assert_eq!(kept, error.written);
            assert_eq!(manager.leaf_id(), entries.last().map(|entry| entry.id()));
        }
        "migrate" => {
            arm();
            let error =
                SessionManager::open(file, None, None).expect_err("a migration the kernel fails");
            assert_eio(&error, file);
        }
        other => panic!("bad write {other}"),
    }
    say(CHECKED);
    std::process::exit(0);
}

/// `error` is the kernel's `EIO` for `file`.
fn assert_eio(error: &SessionError, file: &Path) {
    let SessionError::Io { path, source } = error else {
        panic!("not an I/O error: {error:?}");
    };
    assert_eq!(path.file_name(), file.file_name(), "{error}");
    assert_eq!(source.raw_os_error(), Some(libc::EIO), "{error}");
}

/// Run [`eio_writer_child`] with `spec`; `false` when seccomp is
/// unavailable and the test may skip.
fn run(spec: &str, case: &str) -> bool {
    let mut child = TestChild::spawn(CHILD_TEST, CHILD_ENV, spec);
    let (status, output) = child.wait();
    if let Some(line) = output.iter().find(|line| line.contains(UNAVAILABLE)) {
        skip_unavailable("a seccomp filter", line);
        return false;
    }
    assert!(status.success(), "{case}: {status}: {output:?}");
    assert!(
        output.iter().any(|line| line.contains(CHECKED)),
        "{case}: {output:?}"
    );
    true
}

/// A session file a child writes to, and the lines the same write adds to
/// a copy.
struct Session {
    _dir: TempDir,
    file: PathBuf,
    base: Vec<u8>,
    /// How many entries the base holds.
    entries: usize,
    added: Vec<Vec<u8>>,
}

impl Session {
    /// A tool turn with its first result: a single append writes the
    /// second.
    fn single() -> Self {
        let dir = TempDir::new("eio-single");
        let mut manager = file_session(&dir);
        tool_turn(&mut manager);
        manager
            .append_message(tool_result("c1"))
            .expect("first result");
        Self::rehearse(dir, manager, |manager| {
            manager.append_message(tool_result("c2")).expect("append");
        })
    }

    /// A written conversation, which [`redaction`] edits.
    fn group() -> Self {
        let dir = TempDir::new("eio-group");
        let mut manager = file_session(&dir);
        conversation(&mut manager);
        Self::rehearse(dir, manager, |manager| {
            let ids = Conversation::of(manager);
            manager.append_group(redaction(&ids)).expect("group");
        })
    }

    fn rehearse(
        dir: TempDir,
        manager: SessionManager,
        write: impl FnOnce(&mut SessionManager),
    ) -> Self {
        let file = manager.session_file().expect("file").to_path_buf();
        drop(manager);
        let base = read(&file);
        let rehearsal = TempDir::new("eio-rehearsal");
        let copy = rehearsal.join("copy.jsonl");
        std::fs::write(&copy, &base).expect("copy");
        let mut manager = SessionManager::open(&copy, None, None).expect("open copy");
        write(&mut manager);
        drop(manager);
        let after = read(&copy);
        assert!(after.starts_with(&base));
        let added = lines(&after[base.len()..])
            .into_iter()
            .map(<[u8]>::to_vec)
            .collect();
        Self {
            entries: entries_of(&file).len(),
            _dir: dir,
            file,
            base,
            added,
        }
    }

    fn spec(&self, what: &str, calls: &str, count: usize, line: usize) -> String {
        format!("{what}|{calls}|{count}|{line}|{}", self.file.display())
    }

    /// The file holds the base and `kept` whole lines like the first
    /// `kept` added ones (entry ids and timestamps differ, not their
    /// lengths), loads them, rebuilds a provider-valid request, and is
    /// taken over by the next writer, whose append starts a line of its
    /// own.
    fn check_left(&self, kept: usize, case: &str) {
        let bytes = read(&self.file);
        assert!(bytes.starts_with(&self.base), "{case}");
        let tail = lines(&bytes[self.base.len()..]);
        let shapes = |lines: &[&[u8]]| -> Vec<(usize, Value)> {
            lines
                .iter()
                .map(|line| {
                    assert!(line.ends_with(b"\n"), "{case}: a torn line");
                    let entry: Value = serde_json::from_slice(line).expect("a whole line");
                    (line.len(), entry["type"].clone())
                })
                .collect()
        };
        let expected: Vec<&[u8]> = self.added[..kept].iter().map(Vec::as_slice).collect();
        assert_eq!(shapes(&tail), shapes(&expected), "{case}");
        let before = entries_of(&self.file).len();
        assert_eq!(before, self.entries + kept, "{case}");
        let context = context_of(&self.file);
        assert_eq!(provider_problem(&request(&context)), None, "{case}");
        let mut taken = take_over(&self.file);
        assert_eq!(taken.entries().len(), before, "{case}");
        taken.append_session_info("after").expect("append after");
        drop(taken);
        let after = read(&self.file);
        let [info] = &lines(&after[bytes.len()..])[..] else {
            panic!("{case}: not one appended line");
        };
        let info: Value = serde_json::from_slice(info).expect("the next writer's line");
        assert_eq!(info["type"], "session_info", "{case}");
        let context = context_of(&self.file);
        assert_eq!(provider_problem(&request(&context)), None, "{case}");
    }
}

/// An append whose `write` the kernel fails with `EIO` writes nothing,
/// returns the error, and keeps the entry in memory as Pi does; the file
/// loads with the second call answered by the error result that stands in
/// for a lost one, and the next writer takes it over.
#[test]
fn an_append_whose_write_fails_with_eio_leaves_the_file_as_it_was() {
    let session = Session::single();
    if !run(&session.spec("single", "write", 0, 0), "single write") {
        return;
    }
    let messages = request(&context_of(&session.file));
    let Some(Message::ToolResult(result)) = messages.last() else {
        panic!("{messages:?}");
    };
    assert_eq!(
        (result.tool_call_id.as_str(), result.is_error),
        ("c2", true)
    );
    session.check_left(0, "single write");
}

/// An append whose `fdatasync` the filter fails with `EIO` returns the
/// error after the whole line reached the page cache, which the filter
/// leaves intact: a reader sees the entry, though a power loss may still
/// drop it.
#[test]
fn an_append_whose_sync_fails_with_eio_reports_the_unsynced_line() {
    let session = Session::single();
    if !run(&session.spec("single", "sync", 0, 0), "single sync") {
        return;
    }
    session.check_left(1, "single sync");
}

/// A group whose `write` of any one line the kernel fails with `EIO` keeps
/// the lines before it, synced, and writes nothing of that line or later.
#[test]
fn a_group_whose_line_write_fails_with_eio_keeps_the_lines_before_it() {
    let lengths: Vec<usize> = Session::group().added.iter().map(Vec::len).collect();
    let mut distinct = lengths.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), 3, "the lines must differ in length");
    for (line, len) in lengths.into_iter().enumerate() {
        let case = format!("group line {line} of {len} bytes");
        let session = Session::group();
        if !run(&session.spec("group", "write", len, line), &case) {
            return;
        }
        session.check_left(line, &case);
    }
}

/// A group whose first `fdatasync` the filter fails with `EIO` stops after
/// that line, reported unsynced; with the page cache intact, the line is
/// in the file and loads.
#[test]
fn a_group_whose_sync_fails_with_eio_stops_at_the_unsynced_line() {
    let session = Session::group();
    if !run(&session.spec("group", "sync", 0, 0), "group sync") {
        return;
    }
    session.check_left(1, "group sync");
}

/// A migration whose temporary file's `write` or sync the kernel fails
/// with `EIO` fails the open, removes its temporary file, and leaves the
/// old file, which the next writer migrates.
#[test]
fn a_migration_whose_temporary_file_fails_with_eio_keeps_the_old_file() {
    for calls in ["write", "sync"] {
        let case = format!("migration {calls}");
        let dir = TempDir::new("eio-migration");
        let sessions = dir.join("sessions");
        let cwd = sessions.to_string_lossy().into_owned();
        let (file, old) = v2_session(&sessions);
        let (new, context) = migrated(&old);
        let spec = format!("migrate|{calls}|0|0|{}", file.display());
        if !run(&spec, &case) {
            return;
        }
        assert!(read(&file) == old, "{case}: the old file changed");
        assert!(temps(&sessions).is_empty(), "{case}");
        let name = file.file_name().expect("name").to_string_lossy();
        assert_eq!(
            names(&sessions),
            [name.to_string(), format!("{name}.lock")],
            "{case}"
        );
        assert_eq!(listed(&cwd, &sessions), std::slice::from_ref(&file));
        let taken = take_over(&file);
        assert_eq!(taken.build_session_context(), context, "{case}");
        drop(taken);
        assert!(read(&file) == new, "{case}: not migrated");
    }
}
