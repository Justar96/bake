//! The deepened-case harness shared by `bake-session`'s `deepened_cases`
//! and `bake-cli`'s `deepened_cli_cases`: it collects every Session log of
//! the shared case tables and runtime captures, deepens one JSON value
//! position at a time, and runs the variants in sharded child processes of
//! the calling test binary on a [`STACK`] stack, naming a crashed variant
//! and entry point. Each caller supplies the entry points a variant runs.
//!
//! A position is a member value or array element, at any level, of the
//! header or of one row. Its variant replaces that value with an array chain
//! or an object chain [`DEPTH`] containers deep. Positions are deduplicated
//! by generation, row type, and JSON path across logs, and each kept
//! position takes the first log that holds it, logs whose original form an
//! entry point accepts first, so a deep value reaches past the checks.
//!
//! [`Mode::Full`] runs every position in both shapes. [`Mode::Sample`], the
//! default test's mode so the sweep fits CI's time, runs every
//! [`SAMPLE_STRIDE`]th position in the deterministic position order, the
//! shapes alternating across the sample.

// Each test crate that includes this module uses a different part of it.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use bake_session::{
    PathPlatform, dismantle, encode_event_line, json_text, migrate_released_generation, parse_json,
    released_zstd_plaintext, restore_plain_log,
};
use serde_json::{Map, Value};

/// Containers in a deepened value. With [`STACK`], one frame per level of
/// any recursion overflows; `deepened_negative_controls` checks that.
pub const DEPTH: usize = 10_000;
/// The stack each variant runs on, as the deep payload tests use.
pub const STACK: usize = 256 * 1024;
/// Large enough that no deepened row meets a source budget.
pub const BUDGET: usize = 1 << 24;
/// Logs per deduplicated position; see the module comment.
const LOGS_PER_POSITION: usize = 1;
/// [`Mode::Sample`] runs position 0 and every `SAMPLE_STRIDE`th after it.
pub const SAMPLE_STRIDE: usize = 4;
const SHARD_ENV: &str = "BAKE_DEEPENED_SHARD";
/// The first variant index a resumed shard runs, after a crashed one.
const FROM_ENV: &str = "BAKE_DEEPENED_FROM";
/// [`Mode::name`] of the sweep a shard belongs to.
const MODE_ENV: &str = "BAKE_DEEPENED_MODE";
/// Runs only the variants below this index (the crash-report check).
const LIMIT_ENV: &str = "BAKE_DEEPENED_LIMIT";
/// Aborts the process at this variant index (the crash-report check).
const ABORT_ENV: &str = "BAKE_DEEPENED_ABORT_AT";
/// The entry point an injected abort announces.
pub const INJECTED_ABORT: &str = "injected abort";
const PLACEHOLDER: &str = "\u{1}bake-deepened-slot\u{1}";
const SYNTHETIC_HEADER: &str = r#"{"type":"session","version":3,"id":"deepened","createdAt":1,"isSeeded":false,"delegationDepth":0}"#;

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(relative)
}

fn read_json(relative: &str) -> Value {
    let text = std::fs::read_to_string(repo_path(relative))
        .unwrap_or_else(|error| panic!("read {relative}: {error}"));
    // Tables may hold lone surrogates, which only `parse_json` reads.
    bake_session::parse_json(&text).unwrap_or_else(|error| panic!("parse {relative}: {error:?}"))
}

fn text<'a>(value: &'a Value, context: &str) -> &'a str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("{context}: expected a string, got {value}"))
}

/// A table string as the text it stands for: tables are read with
/// `parse_json`, so a lone surrogate or U+FDD0 arrives spelled.
fn raw(value: &Value, context: &str) -> String {
    bake_session::js_string::to_rust(text(value, context)).into_owned()
}

fn index(value: &Value, context: &str) -> usize {
    usize::try_from(
        value
            .as_u64()
            .unwrap_or_else(|| panic!("{context}: expected a count")),
    )
    .expect("a count")
}

fn hex(text: &str) -> Vec<u8> {
    assert!(text.len().is_multiple_of(2), "odd hex length");
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex"))
        .collect()
}

/// How a collected log reaches the crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Form {
    /// Header record and rows as a file holds them, of the generation its
    /// header names (`None` when it names none of 0 to 3).
    Plain(Option<u64>),
    /// A writer's header metadata and events, as the encoder, the append
    /// models, and the relationship check take them.
    Events,
}

/// One Session log a case table holds.
#[derive(Clone)]
struct Log {
    table: String,
    case: String,
    form: Form,
    /// The header line, then one line per row, without LF.
    lines: Vec<String>,
    /// Bytes after the last LF.
    tail: String,
    /// The inherited event count a writer's header is created with.
    inherited: Option<u64>,
}

/// Split file text into lines and the unterminated tail.
fn split_text(text: &str) -> (Vec<String>, String) {
    let mut lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    let tail = lines.pop().unwrap_or_default();
    (lines, tail)
}

fn header_version(line: Option<&String>) -> Option<u64> {
    let header = parse_json(line?).ok()?;
    let version = header.get("version").and_then(Value::as_u64);
    dismantle(header);
    version.filter(|version| *version <= 3)
}

struct Collector {
    table: String,
    logs: Vec<Log>,
    /// Inputs a table references that another table already supplies.
    referenced: usize,
}

impl Collector {
    fn plain(&mut self, case: &str, lines: Vec<String>, tail: String) {
        self.plain_at(case, lines, tail, None);
    }

    fn plain_at(&mut self, case: &str, lines: Vec<String>, tail: String, version: Option<u64>) {
        if lines.is_empty() && tail.is_empty() {
            return;
        }
        let version = version.or_else(|| header_version(lines.first()));
        self.logs.push(Log {
            table: self.table.clone(),
            case: case.to_owned(),
            form: Form::Plain(version),
            lines,
            tail,
            inherited: None,
        });
    }

    fn plain_text(&mut self, case: &str, text: &str) {
        let (lines, tail) = split_text(text);
        self.plain(case, lines, tail);
    }

    fn events(&mut self, case: &str, header: &Value, events: &[Value], inherited: Option<u64>) {
        let mut lines = vec![value_line(header)];
        lines.extend(events.iter().map(value_line));
        self.logs.push(Log {
            table: self.table.clone(),
            case: case.to_owned(),
            form: Form::Events,
            lines,
            tail: String::new(),
            inherited,
        });
    }

