//! Node's file-system error text, which Pi's tools pass to the model: the
//! libuv code (`ENOENT`) and messages such as
//! `ENOENT: no such file or directory, open '/x'`.

use std::io::{self, ErrorKind};
use std::path::Path;

/// The libuv code Node gives an I/O error, `UNKNOWN` when none applies.
pub(crate) fn code(error: &io::Error) -> &'static str {
    #[cfg(unix)]
    if let Some(errno) = error.raw_os_error() {
        let code = match errno {
            libc::EPERM => "EPERM",
            libc::ENOENT => "ENOENT",
            libc::EIO => "EIO",
            libc::EBADF => "EBADF",
            libc::EAGAIN => "EAGAIN",
            libc::ENOMEM => "ENOMEM",
            libc::EACCES => "EACCES",
            libc::EBUSY => "EBUSY",
            libc::EEXIST => "EEXIST",
            libc::EXDEV => "EXDEV",
            libc::ENOTDIR => "ENOTDIR",
            libc::EISDIR => "EISDIR",
            libc::EINVAL => "EINVAL",
            libc::ENFILE => "ENFILE",
            libc::EMFILE => "EMFILE",
            libc::ETXTBSY => "ETXTBSY",
            libc::EFBIG => "EFBIG",
            libc::ENOSPC => "ENOSPC",
            libc::EROFS => "EROFS",
            libc::EMLINK => "EMLINK",
            libc::ENAMETOOLONG => "ENAMETOOLONG",
            libc::ENOTEMPTY => "ENOTEMPTY",
            libc::ELOOP => "ELOOP",
            _ => "",
        };
        if !code.is_empty() {
            return code;
        }
    }
    match error.kind() {
        ErrorKind::NotFound => "ENOENT",
        // libuv reports Windows' access-denied errors as EPERM.
        ErrorKind::PermissionDenied if cfg!(windows) => "EPERM",
        ErrorKind::PermissionDenied => "EACCES",
        ErrorKind::AlreadyExists => "EEXIST",
        ErrorKind::IsADirectory => "EISDIR",
        ErrorKind::NotADirectory => "ENOTDIR",
        ErrorKind::DirectoryNotEmpty => "ENOTEMPTY",
        ErrorKind::ReadOnlyFilesystem => "EROFS",
        ErrorKind::InvalidFilename => "ENAMETOOLONG",
        ErrorKind::ResourceBusy => "EBUSY",
        ErrorKind::StorageFull => "ENOSPC",
        ErrorKind::InvalidInput => "EINVAL",
        ErrorKind::OutOfMemory => "ENOMEM",
        _ => "UNKNOWN",
    }
}

fn description(code: &str) -> &'static str {
    match code {
        "EPERM" => "operation not permitted",
        "ENOENT" => "no such file or directory",
        "EIO" => "i/o error",
        "EBADF" => "bad file descriptor",
        "EAGAIN" => "resource temporarily unavailable",
        "ENOMEM" => "not enough memory",
        "EACCES" => "permission denied",
        "EBUSY" => "resource busy or locked",
        "EEXIST" => "file already exists",
        "EXDEV" => "cross-device link not permitted",
        "ENOTDIR" => "not a directory",
        "EISDIR" => "illegal operation on a directory",
        "EINVAL" => "invalid argument",
        "ENFILE" => "file table overflow",
        "EMFILE" => "too many open files",
        "ETXTBSY" => "text file is busy",
        "EFBIG" => "file too large",
        "ENOSPC" => "no space left on device",
        "EROFS" => "read-only file system",
        "EMLINK" => "too many links",
        "ENAMETOOLONG" => "name too long",
        "ENOTEMPTY" => "directory not empty",
        "ELOOP" => "too many symbolic links encountered",
        _ => "unknown error",
    }
}

/// Node's message for a failed file-system call: `CODE: description,
/// syscall 'path'`, or without the path for calls on a handle.
pub(crate) fn message(error: &io::Error, syscall: &str, path: Option<&Path>) -> String {
    let code = code(error);
    let mut text = format!("{code}: {}, {syscall}", description(code));
    if let Some(path) = path {
        text.push_str(&format!(" '{}'", path.display()));
    }
    text
}

/// `fs.access` with `R_OK`, and with `W_OK` when `write` is set.
pub(crate) fn access(path: &Path, write: bool) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let bytes = path.as_os_str().as_bytes();
        let Ok(c_path) = std::ffi::CString::new(bytes) else {
            return Err(io::Error::from(ErrorKind::NotFound));
        };
        let mode = if write {
            libc::R_OK | libc::W_OK
        } else {
            libc::R_OK
        };
        // SAFETY: `c_path` is a valid NUL-terminated string that outlives the call.
        let result = unsafe { libc::access(c_path.as_ptr(), mode) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(unix))]
    {
        // Node on Windows checks existence, and for W_OK the read-only bit.
        let metadata = std::fs::metadata(path)?;
        if write && !metadata.is_dir() && metadata.permissions().readonly() {
            return Err(io::Error::from(ErrorKind::PermissionDenied));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_node_messages() {
        let missing = io::Error::from(ErrorKind::NotFound);
        assert_eq!(
            message(&missing, "access", Some(Path::new("/x/y"))),
            "ENOENT: no such file or directory, access '/x/y'"
        );
        let directory = io::Error::from(ErrorKind::IsADirectory);
        assert_eq!(
            message(&directory, "read", None),
            "EISDIR: illegal operation on a directory, read"
        );
    }
}
