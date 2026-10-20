//! Pi-format session files through `bake-coding-agent`'s session store: a
//! session file on disk is untrusted (hand edits, torn writes, other tools),
//! and opening one parses, migrates, indexes, and projects it.
//!
//! Input: the file's bytes. They are parsed whole, as Pi's
//! `parseSessionEntries` does, and loaded line by line from a file, as
//! `loadEntriesFromFile` does; then migrated, adopted by an in-memory
//! manager, and projected from several leaves. Oracle: the line written for
//! each parsed entry parses back to one entry that writes the same line.

#![no_main]

use std::cell::OnceCell;
use std::path::PathBuf;

use bake_coding_agent::session::{
    FileEntry, LeafSelector, NewSessionOptions, SessionEntry, SessionManager,
    build_context_entries, build_session_context, build_unanswered_tool_calls,
    load_entries_from_file, migrate_session_entries, parse_session_entries,
    session_entry_to_context_messages,
};
use libfuzzer_sys::fuzz_target;

thread_local! {
    /// One scratch file per fuzzing process; jobs are separate processes.
    static SCRATCH: OnceCell<PathBuf> = const { OnceCell::new() };
}

fn scratch_path() -> PathBuf {
    SCRATCH.with(|cell| {
        cell.get_or_init(|| {
            std::env::temp_dir().join(format!("bake-fuzz-session-{}.jsonl", std::process::id()))
        })
        .clone()
    })
}

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let mut entries = parse_session_entries(&text);
    for entry in &entries {
        let line = entry.to_line();
        let again = parse_session_entries(&line);
        assert_eq!(again.len(), 1, "a written line {line:?} is one entry");
        assert_eq!(again[0].to_line(), line, "a written line is stable");
    }

    let path = scratch_path();
    if std::fs::write(&path, data).is_ok() {
        let _ = load_entries_from_file(&path);
    }

    migrate_session_entries(&mut entries);
    let tree: Vec<SessionEntry> = entries
        .iter()
        .filter_map(FileEntry::as_entry)
        .cloned()
        .collect();
    let mut leaves = vec![LeafSelector::Last, LeafSelector::Empty];
    if let Some(first) = tree.first() {
        leaves.push(LeafSelector::Id(first.id()));
    }
    if let Some(middle) = tree.get(tree.len() / 2) {
        leaves.push(LeafSelector::Id(middle.id()));
    }
    for leaf in leaves {
        let _ = build_session_context(&tree, leaf);
        let _ = build_context_entries(&tree, leaf);
        let _ = build_unanswered_tool_calls(&tree, leaf);
    }
    for entry in &tree {
        let _ = session_entry_to_context_messages(entry);
        let _ = entry.typed();
        let _ = entry.message();
    }

    if let Ok(mut manager) =
        SessionManager::in_memory(Some("/fuzz"), NewSessionOptions::default(), entries)
    {
        let _ = manager.tree();
        let _ = manager.build_session_context();
        let _ = manager.session_name();
        let ids: Vec<String> = manager
            .entries()
            .iter()
            .map(|entry| entry.id().to_owned())
            .collect();
        if let Some(id) = ids.first() {
            let _ = manager.branched_copy(Some(id));
            if manager.branch(id).is_ok() {
                let _ = manager.build_session_context();
            }
        }
    }
});
