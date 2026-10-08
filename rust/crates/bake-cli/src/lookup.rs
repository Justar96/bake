//! The lookup form of `session inspect`: find the Session with a given id in
//! an explicit root, read-only, as TypeScript's `JsonlSessionPersistence`
//! opens it with `open(id, 'read')` in
//! `packages/session/session-persistence-jsonl/src/index.ts`.
//!
//! The stages run in that backend's order, and an earlier stage's refusal
//! wins:
//!
//! 1. `layout`: `ensureRootEncoding` lists the root's real project
//!    directories, refuses a regular `*.jsonl` or `*.jsonl.zstd` file in one as
//!    the flat legacy layout, and refuses a canonical generation of the other
//!    compression in any real Session directory. Directory symlinks are not
//!    followed here.
//! 2. `lookup`: `findLog` probes each project for `<id>.jsonl.zstd` and
//!    `<id>.jsonl` by opening them, so a link counts, then lists
//!    `<project>/<id>`, following a symlink, for the other compression and the
//!    numerically highest canonical generation. Matches in more than one
//!    project are a duplicate; none is not found.
//! 3. `generation`: a newer generation is refused from its first line or
//!    Zstd frame alone, although the whole file is read within `--max-bytes`
//!    where TypeScript reads only that header; an absent file there is a
//!    malformed header, as in TypeScript. An older generation is a native
//!    limit decided from its name, before any file is opened, because
//!    TypeScript migrates it.
//! 4. `scan`, `identity`, `restore`: the current generation is scanned, its
//!    header id and the path its id and `cwd` name are checked against the
//!    selected file, with a `realpath` comparison when the spellings differ,
//!    and then its events are validated and restored.
//!
//! Every listing is visited in byte order of entry names, where TypeScript
//! uses the operating system's order. Within one stage the first fault found
//! wins, so when faults coexist in different projects or Session directories
//! the reported stage, reason, kind, or even refusal versus failure can
//! differ from TypeScript's; the shared cases hold one fault per stage or an
//! order the source fixes. `--max-entries` bounds the entries read across all
//! listings, counting a directory again each time it is listed. Names that are
//! not UTF-8, which Node reads with replacement characters, are native limits
//! where they would be traversed. Nothing is written, locked, or migrated,
//! and the metadata comparisons are not an atomic snapshot of the root.

use std::ffi::OsStr;
use std::fs::{self, DirEntry};
use std::io;
use std::path::{Path, PathBuf};

use bake_session::{
    CURRENT_SESSION_FORMAT_VERSION, HeaderRefusal, PathPlatform, Rejection, RestoreRefusal,
    SessionHeader, SubsetLimit, first_record, read_header_record, zstd_header_record,
};
use serde_json::{Map, Value, json};

#[cfg(not(windows))]
use crate::inspect::open_read_only;
use crate::inspect::{
    Encoding, InspectArgs, Kind, LogName, LookupArgs, MAX_BUDGET, ReadFailure, Refusal, describe,
    parse_log_name, plaintext_budget, read_bounded, read_bounded_or_missing, restored_fields,
    stage,
};

/// Encode one string as one safe path segment, as TypeScript's
/// `encodeSegment` does over UTF-16 code units: ASCII letters, digits, `.`,
/// `_`, and `-` stay literal, every other unit becomes `~XXXX`, and the whole
/// segments `.` and `..` are escaped.
pub fn encode_segment(raw: &str) -> String {
    match raw {
        "." => return "~002E".into(),
        ".." => return "~002E~002E".into(),
        _ => {}
    }
    let mut out = String::with_capacity(raw.len());
    for unit in raw.encode_utf16() {
        push_unit(&mut out, unit);
    }
    out
}

