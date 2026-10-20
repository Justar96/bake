//! Where a native install lives, and how its pointer moves.
//!
//! ```text
//! <root>/releases/<version>/   one unpacked archive each
//! <root>/current              Unix: a relative link to releases/<version>
//! <root>/current.txt          Windows: that version and a CRLF
//! <root>/previous             the release a rollback returns to
//! <root>/install.lock         held by every install, update, and rollback
//! <root>/links.json           the command links the installer made
//! ```
//!
//! `<bin>/bake` links to `<root>/current/bin/bake` on Unix, so an update
//! moves `current` and nothing else; on Windows `bake.cmd` reads
//! `current.txt` on every run. A running process has already resolved its
//! own release directory, so it keeps running while the pointer moves.
//!
//! The root is separate from Bake 0.3's (`~/.local/share/bake`,
//! `%LOCALAPPDATA%\Bake`) and from its `BAKE_INSTALL_ROOT`: 0.4 installs
//! beside a 0.3 install and never reads its layout ([D32], [D45]).
//! Bake 0.3's `packages/boot/updater/src/layout.ts` is the reference for
//! the pointer forms and for reading them without trusting their contents.
//!
//! [D32]: ../../../docs/roadmap/rust-0.4/scope-00/support.md
//! [D45]: ../../../docs/roadmap/rust-0.4/scope-00/support.md

use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::error::UpdateError;
use crate::version::is_release_version;

/// The directory holding one unpacked archive per version.
pub const RELEASES: &str = "releases";

/// The environment setting that names the native install root.
pub const ROOT_ENV: &str = "BAKE_NATIVE_ROOT";

/// The environment setting that names the directory holding the commands.
pub const BIN_ENV: &str = "BAKE_BIN_DIR";

/// How a root records which release is current.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pointer {
    /// Unix: `current` and `previous`, relative symbolic links.
    Link,
    /// Windows: `current.txt` and `previous.txt`, each the version and a
    /// CRLF, as `install.ps1` and the 0.3 updater write it, because
    /// `bake.cmd` reads it with `set /p`. Tests use this form on any host.
    File,
}

impl Pointer {
    /// The form this host installs with.
    pub fn host() -> Self {
        if cfg!(windows) {
            Self::File
        } else {
            Self::Link
        }
    }

    fn name(self, pointer: Name) -> &'static str {
        match (self, pointer) {
            (Self::Link, Name::Current) => "current",
            (Self::Link, Name::Previous) => "previous",
            (Self::File, Name::Current) => "current.txt",
            (Self::File, Name::Previous) => "previous.txt",
        }
    }
}

/// Which release a pointer names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Name {
    Current,
    Previous,
}

/// A native install root.
#[derive(Debug, Clone)]
pub struct Root {
    path: PathBuf,
    pointer: Pointer,
}

