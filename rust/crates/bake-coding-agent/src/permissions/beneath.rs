//! Opening a checked write target without following links, so a link
//! swapped in between the containment check and the write cannot redirect
//! it. Bake's own; the TypeScript fence accepted this race
//! (`packages/fs/fs-sandbox/src/index.ts`).
//!
//! A [`BeneathTarget`] is a writable root and the link-free components of
//! the file under it, both as the guard checked them. Every open starts at
//! the root, opened itself without following a link anywhere in its path,
//! and walks the components relative to that handle:
//!
//! - **Linux:** `openat2` with `RESOLVE_NO_SYMLINKS | RESOLVE_BENEATH`
//!   relative to the root's descriptor; where the kernel lacks `openat2`
//!   (`ENOSYS`) or a seccomp filter refuses it (`EPERM`), the per-component
//!   walk below. Missing parent directories are made with `mkdirat` during
//!   that walk.
//! - **macOS and other Unix:** one `openat` per component with `O_NOFOLLOW`,
//!   each relative to the directory opened before it, and `mkdirat` for
//!   missing parents.
//! - **Windows (best effort):** before each open, every component from the
//!   root down is checked not to be a link or junction; the file itself is
//!   opened with `FILE_FLAG_OPEN_REPARSE_POINT` and refused if the handle is
//!   a reparse point, and it is truncated only after that check. A
//!   directory junction swapped in between the component checks and the
//!   open is not caught: Windows has no handle-relative open in the standard
//!   library, and creating links there needs a privilege or developer mode.
//!
//! A link met at write time refuses the write with the sandbox's denial
//! text ([`LinkRefused`]). The checked target already had every legitimate
//! link resolved, so a link inside the workspace that points inside it
//! still writes its target.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::tools::definition::MutationTarget;

/// The marker error of a write refused because a link appeared on its
/// checked path after the check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkRefused;

impl std::fmt::Display for LinkRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a link appeared on the checked write target")
    }
}

impl std::error::Error for LinkRefused {}

fn link_refused() -> io::Error {
    io::Error::other(LinkRefused)
}

/// Whether `error` is a [`LinkRefused`].
pub fn is_link_refused(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.is::<LinkRefused>())
}

/// A checked write target: the file at `components` under `root`, opened
/// without following links (see the module docs).
#[derive(Debug, Clone)]
pub struct BeneathTarget {
    root: PathBuf,
    components: Vec<OsString>,
    denial: String,
}

impl BeneathTarget {
    /// The target `target` beneath `root`, refused with `denial`. `None`
    /// unless `target` is `root` or lies under it with only normal
    /// components between them.
    pub fn new(root: &Path, target: &Path, denial: String) -> Option<Self> {
        let rest = target.strip_prefix(root).ok()?;
        let mut components = Vec::new();
        for component in rest.components() {
            match component {
                Component::Normal(name) => components.push(name.to_os_string()),
                Component::CurDir => {}
                _ => return None,
            }
        }
        Some(Self {
            root: root.to_path_buf(),
            components,
            denial,
        })
    }

    /// The root the target was checked under.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The parent components and the file's name; `.` (the root itself)
    /// when there are no components.
    fn split(&self) -> (&[OsString], OsString) {
        match self.components.split_last() {
            Some((name, parents)) => (parents, name.clone()),
            None => (&[], OsString::from(".")),
        }
    }
}

impl MutationTarget for BeneathTarget {
    fn create_parent_dirs(&self) -> io::Result<()> {
        let (parents, _) = self.split();
        imp::make_dirs(&self.root, parents)
    }

    fn create(&self) -> io::Result<File> {
        let (parents, name) = self.split();
        imp::open(&self.root, parents, &name, imp::Access::Create)
    }

    fn open_read(&self) -> io::Result<File> {
        let (parents, name) = self.split();
        imp::open(&self.root, parents, &name, imp::Access::Read)
    }

    fn refusal(&self, error: &io::Error) -> Option<String> {
        is_link_refused(error).then(|| self.denial.clone())
    }
}

/// Open a resolved artifact through the same link-refusing walk as mutations.
/// A substituted FIFO must return immediately, before its file type is checked.
pub(crate) fn open_regular_read(root: &Path, target: &Path) -> io::Result<File> {
    let target = BeneathTarget::new(root, target, String::new())
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let (parents, name) = target.split();
    let file = imp::open(&target.root, parents, &name, imp::Access::ReadRegular)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "artifact must be a regular file",
        ));
    }
    Ok(file)
}

#[cfg(unix)]
mod imp {
    //! The per-component walk, and on Linux `openat2`.

