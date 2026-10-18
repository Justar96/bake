//! The Windows side of the probe: the proposed walk, read-only dumps, and
//! mechanism checks on objects the probe creates.
//!
//! Every `unsafe` block is an FFI call with its contract noted.

mod ledger;
mod mechanisms;
mod walk;

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT,
    FILE_SYNCHRONOUS_IO_NONALERT, FileFsDeviceInformation, NtCreateFile,
    NtQueryVolumeInformationFile,
};
use windows_sys::Wdk::System::SystemServices::FILE_FS_DEVICE_INFORMATION;
use windows_sys::Win32::Foundation::{
    GetLastError, HANDLE, INVALID_HANDLE_VALUE, NTSTATUS, OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE,
    UNICODE_STRING,
};
use windows_sys::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetKernelObjectSecurity, GetLengthSid, GetTokenInformation,
    LABEL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_TAG_INFO, FILE_CASE_SENSITIVE_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_NAME_INFO,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, FILE_TYPE_DISK,
    FileAttributeTagInfo, FileCaseSensitiveInfo, FileIdInfo, FileNameInfo, FileStandardInfo,
    GetFileInformationByHandleEx, GetFileType, GetVolumeInformationByHandleW, OPEN_EXISTING,
    SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::report::{Line, exit};
use crate::sd::{self, Kind, Place, Principals, Sid};

// Access rights (winnt.h).
pub(crate) const DELETE: u32 = 0x1_0000;
pub(crate) const READ_CONTROL: u32 = 0x2_0000;
pub(crate) const WRITE_DAC: u32 = 0x4_0000;
pub(crate) const WRITE_OWNER: u32 = 0x8_0000;
pub(crate) const SYNCHRONIZE: u32 = 0x10_0000;
pub(crate) const FILE_READ_DATA: u32 = 0x1;
pub(crate) const FILE_WRITE_DATA: u32 = 0x2;
pub(crate) const FILE_TRAVERSE: u32 = 0x20;
pub(crate) const FILE_READ_ATTRIBUTES: u32 = 0x80;
pub(crate) const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
pub(crate) const GENERIC_WRITE: u32 = 0x4000_0000;
const TOKEN_QUERY: u32 = 0x8;
pub(crate) const FILE_PERSISTENT_ACLS: u32 = 0x8;
pub(crate) const FILE_DEVICE_DISK: u32 = 0x7;
pub(crate) const FILE_REMOTE_DEVICE: u32 = 0x10;
const FILE_CS_FLAG_CASE_SENSITIVE_DIR: u32 = 0x1;
pub(crate) const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
pub(crate) const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

/// NTSTATUS values the probe distinguishes (ntstatus.h).
pub(crate) mod status {
    use windows_sys::Win32::Foundation::NTSTATUS;
    const fn s(value: u32) -> NTSTATUS {
        value as NTSTATUS
    }
    pub const INVALID_PARAMETER: NTSTATUS = s(0xC000_000D);
    pub const ACCESS_DENIED: NTSTATUS = s(0xC000_0022);
    pub const OBJECT_NAME_INVALID: NTSTATUS = s(0xC000_0033);
    pub const OBJECT_NAME_NOT_FOUND: NTSTATUS = s(0xC000_0034);
    pub const SHARING_VIOLATION: NTSTATUS = s(0xC000_0043);
    pub const DELETE_PENDING: NTSTATUS = s(0xC000_0056);
    pub const FILE_IS_A_DIRECTORY: NTSTATUS = s(0xC000_00BA);
    pub const NOT_A_DIRECTORY: NTSTATUS = s(0xC000_0103);
    pub const REPARSE_POINT_ENCOUNTERED: NTSTATUS = s(0xC000_050B);
}

/// Win32 error codes the probe distinguishes (winerror.h).
pub(crate) mod error {
    pub const FILE_NOT_FOUND: u32 = 2;
    pub const PATH_NOT_FOUND: u32 = 3;
    pub const ACCESS_DENIED: u32 = 5;
    pub const SHARING_VIOLATION: u32 = 32;
    pub const ALREADY_EXISTS: u32 = 183;
    pub const NO_TOKEN: u32 = 1008;
    pub const PRIVILEGE_NOT_HELD: u32 = 1314;
}

/// The proposed handle access and sharing, per object kind (design §5, §7).
pub(crate) const DIR_ACCESS: u32 = READ_CONTROL | FILE_READ_ATTRIBUTES | FILE_TRAVERSE | SYNCHRONIZE;
pub(crate) const DIR_SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;
pub(crate) const FILE_ACCESS: u32 = READ_CONTROL | FILE_READ_ATTRIBUTES | FILE_READ_DATA | SYNCHRONIZE;
pub(crate) const FILE_SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_DELETE;
pub(crate) const LIMIT: u64 = 1 << 20;

