//! Where telecom members find each other: one directory per team, and
//! three files per member in it.
//!
//! ```text
//! <root>/<team>/<callsign>.lock   held with an exclusive lock for the member's life
//! <root>/<team>/<callsign>.json   its presence record, replaced by rename
//! <root>/<team>/<callsign>.sock   the Unix socket its frames arrive on
//! ```
//!
//! The root is `BAKE_TELECOM_DIR`, else `telecom` in the Bake home, created
//! with mode `0700` and refused when another user owns it. A team is the
//! checkouts that share one git common directory (a repository and its
//! worktrees), else one directory outside git; its directory is named by
//! the first 12 hex digits of the SHA-256 of that directory's canonical
//! path.
//!
//! **The lock is the membership.** Taking a callsign is creating its lock
//! file and locking it exclusively without waiting ([`std::fs::File::try_lock`]):
//! only one process can hold it, so two members never share a callsign and
//! no later check is needed. The kernel releases the lock when its holder
//! exits however it exits, so a reader that can take a member's lock knows
//! that member is gone, removes its record and socket, and unlinks the
//! lock file while holding it. A taker therefore checks, after locking,
//! that the path still names the file it locked, and starts again if a
//! cleaner unlinked it in between. Nothing depends on pids, start times,
//! or heartbeats.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::wire::{self, Presence};

/// The variable that places the telecom root.
pub const DIR_ENV: &str = "BAKE_TELECOM_DIR";

/// The callsigns a top-level member takes, in order.
pub const NATO: [&str; 26] = [
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet",
    "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra", "tango",
    "uniform", "victor", "whiskey", "xray", "yankee", "zulu",
];

/// The longest socket path the platform's `sockaddr_un` holds, with room
/// for its terminator on the smallest of them (macOS).
const SOCKET_PATH_CAP: usize = 103;

/// The telecom root: `BAKE_TELECOM_DIR`, else `telecom` in `home`.
pub fn root(home: Option<&Path>) -> Option<PathBuf> {
    std::env::var_os(DIR_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|home| home.join("telecom")))
}

/// The directory a team is known by for a member working in `cwd`: the git
/// common directory of the checkout containing it, else `cwd` itself, and
/// the checkout (the nearest ancestor with `.git`, else `cwd`).
pub fn locate(cwd: &Path) -> (PathBuf, PathBuf) {
    let cwd = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    for checkout in cwd.ancestors() {
        let dot_git = checkout.join(".git");
        let Ok(meta) = fs::metadata(&dot_git) else {
            continue;
        };
        let common = if meta.is_dir() {
            dot_git
        } else {
            git_file_common_dir(&dot_git, checkout).unwrap_or(dot_git)
        };
        let common = fs::canonicalize(&common).unwrap_or(common);
        return (common, checkout.to_path_buf());
    }
    (cwd.clone(), cwd)
}

/// A worktree's `.git` file names its git directory (`gitdir: <path>`),
/// whose `commondir` names the shared one.
fn git_file_common_dir(dot_git: &Path, checkout: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(dot_git).ok()?;
    let gitdir = text
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))?
        .trim();
    let gitdir = checkout.join(gitdir);
    match fs::read_to_string(gitdir.join("commondir")) {
        Ok(common) => Some(gitdir.join(common.trim())),
        Err(_) => Some(gitdir),
    }
}

/// The branch `checkout` has checked out, from its `HEAD`.
pub fn branch(checkout: &Path) -> Option<String> {
    let dot_git = checkout.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        let text = fs::read_to_string(&dot_git).ok()?;
        checkout.join(
            text.lines()
                .find_map(|line| line.strip_prefix("gitdir:"))?
                .trim(),
        )
    };
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    Some(match head.strip_prefix("ref: refs/heads/") {
        Some(branch) => branch.to_owned(),
        None => head.get(..8)?.to_owned(),
    })
}

