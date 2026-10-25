//! Group appends against real file bytes: every line and byte prefix of a
//! written group, every failure at every line, checks that fail before a
//! write, and a writer process killed between the lines of a group. Each
//! test owns its temporary directory and any child process it starts.

use std::io::Write;
use std::path::Path;

use serde_json::{Value, json};

use super::*;
use crate::session::durable::{Fault, FaultKind};
use crate::session::manager::crash_support::{
    CHILD_RETURNED_EXIT, CHILD_WATCHDOG_EXIT, Conversation, PATIENCE, TestChild, context_of,
    conversation, entries_of, file_session, lines, omission, provider_problem, request, take_over,
};
use crate::session::{
    FailedLine, LeafSelector, NewSessionOptions, SessionEntry, build_session_context,
};
use crate::test_support::TempDir;

/// A model switch with its thinking level.
fn model_switch() -> Vec<GroupEntry> {
    vec![
        GroupEntry::model_change("faux", "faux-2"),
        GroupEntry::thinking_level_change("low"),
    ]
}

fn custom_group() -> Vec<GroupEntry> {
    vec![
        GroupEntry::Custom {
            custom_type: "bake.test.document.v1".into(),
            data: Some(json!({"revision": 1})),
        },
        GroupEntry::CustomMessage {
            custom_type: "bake.test.document.context.v1".into(),
            content: bake_ai::UserContent::Text("Exact document revision one".into()),
            display: false,
            details: Some(json!({"revision": 1})),
        },
    ]
}

#[test]
fn an_explicit_durable_group_survives_before_any_conversation() {
    let dir = TempDir::new("durable-custom-before-conversation");
    let mut manager = file_session(&dir);
    let file = manager.session_file().expect("file").to_path_buf();
    let ids = manager
        .append_durable_group(custom_group())
        .expect("durable group");
    let projected = manager.build_session_context();
    assert_eq!(projected.messages.len(), 1);
    drop(manager);
    let reopened = SessionManager::open(&file, None, None).expect("reopen");
    assert_eq!(
        reopened
            .entries()
            .iter()
            .map(|entry| entry.id())
            .collect::<Vec<_>>(),
        ids
    );
    assert_eq!(reopened.build_session_context(), projected);
}

#[test]
fn ordinary_custom_groups_remain_lazy() {
    let dir = TempDir::new("lazy-custom-group");
    let mut manager = file_session(&dir);
    let file = manager.session_file().expect("file").to_path_buf();
    manager.append_group(custom_group()).expect("lazy group");
    assert!(!file.exists());
    manager
        .append_durable_group(custom_group())
        .expect("flush pending entries");
    assert_eq!(entries_of(&file).len(), 4);
}

#[test]
fn initial_durable_custom_groups_keep_only_the_synced_prefix_on_failure() {
    for line in 0..2 {
        for kind in [FaultKind::Refuse, FaultKind::Tear(17), FaultKind::FailSync] {
            let dir = TempDir::new("durable-custom-prefix");
            let mut manager = file_session(&dir);
            let file = manager.session_file().expect("file").to_path_buf();
            manager.arm_group_fault(Fault { line, kind });
            let error = manager
                .append_durable_group(custom_group())
                .expect_err("injected write failure");
            let SessionError::GroupWrite(failure) = error else {
                panic!("{error:?}")
            };
            assert_eq!(failure.written.len(), line);
            assert_eq!(manager.entries().len(), line);
            let seen = entries_of(&file);
            assert_eq!(
                seen.len(),
                line + usize::from(failure.failed == FailedLine::Unsynced)
            );
            assert_eq!(manager.entries(), seen[..line].iter().collect::<Vec<_>>());
        }
    }
}