pub(crate) fn last_error() -> u32 {
    // SAFETY: no preconditions.
    unsafe { GetLastError() }
}

pub(crate) fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(Some(0)).collect()
}

pub(crate) fn owned(raw: HANDLE) -> OwnedHandle {
    // SAFETY: callers pass a fresh, valid handle owned only here.
    unsafe { OwnedHandle::from_raw_handle(raw) }
}

pub(crate) fn h(handle: &OwnedHandle) -> HANDLE {
    handle.as_raw_handle()
}

pub(crate) fn hex_status(status: NTSTATUS) -> String {
    format!("{:#010x}", status as u32)
}

// ---------------------------------------------------------------------------
// Opening.

/// `CreateFileW(OPEN_EXISTING)` that opens a final reparse point itself
/// rather than following it.
pub(crate) fn open_path(path: &Path, access: u32, share: u32) -> Result<OwnedHandle, u32> {
    let name = wide(path.as_os_str());
    // SAFETY: a NUL-terminated path; the result is checked.
    let raw = unsafe {
        CreateFileW(
            name.as_ptr(),
            access,
            share,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS
                | FILE_FLAG_OPEN_REPARSE_POINT
                | SECURITY_SQOS_PRESENT
                | SECURITY_IDENTIFICATION,
            null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE { Err(last_error()) } else { Ok(owned(raw)) }
}

/// How [`nt_open`] opens a name.
#[derive(Clone, Copy)]
pub(crate) struct Open {
    pub directory: bool,
    pub access: u32,
    pub share: u32,
    /// `OBJECT_ATTRIBUTES.Attributes`.
    pub attributes: u32,
    /// `FILE_OPEN_REPARSE_POINT`: open a final reparse point itself.
    pub open_reparse: bool,
}

impl Open {
    /// The proposed relative open (design §5): no reparse following.
    pub fn proposed(directory: bool) -> Self {
        let (access, share) = if directory { (DIR_ACCESS, DIR_SHARE) } else { (FILE_ACCESS, FILE_SHARE) };
        Open { directory, access, share, attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE, open_reparse: true }
    }
}

/// `NtCreateFile(FILE_OPEN)` of `name` relative to `parent`.
pub(crate) fn nt_open(parent: &OwnedHandle, name: &str, open: Open) -> Result<OwnedHandle, NTSTATUS> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let bytes = u16::try_from(units.len() * 2).map_err(|_| status::OBJECT_NAME_INVALID)?;
    let string = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: units.as_ptr().cast_mut() };
    let object = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: h(parent),
        ObjectName: &string,
        Attributes: open.attributes,
        SecurityDescriptor: null(),
        SecurityQualityOfService: null(),
    };
    let mut status_block = IO_STATUS_BLOCK::default();
    let mut raw: HANDLE = null_mut();
    let options = (if open.directory { FILE_DIRECTORY_FILE } else { FILE_NON_DIRECTORY_FILE })
        | FILE_SYNCHRONOUS_IO_NONALERT
        | if open.open_reparse { FILE_OPEN_REPARSE_POINT } else { 0 };
    // SAFETY: every pointer is valid for the call; the name buffer and
    // string outlive it; the handle is owned on success.
    let result = unsafe {
        NtCreateFile(
            &mut raw,
            open.access,
            &object,
            &mut status_block,
            null(),
            0,
            open.share,
            FILE_OPEN,
            options,
            null(),
            0,
        )
    };
    if result >= 0 { Ok(owned(raw)) } else { Err(result) }
}

// ---------------------------------------------------------------------------
// Facts about an opened object.

/// What the handle reports. A query that fails keeps its error, so a
/// failed query is never mistaken for "no reparse point" or "a file".
#[derive(Default)]
pub(crate) struct Facts {
    pub disk: bool,
    pub attributes: u32,
    pub reparse_tag: u32,
    pub tag_error: Option<u32>,
    pub end_of_file: i64,
    pub links: u32,
    pub delete_pending: bool,
    pub directory: bool,
    pub standard_error: Option<u32>,
    pub volume_serial: Option<u64>,
    pub file_id: Option<[u8; 16]>,
    pub id_error: Option<u32>,
    pub name: Option<String>,
    pub case_sensitive: Option<bool>,
}