    /// Events appended to an opened file: the file's header record, as a
    /// writer's header metadata, and the events.
    fn opened_events(&mut self, case: &str, file_text: &str, events: &[Value]) {
        if events.is_empty() {
            return;
        }
        let Some(Value::Object(mut header)) = file_text
            .split('\n')
            .next()
            .and_then(|line| bake_session::parse_json(line).ok())
        else {
            return;
        };
        header.shift_remove("type");
        self.events(case, &Value::Object(header), events, None);
    }
}

/// A table value that is either a row's text or the row itself.
fn value_line(value: &Value) -> String {
    match value {
        // A table holds a row's text as a JavaScript string.
        Value::String(line) => bake_session::js_string::to_rust(line).into_owned(),
        other => json_text(other),
    }
}

fn file_lines(relative: &str) -> Vec<String> {
    let source = std::fs::read_to_string(repo_path(relative))
        .unwrap_or_else(|error| panic!("read {relative}: {error}"));
    let (lines, tail) = split_text(&source);
    assert!(tail.is_empty(), "{relative} lacks a final LF");
    lines
}

/// A table's named logs: a capture path, `{path}`, or derived `{lines}`.
fn named_log(logs: &Value, name: &str) -> Vec<String> {
    match &logs[name] {
        Value::String(path) => file_lines(path),
        Value::Object(fields) if fields.contains_key("path") => {
            file_lines(text(&fields["path"], name))
        }
        Value::Object(fields) => fields["lines"]
            .as_array()
            .unwrap_or_else(|| panic!("{name}: lines"))
            .iter()
            .map(|line| raw(line, name))
            .collect(),
        other => panic!("{name}: unknown log {other}"),
    }
}

/// Resolve a JSON pointer without `~` escapes to its parent and last key.
fn pointer_parent<'a>(root: &'a mut Value, pointer: &str) -> (&'a mut Value, String) {
    let mut parts: Vec<&str> = pointer.split('/').skip(1).collect();
    let last = parts.pop().expect("a pointer").to_owned();
    let mut node = root;
    for part in parts {
        node = match node {
            Value::Array(items) => &mut items[part.parse::<usize>().expect("index")],
            Value::Object(fields) => fields.get_mut(part).expect("member"),
            _ => panic!("{pointer}: no container"),
        };
    }
    (node, last)
}

/// A row edited at `pointer`, re-serialized; `None` removes the member.
fn edit_row(line: &str, pointer: &str, value: Option<&Value>) -> String {
    let mut row: Value = bake_session::parse_json(line).expect("an edited row parses");
    let (parent, key) = pointer_parent(&mut row, pointer);
    match (parent, value) {
        (Value::Array(items), Some(value)) => {
            let at = key.parse::<usize>().expect("index");
            if at < items.len() {
                items[at] = value.clone();
            } else {
                items.push(value.clone());
            }
        }
        (Value::Array(items), None) => {
            items.remove(key.parse::<usize>().expect("index"));
        }
        (Value::Object(fields), Some(value)) => {
            fields.insert(key, value.clone());
        }
        (Value::Object(fields), None) => {
            fields.shift_remove(&key);
        }
        _ => panic!("{pointer}: no container"),
    }
    bake_session::json_text(&row)
}

/// Apply the row edits the case tables share to `lines` (header first).
fn apply_edits(lines: &mut Vec<String>, tail: &mut String, edits: &Value, case: &str) {
    let Some(edits) = edits.as_array() else {
        return;
    };
    for edit in edits {
        let fields = edit.as_object().expect("an edit");
        let mut keys: Vec<&str> = fields.keys().map(String::as_str).collect();
        keys.sort_unstable();
        let row = || index(&edit["row"], case) + 1;
        match keys.as_slice() {
            ["truncate"] => lines.truncate(index(&edit["truncate"], case) + 1),
            ["header"] => lines[0] = raw(&edit["header"], case),
            ["append"] => lines.push(value_line(&edit["append"])),
            ["tail"] => *tail = raw(&edit["tail"], case),
            ["row", "text"] => {
                let row = row();
                lines[row] = raw(&edit["text"], case);
            }
            ["find", "replace", "row"] => {
                let row = row();
                lines[row] =
                    lines[row].replacen(&raw(&edit["find"], case), &raw(&edit["replace"], case), 1);
            }
            ["pointer", "row", "value"] => {
                let row = row();
                lines[row] = edit_row(
                    &lines[row],
                    text(&edit["pointer"], case),
                    Some(&edit["value"]),
                );
            }
            ["pointer", "remove", "row"] => {
                let row = row();
                lines[row] = edit_row(&lines[row], text(&edit["pointer"], case), None);
            }
            other => panic!("{case}: unknown edit {other:?}"),
        }
    }
}

fn case_id(case: &Value) -> String {
    text(&case["id"], "case id").to_owned()
}

fn cases(table: &Value) -> &[Value] {
    table["cases"].as_array().expect("cases")
}

/// Bytes a table stores as hex: a log when they are UTF-8 text.
fn hex_log(collector: &mut Collector, case: &str, bytes_hex: &str, version: Option<u64>) {
    match String::from_utf8(hex(bytes_hex)) {
        Ok(text) => {
            let (lines, tail) = split_text(&text);
            collector.plain_at(case, lines, tail, version);
        }
        // A row's JSON positions are not well defined in invalid UTF-8; the
        // table's own harness reads these bytes.
        Err(_) => collector.referenced += 1,
    }
}

/// `logs` plus cases of `log`, `edits`, optional `rows` prefixes, and
/// optional `migrated` inputs: the restoration and projection tables.
fn capture_cases(table: &Value, collector: &mut Collector) {
    for case in cases(table) {
        let id = case_id(case);
        if let Some(migrated) = case.get("migrated") {
            let mut lines = vec![value_line(&migrated["header"])];
            lines.extend(
                migrated["rows"]
                    .as_array()
                    .expect("rows")
                    .iter()
                    .map(value_line),
            );
            collector.plain_at(&id, lines, String::new(), migrated["version"].as_u64());
            continue;
        }
        let mut lines = named_log(&table["logs"], text(&case["log"], &id));
        if let Some(rows) = case.get("rows") {
            lines.truncate(index(rows, &id) + 1);
        }
        let mut tail = String::new();
        apply_edits(&mut lines, &mut tail, &case["edits"], &id);
        collector.plain(&id, lines, tail);
    }
}

