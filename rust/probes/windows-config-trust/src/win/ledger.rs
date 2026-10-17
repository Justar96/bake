//! The probe's owned root and its ledger.
//!
//! Ownership is proven only by exclusive creation: the ledger file is
//! created with `CREATE_NEW` and the root with `CreateDirectoryW`, which
//! fails if the name exists. The root gets a protected DACL granting only
//! the token user, SYSTEM, and Administrators. The random name only avoids
//! collisions.
//!
//! The ledger lives outside the root and is appended before and after each
//! creation, so it survives a failed run or a crash and names exactly what
//! this run created. Cleanup deletes only recorded objects inside the root,
//! by handle, after checking each one's file identity, children first, and
//! never follows a link: a junction or symbolic link is deleted itself.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::windows::io::OwnedHandle;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
use windows_sys::Win32::Security::{
    DACL_SECURITY_INFORMATION, LABEL_SECURITY_INFORMATION, OBJECT_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, SetKernelObjectSecurity,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FileDispositionInfo, FileDispositionInfoEx,
    SetFileInformationByHandle, WriteFile,
};
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;

use super::{
    DELETE, FILE_READ_ATTRIBUTES, GENERIC_WRITE, READ_CONTROL, SHARE_ALL, SYNCHRONIZE, WRITE_DAC, WRITE_OWNER,
    error, facts, h, id_hex, last_error, open_path, owned, wide,
};
use crate::owned::{deletion_order, within};
use crate::report::{Line, Tally, root_name};

pub(crate) type Identity = (u64, [u8; 16]);

struct Entry {
    path: PathBuf,
    identity: Option<Identity>,
    gone: bool,
}

pub(crate) struct Probe {
    pub root: PathBuf,
    pub user: String,
    pub tally: Tally,
    ledger: File,
    entries: Vec<Entry>,
}

/// A self-relative security descriptor from SDDL.
pub(crate) fn descriptor_from_sddl(sddl: &str) -> Result<Vec<u8>, String> {
    let text = wide(std::ffi::OsStr::new(sddl));
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    let mut size = 0u32;
    // SAFETY: NUL-terminated SDDL; the descriptor is LocalAlloc'd.
    if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(text.as_ptr(), SDDL_REVISION_1, &mut sd, &mut size) }
        == 0
    {
        return Err(format!("SDDL {sddl}: {}", last_error()));
    }
    // SAFETY: `size` bytes from the call; copied, then freed once.
    let bytes = unsafe { std::slice::from_raw_parts(sd.cast::<u8>(), size as usize).to_vec() };
    // SAFETY: allocated by the call.
    unsafe { LocalFree(sd) };
    Ok(bytes)
}

fn attributes(bytes: &mut [u8]) -> SECURITY_ATTRIBUTES {
    SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: bytes.as_mut_ptr().cast(),
        bInheritHandle: 0,
    }
}

