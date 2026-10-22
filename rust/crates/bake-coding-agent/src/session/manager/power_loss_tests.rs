//! Simulated power loss. This is not a physical power cut: no machine
//! loses power and no device drops a write cache. The durable writes log
//! every directory creation, write, file sync, file creation, truncation,
//! rename, removal, and directory sync they make (`durable::disk_log`,
//! compiled only into the crate's tests), and a model of the session
//! directory replays each prefix of that log and enumerates what a power
//! loss right after it can leave:
//!
//! - a file keeps the bytes it held at its last sync, or, of the bytes
//!   written since, any prefix, torn lines included, as a filesystem that
//!   writes back some blocks and not others can; up to [`EVERY_BYTE`]
//!   unsynced bytes are cut at every byte, and a longer run at each line
//!   boundary, each 4096-byte boundary of the file, and densely inside its
//!   first and last line ([`cuts`]). Each kept length may also hold zeros
//!   where the bytes were never written, as a filesystem that extended the
//!   file's size before its data (XFS can) leaves it: zeros from the start
//!   of the unsynced bytes, or from a later 4096-byte boundary. Bytes
//!   written after a truncation are cut the same way, so a truncated file
//!   may come back empty;
//! - names (creations, renames, removals) are kept as of the last sync of
//!   their directory, or all of them, as if the directory reached the
//!   device anyway;
//! - a directory the writer created is lost, with every file in it, until
//!   its parent is synced.
//!
//! Each state is written to the session directory and checked as a
//! restart would see it: every session file loads as Pi's loader loads it,
//! skipping torn and zero-filled lines, and rebuilds a provider-valid
//! request; one whose header survived is taken over by the next writer,
//! whose entry starts on a line of its own; an append or a group leaves a prefix of what it wrote, and
//! loads exactly the entries whose lines it kept whole; a rewrite through a
//! temporary file leaves the old or the new file, never a mix, and a
//! listing finds only whole sessions. Once a write returns, every state
//! holds all of it. A first write or an in-place rewrite torn inside its
//! header, or holding only zeros, leaves a file the next writer refuses to open as not a session
//! (`SessionError::NotASession`), unchanged, while a new session in the
//! directory still starts. Real kills inside a write and kernel errors are
//! tested elsewhere (`kernel_write_tests`, `seccomp_write_tests`,
//! `full_filesystem_tests`); this model cannot see a filesystem's own
//! reordering beyond the rules above, nor a device that loses or corrupts
//! synced data.
//!
//! Unix only: Windows has no directory sync through `std`, so no name a
//! write creates or renames there is durable in this model.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::*;
use crate::session::durable::disk_log::{self, DiskOp};
use crate::session::manager::crash_support::{
    Conversation, context_of, conversation, entries_of, file_session, is_temp_of, lines, listed,
    message, migrated, omission, provider_problem, read, request, take_over, tool_result,
    tool_turn, usage, v2_session,
};
use crate::test_support::TempDir;

/// The block size the model writes back by: a filesystem page.
const PAGE: usize = 4096;

/// Unsynced runs up to this many bytes are cut at every byte.
const EVERY_BYTE: usize = 4096;

/// Where the model cuts `tail`, unsynced bytes written after `base` bytes
/// of a file: at every byte when it is short, else at its start and end,
/// each line boundary, each [`PAGE`] boundary of the file, and inside its
/// first and last line at each of their first and last 64 bytes and every
/// 7th byte between.
fn cuts(base: usize, tail: &[u8]) -> BTreeSet<usize> {
    let len = tail.len();
    if len <= EVERY_BYTE {
        return (0..=len).collect();
    }
    let mut cuts = BTreeSet::from([0, len]);
    let ends: Vec<usize> = tail
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'\n')
        .map(|(at, _)| at + 1)
        .collect();
    cuts.extend(&ends);
    cuts.extend(page_offsets(base, len));
    let first = (0, ends.first().copied().unwrap_or(len));
    let last_start = ends
        .iter()
        .rev()
        .copied()
        .find(|end| *end < len)
        .unwrap_or(0);
    for (start, end) in [first, (last_start, len)] {
        cuts.extend(
            (start..=end).filter(|at| at - start < 64 || end - at < 64 || (at - start) % 7 == 0),
        );
    }
    cuts
}

