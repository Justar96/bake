//! Golden files across Pi and Bake (`tests/fixtures/pi-session/`).
//!
//! `generate.mjs` had Pi v1.1.0 write `pi-v3.jsonl` and recorded, for it and
//! for hand-written version 1, version 2, and torn inputs, what Pi reads
//! (`<name>.pi.json`) and the bytes Pi leaves after opening
//! (`<name>.pi-after-open.jsonl`). These tests open the same bytes in Rust
//! and require the same reading and the same bytes.
//!
//! `generate-held.mjs` recorded the same for `pi-held.jsonl`, whose lines
//! need [`bake_coding_agent::session::json_line`].
//!
//! The other direction: `examples/write_golden_session.rs` wrote
//! `rust-written.jsonl` with this crate, and `read-with-pi.mjs` recorded
//! Pi's reading of it in `rust-written.pi.json`, which the Rust reading must
//! equal. That Pi run is recorded, not repeated here.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use bake_coding_agent::session::json::{JsonObject, js_stringify};
use bake_coding_agent::session::time::format_iso;
use bake_coding_agent::session::{NewSessionOptions, SessionManager};
use serde_json::Value;

use crate::support::TempDir;

const LIST_CWD: &str = "/golden/project";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/pi-session")
        .join(name)
}

fn read_fixture(name: &str) -> String {
    fs::read_to_string(fixture(name)).expect("fixture")
}

fn ids(values: Vec<&str>) -> Value {
    Value::Array(values.into_iter().map(Value::from).collect())
}

fn opt(value: Option<&str>) -> Value {
    value.map_or(Value::Null, Value::from)
}

/// The Rust side of `summary.mjs`.
fn summarize(manager: &SessionManager, list_cwd: &str) -> String {
    let tree = manager.tree();
    let sessions =
        SessionManager::list(list_cwd, Some(manager.session_dir()), None, None).expect("list");
    let info = sessions
        .iter()
        .find(|session| session.id == manager.session_id())
        .map(|info| {
            let mut object = JsonObject::new();
            object.insert("id".into(), info.id.clone().into());
            object.insert("cwd".into(), info.cwd.clone().into());
            if let Some(name) = &info.name {
                object.insert("name".into(), name.clone().into());
            }
            if let Some(parent) = &info.parent_session_path {
                object.insert("parentSessionPath".into(), parent.clone().into());
            }
            object.insert(
                "created".into(),
                info.created_ms
                    .map_or(Value::Null, |ms| format_iso(ms).into()),
            );
            object.insert("modified".into(), format_iso(info.modified_ms).into());
            object.insert("messageCount".into(), info.message_count.into());
            object.insert("firstMessage".into(), info.first_message.clone().into());
            object.insert(
                "allMessagesText".into(),
                info.all_messages_text.clone().into(),
            );
            Value::Object(object)
        })
        .unwrap_or(Value::Null);
    let entries = manager.entries();
    let tree_rows: Vec<Value> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let node = tree.node(index).expect("node");
            Value::Array(vec![
                entry.id().into(),
                ids(tree.children(node).map(|child| child.entry.id()).collect()),
                opt(manager.label(entry.id())),
                opt(node.label_timestamp.as_deref()),
            ])
        })
        .collect();
    let mut summary = JsonObject::new();
    summary.insert(
        "header".into(),
        Value::Object(manager.header().expect("header").as_json().clone()),
    );
    summary.insert("sessionId".into(), manager.session_id().into());
    summary.insert("leafId".into(), opt(manager.leaf_id()));
    summary.insert("sessionName".into(), opt(manager.session_name().as_deref()));
    summary.insert("context".into(), manager.build_session_context().to_json());
    summary.insert(
        "contextEntryIds".into(),
        Value::Array(
            manager
                .build_context_entries()
                .iter()
                .map(|entry| entry.id().into())
                .collect(),
        ),
    );
    summary.insert(
        "branchIds".into(),
        ids(manager
            .branch_entries(None)
            .iter()
            .map(|entry| entry.id())
            .collect()),
    );
    summary.insert(
        "roots".into(),
        ids(tree.root_nodes().map(|node| node.entry.id()).collect()),
    );
    summary.insert("tree".into(), Value::Array(tree_rows));
    summary.insert("info".into(), info);
    js_stringify(&Value::Object(summary))
}

/// Open a copy of a fixture as `generate.mjs` did: in its own directory.
fn open_copy(name: &str, dir: &TempDir) -> (SessionManager, PathBuf) {
    let copy = dir.join(&format!("{name}.jsonl"));
    fs::copy(fixture(&format!("{name}.jsonl")), &copy).expect("copy");
    let manager = SessionManager::open(&copy, Some(dir.path()), None).expect("open");
    (manager, copy)
}

fn check_pi_fixture(name: &str) {
    let dir = TempDir::new(&format!("golden-{name}"));
    let (manager, copy) = open_copy(name, &dir);
    assert_eq!(
        summarize(&manager, LIST_CWD),
        read_fixture(&format!("{name}.pi.json")).trim_end(),
        "{name}: Rust reads what Pi read"
    );
    assert_eq!(
        fs::read(&copy).expect("read"),
        fs::read(fixture(&format!("{name}.pi-after-open.jsonl"))).expect("fixture"),
        "{name}: Rust leaves the bytes Pi left"
    );
}

