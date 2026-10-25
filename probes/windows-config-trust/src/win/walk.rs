//! The proposed read walk (design §5), read-only.
//!
//! Each component is opened relative to the already-checked parent handle,
//! never following a reparse point, and checked on the handle. Only
//! `STATUS_OBJECT_NAME_NOT_FOUND` from such an open means absent; every
//! other failure refuses.

use std::ffi::OsStr;
use std::os::windows::io::OwnedHandle;
use std::path::Path;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{NTSTATUS, OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE};
use windows_sys::Win32::Storage::FileSystem::{GetFullPathNameW, ReadFile};

use super::{
    DIR_ACCESS, DIR_SHARE, FILE_PERSISTENT_ACLS, FILE_READ_ATTRIBUTES, FILE_DEVICE_DISK,
    FILE_REMOTE_DEVICE, Facts, LIMIT, Open, SHARE_ALL, SYNCHRONIZE, descriptor_json, facts,
    facts_json, h, hex_status, last_error, nt_open, open_path, security, status, volume,
    volume_fields, wide,
};
use crate::report::Line;
use crate::sd::{self, Kind, Place, Principals};

/// How a walk ended.
#[derive(Debug)]
pub(crate) enum Outcome {
    /// The file's bytes, read through a checked handle.
    Trusted { bytes: Vec<u8>, links: u32 },
    /// A directory-only walk passed every check.
    Reached,
    /// A checked open reported `STATUS_OBJECT_NAME_NOT_FOUND`.
    Absent { at: String },
    Refused { at: String, reason: String, status: Option<NTSTATUS> },
}

impl Outcome {
    pub fn name(&self) -> &'static str {
        match self {
            Outcome::Trusted { .. } => "trusted",
            Outcome::Reached => "reached",
            Outcome::Absent { .. } => "absent",
            Outcome::Refused { .. } => "refused",
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Outcome::Trusted { bytes, links } => format!("trusted ({} bytes, {links} links)", bytes.len()),
            Outcome::Reached => "reached".into(),
            Outcome::Absent { at } => format!("absent at {at}"),
            Outcome::Refused { at, reason, status: Some(code) } => {
                format!("refused at {at}: {reason} ({})", hex_status(*code))
            }
            Outcome::Refused { at, reason, status: None } => format!("refused at {at}: {reason}"),
        }
    }

    pub fn refusal(&self) -> Option<&str> {
        match self {
            Outcome::Refused { reason, .. } => Some(reason),
            _ => None,
        }
    }
}

/// A finished walk and the directory handles it still holds, in order.
pub(crate) struct Walk {
    pub outcome: Outcome,
    pub held: Vec<OwnedHandle>,
}

