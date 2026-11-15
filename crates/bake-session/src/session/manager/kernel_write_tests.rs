//! A writer stopped by the kernel inside a `write` system call, and real
//! write errors. With a file size limit (`RLIMIT_FSIZE`) a chosen number
//! of bytes into a line, Linux writes the line up to the limit and returns
//! a short count; the writer's next `write`, of the rest, raises `SIGXFSZ`
//! and fails with `EFBIG`. Under the signal's default action the writer
//! dies inside that call; with the signal ignored or caught, the call
//! returns `EFBIG` to it. Either way the file holds exactly the bytes up
//! to the limit, so a test checks every line of an append, a group, and a
//! migration's temporary file, stopped at its first byte, after one byte,
//! half way, and before its newline. `/dev/full` gives a real `ENOSPC`.
//!
//! Linux only. macOS checks `RLIMIT_FSIZE` against the descriptor's offset
//! rather than the end of an `O_APPEND` file: measured on macOS 26.7.1
//! (Intel), a fresh append descriptor on a file already at the limit
//! writes past it with no signal, and one a byte short writes a whole
//! line. The limit cannot stop a macOS writer at a chosen byte, so these
//! cases have no macOS form.
//!
//! Each test owns its temporary directory and the child processes it
//! starts; the limit and the signal disposition are the child's alone.

use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::atomic::{AtomicUsize, Ordering};

use bake_ai::Message;
use serde_json::Value;

use super::*;
use crate::session::manager::crash_support::{
    CHILD_RETURNED_EXIT, CHILD_WATCHDOG_EXIT, Conversation, PATIENCE, TestChild, context_of,
    conversation, entries_of, file_session, is_temp_of, lines, listed, migrated, names, omission,
    provider_problem, read, request, take_over, temps, tool_result, tool_turn, v2_session,
};
use crate::session::{FailedLine, LeafSelector, SessionEntry, build_session_context};
use crate::test_support::TempDir;

const CHILD_ENV: &str = "BAKE_TEST_LIMITED_WRITER";
const CHILD_TEST: &str = "session::manager::kernel_write_tests::limited_writer_child";
const ARMED: &str = "bake-test-size-limit-armed";
const RETURNED: &str = "bake-test-limited-write-returned";
const CHECKED: &str = "bake-test-limited-write-checked";

/// How many `SIGXFSZ` signals the child caught.
static CAUGHT: AtomicUsize = AtomicUsize::new(0);

extern "C" fn catch(_: libc::c_int) {
    CAUGHT.fetch_add(1, Ordering::SeqCst);
}