/// The name of the team directory for the common directory `common`.
pub fn team_key(common: &Path) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, common.as_os_str().as_encoded_bytes());
    digest.as_ref()[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// One team's directory.
#[derive(Debug, Clone)]
pub struct Team {
    dir: PathBuf,
}

/// A held callsign: its lock and its files. Dropping it removes the record
/// and socket, then unlinks the lock while still holding it.
#[derive(Debug)]
pub struct Membership {
    team: Team,
    callsign: String,
    lock: File,
}

impl Team {
    /// Opens (creating) the team `key` under `root`, both private to this
    /// user.
    pub fn open(root: &Path, key: &str) -> io::Result<Self> {
        private_dir(root)?;
        let dir = root.join(key);
        private_dir(&dir)?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file(&self, callsign: &str, extension: &str) -> PathBuf {
        self.dir.join(format!("{callsign}.{extension}"))
    }

    /// The socket a member's frames arrive on.
    pub fn socket(&self, callsign: &str) -> PathBuf {
        self.file(callsign, "sock")
    }

    /// Takes the first free callsign of `candidates`. Returns `None` when
    /// every one is held.
    pub fn claim<'a>(
        &self,
        candidates: impl IntoIterator<Item = &'a str>,
    ) -> io::Result<Option<Membership>> {
        for callsign in candidates {
            if !wire::is_callsign(callsign) {
                continue;
            }
            if self.socket(callsign).as_os_str().len() > SOCKET_PATH_CAP {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "the telecom directory {} is too deep for a socket path; set {DIR_ENV} to a shorter one",
                        self.dir.display()
                    ),
                ));
            }
            if let Some(lock) = self.try_hold(callsign)? {
                return Ok(Some(Membership {
                    team: self.clone(),
                    callsign: callsign.to_owned(),
                    lock,
                }));
            }
        }
        Ok(None)
    }

    /// The lock of `callsign`, held, or `None` when another process holds
    /// it.
    fn try_hold(&self, callsign: &str) -> io::Result<Option<File>> {
        let path = self.file(callsign, "lock");
        // A cleaner may unlink the file between our open and our lock; the
        // lock then guards nothing, so start again on the new file.
        for _ in 0..8 {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)?;
            match file.try_lock() {
                Ok(()) => {}
                Err(fs::TryLockError::WouldBlock) => return Ok(None),
                Err(fs::TryLockError::Error(error)) => return Err(error),
            }
            if same_file(&file, &path) {
                return Ok(Some(file));
            }
        }
        Ok(None)
    }

    /// The live members other than `except`, by callsign. A member whose
    /// lock is free is gone: its files are removed. A live member whose
    /// record is missing or invalid (one starting, or one mid-rename) is
    /// left out.
    pub fn members(&self, except: Option<&str>) -> Vec<Presence> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut callsigns: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let callsign = name.strip_suffix(".lock")?;
                wire::is_callsign(callsign).then(|| callsign.to_owned())
            })
            .filter(|callsign| Some(callsign.as_str()) != except)
            .collect();
        callsigns.sort();
        callsigns
            .into_iter()
            .filter(|callsign| self.is_live(callsign))
            .filter_map(|callsign| {
                let bytes = fs::read(self.file(&callsign, "json")).ok()?;
                Presence::decode(&bytes, &callsign)
            })
            .collect()
    }

    /// Whether `callsign`'s lock is held. A free lock means its member is
    /// gone; its files are removed under that lock.
    fn is_live(&self, callsign: &str) -> bool {
        let path = self.file(callsign, "lock");
        let Ok(file) = OpenOptions::new().read(true).write(true).open(&path) else {
            return false;
        };
        match file.try_lock() {
            Err(fs::TryLockError::WouldBlock) => true,
            Err(fs::TryLockError::Error(_)) => true,
            Ok(()) => {
                if same_file(&file, &path) {
                    let _ = fs::remove_file(self.file(callsign, "json"));
                    let _ = fs::remove_file(self.socket(callsign));
                    let _ = fs::remove_file(&path);
                }
                false
            }
        }
    }
}

impl Membership {
    pub fn callsign(&self) -> &str {
        &self.callsign
    }

    pub fn team(&self) -> &Team {
        &self.team
    }

    /// Publishes `record`: written beside its place and renamed over it,
    /// so a reader sees the old record or the new one whole.
    pub fn publish(&self, record: &Presence) -> io::Result<()> {
        let path = self.team.file(&self.callsign, "json");
        let temp = self
            .team
            .dir
            .join(format!(".{}.json.{}", self.callsign, std::process::id()));
        let mut file = private_file(&temp)?;
        file.write_all(&serde_json::to_vec(record).map_err(io::Error::other)?)?;
        file.sync_data()?;
        drop(file);
        fs::rename(&temp, &path).inspect_err(|_| {
            let _ = fs::remove_file(&temp);
        })
    }