/// The project directory name for a `cwd`, as TypeScript's `projectKey`
/// builds it: runs of `/`, `\`, and `:` become one `-`, other units are kept
/// or escaped as in [`encode_segment`], leading `-` are removed, an empty
/// result is `root`, and the key keeps its first 251 units between `--` and
/// `--`. Escapes are ASCII, so the cut can split one.
pub fn project_key(cwd: &str) -> String {
    let mut readable = String::with_capacity(cwd.len());
    let mut separator_run = false;
    for unit in cwd.encode_utf16() {
        if matches!(unit, 0x2F | 0x5C | 0x3A) {
            if !separator_run {
                readable.push('-');
            }
            separator_run = true;
        } else {
            push_unit(&mut readable, unit);
            separator_run = false;
        }
    }
    let slug = readable.trim_start_matches('-');
    let slug = if slug.is_empty() { "root" } else { slug };
    format!("--{}--", &slug[..slug.len().min(251)])
}

fn push_unit(out: &mut String, unit: u16) {
    match u8::try_from(unit) {
        Ok(byte) if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') => {
            out.push(char::from(byte));
        }
        _ => out.push_str(&format!("~{unit:04X}")),
    }
}

/// Make the root absolute as Node's `path.resolve` does on POSIX: join a
/// relative root to the working directory, then drop `.` and apply `..`
/// lexically, without following a symlink.
#[cfg(not(windows))]
fn resolve_root(root: &OsStr) -> io::Result<PathBuf> {
    use std::path::Component;
    let root = Path::new(root);
    let mut resolved = if root.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir()?
    };
    for component in root.components() {
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            other => resolved.push(other),
        }
    }
    Ok(resolved)
}

/// On Windows the root is made absolute by `GetFullPathNameW`, which also
/// removes `.` and `..` lexically; Node's Win32 `path.resolve` is not
/// reproduced beyond that.
#[cfg(windows)]
fn resolve_root(root: &OsStr) -> io::Result<PathBuf> {
    if root.is_empty() {
        return std::env::current_dir();
    }
    std::path::absolute(root)
}

/// Refuse a root spelling whose resolution this preview does not share with
/// Node: one that is not UTF-8 once resolved, and on Windows any form other
/// than a relative path or a drive-absolute `X:\` path.
pub(crate) fn check_root(spelled: &OsStr, resolved: &Path) -> Step<()> {
    let refuse =
        |reason, message: String| refused("root", reason, Kind::NativeLimit, message, None);
    if let Some(problem) = unsupported_root(Path::new(spelled)) {
        return Err(refuse(
            "unsupported-root",
            format!("this preview does not resolve a root {problem}"),
        ));
    }
    if resolved.to_str().is_none() {
        return Err(refuse(
            "non-utf8-name",
            format!(
                "the resolved root {} is not UTF-8, which this preview does not read",
                resolved.display()
            ),
        ));
    }
    Ok(())
}

/// Why a Windows root spelling is outside the supported forms, or `None`.
#[cfg(windows)]
fn unsupported_root(root: &Path) -> Option<&'static str> {
    use std::path::{Component, Prefix};
    let mut components = root.components();
    match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(_) if matches!(components.next(), Some(Component::RootDir)) => {}
            Prefix::Disk(_) => return Some("relative to a drive's working directory, such as C:x"),
            Prefix::UNC(..) => return Some("on a network share"),
            _ => return Some("in the verbatim or device namespace, such as \\\\?\\ or \\\\.\\"),
        },
        Some(Component::RootDir) => return Some("rooted without a drive, such as \\x"),
        _ => {}
    }
    root.components()
        .any(|component| {
            matches!(component, Component::Normal(name)
                if name.to_str().is_some_and(unsupported_windows_name))
        })
        .then_some("with a name ending in a dot or a space, or a reserved device name")
}

#[cfg(not(windows))]
const fn unsupported_root(_: &Path) -> Option<&'static str> {
    None
}

/// Why a lookup ended before restoration: a refusal record (exit status 3)
/// or an I/O failure (exit status 1).
pub(crate) enum Stop {
    Refused(Box<Refused>),
    Failure(String),
}

pub(crate) struct Refused {
    pub(crate) stage: &'static str,
    pub(crate) reason: Option<&'static str>,
    pub(crate) refusal: Refusal,
    /// The root-relative artifact the refusal names.
    pub(crate) path: Option<String>,
}

