//! A writer killed at each step of a rewrite through a temporary file
//! (`durable::replace`): the migration of an older session file, and a
//! branched session written to a new file. After each kill the file holds
//! all of its old bytes or all of its new ones, never a mix, and loads; a
//! temporary file left behind is never listed as a session; and the next
//! writer rewrites the file, or writes a branch, again. Each test owns its
//! temporary directory and the child processes it starts.

use std::io::Write;
use std::path::Path;

use serde_json::{Value, json};

use super::*;
use crate::session::durable::RewriteStep;
use crate::session::manager::crash_support::{
    CHILD_RETURNED_EXIT, CHILD_WATCHDOG_EXIT, Conversation, PATIENCE, TestChild, conversation,
    file_session, is_temp_of, lines, listed, message, migrated, names, read, take_over, temps,
    usage, v2_session,
};
use crate::session::{SessionEntry, load_entries_from_file};
use crate::test_support::TempDir;

const CHILD_ENV: &str = "BAKE_TEST_REWRITE_HOLDER";
const CHILD_TEST: &str = "session::manager::rewrite_tests::rewrite_holder_child";
const HOLDING: &str = "bake-test-holding-rewrite";

/// Not a test of its own: the child of the tests below, which re-run this
/// test binary with [`CHILD_ENV`] naming what to rewrite (`migrate` or
/// `branch`), the index of the step in [`RewriteStep::ALL`] to stop at,
/// and the session file.
#[test]
fn rewrite_holder_child() {
    let Some(spec) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    let spec = spec.to_string_lossy().into_owned();
    let mut parts = spec.splitn(3, '|');
    let (Some(kind), Some(step), Some(file)) = (parts.next(), parts.next(), parts.next()) else {
        panic!("bad spec {spec}");
    };
    let step = RewriteStep::ALL[step.parse::<usize>().expect("step")];
    let file = Path::new(file);
    let hold: Box<dyn FnOnce() + Send> = Box::new(|| {
        println!("{HOLDING}");
        let _ = std::io::stdout().flush();
        // The parent kills the child here; a parent that never does leaves
        // no child behind for long.
        std::thread::sleep(PATIENCE * 2);
        std::process::exit(CHILD_WATCHDOG_EXIT);
    });
    match kind {
        "migrate" => {
            let dir = file.parent().expect("session directory");
            let mut manager = SessionManager::create(
                &dir.to_string_lossy(),
                Some(dir),
                NewSessionOptions::default(),
            )
            .expect("child session");
            manager.hold_rewrite(step, hold);
            let _ = manager.set_session_file(file);
        }
        "branch" => {
            let mut manager = SessionManager::open(file, None, None).expect("child open");
            let leaf = Conversation::of(&manager).results[1].clone();
            manager.hold_rewrite(step, hold);
            let _ = manager.create_branched_session(&leaf);
        }
        other => panic!("bad kind {other}"),
    }
    std::process::exit(CHILD_RETURNED_EXIT);
}

/// Run [`rewrite_holder_child`] until it holds at `step`, then kill it.
fn kill_at(kind: &str, step: RewriteStep, file: &Path) {
    let index = RewriteStep::ALL
        .iter()
        .position(|candidate| *candidate == step)
        .expect("a step");
    let spec = format!("{kind}|{index}|{}", file.display());
    let mut child = TestChild::spawn(CHILD_TEST, CHILD_ENV, &spec);
    child.wait_for(HOLDING);
    child.kill();
}

/// Whether a kill at `step` comes after the rename that publishes the
/// new contents.
fn renamed(step: RewriteStep) -> bool {
    matches!(step, RewriteStep::Renamed | RewriteStep::DirSynced)
}

/// What the temporary file holds when a rewrite to `new` stops at `step`,
/// before the rename.
fn staged(step: RewriteStep, new: &[u8]) -> &[u8] {
    match step {
        RewriteStep::Created => &new[..0],
        RewriteStep::PartlyWritten => &new[..new.len() / 2],
        _ => new,
    }
}