/// Not a test of its own: the child of the tests below, which re-run this
/// test binary with [`CHILD_ENV`] set to `what|on|limit|line|torn|file`:
/// what to write (`single`, `group`, or `migrate`), the `SIGXFSZ`
/// disposition (`default`, `ignore`, or `count`), the file size limit, and
/// the line, and how many of its bytes, the limit lets through, which a
/// child that survives checks its error against.
#[test]
fn limited_writer_child() {
    let Some(spec) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    let spec = spec.to_string_lossy().into_owned();
    let parts: Vec<&str> = spec.splitn(6, '|').collect();
    let [what, on, limit, line, torn, file] = parts[..] else {
        panic!("bad spec {spec}");
    };
    let limit: u64 = limit.parse().expect("limit");
    let line: usize = line.parse().expect("line");
    let torn: usize = torn.parse().expect("torn");
    let file = Path::new(file);
    prepare(on);
    std::thread::spawn(|| {
        // The write ends the child, or returns; a child that hangs instead
        // does not stay behind for long.
        std::thread::sleep(PATIENCE * 2);
        std::process::exit(CHILD_WATCHDOG_EXIT);
    });
    match what {
        "single" => {
            let mut manager = SessionManager::open(file, None, None).expect("child open");
            let before = manager.entries().len();
            arm(limit);
            let result = manager.append_message(tool_result("c2"));
            returned(on);
            assert_efbig(&result.expect_err("a write over the limit"), file);
            // As in Pi, the entry stays in memory as the leaf.
            let entries = manager.entries();
            assert_eq!(entries.len(), before + 1);
            assert_eq!(manager.leaf_id(), Some(entries[before].id()));
        }
        "group" => {
            let mut manager = SessionManager::open(file, None, None).expect("child open");
            let ids = Conversation::of(&manager);
            let before = manager.entries().len();
            arm(limit);
            let result = manager.append_group(omission(&ids));
            returned(on);
            let Err(SessionError::GroupWrite(error)) = result else {
                panic!("not a group write error: {result:?}");
            };
            let failed = if torn == 0 {
                FailedLine::NotWritten
            } else {
                FailedLine::Torn
            };
            assert_eq!(
                (error.written.len(), error.len, error.failed),
                (line, 3, failed)
            );
            assert_eq!(
                error.source.raw_os_error(),
                Some(libc::EFBIG),
                "{}",
                error.source
            );
            // The manager keeps the synced lines alone.
            let entries = manager.entries();
            let kept: Vec<&str> = entries[before..].iter().map(|entry| entry.id()).collect();
            assert_eq!(kept, error.written);
            assert_eq!(manager.leaf_id(), entries.last().map(|entry| entry.id()));
        }
        "migrate" => {
            arm(limit);
            let result = SessionManager::open(file, None, None);
            returned(on);
            assert_efbig(&result.expect_err("a migration over the limit"), file);
        }
        other => panic!("bad write {other}"),
    }
    if on == "count" {
        assert_eq!(CAUGHT.load(Ordering::SeqCst), 1, "SIGXFSZ caught");
    }
    println!("{CHECKED}");
    let _ = std::io::stdout().flush();
    std::process::exit(0);
}

/// Give `SIGXFSZ` the disposition `on`, explicitly, since an ignored
/// signal stays ignored across `exec`, and unblock it on this thread, the
/// one that writes and that the kernel sends it to. A killed child leaves
/// no core dump: it is not dumpable and its core size limit is zero.
fn prepare(on: &str) {
    let handler = match on {
        "default" => libc::SIG_DFL,
        "ignore" => libc::SIG_IGN,
        "count" => catch as extern "C" fn(libc::c_int) as libc::sighandler_t,
        other => panic!("bad disposition {other}"),
    };
    let zero: libc::c_ulong = 0;
    // SAFETY: each structure is zeroed, then set up by `sigemptyset` and
    // the fields assigned here, before a call reads it, and outlives the
    // calls that borrow it; `catch` only increments an atomic, which is
    // async-signal-safe; `prctl` takes plain values.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = handler;
        assert_eq!(libc::sigemptyset(&mut action.sa_mask), 0);
        assert_eq!(
            libc::sigaction(libc::SIGXFSZ, &action, std::ptr::null_mut()),
            0
        );
        let mut signals: libc::sigset_t = std::mem::zeroed();
        assert_eq!(libc::sigemptyset(&mut signals), 0);
        assert_eq!(libc::sigaddset(&mut signals, libc::SIGXFSZ), 0);
        assert_eq!(
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &signals, std::ptr::null_mut()),
            0
        );
        assert_eq!(
            libc::prctl(libc::PR_SET_DUMPABLE, zero, zero, zero, zero),
            0
        );
        let core = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(libc::setrlimit(libc::RLIMIT_CORE, &core), 0);
    }
}

/// Let no file of the child grow past `limit` bytes, and say so.
fn arm(limit: u64) {
    let mut current = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `getrlimit` and `setrlimit` borrow `current` only for the
    // call.
    unsafe {
        assert_eq!(libc::getrlimit(libc::RLIMIT_FSIZE, &mut current), 0);
        assert!(limit <= current.rlim_max, "hard limit {}", current.rlim_max);
        current.rlim_cur = limit;
        assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &current), 0);
    }
    println!("{ARMED}");
    let _ = std::io::stdout().flush();
}

/// The write over the limit returned: under the signal's default action
/// it must not, so the child says so and exits.
fn returned(on: &str) {
    if on == "default" {
        println!("{RETURNED}");
        let _ = std::io::stdout().flush();
        std::process::exit(CHILD_RETURNED_EXIT);
    }
}