impl Facts {
    /// The first required query that failed.
    pub fn missing(&self) -> Option<String> {
        if let Some(error) = self.tag_error {
            return Some(format!("FileAttributeTagInfo failed: {error}"));
        }
        if let Some(error) = self.standard_error {
            return Some(format!("FileStandardInfo failed: {error}"));
        }
        if let Some(error) = self.id_error {
            return Some(format!("FileIdInfo failed: {error}"));
        }
        None
    }

    pub fn is_reparse(&self) -> bool {
        self.attributes & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT != 0
            || self.reparse_tag != 0
    }

    pub fn is_directory(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }

    /// `(VolumeSerialNumber, FileId)`, the identity of an open object.
    pub fn identity(&self) -> Option<(u64, [u8; 16])> {
        Some((self.volume_serial?, self.file_id?))
    }
}

fn info<T: Default>(handle: &OwnedHandle, class: i32) -> Result<T, u32> {
    let mut value = T::default();
    // SAFETY: `value` is a `T` sized buffer for the class that returns a `T`.
    let ok = unsafe {
        GetFileInformationByHandleEx(h(handle), class, (&mut value as *mut T).cast(), size_of::<T>() as u32)
    };
    if ok == 0 { Err(last_error()) } else { Ok(value) }
}

fn name_info(handle: &OwnedHandle) -> Result<String, u32> {
    let mut buffer = vec![0u64; 8 * 1024 / 8];
    // SAFETY: `buffer` holds 8 KiB, aligned for FILE_NAME_INFO.
    let ok = unsafe {
        GetFileInformationByHandleEx(h(handle), FileNameInfo, buffer.as_mut_ptr().cast(), (buffer.len() * 8) as u32)
    };
    if ok == 0 {
        return Err(last_error());
    }
    let info = buffer.as_ptr().cast::<FILE_NAME_INFO>();
    // SAFETY: the call filled a FILE_NAME_INFO; its length is in bytes and
    // checked against the buffer before the slice is formed.
    unsafe {
        let length = (*info).FileNameLength as usize / 2;
        if length * 2 + 4 > buffer.len() * 8 {
            return Err(u32::MAX);
        }
        let start = std::ptr::addr_of!((*info).FileName).cast::<u16>();
        Ok(String::from_utf16_lossy(std::slice::from_raw_parts(start, length)))
    }
}

pub(crate) fn facts(handle: &OwnedHandle) -> Facts {
    let mut facts = Facts {
        // SAFETY: a live handle.
        disk: unsafe { GetFileType(h(handle)) } == FILE_TYPE_DISK,
        ..Facts::default()
    };
    match info::<FILE_ATTRIBUTE_TAG_INFO>(handle, FileAttributeTagInfo) {
        Ok(tag) => {
            facts.attributes = tag.FileAttributes;
            facts.reparse_tag = tag.ReparseTag;
        }
        Err(error) => facts.tag_error = Some(error),
    }
    match info::<FILE_STANDARD_INFO>(handle, FileStandardInfo) {
        Ok(standard) => {
            facts.end_of_file = standard.EndOfFile;
            facts.links = standard.NumberOfLinks;
            facts.delete_pending = standard.DeletePending;
            facts.directory = standard.Directory;
        }
        Err(error) => facts.standard_error = Some(error),
    }
    match info::<FILE_ID_INFO>(handle, FileIdInfo) {
        Ok(id) => {
            facts.volume_serial = Some(id.VolumeSerialNumber);
            facts.file_id = Some(id.FileId.Identifier);
        }
        Err(error) => facts.id_error = Some(error),
    }
    facts.name = name_info(handle).ok();
    if facts.is_directory() {
        facts.case_sensitive = info::<FILE_CASE_SENSITIVE_INFO>(handle, FileCaseSensitiveInfo)
            .ok()
            .map(|info| info.Flags & FILE_CS_FLAG_CASE_SENSITIVE_DIR != 0);
    }
    facts
}

