//! Path discovery for `@` mentions, rooted at the working directory: a port
//! of `WorkspaceFileSearch` in `bake-file-reference-local`. It lists paths
//! only and never reads a file.
//!
//! A query naming a directory, or an empty one, lists that directory as it
//! is now. A bare name is ranked against one bounded traversal of the tree,
//! built by the first such query. Once [`Search::invalidate`] marks it
//! stale, it keeps answering until [`Search::refresh`] replaces it, so a
//! rebuild never stands between a key and its menu.
//!
//! [`Finder`] runs the search on its own thread and keeps only the newest
//! query waiting.

use std::cmp::Ordering;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use bake_tui_view::mention::{Candidate, PathKind};

/// Candidates listed for one query: the oracle's `maxResults` default.
pub const MAX_RESULTS: usize = 20;
/// Paths one traversal keeps: the oracle's `maxEntries` default.
pub const MAX_ENTRIES: usize = 50_000;
/// Directories never traversed or offered: version-control and dependency
/// stores and build output, as the oracle's defaults name them. `lib` is not
/// one, since many projects keep their sources there.
pub const EXCLUDED: &[&str] = &[
    ".git",
    "node_modules",
    "dist",
    "build",
    "out",
    "coverage",
    "target",
    ".next",
    ".nuxt",
    ".turbo",
    ".venv",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".gradle",
];

/// One workspace's search and its traversal.
pub struct Search {
    root: PathBuf,
    index: Option<Vec<Candidate>>,
    stale: bool,
    max_entries: usize,
}

impl Search {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            index: None,
            stale: false,
            max_entries: MAX_ENTRIES,
        }
    }

    /// The ranked candidates for the text after `@`, at most
    /// [`MAX_RESULTS`]. Only a failure to read the root on the first
    /// traversal is an error; an unreadable directory lists nothing.
    pub fn list(&mut self, query: &str, stop: &AtomicBool) -> io::Result<Vec<Candidate>> {
        let query = query.replace('\\', "/");
        if let Some(slash) = query.rfind('/') {
            return Ok(self.list_directory(&query[..=slash], &query[slash + 1..]));
        }
        if query.is_empty() {
            return Ok(self.list_directory("", ""));
        }
        if self.index.is_none() {
            self.index = Some(scan(&self.root, self.max_entries, stop)?);
        }
        let index = self.index.as_deref().unwrap_or_default();
        let visible = index.iter().filter(|c| visible_for(&c.path, &query));
        Ok(rank(visible, &query))
    }

    /// Marks the traversal stale; it answers until the next refresh.
    pub fn invalidate(&mut self) {
        self.stale = self.index.is_some();
    }

    /// Replaces a stale traversal. A failed one keeps the old entries and
    /// stays stale, to be tried again.
    pub fn refresh(&mut self, stop: &AtomicBool) {
        if !self.stale {
            return;
        }
        if let Ok(index) = scan(&self.root, self.max_entries, stop) {
            self.index = Some(index);
            self.stale = false;
        }
    }

    /// The live entries of `directory`, relative to the root and ending in
    /// `/` unless empty, ranked against `fragment`. Hidden entries show only
    /// for a fragment that starts with `.`. A directory outside the root,
    /// through a symbolic link, or excluded lists nothing.
    fn list_directory(&self, directory: &str, fragment: &str) -> Vec<Candidate> {
        if directory
            .split('/')
            .any(|segment| EXCLUDED.contains(&segment))
        {
            return Vec::new();
        }
        let Some(absolute) = resolve(&self.root, directory) else {
            return Vec::new();
        };
        let found: Vec<Candidate> = read_sorted(&absolute)
            .unwrap_or_default()
            .into_iter()
            .filter(|(name, _)| !name.starts_with('.') || fragment.starts_with('.'))
            .map(|(name, kind)| Candidate {
                path: format!("{directory}{name}"),
                kind,
            })
            .collect();
        rank(found.iter(), fragment)
    }
}

/// The directory a query names inside the root, each step a real directory
/// and not a symbolic link; `None` for one outside the root or missing.
fn resolve(root: &Path, directory: &str) -> Option<PathBuf> {
    let mut current = root.to_path_buf();
    for component in Path::new(directory).components() {
        match component {
            Component::Normal(segment) => {
                current.push(segment);
                let status = fs::symlink_metadata(&current).ok()?;
                if !status.is_dir() {
                    return None;
                }
            }
            Component::CurDir => {}
            // `..`, an absolute path, or a drive leaves the root.
            _ => return None,
        }
    }
    Some(current)
}