/// `error` is the kernel's `EFBIG` for `file`.
fn assert_efbig(error: &SessionError, file: &Path) {
    let SessionError::Io { path, source } = error else {
        panic!("not an I/O error: {error:?}");
    };
    assert_eq!(path.file_name(), file.file_name(), "{error}");
    assert_eq!(source.raw_os_error(), Some(libc::EFBIG), "{error}");
}

/// Run [`limited_writer_child`] with `spec` until it ends; returns its
/// status and output.
fn run(spec: &str) -> (ExitStatus, Vec<String>) {
    let mut child = TestChild::spawn(CHILD_TEST, CHILD_ENV, spec);
    child.wait_for(ARMED);
    child.wait()
}

/// The child died of `SIGXFSZ`'s default action inside its write: not of
/// a core dump, its watchdog, a returned write, or a panic.
fn assert_died_in_write((status, output): (ExitStatus, Vec<String>), case: &str) {
    assert_eq!(
        (status.signal(), status.core_dumped()),
        (Some(libc::SIGXFSZ), false),
        "{case}: {status}: {output:?}"
    );
    assert!(
        !output
            .iter()
            .any(|line| line.contains(RETURNED) || line.contains(CHECKED)),
        "{case}: {output:?}"
    );
}

/// The child's write returned, and the child checked its error and state.
fn assert_checked((status, output): (ExitStatus, Vec<String>), case: &str) {
    assert!(status.success(), "{case}: {status}: {output:?}");
    assert!(
        output.iter().any(|line| line.contains(CHECKED)),
        "{case}: {output:?}"
    );
}

/// How far into a line of `len` bytes the cases stop a write: before its
/// first byte, after one, half way, and before its newline.
fn tears(len: usize) -> [usize; 4] {
    [0, 1, len / 2, len - 1]
}

/// A session file a child writes to, and what that write adds, measured
/// by making the same write to a copy: entry ids and timestamps have a
/// fixed length, so the lines do too.
struct Session {
    _dir: TempDir,
    file: PathBuf,
    base: Vec<u8>,
    /// The entries the file starts with.
    entries: Vec<SessionEntry>,
    /// The length of each line the write adds.
    added: Vec<usize>,
    /// The copy's entries after the write.
    rehearsed: Vec<SessionEntry>,
}

impl Session {
    /// A tool turn with its first result: a single append writes the
    /// second.
    fn single() -> Self {
        let dir = TempDir::new("limited-single");
        let mut manager = file_session(&dir);
        tool_turn(&mut manager);
        manager
            .append_message(tool_result("c1"))
            .expect("first result");
        Self::rehearse(dir, manager, |manager| {
            manager
                .append_message(tool_result("c2"))
                .expect("rehearsed append");
        })
    }