pub(crate) fn id_hex(id: &[u8; 16]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn facts_json(facts: &Facts) -> String {
    let mut line = Line::object();
    line.bool("disk", facts.disk)
        .hex("attributes", facts.attributes)
        .hex("reparseTag", facts.reparse_tag)
        .num("endOfFile", facts.end_of_file)
        .num("links", facts.links)
        .bool("deletePending", facts.delete_pending)
        .bool("directory", facts.directory);
    for (key, value) in [("tagError", facts.tag_error), ("standardError", facts.standard_error), ("idError", facts.id_error)] {
        if let Some(error) = value {
            line.num(key, error);
        }
    }
    if let Some(serial) = facts.volume_serial {
        line.str("volumeSerial", &format!("{serial:016x}"));
    }
    if let Some(id) = &facts.file_id {
        line.str("fileId", &id_hex(id));
    }
    if let Some(name) = &facts.name {
        line.str("volumeRelativeName", name);
    }
    if let Some(case) = facts.case_sensitive {
        line.bool("caseSensitive", case);
    }
    line.finish()
}

pub(crate) struct Volume {
    pub file_system: String,
    pub flags: u32,
    pub device_type: u32,
    pub characteristics: u32,
    pub error: Option<String>,
}

pub(crate) fn volume(handle: &OwnedHandle) -> Volume {
    let mut name = [0u16; 64];
    let mut flags = 0u32;
    let mut serial = 0u32;
    let mut component = 0u32;
    let mut out = Volume { file_system: String::new(), flags: 0, device_type: 0, characteristics: 0, error: None };
    // SAFETY: valid output buffers of the sizes given.
    let ok = unsafe {
        GetVolumeInformationByHandleW(
            h(handle),
            null_mut(),
            0,
            &mut serial,
            &mut component,
            &mut flags,
            name.as_mut_ptr(),
            name.len() as u32,
        )
    };
    if ok == 0 {
        out.error = Some(format!("GetVolumeInformationByHandleW: {}", last_error()));
    } else {
        let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        out.file_system = String::from_utf16_lossy(&name[..end]);
        out.flags = flags;
    }
    let mut device = FILE_FS_DEVICE_INFORMATION::default();
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: a live handle and a correctly sized output structure.
    let result = unsafe {
        NtQueryVolumeInformationFile(
            h(handle),
            &mut status_block,
            (&mut device as *mut FILE_FS_DEVICE_INFORMATION).cast(),
            size_of::<FILE_FS_DEVICE_INFORMATION>() as u32,
            FileFsDeviceInformation,
        )
    };
    if result < 0 {
        out.error.get_or_insert(format!("NtQueryVolumeInformationFile: {}", hex_status(result)));
    } else {
        out.device_type = device.DeviceType;
        out.characteristics = device.Characteristics;
    }
    out
}

pub(crate) fn volume_fields(line: &mut Line, volume: &Volume) {
    line.str("fileSystem", &volume.file_system)
        .hex("fsFlags", volume.flags)
        .hex("deviceType", volume.device_type)
        .hex("deviceCharacteristics", volume.characteristics);
    if let Some(error) = &volume.error {
        line.str("volumeError", error);
    }
}

/// The self-relative descriptor with owner, DACL, and label.
pub(crate) fn security(handle: &OwnedHandle) -> Result<Vec<u8>, u32> {
    let wanted = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION;
    let mut needed = 0u32;
    // SAFETY: a size query with no buffer.
    unsafe { GetKernelObjectSecurity(h(handle), wanted, null_mut(), 0, &mut needed) };
    if needed == 0 || needed > 1 << 20 {
        return Err(last_error());
    }
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
    // SAFETY: `buffer` holds at least `needed` bytes, aligned for the header.
    let ok = unsafe {
        GetKernelObjectSecurity(h(handle), wanted, buffer.as_mut_ptr().cast(), (buffer.len() * 8) as u32, &mut needed)
    };
    if ok == 0 {
        return Err(last_error());
    }
    let length = (needed as usize).min(buffer.len() * 8);
    // SAFETY: the first `length` bytes of the buffer are initialized.
    let bytes = unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), length) };
    Ok(bytes.to_vec())
}

fn token_user() -> Result<Sid, String> {
    let mut token: HANDLE = null_mut();
    // SAFETY: the pseudo-handle and an output slot.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(format!("OpenProcessToken: {}", last_error()));
    }
    let token = owned(token);
    let mut buffer = vec![0u64; 64];
    let mut needed = 0u32;
    // SAFETY: a 512-byte aligned buffer, enough for TOKEN_USER and a SID.
    let ok = unsafe {
        GetTokenInformation(h(&token), TokenUser, buffer.as_mut_ptr().cast(), (buffer.len() * 8) as u32, &mut needed)
    };
    if ok == 0 {
        return Err(format!("GetTokenInformation: {}", last_error()));
    }
    // SAFETY: the buffer holds a TOKEN_USER whose SID points into it, and
    // GetLengthSid bounds the SID; Sid::parse checks it again.
    unsafe {
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let length = GetLengthSid(user.User.Sid) as usize;
        let start = user.User.Sid.cast::<u8>().cast_const();
        let base = buffer.as_ptr().cast::<u8>();
        if start < base || start.add(length) > base.add(buffer.len() * 8) {
            return Err("the token user SID lies outside its buffer".into());
        }
        let bytes = std::slice::from_raw_parts(start, length);
        Sid::parse(bytes).map(|(sid, _)| sid).map_err(|m| m.0.to_owned())
    }
}