/// A written group is the lines its single appends write, chained from the
/// leaf, with the leaf on its last entry.
#[test]
fn a_group_writes_the_lines_its_single_appends_write() {
    let dir = TempDir::new("group-lines");
    let single_dir = TempDir::new("group-lines-single");
    let mut grouped = file_session(&dir);
    let mut single = file_session(&single_dir);
    let ids = conversation(&mut grouped);
    let single_ids = conversation(&mut single);
    let file = grouped.session_file().expect("file").to_path_buf();
    let before = std::fs::read(&file).expect("read");
    let leaf = grouped.leaf_id().map(str::to_owned);

    let mut group = model_switch();
    group.extend(omission(&ids));
    let written = grouped.append_group(group).expect("group");
    single.append_model_change("faux", "faux-2").expect("model");
    single.append_thinking_level_change("low").expect("level");
    for id in single_ids.results.iter().chain([&single_ids.assistant]) {
        single.append_context_edit(id, None).expect("edit");
    }

    let after = std::fs::read(&file).expect("read");
    assert!(after.starts_with(&before));
    let added: Vec<Value> = lines(&after[before.len()..])
        .iter()
        .map(|line| serde_json::from_slice(line).expect("json line"))
        .collect();
    let single_bytes = std::fs::read(single.session_file().expect("file")).expect("read");
    let single_added: Vec<Value> = lines(&single_bytes)
        .iter()
        .skip(lines(&before).len())
        .map(|line| serde_json::from_slice(line).expect("json line"))
        .collect();
    assert_eq!(added.len(), 5);
    let mut parent = leaf.map_or(Value::Null, Value::String);
    for ((line, id), single_line) in added.iter().zip(&written).zip(&single_added) {
        assert_eq!(line["id"], json!(id));
        assert_eq!(line["parentId"], parent);
        parent = line["id"].clone();
        // The same members in the same order; ids and times differ.
        let shape = |value: &Value| -> Vec<(String, Value)> {
            value
                .as_object()
                .expect("object")
                .iter()
                .map(|(key, value)| match key.as_str() {
                    "id" | "parentId" | "timestamp" => (key.clone(), Value::Null),
                    "targetId" => (key.clone(), Value::Null),
                    _ => (key.clone(), value.clone()),
                })
                .collect()
        };
        assert_eq!(shape(line), shape(single_line));
    }
    assert_eq!(grouped.leaf_id(), written.last().map(String::as_str));
    // The file, the manager, and the single appends project alike.
    let context = context_of(&file);
    assert_eq!(context, grouped.build_session_context());
    assert_eq!(context, single.build_session_context());
    assert_eq!(context.messages.len(), 1);
    assert_eq!(context.thinking_level, "low");
}

/// Which error a refused group raises.
type Expected = fn(&SessionError) -> bool;

/// Builds a fault for one case.
type MakeFault = fn() -> FaultKind;

/// A failed check changes nothing, in memory or on disk; an empty group
/// writes nothing.
#[test]
fn a_failed_check_writes_and_keeps_nothing() {
    let dir = TempDir::new("group-checks");
    let mut manager = file_session(&dir);
    let ids = conversation(&mut manager);
    let model = manager
        .append_model_change("faux", "faux-1")
        .expect("model");
    let file = manager.session_file().expect("file").to_path_buf();
    let bytes = std::fs::read(&file).expect("read");
    let entries = manager.entry_count();
    let leaf = manager.leaf_id().map(str::to_owned);
    let cases: [(GroupEntry, Expected); 2] = [
        (
            GroupEntry::context_edit("missing", None),
            |error| matches!(error, SessionError::EntryNotFound(id) if id == "missing"),
        ),
        (GroupEntry::context_edit(model.clone(), None), |error| {
            matches!(error, SessionError::NotEditable(_))
        }),
    ];
    for (bad, expected) in cases {
        let error = manager
            .append_group(vec![
                GroupEntry::model_change("faux", "faux-2"),
                GroupEntry::context_edit(ids.assistant.clone(), None),
                bad,
            ])
            .expect_err("refused");
        assert!(expected(&error), "{error:?}");
        assert_eq!(std::fs::read(&file).expect("read"), bytes);
        assert_eq!(manager.entry_count(), entries);
        assert_eq!(manager.leaf_id().map(str::to_owned), leaf);
    }
    // Off the active branch.
    manager.branch(&ids.assistant).expect("branch");
    let error = manager
        .append_group(vec![GroupEntry::context_edit(ids.results[1].clone(), None)])
        .expect_err("off branch");
    assert!(
        matches!(error, SessionError::NotOnActiveBranch(_)),
        "{error:?}"
    );
    assert_eq!(manager.leaf_id(), Some(ids.assistant.as_str()));
    assert_eq!(
        manager.append_group(Vec::new()).expect("empty"),
        Vec::<String>::new()
    );
    assert_eq!(std::fs::read(&file).expect("read"), bytes);
    // The next entry chains to the leaf the failed groups left.
    let next = manager.append_model_change("faux", "faux-2").expect("next");
    assert_eq!(
        manager.entry(&next).and_then(SessionEntry::parent_id),
        Some(ids.assistant.as_str())
    );
}

