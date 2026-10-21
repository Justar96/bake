//! Simulated power loss. This is not a physical power cut: no machine
//! loses power and no device drops a write cache. The durable writes log
//! every write, file sync, creation, truncation, rename, removal, and
//! directory sync they make (`durable::disk_log`, compiled only into the
//! crate's tests), and a model of the session directory replays each prefix
//! of that log and enumerates what a power loss right after it can leave:
//!
//! - a file keeps the bytes it held at its last sync, or, of the bytes
//!   written since, any prefix that ends at a line boundary (none, each
//!   whole line, or all), as if the device persisted only some of them;
//!   bytes written after a truncation are cut the same way, so a truncated
//!   file may come back empty;
//! - names (creations, renames, removals) are kept as of the last
//!   directory sync, or all of them, as if the directory reached the device
//!   anyway.
//!
//! Each state is written to the session directory and checked as a
//! restart would see it: every session file loads as Pi's loader loads it,
//! rebuilds a provider-valid request, and is taken over by the next
//! writer; an append or a group leaves a whole-line prefix of what it
//! wrote; a rewrite through a temporary file leaves the old or the new
//! file, never a mix, and a listing finds only whole sessions. Once a write
//! returns, every state holds all of it. Real kills inside a write and
//! real kernel errors are tested elsewhere (`kernel_write_tests`,
//! `seccomp_write_tests`, `full_filesystem_tests`); this model cannot see
//! a filesystem's own reordering beyond the two rules above.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::json;

use super::*;
use crate::session::durable::disk_log::{self, DiskOp};
use crate::session::manager::crash_support::{
    Conversation, context_of, conversation, entries_of, file_session, is_temp_of, lines, listed,
    message, migrated, omission, provider_problem, read, request, take_over, tool_result,
    tool_turn, usage, v2_session,
};
use crate::test_support::TempDir;

/// A file's contents in the model: what its last sync made durable, and
/// what it holds since, `base` (the durable bytes, or nothing after a
/// truncation) followed by the unsynced `tail`.
#[derive(Debug, Clone)]
struct Inode {
    durable: Vec<u8>,
    base: Vec<u8>,
    tail: Vec<u8>,
    dirty: bool,
}

impl Inode {
    fn synced(bytes: Vec<u8>) -> Self {
        Self {
            durable: bytes.clone(),
            base: bytes,
            tail: Vec::new(),
            dirty: false,
        }
    }

    /// The contents a power loss can leave.
    fn outcomes(&self) -> BTreeSet<Vec<u8>> {
        let mut out = BTreeSet::from([self.durable.clone()]);
        if !self.dirty {
            return out;
        }
        let mut cuts = vec![0, self.tail.len()];
        cuts.extend(
            self.tail
                .iter()
                .enumerate()
                .filter(|(_, byte)| **byte == b'\n')
                .map(|(at, _)| at + 1),
        );
        for cut in cuts {
            let mut bytes = self.base.clone();
            bytes.extend_from_slice(&self.tail[..cut]);
            out.insert(bytes);
        }
        out
    }
}

/// A file name in the directory, and the files a power loss can leave.
type State = BTreeMap<String, Vec<u8>>;

/// The session directory in the model.
#[derive(Debug, Clone)]
struct Disk {
    dir: PathBuf,
    names: BTreeMap<String, usize>,
    durable_names: BTreeMap<String, usize>,
    inodes: Vec<Inode>,
}

/// Whether `name` is a writer's lock file, which the model leaves alone.
fn is_lock(name: &str) -> bool {
    name.ends_with(".lock")
}