    /// A written conversation, whose tool turn a group of three context
    /// edits omits.
    fn group() -> Self {
        let dir = TempDir::new("limited-group");
        let mut manager = file_session(&dir);
        conversation(&mut manager);
        Self::rehearse(dir, manager, |manager| {
            let ids = Conversation::of(manager);
            manager
                .append_group(omission(&ids))
                .expect("rehearsed group");
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
        let rehearsal = TempDir::new("limited-rehearsal");
        let copy = rehearsal.join("copy.jsonl");
        std::fs::write(&copy, &base).expect("copy");
        let mut manager = SessionManager::open(&copy, None, None).expect("open copy");
        write(&mut manager);
        drop(manager);
        let after = read(&copy);
        assert!(after.starts_with(&base));
        let added = lines(&after[base.len()..])
            .iter()
            .map(|line| line.len())
            .collect();
        Self {
            entries: entries_of(&file),
            rehearsed: entries_of(&copy),
            _dir: dir,
            file,
            base,
            added,
        }
    }

    /// The file size limit that stops the write `torn` bytes into line
    /// `line`.
    fn limit(&self, line: usize, torn: usize) -> u64 {
        (self.base.len() + self.added[..line].iter().sum::<usize>() + torn) as u64
    }

    fn spec(&self, what: &str, on: &str, line: usize, torn: usize) -> String {
        let limit = self.limit(line, torn);
        format!("{what}|{on}|{limit}|{line}|{torn}|{}", self.file.display())
    }

    /// What a write stopped `torn` bytes into line `line` leaves: the base,
    /// the whole lines before that line, and `torn` bytes of it without a
    /// newline. The file loads, reading the torn line as an entry only when
    /// all of its JSON is there, into the context the same lines of the
    /// rehearsed write give, whose request a provider accepts; and the next
    /// writer takes it over and appends on a line of its own.
    fn check_stopped(&self, line: usize, torn: usize, case: &str) {
        let bytes = read(&self.file);
        assert_eq!(bytes.len() as u64, self.limit(line, torn), "{case}");
        assert!(bytes.starts_with(&self.base), "{case}");
        let tail = lines(&bytes[self.base.len()..]);
        let whole: Vec<usize> = tail.iter().take(line).map(|line| line.len()).collect();
        assert_eq!(whole, self.added[..line], "{case}");
        for written in &tail[..line] {
            assert!(written.ends_with(b"\n"), "{case}");
            serde_json::from_slice::<Value>(written).expect("a whole line");
        }
        let complete = torn + 1 == self.added[line];
        match &tail[line..] {
            [] => assert_eq!(torn, 0, "{case}"),
            [piece] => {
                assert_eq!(piece.len(), torn, "{case}");
                assert!(!piece.ends_with(b"\n"), "{case}");
                let parsed = serde_json::from_slice::<Value>(piece).is_ok();
                assert_eq!(parsed, complete, "{case}");
            }
            more => panic!("{case}: {} pieces after the whole lines", more.len()),
        }

        let kept = self.entries.len() + line + usize::from(complete);
        let loaded = entries_of(&self.file);
        assert_eq!(loaded.len(), kept, "{case}");
        assert_eq!(loaded[..self.entries.len()], self.entries[..], "{case}");
        let context = context_of(&self.file);
        let expected = build_session_context(&self.rehearsed[..kept], LeafSelector::Last);
        assert_eq!(context, expected, "{case}");
        assert_eq!(provider_problem(&request(&context)), None, "{case}");

        let mut taken = take_over(&self.file);
        assert_eq!(taken.entries().len(), kept, "{case}");
        taken.append_session_info("after").expect("append after");
        drop(taken);
        let after = read(&self.file);
        assert!(after.starts_with(&bytes), "{case}");
        let appended = lines(&after[bytes.len()..]);
        // A torn line gains the newline that ends it.
        let info = if torn > 0 {
            assert_eq!(appended[0], b"\n", "{case}");
            &appended[1..]
        } else {
            &appended[..]
        };
        let [info] = info else {
            panic!("{case}: {} lines appended", info.len());
        };
        let info: Value = serde_json::from_slice(info).expect("the next writer's line");
        assert_eq!(info["type"], "session_info", "{case}");
        assert_eq!(entries_of(&self.file).len(), kept + 1, "{case}");
        let context = context_of(&self.file);
        assert_eq!(provider_problem(&request(&context)), None, "{case}");
    }
}

/// The last message of the request a single-append session rebuilds: the
/// second tool result, or, when its line is lost, the error result that
/// stands in for it.
fn assert_second_result(session: &Session, complete: bool, case: &str) {
    let messages = request(&context_of(&session.file));
    let Some(Message::ToolResult(result)) = messages.last() else {
        panic!("{case}: {messages:?}");
    };
    assert_eq!(
        (result.tool_call_id.as_str(), result.is_error),
        ("c2", !complete),
        "{case}"
    );
}

/// A writer killed inside the `write` of an appended entry leaves the file
/// with every byte before the limit and none after: a torn last line that
/// loading skips, as Pi's loader does, unless all of its JSON was written.
#[test]
fn a_writer_killed_inside_an_append_write_leaves_a_loadable_file() {
    let len = Session::single().added[0];
    for torn in tears(len) {
        let case = format!("single, {torn} of {len} bytes");
        let session = Session::single();
        assert_died_in_write(run(&session.spec("single", "default", 0, torn)), &case);
        assert_second_result(&session, torn + 1 == len, &case);
        session.check_stopped(0, torn, &case);
    }
}

/// An append whose `write` fails with the kernel's `EFBIG`, the signal
/// ignored or caught once, returns that error, keeps the entry in memory
/// as Pi does, and leaves the file as a killed writer does.
#[test]
fn an_append_over_the_file_size_limit_returns_efbig() {
    let len = Session::single().added[0];
    for on in ["ignore", "count"] {
        for torn in tears(len) {
            let case = format!("single, {on}, {torn} of {len} bytes");
            let session = Session::single();
            assert_checked(run(&session.spec("single", on, 0, torn)), &case);
            session.check_stopped(0, torn, &case);
        }
    }
}

/// A writer killed inside the `write` of any line of a group leaves the
/// lines before it, synced, and a torn prefix of that line: what a group
/// written up to that byte holds.
#[test]
fn a_writer_killed_inside_a_group_line_write_leaves_a_loadable_prefix() {
    let added = Session::group().added;
    assert_eq!(added.len(), 3);
    for (line, len) in added.into_iter().enumerate() {
        for torn in tears(len) {
            let case = format!("group line {line}, {torn} of {len} bytes");
            let session = Session::group();
            assert_died_in_write(run(&session.spec("group", "default", line, torn)), &case);
            session.check_stopped(line, torn, &case);
        }
    }
}

/// A group whose `write` of a line fails with `EFBIG` reports the synced
/// lines as written and that line as not written or torn, and the manager
/// keeps only the synced lines.
#[test]
fn a_group_over_the_file_size_limit_reports_the_line_it_stopped_at() {
    let added = Session::group().added;
    for on in ["ignore", "count"] {
        for (line, len) in added.iter().copied().enumerate() {
            for torn in tears(len) {
                let case = format!("group, {on}, line {line}, {torn} of {len} bytes");
                let session = Session::group();
                assert_checked(run(&session.spec("group", on, line, torn)), &case);
                session.check_stopped(line, torn, &case);
            }
        }
    }
}

/// A version 2 session, its directory, and what migrating it writes.
struct Migration {
    _dir: TempDir,
    sessions: PathBuf,
    file: PathBuf,
    old: Vec<u8>,
    new: Vec<u8>,
    context: crate::session::SessionContext,
}

impl Migration {
    fn new() -> Self {
        let dir = TempDir::new("limited-migration");
        let sessions = dir.join("sessions");
        let (file, old) = v2_session(&sessions);
        let (new, context) = migrated(&old);
        Self {
            _dir: dir,
            sessions,
            file,
            old,
            new,
            context,
        }
    }

    fn spec(&self, on: &str, torn: usize) -> String {
        format!("migrate|{on}|{torn}|0|{torn}|{}", self.file.display())
    }

    fn name(&self) -> String {
        let name = self.file.file_name().expect("name");
        name.to_string_lossy().into_owned()
    }

    /// The next writer migrates the old file and appends to it; a
    /// temporary file left behind stays, and is never listed.
    fn check_taken_over(&self, left: &[(String, Vec<u8>)], case: &str) {
        let cwd = self.sessions.to_string_lossy().into_owned();
        assert_eq!(
            listed(&cwd, &self.sessions),
            std::slice::from_ref(&self.file),
            "{case}"
        );
        let mut taken = take_over(&self.file);
        assert_eq!(taken.build_session_context(), self.context, "{case}");
        assert_eq!(read(&self.file), self.new, "{case}");
        taken.append_session_info("after").expect("append");
        drop(taken);
        let after = read(&self.file);
        assert!(after.starts_with(&self.new), "{case}");
        assert_eq!(lines(&after[self.new.len()..]).len(), 1, "{case}");
        assert_eq!(temps(&self.sessions), left, "{case}");
        assert_eq!(
            listed(&cwd, &self.sessions),
            std::slice::from_ref(&self.file),
            "{case}"
        );
    }
}

/// A migration killed inside the `write` of its temporary file leaves the
/// session file as it was and the temporary file holding exactly the bytes
/// written, which no listing finds.
#[test]
fn a_migration_killed_inside_its_temporary_file_write_leaves_the_old_file() {
    for index in 0..4 {
        let migration = Migration::new();
        let len = migration.new.len();
        let torn = tears(len)[index];
        let case = format!("migration, {torn} of {len} bytes");
        assert_died_in_write(run(&migration.spec("default", torn)), &case);
        assert_eq!(read(&migration.file), migration.old, "{case}");
        let left = temps(&migration.sessions);
        let name = migration.name();
        let [(temp, bytes)] = &left[..] else {
            panic!("{case}: {left:?}");
        };
        assert!(is_temp_of(temp, &name), "{case}: {temp}");
        assert_eq!(bytes[..], migration.new[..torn], "{case}");
        let mut expected = vec![name.clone(), format!("{name}.lock"), temp.clone()];
        expected.sort();
        assert_eq!(names(&migration.sessions), expected, "{case}");
        migration.check_taken_over(&left, &case);
    }
}

/// A migration whose temporary file `write` fails with `EFBIG` fails the
/// open with that error, removes its temporary file, and leaves the
/// session file as it was.
#[test]
fn a_migration_over_the_file_size_limit_fails_and_removes_its_temporary_file() {
    for on in ["ignore", "count"] {
        for half in [false, true] {
            let migration = Migration::new();
            let len = migration.new.len();
            let torn = if half { len / 2 } else { 0 };
            let case = format!("migration, {on}, {torn} of {len} bytes");
            assert_checked(run(&migration.spec(on, torn)), &case);
            assert_eq!(read(&migration.file), migration.old, "{case}");
            let name = migration.name();
            assert_eq!(
                names(&migration.sessions),
                [name.clone(), format!("{name}.lock")],
                "{case}"
            );
            migration.check_taken_over(&[], &case);
        }
    }
}

/// Appends to a file on a full device fail with the kernel's `ENOSPC`
/// before a byte is written: a group keeps none of its entries, and a
/// single append keeps its entry in memory, as in Pi. This is a real
/// errno on the write path, not a filesystem filling up.
#[test]
fn appends_to_a_full_device_fail_with_enospc_and_write_nothing() {
    let dir = TempDir::new("limited-full-device");
    let mut manager = file_session(&dir);
    let ids = conversation(&mut manager);
    let file = manager.session_file().expect("file").to_path_buf();
    let base = read(&file);
    let before = manager.entries().len();
    let set_aside = dir.join("set-aside.jsonl");
    std::fs::rename(&file, &set_aside).expect("set the file aside");
    std::os::unix::fs::symlink("/dev/full", &file).expect("link /dev/full");

    let result = manager.append_group(omission(&ids));
    let Err(SessionError::GroupWrite(error)) = result else {
        panic!("not a group write error: {result:?}");
    };
    assert_eq!(
        (error.written.len(), error.len, error.failed),
        (0, 3, FailedLine::NotWritten)
    );
    assert_eq!(error.source.raw_os_error(), Some(libc::ENOSPC), "{error}");
    assert_eq!(manager.entries().len(), before);

    let error = manager
        .append_session_info("full")
        .expect_err("an append to a full device");
    let SessionError::Io { source, .. } = &error else {
        panic!("not an I/O error: {error:?}");
    };
    assert_eq!(source.raw_os_error(), Some(libc::ENOSPC), "{error}");
    assert_eq!(manager.entries().len(), before + 1);
    drop(manager);

    std::fs::remove_file(&file).expect("unlink");
    std::fs::rename(&set_aside, &file).expect("restore the file");
    assert_eq!(read(&file), base);
    let mut taken = take_over(&file);
    assert_eq!(taken.entries().len(), before);
    taken.append_session_info("after").expect("append after");
    drop(taken);
    let after = read(&file);
    assert!(after.starts_with(&base));
    assert_eq!(lines(&after[base.len()..]).len(), 1);
}