impl Root {
    /// A root at `path` using this host's pointer form.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            pointer: Pointer::host(),
        }
    }

    /// A root using a named pointer form, so a test can exercise the
    /// Windows form on any host.
    pub fn with_pointer(path: impl Into<PathBuf>, pointer: Pointer) -> Self {
        Self {
            path: path.into(),
            pointer,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn pointer_form(&self) -> Pointer {
        self.pointer
    }

    /// `<root>/releases`.
    pub fn releases(&self) -> PathBuf {
        self.path.join(RELEASES)
    }

    /// `<root>/releases/<version>`.
    pub fn release(&self, version: &str) -> PathBuf {
        self.releases().join(version)
    }

    /// The path of one pointer.
    pub fn pointer_path(&self, name: Name) -> PathBuf {
        self.path.join(self.pointer.name(name))
    }

    /// The version a pointer names, or `None` when it names none, names
    /// something that is not a release version, or cannot be read.
    pub fn version(&self, name: Name) -> Option<String> {
        let path = self.pointer_path(name);
        let text = match self.pointer {
            Pointer::Link => {
                let target = fs::read_link(&path).ok()?;
                target
                    .components()
                    .next_back()
                    .and_then(|last| match last {
                        Component::Normal(name) => name.to_str().map(str::to_owned),
                        _ => None,
                    })?
            }
            Pointer::File => fs::read_to_string(&path).ok()?.trim().to_owned(),
        };
        is_release_version(&text).then_some(text)
    }

    /// The release the next command runs.
    pub fn current(&self) -> Option<String> {
        self.version(Name::Current)
    }

    /// The release a rollback returns to; `None` when it is unset, equal
    /// to current (an interrupted switch), or no longer on disk.
    pub fn previous(&self) -> Option<String> {
        let previous = self.version(Name::Previous)?;
        if Some(&previous) == self.current().as_ref() {
            return None;
        }
        executable(&self.release(&previous))
            .is_some()
            .then_some(previous)
    }

    /// Point `name` at `version`, atomically: a temporary link or file
    /// beside it, then a rename, so a reader sees the old release or the
    /// new one and never neither.
    ///
    /// # Errors
    ///
    /// [`UpdateError::Install`] when the pointer cannot be written, or
    /// when an existing pointer is a plain file where a link belongs,
    /// which no Bake installer makes.
    pub fn point_at(&self, name: Name, version: &str) -> Result<(), UpdateError> {
        let path = self.pointer_path(name);
        if self.pointer == Pointer::Link
            && fs::symlink_metadata(&path).is_ok_and(|meta| !meta.file_type().is_symlink())
        {
            return Err(UpdateError::Install(format!(
                "{} exists and is not a Bake-managed link",
                path.display()
            )));
        }
        let temp = self.path.join(format!(
            ".{}.{}",
            self.pointer.name(name),
            crate::update::unique()
        ));
        let written = match self.pointer {
            Pointer::Link => write_link(&format!("{RELEASES}/{version}"), &temp),
            Pointer::File => fs::write(&temp, format!("{version}\r\n")),
        };
        written.map_err(|error| {
            UpdateError::Install(format!("could not write {}: {error}", temp.display()))
        })?;
        if let Err(error) = fs::rename(&temp, &path) {
            let _ = fs::remove_file(&temp);
            return Err(UpdateError::Install(format!(
                "could not move {} into place: {error}",
                path.display()
            )));
        }
        Ok(())
    }

    /// Remove what an interrupted run left behind: a temporary pointer
    /// that was never renamed, and a staging directory under `releases`.
    /// Called under the lock, so nothing else is writing them. Every
    /// failure is ignored; the files are only clutter.
    pub fn sweep(&self) {
        if let Ok(entries) = fs::read_dir(&self.path) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if name.starts_with(".current") || name.starts_with(".previous") {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        if let Ok(entries) = fs::read_dir(self.releases()) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if name.starts_with(".staging-") {
                    let _ = fs::remove_dir_all(entry.path());
                }
            }
        }
    }

    /// The versions under `releases/`, in no order, skipping staging
    /// directories and anything that is not named for a release.
    pub fn installed(&self) -> Vec<String> {
        let mut versions = Vec::new();
        let Ok(entries) = fs::read_dir(self.releases()) else {
            return versions;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if is_release_version(name) && entry.path().is_dir() {
                versions.push(name.to_owned());
            }
        }
        versions
    }
}

#[cfg(unix)]
fn write_link(target: &str, at: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, at)
}

#[cfg(not(unix))]
fn write_link(_target: &str, _at: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "a native install on this platform points with current.txt",
    ))
}

