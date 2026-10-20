//! Unpacking a release archive, in process.
//!
//! The archive is `scripts/release/pack-rust.ts`'s layout: `bin/bake`
//! (`bin/bake.exe` on Windows, or `bin/bake-rs` from an archive built
//! before the command was renamed), `LICENSE`, `THIRD_PARTY_NOTICES.md`,
//! `notices/`, and `licenses/`. Only regular files and directories are
//! unpacked: a member that is absolute, holds `..`, or is a link, device,
//! or other special file is refused, and so is an archive whose members
//! together exceed [`MAX_UNPACKED_BYTES`].
//!
//! The archive is read twice: once to check every member and add up what
//! it would write, and again to write it. Nothing is written until the
//! first pass has accepted the whole archive.

use std::fs::{self, File};
use std::io;
#[cfg(test)]
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use flate2::read::GzDecoder;
use tar::{Archive, EntryType};

use crate::error::UpdateError;

/// The most an archive may unpack to. A native release is tens of
/// megabytes; this bound stops a decompression bomb long before the disk
/// fills.
pub const MAX_UNPACKED_BYTES: u64 = 512 * 1024 * 1024;

/// Unpack `archive` into `into`, which the call creates.
///
/// # Errors
///
/// [`UpdateError::Archive`] for an unreadable gzip or tar stream, a
/// refused member, or an archive that unpacks to more than
/// [`MAX_UNPACKED_BYTES`]; [`UpdateError::Install`] when a file cannot be
/// written.
pub fn extract(archive: &Path, into: &Path) -> Result<(), UpdateError> {
    extract_within(archive, into, MAX_UNPACKED_BYTES)
}

/// [`extract`] with the unpacked-size bound named, so a test can reach
/// the refusal without writing half a gigabyte.
pub fn extract_within(archive: &Path, into: &Path, max_unpacked: u64) -> Result<(), UpdateError> {
    if read_members(archive, max_unpacked)? == 0 {
        return Err(UpdateError::Archive(
            "the release archive is empty".to_owned(),
        ));
    }
    fs::create_dir_all(into).map_err(|error| {
        UpdateError::Install(format!("could not create {}: {error}", into.display()))
    })?;
    write_members(archive, into)
}

/// Check every member, and add up the bytes the archive would write.
/// Returns how many members it accepted.
fn read_members(archive: &Path, max_unpacked: u64) -> Result<usize, UpdateError> {
    let mut reader = open(archive)?;
    let mut count = 0;
    let mut total: u64 = 0;
    for entry in reader.entries().map_err(read_failure)? {
        let entry = entry.map_err(read_failure)?;
        let path = member_path(entry.path())?;
        let size = match entry.header().entry_type() {
            EntryType::Regular | EntryType::Continuous => entry.header().size().unwrap_or(0),
            EntryType::Directory => 0,
            _ => {
                return Err(UpdateError::Archive(format!(
                    "the release archive's '{}' is not a regular file or directory",
                    path.display()
                )));
            }
        };
        total = total.saturating_add(size);
        if total > max_unpacked {
            return Err(UpdateError::Archive(format!(
                "the release archive unpacks to more than {} MiB",
                max_unpacked / (1024 * 1024)
            )));
        }
        count += 1;
    }
    Ok(count)
}

/// Write the members the first pass accepted.
fn write_members(archive: &Path, into: &Path) -> Result<(), UpdateError> {
    let mut reader = open(archive)?;
    for entry in reader.entries().map_err(read_failure)? {
        let mut entry = entry.map_err(read_failure)?;
        let relative = member_path(entry.path())?;
        let destination = into.join(&relative);
        if entry.header().entry_type() == EntryType::Directory {
            create_dir(&destination)?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            create_dir(parent)?;
        }
        let mut file = File::create(&destination).map_err(|error| {
            UpdateError::Install(format!(
                "could not write {}: {error}",
                destination.display()
            ))
        })?;
        io::copy(&mut entry, &mut file).map_err(|error| match error.kind() {
            io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidData => read_failure(error),
            _ => UpdateError::Install(format!(
                "could not write {}: {error}",
                destination.display()
            )),
        })?;
        set_mode(&destination, entry.header().mode().unwrap_or(0o644))?;
    }
    Ok(())
}