/// Before a session holds a conversation its file is not written, groups
/// included; an in-memory session never writes.
#[test]
fn groups_follow_the_first_write_rule() {
    let dir = TempDir::new("group-unflushed");
    let mut manager = file_session(&dir);
    let file = manager.session_file().expect("file").to_path_buf();
    manager.append_group(model_switch()).expect("group");
    assert!(!file.exists());
    conversation(&mut manager);
    let context = context_of(&file);
    assert_eq!(context.thinking_level, "low");
    assert_eq!(context, manager.build_session_context());

    let mut memory =
        SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
            .expect("memory");
    let ids = conversation(&mut memory);
    memory.append_group(omission(&ids)).expect("group");
    assert!(memory.build_session_context().messages.len() == 1);
}

/// Every byte prefix of a written group loads as Pi's loader reads it and
/// projects as the entries it holds do; every prefix rebuilds a request a
/// provider accepts; and a writer that takes over any prefix appends on a
/// line of its own. Pi's order for the omission (the attempt first) would
/// leave tool results without their call after its first line.
#[test]
fn every_prefix_of_a_group_loads_and_rebuilds_a_valid_request() {
    for (name, make) in [
        ("omission", omission as fn(&Conversation) -> Vec<GroupEntry>),
        ("model", |_: &Conversation| model_switch()),
    ] {
        let dir = TempDir::new(&format!("group-prefix-{name}"));
        let mut manager = file_session(&dir);
        let ids = conversation(&mut manager);
        let file = manager.session_file().expect("file").to_path_buf();
        let before = std::fs::read(&file).expect("read").len();
        let kept = manager.entries().len();
        manager.append_group(make(&ids)).expect("group");
        let all: Vec<SessionEntry> = manager.entries().into_iter().cloned().collect();
        drop(manager);
        let bytes = std::fs::read(&file).expect("read");
        for cut in before..=bytes.len() {
            // Each prefix owns its lock path: another test's child may
            // inherit a just-closed descriptor until it reaches exec.
            let copy = dir.join(&format!("prefix-{cut}.jsonl"));
            let prefix = &bytes[..cut];
            std::fs::write(&copy, prefix).expect("write prefix");
            // Complete lines, and a last line whose whole JSON is there.
            let whole = lines(&prefix[before..])
                .iter()
                .filter(|line| serde_json::from_slice::<Value>(line).is_ok())
                .count();
            let loaded = entries_of(&copy);
            assert_eq!(loaded, all[..kept + whole], "{name} cut {cut}");
            let context = context_of(&copy);
            assert_eq!(
                context,
                build_session_context(&all[..kept + whole], LeafSelector::Last)
            );
            if let Some(problem) = provider_problem(&request(&context)) {
                panic!("{name} cut {cut}: {problem}");
            }
            let at_line = prefix.last() == Some(&b'\n');
            if at_line || cut % 13 == 0 {
                let mut taken = SessionManager::open(&copy, None, None).expect("take over");
                taken
                    .append_session_info("after")
                    .expect("append after prefix");
                drop(taken);
                let reloaded = entries_of(&copy);
                assert_eq!(reloaded.len(), kept + whole + 1, "{name} cut {cut}");
                assert_eq!(reloaded[..kept + whole], all[..kept + whole]);
            }
        }
    }
    // Pi's order: after its first line, the results answer an omitted call.
    let mut memory =
        SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
            .expect("memory");
    let ids = conversation(&mut memory);
    memory
        .append_context_edit(&ids.assistant, None)
        .expect("Pi's first edit");
    assert!(provider_problem(&request(&memory.build_session_context())).is_some());
}

