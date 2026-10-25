//! What every release step shares: errors as messages, hashing, running a
//! tool, and the repository's paths.

use std::ffi::OsStr;
use std::fs::File;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use ring::digest::{Context, SHA256, digest};

/// A step's failure, one or more lines a person reads.
pub type Result<T> = std::result::Result<T, String>;

/// The repository this tool was built from: two directories above the
/// crate, where the workspace `Cargo.toml` is.
pub fn repository_root() -> PathBuf {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| crate_dir.to_path_buf(), Path::to_path_buf)
}

/// Lowercase hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// SHA-256 of `bytes`, as lowercase hex.
pub fn sha256(bytes: &[u8]) -> String {
    hex(digest(&SHA256, bytes).as_ref())
}

/// SHA-256 and size of the file at `path`, read in chunks.
pub fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let mut file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut context = Context::new(&SHA256);
    let mut buffer = vec![0u8; 1 << 16];
    let mut size: u64 = 0;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        context.update(&buffer[..read]);
        size += read as u64;
    }
    Ok((hex(context.finish().as_ref()), size))
}

/// Reads `path` as UTF-8 text.
pub fn read_text(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    String::from_utf8(bytes).map_err(|_| format!("{} is not UTF-8 text", path.display()))
}

/// Runs `program` with `args` in `cwd` and returns its standard output.
/// The environment is inherited, so rustup selects the pinned toolchain.
pub fn run<S: AsRef<OsStr>>(program: &str, args: &[S], cwd: &Path) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("{program} could not start: {error}"))?;
    if !output.status.success() {
        let shown: Vec<String> = args
            .iter()
            .map(|arg| arg.as_ref().to_string_lossy().into_owned())
            .collect();
        return Err(format!(
            "{program} {} exited {}:\n{}",
            shown.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|_| format!("{program} printed text that is not UTF-8"))
}

/// Whether `child` lies strictly inside `parent`, by components.
pub fn inside(parent: &Path, child: &Path) -> bool {
    child != parent && child.starts_with(parent)
}

/// `path` with its deepest existing ancestor resolved through links and
/// the rest, which does not exist yet, appended.
///
/// # Errors
///
/// When an ancestor is a link whose target does not exist, which a later
/// `create_dir_all` would follow.
pub fn canonical_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("the working directory: {error}"))?
            .join(path)
    };
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    let mut current = normalize(&absolute);
    loop {
        if let Ok(real) = std::fs::canonicalize(&current) {
            let mut out = real;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return Ok(out);
        }
        if std::fs::symlink_metadata(&current).is_ok() {
            return Err(format!(
                "{} is a link to a path that does not exist",
                current.display()
            ));
        }
        let Some(name) = current.file_name().map(ToOwned::to_owned) else {
            return Ok(absolute);
        };
        rest.push(name);
        if !current.pop() {
            return Ok(absolute);
        }
    }
}

/// `path` with `.` and `..` components resolved lexically.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// A private directory under the system temporary directory, removed when
/// dropped.
#[derive(Debug)]
pub struct Scratch {
    path: PathBuf,
}

impl Scratch {
    /// Creates `<tmp>/<prefix>-<pid>-<random>`.
    pub fn new(prefix: &str) -> Result<Self> {
        use ring::rand::SecureRandom as _;
        let mut random = [0u8; 8];
        ring::rand::SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| "no randomness for a temporary directory name".to_owned())?;
        let path =
            std::env::temp_dir().join(format!("{prefix}-{}-{}", std::process::id(), hex(&random)));
        std::fs::create_dir(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_and_paths() {
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(inside(Path::new("/a/b"), Path::new("/a/b/c")));
        assert!(!inside(Path::new("/a/b"), Path::new("/a/b")));
        assert!(!inside(Path::new("/a/b"), Path::new("/a/bc")));
        assert_eq!(normalize(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
    }

    #[test]
    fn a_scratch_directory_is_removed_when_dropped() {
        let scratch = Scratch::new("bake-release-test").expect("scratch");
        let path = scratch.path().to_path_buf();
        std::fs::write(path.join("f"), b"x").expect("write");
        drop(scratch);
        assert!(!path.exists());
    }
}