/// Cases of a released `header` text and `rows` texts, at the case's
/// `version` or the header's.
fn released_row_cases(table: &Value, collector: &mut Collector) {
    for case in cases(table) {
        let id = case_id(case);
        let mut lines = vec![value_line(&case["header"])];
        lines.extend(
            case["rows"]
                .as_array()
                .expect("rows")
                .iter()
                .map(value_line),
        );
        collector.plain_at(
            &id,
            lines,
            String::new(),
            case.get("version").and_then(Value::as_u64),
        );
    }
}

fn request_derivation_cases(table: &Value, collector: &mut Collector) {
    let fixture = text(&table["fixture"]["log"], "fixture");
    for case in cases(table) {
        let id = case_id(case);
        let (mut lines, mut tail) = match &case["log"] {
            Value::String(name) if name == "fixture" => (file_lines(fixture), String::new()),
            Value::Array(lines) => (
                lines.iter().map(|line| raw(line, &id)).collect(),
                String::new(),
            ),
            other => panic!("{id}: unknown log {other}"),
        };
        apply_edits(&mut lines, &mut tail, &case["edits"], &id);
        collector.plain(&id, lines, tail);
    }
}

/// Single rows after a current header, and the fixture's pointer mutants.
fn row_cases(table: &Value, collector: &mut Collector) {
    for case in cases(table) {
        let id = case_id(case);
        let lines = vec![SYNTHETIC_HEADER.to_owned(), raw(&case["row"], &id)];
        collector.plain(&id, lines, String::new());
    }
    if let Some(fixture) = table
        .get("fixture")
        .filter(|fixture| fixture.get("mutants").is_some())
    {
        let base = file_lines(text(&fixture["log"], "fixture log"));
        for mutant in fixture["mutants"].as_array().expect("mutants") {
            let id = case_id(mutant);
            let mut lines = base.clone();
            let row = index(&mutant["seq"], &id) + 1;
            lines[row] = edit_row(
                &lines[row],
                text(&mutant["pointer"], &id),
                Some(&mutant["value"]),
            );
            collector.plain(&id, lines, String::new());
        }
    }
}

fn relationship_cases(table: &Value, collector: &mut Collector) {
    for case in cases(table) {
        let id = case_id(case);
        let header = parse_json(&value_line(&case["header"])).expect("header");
        let events: Vec<Value> = case["events"]
            .as_array()
            .expect("events")
            .iter()
            .map(|event| parse_json(&value_line(event)).expect("event"))
            .collect();
        collector.events(&id, &header, &events, None);
    }
}

fn row_encode_cases(table: &Value, collector: &mut Collector) {
    for case in cases(table) {
        let id = case_id(case);
        let header = case
            .get("header")
            .filter(|header| header.is_object())
            .cloned();
        let mut events: Vec<Value> = case
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if let Some(event) = case.get("event").filter(|event| !event.is_null()) {
            events.push(event.clone());
        }
        let header = header.unwrap_or_else(|| {
            let mut fields =
                serde_json::from_str::<Map<String, Value>>(SYNTHETIC_HEADER).expect("header");
            fields.shift_remove("type");
            Value::Object(fields)
        });
        collector.events(&id, &header, &events, None);
    }
}

fn plain_append_cases(table: &Value, collector: &mut Collector) {
    for case in cases(table) {
        let id = case_id(case);
        let appended: Vec<Value> = case["ops"]
            .as_array()
            .expect("ops")
            .iter()
            .filter_map(|op| op.get("events").and_then(Value::as_array))
            .flatten()
            .cloned()
            .collect();
        if let Some(create) = case.get("create") {
            let inherited = create.get("inheritedEventCount").and_then(Value::as_u64);
            collector.events(&id, &create["header"], &appended, inherited);
        }
        if let Some(open) = case.get("open") {
            collector.plain_text(&id, &raw(open, &id));
            collector.opened_events(&id, &raw(open, &id), &appended);
        }
    }
}

/// `plain-log-file` and `fault` cases: seeded file text, a hard-linked
/// seed carrying none, and the logs their create and append steps write.
fn plain_log_file_cases(table: &Value, collector: &mut Collector) {
    for case in cases(table) {
        let id = case_id(case);
        for seed in case["seed"].as_array().expect("seed") {
            if let Some(file_text) = seed.get("text") {
                collector.plain_text(&id, &raw(file_text, &id));
            }
        }
        let steps = case["steps"].as_array().expect("steps");
        let appended: Vec<Value> = steps
            .iter()
            .filter_map(|step| step.get("events").and_then(Value::as_array))
            .flatten()
            .cloned()
            .collect();
        let mut created = false;
        for step in steps {
            if step["step"] == "create" {
                collector.events(&id, &step["header"], &appended, None);
                created = true;
            }
        }
        if !created {
            let seeded = case["seed"]
                .as_array()
                .expect("seed")
                .iter()
                .filter(|seed| seed["file"].as_str().is_some_and(is_log_seed))
                .find_map(|seed| seed.get("text"));
            if let Some(file_text) = seeded {
                collector.opened_events(&id, &raw(file_text, &id), &appended);
            }
        }
    }
}

/// Whether a seed is a log rather than the write lock or a temporary file
/// a fault case also seeds, so an opened case's header comes from its log.
fn is_log_seed(file: &str) -> bool {
    let name = file.rsplit('/').next().unwrap_or(file);
    name != "session.lock" && !name.ends_with(".tmp")
}

/// `list`, `stat`, and `lookup` directory layouts: inline text, Zstd frame
/// plaintext, and capture inputs with edits.
fn layout_cases(table: &Value, collector: &mut Collector) {
    for case in cases(table) {
        let id = case_id(case);
        for entry in case["layout"].as_array().expect("layout") {
            if let Some(file_text) = entry.get("text") {
                collector.plain_text(&id, &raw(file_text, &id));
            } else if let Some(frames) = entry.get("frames") {
                let plaintext: String = frames
                    .as_array()
                    .expect("frames")
                    .iter()
                    .map(|frame| raw(frame, &id))
                    .collect();
                collector.plain_text(&id, &plaintext);
            } else if let Some(log) = entry.get("log") {
                let input = &table["inputs"][text(&log["input"], &id)];
                let mut lines = file_lines(text(&input["path"], &id));
                if let Some(header) = log.get("header") {
                    lines[0] = raw(header, &id);
                }
                let mut tail = String::new();
                apply_edits(&mut lines, &mut tail, &log["edits"], &id);
                collector.plain(&id, lines, tail);
            } else if entry.get("zstdCase").is_some() {
                // zstd-cases.json is collected as its own table.
                collector.referenced += 1;
            }
        }
    }
}