impl Refused {
    /// The record's `refusal` object.
    pub(crate) fn to_json(&self) -> Value {
        let refusal = &self.refusal;
        json!({
            "stage": self.stage,
            "reason": self.reason,
            "kind": refusal.kind.label(),
            "message": refusal.message,
            "path": self.path,
            "line": refusal.line,
            "seq": refusal.seq,
            "offset": refusal.offset,
        })
    }
}

pub(crate) type Step<T> = Result<T, Stop>;

pub(crate) fn refused(
    stage: &'static str,
    reason: &'static str,
    kind: Kind,
    message: String,
    path: Option<String>,
) -> Stop {
    Stop::Refused(Box::new(Refused {
        stage,
        reason: Some(reason),
        refusal: Refusal::new(kind, message),
        path,
    }))
}

/// The selected generation: the numerically highest canonical name of the
/// configured compression in one Session directory, whatever its file type.
pub(crate) struct Selected {
    /// Project directory name, a listed UTF-8 entry.
    project: String,
    /// Session directory name: the encoded id for a lookup, a listed UTF-8
    /// entry for discovery.
    session: String,
    name: String,
    pub(crate) version: u64,
}

impl Selected {
    /// The root-relative path, with `/` separators.
    pub(crate) fn relative(&self) -> String {
        format!("{}/{}/{}", self.project, self.session, self.name)
    }

    pub(crate) fn path(&self, root: &Path) -> PathBuf {
        root.join(&self.project)
            .join(&self.session)
            .join(&self.name)
    }
}

/// The root made absolute as TypeScript's backend resolves it.
pub(crate) fn resolve(root: &OsStr) -> Result<PathBuf, String> {
    resolve_root(root).map_err(|error| format!("cannot resolve the root {root:?}: {error}"))
}

/// The root and id checks, `ensureRootEncoding`, and `findLog` for the
/// resolved `root`: the selected generation, or `None` when no project holds
/// the id.
pub(crate) fn select(root: &Path, lookup: &LookupArgs) -> Step<Option<Selected>> {
    check_root(&lookup.root, root)?;
    check_name("lookup", &encode_segment(&lookup.id))?;
    Scan::new(root, lookup.encoding, lookup.max_entries).find(&lookup.id)
}

/// The directory listings of one lookup or listing, bounded together by
/// `--max-entries`. Refusals name the current stage.
pub(crate) struct Scan<'a> {
    root: &'a Path,
    encoding: Encoding,
    max_entries: u64,
    entries: u64,
    stage: &'static str,
}

impl<'a> Scan<'a> {
    /// A scan of the resolved `root` that starts at the `layout` stage.
    pub(crate) const fn new(root: &'a Path, encoding: Encoding, max_entries: u64) -> Self {
        Self {
            root,
            encoding,
            max_entries,
            entries: 0,
            stage: "layout",
        }
    }

    /// One directory's entries in byte order of their names, or `None` when
    /// `missing_ok` and the directory does not exist.
    fn list(&mut self, dir: &Path, missing_ok: bool) -> Step<Option<Vec<DirEntry>>> {
        let failure = |error: io::Error| Stop::Failure(format!("cannot list {dir:?}: {error}"));
        let listing = match fs::read_dir(dir) {
            Ok(listing) => listing,
            Err(error) if missing_ok && error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(failure(error)),
        };
        let mut entries = Vec::new();
        for entry in listing {
            self.entries += 1;
            if self.entries > self.max_entries {
                return Err(refused(
                    self.stage,
                    "entry-budget",
                    Kind::NativeLimit,
                    format!(
                        "more than --max-entries {} directory entries were read",
                        self.max_entries
                    ),
                    None,
                ));
            }
            entries.push(entry.map_err(failure)?);
        }
        entries.sort_by_key(DirEntry::file_name);
        Ok(Some(entries))
    }