/// 128 bits from the system RNG; the source is reported.
fn random_bits() -> ([u8; 16], &'static str) {
    let mut bytes = [0u8; 16];
    // SAFETY: a 16-byte output buffer; no algorithm handle with the
    // system-preferred flag.
    let result = unsafe { BCryptGenRandom(null_mut(), bytes.as_mut_ptr(), 16, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if result >= 0 {
        return (bytes, "BCryptGenRandom");
    }
    // Uniqueness only; exclusive creation still decides ownership.
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    (seed.to_le_bytes(), "clock fallback")
}

/// The identity of the object at `path` itself, never following a link.
pub(crate) fn identity_at(path: &Path) -> Result<Identity, u32> {
    let handle = open_path(path, FILE_READ_ATTRIBUTES | SYNCHRONIZE, SHARE_ALL)?;
    facts(&handle).identity().ok_or(u32::MAX)
}

impl Probe {
    /// Creates the ledger and the root, both exclusively.
    pub fn start(base: &Path, ledger_path: &Path, user: String) -> Result<Probe, String> {
        if !base.is_absolute() {
            return Err(format!("{} is not absolute", base.display()));
        }
        let ledger = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(ledger_path)
            .map_err(|e| format!("create ledger {}: {e}", ledger_path.display()))?;
        let (random, source) = random_bits();
        let root = base.join(root_name(std::process::id(), &random));
        let mut probe = Probe { root: root.clone(), user, tally: Tally::default(), ledger, entries: Vec::new() };
        let mut line = Line::new("ledger-start");
        line.str("root", &root.display().to_string())
            .str("ledger", &ledger_path.display().to_string())
            .num("pid", std::process::id())
            .str("randomSource", source);
        probe.log(line);
        let sddl = probe.private_sddl();
        probe.create_dir(&root, Some(&sddl))?;
        Ok(probe)
    }

    pub fn private_sddl(&self) -> String {
        format!("D:P(A;OICI;FA;;;{})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)", self.user)
    }

    /// Writes `line` to the ledger and to stdout. A ledger write failure is
    /// a harness error: the record of what to clean up would be incomplete.
    fn log(&mut self, line: Line) {
        let text = line.finish();
        println!("{text}");
        if let Err(e) = writeln!(self.ledger, "{text}") {
            self.tally.error("ledger", "write", &e.to_string());
        }
    }

    fn inside(&self, relative: &str) -> Result<PathBuf, String> {
        let path = self.root.join(relative);
        if path == self.root || !within(&self.root, &path) {
            return Err(format!("{relative} is not below the probe root"));
        }
        Ok(path)
    }

    fn intent(&mut self, path: &Path) {
        let mut line = Line::new("ledger-intent");
        line.str("path", &path.display().to_string());
        self.log(line);
    }

    fn created(&mut self, path: &Path, identity: Option<Identity>, how: &str) {
        let mut line = Line::new("ledger-created");
        line.str("path", &path.display().to_string()).str("how", how);
        if let Some((serial, id)) = &identity {
            line.str("volumeSerial", &format!("{serial:016x}")).str("fileId", &id_hex(id));
        }
        self.log(line);
        self.entries.push(Entry { path: path.to_owned(), identity, gone: false });
    }

    fn create_dir(&mut self, path: &Path, sddl: Option<&str>) -> Result<(), String> {
        let mut descriptor = sddl.map(descriptor_from_sddl).transpose()?;
        let security = descriptor.as_mut().map(|bytes| attributes(bytes));
        let name = wide(path.as_os_str());
        self.intent(path);
        // SAFETY: NUL-terminated path; the attributes point at a live
        // descriptor. Fails when the name exists, which proves ownership.
        let ok = unsafe { CreateDirectoryW(name.as_ptr(), security.as_ref().map_or(null(), |a| a as *const _)) };
        if ok == 0 {
            let code = last_error();
            let mut line = Line::new("ledger-not-created");
            line.str("path", &path.display().to_string()).num("error", code);
            self.log(line);
            return Err(format!("CreateDirectoryW {}: {code}", path.display()));
        }
        let identity = identity_at(path).ok();
        self.created(path, identity, "CreateDirectoryW");
        if identity.is_none() {
            return Err(format!("cannot read the identity of {}", path.display()));
        }
        Ok(())
    }

    pub fn mkdir(&mut self, relative: &str, sddl: Option<&str>) -> Result<PathBuf, String> {
        let path = self.inside(relative)?;
        self.create_dir(&path, sddl)?;
        Ok(path)
    }

    /// Creates a new file (`CREATE_NEW`) with `content`.
    pub fn file(&mut self, relative: &str, sddl: Option<&str>, content: &[u8]) -> Result<PathBuf, String> {
        let path = self.inside(relative)?;
        let mut descriptor = sddl.map(descriptor_from_sddl).transpose()?;
        let security = descriptor.as_mut().map(|bytes| attributes(bytes));
        let name = wide(path.as_os_str());
        self.intent(&path);
        // SAFETY: as above; CREATE_NEW never opens an existing object.
        let raw = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_WRITE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                0,
                security.as_ref().map_or(null(), |a| a as *const _),
                CREATE_NEW,
                0,
                null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            let code = last_error();
            let mut line = Line::new("ledger-not-created");
            line.str("path", &path.display().to_string()).num("error", code);
            self.log(line);
            return Err(format!("CreateFileW {}: {code}", path.display()));
        }
        let handle = owned(raw);
        self.created(&path, facts(&handle).identity(), "CreateFileW");
        let length = u32::try_from(content.len()).map_err(|_| "content too large".to_owned())?;
        let mut written = 0u32;
        // SAFETY: a live handle and the content buffer.
        if unsafe { WriteFile(h(&handle), content.as_ptr(), length, &mut written, null_mut()) } == 0 || written != length {
            return Err(format!("WriteFile {}: {}", path.display(), last_error()));
        }
        Ok(path)
    }

    /// Records an object a mechanism created by other means (a link, or a
    /// file created under another token) at `path`, if it exists.
    pub fn record(&mut self, path: &Path, how: &str) {
        if !within(&self.root, path) || path == self.root {
            self.tally.error("ledger", "record", &format!("{} is not below the probe root", path.display()));
            return;
        }
        if let Ok(identity) = identity_at(path) {
            self.created(path, Some(identity), how);
        }
    }

    /// Replaces the owner-set DACL (and, for `label`, the label) of an
    /// object this run created. Refuses anything else.
    pub fn set_security(&mut self, path: &Path, sddl: &str, label: bool) -> Result<(), String> {
        if !self.entries.iter().any(|entry| entry.path == path && !entry.gone) || path == self.root {
            return Err(format!("{} is not an object this run created below its root", path.display()));
        }
        let mut bytes = descriptor_from_sddl(sddl)?;
        let (access, info): (u32, OBJECT_SECURITY_INFORMATION) = if label {
            (WRITE_OWNER | READ_CONTROL, LABEL_SECURITY_INFORMATION)
        } else {
            (WRITE_DAC | READ_CONTROL, DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION)
        };
        let handle = open_path(path, access | SYNCHRONIZE, SHARE_ALL).map_err(|e| format!("open {}: {e}", path.display()))?;
        // SAFETY: a live handle with the needed right and a self-relative
        // descriptor that outlives the call.
        if unsafe { SetKernelObjectSecurity(h(&handle), info, bytes.as_mut_ptr().cast()) } == 0 {
            return Err(format!("SetKernelObjectSecurity {}: {}", path.display(), last_error()));
        }
        Ok(())
    }

    /// Brings the ledger in line with the disk: below the exclusive root
    /// only, without following links. A recorded object no longer at its
    /// path is marked gone; one whose identity changed (a rename replaced
    /// it) is rebound; an unrecorded object found there is adopted, since
    /// only this run and its own restricted token could create it (the
    /// root's DACL also admits SYSTEM and Administrators, which are out of
    /// scope as in design §2).
    pub fn reconcile(&mut self) {
        let mut found: Vec<PathBuf> = Vec::new();
        let mut pending = vec![self.root.clone()];
        while let Some(dir) = pending.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(e) => {
                    self.tally.cleanup_failure(&dir.display().to_string(), &format!("cannot list: {e}"));
                    continue;
                }
            };
            for entry in entries {
                match entry {
                    Ok(entry) => {
                        let path = entry.path();
                        // A junction or symbolic link reports is_symlink and
                        // is never descended into.
                        if entry.file_type().is_ok_and(|t| t.is_dir()) {
                            pending.push(path.clone());
                        }
                        found.push(path);
                    }
                    Err(e) => self.tally.cleanup_failure(&dir.display().to_string(), &format!("cannot list: {e}")),
                }
            }
        }
        let mut changes = Vec::new();
        for (index, entry) in self.entries.iter().enumerate() {
            if entry.gone || entry.path == self.root {
                continue;
            }
            if !found.contains(&entry.path) {
                changes.push((index, None));
            } else if let Ok(now) = identity_at(&entry.path)
                && Some(now) != entry.identity
            {
                changes.push((index, Some(now)));
            }
        }
        for (index, now) in changes {
            let entry = &mut self.entries[index];
            let mut line = Line::new(if now.is_some() { "ledger-rebound" } else { "ledger-gone" });
            line.str("path", &entry.path.display().to_string());
            match now {
                None => entry.gone = true,
                Some(identity) => entry.identity = Some(identity),
            }
            self.log(line);
        }
        for path in found {
            if !self.entries.iter().any(|entry| entry.path == path && !entry.gone) {
                let identity = identity_at(&path).ok();
                self.created(&path, identity, "adopted from the exclusive root");
            }
        }
    }

    /// Deletes every recorded object, children first and the root last, by
    /// handle after an identity check, and reports anything left.
    pub fn cleanup(&mut self) {
        self.reconcile();
        let live: Vec<(PathBuf, Option<Identity>)> =
            self.entries.iter().filter(|e| !e.gone).map(|e| (e.path.clone(), e.identity)).collect();
        let paths: Vec<&PathBuf> = live.iter().map(|(path, _)| path).collect();
        for index in deletion_order(&self.root, &paths) {
            let (path, identity) = &live[index];
            let shown = path.display().to_string();
            match delete_owned(path, *identity) {
                Ok(()) => {
                    let mut line = Line::new("ledger-deleted");
                    line.str("path", &shown);
                    self.log(line);
                }
                Err(detail) => self.tally.cleanup_failure(&shown, &detail),
            }
        }
        let root_absent = matches!(identity_at(&self.root), Err(code) if code == error::FILE_NOT_FOUND || code == error::PATH_NOT_FOUND);
        if !root_absent {
            self.tally.cleanup_failure(&self.root.display().to_string(), "the probe root still exists");
        }
        let mut line = Line::new("ledger-end");
        line.str("root", &self.root.display().to_string())
            .bool("rootAbsent", root_absent)
            .num("cleanupFailures", self.tally.cleanup_failures);
        self.log(line);
    }
}