pub(crate) fn descriptor_json(bytes: &[u8]) -> String {
    match sd::parse(bytes) {
        Err(malformed) => {
            let mut line = Line::object();
            line.str("malformed", malformed.0);
            line.finish()
        }
        Ok(descriptor) => {
            let mut line = Line::object();
            line.hex("control", u32::from(descriptor.control));
            if let Some(owner) = &descriptor.owner {
                line.str("owner", &owner.to_string());
            }
            if let Some(rid) = descriptor.label_rid {
                line.hex("labelRid", rid);
            }
            match &descriptor.dacl {
                None => {
                    line.raw("dacl", "null");
                }
                Some(aces) => {
                    let entries: Vec<String> = aces
                        .iter()
                        .map(|ace| {
                            let mut entry = Line::object();
                            entry
                                .num("type", ace.ace_type)
                                .hex("flags", u32::from(ace.flags))
                                .hex("mask", ace.mask)
                                .str("sid", &ace.sid.as_ref().map(ToString::to_string).unwrap_or_default());
                            entry.finish()
                        })
                        .collect();
                    line.raw("dacl", &format!("[{}]", entries.join(",")));
                }
            }
            line.finish()
        }
    }
}

pub(crate) fn verdict_json(bytes: &[u8], kind: Kind, place: Place, principals: &Principals) -> String {
    let mut line = Line::object();
    match sd::evaluate(bytes, kind, place, principals) {
        Ok(passed) => {
            line.bool("ok", true).hex("toleratedForeign", passed.tolerated_foreign);
        }
        Err(refusal) => {
            line.bool("ok", false).str("refusal", &refusal.to_string());
        }
    }
    line.finish()
}

// ---------------------------------------------------------------------------
// dump: read-only.

/// Returns whether every path was opened and its descriptor read.
fn dump(paths: &[String], principals: &Principals) -> bool {
    let mut complete = true;
    for path in paths {
        let mut line = Line::new("dump");
        line.str("path", path);
        let handle = match open_path(Path::new(path), READ_CONTROL | FILE_READ_ATTRIBUTES | SYNCHRONIZE, SHARE_ALL) {
            Ok(handle) => handle,
            Err(error) => {
                complete = false;
                line.num("openError", error).emit();
                continue;
            }
        };
        let facts = facts(&handle);
        line.raw("facts", &facts_json(&facts));
        volume_fields(&mut line, &volume(&handle));
        match security(&handle) {
            Err(error) => {
                complete = false;
                line.num("securityError", error);
            }
            Ok(bytes) => {
                let kind = if facts.is_directory() { Kind::Directory } else { Kind::File };
                line.raw("descriptor", &descriptor_json(&bytes))
                    .raw("asManaged", &verdict_json(&bytes, kind, Place::Managed, principals))
                    .raw("asAncestor", &verdict_json(&bytes, kind, Place::Ancestor, principals))
                    .raw("asVolumeRoot", &verdict_json(&bytes, kind, Place::VolumeRoot, principals));
            }
        }
        line.emit();
    }
    complete
}

// ---------------------------------------------------------------------------

const USAGE: &str = "usage: probe dump <path>... | walk <home> [<file>] | mechanisms <base> <ledger>";

pub fn main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let user = match token_user() {
        Ok(user) => user,
        Err(error) => {
            eprintln!("cannot read the token user: {error}");
            return exit::USAGE;
        }
    };
    let mut line = Line::new("token");
    line.str("user", &user.to_string());
    line.emit();
    let principals = Principals::new(user.clone());
    match args.first().map(String::as_str) {
        Some("dump") if args.len() > 1 => {
            if dump(&args[1..], &principals) { 0 } else { exit::HARNESS_ERROR }
        }
        Some("walk") if args.len() == 2 || args.len() == 3 => {
            walk::walk_command(&args[1], args.get(2).map(String::as_str), &principals)
        }
        Some("mechanisms") if args.len() == 3 => {
            mechanisms::run(Path::new(&args[1]), Path::new(&args[2]), &principals, &user)
        }
        _ => {
            eprintln!("{USAGE}");
            exit::USAGE
        }
    }
}