    use std::ffi::{CString, OsStr, OsString};
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Component, Path};

    use super::link_refused;

    #[derive(Clone, Copy)]
    pub(super) enum Access {
        Read,
        ReadRegular,
        Create,
    }

    impl Access {
        fn flags(self) -> libc::c_int {
            match self {
                Self::Read => libc::O_RDONLY,
                Self::ReadRegular => libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOCTTY,
                Self::Create => libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
            }
        }
    }

    /// Flags of a directory opened only to walk through.
    #[cfg(target_os = "linux")]
    const DIR_FLAGS: libc::c_int = libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    const DIR_FLAGS: libc::c_int = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC;

    fn c_name(name: &OsStr) -> io::Result<CString> {
        CString::new(name.as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
    }

    fn last_error() -> io::Error {
        io::Error::last_os_error()
    }

    /// Whether `name` in `dir` is a symbolic link now.
    fn is_link(dir: RawFd, name: &CString) -> bool {
        // SAFETY: `stat` is plain old data; zeroed is a valid value, and
        // `fstatat` only writes it.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `name` is NUL-terminated and outlives the call; `stat`
        // is a valid out-pointer.
        let result =
            unsafe { libc::fstatat(dir, name.as_ptr(), &mut stat, libc::AT_SYMLINK_NOFOLLOW) };
        result == 0 && (stat.st_mode & libc::S_IFMT) == libc::S_IFLNK
    }

    /// `openat(dir, name, flags | O_NOFOLLOW)`; a link there is
    /// [`super::LinkRefused`].
    fn open_at(dir: RawFd, name: &OsStr, flags: libc::c_int) -> io::Result<OwnedFd> {
        let name = c_name(name)?;
        let flags = flags | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let mode: libc::c_uint = 0o666;
        // SAFETY: `name` is NUL-terminated and outlives the call; the mode
        // argument is read only with `O_CREAT`.
        let fd = unsafe { libc::openat(dir, name.as_ptr(), flags, mode) };
        if fd < 0 {
            let error = last_error();
            if error.raw_os_error() == Some(libc::ELOOP) || is_link(dir, &name) {
                return Err(link_refused());
            }
            return Err(error);
        }
        // SAFETY: `fd` is a fresh descriptor this function owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// `mkdirat(dir, name, 0o777)`, an existing entry accepted.
    fn make_dir_at(dir: RawFd, name: &OsStr) -> io::Result<()> {
        let name = c_name(name)?;
        // SAFETY: `name` is NUL-terminated and outlives the call.
        if unsafe { libc::mkdirat(dir, name.as_ptr(), 0o777) } == 0 {
            return Ok(());
        }
        let error = last_error();
        if error.raw_os_error() == Some(libc::EEXIST) {
            Ok(())
        } else {
            Err(error)
        }
    }

    /// The root, opened without following a link anywhere in its path.
    fn open_root(root: &Path) -> io::Result<OwnedFd> {
        #[cfg(target_os = "linux")]
        match openat2_unless_disabled(
            root,
            libc::AT_FDCWD,
            root.as_os_str(),
            DIR_FLAGS,
            0,
            libc::RESOLVE_NO_SYMLINKS,
        ) {
            Err(Openat2::Missing) => {}
            Err(Openat2::Failed(error)) => return Err(error),
            Ok(fd) => return Ok(fd),
        }
        let mut names = Vec::new();
        for component in root.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => names.push(name),
                // Writable roots are canonical; anything else is refused.
                _ => return Err(io::Error::from(io::ErrorKind::InvalidInput)),
            }
        }
        if !root.has_root() {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let slash = c_name(OsStr::new("/"))?;
        // SAFETY: `slash` is NUL-terminated and outlives the call.
        let fd = unsafe { libc::open(slash.as_ptr(), DIR_FLAGS) };
        if fd < 0 {
            return Err(last_error());
        }
        // SAFETY: `fd` is a fresh descriptor this function owns.
        let mut dir = unsafe { OwnedFd::from_raw_fd(fd) };
        for name in names {
            dir = open_at(dir.as_raw_fd(), name, DIR_FLAGS)?;
        }
        Ok(dir)
    }

    /// The directory at `names` under `dir`, one `O_NOFOLLOW` open per
    /// component, each missing one made first when `create` is set.
    fn walk(dir: OwnedFd, names: &[OsString], create: bool) -> io::Result<OwnedFd> {
        let mut dir = dir;
        for name in names {
            if create {
                make_dir_at(dir.as_raw_fd(), name)?;
            }
            dir = open_at(dir.as_raw_fd(), name, DIR_FLAGS)?;
        }
        Ok(dir)
    }

    pub(super) fn make_dirs(root: &Path, parents: &[OsString]) -> io::Result<()> {
        walk(open_root(root)?, parents, true).map(drop)
    }

    pub(super) fn open(
        root: &Path,
        parents: &[OsString],
        name: &OsStr,
        access: Access,
    ) -> io::Result<File> {
        let root_path = root;
        let root = open_root(root_path)?;
        #[cfg(target_os = "linux")]
        {
            let mut relative = std::path::PathBuf::new();
            for parent in parents {
                relative.push(parent);
            }
            relative.push(name);
            let mode = match access {
                Access::Create => 0o666,
                Access::Read | Access::ReadRegular => 0,
            };
            match openat2_unless_disabled(
                root_path,
                root.as_raw_fd(),
                relative.as_os_str(),
                access.flags(),
                mode,
                libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_BENEATH,
            ) {
                Err(Openat2::Missing) => {}
                Err(Openat2::Failed(error)) => return Err(error),
                Ok(fd) => return Ok(File::from(fd)),
            }
        }
        let dir = walk(root, parents, false)?;
        open_at(dir.as_raw_fd(), name, access.flags()).map(File::from)
    }

    #[cfg(target_os = "linux")]
    enum Openat2 {
        /// The kernel has no `openat2`, or a filter refused it.
        Missing,
        Failed(io::Error),
    }

    /// [`openat2`] for a target under `root`, except where a test has made
    /// the kernel look as if it lacked it.
    #[cfg(target_os = "linux")]
    fn openat2_unless_disabled(
        root: &Path,
        dir: RawFd,
        path: &OsStr,
        flags: libc::c_int,
        mode: u64,
        resolve: u64,
    ) -> Result<OwnedFd, Openat2> {
        #[cfg(test)]
        if super::super::fs::seam::openat2_disabled(root) {
            return Err(Openat2::Missing);
        }
        #[cfg(not(test))]
        let _ = root;
        openat2(dir, path, flags, mode, resolve)
    }

    /// `openat2(dir, path, flags | O_CLOEXEC, mode, resolve)`; a link or an
    /// escape met during resolution is [`super::LinkRefused`].
    #[cfg(target_os = "linux")]
    fn openat2(
        dir: RawFd,
        path: &OsStr,
        flags: libc::c_int,
        mode: u64,
        resolve: u64,
    ) -> Result<OwnedFd, Openat2> {
        let path = c_name(path).map_err(Openat2::Failed)?;
        // SAFETY: `open_how` is plain old data; zeroed is valid and is how
        // the kernel expects unused fields.
        let mut how: libc::open_how = unsafe { std::mem::zeroed() };
        how.flags = (flags | libc::O_CLOEXEC) as u64;
        how.mode = mode;
        how.resolve = resolve;
        // SAFETY: `path` is NUL-terminated and `how` is a valid `open_how`
        // of the size passed; both outlive the call.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                dir,
                path.as_ptr(),
                &how as *const libc::open_how,
                std::mem::size_of::<libc::open_how>(),
            )
        };
        if fd < 0 {
            let error = last_error();
            return Err(match error.raw_os_error() {
                Some(libc::ENOSYS | libc::EPERM) => Openat2::Missing,
                Some(libc::ELOOP | libc::EXDEV) => Openat2::Failed(link_refused()),
                _ => Openat2::Failed(error),
            });
        }
        let Ok(fd) = RawFd::try_from(fd) else {
            return Err(Openat2::Failed(io::Error::from(io::ErrorKind::InvalidData)));
        };
        // SAFETY: `fd` is a fresh descriptor this function owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

