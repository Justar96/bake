//! Lines with many values held differently from `JSON.parse`
//! ([`bake_coding_agent::session::json_line`]) open, list, migrate, and fork
//! in time linear in their length. Each test runs its work on a worker
//! thread and fails once [`BOUND`] passes, so a quadratic reader or writer
//! fails here instead of stalling the suite. No Pi test covers this; the
//! lines follow Pi's `parseSessionEntries` (`JSON.parse` per line) and
//! `JSON.stringify` rewrites.

use std::fs;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use bake_coding_agent::session::{NewSessionOptions, SessionManager};

use crate::support::TempDir;

/// Linear work here takes well under a second in a debug build; a reader or
/// writer quadratic in the number of held values takes minutes.
const BOUND: Duration = Duration::from_secs(10);

/// Lone surrogate strings in one array.
const STRINGS: usize = 100_000;
/// Members holding a lone surrogate, then as many repeats of one key.
const MEMBERS: usize = 40_000;

const HEADER_V2: &str = r#"{"type":"session","version":2,"id":"s","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp"}"#;
const HEADER_V3: &str = r#"{"type":"session","version":3,"id":"s","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp"}"#;

fn user_line(id: &str, parent: Option<&str>) -> String {
    let parent = parent.map_or("null".to_owned(), |parent| format!("\"{parent}\""));
    format!(
        r#"{{"type":"message","id":"{id}","parentId":{parent},"timestamp":"2025-01-01T00:00:01.000Z","message":{{"role":"user","content":"hi","timestamp":1}}}}"#
    )
}

fn custom_line(id: &str, parent: &str, data: &str) -> String {
    format!(
        r#"{{"type":"custom","customType":"ext","data":{data},"id":"{id}","parentId":"{parent}","timestamp":"2025-01-01T00:00:02.000Z"}}"#
    )
}

/// Run `work` on a worker thread; fail when it takes longer than [`BOUND`].
fn within_bound<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let (sender, receiver) = mpsc::channel();
    let started = Instant::now();
    std::thread::spawn(move || {
        let _ = sender.send(work());
    });
    match receiver.recv_timeout(BOUND) {
        Ok(value) => value,
        Err(error) => panic!(
            "not done after {:?} ({error}); held values must cost linear time",
            started.elapsed()
        ),
    }
}

#[test]
fn many_held_values_open_list_migrate_and_fork_in_linear_time() {
    let dir = TempDir::new("held-scaling");
    let file = dir.join("v2.jsonl");
    let strings = format!("[{}]", vec![r#""\ud800""#; STRINGS].join(","));
    let members: Vec<String> = (0..MEMBERS)
        .map(|index| format!(r#""k{index}":"\udc00""#))
        .collect();
    let repeated = format!(
        "{{{},{}}}",
        members.join(","),
        vec![r#""r":1"#; MEMBERS].join(",")
    );
    let lines = [
        HEADER_V2.to_owned(),
        user_line("a", None),
        custom_line("b", "a", &strings),
        custom_line("c", "b", &repeated),
        user_line("d", Some("c")),
    ];
    fs::write(&file, format!("{}\n", lines.join("\n"))).expect("write");

    // Node: `JSON.stringify(JSON.parse(line))` for each line after Pi's
    // version 2 to 3 migration; the repeated key keeps its first position.
    let mut expected = lines.clone();
    expected[0] = HEADER_V3.to_owned();
    expected[3] = custom_line("c", "b", &format!("{{{},\"r\":1}}", members.join(",")));
    let expected = format!("{}\n", expected.join("\n"));

    let root = dir.path().to_path_buf();
    let (migrated, listed, forked) = within_bound(move || {
        let session = SessionManager::open(&file, Some(&root), None).expect("open");
        assert_eq!(session.entries().len(), 4);
        assert_eq!(session.leaf_id(), Some("d"));
        let migrated = fs::read_to_string(&file).expect("read");
        let root_str = root.to_str().expect("UTF-8 temp dir").to_owned();
        let listed = SessionManager::list_all(Some(&root), None, None)
            .expect("list")
            .len();
        let fork_dir = root.join("fork");
        let fork = SessionManager::fork_from(
            &file,
            &root_str,
            Some(&fork_dir),
            NewSessionOptions::default(),
        )
        .expect("fork");
        let forked = fs::read_to_string(fork.session_file().expect("file")).expect("read");
        (migrated, listed, forked)
    });
    assert_eq!(migrated, expected);
    assert_eq!(listed, 1);
    let forked_entries: Vec<&str> = forked.lines().skip(1).collect();
    let expected_entries: Vec<&str> = expected.lines().skip(1).collect();
    assert_eq!(forked_entries, expected_entries);
}
