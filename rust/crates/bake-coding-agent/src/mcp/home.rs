//! `$BAKE_HOME/mcp.json`, the one MCP configuration `bake-rs` reads, and
//! the checks that make it the owner's.
//!
//! Pi v1.3.0 reads `~/.pi/agent/mcp.json` and, for a trusted project, the
//! project's `.pi/mcp.json` (`src/extensions/mcp/config.ts`
//! `loadMcpConfig`). Bake reads only the Bake home's file: no project file
//! and no project trust. An entry's `env` may pass variables Bake's scrub
//! drops ([`super::config`]), so a server can receive credentials, and the
//! file must be one only its owner can change. The Bake home is wherever
//! `BAKE_HOME` (then `DSH_HOME`) points, so neither the name nor the path
//! as spelled is trusted.
//!
//! # Unix: checks bound to what is read
//!
//! The home is resolved once to its canonical path, which holds no link.
//! From there nothing goes through the spelled path again: the directories
//! are opened one component at a time from `/`, each with `O_NOFOLLOW |
//! O_DIRECTORY` relative to the one above (`O_PATH` on Linux, so a
//! directory needs only search permission), and `mcp.json` is opened
//! relative to the last with `O_NOFOLLOW | O_NONBLOCK`. A link swapped into
//! the path after it was resolved, at any component, fails the open
//! (`ELOOP` or `ENOTDIR`); a link anywhere in the spelled path that is
//! changed later reaches nothing, since that path is not opened. Every rule
//! then reads the opened handles (`fstat`, `fstatat`), so it judges the
//! very directories and file that are read, refusing the file, with the
//! reason and what to change, and starting no server from it, unless:
//!
//! - **The file** is a regular file (a FIFO is refused without waiting on
//!   it) of at most [`MAX_CONFIG_BYTES`](super::config::MAX_CONFIG_BYTES).
//! - **Owner and mode.** The file and every directory above it belong to
//!   the current user or to root, and no other user may write them: no
//!   group or other write bit, except on a directory with the sticky bit
//!   (such as `/tmp`), where only an entry's owner may rename or remove it.
//!   Bake's own stores create the home `0700` and its files `0600`
//!   ([`crate::json_store`]). Reading by others is not refused, as other MCP
//!   clients' files are commonly `0644`.
//! - **Access control lists.** Mode bits alone do not show who may write
//!   where an ACL grants more. On Linux a POSIX ACL's named entries are
//!   bounded by its mask, which is the group bits, so the mode rule covers
//!   them. On macOS each opened object's extended ACL is read
//!   (`acl_get_fd_np`), and an `allow` entry granting any change to anyone
//!   but the owner refuses it; `deny` entries are kept. Other Unix systems'
//!   ACLs are not read, so the file is refused there.
//! - **Outside the workspace and repositories.** No opened directory below
//!   the user's home directory (every one when it is unknown) is the
//!   session's working directory, which the agent's tools may write under
//!   `workspace-write`, or holds a `.git` entry: a git work tree's files come
//!   from its repository. Directories are compared by device and inode. The
//!   user's home directory and those above it are the user's own, so
//!   running in the home directory, or keeping it in git, does not refuse
//!   `~/.bake/mcp.json`.
//!
//! An attacker who could still change what is read would need write access
//! to one of the opened directories, which the mode rule refuses, or to be
//! the user or root.
//!
//! # Other platforms
//!
//! Windows ACLs are not inspected yet, so who may change the file cannot
//! be shown. Until that check is implemented and qualified, an existing
//! `mcp.json` is refused there unread, and no server starts from it; a
//! missing one is an ordinary empty configuration.
//!
//! Every read is of the local file system, bounded in size, and blocking:
//! the caller runs it on Tokio's blocking pool, as it reads `settings.json`.

use std::path::Path;

use super::config::{MCP_CONFIG_FILE, McpConfig, failed};

