//! Packs the built `bake` for this host into its release archive.
//!
//! The archive holds the binary, the repository's `LICENSE` and
//! `THIRD_PARTY_NOTICES.md`, the NOTICE of each Bake crate linked into the
//! binary, and this platform's third-party license text with the bundle
//! inventory, generated for this run, and nothing else. The binary's
//! `--version` and `--self-check` must name the Cargo version and this
//! platform, and its embedded documentation must match the sources, both
//! before and after the archive is unpacked by `bake-update`'s own reader;
//! only then does the archive replace the destination, by rename.
//!
//! Ported from Bake's `scripts/release/pack-rust.ts` (revision 97ce6abba0).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use bake_update::manifest::{RELEASE_TARGETS, archive_name};
use serde_json::Value;

use crate::archive::{Member, Source, verify, write_tar_gz};
use crate::licenses::{
    BUNDLE_FILE, Bundle, CARGO_ABOUT_VERSION, INVENTORY_FILE, PLATFORMS, SCHEMA,
};
use crate::util::{Result, Scratch, canonical_path, inside, sha256, sha256_file};
use crate::workspace::{Inputs, ROOT_PACKAGE};

/// Where archives are written by default, under the repository.
pub const OUTPUT_DIR: &str = ".artifacts/bake-native-release";

/// Each run of the binary is bounded by this.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(30);

/// The SHA-256 of every `*.md` file in `dir`, by name without `.md`, as
/// `bake --self-check` names its topics.
pub fn docs_digests(dir: &Path) -> Result<BTreeMap<String, String>> {
    let mut digests = BTreeMap::new();
    let entries = std::fs::read_dir(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(topic) = name.strip_suffix(".md") else {
            continue;
        };
        let path = entry.path();
        if !std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file()) {
            return Err(format!("{} is not a regular file", path.display()));
        }
        digests.insert(topic.to_owned(), sha256_file(&path)?.0);
    }
    if digests.is_empty() {
        return Err(format!("{} holds no documentation", dir.display()));
    }
    Ok(digests)
}