/// Normalizes `path` with Win32 rules and splits `X:\a\b` into the drive
/// letter and components; refuses every other form.
fn split_drive_path(path: &str) -> Result<(char, Vec<String>), String> {
    let input = wide(OsStr::new(path));
    let mut buffer = vec![0u16; 32_768];
    // SAFETY: NUL-terminated input and an output buffer of the given size.
    let length =
        unsafe { GetFullPathNameW(input.as_ptr(), buffer.len() as u32, buffer.as_mut_ptr(), null_mut()) } as usize;
    if length == 0 || length >= buffer.len() {
        return Err(format!("GetFullPathNameW failed: {}", last_error()));
    }
    let full = String::from_utf16(&buffer[..length]).map_err(|_| "not Unicode".to_owned())?;
    let rest = full.strip_prefix(r"\\?\").unwrap_or(&full);
    let mut chars = rest.chars();
    let (Some(letter), Some(':'), Some('\\')) = (chars.next(), chars.next(), chars.next()) else {
        return Err(format!("{full} is not a drive-letter path (UNC, device, and volume-GUID paths are unsupported)"));
    };
    if !letter.is_ascii_alphabetic() {
        return Err(format!("{full} is not a drive-letter path"));
    }
    let components: Vec<String> =
        chars.as_str().split('\\').filter(|part| !part.is_empty()).map(str::to_owned).collect();
    for part in &components {
        if part == "." || part == ".." || part.contains([':', '/', '\0']) {
            return Err(format!("{full} has an unsupported component {part:?}"));
        }
    }
    Ok((letter.to_ascii_uppercase(), components))
}

/// Checks an opened object; returns its facts or why it is refused.
fn step(
    line: &mut Line,
    handle: &OwnedHandle,
    kind: Kind,
    place: Place,
    principals: &Principals,
    root_serial: Option<u64>,
) -> Result<Facts, String> {
    let facts = facts(handle);
    line.raw("facts", &facts_json(&facts));
    if let Some(missing) = facts.missing() {
        return Err(missing);
    }
    if !facts.disk {
        return Err("not a disk file".into());
    }
    if facts.is_reparse() {
        return Err(format!("is a reparse point (tag {:#x})", facts.reparse_tag));
    }
    if facts.is_directory() != (kind == Kind::Directory) {
        return Err("has the wrong type".into());
    }
    if root_serial.is_some() && facts.volume_serial != root_serial {
        return Err("is on another volume".into());
    }
    if facts.delete_pending {
        return Err("is being deleted".into());
    }
    let bytes = security(handle).map_err(|error| format!("GetKernelObjectSecurity failed: {error}"))?;
    line.raw("descriptor", &descriptor_json(&bytes));
    sd::evaluate(&bytes, kind, place, principals).map_err(|refusal| refusal.to_string())?;
    Ok(facts)
}

/// The proposed relative open, retried once without `OBJ_DONT_REPARSE` if
/// the system rejects the flag (as Rust std does); safe only because every
/// name is one component and `FILE_OPEN_REPARSE_POINT` stays set.
pub(crate) fn open_child(line: &mut Line, parent: &OwnedHandle, name: &str, directory: bool) -> Result<OwnedHandle, NTSTATUS> {
    let open = Open::proposed(directory);
    match nt_open(parent, name, open) {
        Err(code) if code == status::INVALID_PARAMETER => {
            line.bool("dontReparseRejected", true);
            nt_open(parent, name, Open { attributes: OBJ_CASE_INSENSITIVE, ..open })
        }
        other => other,
    }
}

/// Tri-state: whether `name` exists in `dir`, without following it.
pub(crate) fn has_entry(dir: &OwnedHandle, name: &str) -> Result<bool, NTSTATUS> {
    let open = Open {
        directory: false,
        access: FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        share: SHARE_ALL,
        attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        open_reparse: true,
    };
    match nt_open(dir, name, open) {
        Ok(_) => Ok(true),
        // FILE_NON_DIRECTORY_FILE on a directory: present.
        Err(code) if code == status::FILE_IS_A_DIRECTORY => Ok(true),
        Err(code) if code == status::OBJECT_NAME_NOT_FOUND => Ok(false),
        Err(code) => Err(code),
    }
}

fn refused(at: &str, reason: impl Into<String>, status: Option<NTSTATUS>) -> Outcome {
    Outcome::Refused { at: at.to_owned(), reason: reason.into(), status }
}

/// Walks `components` below `start`, then reads `file` if given.
/// Components at index `managed_from` and later are checked as `Managed`,
/// earlier ones as `Ancestor`; a `.git` entry refuses a managed directory.
pub(crate) fn walk_from(
    start: &OwnedHandle,
    root_serial: Option<u64>,
    shown: &str,
    components: &[String],
    managed_from: usize,
    file: Option<&str>,
    principals: &Principals,
) -> Walk {
    let mut held: Vec<OwnedHandle> = Vec::new();
    let mut shown = shown.to_owned();
    for (index, name) in components.iter().enumerate() {
        shown.push('\\');
        shown.push_str(name);
        let place = if index >= managed_from { Place::Managed } else { Place::Ancestor };
        let mut line = Line::new("walk-dir");
        line.str("path", &shown).str("place", &format!("{place:?}"));
        let parent = held.last().unwrap_or(start);
        let handle = match open_child(&mut line, parent, name, true) {
            Err(code) if code == status::OBJECT_NAME_NOT_FOUND => {
                line.str("result", "absent").emit();
                return Walk { outcome: Outcome::Absent { at: shown }, held };
            }
            Err(code) => {
                line.str("ntstatus", &hex_status(code)).str("result", "refused").emit();
                return Walk { outcome: refused(&shown, "open failed; not treated as absence", Some(code)), held };
            }
            Ok(handle) => handle,
        };
        if let Err(reason) = step(&mut line, &handle, Kind::Directory, place, principals, root_serial) {
            line.str("refused", &reason).emit();
            return Walk { outcome: refused(&shown, reason, None), held };
        }
        match has_entry(&handle, ".git") {
            Ok(git) => {
                line.bool("hasGit", git);
                if git && place == Place::Managed {
                    line.str("refused", "holds a .git entry").emit();
                    return Walk { outcome: refused(&shown, "holds a .git entry", None), held };
                }
            }
            Err(code) => {
                line.str("gitProbeStatus", &hex_status(code)).str("refused", "the .git probe failed").emit();
                return Walk { outcome: refused(&shown, "the .git probe failed", Some(code)), held };
            }
        }
        line.emit();
        held.push(handle);
    }
    let Some(name) = file else {
        return Walk { outcome: Outcome::Reached, held };
    };
    let shown = format!("{shown}\\{name}");
    let outcome = read_file(held.last().unwrap_or(start), &shown, name, principals, root_serial);
    Walk { outcome, held }
}

fn read_file(dir: &OwnedHandle, shown: &str, name: &str, principals: &Principals, root_serial: Option<u64>) -> Outcome {
    let mut line = Line::new("walk-file");
    line.str("path", shown);
    let file = match open_child(&mut line, dir, name, false) {
        Err(code) if code == status::OBJECT_NAME_NOT_FOUND => {
            line.str("result", "absent").emit();
            return Outcome::Absent { at: shown.to_owned() };
        }
        Err(code) => {
            line.str("ntstatus", &hex_status(code)).str("result", "refused").emit();
            return refused(shown, "open failed; not treated as absence", Some(code));
        }
        Ok(file) => file,
    };
    let facts = match step(&mut line, &file, Kind::File, Place::Managed, principals, root_serial) {
        Ok(facts) => facts,
        Err(reason) => {
            line.str("refused", &reason).emit();
            return refused(shown, reason, None);
        }
    };
    if facts.end_of_file < 0 || facts.end_of_file as u64 > LIMIT {
        line.str("refused", "too large").emit();
        return refused(shown, "too large", None);
    }
    let mut bytes = vec![0u8; (LIMIT + 1) as usize];
    let mut total = 0usize;
    while total < bytes.len() {
        let mut read = 0u32;
        let want = (bytes.len() - total).min(1 << 16) as u32;
        // SAFETY: a live synchronous handle and the unread part of `bytes`.
        let ok = unsafe { ReadFile(h(&file), bytes[total..].as_mut_ptr(), want, &mut read, null_mut()) };
        if ok == 0 {
            let error = last_error();
            line.num("readError", error).str("refused", "read failed").emit();
            return refused(shown, format!("read failed: {error}"), None);
        }
        if read == 0 {
            break;
        }
        total += read as usize;
    }
    line.num("bytesRead", total);
    if total as u64 > LIMIT || total as i64 != facts.end_of_file {
        line.str("refused", "the bytes read do not match the checked size").emit();
        return refused(shown, "the bytes read do not match the checked size", None);
    }
    bytes.truncate(total);
    line.str("result", "trusted").emit();
    Outcome::Trusted { bytes, links: facts.links }
}

/// Opens the volume root of `letter` and checks it as design §5 step 3.
fn open_root(letter: char, principals: &Principals) -> Result<(OwnedHandle, Option<u64>), Outcome> {
    let root_path = format!(r"\\?\{letter}:\");
    let mut line = Line::new("walk-root");
    line.str("path", &root_path);
    let root = match open_path(Path::new(&root_path), DIR_ACCESS, DIR_SHARE) {
        Ok(root) => root,
        Err(error) => {
            line.num("openError", error).emit();
            return Err(refused(&root_path, format!("cannot open the volume root: {error}"), None));
        }
    };
    let volume = volume(&root);
    volume_fields(&mut line, &volume);
    let mut refusal = volume.error.clone();
    if volume.flags & FILE_PERSISTENT_ACLS == 0 {
        refusal.get_or_insert("the volume does not keep ACLs".to_owned());
    } else if volume.device_type != FILE_DEVICE_DISK || volume.characteristics & FILE_REMOTE_DEVICE != 0 {
        refusal.get_or_insert("the volume is not a local disk".to_owned());
    } else if !matches!(volume.file_system.as_str(), "NTFS" | "ReFS") {
        refusal.get_or_insert(format!("the file system {} is not qualified", volume.file_system));
    }
    let mut serial = None;
    match step(&mut line, &root, Kind::Directory, Place::VolumeRoot, principals, None) {
        Ok(facts) => {
            serial = facts.volume_serial;
            if facts.name.as_deref() != Some("\\") {
                refusal.get_or_insert("the drive letter does not name a volume root (subst?)".to_owned());
            }
        }
        Err(reason) => {
            refusal.get_or_insert(reason);
        }
    }
    if let Some(reason) = refusal {
        line.str("refused", &reason).emit();
        return Err(refused(&root_path, reason, None));
    }
    line.emit();
    Ok((root, serial))
}

/// `walk <home> [<file>]`: always exits 0 once it reports an outcome; the
/// outcome is evidence, not a verdict.
pub(crate) fn walk_command(home: &str, file: Option<&str>, principals: &Principals) -> i32 {
    let mut done = Line::new("walk-result");
    done.str("home", home);
    let outcome = match split_drive_path(home) {
        Err(reason) => refused(home, reason, None),
        Ok((letter, components)) => match open_root(letter, principals) {
            Err(outcome) => outcome,
            Ok((root, serial)) => {
                let managed_from = components.len().saturating_sub(1);
                walk_from(&root, serial, &format!("{letter}:"), &components, managed_from, file, principals).outcome
            }
        },
    };
    done.str("outcome", outcome.name()).str("detail", &outcome.describe()).emit();
    0
}