/// The offsets into `len` unsynced bytes, after `base` bytes, at which the
/// file reaches a [`PAGE`] boundary.
fn page_offsets(base: usize, len: usize) -> impl Iterator<Item = usize> {
    let first = (PAGE - base % PAGE) % PAGE;
    (first..=len).step_by(PAGE)
}

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

    /// The contents a power loss can leave: the durable bytes, or the base
    /// with each cut of the tail, written or, from its start or a later
    /// page boundary, zero-filled.
    fn outcomes(&self) -> BTreeSet<Vec<u8>> {
        let mut out = BTreeSet::from([self.durable.clone()]);
        if !self.dirty {
            return out;
        }
        let pages: Vec<usize> = page_offsets(self.base.len(), self.tail.len()).collect();
        for cut in cuts(self.base.len(), &self.tail) {
            for written in std::iter::once(cut)
                .chain([0])
                .chain(pages.iter().copied().filter(|page| *page < cut))
            {
                let mut bytes = self.base.clone();
                bytes.extend_from_slice(&self.tail[..written]);
                bytes.resize(self.base.len() + cut, 0);
                out.insert(bytes);
            }
        }
        out
    }
}

/// A file name in the directory, and the files a power loss can leave.
type State = BTreeMap<String, Vec<u8>>;

/// What a power loss can leave: the directory's files, or `None` when the
/// directory itself is lost.
type Crash = Option<State>;

/// The session directory in the model.
#[derive(Debug, Clone)]
struct Disk {
    dir: PathBuf,
    /// Whether the directory exists.
    exists: bool,
    /// Directories created, the session directory or its ancestors, whose
    /// names no sync of their parent has made durable.
    unsynced_dirs: BTreeSet<PathBuf>,
    names: BTreeMap<String, usize>,
    durable_names: BTreeMap<String, usize>,
    inodes: Vec<Inode>,
}

/// Whether `name` is a writer's lock file, which the model leaves alone.
fn is_lock(name: &str) -> bool {
    name.ends_with(".lock")
}

/// `path` with its symbolic links resolved, though it may not exist yet:
/// its parent is resolved and its name kept.
fn resolved(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        let parent = path.parent().expect("parent").canonicalize().expect("dir");
        parent.join(path.file_name().expect("name"))
    })
}

impl Disk {
    /// The files in `dir`, all durable.
    fn snapshot(dir: &Path) -> Self {
        let mut disk = Self::absent(dir);
        disk.exists = true;
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

    /// `dir`, which does not exist yet, though its parent does.
    fn absent(dir: &Path) -> Self {
        Self {
            dir: resolved(dir),
            exists: false,
            unsynced_dirs: BTreeSet::new(),
            names: BTreeMap::new(),
            durable_names: BTreeMap::new(),
            inodes: Vec::new(),
        }
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
            DiskOp::CreateDir(path) => {
                let path = resolved(path);
                assert!(self.dir.starts_with(&path), "{op:?} is not on the way");
                if path == self.dir {
                    assert!(!self.exists, "{op:?} over an existing directory");
                    self.exists = true;
                }
                self.unsynced_dirs.insert(path);
            }
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
                assert!(self.dir.starts_with(&dir), "{op:?} is not on the way");
                if dir == self.dir {
                    self.durable_names = self.names.clone();
                }
                self.unsynced_dirs
                    .retain(|created| created.parent() != Some(dir.as_path()));
            }
        }
    }

    /// Every state a power loss now can leave.
    fn crash_states(&self) -> BTreeSet<Crash> {
        let mut states = BTreeSet::new();
        if !self.exists || !self.unsynced_dirs.is_empty() {
            states.insert(None);
        }
        if !self.exists {
            return states;
        }
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
            states.extend(partial.into_iter().map(Some));
        }
        states
    }
}

/// Every state a power loss can leave at each point of `ops`, starting
/// from `before`, and the states once `ops` are all done.
fn enumerate_crashes(before: &Disk, ops: &[DiskOp]) -> (BTreeSet<Crash>, BTreeSet<Crash>) {
    let mut disk = before.clone();
    let mut all = disk.crash_states();
    for op in ops {
        disk.apply(op);
        all.extend(disk.crash_states());
    }
    (all, disk.crash_states())
}