/// A migration killed at each step leaves the old file, with any partial
/// contents only in a temporary file, or the whole new file. Either loads;
/// a listing finds only the session; and the next writer migrates the old
/// file again, or reads the new one, and appends to it.
#[test]
fn a_migration_killed_at_each_rewrite_step_leaves_the_old_or_the_new_file() {
    for step in RewriteStep::ALL {
        let dir = TempDir::new(&format!("rewrite-migrate-{step:?}"));
        let sessions = dir.join("sessions");
        let cwd = sessions.to_string_lossy().into_owned();
        let (file, old) = v2_session(&sessions);
        let name = file
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        let (new, context) = migrated(&old);

        kill_at("migrate", step, &file);

        let left = temps(&sessions);
        if renamed(step) {
            assert_eq!(read(&file), new, "{step:?}");
            assert!(left.is_empty(), "{step:?}: {left:?}");
        } else {
            assert_eq!(read(&file), old, "{step:?}");
            assert_eq!(left.len(), 1, "{step:?}");
            assert!(is_temp_of(&left[0].0, &name), "{step:?}: {}", left[0].0);
            assert_eq!(left[0].1, staged(step, &new), "{step:?}");
        }
        let lock = format!("{name}.lock");
        let mut expected_names = vec![name.clone(), lock.clone()];
        expected_names.extend(left.iter().map(|(temp, _)| temp.clone()));
        expected_names.sort();
        assert_eq!(names(&sessions), expected_names, "{step:?}");
        // Pi's loader reads either version's lines.
        let entries = load_entries_from_file(&file).expect("load");
        assert_eq!(entries.len(), 4, "{step:?}");
        assert_eq!(
            listed(&cwd, &sessions),
            std::slice::from_ref(&file),
            "{step:?}"
        );

        let mut taken = take_over(&file);
        assert_eq!(taken.build_session_context(), context, "{step:?}");
        assert_eq!(read(&file), new, "{step:?}");
        taken.append_session_info("after").expect("append");
        drop(taken);
        let after = read(&file);
        assert!(after.starts_with(&new), "{step:?}");
        assert_eq!(lines(&after[new.len()..]).len(), 1, "{step:?}");
        // A temporary file left behind stays, and is still not a session.
        assert_eq!(temps(&sessions), left, "{step:?}");
        assert_eq!(
            listed(&cwd, &sessions),
            std::slice::from_ref(&file),
            "{step:?}"
        );
    }
}

/// The entries a branch of `source` at `leaf` holds, built in memory.
fn branch_entries(source: &Path, leaf: &str) -> Vec<SessionEntry> {
    let entries = load_entries_from_file(source).expect("load source");
    let mut memory = SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), entries)
        .expect("memory");
    assert_eq!(memory.create_branched_session(leaf).expect("branch"), None);
    memory.entries().into_iter().cloned().collect()
}

/// The whole file of a branch of `source` holding `entries` under the
/// header line `header`, after checking that header.
fn branch_file(header: &[u8], source: &Path, cwd: &str, entries: &[SessionEntry]) -> Vec<u8> {
    let parsed: Value = serde_json::from_slice(header).expect("header JSON");
    assert_eq!(parsed["type"], json!("session"));
    assert_eq!(parsed["version"], json!(3));
    assert_eq!(parsed["cwd"], json!(cwd));
    assert_eq!(parsed["parentSession"], json!(source.to_string_lossy()));
    let mut bytes = header.to_vec();
    if bytes.last() != Some(&b'\n') {
        bytes.push(b'\n');
    }
    for entry in entries {
        bytes.extend_from_slice(entry.to_line().as_bytes());
        bytes.push(b'\n');
    }
    bytes
}