#[cfg(windows)]
mod imp {
    //! The best-effort Windows walk; see the module docs.

    use std::ffi::{OsStr, OsString};
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};

    use super::link_refused;

    /// `FILE_FLAG_OPEN_REPARSE_POINT`: open a link itself, not its target.
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    #[derive(Clone, Copy)]
    pub(super) enum Access {
        Read,
        ReadRegular,
        Create,
    }

    /// Refuses when `path` is a link or junction; a missing entry passes.
    fn not_a_link(path: &Path) -> io::Result<()> {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => Err(link_refused()),
            _ => Ok(()),
        }
    }

    /// Checks the root's own path and then each parent, making missing
    /// ones when `create` is set; returns the parent directory.
    fn walk(root: &Path, parents: &[OsString], create: bool) -> io::Result<PathBuf> {
        let mut dir = PathBuf::new();
        for component in root.components() {
            dir.push(component.as_os_str());
            if let std::path::Component::Normal(_) = component {
                not_a_link(&dir)?;
            }
        }
        for name in parents {
            dir.push(name);
            not_a_link(&dir)?;
            if create {
                match std::fs::create_dir(&dir) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
                not_a_link(&dir)?;
            }
        }
        Ok(dir)
    }

    pub(super) fn make_dirs(root: &Path, parents: &[OsString]) -> io::Result<()> {
        walk(root, parents, true).map(drop)
    }

    pub(super) fn open(
        root: &Path,
        parents: &[OsString],
        name: &OsStr,
        access: Access,
    ) -> io::Result<File> {
        let path = walk(root, parents, false)?.join(name);
        let mut options = OpenOptions::new();
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        match access {
            Access::Read | Access::ReadRegular => options.read(true),
            // Truncated below, once the handle is known not to be a link.
            Access::Create => options.write(true).create(true),
        };
        let file = options.open(&path)?;
        if file.metadata()?.file_type().is_symlink() {
            return Err(link_refused());
        }
        if let Access::Create = access {
            file.set_len(0)?;
        }
        Ok(file)
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    //! No link-safe open exists here: every write is refused.

    use std::ffi::{OsStr, OsString};
    use std::fs::File;
    use std::io;
    use std::path::Path;

    #[derive(Clone, Copy)]
    pub(super) enum Access {
        Read,
        ReadRegular,
        Create,
    }

    pub(super) fn make_dirs(_root: &Path, _parents: &[OsString]) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    pub(super) fn open(
        _root: &Path,
        _parents: &[OsString],
        _name: &OsStr,
        _access: Access,
    ) -> io::Result<File> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}