/// [`enumerate_crashes`] of writes that never lose the directory.
fn enumerate(before: &Disk, ops: &[DiskOp]) -> (BTreeSet<State>, BTreeSet<State>) {
    let (all, done) = enumerate_crashes(before, ops);
    let files = |crashes: BTreeSet<Crash>| -> BTreeSet<State> {
        crashes
            .into_iter()
            .map(|crash| crash.expect("the session directory can be lost"))
            .collect()
    };
    (files(all), files(done))
}

/// Replace every file of `dir` but the lock files with `state`, creating
/// `dir` when it is missing.
fn materialize(dir: &Path, state: &State) {
    std::fs::create_dir_all(dir).expect("directory");
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

/// How `bytes` follows from `base` and the bytes `added` after it: the
/// number of `added` bytes it kept, and its length past `base`, any bytes
/// past those kept being zeros; `None` when it is not such a file.
fn kept_of(bytes: &[u8], base: &[u8], added: &[u8]) -> Option<(usize, usize)> {
    let rest = bytes.strip_prefix(base)?;
    if rest.len() > added.len() {
        return None;
    }
    let kept = rest
        .iter()
        .zip(added)
        .take_while(|(left, right)| left == right)
        .count();
    rest[kept..]
        .iter()
        .all(|byte| *byte == 0)
        .then_some((kept, rest.len()))
}

/// The entries of the lines of `added` that Pi's loader reads from its
/// first `kept` bytes, followed by zeros up to `len`: each line kept with
/// its newline, and a last line whose JSON is whole when no zero follows.
fn whole_lines(added: &[u8], kept: usize, len: usize) -> usize {
    let mut end = 0;
    lines(added)
        .iter()
        .filter(|line| {
            end += line.len();
            end <= kept || (end - 1 == kept && kept == len)
        })
        .count()
}

/// The states every cut and every zero-filled cut of `added` after `base`
/// leaves, which the model must produce: `base` with each prefix of
/// `added`, and with zeros as long as each.
fn expected_cuts(base: &[u8], added: &[u8]) -> BTreeSet<Vec<u8>> {
    (0..=added.len())
        .flat_map(|cut| {
            let written = [base, &added[..cut]].concat();
            let zeros = [base, &vec![0; cut]].concat();
            [written, zeros]
        })
        .collect()
}

/// A state's file follows from `base` and the bytes `added` after it as
/// the model allows: a prefix of them, with zeros only from one of the
/// `synced` offsets into `added`, where a sync left the file, or from a
/// page boundary of the file. Returns the entries Pi's loader reads past
/// `base`'s.
fn check_cut(bytes: &[u8], base: &[u8], added: &[u8], synced: &[usize], case: &str) -> usize {
    let (kept, len) =
        kept_of(bytes, base, added).unwrap_or_else(|| panic!("{case}: not a cut of the write"));
    assert!(
        kept == len || synced.contains(&kept) || (base.len() + kept).is_multiple_of(PAGE),
        "{case}: data kept to {kept} before zeros"
    );
    whole_lines(added, kept, len)
}

/// The next writer takes over the file a state left holding `bytes`, a
/// session of the current version, and appends an entry on a line of its
/// own: the file keeps `bytes`, gains a newline when they end in a torn
/// line, then the entry's line, and loads with one more entry and a
/// provider-valid request.
fn takes_over(file: &Path, bytes: &[u8], case: &str) {
    assert!(!bytes.is_empty(), "{case}: an empty file gets a new header");
    let loaded = entries_of(file).len();
    let mut taken = take_over(file);
    assert_eq!(taken.entries().len(), loaded, "{case}");
    taken.append_session_info("after").expect("append after");
    drop(taken);
    let after = read(file);
    let rest = after
        .strip_prefix(bytes)
        .unwrap_or_else(|| panic!("{case}: the next writer changed what it found"));
    let rest = if bytes.ends_with(b"\n") {
        rest
    } else {
        rest.strip_prefix(b"\n")
            .unwrap_or_else(|| panic!("{case}: no newline after a torn line"))
    };
    assert_eq!(lines(rest).len(), 1, "{case}: {rest:?}");
    let info: Value = serde_json::from_slice(rest).expect("the next writer's line");
    assert_eq!(info["type"], "session_info", "{case}");
    assert_eq!(entries_of(file).len(), loaded + 1, "{case}");
    assert_eq!(
        provider_problem(&request(&context_of(file))),
        None,
        "{case}"
    );
}

/// An append, from first write to sync, leaves the file as it was, with
/// any prefix of the entry, or with zeros as long as one; each loads to
/// the entries it kept whole, and the next writer takes it over. Once the
/// append returns, every state holds the entry.
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
    let entries = entries_of(&file).len();
    let before = Disk::snapshot(&sessions);
    let ops = record(|| {
        manager.append_message(tool_result("c2")).expect("append");
    });
    drop(manager);
    let after = read(&file);
    let added = &after[base.len()..];
    let (states, done) = enumerate(&before, &ops);
    assert_eq!(done.len(), 1, "a returned append is durable: {done:?}");
    assert_eq!(
        done.first().and_then(|state| state.get(&name)),
        Some(&after)
    );
    let contents: BTreeSet<Vec<u8>> = states.iter().map(|state| state[&name].clone()).collect();
    assert!(contents.is_superset(&expected_cuts(&base, added)));
    for_each_state(&sessions, &states, |state, case| {
        let bytes = &state[&name];
        let kept = check_cut(bytes, &base, added, &[0], case);
        assert_eq!(entries_of(&file).len(), entries + kept, "{case}");
        check_sessions(&sessions, &cwd, state, case);
        takes_over(&file, bytes, case);
    });
}