/// Reads `home`'s `mcp.json` for a session whose working directory is
/// `workspace`, if the file passes the module's rules; `user_home` is the
/// user's home directory. A missing file or home is an empty configuration;
/// a refused file is one error naming its path and no server. Blocking.
pub fn load_home_mcp_config(home: &Path, workspace: &Path, user_home: Option<&Path>) -> McpConfig {
    let path = home.join(MCP_CONFIG_FILE);
    let dir = match std::fs::canonicalize(home) {
        Ok(dir) => dir,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return McpConfig::default();
        }
        Err(error) => return refused(&path, &error.to_string()),
    };
    #[cfg(test)]
    seam::after_resolve(&dir);
    match open_owner_file(&dir, &MCP_RULES, workspace, user_home) {
        Ok(None) => McpConfig::default(),
        Ok(Some(file)) => super::config::read_config(file, &path),
        Err(reason) => refused(&path, &reason),
    }
}

/// A file the module's rules admit, and how its refusals name it and say
/// what to change. `$BAKE_HOME/mcp.json` is one; a hooks file
/// `settings.json` names is another ([`crate::hooks`]).
#[cfg_attr(
    not(unix),
    allow(dead_code, reason = "the ownership rules are read on Unix")
)]
pub(crate) struct OwnerFile<'a> {
    /// The file's name in its directory.
    pub name: &'a str,
    /// What resolved the directory, for a directory changed since.
    pub resolved: &'a str,
    /// The file as the ownership rules name it, such as `an mcp.json`.
    pub kind: &'a str,
    /// What to do about a file inside the working directory.
    pub workspace_remedy: &'a str,
    /// What to do about a file inside a git work tree.
    pub repository_remedy: &'a str,
    /// Why an existing file is refused unread where who may change it
    /// cannot be checked.
    #[cfg_attr(
        unix,
        allow(dead_code, reason = "read where ownership cannot be checked")
    )]
    pub unchecked: &'a str,
}

const MCP_RULES: OwnerFile<'static> = OwnerFile {
    name: MCP_CONFIG_FILE,
    resolved: "the Bake home",
    kind: "an mcp.json",
    workspace_remedy: "set BAKE_HOME to a directory outside it, or run with --no-mcp",
    repository_remedy: "keep mcp.json in a Bake home outside every repository (set BAKE_HOME), or run with --no-mcp",
    unchecked: "bake-rs cannot yet check on this platform who may change mcp.json, so it does not read it; remove the file or run with --no-mcp until that check is supported",
};

/// Opens `rules.name` in the canonical directory `dir` if it passes the
/// module's rules for a session in `workspace`; `Ok(None)` when it does not
/// exist, and `Err` with the reason it is refused. Blocking.
pub(crate) fn open_owner_file(
    dir: &Path,
    rules: &OwnerFile<'_>,
    workspace: &Path,
    user_home: Option<&Path>,
) -> Result<Option<std::fs::File>, String> {
    platform::open(dir, rules, workspace, user_home)
}

fn refused(path: &Path, reason: &str) -> McpConfig {
    failed(format!(
        "{}: {reason}. No MCP server was started from it",
        path.display()
    ))
}

