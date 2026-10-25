//! Native release archives: a deterministic writer, and the reader that
//! tells a native archive's layout apart from anything else before it is
//! published.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read as _;
use std::path::Path;

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;

use crate::util::Result;

/// The executables a native archive may carry.
pub const NATIVE_EXECUTABLES: [&str; 2] = ["bin/bake", "bin/bake.exe"];
/// The license inventory every native archive carries.
pub const NATIVE_INVENTORY: &str = "licenses/inventory.json";
/// Where a native archive keeps the linked Bake crates' NOTICE files.
pub const NATIVE_NOTICES_DIR: &str = "notices/";
/// Files only a 0.3 TypeScript archive holds, which a native one must not.
pub const TYPESCRIPT_MARKERS: [&str; 2] = ["apps/cli/lib/bin.js", "package.json"];

/// The group and other write bits.
pub const SHARED_WRITE: u32 = 0o022;

/// One file of an archive: where its bytes come from and its mode.
#[derive(Debug, Clone)]
pub enum Source {
    Disk(std::path::PathBuf),
    Bytes(Vec<u8>),
}

/// A file to archive at a `/`-separated path.
#[derive(Debug, Clone)]
pub struct Member {
    pub path: String,
    pub mode: u32,
    pub source: Source,
}

/// Every directory the members' paths need.
pub fn directories(members: &[Member]) -> Vec<String> {
    let mut dirs = std::collections::BTreeSet::new();
    for member in members {
        let parts: Vec<&str> = member.path.split('/').collect();
        for end in 1..parts.len() {
            dirs.insert(parts[..end].join("/"));
        }
    }
    dirs.into_iter().collect()
}

/// Writes `members` as a gzipped tar at `out`: directories `0755`, each
/// file with its mode, owner 0, every time `mtime`, in path order with each
/// directory before what it holds, so the same inputs give the same bytes.
pub fn write_tar_gz(members: &[Member], out: &Path, mtime: u64) -> Result<()> {
    let file = File::create(out).map_err(|error| format!("{}: {error}", out.display()))?;
    let mut builder = tar::Builder::new(GzEncoder::new(file, Compression::default()));
    builder.mode(tar::HeaderMode::Deterministic);
    let mut entries: Vec<(String, Option<&Member>)> = directories(members)
        .into_iter()
        .map(|dir| (dir, None))
        .collect();
    entries.extend(
        members
            .iter()
            .map(|member| (member.path.clone(), Some(member))),
    );
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, member) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_mtime(mtime);
        header.set_uid(0);
        header.set_gid(0);
        match member {
            None => {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_mode(0o755);
                header.set_size(0);
                builder
                    .append_data(&mut header, format!("{path}/"), std::io::empty())
                    .map_err(|error| format!("{}: {error}", out.display()))?;
            }
            Some(member) => {
                let bytes = match &member.source {
                    Source::Bytes(bytes) => bytes.clone(),
                    Source::Disk(source) => {
                        let meta = std::fs::symlink_metadata(source)
                            .map_err(|error| format!("{}: {error}", source.display()))?;
                        if !meta.is_file() {
                            return Err(format!(
                                "{} is not a regular file; the archive takes regular files only",
                                source.display()
                            ));
                        }
                        std::fs::read(source)
                            .map_err(|error| format!("{}: {error}", source.display()))?
                    }
                };
                if bytes.is_empty() {
                    return Err(format!("{} is empty", member.path));
                }
                header.set_entry_type(tar::EntryType::Regular);
                header.set_mode(member.mode);
                header.set_size(bytes.len() as u64);
                builder
                    .append_data(&mut header, &member.path, bytes.as_slice())
                    .map_err(|error| format!("{}: {error}", out.display()))?;
            }
        }
    }
    builder
        .into_inner()
        .and_then(GzEncoder::finish)
        .map_err(|error| format!("{}: {error}", out.display()))?;
    Ok(())
}

/// One entry of an archive as read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub directory: bool,
    pub mode: u32,
}

/// What an archive holds.
#[derive(Debug, Clone)]
pub struct Layout {
    pub entries: Vec<Entry>,
    /// Entries that are links or devices, absolute, or leave the archive.
    pub refused: Vec<String>,
}