/// A group, line by line, leaves the file with a prefix of the group,
/// whole lines and a torn one or zeros after them, never a later line
/// without an earlier one; it loads to the lines it kept whole.
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
    let entries = entries_of(&file).len();
    let before = Disk::snapshot(&sessions);
    let ops = record(|| {
        manager.append_group(omission(&ids)).expect("group");
    });
    drop(manager);
    let after = read(&file);
    let added = &after[base.len()..];
    assert_eq!(lines(added).len(), 3);
    let (states, done) = enumerate(&before, &ops);
    assert_eq!(done.len(), 1, "a returned group is durable: {done:?}");
    let contents: BTreeSet<Vec<u8>> = states.iter().map(|state| state[&name].clone()).collect();
    // Each line is synced before the next is written, so zeros follow
    // only the bytes of the line being written.
    let synced: Vec<usize> = lines(added)
        .iter()
        .scan(0, |end, line| {
            let start = *end;
            *end += line.len();
            Some(start)
        })
        .collect();
    for start in &synced {
        let line_base = [&base[..], &added[..*start]].concat();
        let line = lines(&added[*start..])[0];
        assert!(contents.is_superset(&expected_cuts(&line_base, line)));
    }
    for_each_state(&sessions, &states, |state, case| {
        let bytes = &state[&name];
        let kept = check_cut(bytes, &base, added, &synced, case);
        assert_eq!(entries_of(&file).len(), entries + kept, "{case}");
        check_sessions(&sessions, &cwd, state, case);
        takes_over(&file, bytes, case);
    });
}