/// A write that fails at any line keeps exactly the lines before it in the
/// file and in memory, leaves the failed line as reported, and writes no
/// later line.
#[test]
fn a_failed_write_at_each_line_keeps_the_written_prefix() {
    let kinds: [(&str, MakeFault, FailedLine); 4] = [
        ("refuse", || FaultKind::Refuse, FailedLine::NotWritten),
        ("tear-none", || FaultKind::Tear(0), FailedLine::NotWritten),
        ("tear", || FaultKind::Tear(17), FailedLine::Torn),
        ("sync", || FaultKind::FailSync, FailedLine::Unsynced),
    ];
    for line in 0..3 {
        for (name, kind, state) in &kinds {
            let dir = TempDir::new(&format!("group-fail-{line}-{name}"));
            let mut manager = file_session(&dir);
            let ids = conversation(&mut manager);
            let file = manager.session_file().expect("file").to_path_buf();
            let before = std::fs::read(&file).expect("read");
            let leaf = manager.leaf_id().map(str::to_owned);
            let kept = manager.entries().len();
            manager.arm_group_fault(Fault { line, kind: kind() });
            let error = manager.append_group(omission(&ids)).expect_err("injected");
            let SessionError::GroupWrite(failure) = &error else {
                panic!("{error:?}");
            };
            assert_eq!((failure.written.len(), failure.len), (line, 3));
            assert_eq!(failure.failed, *state, "{line} {name}");
            assert_eq!(failure.path, file);
            assert!(
                error.to_string().contains(&format!("{line} of 3")),
                "{error}"
            );

            let after = std::fs::read(&file).expect("read");
            let added = lines(&after[before.len()..]);
            let complete = added.iter().filter(|l| l.ends_with(b"\n")).count();
            let torn = added.len() - complete;
            let expected = match state {
                FailedLine::NotWritten => (line, 0),
                FailedLine::Torn => (line, 1),
                FailedLine::Unsynced => (line + 1, 0),
            };
            assert_eq!((complete, torn), expected, "{line} {name}");
            // The manager keeps the written entries, with the leaf on the
            // last, and nothing after.
            let entries: Vec<String> = manager
                .entries()
                .iter()
                .skip(kept)
                .map(|entry| entry.id().to_owned())
                .collect();
            assert_eq!(entries, failure.written);
            assert_eq!(
                manager.leaf_id().map(str::to_owned),
                failure.written.last().cloned().or(leaf)
            );
            // A reader sees the manager's entries, and a line written but
            // not synced as well.
            let read = entries_of(&file);
            let extra = usize::from(*state == FailedLine::Unsynced);
            assert_eq!(read.len(), kept + line + extra);
            assert_eq!(
                read.iter().take(kept + line).collect::<Vec<_>>(),
                manager.entries()
            );
            if let Some(problem) = provider_problem(&request(&context_of(&file))) {
                panic!("{line} {name}: {problem}");
            }
        }
    }
}

/// The two failures that leave the failed line readable: written in full
/// without a sync, and torn after all of its JSON but before its newline.
/// The error and the manager name the known synced prefix only, while an
/// immediate reopen reads one entry more and so projects differently; the
/// error says which state the line is in, and a writer that takes over
/// appends on a line of its own.
#[test]
fn a_readable_failed_line_is_not_in_the_known_synced_prefix() {
    for (name, kind, state, text) in [
        (
            "unsynced",
            FaultKind::FailSync,
            FailedLine::Unsynced,
            "the next was written but not synced, so a reader may see it",
        ),
        (
            "whole-json",
            FaultKind::Tear(usize::MAX),
            FailedLine::Torn,
            "the next may be partly written, without its newline or a sync",
        ),
    ] {
        let dir = TempDir::new(&format!("group-readable-{name}"));
        let mut manager = file_session(&dir);
        let ids = conversation(&mut manager);
        let file = manager.session_file().expect("file").to_path_buf();
        let before = std::fs::read(&file).expect("read");
        let kept = manager.entries().len();
        manager.arm_group_fault(Fault { line: 1, kind });
        let error = manager.append_group(omission(&ids)).expect_err("injected");
        let SessionError::GroupWrite(failure) = &error else {
            panic!("{error:?}");
        };
        assert_eq!(failure.failed, state, "{name}");
        assert_eq!(failure.written.len(), 1, "{name}");
        assert!(
            error
                .to_string()
                .contains(&format!("1 of 3 grouped entries written; {text}")),
            "{name}: {error}"
        );
        let after = std::fs::read(&file).expect("read");
        let added = lines(&after[before.len()..]);
        assert_eq!(added.len(), 2, "{name}");
        assert_eq!(
            added[1].ends_with(b"\n"),
            state == FailedLine::Unsynced,
            "{name}"
        );
        // The second line is the whole edit of the second result.
        let second: Value = serde_json::from_slice(added[1]).expect("whole JSON");
        assert_eq!(second["targetId"], json!(ids.results[1]), "{name}");

        let known = manager.build_session_context();
        assert_eq!(manager.entries().len(), kept + 1, "{name}");
        drop(manager);
        let mut reopened = SessionManager::open(&file, None, None).expect("reopen");
        assert_eq!(reopened.entries().len(), kept + 2, "{name}");
        let read = reopened.build_session_context();
        assert_ne!(read, known, "{name}: the reopen reads the failed line");
        // Both views rebuild a request a provider accepts.
        for context in [&known, &read] {
            if let Some(problem) = provider_problem(&request(context)) {
                panic!("{name}: {problem}");
            }
        }
        reopened.append_session_info("after").expect("append");
        drop(reopened);
        let reread = entries_of(&file);
        assert_eq!(reread.len(), kept + 3, "{name}");
        assert_eq!(
            reread.last().map(SessionEntry::entry_type),
            Some("session_info")
        );
    }
}