fn log_scan_cases(table: &Value, collector: &mut Collector) {
    for case in cases(table) {
        let id = case_id(case);
        if let Some(fixture) = case.get("fixture") {
            collector.plain(&id, file_lines(text(fixture, &id)), String::new());
        } else if let Some(bytes_hex) = case.get("bytesHex") {
            hex_log(collector, &id, text(bytes_hex, &id), None);
        } else {
            let lines = case["lines"]
                .as_array()
                .expect("lines")
                .iter()
                .map(|line| raw(line, &id))
                .collect();
            let tail = case
                .get("tail")
                .map_or_else(String::new, |tail| raw(tail, &id));
            collector.plain(&id, lines, tail);
        }
    }
}

/// Header records: a header-only log each.
fn header_cases(table: &Value, collector: &mut Collector) {
    for case in cases(table) {
        let id = case_id(case);
        let version = case.get("sourceVersion").and_then(Value::as_u64);
        if let Some(record) = case.get("record") {
            let (lines, tail) = split_text(&raw(record, &id));
            collector.plain_at(&id, lines, tail, version);
        } else if let Some(bytes_hex) = case.get("bytesHex") {
            hex_log(collector, &id, text(bytes_hex, &id), version);
        } else if let Some(fixture) = case.get("fixtureFirstRecord") {
            let header = file_lines(text(fixture, &id)).swap_remove(0);
            collector.plain(&id, vec![header], String::new());
        }
    }
}

fn zstd_cases(table: &Value, collector: &mut Collector) {
    for case in cases(table) {
        let id = case_id(case);
        match released_zstd_plaintext(&hex(text(&case["hex"], &id)), BUDGET) {
            Ok(decoded) => match String::from_utf8(decoded.plaintext) {
                Ok(plaintext) => collector.plain_text(&id, &plaintext),
                Err(_) => collector.referenced += 1,
            },
            // A frame that does not decode holds no log to deepen.
            Err(_) => collector.referenced += 1,
        }
    }
}

/// How the harness reads each shared table; a table missing here fails
/// `every_table_is_known`.
enum Coverage {
    Logs(fn(&Value, &mut Collector)),
    /// The table holds no Session log; the reason says what it holds.
    NoLogs(&'static str),
}

const TABLES: &[(&str, Coverage)] = &[
    ("session/boundary-cases.json", Coverage::Logs(capture_cases)),
    (
        "session/event-envelope-cases.json",
        Coverage::Logs(row_cases),
    ),
    (
        "session/fault-cases.json",
        Coverage::Logs(plain_log_file_cases),
    ),
    ("session/fork-cases.json", Coverage::Logs(capture_cases)),
    (
        "session/generation-header-cases.json",
        Coverage::Logs(header_cases),
    ),
    ("session/goal-cases.json", Coverage::Logs(capture_cases)),
    ("session/header-cases.json", Coverage::Logs(header_cases)),
    (
        "session/history-cases.json",
        Coverage::Logs(released_row_cases),
    ),
    ("session/inbox-cases.json", Coverage::Logs(capture_cases)),
    (
        "session/json-parse-cases.json",
        Coverage::NoLogs("JSON texts, not Session logs"),
    ),
    ("session/list-cases.json", Coverage::Logs(layout_cases)),
    (
        "session/log-scan-cases.json",
        Coverage::Logs(log_scan_cases),
    ),
    ("session/lookup-cases.json", Coverage::Logs(layout_cases)),
    (
        "session/migrated-restore-cases.json",
        Coverage::Logs(released_row_cases),
    ),
    (
        "session/migration-publication-cases.json",
        Coverage::Logs(plain_log_file_cases),
    ),
    (
        "session/number-cases.json",
        Coverage::NoLogs("number lexemes and doubles, not Session logs"),
    ),
    (
        "session/plain-append-cases.json",
        Coverage::Logs(plain_append_cases),
    ),
    (
        "session/plain-log-file-cases.json",
        Coverage::Logs(plain_log_file_cases),
    ),
    (
        "session/prefix-restore-cases.json",
        Coverage::Logs(capture_cases),
    ),
    ("session/pressure-cases.json", Coverage::Logs(capture_cases)),
    (
        "session/prompt-admission-cases.json",
        Coverage::Logs(capture_cases),
    ),
    (
        "session/relationships-cases.json",
        Coverage::Logs(relationship_cases),
    ),
    ("session/restore-cases.json", Coverage::Logs(capture_cases)),
    (
        "session/row-encode-cases.json",
        Coverage::Logs(row_encode_cases),
    ),
    (
        "session/source-event-seqs-cases.json",
        Coverage::NoLogs("one sourceEventSeqs field value per case, not Session logs"),
    ),
    ("session/stat-cases.json", Coverage::Logs(layout_cases)),
    ("session/subagent-cases.json", Coverage::Logs(capture_cases)),
    (
        "session/subagent-catalog-cases.json",
        Coverage::Logs(capture_cases),
    ),
    (
        "session/unfinished-work-cases.json",
        Coverage::Logs(capture_cases),
    ),
    ("session/usage-cases.json", Coverage::Logs(capture_cases)),
    (
        "session/v0-to-v1-cases.json",
        Coverage::Logs(released_row_cases),
    ),
    (
        "session/v1-codec-cases.json",
        Coverage::Logs(released_row_cases),
    ),
    (
        "session/v1-to-v2-cases.json",
        Coverage::Logs(released_row_cases),
    ),
    (
        "session/v1-to-v2-decoded-cases.json",
        Coverage::Logs(released_row_cases),
    ),
    (
        "session/v1-to-v2-run-cases.json",
        Coverage::Logs(released_row_cases),
    ),
    (
        "session/v2-to-v3-cases.json",
        Coverage::Logs(released_row_cases),
    ),
    ("session/v3-row-cases.json", Coverage::Logs(row_cases)),
    ("session/zstd-cases.json", Coverage::Logs(zstd_cases)),
    (
        "runtime/request-derivation-cases.json",
        Coverage::Logs(request_derivation_cases),
    ),
    (
        "runtime/restored-request-derivation-cases.json",
        Coverage::Logs(capture_cases),
    ),
];

const CAPTURES: &str = "conformance/runtime/request-reconstruction";

fn entries(directory: &str) -> BTreeSet<String> {
    std::fs::read_dir(repo_path(directory))
        .expect("read a table directory")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .into_string()
                .expect("a UTF-8 name")
        })
        .collect()
}