/// A new session's first write creates its directory and its file. A power
/// loss leaves no directory, no file, an empty one, or a prefix of the
/// first write, torn or zero-filled. Once the header is whole the next
/// writer takes the file over; a torn or zero-filled header leaves a file
/// it refuses, unchanged, as not a session, which no listing finds, while
/// a new session still starts. Once the write returns, the directory and
/// the whole file are durable.
#[test]
fn a_power_loss_during_a_first_write_leaves_no_file_or_a_prefix() {
    let dir = TempDir::new("power-first");
    let sessions = dir.join("sessions");
    let before = Disk::absent(&sessions);
    let mut manager = None;
    let ops = record(|| {
        let mut created = file_session(&dir);
        created
            .append_message(message(
                json!({"role": "user", "content": "go", "timestamp": 1}),
            ))
            .expect("user");
        manager = Some(created);
    });
    let manager = manager.expect("manager");
    let cwd = manager.cwd().to_owned();
    let file = manager.session_file().expect("file").to_path_buf();
    let name = file
        .file_name()
        .expect("name")
        .to_string_lossy()
        .into_owned();
    drop(manager);
    let after = read(&file);
    let header = lines(&after)[0];
    let (states, done) = enumerate_crashes(&before, &ops);
    assert_eq!(done.len(), 1, "a returned first write is durable: {done:?}");
    assert_eq!(
        done.first()
            .and_then(Option::as_ref)
            .and_then(|state| state.get(&name)),
        Some(&after)
    );
    assert!(
        states.contains(&None),
        "the directory is lost before its sync"
    );
    let contents: BTreeSet<Vec<u8>> = states
        .iter()
        .flatten()
        .filter_map(|state| state.get(&name).cloned())
        .collect();
    assert!(contents.is_superset(&expected_cuts(&[], &after)));
    let (mut refused, mut taken) = (0, 0);
    for (index, crash) in states.iter().enumerate() {
        let case = format!("state {index} of {}", states.len());
        let Some(state) = crash else {
            if sessions.exists() {
                std::fs::remove_dir_all(&sessions).expect("lose the directory");
            }
            assert!(listed(&cwd, &sessions).is_empty(), "{case}");
            let mut fresh = file_session(&dir);
            fresh.append_session_info("fresh").expect("a new session");
            continue;
        };
        materialize(&sessions, state);
        check_sessions(&sessions, &cwd, state, &case);
        let Some(bytes) = state.get(&name) else {
            assert!(listed(&cwd, &sessions).is_empty(), "{case}");
            continue;
        };
        let lines_kept = check_cut(bytes, &[], &after, &[0], &case);
        if bytes.is_empty() {
            // An empty file gets a fresh header from its next writer.
            let mut opened = take_over(&file);
            opened.append_session_info("after").expect("append after");
            drop(opened);
            assert_eq!(entries_of(&file).len(), 1, "{case}");
            assert_eq!(lines(&read(&file)).len(), 2, "{case}");
        } else if lines_kept == 0 {
            let (kept, _) = kept_of(bytes, &[], &after).expect("a cut");
            assert!(kept + 1 < header.len() || bytes.len() > kept, "{case}");
            let loaded = crate::session::load_entries_from_file(&file).expect("load");
            assert!(loaded.is_empty(), "{case}");
            assert!(listed(&cwd, &sessions).is_empty(), "{case}");
            match SessionManager::open(&file, None, None) {
                Err(SessionError::NotASession(path)) => assert_eq!(path, file, "{case}"),
                other => panic!("{case}: a torn header opened: {:?}", other.err()),
            }
            assert_eq!(read(&file), *bytes, "{case}: a refused file changed");
            let mut fresh = file_session(&dir);
            fresh.append_session_info("fresh").expect("a new session");
            refused += 1;
        } else {
            assert_eq!(
                listed(&cwd, &sessions),
                std::slice::from_ref(&file),
                "{case}"
            );
            takes_over(&file, bytes, &case);
            taken += 1;
        }
    }
    assert!(refused > 0 && taken > 0, "{refused} refused, {taken} taken");
}

/// The parent sync of a new session directory is what makes it durable:
/// replayed without it, the model loses the directory, and with it the
/// returned first write.
#[test]
fn a_new_session_directory_without_its_parent_sync_can_be_lost() {
    let dir = TempDir::new("power-first-dir");
    let sessions = dir.join("sessions");
    let before = Disk::absent(&sessions);
    let ops = record(|| {
        let mut manager = file_session(&dir);
        manager
            .append_message(message(
                json!({"role": "user", "content": "go", "timestamp": 1}),
            ))
            .expect("user");
    });
    let parent = dir.path().canonicalize().expect("parent");
    let unsynced: Vec<DiskOp> = ops
        .iter()
        .filter(|op| !matches!(op, DiskOp::DirSync(synced) if synced.canonicalize().ok().as_ref() == Some(&parent)))
        .cloned()
        .collect();
    assert_eq!(unsynced.len() + 1, ops.len(), "one parent sync: {ops:?}");
    let (_, done) = enumerate_crashes(&before, &ops);
    assert!(!done.contains(&None), "{done:?}");
    let (_, done) = enumerate_crashes(&before, &unsynced);
    assert!(done.contains(&None), "{done:?}");
}