/// A release's own executable: `bin/bake`, and `bin/bake-rs` for an
/// archive built before the command was renamed. `None` when the release
/// holds neither.
pub fn executable(release: &Path) -> Option<PathBuf> {
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    for name in ["bake", "bake-rs"] {
        let path = release.join("bin").join(format!("{name}{suffix}"));
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

/// The install root this executable runs from, or the one
/// `BAKE_NATIVE_ROOT` names.
///
/// A native install runs `<root>/releases/<version>/bin/<command>`, so the
/// root is three directories above the executable, and only when the
/// directory above the release is `releases`. Anything else — a source
/// build, or a copy someone unpacked by hand — has no root, and the
/// updater leaves it alone.
pub fn detect_root(
    env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    executable: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(named) = env(ROOT_ENV).filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(named));
    }
    let release = executable?.parent()?.parent()?;
    let releases = release.parent()?;
    if releases.file_name() != Some(OsStr::new(RELEASES)) {
        return None;
    }
    releases.parent().map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use crate::update::tests::temp_dir;

    #[test]
    fn a_file_pointer_holds_the_version_and_a_newline() {
        let dir = temp_dir("layout-file");
        let root = Root::with_pointer(dir.path(), Pointer::File);
        fs::create_dir_all(root.releases()).expect("releases");
        assert_eq!(root.current(), None);
        root.point_at(Name::Current, "0.4.0").expect("current");
        // CRLF, the line ending install.ps1 writes and `bake.cmd` reads.
        assert_eq!(
            fs::read_to_string(root.pointer_path(Name::Current)).expect("pointer"),
            "0.4.0\r\n"
        );
        assert_eq!(root.current(), Some("0.4.0".to_owned()));
        root.point_at(Name::Current, "0.4.1").expect("replaced");
        assert_eq!(root.current(), Some("0.4.1".to_owned()));
        // Anything that is not a release version names no release.
        fs::write(root.pointer_path(Name::Current), "../etc\n").expect("write");
        assert_eq!(root.current(), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_pointer_is_relative_and_replaced_atomically() {
        let dir = temp_dir("layout-link");
        let root = Root::new(dir.path());
        fs::create_dir_all(root.release("0.4.0")).expect("release");
        root.point_at(Name::Current, "0.4.0").expect("current");
        assert_eq!(
            fs::read_link(root.pointer_path(Name::Current)).expect("link"),
            PathBuf::from("releases/0.4.0")
        );
        assert_eq!(root.current(), Some("0.4.0".to_owned()));
        root.point_at(Name::Current, "0.4.1").expect("replaced");
        assert_eq!(root.current(), Some("0.4.1".to_owned()));
        // A plain file where the link belongs is not a Bake install.
        fs::remove_file(root.pointer_path(Name::Current)).expect("remove");
        fs::write(root.pointer_path(Name::Current), "0.4.1\n").expect("write");
        let error = root
            .point_at(Name::Current, "0.4.2")
            .expect_err("not a link");
        assert!(
            error.message().contains("not a Bake-managed link"),
            "{error}"
        );
    }

    #[test]
    fn sweep_removes_an_interrupted_runs_leftovers_only() {
        let dir = temp_dir("layout-sweep");
        let root = Root::with_pointer(dir.path(), Pointer::File);
        fs::create_dir_all(root.release("0.4.0")).expect("release");
        fs::create_dir_all(root.releases().join(".staging-abcd")).expect("staging");
        fs::write(root.path().join(".current.txt.1-ab"), "0.4.1\n").expect("temp");
        root.point_at(Name::Current, "0.4.0").expect("current");
        root.sweep();
        assert!(!root.path().join(".current.txt.1-ab").exists());
        assert!(!root.releases().join(".staging-abcd").exists());
        assert!(root.release("0.4.0").is_dir());
        assert_eq!(root.current(), Some("0.4.0".to_owned()));
        assert_eq!(root.installed(), vec!["0.4.0".to_owned()]);
    }

    #[test]
    fn the_root_comes_from_the_environment_or_the_executables_place() {
        let none = |_: &str| None;
        let named = |name: &str| (name == ROOT_ENV).then(|| OsString::from("/opt/bake-rs"));
        assert_eq!(
            detect_root(&named, Some(Path::new("/elsewhere/bin/bake"))),
            Some(PathBuf::from("/opt/bake-rs"))
        );
        assert_eq!(
            detect_root(
                &none,
                Some(Path::new(
                    "/home/a/.local/share/bake-rs/releases/0.4.0/bin/bake"
                ))
            ),
            Some(PathBuf::from("/home/a/.local/share/bake-rs"))
        );
        for path in [
            "/home/a/src/bake/rust/target/debug/bake-rs",
            "/usr/local/bin/bake",
            "bake",
        ] {
            assert_eq!(detect_root(&none, Some(Path::new(path))), None, "{path}");
        }
        assert_eq!(detect_root(&none, None), None);
    }

    #[test]
    fn a_release_is_found_under_either_command_name() {
        let dir = temp_dir("layout-exe");
        let release = dir.path().join("0.4.0");
        fs::create_dir_all(release.join("bin")).expect("bin");
        assert_eq!(executable(&release), None);
        let old = release.join("bin").join(if cfg!(windows) {
            "bake-rs.exe"
        } else {
            "bake-rs"
        });
        fs::write(&old, "").expect("write");
        assert_eq!(executable(&release), Some(old));
        let new = release
            .join("bin")
            .join(if cfg!(windows) { "bake.exe" } else { "bake" });
        fs::write(&new, "").expect("write");
        assert_eq!(executable(&release), Some(new));
    }
}
