//! Whether the plugin files a session reads are the owner's, checked on
//! the objects that are then read rather than on their paths.
//!
//! - **Bound to opened objects** ([`open_dir`], [`TrustedDir`]): a path is
//!   resolved once, then opened from `/` one component at a time, each
//!   with `openat` relative to the directory checked before it and without
//!   following a symbolic link. Every opened directory and file is checked
//!   through its descriptor (`fstat`, and on macOS its ACL), and file bytes
//!   are read from the checked descriptor. A component replaced between the
//!   resolution and the walk makes the walk fail or reach an object that is
//!   itself checked, so no other object is read under a checked name.
//! - **Ownership and mode**: each object must be owned by the effective
//!   user or root and not writable by its group or others, except that a
//!   group- or world-writable directory with the sticky bit (such as
//!   `/tmp`) is accepted: others can add entries there but not rename or
//!   remove ours, and anything they add is owned by them and refused.
//!   Each symbolic link on the path as given must be the user's or root's.
//!   This is OpenSSH's `StrictModes` rule. On Linux a POSIX ACL that grants
//!   write to a named user or group shows as group write through its mask,
//!   so the mode check covers it.
//! - **macOS ACLs**: extended ACLs can grant write beyond the mode bits.
//!   Every `allow` entry on an opened object that grants writing data,
//!   appending, deleting, deleting children, changing the ACL, or taking
//!   ownership is refused, whoever it names; `deny` entries, such as the
//!   one macOS puts on a home directory, are accepted.
//! - **Other platforms**, Windows among them: owner and DACL checks are
//!   not implemented, so [`SUPPORTED`] is false and the caller refuses
//!   any configuration that enables a plugin. An empty home still works.
//! - **Placement** ([`check_outside`]): the plugins directory must not lie
//!   inside the session's workspace, nor the workspace inside it, where the
//!   agent's own file tools under `workspace-write` could change it; nor
//!   inside a git working tree (a directory with a `.git` entry), so a
//!   repository's files never become plugin configuration. Directories are
//!   compared by device and inode: those of the plugins directory and every
//!   directory above it as opened by [`open_dir`], and those of the
//!   workspace and its ancestors as `stat` finds them by its canonical path
//!   when the check runs. Each opened directory is probed for `.git`
//!   relative to its descriptor; an error other than "not found" refuses
//!   the placement rather than reading as no repository. The sandbox's
//!   other writable roots, such as temporary directories, are not checked,
//!   and `danger-full-access`, or anything else running as the user, can
//!   change whatever the user can.
//! - **Updates** ([`TrustedDir::check_same_file`]): management checks the
//!   configuration it changes on the handle the update read under its lock,
//!   by opening the file again through the checked directories and
//!   requiring the same device and inode, so it never rewrites contents
//!   read from another object than the one checked.
//!
//! The checks bound who other than the user can change the files. They say
//! nothing about the user's own processes; a store copy's integrity is its
//! rehash at every read, and its read-only bits only keep accidental
//! writes out.

/// Whether this platform checks ownership and permissions. Where it does
/// not, a configuration that enables a plugin is refused.
pub(super) const SUPPORTED: bool = cfg!(any(target_os = "linux", target_os = "macos"));

#[cfg(unix)]
pub(super) use unix::{TrustedPath, check_outside, open_dir};