struct TableReport {
    table: String,
    logs: usize,
    referenced: usize,
    /// Why a table holds no Session log.
    excluded: Option<&'static str>,
}

/// Every log of every table and capture, in a fixed order, with a count per table.
fn collect() -> (Vec<Log>, Vec<TableReport>) {
    let mut logs = Vec::new();
    let mut reports = Vec::new();
    let mut captures: Vec<String> = entries(CAPTURES).into_iter().collect();
    captures.sort();
    let mut collector = Collector {
        table: "runtime/request-reconstruction".to_owned(),
        logs: Vec::new(),
        referenced: 0,
    };
    for capture in &captures {
        let path = format!("{CAPTURES}/{capture}/session.jsonl");
        collector.plain(capture, file_lines(&path), String::new());
    }
    reports.push(TableReport {
        table: collector.table.clone(),
        logs: collector.logs.len(),
        referenced: 0,
        excluded: None,
    });
    logs.append(&mut collector.logs);
    for (name, coverage) in TABLES {
        let load = match coverage {
            Coverage::Logs(load) => load,
            Coverage::NoLogs(reason) => {
                reports.push(TableReport {
                    table: (*name).to_owned(),
                    logs: 0,
                    referenced: 0,
                    excluded: Some(reason),
                });
                continue;
            }
        };
        let table = read_json(&format!("conformance/{name}"));
        let mut collector = Collector {
            table: (*name).to_owned(),
            logs: Vec::new(),
            referenced: 0,
        };
        load(&table, &mut collector);
        reports.push(TableReport {
            table: (*name).to_owned(),
            logs: collector.logs.len(),
            referenced: collector.referenced,
            excluded: None,
        });
        logs.append(&mut collector.logs);
    }
    (logs, reports)
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Step {
    Key(String),
    Index(usize),
}

fn path_text(path: &[Step]) -> String {
    let mut text = String::new();
    for step in path {
        match step {
            Step::Key(key) => write!(text, "/{key}"),
            Step::Index(at) => write!(text, "/{at}"),
        }
        .expect("write to a string");
    }
    text
}

/// Every member value and array element of `root`, at every level.
fn positions(root: &Value) -> Vec<Vec<Step>> {
    let mut found = Vec::new();
    let mut pending = vec![(root, Vec::new())];
    while let Some((value, path)) = pending.pop() {
        let children: Vec<(Step, &Value)> = match value {
            Value::Array(items) => items
                .iter()
                .enumerate()
                .map(|(at, item)| (Step::Index(at), item))
                .collect(),
            Value::Object(fields) => fields
                .iter()
                .map(|(key, item)| (Step::Key(key.clone()), item))
                .collect(),
            _ => continue,
        };
        for (step, child) in children.into_iter().rev() {
            let mut child_path = path.clone();
            child_path.push(step);
            found.push(child_path.clone());
            pending.push((child, child_path));
        }
    }
    found.sort();
    found
}

/// An array or an object chain [`DEPTH`] containers deep around `1`.
pub fn nested(object: bool) -> String {
    if object {
        format!("{}1{}", "{\"a\":".repeat(DEPTH), "}".repeat(DEPTH))
    } else {
        format!("{}1{}", "[".repeat(DEPTH), "]".repeat(DEPTH))
    }
}

/// `line` with the value at `path` replaced by `deep`.
fn deepen(line: &str, path: &[Step], deep: &str) -> String {
    let mut row = parse_json(line).expect("a deepened line parses");
    let mut node = &mut row;
    for step in path {
        node = match (node, step) {
            (Value::Array(items), Step::Index(at)) => &mut items[*at],
            (Value::Object(fields), Step::Key(key)) => fields.get_mut(key).expect("member"),
            _ => unreachable!("positions name existing values"),
        };
    }
    *node = Value::String(PLACEHOLDER.to_owned());
    let text = json_text(&row);
    dismantle(row);
    let slot = json_text(&Value::String(PLACEHOLDER.to_owned()));
    assert_eq!(text.matches(&slot).count(), 1, "the slot is unique");
    text.replacen(&slot, deep, 1)
}

/// One deepened position of one log.
struct Variant {
    log: usize,
    line: usize,
    path: Vec<Step>,
    object: bool,
}

/// Whether an entry point accepts the log as the table holds it.
fn accepted(log: &Log) -> bool {
    let bytes = log_bytes(&log.lines, &log.tail);
    match log.form {
        Form::Plain(Some(3)) => restore_plain_log(&bytes, PathPlatform::Posix, BUDGET).is_ok(),
        Form::Plain(Some(version)) => migrate_released_generation(&bytes, version, BUDGET).is_ok(),
        Form::Plain(None) => false,
        Form::Events => log.lines.iter().skip(1).all(|line| {
            parse_json(line).is_ok_and(|event| {
                let encoded = encode_event_line(&event).is_ok();
                dismantle(event);
                encoded
            })
        }),
    }
}

fn row_type(line: &str, at: usize) -> String {
    if at == 0 {
        return "<header>".to_owned();
    }
    let Ok(row) = parse_json(line) else {
        return "<not JSON>".to_owned();
    };
    let kind = row
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("<untyped>")
        .to_owned();
    dismantle(row);
    kind
}

/// Which variants a sweep runs; see the module comment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Sample,
    Full,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Sample => "sample",
            Self::Full => "full",
        }
    }

    fn from_env() -> Self {
        match std::env::var(MODE_ENV).as_deref() {
            Ok("full") => Self::Full,
            Ok("sample") => Self::Sample,
            other => panic!("{MODE_ENV}: unknown sweep mode {other:?}"),
        }
    }
}

