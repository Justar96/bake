//! Lines `JSON.parse` reads but `serde_json` rejects: nesting past
//! [`MAX_HELD_DEPTH`], lone surrogate escapes, and numbers beyond the double
//! range. Pi reads each; dropping one would cut the parent chain and the
//! resumed context at it. No Pi test covers these; they follow Pi's
//! `parseSessionEntries` (`JSON.parse` per line).

use std::fs;

use bake_coding_agent::session::json_line::MAX_HELD_DEPTH;
use bake_coding_agent::session::{NewSessionOptions, SessionManager};
use serde_json::{Value, json};

use crate::support::{TempDir, roles, text_of, user_msg};

const HEADER: &str = r#"{"type":"session","version":3,"id":"s","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp"}"#;

fn nested_objects(depth: usize, leaf: &str) -> String {
    format!("{}{leaf}{}", "{\"a\":".repeat(depth), "}".repeat(depth))
}

fn user_line(id: &str, parent: Option<&str>, text: &str) -> String {
    let parent = parent.map_or("null".to_owned(), |parent| format!("\"{parent}\""));
    format!(
        r#"{{"type":"message","id":"{id}","parentId":{parent},"timestamp":"2025-01-01T00:00:01.000Z","message":{{"role":"user","content":"{text}","timestamp":1}}}}"#
    )
}