/// The full session Pi v1.1.0 wrote, including an entry kind it does not
/// know, reads to the same context, tree, labels, name, and listing.
#[test]
fn pi_written_session_reads_the_same() {
    check_pi_fixture("pi-v3");
}

/// A version 2 file migrates to the same bytes Pi rewrites.
#[test]
fn pi_v2_migrates_the_same() {
    check_pi_fixture("pi-v2");
}

/// A torn final line is skipped and set apart by a newline, as Pi does.
#[test]
fn torn_tail_repairs_the_same() {
    check_pi_fixture("pi-torn");
}

/// Lines `JSON.parse` reads and `serde_json` does not (`generate-held.mjs`):
/// a 300-level custom entry, lone surrogate escapes, and `1e400`. They keep
/// the parent chain and migrate to the bytes Pi rewrites. In memory a lone
/// surrogate is U+FFFD, the one difference in what a reader sees.
#[test]
fn pi_held_values_read_the_same() {
    let dir = TempDir::new("golden-pi-held");
    let (manager, copy) = open_copy("pi-held", &dir);
    let pi_reading = read_fixture("pi-held.pi.json")
        .trim_end()
        .replace("\\ud800", "\u{fffd}")
        .replace("\\udc00", "\u{fffd}");
    assert_eq!(summarize(&manager, LIST_CWD), pi_reading);
    assert_eq!(
        fs::read(&copy).expect("read"),
        fs::read(fixture("pi-held.pi-after-open.jsonl")).expect("fixture"),
        "Rust leaves the bytes Pi left"
    );
}

fn line_ids(bytes: &str) -> Vec<String> {
    bytes
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|value| value.get("type").and_then(Value::as_str) != Some("session"))
        .filter_map(|value| value.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect()
}

/// Version 1 migration mints random ids, so the comparison maps Pi's ids to
/// Bake's by position; everything else must match byte for byte.
#[test]
fn pi_v1_migrates_the_same() {
    let dir = TempDir::new("golden-pi-v1");
    let (manager, copy) = open_copy("pi-v1", &dir);
    let rust_bytes = fs::read_to_string(&copy).expect("read");
    let pi_bytes = read_fixture("pi-v1.pi-after-open.jsonl");
    let pi_ids = line_ids(&pi_bytes);
    let rust_ids = line_ids(&rust_bytes);
    assert_eq!(pi_ids.len(), rust_ids.len());
    let mapping: HashMap<&str, &str> = pi_ids
        .iter()
        .map(String::as_str)
        .zip(rust_ids.iter().map(String::as_str))
        .collect();
    let remap = |text: &str| {
        let mut out = text.to_owned();
        for (pi, rust) in &mapping {
            out = out.replace(&format!("\"{pi}\""), &format!("\"{rust}\""));
        }
        out
    };
    assert_eq!(remap(&pi_bytes), rust_bytes);
    assert_eq!(
        summarize(&manager, LIST_CWD),
        remap(read_fixture("pi-v1.pi.json").trim_end())
    );
}

/// Forking copies every entry line byte for byte under a new header.
#[test]
fn fork_copies_pi_lines_verbatim() {
    let dir = TempDir::new("golden-fork");
    let source = dir.join("source.jsonl");
    fs::copy(fixture("pi-v3.jsonl"), &source).expect("copy");
    let target = dir.join("forks");
    let forked = SessionManager::fork_from(
        &source,
        "/golden/other",
        Some(&target),
        NewSessionOptions::default(),
    )
    .expect("fork");
    let forked_bytes = fs::read_to_string(forked.session_file().expect("file")).expect("read");
    let source_bytes = read_fixture("pi-v3.jsonl");
    let body = |text: &str| {
        text.split_once('\n')
            .map(|(_, body)| body.to_owned())
            .unwrap_or_default()
    };
    assert_eq!(body(&forked_bytes), body(&source_bytes));
    let header: Value =
        serde_json::from_str(forked_bytes.lines().next().unwrap_or("")).expect("header");
    assert_eq!(
        header["parentSession"],
        Value::from(source.to_string_lossy().into_owned())
    );
    assert_eq!(
        forked.build_session_context().to_json(),
        SessionManager::open(&source, Some(dir.path()), None)
            .expect("open")
            .build_session_context()
            .to_json()
    );
}

/// Every line Bake rewrites from Pi's file is the line Pi wrote: parsing
/// and `JSON.stringify` round-trip.
#[test]
fn rewritten_lines_match_pi_bytes() {
    let dir = TempDir::new("golden-lines");
    let (manager, _) = open_copy("pi-v3", &dir);
    let lines: Vec<String> = manager
        .file_entries()
        .iter()
        .map(|entry| entry.to_line())
        .collect();
    let expected: Vec<String> = read_fixture("pi-v3.jsonl")
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(lines, expected);
}

/// A session Bake wrote reads in Pi v1.1.0 to what Bake reads
/// (`rust-written.pi.json`, recorded by `read-with-pi.mjs`).
#[test]
fn rust_written_session_reads_the_same_in_pi() {
    let dir = TempDir::new("golden-rust");
    let (manager, copy) = open_copy("rust-written", &dir);
    assert_eq!(
        summarize(&manager, LIST_CWD),
        read_fixture("rust-written.pi.json").trim_end()
    );
    assert_eq!(
        fs::read(&copy).expect("read"),
        fs::read(fixture("rust-written.jsonl")).expect("fixture")
    );
}