impl Disk {
    /// The files in `dir`, all durable.
    fn snapshot(dir: &Path) -> Self {
        let mut disk = Self {
            dir: dir.canonicalize().expect("canonical directory"),
            names: BTreeMap::new(),
            durable_names: BTreeMap::new(),
            inodes: Vec::new(),
        };
        for entry in std::fs::read_dir(dir).expect("read dir") {
            let entry = entry.expect("entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_lock(&name) || !entry.file_type().expect("type").is_file() {
                continue;
            }
            disk.inodes.push(Inode::synced(read(&entry.path())));
            disk.names.insert(name, disk.inodes.len() - 1);
        }
        disk.durable_names = disk.names.clone();
        disk
    }

    /// The name of `path`, which must be in the directory.
    fn name(&self, path: &Path) -> String {
        let parent = path.parent().expect("parent").canonicalize().expect("dir");
        assert_eq!(parent, self.dir, "{path:?} is outside the directory");
        path.file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned()
    }

    fn inode(&mut self, path: &Path) -> &mut Inode {
        let name = self.name(path);
        let index = *self.names.get(&name).unwrap_or_else(|| panic!("{name}"));
        &mut self.inodes[index]
    }

    fn apply(&mut self, op: &DiskOp) {
        match op {
            DiskOp::Create(path) => {
                let name = self.name(path);
                self.inodes.push(Inode::synced(Vec::new()));
                let created = self.names.insert(name, self.inodes.len() - 1);
                assert!(created.is_none(), "{op:?} over an existing name");
            }
            DiskOp::Truncate(path) => {
                let inode = self.inode(path);
                inode.base.clear();
                inode.tail.clear();
                inode.dirty = true;
            }
            DiskOp::Write(path, bytes) => {
                let inode = self.inode(path);
                inode.tail.extend_from_slice(bytes);
                inode.dirty = true;
            }
            DiskOp::Fsync(path) => {
                let inode = self.inode(path);
                let mut bytes = std::mem::take(&mut inode.base);
                bytes.append(&mut inode.tail);
                *inode = Inode::synced(bytes);
            }
            DiskOp::Rename(from, to) => {
                let (from, to) = (self.name(from), self.name(to));
                let index = self.names.remove(&from).expect("renamed name");
                self.names.insert(to, index);
            }
            DiskOp::Remove(path) => {
                let name = self.name(path);
                self.names.remove(&name).expect("removed name");
            }
            DiskOp::DirSync(dir) => {
                let dir = dir.canonicalize().expect("synced dir");
                assert_eq!(dir, self.dir, "{op:?}");
                self.durable_names = self.names.clone();
            }
        }
    }

    /// Every state a power loss now can leave.
    fn crash_states(&self) -> BTreeSet<State> {
        let mut states = BTreeSet::new();
        for names in [&self.durable_names, &self.names] {
            let mut partial: Vec<State> = vec![State::new()];
            for (name, index) in names {
                let outcomes = self.inodes[*index].outcomes();
                partial = partial
                    .into_iter()
                    .flat_map(|state| {
                        outcomes.iter().map(move |bytes| {
                            let mut state = state.clone();
                            state.insert(name.clone(), bytes.clone());
                            state
                        })
                    })
                    .collect();
            }
            states.extend(partial);
        }
        states
    }
}

/// Every state a power loss can leave in `dir` at each point of `ops`,
/// starting from `before`, and the states once `ops` are all done.
fn enumerate(before: &Disk, ops: &[DiskOp]) -> (BTreeSet<State>, BTreeSet<State>) {
    let mut disk = before.clone();
    let mut all = disk.crash_states();
    for op in ops {
        disk.apply(op);
        all.extend(disk.crash_states());
    }
    (all, disk.crash_states())
}

/// Replace every file of `dir` but the lock files with `state`.
fn materialize(dir: &Path, state: &State) {
    for entry in std::fs::read_dir(dir).expect("read dir") {
        let entry = entry.expect("entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_lock(&name) && entry.file_type().expect("type").is_file() {
            std::fs::remove_file(entry.path()).expect("remove");
        }
    }
    for (name, bytes) in state {
        std::fs::write(dir.join(name), bytes).expect("materialize");
    }
}