    /// The canonical generation version an entry name has for `encoding`.
    fn version(name: &OsStr, encoding: Encoding) -> Option<u64> {
        match parse_log_name(name.to_str()?) {
            LogName::Canonical {
                version,
                encoding: named,
            } if named == encoding => Some(version),
            _ => None,
        }
    }

    const fn opposite(&self) -> Encoding {
        match self.encoding {
            Encoding::None => Encoding::Zstd,
            Encoding::Zstd => Encoding::None,
        }
    }

    fn mismatch(&self, path: String) -> Stop {
        refused(
            self.stage,
            "encoding-mismatch",
            Kind::Invalid,
            format!(
                "{path} is a {} Session generation, but --compression is {}; use a separate \
                 root or the matching compression",
                match self.opposite() {
                    Encoding::None => "plain",
                    Encoding::Zstd => "Zstd",
                },
                self.encoding.label()
            ),
            Some(path),
        )
    }

    fn legacy(&self, path: String) -> Stop {
        refused(
            self.stage,
            "legacy-layout",
            Kind::Invalid,
            format!("{path} uses the unsupported flat-file Session layout"),
            Some(path),
        )
    }

    /// `ensureRootEncoding`, then `findLog`.
    fn find(&mut self, id: &str) -> Step<Option<Selected>> {
        let projects = self.layout()?;
        self.stage = "lookup";
        let encoded = encode_segment(id);
        let mut matches = Vec::new();
        for project in &projects {
            let dir = self.root.join(project);
            for suffix in [".jsonl.zstd", ".jsonl"] {
                let name = format!("{encoded}{suffix}");
                match open_legacy_probe(&dir.join(&name)) {
                    Ok(_) => return Err(self.legacy(format!("{project}/{name}"))),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(Stop::Failure(format!(
                            "cannot probe {:?}: {error}",
                            dir.join(&name)
                        )));
                    }
                }
            }
            matches.extend(self.generation(project, &encoded)?);
        }
        if matches.len() > 1 {
            return Err(refused(
                "lookup",
                "duplicate-id",
                Kind::Invalid,
                format!(
                    "Session {id:?} appears in {} project directories",
                    matches.len()
                ),
                None,
            ));
        }
        Ok(matches.pop())
    }

    /// `resolveGenerationInDirectory` for `<project>/<session>`, following a
    /// link: the first generation of the other compression is refused, and
    /// the highest canonical name is selected without opening it.
    fn generation(&mut self, project: &str, session: &str) -> Step<Option<Selected>> {
        let Some(entries) = self.list(&self.root.join(project).join(session), true)? else {
            return Ok(None);
        };
        let mut latest: Option<(u64, String)> = None;
        for entry in &entries {
            let name = entry.file_name();
            if Self::version(&name, self.opposite()).is_some() {
                return Err(self.mismatch(format!("{project}/{session}/{}", name.display())));
            }
            if let Some(version) = Self::version(&name, self.encoding)
                && latest.as_ref().is_none_or(|(best, _)| version > *best)
            {
                latest = Some((version, name.to_string_lossy().into_owned()));
            }
        }
        Ok(latest.map(|(version, name)| Selected {
            project: project.to_owned(),
            session: session.to_owned(),
            name,
            version,
        }))
    }

    /// `listProjectDirs`: the root's real directories, with names not yet
    /// checked, or none when the root does not exist.
    fn projects(&mut self) -> Step<Vec<std::ffi::OsString>> {
        let Some(root_entries) = self.list(self.root, true)? else {
            return Ok(Vec::new());
        };
        let mut projects = Vec::new();
        for entry in root_entries {
            if file_type(&entry)?.is_some_and(|kind| kind.is_dir()) {
                projects.push(entry.file_name());
            }
        }
        Ok(projects)
    }

    /// A listed project directory name this preview can traverse.
    fn project_name(&self, name: &OsStr) -> Step<String> {
        let Some(project) = name.to_str() else {
            return Err(non_utf8(
                self.stage,
                &format!("project directory {}", name.display()),
            ));
        };
        check_name(self.stage, project)?;
        Ok(project.to_owned())
    }

    /// `listSessionDirs`: refuse a regular flat legacy file, then return the
    /// project's real Session directories. The project must exist.
    fn sessions(&mut self, project: &str) -> Step<Vec<String>> {
        let entries = self
            .list(&self.root.join(project), false)?
            .unwrap_or_default();
        let mut sessions = Vec::new();
        for entry in &entries {
            let Some(kind) = file_type(entry)? else {
                continue;
            };
            let name = entry.file_name();
            let bytes = name.as_encoded_bytes();
            if kind.is_file() && (bytes.ends_with(b".jsonl") || bytes.ends_with(b".jsonl.zstd")) {
                return Err(self.legacy(format!("{project}/{}", name.display())));
            }
            if kind.is_dir() {
                let Some(session) = name.to_str() else {
                    return Err(non_utf8(
                        self.stage,
                        &format!("Session directory {project}/{}", name.display()),
                    ));
                };
                check_name(self.stage, session)?;
                sessions.push(session.to_owned());
            }
        }
        Ok(sessions)
    }

    /// `ensureRootEncoding`: the real project directories, after refusing a
    /// flat legacy file or a generation of the other compression anywhere.
    fn layout(&mut self) -> Step<Vec<String>> {
        let mut projects = Vec::new();
        for name in self.projects()? {
            let project = self.project_name(&name)?;
            let dir = self.root.join(&project);
            for session in self.sessions(&project)? {
                let Some(entries) = self.list(&dir.join(&session), true)? else {
                    continue;
                };
                let opposite = entries
                    .iter()
                    .filter_map(|entry| {
                        let name = entry.file_name();
                        Self::version(&name, self.opposite()).map(|version| (version, name))
                    })
                    .max_by_key(|(version, _)| *version);
                if let Some((_, name)) = opposite {
                    return Err(self.mismatch(format!("{project}/{session}/{}", name.display())));
                }
            }
            projects.push(project);
        }
        Ok(projects)
    }

    /// `listArtifacts` up to its header reads: `ensureRootEncoding` at the
    /// `layout` stage, then, at the `discovery` stage, the root and every
    /// real project directory are listed again, and each real Session
    /// directory's selected generation is passed to `visit` before the next
    /// directory is listed. Directory links are not followed.
    pub(crate) fn discover(&mut self, mut visit: impl FnMut(Selected) -> Step<()>) -> Step<()> {
        self.layout()?;
        self.stage = "discovery";
        for name in self.projects()? {
            let project = self.project_name(&name)?;
            for session in self.sessions(&project)? {
                if let Some(selected) = self.generation(&project, &session)? {
                    visit(selected)?;
                }
            }
        }
        Ok(())
    }
}