/// A directory's files and directories by name, in byte order. Symbolic
/// links and names that are not UTF-8 are left out.
fn read_sorted(directory: &Path) -> io::Result<Vec<(String, PathKind)>> {
    let mut entries: Vec<(String, PathKind)> = fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let kind = entry.file_type().ok()?;
            let kind = if kind.is_dir() {
                PathKind::Directory
            } else if kind.is_file() {
                PathKind::File
            } else {
                return None;
            };
            let name = entry.file_name().into_string().ok()?;
            (kind == PathKind::File || !EXCLUDED.contains(&name.as_str())).then_some((name, kind))
        })
        .collect();
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));
    Ok(entries)
}

/// Every path under the root, breadth first, up to `max_entries`. An
/// unreadable root fails the traversal; an unreadable subtree only loses
/// its own paths.
fn scan(root: &Path, max_entries: usize, stop: &AtomicBool) -> io::Result<Vec<Candidate>> {
    let mut found = Vec::new();
    let mut queue = vec![(root.to_path_buf(), String::new())];
    let mut cursor = 0;
    while cursor < queue.len() && found.len() < max_entries {
        if stop.load(AtomicOrdering::Relaxed) {
            return Err(io::Error::other("file search stopped"));
        }
        let (absolute, relative) = queue[cursor].clone();
        let entries = if cursor == 0 {
            read_sorted(&absolute)?
        } else {
            read_sorted(&absolute).unwrap_or_default()
        };
        cursor += 1;
        for (name, kind) in entries {
            let path = if relative.is_empty() {
                name.clone()
            } else {
                format!("{relative}/{name}")
            };
            if kind == PathKind::Directory {
                queue.push((absolute.join(&name), path.clone()));
            }
            found.push(Candidate { path, kind });
            if found.len() >= max_entries {
                break;
            }
        }
    }
    Ok(found)
}

/// Whether a bare query may offer a path: one under a hidden directory, or
/// hidden itself, only when the query starts with `.` or reaches into one.
fn visible_for(path: &str, query: &str) -> bool {
    query.starts_with('.')
        || query.contains("/.")
        || !path.split('/').any(|segment| segment.starts_with('.'))
}

/// The best [`MAX_RESULTS`] candidates for `query`: by score, directories
/// before files, shorter paths first for a non-empty query, then by path.
fn rank<'a>(candidates: impl Iterator<Item = &'a Candidate>, query: &str) -> Vec<Candidate> {
    let mut ranked: Vec<(u32, &Candidate)> = candidates
        .filter_map(|candidate| score(candidate, query).map(|score| (score, candidate)))
        .collect();
    let length = |c: &Candidate| c.path.encode_utf16().count();
    ranked.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then(kind_rank(left.kind).cmp(&kind_rank(right.kind)))
            .then(if query.is_empty() {
                Ordering::Equal
            } else {
                length(left).cmp(&length(right))
            })
            .then(left.path.cmp(&right.path))
    });
    ranked
        .into_iter()
        .take(MAX_RESULTS)
        .map(|(_, candidate)| candidate.clone())
        .collect()
}

fn kind_rank(kind: PathKind) -> u8 {
    match kind {
        PathKind::Directory => 0,
        PathKind::File => 1,
    }
}

/// The oracle's score: the name equal to, starting with, or holding the
/// query, then the path holding it, then its letters in order, closer
/// together scoring higher; a directory a little ahead of a file.
fn score(candidate: &Candidate, query: &str) -> Option<u32> {
    if query.is_empty() {
        return Some(0);
    }
    let path = candidate.path.to_lowercase();
    let name = path.rsplit('/').next().unwrap_or(&path);
    let needle = query.to_lowercase();
    let bonus = if candidate.kind == PathKind::Directory {
        25
    } else {
        0
    };
    let base = if name == needle {
        1_000
    } else if name.starts_with(&needle) {
        900
    } else if name.contains(&needle) {
        700
    } else if path.contains(&needle) {
        500
    } else {
        300 + subsequence(&path, &needle)?
    };
    Some(base + bonus)
}