fn open(archive: &Path) -> Result<Archive<GzDecoder<File>>, UpdateError> {
    let file = File::open(archive).map_err(|error| {
        UpdateError::Install(format!("could not read {}: {error}", archive.display()))
    })?;
    Ok(Archive::new(GzDecoder::new(file)))
}

fn read_failure(error: io::Error) -> UpdateError {
    UpdateError::Archive(format!("the release archive could not be read: {error}"))
}

/// A member's path inside the release, refusing anything that could write
/// outside it.
fn member_path(path: io::Result<std::borrow::Cow<'_, Path>>) -> Result<PathBuf, UpdateError> {
    let path = path.map_err(|_| {
        UpdateError::Archive("the release archive has a member with an unreadable name".to_owned())
    })?;
    let refuse = |reason: &str| {
        Err(UpdateError::Archive(format!(
            "the release archive has a member whose name {reason}"
        )))
    };
    let mut safe = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                if part.to_str().is_none() {
                    return refuse("is not valid UTF-8");
                }
                safe.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir => return refuse("leaves the release directory"),
            Component::RootDir | Component::Prefix(_) => return refuse("is absolute"),
        }
    }
    if safe.as_os_str().is_empty() {
        return refuse("is empty");
    }
    Ok(safe)
}

fn create_dir(path: &Path) -> Result<(), UpdateError> {
    fs::create_dir_all(path).map_err(|error| {
        UpdateError::Install(format!("could not create {}: {error}", path.display()))
    })
}