/// What one isolated run printed.
struct Ran {
    status: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Runs `binary` with `args` in a private empty home and `PATH`, bounded,
/// then requires that both are still empty.
fn run_isolated(binary: &Path, args: &[&str]) -> Result<Ran> {
    let scratch = Scratch::new("bake-pack-run")?;
    let home = scratch.path().join("home");
    let path = scratch.path().join("path");
    for dir in [&home, &path] {
        std::fs::create_dir(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    }
    let command = format!("{} {}", binary.display(), args.join(" "));
    let mut child = Command::new(binary)
        .args(args)
        .current_dir(scratch.path())
        .env_clear()
        .env("PATH", &path)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("BAKE_HOME", home.join(".bake"))
        .env("DSH_HOME", home.join(".dsh"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("{command} could not start: {error}"))?;
    let read = |stream: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(mut stream) = stream {
                let _ = std::io::Read::take(&mut stream, 1 << 20).read_to_end(&mut out);
            }
            out
        })
    };
    use std::io::Read as _;
    let stdout = read(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
    );
    let stderr = read(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
    );
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            break status;
        }
        if started.elapsed() > RUN_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{command} did not finish within {RUN_TIMEOUT:?}"));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    let written: Vec<String> = [&home, &path]
        .iter()
        .flat_map(|dir| std::fs::read_dir(dir).into_iter().flatten().flatten())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    if !written.is_empty() {
        return Err(format!(
            "{command} wrote {} into its empty home or PATH",
            written.join(", ")
        ));
    }
    Ok(Ran {
        status: status.code(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

/// Checks a binary as a release runs it: `--version`, then `--self-check`,
/// whose record must name `version` and `target` and embed exactly the
/// documentation in `docs`. Returns the record line.
pub fn check_binary(
    binary: &Path,
    version: &str,
    target: &str,
    docs: &BTreeMap<String, String>,
) -> Result<String> {
    let ran = run_isolated(binary, &["--version"])?;
    if ran.status != Some(0) || ran.stdout != format!("bake {version}\n") {
        return Err(format!(
            "bake --version exited {:?} with {:?}; expected \"bake {version}\"",
            ran.status, ran.stdout
        ));
    }
    let ran = run_isolated(binary, &["--self-check"])?;
    if ran.status != Some(0) {
        return Err(format!(
            "bake --self-check exited {:?}: {}",
            ran.status,
            ran.stderr.trim()
        ));
    }
    let Some(line) = ran
        .stdout
        .strip_suffix('\n')
        .filter(|line| !line.contains('\n'))
    else {
        return Err(format!(
            "the self-check printed {:?}, not one line",
            ran.stdout
        ));
    };
    bake_update::check::check_record(line, version, target)
        .map_err(|why| format!("the self-check record is refused: {why}"))?;
    let record: Value =
        serde_json::from_str(line).map_err(|error| format!("the self-check record: {error}"))?;
    let reported = record
        .get("docs")
        .and_then(Value::as_object)
        .ok_or_else(|| "the self-check record's docs is not an object".to_owned())?;
    for name in reported.keys().chain(docs.keys()) {
        match (reported.get(name).and_then(Value::as_str), docs.get(name)) {
            (Some(_), None) => {
                return Err(format!(
                    "The binary embeds documentation topic '{name}', which the sources lack"
                ));
            }
            (None, Some(_)) => {
                return Err(format!("The binary lacks documentation topic '{name}'"));
            }
            (Some(have), Some(want)) if have != want => {
                return Err(format!(
                    "The binary's documentation topic '{name}' differs from its source"
                ));
            }
            _ => {}
        }
    }
    Ok(line.to_owned())
}

/// This platform's license text and the inventory from `bundle`, once the
/// bundle is shown to describe this build: its inputs unchanged, its text
/// matching its inventory hash and naming this platform, its `bake-cli`
/// version this one, and its Bake NOTICEs exactly the linked ones.
/// Returns the text, the inventory, and each NOTICE's SHA-256 by crate.
pub fn target_licenses(
    bundle: &Bundle,
    target: &str,
    version: &str,
    root: &Path,
    notices: &BTreeMap<String, PathBuf>,
) -> Result<(String, String, BTreeMap<String, String>)> {
    let fail =
        |why: String| format!("The native license bundle {why}; regenerate it from this checkout");
    let inventory_text = bundle
        .file(INVENTORY_FILE)
        .ok_or_else(|| fail(format!("has no {INVENTORY_FILE}")))?;
    let inventory: Value = serde_json::from_str(inventory_text)
        .map_err(|_| fail(format!("{INVENTORY_FILE} is not JSON")))?;
    if inventory.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(fail(format!(
            "{INVENTORY_FILE} is not a {SCHEMA} inventory"
        )));
    }
    let tool = inventory.pointer("/generator/tool").and_then(Value::as_str);
    if tool != Some(format!("cargo-about {CARGO_ABOUT_VERSION}").as_str()) {
        return Err(fail(format!(
            "was generated by {tool:?}, not cargo-about {CARGO_ABOUT_VERSION}"
        )));
    }
    let inputs = inventory
        .get("inputs")
        .and_then(Value::as_object)
        .ok_or_else(|| fail("records no inputs".to_owned()))?;
    if !inputs
        .values()
        .any(|input| input.get("path").and_then(Value::as_str) == Some("Cargo.lock"))
    {
        return Err(fail("does not record Cargo.lock as an input".to_owned()));
    }
    for input in inputs.values() {
        let (Some(path), Some(digest)) = (
            input.get("path").and_then(Value::as_str),
            input.get("sha256").and_then(Value::as_str),
        ) else {
            return Err(fail(format!(
                "records an input {input} that is not a file of the repository"
            )));
        };
        if Path::new(path).is_absolute() || path.split('/').any(|part| part == "..") {
            return Err(fail(format!(
                "records an input {path:?} outside the repository"
            )));
        }
        let actual = std::fs::read(root.join(path))
            .map_or_else(|_| "missing".to_owned(), |bytes| sha256(&bytes));
        if actual != digest {
            return Err(fail(format!(
                "was generated from {path} at sha256 {digest}, but this checkout has {actual}"
            )));
        }
    }
    let platforms = inventory
        .get("platforms")
        .and_then(Value::as_array)
        .ok_or_else(|| fail("lists no platforms".to_owned()))?;
    let mut listed = std::collections::BTreeSet::from([INVENTORY_FILE.to_owned()]);
    for entry in platforms {
        let (Some(file), Some(digest)) = (
            entry.get("file").and_then(Value::as_str),
            entry.get("sha256").and_then(Value::as_str),
        ) else {
            return Err(fail("lists a platform without a file and hash".to_owned()));
        };
        listed.insert(file.to_owned());
        let text = bundle
            .file(file)
            .ok_or_else(|| fail(format!("lacks {file}")))?;
        if sha256(text.as_bytes()) != digest {
            return Err(fail(format!(
                "has a {file} that differs from its inventory hash"
            )));
        }
    }
    let extra: Vec<&str> = bundle
        .files
        .iter()
        .map(|(path, _)| path.as_str())
        .filter(|path| !listed.contains(*path))
        .collect();
    if !extra.is_empty() {
        return Err(fail(format!(
            "holds {}, which its inventory does not list",
            extra.join(", ")
        )));
    }
    let triple = PLATFORMS
        .iter()
        .find(|(id, _)| *id == target)
        .map(|(_, triple)| *triple)
        .ok_or_else(|| fail(format!("has no {target} text")))?;
    let path = format!("{target}/{BUNDLE_FILE}");
    let text = bundle
        .file(&path)
        .ok_or_else(|| fail(format!("has no {target} text")))?;
    if !text.starts_with(&format!(
        "Third-party licenses for bake, {target} ({triple})\n"
    )) {
        return Err(fail(format!(
            "has a {path} whose heading names another platform"
        )));
    }
    let workspace = inventory
        .get("workspace")
        .and_then(Value::as_array)
        .ok_or_else(|| fail("lists no Bake crates".to_owned()))?;
    let cli = workspace
        .iter()
        .find(|entry| entry.get("name").and_then(Value::as_str) == Some(ROOT_PACKAGE))
        .and_then(|entry| entry.get("version"))
        .and_then(Value::as_str);
    if cli != Some(version) {
        return Err(fail(format!(
            "describes {ROOT_PACKAGE} {cli:?}, not {version}"
        )));
    }
    let mut reproduced = BTreeMap::new();
    for entry in workspace {
        let on_target = entry
            .get("platforms")
            .and_then(Value::as_array)
            .is_some_and(|platforms| platforms.iter().any(|p| p.as_str() == Some(target)));
        let (Some(name), Some(digest)) = (
            entry.get("name").and_then(Value::as_str),
            entry.pointer("/notice/sha256").and_then(Value::as_str),
        ) else {
            continue;
        };
        if on_target {
            reproduced.insert(name.to_owned(), digest.to_owned());
        }
    }
    for krate in reproduced.keys().chain(notices.keys()) {
        if !reproduced.contains_key(krate) {
            return Err(fail(format!(
                "does not reproduce the NOTICE of {krate}, which bake links"
            )));
        }
        if !notices.contains_key(krate) {
            return Err(fail(format!(
                "reproduces a NOTICE of {krate}, which the archive does not carry"
            )));
        }
    }
    Ok((text.to_owned(), inventory_text.to_owned(), reproduced))
}

/// The archive's members, in a fixed order.
pub fn payload(
    binary: &Path,
    root: &Path,
    notices: &BTreeMap<String, PathBuf>,
    target: &str,
    text: &str,
    inventory: &str,
) -> Result<Vec<Member>> {
    let mut members = vec![
        Member {
            path: if target == "win32-x64" {
                "bin/bake.exe"
            } else {
                "bin/bake"
            }
            .to_owned(),
            mode: 0o755,
            source: Source::Disk(binary.to_path_buf()),
        },
        Member {
            path: "LICENSE".to_owned(),
            mode: 0o644,
            source: Source::Disk(root.join("LICENSE")),
        },
        Member {
            path: "THIRD_PARTY_NOTICES.md".to_owned(),
            mode: 0o644,
            source: Source::Disk(root.join("THIRD_PARTY_NOTICES.md")),
        },
    ];
    for (krate, notice) in notices {
        if !krate
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(format!(
                "Crate name {krate:?} cannot name a notice directory"
            ));
        }
        members.push(Member {
            path: format!("notices/{krate}/NOTICE"),
            mode: 0o644,
            source: Source::Disk(notice.clone()),
        });
    }
    members.push(Member {
        path: format!("licenses/{INVENTORY_FILE}"),
        mode: 0o644,
        source: Source::Bytes(inventory.as_bytes().to_vec()),
    });
    members.push(Member {
        path: format!("licenses/{target}/{BUNDLE_FILE}"),
        mode: 0o644,
        source: Source::Bytes(text.as_bytes().to_vec()),
    });
    Ok(members)
}

/// The directory an archive may be written into: outside the repository,
/// or under its [`OUTPUT_DIR`], never in the download service's payload.
pub fn check_output_dir(root: &Path, out: &Path) -> Result<PathBuf> {
    let out = canonical_path(out)?;
    let root = canonical_path(root)?;
    let public = canonical_path(&root.join("distribution/host/public"))?;
    if out == public || inside(&public, &out) {
        return Err(format!(
            "{} is inside distribution/host/public/, which only `assemble` writes",
            out.display()
        ));
    }
    let allowed = canonical_path(&root.join(OUTPUT_DIR))?;
    if (out == root || inside(&root, &out)) && !(out == allowed || inside(&allowed, &out)) {
        return Err(format!(
            "{} is in the repository but not under {OUTPUT_DIR}/",
            out.display()
        ));
    }
    Ok(out)
}

/// The time every archive entry carries: `SOURCE_DATE_EPOCH`, else the
/// commit's time, so a commit's archive is the same bytes on a rerun.
pub fn archive_time(root: &Path) -> u64 {
    std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .or_else(|| {
            crate::util::run("git", &["log", "-1", "--format=%ct"], root)
                .ok()
                .and_then(|value| value.trim().parse().ok())
        })
        .unwrap_or(0)
}

/// What a pack made.
#[derive(Debug)]
pub struct Packed {
    pub archive: PathBuf,
    pub record: String,
    pub listing: Vec<String>,
}

/// Stages, checks, archives, unpacks, and checks again; then moves the
/// archive into `out` as its release name.
pub fn pack(
    root: &Path,
    inputs: &Inputs,
    binary: &Path,
    bundle: &Bundle,
    out: &Path,
) -> Result<Packed> {
    let target = bake_update::manifest::host_target()
        .ok_or_else(|| "this host is not a Bake release platform".to_owned())?;
    if !RELEASE_TARGETS.contains(&target) {
        return Err(format!(
            "Bake releases {} only; native {target} releases arrive in a later 0.4.x",
            RELEASE_TARGETS.join(" and ")
        ));
    }
    let version = inputs.version.as_str();
    let docs = docs_digests(&inputs.docs)?;
    let (text, inventory, reproduced) =
        target_licenses(bundle, target, version, root, &inputs.notices)?;
    for (krate, notice) in &inputs.notices {
        if reproduced.get(krate) != Some(&sha256_file(notice)?.0) {
            return Err(format!(
                "The native license bundle reproduces a NOTICE of {krate} other than {}; regenerate it from this checkout",
                notice.display()
            ));
        }
    }
    let members = payload(binary, root, &inputs.notices, target, &text, &inventory)?;
    let out = check_output_dir(root, out)?;
    std::fs::create_dir_all(&out).map_err(|error| format!("{}: {error}", out.display()))?;
    let name = archive_name(version, target);
    let scratch = Scratch::new("bake-pack")?;
    // The binary is checked as staged, a private copy, before it is archived.
    let staged = scratch.path().join("bake");
    std::fs::copy(binary, &staged).map_err(|error| format!("{}: {error}", binary.display()))?;
    let record = check_binary(&staged, version, target, &docs)?;
    let archive = scratch.path().join("archive.tar.gz");
    write_tar_gz(&members, &archive, archive_time(root))?;
    let listing = verify(&archive, &members)?;
    let unpacked = scratch.path().join("unpacked");
    bake_update::archive::extract(&archive, &unpacked).map_err(|error| error.to_string())?;
    for member in &members {
        let want = match &member.source {
            Source::Disk(path) => sha256_file(path)?.0,
            Source::Bytes(bytes) => sha256(bytes),
        };
        if sha256_file(&unpacked.join(&member.path))?.0 != want {
            return Err(format!(
                "The unpacked {} differs from the staged file",
                member.path
            ));
        }
    }
    let unpacked_record = check_binary(&unpacked.join(&members[0].path), version, target, &docs)?;
    if unpacked_record != record {
        return Err("The unpacked binary's self-check differs from the staged binary's".to_owned());
    }
    // Beside the destination, so the rename stays on one file system.
    let partial = out.join(format!(".{name}.{}.partial", std::process::id()));
    let destination = out.join(&name);
    std::fs::copy(&archive, &partial).map_err(|error| format!("{}: {error}", partial.display()))?;
    std::fs::rename(&partial, &destination).map_err(|error| {
        let _ = std::fs::remove_file(&partial);
        format!("{}: {error}", destination.display())
    })?;
    Ok(Packed {
        archive: destination,
        record,
        listing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_docs_are_the_topics() {
        let root = crate::util::repository_root();
        let docs = docs_digests(&root.join("crates/bake-cli/docs")).expect("docs");
        assert!(docs.contains_key("cli") && docs.contains_key("update"));
    }

    #[test]
    fn archives_stay_out_of_the_repository_but_their_own_directory() {
        let root = crate::util::repository_root();
        assert!(
            check_output_dir(&root, &root.join(".artifacts/bake-native-release/0.4.2")).is_ok()
        );
        assert!(check_output_dir(&root, &root.join("crates")).is_err());
        assert!(check_output_dir(&root, &root.join("distribution/host/public/native")).is_err());
        assert!(check_output_dir(&root, &std::env::temp_dir().join("bake-out")).is_ok());
    }
}