/// Reads every entry header of the gzipped tar at `path`.
pub fn read_layout(path: &Path) -> Result<Layout> {
    let file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut archive = tar::Archive::new(GzDecoder::new(file));
    let mut entries = Vec::new();
    let mut refused = Vec::new();
    let unreadable = |error: std::io::Error| {
        format!(
            "{} is not a readable gzipped tar archive: {error}",
            path.display()
        )
    };
    for entry in archive.entries().map_err(unreadable)? {
        let mut entry = entry.map_err(unreadable)?;
        let raw = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        let name = raw
            .trim_start_matches("./")
            .trim_end_matches('/')
            .to_owned();
        if name.is_empty() || name == "." {
            continue;
        }
        let kind = entry.header().entry_type();
        let mode = entry.header().mode().map_err(unreadable)?;
        if !(kind.is_file() || kind.is_dir()) {
            refused.push(format!("{name} (tar type {:?})", kind.as_byte() as char));
        } else if name.starts_with('/')
            || name.as_bytes().get(1) == Some(&b':')
            || name.split('/').any(|part| part == "..")
        {
            refused.push(name.clone());
        }
        // Drain the body so a truncated archive is noticed here.
        std::io::copy(&mut entry, &mut std::io::sink()).map_err(unreadable)?;
        entries.push(Entry {
            path: name,
            directory: kind.is_dir(),
            mode,
        });
    }
    Ok(Layout { entries, refused })
}

/// Checks that `layout` is a native archive the native route may publish.
pub fn assert_native(archive: &Path, layout: &Layout) -> Result<()> {
    if let Some(refused) = layout.refused.first() {
        return Err(format!(
            "{} holds {refused}, which no release archive may carry",
            archive.display()
        ));
    }
    let paths: Vec<&str> = layout
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    if let Some(marker) = TYPESCRIPT_MARKERS
        .iter()
        .find(|marker| paths.contains(marker))
        .copied()
        .or_else(|| {
            paths
                .iter()
                .any(|path| path.starts_with("node_modules/"))
                .then_some("node_modules/")
        })
    {
        return Err(format!(
            "{} holds {marker} of a TypeScript release; a native archive carries no Node runtime or TypeScript product code",
            archive.display()
        ));
    }
    let native = NATIVE_EXECUTABLES.iter().any(|exe| paths.contains(exe))
        && paths.contains(&NATIVE_INVENTORY)
        && paths
            .iter()
            .any(|path| path.starts_with(NATIVE_NOTICES_DIR));
    if !native {
        return Err(format!(
            "{} is not a native archive: it needs one of {} with {NATIVE_INVENTORY} and {NATIVE_NOTICES_DIR}",
            archive.display(),
            NATIVE_EXECUTABLES.join(", ")
        ));
    }
    Ok(())
}