/// 100 less the characters skipped to find `query`'s in order in `target`,
/// at least 0; `None` when they are not all there.
fn subsequence(target: &str, query: &str) -> Option<u32> {
    let target: Vec<char> = target.chars().collect();
    let mut at = 0;
    let mut gap = 0;
    for wanted in query.chars() {
        let found = at + target[at..].iter().position(|&c| c == wanted)?;
        gap += found - at;
        at = found + 1;
    }
    Some(100u32.saturating_sub(u32::try_from(gap).unwrap_or(u32::MAX)))
}

/// What the finder's thread is asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    /// The paths for this query.
    Find(String),
    /// Mark the traversal stale, to be rebuilt after the next answer.
    Invalidate,
}

/// Runs [`Search`] on its own thread. Of the queries that wait while it
/// works, only the newest is answered. Stopping or dropping it joins the
/// thread; a traversal in progress gives up at its next directory.
pub struct Finder {
    requests: Option<Sender<Ask>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Finder {
    /// Starts the thread over `root`. `send` delivers each answer and
    /// returns `false` once the loop has gone, which ends the thread.
    pub fn start(
        root: PathBuf,
        send: impl Fn(String, Result<Vec<Candidate>, String>) -> bool + Send + 'static,
    ) -> io::Result<Self> {
        let (requests, inbox) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("bake-tui-files".into())
            .spawn(move || serve(Search::new(root), &inbox, &flag, send))?;
        Ok(Self {
            requests: Some(requests),
            stop,
            thread: Some(thread),
        })
    }

    /// Passes a request on. A query supersedes any still waiting.
    pub fn ask(&self, ask: Ask) {
        if let Some(requests) = &self.requests {
            let _ = requests.send(ask);
        }
    }

    /// Ends the thread and waits for it; reports a panic in it.
    pub fn stop(mut self) -> io::Result<()> {
        self.join()
    }

    fn join(&mut self) -> io::Result<()> {
        self.stop.store(true, AtomicOrdering::Relaxed);
        // Closing the channel wakes the thread, which then returns.
        self.requests = None;
        match self.thread.take().map(JoinHandle::join) {
            Some(Err(_)) => Err(io::Error::other("file search thread panicked")),
            _ => Ok(()),
        }
    }
}

impl Drop for Finder {
    fn drop(&mut self) {
        let _ = self.join();
    }
}

/// The thread's loop: takes every waiting request, answers the newest
/// query, then refreshes a stale traversal before waiting again.
fn serve(
    mut search: Search,
    inbox: &Receiver<Ask>,
    stop: &AtomicBool,
    send: impl Fn(String, Result<Vec<Candidate>, String>) -> bool,
) {
    while let Ok(first) = inbox.recv() {
        let mut query = None;
        let mut next = Some(first);
        while let Some(request) = next {
            match request {
                Ask::Find(text) => query = Some(text),
                Ask::Invalidate => search.invalidate(),
            }
            next = match inbox.try_recv() {
                Ok(request) => Some(request),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => return,
            };
        }
        if let Some(query) = query {
            let found = search.list(&query, stop).map_err(|err| err.to_string());
            if stop.load(AtomicOrdering::Relaxed) || !send(query, found) {
                return;
            }
        }
        search.refresh(stop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::mpsc;
    use std::time::Duration;

    struct Tree(PathBuf);

    impl Tree {
        fn new(name: &str, files: &[&str]) -> Self {
            let root =
                std::env::temp_dir().join(format!("bake-files-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            for file in files {
                let path = root.join(file);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                if file.ends_with('/') {
                    fs::create_dir_all(&path).unwrap();
                } else {
                    fs::write(&path, "").unwrap();
                }
            }
            Self(root)
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn paths(found: io::Result<Vec<Candidate>>) -> Vec<String> {
        found
            .unwrap()
            .into_iter()
            .map(|c| match c.kind {
                PathKind::Directory => format!("{}/", c.path),
                PathKind::File => c.path,
            })
            .collect()
    }

    static GO: AtomicBool = AtomicBool::new(false);

    #[test]
    fn a_directory_query_lists_it_live_hiding_dotfiles_and_excluded_names() {
        let tree = Tree::new(
            "dir",
            &[
                "src/main.rs",
                "src/lib.rs",
                "src/ui/",
                ".env",
                "README.md",
                "target/x",
                "node_modules/y",
            ],
        );
        let mut search = Search::new(tree.0.clone());
        assert_eq!(paths(search.list("", &GO)), ["src/", "README.md"]);
        // A hidden name shows once the query starts with a dot.
        assert_eq!(paths(search.list(".e", &GO)), [".env"]);
        assert_eq!(
            paths(search.list("src/", &GO)),
            ["src/ui/", "src/lib.rs", "src/main.rs"]
        );
        assert_eq!(paths(search.list("src/ma", &GO)), ["src/main.rs"]);
        assert_eq!(paths(search.list("src\\l", &GO)), ["src/lib.rs"]);
        // Outside the root, missing, excluded, or a file: nothing.
        assert!(paths(search.list("../", &GO)).is_empty());
        assert!(paths(search.list("nope/", &GO)).is_empty());
        assert!(paths(search.list("target/", &GO)).is_empty());
        assert!(paths(search.list("README.md/", &GO)).is_empty());
    }

    #[test]
    fn a_bare_query_ranks_the_tree_as_the_oracle_does() {
        let tree = Tree::new(
            "rank",
            &[
                "app/main.ts",
                "app/domain.ts",
                "main/",
                "docs/maintain.md",
                "x/m/a/i/n.txt",
                ".hidden/main.ts",
                "dist/main.js",
            ],
        );
        let mut search = Search::new(tree.0.clone());
        // An exact directory name, an exact-prefix file, a name holding it,
        // a path holding it, then letters in order.
        assert_eq!(
            paths(search.list("main", &GO)),
            [
                "main/",
                "app/main.ts",
                "docs/maintain.md",
                "app/domain.ts",
                "x/m/a/i/n.txt"
            ]
        );
        // Hidden paths only when asked for; excluded trees never.
        assert_eq!(
            paths(search.list(".hid", &GO)),
            [".hidden/", ".hidden/main.ts"]
        );
        assert!(
            !paths(search.list("main", &GO))
                .iter()
                .any(|p| p.starts_with("dist"))
        );
    }

    #[test]
    fn a_stale_traversal_answers_until_it_is_refreshed() {
        let tree = Tree::new("stale", &["one.md"]);
        let mut search = Search::new(tree.0.clone());
        assert_eq!(paths(search.list("md", &GO)), ["one.md"]);
        fs::write(tree.0.join("two.md"), "").unwrap();
        // Without an invalidation the traversal stands; directories are live.
        assert_eq!(paths(search.list("md", &GO)), ["one.md"]);
        assert_eq!(paths(search.list("", &GO)), ["one.md", "two.md"]);
        search.invalidate();
        assert_eq!(paths(search.list("md", &GO)), ["one.md"]);
        search.refresh(&GO);
        assert_eq!(paths(search.list("md", &GO)), ["one.md", "two.md"]);
    }

    #[test]
    fn an_unreadable_root_is_an_error_and_the_limit_bounds_the_traversal() {
        let mut gone = Search::new(std::env::temp_dir().join("bake-files-missing-root"));
        assert!(gone.list("x", &GO).is_err());
        let tree = Tree::new("limit", &["a/b/c.md", "d.md"]);
        let mut search = Search::new(tree.0.clone());
        search.max_entries = 2;
        assert_eq!(paths(search.list("d", &GO)), ["d.md"]);
        // `a/b/c.md` lies past the two paths kept.
        assert!(paths(search.list("c", &GO)).is_empty());
    }

    #[test]
    fn the_finder_answers_the_newest_query_and_stop_joins_it() {
        let tree = Tree::new("finder", &["alpha.md", "beta.md"]);
        let (sender, answers) = mpsc::channel();
        let finder = Finder::start(tree.0.clone(), move |query, found| {
            sender.send((query, found)).is_ok()
        })
        .unwrap();
        finder.ask(Ask::Find("alp".into()));
        let (query, found) = answers.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(query, "alp");
        assert_eq!(paths(found.map_err(io::Error::other)), ["alpha.md"]);
        finder.ask(Ask::Invalidate);
        finder.ask(Ask::Find("be".into()));
        let (query, _) = answers.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(query, "be");
        finder.stop().unwrap();
    }
}