/// Record the durable operations `write` makes on this thread.
fn record(write: impl FnOnce()) -> Vec<DiskOp> {
    disk_log::start();
    write();
    let ops = disk_log::finish();
    assert!(!ops.is_empty(), "the write recorded nothing");
    ops
}

/// Every session file a state holds loads, rebuilds a provider-valid
/// request, and is listed; temporary files are never listed.
fn check_sessions(dir: &Path, cwd: &str, state: &State, case: &str) {
    let mut sessions: Vec<PathBuf> = Vec::new();
    for name in state.keys() {
        if name.starts_with('.') {
            assert!(name.ends_with(".tmp"), "{case}: {name}");
            continue;
        }
        let path = dir.join(name);
        let context = context_of(&path);
        assert_eq!(provider_problem(&request(&context)), None, "{case}: {name}");
        sessions.push(path);
    }
    let found = listed(cwd, dir);
    assert!(
        found.iter().all(|path| sessions.contains(path)),
        "{case}: listed {found:?} of {sessions:?}"
    );
}

/// After every state is checked, the directory is put back as the write
/// left it, for the next case.
fn for_each_state(dir: &Path, states: &BTreeSet<State>, mut check: impl FnMut(&State, &str)) {
    for (index, state) in states.iter().enumerate() {
        materialize(dir, state);
        check(state, &format!("state {index} of {}", states.len()));
    }
}

/// An append, from first write to sync, leaves the file as it was or with
/// the whole entry; the next writer takes either over. Once the append
/// returns, every state holds the entry.
#[test]
fn a_power_loss_during_an_append_keeps_the_file_or_the_whole_entry() {
    let dir = TempDir::new("power-append");
    let sessions = dir.join("sessions");
    let mut manager = file_session(&dir);
    let cwd = manager.cwd().to_owned();
    tool_turn(&mut manager);
    manager.append_message(tool_result("c1")).expect("first");
    let file = manager.session_file().expect("file").to_path_buf();
    let name = file
        .file_name()
        .expect("name")
        .to_string_lossy()
        .into_owned();
    let base = read(&file);
    let before = Disk::snapshot(&sessions);
    let ops = record(|| {
        manager.append_message(tool_result("c2")).expect("append");
    });
    drop(manager);
    let after = read(&file);
    let (states, done) = enumerate(&before, &ops);
    assert_eq!(done.len(), 1, "a returned append is durable: {done:?}");
    assert_eq!(
        done.first().and_then(|state| state.get(&name)),
        Some(&after)
    );
    assert_eq!(states.len(), 2, "{states:?}");
    for_each_state(&sessions, &states, |state, case| {
        let bytes = &state[&name];
        assert!(*bytes == base || *bytes == after, "{case}");
        check_sessions(&sessions, &cwd, state, case);
        let mut taken = take_over(&file);
        taken.append_session_info("after").expect("append after");
        drop(taken);
        let context = context_of(&file);
        assert_eq!(provider_problem(&request(&context)), None, "{case}");
    });
}

/// A group, line by line, leaves the file with a whole-line prefix of the
/// group, never a later line without an earlier one.
#[test]
fn a_power_loss_during_a_group_keeps_a_prefix_of_its_lines() {
    let dir = TempDir::new("power-group");
    let sessions = dir.join("sessions");
    let mut manager = file_session(&dir);
    let cwd = manager.cwd().to_owned();
    let ids = conversation(&mut manager);
    let file = manager.session_file().expect("file").to_path_buf();
    let name = file
        .file_name()
        .expect("name")
        .to_string_lossy()
        .into_owned();
    let base = read(&file);
    let before = Disk::snapshot(&sessions);
    let ops = record(|| {
        manager.append_group(omission(&ids)).expect("group");
    });
    drop(manager);
    let after = read(&file);
    let added = lines(&after[base.len()..]);
    assert_eq!(added.len(), 3);
    let prefixes: Vec<Vec<u8>> = (0..=added.len())
        .map(|kept| [&base[..], &added[..kept].concat()].concat())
        .collect();
    let (states, done) = enumerate(&before, &ops);
    assert_eq!(done.len(), 1, "a returned group is durable: {done:?}");
    assert_eq!(states.len(), prefixes.len(), "{states:?}");
    for_each_state(&sessions, &states, |state, case| {
        let bytes = &state[&name];
        let kept = prefixes
            .iter()
            .position(|prefix| prefix == bytes)
            .unwrap_or_else(|| panic!("{case}: not a prefix of the group"));
        check_sessions(&sessions, &cwd, state, case);
        let loaded = entries_of(&file).len();
        let mut taken = take_over(&file);
        assert_eq!(taken.entries().len(), loaded, "{case}: {kept} lines");
        taken.append_session_info("after").expect("append after");
    });
}