/// Keep an archived executable executable, and nothing group- or
/// other-writable. Windows has no mode to set.
#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), UpdateError> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = if mode & 0o111 == 0 { 0o644 } else { 0o755 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|error| {
        UpdateError::Install(format!(
            "could not set the mode of {}: {error}",
            path.display()
        ))
    })
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), UpdateError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::tests::temp_dir;

    /// A gzipped tar of `(path, bytes)` members, as a release archive
    /// holds them: the binary executable, everything else read-only.
    pub fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (path, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(if path.starts_with("bin/") {
                0o755
            } else {
                0o644
            });
            builder
                .append_data(&mut header, path, *bytes)
                .expect("a tar member");
        }
        builder
            .into_inner()
            .expect("the tar")
            .finish()
            .expect("the gzip")
    }

    /// A gzipped tar whose member names are written into the headers as
    /// they are, which `tar::Builder` refuses to do: a hostile archive
    /// names `../` and absolute paths.
    fn hostile_tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut blocks: Vec<u8> = Vec::new();
        for (path, bytes) in files {
            let mut header = tar::Header::new_ustar();
            header.set_mode(0o644);
            header.set_uid(0);
            header.set_gid(0);
            header.set_mtime(0);
            header.set_entry_type(EntryType::Regular);
            header.set_size(bytes.len() as u64);
            let name = path.as_bytes();
            assert!(name.len() <= 100, "{path}");
            header.as_mut_bytes()[..name.len()].copy_from_slice(name);
            header.set_cksum();
            blocks.extend_from_slice(header.as_bytes());
            blocks.extend_from_slice(bytes);
            blocks.extend(std::iter::repeat_n(0u8, (512 - bytes.len() % 512) % 512));
        }
        blocks.extend(std::iter::repeat_n(0u8, 1024));
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&blocks).expect("the gzip");
        encoder.finish().expect("the gzip")
    }

    #[test]
    fn an_archive_unpacks_its_files_with_their_modes() {
        let dir = temp_dir("archive-ok");
        let archive = dir.path().join("release.tar.gz");
        fs::write(
            &archive,
            tar_gz(&[("bin/bake", b"#!/bin/sh\n"), ("LICENSE", b"MIT\n")]),
        )
        .expect("archive");
        let into = dir.path().join("unpacked");
        extract(&archive, &into).expect("unpacked");
        assert_eq!(
            fs::read_to_string(into.join("LICENSE")).expect("license"),
            "MIT\n"
        );
        assert!(crate::layout::executable(&into).is_some());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = |name: &str| {
                fs::metadata(into.join(name))
                    .expect(name)
                    .permissions()
                    .mode()
                    & 0o777
            };
            assert_eq!(mode("bin/bake"), 0o755);
            assert_eq!(mode("LICENSE"), 0o644);
        }
    }

    #[test]
    fn an_unsafe_member_is_refused_before_anything_is_written() {
        let dir = temp_dir("archive-unsafe");
        let into = dir.path().join("unpacked");
        for (name, reason) in [
            ("../escape", "leaves the release directory"),
            ("bin/../../escape", "leaves the release directory"),
            ("/etc/passwd", "is absolute"),
        ] {
            let archive = dir.path().join("release.tar.gz");
            fs::write(
                &archive,
                hostile_tar_gz(&[("bin/bake", b"x"), (name, b"bad")]),
            )
            .expect("archive");
            let error = extract(&archive, &into).expect_err(name);
            assert!(error.message().contains(reason), "{error} for {name}");
            assert!(!into.exists(), "{name} wrote something");
        }
    }

    #[test]
    fn a_link_member_is_refused() {
        let dir = temp_dir("archive-link");
        let archive = dir.path().join("release.tar.gz");
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o777);
        header.set_entry_type(EntryType::Symlink);
        builder
            .append_link(&mut header, "bin/bake", "/bin/sh")
            .expect("a link member");
        let bytes = builder
            .into_inner()
            .expect("the tar")
            .finish()
            .expect("the gzip");
        fs::write(&archive, bytes).expect("archive");
        let into = dir.path().join("unpacked");
        let error = extract(&archive, &into).expect_err("a link");
        assert!(
            error.message().contains("not a regular file or directory"),
            "{error}"
        );
        assert!(!into.exists());
    }

    #[test]
    fn an_archive_that_unpacks_past_the_bound_is_refused_before_writing() {
        let dir = temp_dir("archive-bound");
        let archive = dir.path().join("release.tar.gz");
        fs::write(
            &archive,
            tar_gz(&[("bin/bake", &[b'x'; 2048]), ("LICENSE", b"MIT\n")]),
        )
        .expect("archive");
        let into = dir.path().join("unpacked");
        let error = extract_within(&archive, &into, 1024).expect_err("too large");
        assert!(error.message().contains("unpacks to more than"), "{error}");
        assert!(!into.exists());
        // The shipped bound is the one the contract names.
        assert_eq!(MAX_UNPACKED_BYTES, 512 * 1024 * 1024);
        extract_within(&archive, &into, MAX_UNPACKED_BYTES).expect("within the bound");
    }

    #[test]
    fn a_truncated_or_corrupt_archive_is_refused() {
        let dir = temp_dir("archive-truncated");
        let archive = dir.path().join("release.tar.gz");
        let whole = tar_gz(&[("bin/bake", &[b'x'; 4096])]);
        fs::write(&archive, &whole[..whole.len() / 2]).expect("archive");
        let into = dir.path().join("unpacked");
        let error = extract(&archive, &into).expect_err("truncated");
        assert!(error.message().contains("could not be read"), "{error}");
        fs::write(&archive, b"not a gzip stream").expect("archive");
        let error = extract(&archive, &into).expect_err("corrupt");
        assert!(error.message().contains("could not be read"), "{error}");
    }

    #[test]
    fn an_empty_archive_is_refused() {
        let dir = temp_dir("archive-empty");
        let archive = dir.path().join("release.tar.gz");
        fs::write(&archive, tar_gz(&[])).expect("archive");
        let error = extract(&archive, &dir.path().join("unpacked")).expect_err("empty");
        assert!(error.message().contains("is empty"), "{error}");
    }
}