/// Node lists a directory name that is not UTF-8 with replacement
/// characters and then opens the path that spelling names, which can be
/// absent or another entry; this preview reproduces neither.
fn non_utf8(stage: &'static str, subject: &str) -> Stop {
    refused(
        stage,
        "non-utf8-name",
        Kind::NativeLimit,
        format!("{subject} is not named in UTF-8, which this preview does not read"),
        None,
    )
}

/// Match Node's directory-entry classification; libuv labels every Windows
/// reparse point a link, including tags that Rust classifies as directories.
fn file_type(entry: &DirEntry) -> Step<Option<fs::FileType>> {
    let failure = |error| Stop::Failure(format!("cannot inspect {:?}: {error}", entry.path()));
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if is_reparse_point(entry.metadata().map_err(failure)?.file_attributes()) {
            return Ok(None);
        }
    }
    entry.file_type().map(Some).map_err(failure)
}

#[cfg(any(windows, test))]
const fn is_reparse_point(attributes: u32) -> bool {
    attributes & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
}

/// The legacy probe asks whether a read handle can be opened, including for
/// a directory. Keep its Windows flags separate from regular-file reads.
fn open_legacy_probe(path: &Path) -> io::Result<fs::File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(0x0200_0000) // FILE_FLAG_BACKUP_SEMANTICS, as in libuv
            .open(path)
    }
    #[cfg(not(windows))]
    open_read_only(path)
}