/// A new session's first write creates its file: a power loss leaves no
/// file, an empty one, or a whole-line prefix of the first write, each of
/// which loads, and once the write returns the whole file and its name.
#[test]
fn a_power_loss_during_a_first_write_leaves_no_file_or_a_prefix() {
    let dir = TempDir::new("power-first");
    let sessions = dir.join("sessions");
    std::fs::create_dir_all(&sessions).expect("sessions");
    let mut manager = file_session(&dir);
    let cwd = manager.cwd().to_owned();
    let before = Disk::snapshot(&sessions);
    let ops = record(|| {
        manager
            .append_message(message(
                json!({"role": "user", "content": "go", "timestamp": 1}),
            ))
            .expect("user");
    });
    let file = manager.session_file().expect("file").to_path_buf();
    let name = file
        .file_name()
        .expect("name")
        .to_string_lossy()
        .into_owned();
    drop(manager);
    let after = read(&file);
    let (states, done) = enumerate(&before, &ops);
    assert_eq!(done.len(), 1, "a returned first write is durable: {done:?}");
    for_each_state(&sessions, &states, |state, case| {
        if let Some(bytes) = state.get(&name) {
            assert!(after.starts_with(bytes), "{case}");
            assert!(bytes.is_empty() || bytes.ends_with(b"\n"), "{case}");
        }
        check_sessions(&sessions, &cwd, state, case);
        if state.contains_key(&name) {
            let mut taken = take_over(&file);
            taken.append_session_info("after").expect("append after");
        }
    });
}

/// A migration rewrites the file through a temporary one: every state
/// holds the old file or the whole new one, never a mix; a temporary file
/// is never listed; and the next writer migrates again or reads the new
/// file.
#[test]
fn a_power_loss_during_a_migration_keeps_the_old_or_the_new_file() {
    let dir = TempDir::new("power-migration");
    let sessions = dir.join("sessions");
    let (file, old) = v2_session(&sessions);
    let cwd = sessions.to_string_lossy().into_owned();
    let name = file
        .file_name()
        .expect("name")
        .to_string_lossy()
        .into_owned();
    let (new, context) = migrated(&old);
    let before = Disk::snapshot(&sessions);
    let ops = record(|| {
        drop(SessionManager::open(&file, None, None).expect("migrate"));
    });
    assert_eq!(read(&file), new);
    let (states, done) = enumerate(&before, &ops);
    assert_eq!(done.len(), 1, "a returned migration is durable: {done:?}");
    assert!(states.len() > 2, "{states:?}");
    for_each_state(&sessions, &states, |state, case| {
        let bytes = &state[&name];
        assert!(*bytes == old || *bytes == new, "{case}: a mixed file");
        for other in state.keys().filter(|other| **other != name) {
            assert!(is_temp_of(other, &name), "{case}: {other}");
        }
        check_sessions(&sessions, &cwd, state, case);
        assert_eq!(
            listed(&cwd, &sessions),
            std::slice::from_ref(&file),
            "{case}"
        );
        let taken = take_over(&file);
        assert_eq!(taken.build_session_context(), context, "{case}");
        drop(taken);
        assert_eq!(read(&file), new, "{case}");
    });
}