#[cfg(unix)]
mod unix {
    use std::ffi::CString;
    use std::fs::File;
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Component, Path, PathBuf};

    /// A directory opened and checked, with every directory above it.
    #[derive(Debug)]
    pub(in super::super) struct TrustedDir {
        fd: OwnedFd,
        path: PathBuf,
        user: u32,
    }

    /// A directory opened by [`open_dir`] and the opened, checked
    /// directories above it, from `/` down.
    #[derive(Debug)]
    pub(in super::super) struct TrustedPath {
        above: Vec<TrustedDir>,
        dir: TrustedDir,
    }

    impl TrustedPath {
        /// The directory itself.
        pub(in super::super) fn dir(&self) -> &TrustedDir {
            &self.dir
        }

        /// The directory, then each directory above it up to `/`.
        fn upwards(&self) -> impl Iterator<Item = &TrustedDir> {
            std::iter::once(&self.dir).chain(self.above.iter().rev())
        }
    }

    /// Checks that the opened plugins directory `root` is outside
    /// `workspace` and any git working tree, as the module documentation
    /// describes. Blocks.
    pub(in super::super) fn check_outside(
        root: &TrustedPath,
        workspace: Option<&Path>,
    ) -> Result<(), String> {
        let shown = root.dir.path.display();
        if let Some(workspace) = workspace {
            let canonical = std::fs::canonicalize(workspace)
                .map_err(|error| format!("cannot resolve {}: {error}", workspace.display()))?;
            let mut ids = Vec::new();
            for dir in canonical.ancestors() {
                let metadata = std::fs::metadata(dir)
                    .map_err(|error| format!("cannot inspect {}: {error}", dir.display()))?;
                ids.push((metadata.dev(), metadata.ino()));
            }
            // The workspace lies in the plugins directory, or the plugins
            // directory in the workspace.
            let mut overlaps = ids.contains(&root.dir.id()?);
            for dir in root.upwards() {
                overlaps |= ids.first() == Some(&dir.id()?);
            }
            if overlaps {
                return Err(format!(
                    "{shown} overlaps the session's workspace {}, where the agent's tools can change it; set BAKE_HOME outside the workspace",
                    canonical.display()
                ));
            }
        }
        for dir in root.upwards() {
            if dir.has_entry(".git")? {
                return Err(format!(
                    "{shown} is inside the git working tree {}, whose files are not plugin configuration; set BAKE_HOME outside any repository",
                    dir.path.display()
                ));
            }
        }
        Ok(())
    }

    /// Opens and checks the directory `path` and every directory above it,
    /// as the module documentation describes; `None` when part of it does
    /// not exist. Blocks.
    pub(in super::super) fn open_dir(path: &Path) -> Result<Option<TrustedPath>, String> {
        // SAFETY: no arguments, cannot fail.
        let user = unsafe { libc::geteuid() };
        let given = std::path::absolute(path)
            .map_err(|error| format!("cannot resolve {}: {error}", path.display()))?;
        // A link on the given path could be repointed by its owner.
        for entry in given.ancestors() {
            match std::fs::symlink_metadata(entry) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    owner(entry, metadata.uid(), user)?;
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(format!("cannot inspect {}: {error}", entry.display())),
            }
        }
        let canonical = match std::fs::canonicalize(&given) {
            Ok(canonical) => canonical,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("cannot resolve {}: {error}", given.display())),
        };
        let root = open(None, Path::new("/"), libc::O_DIRECTORY)
            .map_err(|error| format!("cannot open /: {error}"))?;
        let mut dir = TrustedDir {
            fd: root,
            path: PathBuf::from("/"),
            user,
        };
        dir.check(true)?;
        let mut above = Vec::new();
        for component in canonical.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => match dir.dir(Path::new(name))? {
                    Some(next) => above.push(std::mem::replace(&mut dir, next)),
                    None => return Ok(None),
                },
                // `canonicalize` leaves none of these.
                _ => return Err(format!("cannot resolve {}", canonical.display())),
            }
        }
        Ok(Some(TrustedPath { above, dir }))
    }

    impl TrustedDir {
        /// Opens and checks the subdirectory `name`; `None` when it does
        /// not exist. Blocks.
        pub(in super::super) fn dir(&self, name: &Path) -> Result<Option<TrustedDir>, String> {
            let path = self.path.join(name);
            let fd = match self.open(name, libc::O_DIRECTORY) {
                Ok(fd) => fd,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => {
                    return Err(format!(
                        "cannot open {} as a directory without following links: {error}",
                        path.display()
                    ));
                }
            };
            let dir = TrustedDir {
                fd,
                path,
                user: self.user,
            };
            dir.check(true)?;
            Ok(Some(dir))
        }

        /// Reads the regular file `name`, at most `limit + 1` bytes, from
        /// the descriptor its checks ran on; `None` when it does not exist.
        /// Blocks.
        pub(in super::super) fn read(
            &self,
            name: &str,
            limit: usize,
        ) -> Result<Option<Vec<u8>>, String> {
            let path = self.path.join(name);
            // A FIFO would block the open itself; the type check refuses it.
            let fd = match self.open(Path::new(name), libc::O_NONBLOCK) {
                Ok(fd) => fd,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
                    return Err(format!("{} is a symbolic link", path.display()));
                }
                Err(error) => return Err(format!("cannot open {}: {error}", path.display())),
            };
            let file = TrustedDir {
                fd,
                path,
                user: self.user,
            };
            file.check(false)?;
            let mut bytes = Vec::new();
            File::from(file.fd)
                .take(limit as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| format!("cannot read {}: {error}", file.path.display()))?;
            Ok(Some(bytes))
        }

        /// Checks the regular file `name` as [`Self::read`] does, and that
        /// it is the object `file` has open: the same device and inode. An
        /// update checks the handle it read from this way, after reading,
        /// so contents read from a replaced or swapped entry are refused.
        /// Blocks.
        pub(in super::super) fn check_same_file(
            &self,
            name: &str,
            file: &File,
        ) -> Result<(), String> {
            let path = self.path.join(name);
            let fd = match self.open(Path::new(name), libc::O_NONBLOCK) {
                Ok(fd) => fd,
                Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
                    return Err(format!("{} is a symbolic link", path.display()));
                }
                Err(error) => return Err(format!("cannot open {}: {error}", path.display())),
            };
            let checked = TrustedDir {
                fd,
                path,
                user: self.user,
            };
            checked.check(false)?;
            let read = file
                .metadata()
                .map_err(|error| format!("cannot inspect {}: {error}", checked.path.display()))?;
            if checked.id()? != (read.dev(), read.ino()) {
                return Err(format!(
                    "{} was replaced while it was read; try again",
                    checked.path.display()
                ));
            }
            Ok(())
        }

        fn open(&self, name: &Path, flags: libc::c_int) -> std::io::Result<OwnedFd> {
            open(Some(&self.fd), name, flags)
        }

        fn metadata(&self) -> Result<std::fs::Metadata, String> {
            File::from(
                self.fd
                    .try_clone()
                    .map_err(|error| format!("cannot inspect {}: {error}", self.path.display()))?,
            )
            .metadata()
            .map_err(|error| format!("cannot inspect {}: {error}", self.path.display()))
        }

        /// The opened object's device and inode.
        fn id(&self) -> Result<(u64, u64), String> {
            let metadata = self.metadata()?;
            Ok((metadata.dev(), metadata.ino()))
        }

        /// Whether this directory has an entry `name`, not following a
        /// link; an error other than "not found" is an error, not absence.
        fn has_entry(&self, name: &str) -> Result<bool, String> {
            let path = self.path.join(name);
            let c_name = CString::new(name)
                .map_err(|_| format!("cannot inspect {}: a NUL in the name", path.display()))?;
            // SAFETY: an all-zero `stat` is valid output space.
            let mut stat: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: an open directory descriptor, a NUL-terminated name,
            // and that space.
            let result = unsafe {
                libc::fstatat(
                    self.fd.as_raw_fd(),
                    c_name.as_ptr(),
                    &mut stat,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if result == 0 {
                return Ok(true);
            }
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                Ok(false)
            } else {
                Err(format!(
                    "cannot check {} for a git working tree: {error}",
                    self.path.display()
                ))
            }
        }

        /// Checks the opened object: its type, owner, mode, and ACL.
        fn check(&self, directory: bool) -> Result<(), String> {
            let metadata = self.metadata()?;
            if directory != metadata.is_dir() || !(directory || metadata.is_file()) {
                return Err(format!(
                    "{} is not a {}",
                    self.path.display(),
                    if directory {
                        "directory"
                    } else {
                        "regular file"
                    }
                ));
            }
            owner(&self.path, metadata.uid(), self.user)?;
            let mode = metadata.mode();
            let sticky = directory && mode & 0o1000 != 0;
            if mode & 0o022 != 0 && !sticky {
                return Err(format!(
                    "{} is writable by its group or by others (mode {:o}); run `chmod go-w` on it",
                    self.path.display(),
                    mode & 0o7777
                ));
            }
            #[cfg(target_os = "macos")]
            super::macos::check_acl(&self.fd, &self.path)?;
            Ok(())
        }
    }

    fn owner(path: &Path, uid: u32, user: u32) -> Result<(), String> {
        if uid == user || uid == 0 {
            Ok(())
        } else {
            Err(format!(
                "{} is owned by user {uid}, not by you or root",
                path.display()
            ))
        }
    }

    /// `openat(dir, name)` for reading, never following a final link.
    fn open(dir: Option<&OwnedFd>, name: &Path, flags: libc::c_int) -> std::io::Result<OwnedFd> {
        let name = CString::new(name.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        let dirfd = dir.map_or(libc::AT_FDCWD, AsRawFd::as_raw_fd);
        // SAFETY: `name` is a valid C string and `dirfd` an open descriptor
        // or `AT_FDCWD`; the result is checked before it is owned.
        let fd = unsafe {
            libc::openat(
                dirfd,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | flags,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `fd` was just returned open and is owned by nothing else.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

/// macOS extended ACLs, read through the opened descriptor.
#[cfg(target_os = "macos")]
mod macos {
    use std::os::fd::{AsRawFd, OwnedFd};
    use std::os::raw::{c_int, c_void};
    use std::path::Path;

    // <sys/acl.h>
    const ACL_TYPE_EXTENDED: c_int = 0x0000_0100;
    const ACL_FIRST_ENTRY: c_int = 0;
    const ACL_NEXT_ENTRY: c_int = -1;
    const ACL_EXTENDED_ALLOW: c_int = 1;
    const ACL_EXTENDED_DENY: c_int = 2;
    // The permissions that change an object, its entries, or who may.
    const WRITES: [(c_int, &str); 8] = [
        (1 << 2, "write_data/add_file"),
        (1 << 4, "delete"),
        (1 << 5, "append_data/add_subdirectory"),
        (1 << 6, "delete_child"),
        (1 << 8, "writeattr"),
        (1 << 10, "writeextattr"),
        (1 << 12, "writesecurity"),
        (1 << 13, "chown"),
    ];

    unsafe extern "C" {
        fn acl_get_fd_np(fd: c_int, kind: c_int) -> *mut c_void;
        fn acl_valid(acl: *mut c_void) -> c_int;
        fn acl_get_entry(acl: *mut c_void, entry_id: c_int, entry: *mut *mut c_void) -> c_int;
        fn acl_get_tag_type(entry: *mut c_void, tag: *mut c_int) -> c_int;
        fn acl_get_permset(entry: *mut c_void, permset: *mut *mut c_void) -> c_int;
        fn acl_get_perm_np(permset: *mut c_void, perm: c_int) -> c_int;
        fn acl_free(object: *mut c_void) -> c_int;
    }

    struct Acl(*mut c_void);

    impl Drop for Acl {
        fn drop(&mut self) {
            // SAFETY: a non-null ACL from `acl_get_fd_np`, freed once.
            unsafe { acl_free(self.0) };
        }
    }

    /// Refuses an `allow` entry that grants any of [`WRITES`].
    pub(super) fn check_acl(fd: &OwnedFd, path: &Path) -> Result<(), String> {
        // SAFETY: `fd` is open; a null result is handled below.
        let acl = unsafe { acl_get_fd_np(fd.as_raw_fd(), ACL_TYPE_EXTENDED) };
        if acl.is_null() {
            let error = std::io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::ENOENT) => Ok(()),
                _ => Err(format!(
                    "cannot read the ACL of {}: {error}",
                    path.display()
                )),
            };
        }
        let acl = Acl(acl);
        // SAFETY: an owned ACL returned by libSystem, not modified here.
        if unsafe { acl_valid(acl.0) } != 0 {
            return Err(format!("the ACL of {} is invalid", path.display()));
        }
        let mut which = ACL_FIRST_ENTRY;
        let mut index = 0usize;
        loop {
            let mut entry = std::ptr::null_mut();
            // SAFETY: `acl` is a valid ACL and `entry` a valid out pointer.
            if unsafe { acl_get_entry(acl.0, which, &mut entry) } != 0 {
                // Darwin uses EINVAL for the end of a valid ACL, including
                // an empty one (Libc posix1e/acl_get_entry.3). This ACL was
                // validated above and is never modified during iteration.
                let error = std::io::Error::last_os_error();
                return if error.raw_os_error() == Some(libc::EINVAL) {
                    Ok(())
                } else {
                    Err(format!(
                        "cannot enumerate the ACL of {}: {error}",
                        path.display()
                    ))
                };
            }
            which = ACL_NEXT_ENTRY;
            let unreadable = || format!("cannot read the ACL of {}", path.display());
            let mut tag = 0;
            let mut permset = std::ptr::null_mut();
            // SAFETY: `entry` came from `acl_get_entry` on the live ACL.
            if unsafe { acl_get_tag_type(entry, &mut tag) } != 0
                || unsafe { acl_get_permset(entry, &mut permset) } != 0
            {
                return Err(unreadable());
            }
            if tag != ACL_EXTENDED_ALLOW && tag != ACL_EXTENDED_DENY {
                return Err(unreadable());
            }
            if tag == ACL_EXTENDED_ALLOW {
                for (perm, name) in WRITES {
                    // SAFETY: `permset` came from the live entry.
                    match unsafe { acl_get_perm_np(permset, perm) } {
                        0 => {}
                        1 => {
                            return Err(format!(
                                "{} has ACL entry {index}, which allows {name}; `ls -led` lists it and `chmod -a# {index}` removes it",
                                path.display()
                            ));
                        }
                        _ => return Err(unreadable()),
                    }
                }
            }
            index += 1;
        }
    }
}