#[cfg(unix)]
mod platform {
    use std::ffi::{CString, OsStr};
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Component, Path, PathBuf};

    use super::super::config::{LINK_REFUSED, MAX_CONFIG_BYTES};
    use super::OwnerFile;

    /// How a directory of the chain is opened: for search only where the
    /// platform allows it.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    const DIRECTORY_ACCESS: libc::c_int = libc::O_PATH;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    const DIRECTORY_ACCESS: libc::c_int = libc::O_RDONLY;

    /// The facts the rules read from one opened object.
    struct Stat {
        dev: u64,
        ino: u64,
        uid: u32,
        mode: u32,
        size: u64,
    }

    impl Stat {
        fn id(&self) -> (u64, u64) {
            (self.dev, self.ino)
        }

        fn from_raw(stat: &libc::stat) -> Self {
            // The field types differ between platforms; every value fits.
            #[allow(clippy::unnecessary_cast)]
            Self {
                dev: stat.st_dev as u64,
                ino: stat.st_ino as u64,
                uid: stat.st_uid as u32,
                mode: stat.st_mode as u32,
                size: u64::try_from(stat.st_size).unwrap_or(0),
            }
        }
    }

    fn fstat(fd: &OwnedFd) -> io::Result<Stat> {
        // SAFETY: an all-zero `stat` is valid output space.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: a live descriptor and that space.
        if unsafe { libc::fstat(fd.as_raw_fd(), &mut stat) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Stat::from_raw(&stat))
    }

    /// `stat(2)` of a path, following links: the identity of a directory
    /// named by the caller, such as the working directory.
    fn stat_path(path: &Path) -> Option<Stat> {
        let name = CString::new(path.as_os_str().as_bytes()).ok()?;
        // SAFETY: an all-zero `stat` is valid output space.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: a NUL-terminated path and that space.
        (unsafe { libc::stat(name.as_ptr(), &mut stat) } == 0).then(|| Stat::from_raw(&stat))
    }

    /// Whether `name` exists in the directory `dir`, not following a link.
    fn has_entry(dir: &OwnedFd, name: &str) -> io::Result<bool> {
        let name = CString::new(name)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a NUL in the name"))?;
        // SAFETY: an all-zero `stat` is valid output space.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: a live directory descriptor, a NUL-terminated name, and
        // that space.
        let result = unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                name.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            Ok(false)
        } else {
            Err(error)
        }
    }

    /// `openat(dir, name, flags)` with close-on-exec; `dir` `None` opens the
    /// absolute `name`.
    fn open_at(dir: Option<&OwnedFd>, name: &OsStr, flags: libc::c_int) -> io::Result<OwnedFd> {
        let name = CString::new(name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a NUL in the path"))?;
        let at = dir.map_or(libc::AT_FDCWD, AsRawFd::as_raw_fd);
        // SAFETY: a live descriptor or `AT_FDCWD`, and a NUL-terminated
        // name; the result is a new descriptor or -1.
        let fd = unsafe { libc::openat(at, name.as_ptr(), flags | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh descriptor owned only here.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// Opens `/` and then each component of the canonical `dir` relative to
    /// the one before, never following a link: every directory from the
    /// root down to `dir`, with its path for messages.
    fn open_chain(dir: &Path, rules: &OwnerFile<'_>) -> Result<Vec<(PathBuf, OwnedFd)>, String> {
        let flags = DIRECTORY_ACCESS | libc::O_DIRECTORY | libc::O_NOFOLLOW;
        let mut chain: Vec<(PathBuf, OwnedFd)> = Vec::new();
        let mut shown = PathBuf::new();
        for component in dir.components() {
            let name = match component {
                Component::RootDir => OsStr::new("/"),
                Component::Normal(name) => name,
                // Canonical paths hold neither; one that does is refused.
                _ => return Err(format!("{} is not a canonical path", dir.display())),
            };
            shown.push(name);
            let opened = match chain.last() {
                None => open_at(None, name, flags),
                Some((_, parent)) => open_at(Some(parent), name, flags),
            };
            let fd = opened.map_err(|error| match error.raw_os_error() {
                Some(libc::ELOOP | libc::ENOTDIR) => format!(
                    "{} changed after bake-rs resolved {} (it is now a link or not a directory)",
                    shown.display(),
                    rules.resolved
                ),
                _ => format!("cannot open the directory {}: {error}", shown.display()),
            })?;
            chain.push((shown.clone(), fd));
        }
        Ok(chain)
    }

    /// Access control lists, which can grant changes the mode bits do not
    /// show, read through the opened handle.
    ///
    /// - Linux: a POSIX ACL's named entries are bounded by its mask, which
    ///   `stat` reports as the group bits, so the mode check (no group
    ///   write) already shows no named entry can write. Nothing more is
    ///   read.
    /// - macOS: extended ACLs are independent of the mode bits. Every
    ///   `allow` entry that grants a change (data, append, add or delete an
    ///   entry, delete, attributes, extended attributes, security, owner)
    ///   is refused unless its qualifier is the object's owner, the current
    ///   user or root (checked above); `deny` entries only narrow access and
    ///   are kept. Inherit-only entries are judged too, conservatively.
    ///   The check fails closed: an ACL `acl_valid` rejects, an entry that
    ///   cannot be read, a tag other than allow or deny, a permission
    ///   result other than set or unset, and any end of the entries other
    ///   than `acl_get_entry`'s documented -1 with `EINVAL` all refuse it
    ///   (`acl_rules`).
    /// - Other Unix systems: their ACLs are not read, so every object is
    ///   refused there.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    mod acl {
        use std::os::fd::OwnedFd;

        pub(super) fn check(_fd: &OwnedFd, _owner: u32) -> Result<(), String> {
            Ok(())
        }
    }

    #[cfg(target_os = "macos")]
    mod acl {
        use std::ffi::c_void;
        use std::os::fd::{AsRawFd, OwnedFd};

        use super::super::acl_rules::{
            ACL_FIRST_ENTRY, ACL_MAX_ENTRIES, ACL_NEXT_ENTRY, ACL_TYPE_EXTENDED, CHANGES, Kind,
            Step, UNREADABLE, kind, perm_set, step,
        };

        type Acl = *mut c_void;
        type Entry = *mut c_void;
        type Permset = *mut c_void;

        unsafe extern "C" {
            fn acl_get_fd_np(fd: libc::c_int, kind: libc::c_int) -> Acl;
            fn acl_valid(acl: Acl) -> libc::c_int;
            fn acl_get_entry(acl: Acl, id: libc::c_int, entry: *mut Entry) -> libc::c_int;
            fn acl_get_tag_type(entry: Entry, tag: *mut libc::c_int) -> libc::c_int;
            fn acl_get_permset(entry: Entry, permset: *mut Permset) -> libc::c_int;
            fn acl_get_perm_np(permset: Permset, perm: libc::c_int) -> libc::c_int;
            fn acl_get_qualifier(entry: Entry) -> *mut c_void;
            fn acl_free(object: *mut c_void) -> libc::c_int;
            fn mbr_uid_to_uuid(uid: libc::uid_t, uuid: *mut u8) -> libc::c_int;
        }

        /// Frees what an `acl_*` call returned.
        struct Freed(*mut c_void);

        impl Drop for Freed {
            fn drop(&mut self) {
                // SAFETY: an object an `acl_*` call returned, freed once.
                unsafe {
                    acl_free(self.0);
                }
            }
        }

        fn uuid_of(uid: u32) -> Option<[u8; 16]> {
            let mut uuid = [0u8; 16];
            // SAFETY: a 16-byte output buffer, the size of `uuid_t`.
            (unsafe { mbr_uid_to_uuid(uid, uuid.as_mut_ptr()) } == 0).then_some(uuid)
        }

        fn errno() -> libc::c_int {
            std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
        }

        fn clear_errno() {
            // SAFETY: `__error` returns this thread's `errno` slot.
            unsafe {
                *libc::__error() = 0;
            }
        }

        pub(super) fn check(fd: &OwnedFd, owner: u32) -> Result<(), String> {
            // SAFETY: a live descriptor; NULL means no ACL or an error.
            let acl = unsafe { acl_get_fd_np(fd.as_raw_fd(), ACL_TYPE_EXTENDED) };
            if acl.is_null() {
                let error = std::io::Error::last_os_error();
                return if error.raw_os_error() == Some(libc::ENOENT) {
                    Ok(())
                } else {
                    Err(format!("its access control list cannot be read: {error}"))
                };
            }
            let _acl = Freed(acl);
            // Only a valid ACL lets `acl_get_entry`'s EINVAL mean its end.
            // SAFETY: the live ACL.
            if unsafe { acl_valid(acl) } != 0 {
                return Err(UNREADABLE.to_owned());
            }
            let owner = uuid_of(owner);
            let mut read = 0usize;
            loop {
                let id = if read == 0 {
                    ACL_FIRST_ENTRY
                } else {
                    ACL_NEXT_ENTRY
                };
                let mut entry: Entry = std::ptr::null_mut();
                clear_errno();
                // SAFETY: a live ACL and an output slot.
                let status = unsafe { acl_get_entry(acl, id, &mut entry) };
                match step(status, errno(), entry.is_null(), read) {
                    Step::End => return Ok(()),
                    Step::Entry => read += 1,
                    Step::Unreadable => return Err(UNREADABLE.to_owned()),
                }
                debug_assert!(read <= ACL_MAX_ENTRIES);
                let mut tag = 0;
                // SAFETY: an entry of the live ACL and an output slot.
                if unsafe { acl_get_tag_type(entry, &mut tag) } != 0 {
                    return Err(UNREADABLE.to_owned());
                }
                match kind(tag) {
                    Kind::Deny => continue,
                    Kind::Allow => {}
                    Kind::Unknown => return Err(UNREADABLE.to_owned()),
                }
                let mut permset: Permset = std::ptr::null_mut();
                // SAFETY: as above.
                if unsafe { acl_get_permset(entry, &mut permset) } != 0 || permset.is_null() {
                    return Err(UNREADABLE.to_owned());
                }
                let mut changes = false;
                for perm in CHANGES {
                    // SAFETY: the entry's permission set and a documented permission.
                    match perm_set(unsafe { acl_get_perm_np(permset, perm) }) {
                        Some(set) => changes |= set,
                        None => return Err(UNREADABLE.to_owned()),
                    }
                }
                if !changes {
                    continue;
                }
                // SAFETY: an entry of the live ACL; the result is freed.
                let qualifier = unsafe { acl_get_qualifier(entry) };
                if qualifier.is_null() {
                    return Err(UNREADABLE.to_owned());
                }
                let qualifier = Freed(qualifier);
                // SAFETY: an extended entry's qualifier is a 16-byte GUID.
                let guid = unsafe { std::ptr::read_unaligned(qualifier.0.cast::<[u8; 16]>()) };
                if Some(guid) != owner {
                    return Err(
                        "it has an access control entry that lets another user or group change it (see ls -le; remove it with chmod -a or chmod -N)"
                            .to_owned(),
                    );
                }
            }
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    mod acl {
        use std::os::fd::OwnedFd;

        pub(super) fn check(_fd: &OwnedFd, _owner: u32) -> Result<(), String> {
            Err("bake-rs cannot check this platform's access control lists".to_owned())
        }
    }

    fn is_trusted_owner(uid: u32) -> bool {
        // SAFETY: `geteuid` has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        uid == me || uid == 0
    }

    /// Opens the file under the rules (see [`super::open_owner_file`]).
    pub(super) fn open(
        dir: &Path,
        rules: &OwnerFile<'_>,
        workspace: &Path,
        user_home: Option<&Path>,
    ) -> Result<Option<std::fs::File>, String> {
        let chain = match open_chain(dir, rules) {
            Ok(chain) => chain,
            // No file, nothing to refuse: a directory bake-rs cannot walk is
            // not a reason to warn about a file it does not hold.
            Err(_)
                if std::fs::symlink_metadata(dir.join(rules.name))
                    .is_err_and(|error| error.kind() == io::ErrorKind::NotFound) =>
            {
                return Ok(None);
            }
            Err(reason) => return Err(reason),
        };
        let Some((_, last)) = chain.last() else {
            return Err(format!("{} has no directory", rules.resolved));
        };
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY;
        let file = match open_at(Some(last), OsStr::new(rules.name), flags) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
                return Err(LINK_REFUSED.to_owned());
            }
            Err(error) => return Err(error.to_string()),
        };
        check(&chain, &file, rules, workspace, user_home)?;
        Ok(Some(std::fs::File::from(file)))
    }

    /// Every rule, read from the opened handles.
    fn check(
        chain: &[(PathBuf, OwnedFd)],
        file: &OwnedFd,
        rules: &OwnerFile<'_>,
        workspace: &Path,
        user_home: Option<&Path>,
    ) -> Result<(), String> {
        let stat = fstat(file).map_err(|error| error.to_string())?;
        // `mode_t` is `u16` on macOS and `u32` on Linux.
        #[allow(clippy::useless_conversion)]
        let (kind, regular) = (u32::from(libc::S_IFMT), u32::from(libc::S_IFREG));
        if stat.mode & kind != regular {
            return Err("is not a regular file; bake-rs reads only a regular file".to_owned());
        }
        if stat.size > MAX_CONFIG_BYTES {
            return Err(format!("larger than {MAX_CONFIG_BYTES} bytes"));
        }
        if !is_trusted_owner(stat.uid) {
            return Err(format!(
                "is owned by another user (uid {}); bake-rs reads only {} that you or root own",
                stat.uid, rules.kind
            ));
        }
        let mode = stat.mode & 0o7777;
        if mode & 0o022 != 0 {
            return Err(format!(
                "can be changed by other users (mode {mode:04o}); remove their write access with chmod go-w"
            ));
        }
        acl::check(file, stat.uid).map_err(|reason| {
            format!(
                "{reason}; bake-rs reads only {} that only you can change",
                rules.kind
            )
        })?;
        let mut stats = Vec::with_capacity(chain.len());
        for (shown, fd) in chain {
            let stat = fstat(fd)
                .map_err(|error| format!("cannot inspect {}: {error}", shown.display()))?;
            let mode = stat.mode & 0o7777;
            if !is_trusted_owner(stat.uid) {
                return Err(format!(
                    "its directory {} is owned by another user (uid {}), who could replace the file",
                    shown.display(),
                    stat.uid
                ));
            }
            if mode & 0o022 != 0 && mode & 0o1000 == 0 {
                return Err(format!(
                    "its directory {} can be changed by other users (mode {mode:04o}), who could replace the file; remove their write access with chmod go-w",
                    shown.display()
                ));
            }
            acl::check(fd, stat.uid).map_err(|reason| {
                format!(
                    "its directory {}: {reason}, so the file could be replaced",
                    shown.display()
                )
            })?;
            stats.push(stat);
        }
        // The user's home directory and every directory above it.
        let mine: Vec<(u64, u64)> = user_home
            .and_then(|home| std::fs::canonicalize(home).ok())
            .map(|home| {
                home.ancestors()
                    .filter_map(stat_path)
                    .map(|stat| stat.id())
                    .collect()
            })
            .unwrap_or_default();
        let workspace = stat_path(workspace).map(|stat| stat.id());
        for ((shown, fd), stat) in chain.iter().zip(&stats).rev() {
            if mine.contains(&stat.id()) {
                break;
            }
            if workspace == Some(stat.id()) {
                return Err(format!(
                    "is inside the session's working directory {}, which the agent's tools can change; {}",
                    shown.display(),
                    rules.workspace_remedy
                ));
            }
            if has_entry(fd, ".git").map_err(|error| {
                format!(
                    "cannot check {} for a git work tree: {error}",
                    shown.display()
                )
            })? {
                return Err(format!(
                    "is inside the git work tree {}, whose files its repository supplies; {}",
                    shown.display(),
                    rules.repository_remedy
                ));
            }
        }
        Ok(())
    }
}

#[cfg(not(unix))]
mod platform {
    use std::path::Path;

    use super::OwnerFile;

    /// Refuses an existing file unread: who may change it cannot be shown
    /// here yet (see the module documentation).
    pub(super) fn open(
        dir: &Path,
        rules: &OwnerFile<'_>,
        _workspace: &Path,
        _user_home: Option<&Path>,
    ) -> Result<Option<std::fs::File>, String> {
        match std::fs::symlink_metadata(dir.join(rules.name)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            _ => Err(rules.unchecked.to_owned()),
        }
    }
}

/// Text of the refusal an existing `mcp.json` gets where its owner cannot
/// be checked yet.
#[cfg(not(unix))]
pub const UNCHECKED_PLATFORM: &str = MCP_RULES.unchecked;

/// Test hooks into the read.
#[cfg(test)]
pub(crate) mod seam {
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, PoisonError};

    type Hook = Box<dyn FnOnce() + Send>;

    static AFTER_RESOLVE: Mutex<Vec<(PathBuf, Hook)>> = Mutex::new(Vec::new());

    /// Runs `hook` once, after a load has resolved the home to `dir` and
    /// before it opens anything.
    #[cfg(unix)]
    pub(crate) fn on_after_resolve(dir: &Path, hook: impl FnOnce() + Send + 'static) {
        AFTER_RESOLVE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((dir.to_path_buf(), Box::new(hook)));
    }

    pub(super) fn after_resolve(dir: &Path) {
        let hook = {
            let mut hooks = AFTER_RESOLVE.lock().unwrap_or_else(PoisonError::into_inner);
            hooks
                .iter()
                .position(|(path, _)| path == dir)
                .map(|index| hooks.remove(index).1)
        };
        if let Some(hook) = hook {
            hook();
        }
    }
}

/// How the macOS ACL check reads each libc result, kept apart from the
/// calls so every platform's tests cover it. Each meaning is the one Apple's
/// Libc documents and implements (`posix1e/acl_entry.c`, `acl_perm.c`,
/// `acl.c`, `include/sys/acl.h`, and `acl_get_entry(3)`); any other result
/// refuses the file.
#[cfg(any(test, target_os = "macos"))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod acl_rules {
    /// `ACL_TYPE_EXTENDED`.
    pub(crate) const ACL_TYPE_EXTENDED: i32 = 0x0000_0100;
    pub(crate) const ACL_FIRST_ENTRY: i32 = 0;
    pub(crate) const ACL_NEXT_ENTRY: i32 = -1;
    /// `ACL_MAX_ENTRIES`: an ACL holds no more entries.
    pub(crate) const ACL_MAX_ENTRIES: usize = 128;
    const ACL_EXTENDED_ALLOW: i32 = 1;
    const ACL_EXTENDED_DENY: i32 = 2;
    const EINVAL: i32 = 22;
    #[cfg(target_os = "macos")]
    const _: () = assert!(EINVAL == libc::EINVAL);
    /// Every permission that changes the object or what a directory holds:
    /// `ACL_WRITE_DATA` (`ADD_FILE`), `ACL_DELETE`, `ACL_APPEND_DATA`
    /// (`ADD_SUBDIRECTORY`), `ACL_DELETE_CHILD`, `ACL_WRITE_ATTRIBUTES`,
    /// `ACL_WRITE_EXTATTRIBUTES`, `ACL_WRITE_SECURITY`, and
    /// `ACL_CHANGE_OWNER`.
    pub(crate) const CHANGES: [i32; 8] = [
        1 << 2,
        1 << 4,
        1 << 5,
        1 << 6,
        1 << 8,
        1 << 10,
        1 << 12,
        1 << 13,
    ];
    pub(crate) const UNREADABLE: &str = "its access control list cannot be read completely";

    /// What one `acl_get_entry` result means.
    #[derive(Debug, PartialEq, Eq)]
    pub(crate) enum Step {
        Entry,
        End,
        Unreadable,
    }

    /// `acl_get_entry` returned `status` with `errno` after `read` entries
    /// of an ACL `acl_valid` accepted. On such an ACL, -1 with `EINVAL` is
    /// the end: the next index is past `a_entries`. Anything else, or more
    /// entries than an ACL holds, is not understood.
    pub(crate) fn step(status: i32, errno: i32, null_entry: bool, read: usize) -> Step {
        match status {
            0 if !null_entry && read < ACL_MAX_ENTRIES => Step::Entry,
            -1 if errno == EINVAL => Step::End,
            _ => Step::Unreadable,
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    pub(crate) enum Kind {
        Allow,
        Deny,
        Unknown,
    }

    /// An entry's tag: `acl_set_tag_type` accepts only allow and deny.
    pub(crate) fn kind(tag: i32) -> Kind {
        match tag {
            ACL_EXTENDED_ALLOW => Kind::Allow,
            ACL_EXTENDED_DENY => Kind::Deny,
            _ => Kind::Unknown,
        }
    }

    /// `acl_get_perm_np`: 1 set, 0 not set, anything else an error.
    pub(crate) fn perm_set(status: i32) -> Option<bool> {
        match status {
            1 => Some(true),
            0 => Some(false),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