fn set_delete(handle: &OwnedHandle) -> Result<(), u32> {
    let posix = FILE_DISPOSITION_INFO_EX { Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS };
    // SAFETY: a handle opened with DELETE and a correctly sized structure.
    let ok = unsafe {
        SetFileInformationByHandle(
            h(handle),
            FileDispositionInfoEx,
            (&posix as *const FILE_DISPOSITION_INFO_EX).cast(),
            size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    };
    if ok != 0 {
        return Ok(());
    }
    let first = last_error();
    // File systems without POSIX deletion reject the Ex class.
    if first != 87 && first != 50 {
        return Err(first);
    }
    let classic = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: as above.
    let ok = unsafe {
        SetFileInformationByHandle(
            h(handle),
            FileDispositionInfo,
            (&classic as *const FILE_DISPOSITION_INFO).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    };
    if ok != 0 { Ok(()) } else { Err(last_error()) }
}

/// Deletes the object at `path` itself if it is still `expected`.
fn delete_owned(path: &Path, expected: Option<Identity>) -> Result<(), String> {
    let handle = open_path(path, DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE, SHARE_ALL)
        .map_err(|e| format!("open for deletion: {e}"))?;
    let now = facts(&handle).identity();
    if expected.is_none() || now != expected {
        return Err("its identity is unknown or no longer the recorded one; not deleted".into());
    }
    set_delete(&handle).map_err(|e| format!("set disposition: {e}"))?;
    drop(handle);
    // A classic disposition removes the name only when the last handle,
    // possibly another process's, closes; wait for that, bounded.
    for _ in 0..20 {
        match identity_at(path) {
            Err(code) if code == error::FILE_NOT_FOUND || code == error::PATH_NOT_FOUND => return Ok(()),
            _ => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
    Err("still present 2 s after deletion".into())
}