/// Every log, accepted ones first, and `mode`'s variants over the
/// deduplicated positions.
fn variants(mode: Mode) -> (Vec<Log>, Vec<TableReport>, Vec<Variant>) {
    let (mut logs, reports) = collect();
    let mut keyed: Vec<(bool, Log)> = logs.drain(..).map(|log| (!accepted(&log), log)).collect();
    keyed.sort_by_key(|(refused, _)| *refused);
    let logs: Vec<Log> = keyed.into_iter().map(|(_, log)| log).collect();
    let mut seen: BTreeMap<(Form, String, String), usize> = BTreeMap::new();
    let mut variants = Vec::new();
    let mut position: usize = 0;
    for (at, log) in logs.iter().enumerate() {
        for (line_at, line) in log.lines.iter().enumerate() {
            let Ok(row) = parse_json(line) else {
                continue;
            };
            let kind = row_type(line, line_at);
            for path in positions(&row) {
                let uses = seen
                    .entry((log.form, kind.clone(), path_text(&path)))
                    .or_default();
                if *uses == LOGS_PER_POSITION {
                    continue;
                }
                *uses += 1;
                let shapes: &[bool] = match mode {
                    Mode::Full => &[false, true],
                    Mode::Sample if position.is_multiple_of(SAMPLE_STRIDE) => {
                        if (position / SAMPLE_STRIDE).is_multiple_of(2) {
                            &[false]
                        } else {
                            &[true]
                        }
                    }
                    Mode::Sample => &[],
                };
                position += 1;
                for &object in shapes {
                    variants.push(Variant {
                        log: at,
                        line: line_at,
                        path: path.clone(),
                        object,
                    });
                }
            }
            dismantle(row);
        }
    }
    (logs, reports, variants)
}

pub fn log_bytes(lines: &[String], tail: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    for line in lines {
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(tail.as_bytes());
    bytes
}

/// A raw-block Zstd frame holding `content`, as a writer's frame decodes.
fn raw_frame(content: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x38];
    let mut blocks = content.chunks(131_072).peekable();
    if blocks.peek().is_none() {
        frame.extend([1, 0, 0]);
    }
    while let Some(block) = blocks.next() {
        let header = (u32::try_from(block.len()).expect("a block length") << 3)
            | u32::from(blocks.peek().is_none());
        frame.extend(&header.to_le_bytes()[..3]);
        frame.extend(block);
    }
    frame
}

/// The log as a writer frames it: the header record, then the rest.
pub fn zstd_bytes(bytes: &[u8]) -> Vec<u8> {
    let split = bytes
        .iter()
        .position(|&byte| byte == b'\n')
        .map_or(bytes.len(), |at| at + 1);
    let mut framed = raw_frame(&bytes[..split]);
    framed.extend(raw_frame(&bytes[split..]));
    framed
}

/// Every line parsed, header first; `None`, with the parsed ones dismantled,
/// when a line is not JSON or there is none.
pub fn parse_lines(lines: &[String]) -> Option<Vec<Value>> {
    let mut parsed = Vec::new();
    for line in lines {
        match parse_json(line) {
            Ok(value) => parsed.push(value),
            Err(_) => {
                parsed.into_iter().for_each(dismantle);
                return None;
            }
        }
    }
    (!parsed.is_empty()).then_some(parsed)
}

/// Discards `Debug` output.
struct Sink;

impl std::fmt::Write for Sink {
    fn write_str(&mut self, _: &str) -> std::fmt::Result {
        Ok(())
    }
}

/// What a caller may do with any result: format it and drop it.
pub fn inspect<T: std::fmt::Debug>(value: T) {
    eprintln!("R");
    write!(Sink, "{value:?}").expect("formatting into a sink");
    drop(value);
}

/// [`inspect`], after cloning the result and comparing the clone with it.
pub fn inspect_twin<T: std::fmt::Debug + Clone + PartialEq>(value: T) {
    eprintln!("R");
    let copy = value.clone();
    std::hint::black_box(copy == value);
    inspect(copy);
    inspect(value);
}

/// The Session identity of a log's original header, naming its directory.
#[derive(Clone)]
pub struct Identity {
    pub id: String,
    pub cwd: Option<String>,
}

pub fn identity(header: Option<&String>) -> Option<Identity> {
    let header = parse_json(header?).ok()?;
    let id = header.get("id").and_then(Value::as_str).map(str::to_owned);
    let cwd = match header.get("cwd") {
        None => Some(None),
        Some(Value::String(cwd)) => Some(Some(cwd.clone())),
        Some(_) => None,
    };
    dismantle(header);
    Some(Identity { id: id?, cwd: cwd? })
}

/// The seq a deepened row carries, or its line number.
fn row_label(line: &str, at: usize) -> String {
    if at == 0 {
        return "header".to_owned();
    }
    let Ok(row) = parse_json(line) else {
        return format!("line {at}");
    };
    let label = row
        .get("seq")
        .filter(|seq| seq.is_number())
        .map_or_else(|| format!("line {at}"), |seq| format!("seq {seq}"));
    dismantle(row);
    label
}

/// A directory this test owns, removed on drop.
struct Scratch(PathBuf);

/// A shard's scratch directory in process `pid`; its parent removes it when
/// the shard crashes, which skips [`Scratch`]'s drop.
fn shard_scratch(pid: u32, shard: usize) -> PathBuf {
    std::env::temp_dir().join(format!("bake-session-deepened-{pid}-shard-{shard}"))
}