/// A branch killed at each step of writing its new file leaves the source
/// untouched and the branch either absent, with any partial contents only
/// in a temporary file, or whole: its header naming the source and every
/// entry of the branch. A listing finds only whole sessions, and a branch
/// from a fresh open of the source, and a takeover of a published branch,
/// both work.
#[test]
fn a_branch_killed_at_each_rewrite_step_leaves_no_file_or_a_whole_one() {
    for step in RewriteStep::ALL {
        let dir = TempDir::new(&format!("rewrite-branch-{step:?}"));
        let sessions = dir.join("sessions");
        let mut manager = file_session(&dir);
        let cwd = manager.cwd().to_owned();
        let ids = conversation(&mut manager);
        manager
            .append_message(message(
                json!({"role": "user", "content": "next", "timestamp": 4}),
            ))
            .expect("user");
        manager
            .append_message(message(json!({"role": "assistant",
                "content": [{"type": "text", "text": "done"}], "api": "faux",
                "provider": "faux", "model": "faux-1", "usage": usage(),
                "stopReason": "stop", "timestamp": 5})))
            .expect("assistant");
        let source = manager.session_file().expect("file").to_path_buf();
        let source_name = source
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        drop(manager);
        let old = read(&source);
        let entries = branch_entries(&source, &ids.results[1]);
        assert_eq!(entries.len(), 4);

        kill_at("branch", step, &source);

        assert_eq!(read(&source), old, "{step:?}");
        // The child took the branch's own writer lock before writing it.
        let all = names(&sessions);
        let branch_lock = all
            .iter()
            .find(|name| name.ends_with(".jsonl.lock") && **name != format!("{source_name}.lock"))
            .expect("the branch's lock file")
            .clone();
        let branch_name = branch_lock.trim_end_matches(".lock").to_owned();
        let branch = sessions.join(&branch_name);
        let left = temps(&sessions);
        let mut expected_names = vec![
            source_name.clone(),
            format!("{source_name}.lock"),
            branch_lock.clone(),
        ];
        if renamed(step) {
            assert!(left.is_empty(), "{step:?}: {left:?}");
            let bytes = read(&branch);
            let header = lines(&bytes)[0];
            assert_eq!(
                bytes,
                branch_file(header, &source, &cwd, &entries),
                "{step:?}"
            );
            expected_names.push(branch_name.clone());
        } else {
            assert_eq!(left.len(), 1, "{step:?}");
            let (temp, bytes) = &left[0];
            assert!(is_temp_of(temp, &branch_name), "{step:?}: {temp}");
            if step == RewriteStep::Created {
                assert!(bytes.is_empty(), "{step:?}");
            } else {
                // The header line is whole even in the first half.
                let header = lines(bytes)[0];
                assert!(header.ends_with(b"\n"), "{step:?}");
                let whole = branch_file(header, &source, &cwd, &entries);
                assert_eq!(bytes[..], *staged(step, &whole), "{step:?}");
            }
            expected_names.push(temp.clone());
        }
        expected_names.sort();
        assert_eq!(all, expected_names, "{step:?}");
        let mut sessions_listed = vec![source.clone()];
        if renamed(step) {
            sessions_listed.push(branch.clone());
            sessions_listed.sort();
            let mut taken = take_over(&branch);
            assert_eq!(taken.entries().len(), entries.len(), "{step:?}");
            taken
                .append_session_info("after")
                .expect("append to branch");
        }
        assert_eq!(listed(&cwd, &sessions), sessions_listed, "{step:?}");

        let mut again = take_over(&source);
        let later = again
            .create_branched_session(&ids.results[1])
            .expect("branch again")
            .expect("a branch file");
        drop(again);
        let bytes = read(&later);
        let header = lines(&bytes)[0];
        assert_eq!(
            bytes,
            branch_file(header, &source, &cwd, &entries),
            "{step:?}"
        );
        sessions_listed.push(later);
        sessions_listed.sort();
        assert_eq!(listed(&cwd, &sessions), sessions_listed, "{step:?}");
        assert_eq!(temps(&sessions), left, "{step:?}");
    }
}