/// A failure before the group's first line writes and keeps nothing.
#[test]
fn a_closed_writer_refuses_the_whole_group() {
    let dir = TempDir::new("group-closed");
    let mut manager = file_session(&dir);
    conversation(&mut manager);
    let file = manager.session_file().expect("file").to_path_buf();
    let bytes = std::fs::read(&file).expect("read");
    let entries = manager.entry_count();
    manager.close_writer();
    let error = manager.append_group(model_switch()).expect_err("closed");
    assert!(matches!(error, SessionError::WriterClosed(_)), "{error:?}");
    assert_eq!(manager.entry_count(), entries);
    assert_eq!(std::fs::read(&file).expect("read"), bytes);
}

const CHILD_ENV: &str = "BAKE_TEST_GROUP_WRITER";
const CHILD_TEST: &str = "session::manager::group::tests::group_writer_child";
const HOLDING: &str = "bake-test-holding-group-line";

/// Not a test of its own: the child of
/// [`a_writer_killed_between_group_lines_leaves_a_loadable_prefix`], which
/// re-runs this test binary with [`CHILD_ENV`] naming the file, the line
/// to stop at, and how many of its bytes to write first.
#[test]
fn group_writer_child() {
    let Some(spec) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    let spec = spec.to_string_lossy().into_owned();
    let mut parts = spec.splitn(3, '|');
    let (Some(line), Some(torn), Some(file)) = (parts.next(), parts.next(), parts.next()) else {
        panic!("bad spec {spec}");
    };
    let line: usize = line.parse().expect("line");
    let torn: usize = torn.parse().expect("torn");
    let mut manager = SessionManager::open(Path::new(file), None, None).expect("child open");
    let ids = Conversation::of(&manager);
    manager.arm_group_fault(Fault {
        line,
        kind: FaultKind::Hold(
            torn,
            Box::new(|| {
                println!("{HOLDING}");
                let _ = std::io::stdout().flush();
                // The parent kills the child here; a parent that never does
                // leaves no child behind for long.
                std::thread::sleep(PATIENCE * 2);
                std::process::exit(CHILD_WATCHDOG_EXIT);
            }),
        ),
    });
    let _ = manager.append_group(omission(&ids));
    std::process::exit(CHILD_RETURNED_EXIT);
}

/// A writer process killed while it writes a group, after each synced line
/// and inside a line, leaves a file that loads as the lines it synced (the
/// torn part skipped), rebuilds a request a provider accepts, and is taken
/// over by the next writer, whose append starts on a line of its own. The
/// child writes and syncs the torn bytes through the test seam and then
/// waits, so the kill lands between system calls, not inside a `write`.
#[test]
fn a_writer_killed_between_group_lines_leaves_a_loadable_prefix() {
    for (line, torn) in [(0, 0), (1, 0), (2, 0), (1, 20)] {
        let dir = TempDir::new(&format!("group-killed-{line}-{torn}"));
        let mut manager = file_session(&dir);
        conversation(&mut manager);
        let file = manager.session_file().expect("file").to_path_buf();
        let kept: Vec<SessionEntry> = manager.entries().into_iter().cloned().collect();
        let before = std::fs::read(&file).expect("read");
        drop(manager);

        let spec = format!("{line}|{torn}|{}", file.display());
        let mut child = TestChild::spawn(CHILD_TEST, CHILD_ENV, &spec);
        child.wait_for(HOLDING);
        child.kill();
        drop(child);

        let after = std::fs::read(&file).expect("read");
        assert!(after.starts_with(&before));
        let added = lines(&after[before.len()..]);
        assert_eq!(added.len(), line + usize::from(torn > 0), "{line} {torn}");
        assert_eq!(added.last().is_some_and(|l| !l.ends_with(b"\n")), torn > 0);
        let read = entries_of(&file);
        assert_eq!(read[..kept.len()], kept[..]);
        assert_eq!(read.len(), kept.len() + line);
        for entry in &read[kept.len()..] {
            assert_eq!(entry.entry_type(), "context_edit");
        }
        let context = context_of(&file);
        if let Some(problem) = provider_problem(&request(&context)) {
            panic!("{line} {torn}: {problem}");
        }
        // The lock died with the writer.
        let mut taken = take_over(&file);
        assert_eq!(taken.build_session_context(), context);
        taken.append_session_info("after").expect("append");
        drop(taken);
        let reread = entries_of(&file);
        assert_eq!(reread.len(), kept.len() + line + 1);
        assert_eq!(
            reread.last().map(SessionEntry::entry_type),
            Some("session_info")
        );
    }
}