/// A branched session's file is written through a temporary file: every
/// state holds the source as it was and the branch absent or whole.
#[test]
fn a_power_loss_during_a_branch_rewrite_keeps_no_branch_or_a_whole_one() {
    let dir = TempDir::new("power-branch");
    let sessions = dir.join("sessions");
    let mut manager = file_session(&dir);
    let cwd = manager.cwd().to_owned();
    let ids = conversation(&mut manager);
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
    let old = read(&source);
    let before = Disk::snapshot(&sessions);
    let mut branch = PathBuf::new();
    let ops = record(|| {
        branch = manager
            .create_branched_session(&ids.results[1])
            .expect("branch")
            .expect("a branch file");
    });
    drop(manager);
    let branch_name = branch
        .file_name()
        .expect("name")
        .to_string_lossy()
        .into_owned();
    let whole = read(&branch);
    let (states, done) = enumerate(&before, &ops);
    assert_eq!(done.len(), 1, "a returned branch is durable: {done:?}");
    for_each_state(&sessions, &states, |state, case| {
        assert_eq!(state[&source_name], old, "{case}");
        if let Some(bytes) = state.get(&branch_name) {
            assert_eq!(*bytes, whole, "{case}: a partial branch");
        }
        for other in state.keys() {
            assert!(
                *other == source_name || *other == branch_name || is_temp_of(other, &branch_name),
                "{case}: {other}"
            );
        }
        check_sessions(&sessions, &cwd, state, case);
        let mut expected = vec![source.clone()];
        if state.contains_key(&branch_name) {
            expected.push(branch.clone());
            expected.sort();
            let mut taken = take_over(&branch);
            taken
                .append_session_info("after")
                .expect("append to branch");
        }
        assert_eq!(listed(&cwd, &sessions), expected, "{case}");
        let ids_again = Conversation::of(&take_over(&source));
        assert_eq!(ids_again.results, ids.results, "{case}");
    });
}

/// The in-place rewrite of a hard-linked file truncates it and writes it
/// again: a power loss before its sync can leave the file empty or with a
/// prefix of the new lines, losing the old contents. This pins the hazard
/// the module docs of `durable` name; the rewrite through a temporary file
/// has no such state.
#[test]
fn a_power_loss_during_an_in_place_rewrite_can_leave_a_truncated_file() {
    let dir = TempDir::new("power-in-place");
    let sessions = dir.join("sessions");
    let (file, old) = v2_session(&sessions);
    let (new, _) = migrated(&old);
    let outside = dir.join("outside-link.jsonl");
    std::fs::hard_link(&file, &outside).expect("hard link");
    let name = file
        .file_name()
        .expect("name")
        .to_string_lossy()
        .into_owned();
    let before = Disk::snapshot(&sessions);
    let ops = record(|| {
        drop(SessionManager::open(&file, None, None).expect("migrate in place"));
    });
    assert!(matches!(ops.first(), Some(DiskOp::Truncate(_))), "{ops:?}");
    assert_eq!(read(&outside), new, "the link sees the rewrite");
    let (states, done) = enumerate(&before, &ops);
    assert_eq!(done.len(), 1, "{done:?}");
    let contents: BTreeSet<&Vec<u8>> = states.iter().map(|state| &state[&name]).collect();
    assert!(contents.contains(&Vec::new()), "a truncated state");
    assert!(contents.contains(&old) && contents.contains(&new));
    for bytes in contents {
        assert!(*bytes == old || new.starts_with(bytes), "{bytes:?}");
        // Every such file still loads; a prefix of the new lines rebuilds
        // a provider-valid request.
        materialize(&sessions, &BTreeMap::from([(name.clone(), bytes.clone())]));
        let context = context_of(&file);
        assert_eq!(provider_problem(&request(&context)), None);
    }
}