/// Reads `archive` back and checks it holds exactly `members` and their
/// directories, with the binary executable and nothing writable by group
/// or others. Returns the listing, `<mode> <path>`.
pub fn verify(archive: &Path, members: &[Member]) -> Result<Vec<String>> {
    let layout = read_layout(archive)?;
    let mut expected: BTreeMap<String, Option<u32>> = directories(members)
        .into_iter()
        .map(|dir| (dir, None))
        .collect();
    for member in members {
        expected.insert(member.path.clone(), Some(member.mode));
    }
    if let Some(refused) = layout.refused.first() {
        return Err(format!("The archive holds {refused}"));
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut listing = Vec::new();
    for entry in &layout.entries {
        if !seen.insert(entry.path.clone()) {
            return Err(format!("The archive holds {} twice", entry.path));
        }
        let Some(want) = expected.get(&entry.path) else {
            return Err(format!(
                "The archive holds {}, which is not part of the package",
                entry.path
            ));
        };
        if want.is_none() != entry.directory {
            return Err(format!("The archive's {} has the wrong type", entry.path));
        }
        if entry.mode & SHARED_WRITE != 0 {
            return Err(format!(
                "The archive's {} is writable by group or others ({:o})",
                entry.path, entry.mode
            ));
        }
        if *want == Some(0o755) && entry.mode & 0o111 != 0o111 {
            return Err(format!("The archive's {} is not executable", entry.path));
        }
        listing.push(format!(
            "{:04o} {}{}",
            entry.mode,
            entry.path,
            if entry.directory { "/" } else { "" }
        ));
    }
    let missing: Vec<&String> = expected
        .keys()
        .filter(|path| !seen.contains(*path))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "The archive lacks {}",
            missing
                .iter()
                .map(|path| path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(listing)
}

/// Reads one member's bytes from a gzipped tar.
pub fn read_member(archive: &Path, wanted: &str) -> Result<Option<Vec<u8>>> {
    let file = File::open(archive).map_err(|error| format!("{}: {error}", archive.display()))?;
    let mut tar = tar::Archive::new(GzDecoder::new(file));
    for entry in tar.entries().map_err(|error| error.to_string())? {
        let mut entry = entry.map_err(|error| error.to_string())?;
        let path = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        if path.trim_start_matches("./") == wanted {
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            return Ok(Some(bytes));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Scratch;

    fn members() -> Vec<Member> {
        vec![
            Member {
                path: "bin/bake".to_owned(),
                mode: 0o755,
                source: Source::Bytes(b"#!/bin/sh\n".to_vec()),
            },
            Member {
                path: "LICENSE".to_owned(),
                mode: 0o644,
                source: Source::Bytes(b"MIT\n".to_vec()),
            },
            Member {
                path: "notices/bake-cli/NOTICE".to_owned(),
                mode: 0o644,
                source: Source::Bytes(b"n\n".to_vec()),
            },
            Member {
                path: NATIVE_INVENTORY.to_owned(),
                mode: 0o644,
                source: Source::Bytes(b"{}\n".to_vec()),
            },
        ]
    }

    #[test]
    fn the_written_archive_reads_back_exactly_and_is_reproducible() {
        let scratch = Scratch::new("bake-release-archive").expect("scratch");
        let first = scratch.path().join("a.tar.gz");
        let second = scratch.path().join("b.tar.gz");
        write_tar_gz(&members(), &first, 1_700_000_000).expect("write");
        write_tar_gz(&members(), &second, 1_700_000_000).expect("write");
        assert_eq!(std::fs::read(&first).ok(), std::fs::read(&second).ok());
        let listing = verify(&first, &members()).expect("verify");
        assert_eq!(
            listing,
            [
                "0644 LICENSE",
                "0755 bin/",
                "0755 bin/bake",
                "0755 licenses/",
                "0644 licenses/inventory.json",
                "0755 notices/",
                "0755 notices/bake-cli/",
                "0644 notices/bake-cli/NOTICE"
            ]
        );
        assert_native(&first, &read_layout(&first).expect("layout")).expect("native");
        // bake-update unpacks it, as an install does.
        let into = scratch.path().join("unpacked");
        bake_update::archive::extract(&first, &into).expect("extract");
        assert_eq!(
            std::fs::read(into.join("bin/bake")).ok().as_deref(),
            Some(&b"#!/bin/sh\n"[..])
        );
        assert_eq!(
            read_member(&first, "LICENSE").expect("read").as_deref(),
            Some(&b"MIT\n"[..])
        );
    }

    #[test]
    fn an_archive_that_is_not_the_package_is_refused() {
        let scratch = Scratch::new("bake-release-archive").expect("scratch");
        let path = scratch.path().join("a.tar.gz");
        let mut extra = members();
        extra.push(Member {
            path: "package.json".to_owned(),
            mode: 0o644,
            source: Source::Bytes(b"{}".to_vec()),
        });
        write_tar_gz(&extra, &path, 0).expect("write");
        assert!(verify(&path, &members()).is_err());
        assert!(
            assert_native(&path, &read_layout(&path).expect("layout"))
                .expect_err("typescript")
                .contains("package.json")
        );
        let mut writable = members();
        writable[1].mode = 0o666;
        write_tar_gz(&writable, &path, 0).expect("write");
        assert!(
            verify(&path, &writable)
                .expect_err("shared write")
                .contains("writable")
        );
        write_tar_gz(&members()[1..], &path, 0).expect("write");
        assert!(assert_native(&path, &read_layout(&path).expect("layout")).is_err());
        std::fs::write(&path, b"not gzip").expect("write");
        assert!(read_layout(&path).is_err());
    }
}