    /// Clears a stale socket file before the member binds its own: safe,
    /// since only the lock's holder binds there.
    pub fn clear_socket(&self) {
        let _ = fs::remove_file(self.team.socket(&self.callsign));
    }

    #[cfg(test)]
    pub(crate) fn lock_file(&self) -> &File {
        &self.lock
    }
}

impl Drop for Membership {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.team.file(&self.callsign, "json"));
        let _ = fs::remove_file(self.team.socket(&self.callsign));
        let _ = fs::remove_file(self.team.file(&self.callsign, "lock"));
        let _ = self.lock.unlock();
    }
}

#[cfg(unix)]
fn same_file(file: &File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (file.metadata(), fs::metadata(path)) {
        (Ok(held), Ok(named)) => held.dev() == named.dev() && held.ino() == named.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_file(_file: &File, path: &Path) -> bool {
    // Windows does not unlink an open file, so the name cannot move.
    path.exists()
}

#[cfg(unix)]
fn private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let meta = fs::symlink_metadata(dir)?;
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    if !meta.is_dir() || meta.uid() != uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a directory owned by this user", dir.display()),
        ));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

#[cfg(unix)]
fn private_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn private_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telecom::wire::{Activity, WIRE_VERSION};

    fn record(callsign: &str) -> Presence {
        Presence {
            v: WIRE_VERSION,
            session: format!("s-{callsign}"),
            callsign: callsign.into(),
            pid: std::process::id(),
            checkout: "/repo".into(),
            branch: None,
            task: None,
            activity: Activity::Idle,
            since: 0,
            tool: None,
            parent: None,
            model: None,
            started_at: 0,
        }
    }

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bt-{}", crate::session::id::uuid_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn callsigns_are_held_once_and_freed_by_their_holder() {
        let root = temp();
        let team = Team::open(&root, "t").unwrap();
        let alpha = team.claim(NATO).unwrap().unwrap();
        assert_eq!(alpha.callsign(), "alpha");
        let bravo = team.claim(NATO).unwrap().unwrap();
        assert_eq!(bravo.callsign(), "bravo");
        assert!(team.claim(["alpha"]).unwrap().is_none());

        alpha.publish(&record("alpha")).unwrap();
        bravo.publish(&record("bravo")).unwrap();
        let seen: Vec<_> = team
            .members(Some("bravo"))
            .into_iter()
            .map(|p| p.callsign)
            .collect();
        assert_eq!(seen, ["alpha"]);

        drop(alpha);
        assert!(!team.dir().join("alpha.lock").exists());
        assert_eq!(team.claim(NATO).unwrap().unwrap().callsign(), "alpha");
        drop(bravo);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_member_whose_lock_is_free_is_swept() {
        let root = temp();
        let team = Team::open(&root, "t").unwrap();
        let alpha = team.claim(["alpha"]).unwrap().unwrap();
        alpha.publish(&record("alpha")).unwrap();
        // A crashed holder: its files remain, its lock is released.
        alpha.lock_file().unlock().unwrap();
        std::mem::forget(alpha);
        assert!(team.members(None).is_empty());
        assert!(!team.dir().join("alpha.json").exists());
        assert!(!team.dir().join("alpha.lock").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn worktrees_share_their_common_directory() {
        let root = temp();
        let main = root.join("main");
        fs::create_dir_all(main.join(".git/worktrees/lane")).unwrap();
        fs::write(main.join(".git/HEAD"), "ref: refs/heads/develop\n").unwrap();
        fs::write(main.join(".git/worktrees/lane/commondir"), "../..\n").unwrap();
        fs::write(
            main.join(".git/worktrees/lane/HEAD"),
            "ref: refs/heads/lane\n",
        )
        .unwrap();
        let lane = root.join("lane");
        fs::create_dir_all(lane.join("src")).unwrap();
        fs::write(
            lane.join(".git"),
            format!("gitdir: {}\n", main.join(".git/worktrees/lane").display()),
        )
        .unwrap();

        let (common_main, checkout_main) = locate(&main);
        let (common_lane, checkout_lane) = locate(&lane.join("src"));
        assert_eq!(common_main, common_lane);
        assert_eq!(team_key(&common_main), team_key(&common_lane));
        assert_eq!(checkout_lane, fs::canonicalize(&lane).unwrap());
        assert_eq!(branch(&checkout_main).as_deref(), Some("develop"));
        assert_eq!(branch(&checkout_lane).as_deref(), Some("lane"));
        fs::remove_dir_all(root).unwrap();
    }
}