/// The lines of a file as Pi v1.1.0 writes them (`JSON.stringify`): a deep
/// custom entry, an out-of-range number (Pi wrote `null`, so a hand-edited
/// one), and a lone surrogate between two user messages.
fn pi_lines() -> Vec<String> {
    vec![
        HEADER.to_owned(),
        user_line("a", None, "first"),
        format!(
            r#"{{"type":"custom","customType":"ext","data":{},"id":"b","parentId":"a","timestamp":"2025-01-01T00:00:02.000Z"}}"#,
            nested_objects(300, r#""\ud800""#)
        ),
        r#"{"type":"custom","customType":"ext","data":{"n":1e400,"s":"x\udc00"},"id":"c","parentId":"b","timestamp":"2025-01-01T00:00:03.000Z"}"#.to_owned(),
        user_line("d", Some("c"), "x\\ud800y"),
    ]
}

fn write_lines(path: &std::path::Path, lines: &[String]) {
    fs::write(path, format!("{}\n", lines.join("\n"))).expect("write");
}

#[test]
fn lines_json_parse_reads_keep_the_parent_chain() {
    let dir = TempDir::new("held-chain");
    let file = dir.join("pi.jsonl");
    let lines = pi_lines();
    write_lines(&file, &lines);
    let before = fs::read(&file).expect("read");
    let session = SessionManager::open(&file, Some(dir.path()), None).expect("open");
    assert_eq!(session.entries().len(), 4);
    assert_eq!(session.leaf_id(), Some("d"));
    let messages = session.build_session_context().messages;
    assert_eq!(roles(&messages), ["user", "user"]);
    assert_eq!(text_of(&messages[1]), "x\u{fffd}y");
    // A current-version file is not rewritten.
    assert_eq!(fs::read(&file).expect("read"), before);
    // The entries write back as Pi would, except `1e400`, which Pi writes as
    // `null` too.
    let written: Vec<String> = session
        .file_entries()
        .iter()
        .map(|entry| entry.to_line())
        .collect();
    let mut expected = lines.clone();
    expected[3] = expected[3].replace("1e400", "null");
    assert_eq!(written, expected);
}

#[test]
fn a_deep_value_this_crate_appends_reads_back() {
    let dir = TempDir::new("held-own");
    let mut session =
        SessionManager::create(dir.str(), Some(dir.path()), NewSessionOptions::default())
            .expect("create");
    session.append_message(user_msg("one")).expect("append");
    let mut data = Value::from(1);
    for _ in 0..300 {
        data = json!({ "a": data });
    }
    session
        .append_custom_entry("ext", Some(data))
        .expect("custom");
    let last = session.append_message(user_msg("two")).expect("append");
    let file = session.session_file().expect("file").to_path_buf();
    assert_eq!(session.build_session_context().messages.len(), 2);

    let mut reopened = SessionManager::open(&file, Some(dir.path()), None).expect("open");
    assert_eq!(reopened.leaf_id(), Some(last.as_str()));
    assert_eq!(reopened.build_session_context().messages.len(), 2);
    let deep_line = fs::read_to_string(&file)
        .expect("read")
        .lines()
        .nth(2)
        .expect("custom line")
        .to_owned();
    assert!(deep_line.contains(&nested_objects(300, "1")));

    // A branched session copies the line as it was written.
    let branched = reopened
        .create_branched_session(&last)
        .expect("branch")
        .expect("file");
    let copied = fs::read_to_string(branched).expect("read");
    assert!(copied.lines().any(|line| line == deep_line));
}

#[test]
fn migration_and_fork_keep_held_text() {
    let dir = TempDir::new("held-migrate");
    let file = dir.join("v2.jsonl");
    let mut lines = pi_lines();
    lines[0] = lines[0].replace(r#""version":3"#, r#""version":2"#);
    lines.push(
        r#"{"type":"message","id":"e","parentId":"d","timestamp":"2025-01-01T00:00:04.000Z","message":{"role":"hookMessage","customType":"x","content":"\ud800","display":true,"timestamp":1}}"#.to_owned(),
    );
    write_lines(&file, &lines);
    let session = SessionManager::open(&file, Some(dir.path()), None).expect("open");
    assert_eq!(session.entries().len(), 5);

    // Node: each line through `JSON.stringify(JSON.parse(line))` after Pi's
    // version 2 to 3 migration.
    let mut expected = lines.clone();
    expected[0] = HEADER.to_owned();
    expected[3] = expected[3].replace("1e400", "null");
    expected[5] = expected[5].replace("hookMessage", "custom");
    let migrated = fs::read_to_string(&file).expect("read");
    assert_eq!(migrated, format!("{}\n", expected.join("\n")));

    let fork_dir = dir.join("fork");
    let fork = SessionManager::fork_from(
        &file,
        dir.str(),
        Some(&fork_dir),
        NewSessionOptions::default(),
    )
    .expect("fork");
    let forked = fs::read_to_string(fork.session_file().expect("file")).expect("read");
    let forked: Vec<&str> = forked.lines().skip(1).collect();
    assert_eq!(
        forked,
        expected[1..].iter().map(String::as_str).collect::<Vec<_>>()
    );
}

/// The deepest value held in memory survives the recursive `serde_json`
/// operations a reader applies, on a 1 MiB thread stack: Windows' main
/// thread. In a debug build they need about 350 KiB at this depth; at 256
/// levels they needed about 700 KiB, too little margin.
#[test]
fn the_held_depth_is_safe_on_a_small_stack() {
    let dir = TempDir::new("held-stack");
    let file = dir.join("deep.jsonl");
    // Root, message, content, and block are four levels.
    let arguments = nested_objects(MAX_HELD_DEPTH - 4, "1");
    let line = format!(
        r#"{{"type":"message","id":"a","parentId":null,"timestamp":"2025-01-01T00:00:01.000Z","message":{{"role":"assistant","content":[{{"type":"toolCall","id":"t","name":"n","arguments":{arguments}}}],"api":"x","provider":"p","model":"m","usage":{{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}}},"stopReason":"toolUse","timestamp":1}}}}"#
    );
    write_lines(&file, &[HEADER.to_owned(), line.clone()]);
    let worker = std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(move || {
            let session = SessionManager::open(&file, None, None).expect("open");
            let messages = session.build_session_context().messages;
            let live = messages[0].to_agent();
            assert!(live.as_llm().is_some());
            let copy = messages.clone();
            assert_eq!(copy, messages);
            // Held in full: the innermost member is still the number.
            let mut value = &messages[0].as_json()["content"][0]["arguments"];
            for _ in 0..MAX_HELD_DEPTH - 5 {
                value = &value["a"];
            }
            assert_eq!(value["a"], 1);
            let written: Vec<String> = session
                .file_entries()
                .iter()
                .map(|entry| entry.to_line())
                .collect();
            written
        })
        .expect("spawn");
    let written = worker.join().expect("no stack overflow");
    assert_eq!(written[1], line);
}