/// The model cuts a long unsynced run at its line ends, its page
/// boundaries, and densely inside its first and last line, and zero-fills
/// each kept length from the run's start or a later page boundary.
#[test]
fn long_unsynced_runs_are_cut_at_lines_pages_and_inside_their_ends() {
    let line = |byte: u8, len: usize| [vec![byte; len - 1], vec![b'\n']].concat();
    let tail = [line(b'a', 3000), line(b'b', 5000), line(b'c', 2000)].concat();
    let base = 100;
    let cut = cuts(base, &tail);
    for at in [0, 1, 63, 2937, 2999, 3000, 8000, 9001, 9999, 10_000] {
        assert!(cut.contains(&at), "{at}");
    }
    for page in [PAGE - base, 2 * PAGE - base] {
        assert!(cut.contains(&page), "{page}");
    }
    assert!(cut.contains(&700) && !cut.contains(&701), "every 7th byte");
    assert!(
        !cut.contains(&5000),
        "the middle line is cut at lines and pages"
    );
    let inode = Inode {
        durable: vec![b'x'; base],
        base: vec![b'x'; base],
        tail: tail.clone(),
        dirty: true,
    };
    let outcomes = inode.outcomes();
    let zeros_from = |written: usize, len: usize| {
        let mut bytes = [&inode.base[..], &tail[..written]].concat();
        bytes.resize(base + len, 0);
        bytes
    };
    assert!(outcomes.contains(&zeros_from(9001, 9001)));
    assert!(outcomes.contains(&zeros_from(0, 9001)));
    assert!(outcomes.contains(&zeros_from(PAGE - base, 9001)));
    assert!(
        !outcomes.contains(&zeros_from(3000, 9001)),
        "zeros from a line end"
    );
}

/// A migration rewrites the file through a temporary one: every state
/// holds the old file or the whole new one, never a mix; a temporary file
/// is never listed; and the next writer migrates again or reads the new
/// file, then appends on a line of its own.
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
        takes_over(&file, &new, case);
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
        }
        assert_eq!(listed(&cwd, &sessions), expected, "{case}");
        if state.contains_key(&branch_name) {
            takes_over(&branch, &whole, case);
        }
        let ids_again = Conversation::of(&take_over(&source));
        assert_eq!(ids_again.results, ids.results, "{case}");
    });
}

/// The in-place rewrite of a hard-linked file truncates it and writes it
/// again: a power loss before its sync can leave the file empty, with a
/// prefix of the new lines, or with zeros, losing the old contents. This
/// pins the hazard the module docs of `durable` name; the rewrite through
/// a temporary file has no such state. A torn or zero-filled header leaves
/// a file the next writer refuses as not a session.
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
    let cut: BTreeSet<&Vec<u8>> = contents
        .iter()
        .copied()
        .filter(|bytes| **bytes != old)
        .collect();
    assert_eq!(
        cut,
        expected_cuts(&[], &new).iter().collect::<BTreeSet<_>>(),
        "every cut of the new contents, written or zero-filled"
    );
    let (mut refused, mut taken) = (0, 0);
    for bytes in contents {
        materialize(&sessions, &BTreeMap::from([(name.clone(), bytes.clone())]));
        // Every such file still loads; a prefix of the new lines rebuilds
        // a provider-valid request.
        let context = context_of(&file);
        assert_eq!(provider_problem(&request(&context)), None);
        if *bytes == old || bytes.is_empty() {
            continue;
        }
        let lines_kept = check_cut(bytes, &[], &new, &[0], "in place");
        if lines_kept == 0 {
            assert!(matches!(
                SessionManager::open(&file, None, None),
                Err(SessionError::NotASession(_))
            ));
            assert_eq!(read(&file), *bytes, "a refused file changed");
            refused += 1;
        } else {
            takes_over(&file, bytes, "in place");
            taken += 1;
        }
    }
    assert!(refused > 0 && taken > 0, "{refused} refused, {taken} taken");
}