/// Node uses extended Windows paths, while this preview accepts ordinary
/// paths only. Refuse names that Win32 can normalize or treat as devices.
fn check_name(stage: &'static str, name: &str) -> Step<()> {
    #[cfg(windows)]
    if unsupported_windows_name(name) {
        return Err(refused(
            stage,
            "unsupported-name",
            Kind::NativeLimit,
            format!(
                "this preview does not look up Windows names ending in a dot or space, or reserved device names: {name:?}"
            ),
            None,
        ));
    }
    #[cfg(not(windows))]
    let _ = (stage, name);
    Ok(())
}

#[cfg(any(windows, test))]
fn unsupported_windows_name(name: &str) -> bool {
    if name.ends_with(['.', ' ']) {
        return true;
    }
    let upper = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    upper
        .strip_prefix("COM")
        .or_else(|| upper.strip_prefix("LPT"))
        .is_some_and(|number| {
            matches!(
                number,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
}

/// Run the lookup form, returning its record and exit status, or a
/// diagnostic for exit status 1.
pub fn run(args: &InspectArgs, lookup: &LookupArgs) -> Result<(Value, u8), String> {
    let root = resolve(&lookup.root)?;
    let mut file_bytes = None;
    let mut selected_path = None;
    let outcome = select(&root, lookup)
        .and_then(|selected| {
            selected.ok_or_else(|| {
                refused(
                    "lookup",
                    "not-found",
                    Kind::NotFound,
                    format!("no Session {:?} in the root", lookup.id),
                    None,
                )
            })
        })
        .and_then(|selected| {
            let relative = selected.relative();
            selected_path = Some(relative.clone());
            let path = selected.path(&root);
            if selected.version < CURRENT_SESSION_FORMAT_VERSION {
                return Err(refused(
                    "generation",
                    "migration-required",
                    Kind::NativeLimit,
                    format!(
                        "{relative} is a format v{} generation, which TypeScript migrates and this \
                     preview does not",
                        selected.version
                    ),
                    Some(relative),
                ));
            }
            if selected.version > CURRENT_SESSION_FORMAT_VERSION {
                // `readGenerationHeader` treats an absent file as a malformed
                // header, where the current read's `stat` fails.
                let bytes = match read_bounded_or_missing(&path, args.max_bytes) {
                    Ok(bytes) => bytes,
                    Err(ReadFailure::Missing(_)) => return Err(malformed_header(relative)),
                    Err(ReadFailure::Other(message)) => return Err(Stop::Failure(message)),
                };
                file_bytes = Some(bytes.len());
                return Err(newer_generation(&bytes, &selected, relative, lookup, args));
            }
            let bytes = read_bounded(&path, args.max_bytes).map_err(Stop::Failure)?;
            file_bytes = Some(bytes.len());
            let staged = stage(&bytes, lookup.encoding, args)
                .map_err(|refusal| restore_stage("scan", &refusal, args, &relative))?;
            check_identity(
                &root,
                &path,
                &selected,
                Some(&lookup.id),
                staged.header(),
                &relative,
            )?;
            staged
                .restore()
                .map_err(|refusal| restore_stage("restore", &refusal, args, &relative))
        });
    let mut record = Map::new();
    record.insert(
        "status".into(),
        json!(if outcome.is_ok() {
            "restored"
        } else {
            "refused"
        }),
    );
    record.insert("encoding".into(), json!(lookup.encoding.label()));
    record.insert(
        "formatVersion".into(),
        json!(CURRENT_SESSION_FORMAT_VERSION),
    );
    record.insert("fileBytes".into(), json!(file_bytes));
    record.insert("path".into(), json!(selected_path));
    match outcome {
        Ok(restored) => {
            record.extend(restored_fields(&restored));
            Ok((Value::Object(record), 0))
        }
        Err(Stop::Failure(message)) => Err(message),
        Err(Stop::Refused(refused)) => {
            record.insert("refusal".into(), refused.to_json());
            Ok((Value::Object(record), 3))
        }
    }
}

fn restore_stage(
    stage: &'static str,
    refusal: &RestoreRefusal,
    args: &InspectArgs,
    relative: &str,
) -> Stop {
    Stop::Refused(Box::new(Refused {
        stage,
        reason: None,
        refusal: describe(refusal, args),
        path: Some(relative.to_owned()),
    }))
}

/// `assertStoredIdentity`: the header names the `expected` id when one was
/// requested, and its id and `cwd` name the selected file, by spelling or
/// else by `realpath`. An empty header id names no path, which TypeScript's
/// `encodeSegment` refuses.
pub(crate) fn check_identity(
    root: &Path,
    path: &Path,
    selected: &Selected,
    expected: Option<&str>,
    header: &SessionHeader,
    relative: &str,
) -> Step<()> {
    if let Some(expected) = expected
        && header.id != expected
    {
        return Err(refused(
            "identity",
            "id-mismatch",
            Kind::Invalid,
            format!(
                "{relative} has header id {:?}, not the requested {expected:?}",
                header.id
            ),
            Some(relative.to_owned()),
        ));
    }
    if header.id.is_empty() {
        return Err(refused(
            "identity",
            "unencodable-id",
            Kind::Invalid,
            format!("{relative} has an empty header id, which cannot name a storage path"),
            Some(relative.to_owned()),
        ));
    }
    let project = header
        .cwd
        .as_deref()
        .map_or_else(|| "_no-cwd".into(), project_key);
    let expected = root
        .join(&project)
        .join(encode_segment(&header.id))
        .join(&selected.name);
    if expected == path || same_file(path, &expected)? {
        return Ok(());
    }
    Err(refused(
        "identity",
        "path-mismatch",
        Kind::Invalid,
        format!(
            "{relative} has a header whose id and cwd name {project}/{}/{}",
            encode_segment(&header.id),
            selected.name
        ),
        Some(relative.to_owned()),
    ))
}

/// Whether both paths resolve to one file; an absent path resolves to none.
fn same_file(path: &Path, expected: &Path) -> Step<bool> {
    let resolve = |path: &Path| match fs::canonicalize(path) {
        Ok(resolved) => Ok(Some(resolved)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Stop::Failure(format!("cannot resolve {path:?}: {error}"))),
    };
    let (actual, expected) = (resolve(path)?, resolve(expected)?);
    Ok(actual.is_some() && actual == expected)
}

fn malformed_header(relative: String) -> Stop {
    refused(
        "generation",
        "malformed-header",
        Kind::Invalid,
        format!("{relative} has a malformed header"),
        Some(relative),
    )
}

/// `readGenerationHeader` for a newer generation: every outcome refuses.
/// Retired fields are checked before the version, and a valid version other
/// than the file name's is a mismatch.
fn newer_generation(
    bytes: &[u8],
    selected: &Selected,
    relative: String,
    lookup: &LookupArgs,
    args: &InspectArgs,
) -> Stop {
    let path = Some(relative.clone());
    let malformed = || malformed_header(relative.clone());
    let native = |message: &str| {
        Stop::Refused(Box::new(Refused {
            stage: "generation",
            reason: None,
            refusal: Refusal::new(Kind::NativeLimit, message.into()),
            path: path.clone(),
        }))
    };
    let record = match lookup.encoding {
        Encoding::None => first_record(bytes).map(<[u8]>::to_vec),
        Encoding::Zstd => match zstd_header_record(bytes, plaintext_budget(args)) {
            Ok(record) => record,
            Err(refusal) => return restore_stage("generation", &refusal, args, &relative),
        },
    };
    let Some(record) = record else {
        return malformed();
    };
    // The current header reader decides only whether the line is JSON;
    // its version and field checks run in another order.
    match read_header_record(&record, PathPlatform::host()) {
        Err(HeaderRefusal::Rejected(Rejection::Json)) => return malformed(),
        Err(HeaderRefusal::NativeSubset(SubsetLimit::InvalidUtf8)) => {
            return native("this preview requires valid UTF-8 in the session header");
        }
        Err(HeaderRefusal::NativeSubset(SubsetLimit::JsonParser)) => {
            return native("the session header uses JSON this preview does not read");
        }
        _ => {}
    }
    let value: Value =
        serde_json::from_slice(&record).expect("the header reader parsed this record");
    if let Value::Object(fields) = &value
        && (fields.contains_key("sandboxMode") || fields.contains_key("approvalPolicy"))
    {
        return refused(
            "generation",
            "retired-header-fields",
            Kind::Invalid,
            format!("{relative} has a header with retired policy baseline fields"),
            path,
        );
    }
    let version = match value.get("version") {
        Some(Value::Number(number)) if value.is_object() => {
            if let Some(version) = number.as_u64() {
                if version > MAX_BUDGET {
                    return malformed();
                }
                version
            } else if number.is_i64() || number.as_f64().is_some_and(f64::is_sign_negative) {
                return malformed();
            } else {
                return native(
                    "the session header's version is written with a fraction or an exponent, or \
                     does not fit in 64 bits, which this preview does not read",
                );
            }
        }
        _ => return malformed(),
    };
    if version != selected.version {
        return refused(
            "generation",
            "generation-header-mismatch",
            Kind::Invalid,
            format!(
                "{relative} is named format v{}, but its header records v{version}",
                selected.version
            ),
            path,
        );
    }
    // TypeScript names the refused Session with `String(id)`, which can
    // throw for an object or an array, as `SubsetLimit::VersionDiagnostic`
    // records for the current header reader.
    if matches!(value["id"], Value::Object(_) | Value::Array(_)) {
        return native(
            "this preview cannot report a header of a newer format whose id is an object or array",
        );
    }
    refused(
        "generation",
        "newer-format",
        Kind::Unsupported,
        format!(
            "{relative} uses Session format v{version}, newer than the format \
             {CURRENT_SESSION_FORMAT_VERSION} this preview reads"
        ),
        path,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../conformance/session/lookup-cases.json");
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn encodings_match_the_shared_vectors() {
        let table = table();
        let pairs = |key: &str| -> Vec<(String, String)> {
            table[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|pair| {
                    let text = |index: usize| pair[index].as_str().unwrap().to_owned();
                    (text(0), text(1))
                })
                .collect()
        };
        let segments = pairs("segments");
        let keys = pairs("projectKeys");
        assert_eq!((segments.len(), keys.len()), (15, 19));
        for (raw, encoded) in segments {
            assert_eq!(encode_segment(&raw), encoded, "{raw:?}");
        }
        for (cwd, key) in keys {
            assert_eq!(project_key(&cwd), key, "{cwd:?}");
        }
    }

    #[test]
    fn windows_name_limits_and_reparse_classification_are_explicit() {
        for name in [
            "a.", "a ", "CON", "nul.txt", "CoM1", "LPT9.log", "COM¹", "CONIN$",
        ] {
            assert!(unsupported_windows_name(name), "{name}");
        }
        for name in [
            "a",
            "a.b",
            "NULx",
            "COM0",
            "COM10",
            "LPT10",
            "~002E",
            "~002E~002E",
        ] {
            assert!(!unsupported_windows_name(name), "{name}");
        }
        for (attributes, expected) in [
            (0, false),
            (0x10, false),
            (0x400, true),
            (0x410, true),
            (0x420, true),
        ] {
            assert_eq!(is_reparse_point(attributes), expected);
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn the_root_resolves_lexically_like_node() {
        let cwd = std::env::current_dir().unwrap();
        for (root, want) in [
            ("/a/b/../c", PathBuf::from("/a/c")),
            ("/a/./b/.", PathBuf::from("/a/b")),
            ("/../..", PathBuf::from("/")),
            ("//a//b/", PathBuf::from("/a/b")),
            ("x/../y", cwd.join("y")),
            ("", cwd.clone()),
            (".", cwd.clone()),
        ] {
            assert_eq!(resolve_root(OsStr::new(root)).unwrap(), want, "{root:?}");
        }
    }
}