impl Scratch {
    fn new(shard: usize) -> Self {
        let path = shard_scratch(std::process::id(), shard);
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir(&path).expect("create an unused scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Per entry point: accepted, refused.
pub type Tally = BTreeMap<&'static str, [u64; 2]>;

/// One deepened log, handed to a caller's entry points on a [`STACK`] thread.
pub struct VariantInput {
    pub form: Form,
    /// The header line, then one line per row, without LF; one is deepened.
    pub lines: Vec<String>,
    pub tail: String,
    /// The original header's identity, naming the log's directory.
    pub identity: Option<Identity>,
    /// [`Form::Events`]: the inherited event count the header is created with.
    pub inherited: Option<u64>,
    /// A directory the variant may use; empty between variants.
    pub scratch: PathBuf,
    /// A value [`DEPTH`] deep of the other shape, for appended events.
    pub other_deep: String,
}

/// Announce an entry point on stderr before running it.
pub fn enter(entry: &'static str) {
    eprintln!("E\t{entry}");
}

/// Run `entry`, announced, and tally whether it accepted.
pub fn call<T, E>(
    tally: &mut Tally,
    entry: &'static str,
    run: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    enter(entry);
    let result = run();
    tally.entry(entry).or_default()[usize::from(result.is_err())] += 1;
    result
}

/// The variants of the shard [`sweep`] names in this process's environment,
/// each through `run` on a [`STACK`] thread; a no-op outside a sweep.
pub fn shard(run: fn(VariantInput) -> Tally) {
    let Ok(shard) = std::env::var(SHARD_ENV) else {
        return;
    };
    let (shard, shards) = shard.split_once('/').expect("SHARD/SHARDS");
    let (shard, shards): (usize, usize) = (
        shard.parse().expect("shard"),
        shards.parse().expect("shards"),
    );
    let from: usize =
        std::env::var(FROM_ENV).map_or(0, |from| from.parse().expect("a variant index"));
    let limit: usize =
        std::env::var(LIMIT_ENV).map_or(usize::MAX, |limit| limit.parse().expect("a limit"));
    let abort_at: Option<usize> = std::env::var(ABORT_ENV)
        .ok()
        .map(|at| at.parse().expect("a variant index"));
    let (logs, _, variants) = variants(Mode::from_env());
    let scratch = Scratch::new(shard);
    let deep = [nested(false), nested(true)];
    let mut tally = Tally::new();
    let mut panicked = Vec::new();
    for (at, variant) in variants
        .iter()
        .enumerate()
        .filter(|(at, _)| at % shards == shard && *at >= from && *at < limit)
    {
        let log = &logs[variant.log];
        let original = &log.lines[variant.line];
        let label = format!(
            "{at}\t{}\t{}\t{}\t{}\t{}",
            log.table,
            log.case,
            row_label(original, variant.line),
            path_text(&variant.path),
            if variant.object { "objects" } else { "arrays" },
        );
        eprintln!("V\t{label}");
        if abort_at == Some(at) {
            enter(INJECTED_ABORT);
            std::process::abort();
        }
        let mut lines = log.lines.clone();
        lines[variant.line] = deepen(original, &variant.path, &deep[usize::from(variant.object)]);
        let tail = log.tail.clone();
        let form = log.form;
        let identity = identity(log.lines.first());
        let input = VariantInput {
            form,
            lines,
            tail,
            identity,
            inherited: log.inherited,
            scratch: scratch.0.clone(),
            other_deep: deep[usize::from(!variant.object)].clone(),
        };
        let outcome = std::thread::Builder::new()
            .stack_size(STACK)
            .spawn(move || run(input))
            .expect("spawn")
            .join();
        match outcome {
            Ok(counts) => {
                for (entry, [accepted, refused]) in counts {
                    let total = tally.entry(entry).or_default();
                    total[0] += accepted;
                    total[1] += refused;
                }
            }
            Err(_) => panicked.push(label),
        }
    }
    // libtest leaves its `test <name> ... ` line open.
    println!();
    for (entry, [accepted, refused]) in &tally {
        println!("T\t{entry}\t{accepted}\t{refused}");
    }
    assert!(
        panicked.is_empty(),
        "variants panicked:\n{}",
        panicked.join("\n")
    );
}

/// What a crashed or failed shard last announced.
struct ShardOutcome {
    success: bool,
    /// Every variant index the shard announced.
    ran: Vec<usize>,
    last_variant: Option<String>,
    last_entry: Option<String>,
    recent: Vec<String>,
    tally: Vec<String>,
    /// The shard's scratch directory, when it outlived the shard.
    leaked: Option<PathBuf>,
}

fn variant_index(variant: &str) -> Option<usize> {
    variant.split('\t').next()?.parse().ok()
}

fn run_shard(command: &mut Command, shard: usize) -> ShardOutcome {
    use std::io::{BufRead, BufReader, Read};
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn this test binary");
    let mut stdout = child.stdout.take().expect("stdout");
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        stdout.read_to_string(&mut text).expect("read stdout");
        text
    });
    let mut ran = Vec::new();
    let mut last_variant: Option<String> = None;
    let mut last_entry: Option<String> = None;
    let mut recent = std::collections::VecDeque::new();
    for line in BufReader::new(child.stderr.take().expect("stderr")).lines() {
        let line = line.unwrap_or_else(|error| format!("<unreadable stderr: {error}>"));
        if let Some(variant) = line.strip_prefix("V\t") {
            ran.extend(variant_index(variant));
            last_variant = Some(variant.to_owned());
            last_entry = None;
        } else if let Some(entry) = line.strip_prefix("E\t") {
            last_entry = Some(entry.to_owned());
        } else if line == "R" {
            if let Some(entry) = &mut last_entry
                && !entry.ends_with(RESULT)
            {
                entry.push_str(RESULT);
            }
        } else {
            recent.push_back(line);
            if recent.len() > 40 {
                recent.pop_front();
            }
        }
    }
    let status = child.wait().expect("wait for the shard");
    let stdout = reader.join().expect("stdout reader");
    let scratch = shard_scratch(child.id(), shard);
    if !status.success() {
        let _ = std::fs::remove_dir_all(&scratch);
    }
    ShardOutcome {
        success: status.success(),
        ran,
        last_variant,
        last_entry,
        recent: recent.into(),
        tally: stdout
            .lines()
            .filter_map(|line| line.strip_prefix("T\t"))
            .map(str::to_owned)
            .collect(),
        leaked: scratch.exists().then_some(scratch),
    }
}

const RESULT: &str = ", formatting, cloning, comparing, or dropping its result";

pub fn this_test(name: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("this test binary"));
    command.args([
        "--exact",
        name,
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ]);
    command
}

/// What a sweep's shards ran and reported.
pub struct SweepReport {
    /// Per entry point: accepted, refused.
    pub tally: BTreeMap<String, [u64; 2]>,
    /// One per crashed or failed shard run: the variant, its entry point,
    /// and the shard's last stderr lines.
    pub failures: Vec<String>,
    /// Every variant index a shard announced.
    pub ran: BTreeSet<usize>,
    /// Shard scratch directories left behind.
    pub leaked: Vec<PathBuf>,
}

/// Run the variants below `limit` in `shards` children of this test binary,
/// each the `#[ignore]`d `shard_test` calling [`shard`], resuming a shard
/// after a crash; `abort_at` makes the shard holding that variant abort
/// there.
fn run_sweep(
    shard_test: &str,
    mode: Mode,
    shards: usize,
    limit: Option<usize>,
    abort_at: Option<usize>,
) -> SweepReport {
    let outcomes: Vec<ShardOutcome> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..shards)
            .map(|shard| {
                scope.spawn(move || {
                    let mut runs = Vec::new();
                    let mut from = 0;
                    loop {
                        let mut command = this_test(shard_test);
                        command
                            .env(SHARD_ENV, format!("{shard}/{shards}"))
                            .env(FROM_ENV, from.to_string())
                            .env(MODE_ENV, mode.name())
                            .env_remove(LIMIT_ENV)
                            .env_remove(ABORT_ENV);
                        if let Some(limit) = limit {
                            command.env(LIMIT_ENV, limit.to_string());
                        }
                        if let Some(at) = abort_at {
                            command.env(ABORT_ENV, at.to_string());
                        }
                        let outcome = run_shard(&mut command, shard);
                        let resume = match (outcome.success, outcome.ran.last()) {
                            (false, Some(at)) if outcome.tally.is_empty() => Some(at + 1),
                            _ => None,
                        };
                        runs.push(outcome);
                        match resume {
                            Some(next) => from = next,
                            None => break runs,
                        }
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("a shard runner"))
            .collect()
    });
    let mut report = SweepReport {
        tally: BTreeMap::new(),
        failures: Vec::new(),
        ran: BTreeSet::new(),
        leaked: Vec::new(),
    };
    for outcome in &outcomes {
        report.ran.extend(outcome.ran.iter().copied());
        report.leaked.extend(outcome.leaked.clone());
        for line in &outcome.tally {
            let mut fields = line.split('\t');
            let entry = fields.next().expect("entry").to_owned();
            let total = report.tally.entry(entry).or_default();
            total[0] += fields
                .next()
                .expect("accepted")
                .parse::<u64>()
                .expect("count");
            total[1] += fields
                .next()
                .expect("refused")
                .parse::<u64>()
                .expect("count");
        }
        if !outcome.success {
            report.failures.push(format!(
                "variant {}\nentry point {}\n{}",
                outcome.last_variant.as_deref().unwrap_or("<none>"),
                outcome.last_entry.as_deref().unwrap_or("<none>"),
                outcome.recent.join("\n")
            ));
        }
    }
    report
}

/// Run `mode`'s variants in sharded children of this test binary, each the
/// `#[ignore]`d `shard_test` calling [`shard`], resuming a shard after a
/// crash; fail naming each crashed variant and entry point, or when an
/// entry point of `accepted` never accepted a variant.
pub fn sweep(shard_test: &str, mode: Mode, accepted: &[&str]) {
    let started = std::time::Instant::now();
    let (logs, reports, variants) = variants(mode);
    let mut per_table: BTreeMap<&str, usize> = BTreeMap::new();
    for variant in &variants {
        *per_table.entry(&logs[variant.log].table).or_default() += 1;
    }
    eprintln!(
        "deepened sweep ({}): depth {DEPTH}, stack {STACK} bytes, {} logs, {} variants",
        mode.name(),
        logs.len(),
        variants.len()
    );
    for report in &reports {
        if let Some(reason) = report.excluded {
            eprintln!("  {}: no Session logs ({reason})", report.table);
            continue;
        }
        eprintln!(
            "  {}: {} logs collected, {} inputs not collected, {} variants",
            report.table,
            report.logs,
            report.referenced,
            per_table.get(report.table.as_str()).copied().unwrap_or(0)
        );
    }
    let total = variants.len();
    drop((logs, variants));
    let shards = std::thread::available_parallelism()
        .map_or(4, usize::from)
        .clamp(2, 16);
    let report = run_sweep(shard_test, mode, shards, None, None);
    for (entry, [accepted, refused]) in &report.tally {
        eprintln!("  {entry}: {accepted} accepted, {refused} refused");
    }
    eprintln!(
        "deepened sweep: {} shards in {:.1?}",
        shards,
        started.elapsed()
    );
    assert!(
        report.failures.is_empty(),
        "deepened variants crashed:\n{}",
        report.failures.join("\n\n")
    );
    assert_eq!(report.ran.len(), total, "every variant ran");
    assert!(
        report.leaked.is_empty(),
        "scratch left: {:?}",
        report.leaked
    );
    for entry in accepted {
        assert!(
            report
                .tally
                .get(*entry)
                .is_some_and(|[accepted, _]| *accepted > 0),
            "no variant reached {entry} with an accepted result"
        );
    }
}

/// A sweep of the first six sampled variants on two shards, one of which
/// aborts at variant 2: the report names variant 2 and the injected entry
/// point, the crashed shard resumes and runs variant 4, and its scratch
/// directory is gone.
pub fn check_crash_reporting(shard_test: &str) {
    let report = run_sweep(shard_test, Mode::Sample, 2, Some(6), Some(2));
    assert_eq!(report.failures.len(), 1, "{:?}", report.failures);
    let failure = &report.failures[0];
    assert!(failure.starts_with("variant 2\t"), "{failure}");
    assert!(
        failure.contains(&format!("entry point {INJECTED_ABORT}\n")),
        "{failure}"
    );
    assert_eq!(report.ran, (0..6).collect(), "the crashed shard resumed");
    assert!(!report.tally.is_empty(), "the resumed shard reported");
    assert!(
        report.leaked.is_empty(),
        "scratch left: {:?}",
        report.leaked
    );
}

/// Every entry of `conformance/session/` and `conformance/runtime/`, other
/// than the captures directory, is a table in [`TABLES`], every capture
/// holds only its log and requests, and every table with logs yields one.
pub fn check_tables_known() {
    let known: BTreeSet<&str> = TABLES.iter().map(|(name, _)| *name).collect();
    let mut found = BTreeSet::new();
    for directory in ["session", "runtime"] {
        for name in entries(&format!("conformance/{directory}")) {
            let entry = format!("{directory}/{name}");
            // The captures directory is read entry by entry below.
            if format!("conformance/{entry}") != CAPTURES {
                found.insert(entry);
            }
        }
    }
    let found: BTreeSet<&str> = found.iter().map(String::as_str).collect();
    assert_eq!(
        found, known,
        "a shared table the deepened harness does not know"
    );
    for capture in entries(CAPTURES) {
        let mut files = entries(&format!("{CAPTURES}/{capture}"));
        assert!(files.remove("session.jsonl"), "{capture}: no session.jsonl");
        assert_eq!(
            files,
            BTreeSet::from(["expected-requests.json".to_owned()]),
            "{capture}: an unread file"
        );
    }
    let (_, reports) = collect();
    for report in reports {
        assert!(
            report.excluded.is_some() || report.logs > 0,
            "{}: no log collected",
            report.table
        );
    }
}
