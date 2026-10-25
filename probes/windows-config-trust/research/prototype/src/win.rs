//! The Windows side of the probe: the proposed walk, read-only dumps, and
//! mechanism experiments on objects the probe creates.
//!
//! Every `unsafe` block is an FFI call with its contract noted. Output is
//! one JSON object per line on stdout.

use std::ffi::{OsStr, c_void};
use std::fmt::Write as _;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_DIRECTORY_FILE, FILE_LINK_INFORMATION, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
    FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT, FileFsDeviceInformation,
    FileLinkInformation, NtCreateFile, NtQueryVolumeInformationFile, NtSetInformationFile,
};
use windows_sys::Wdk::System::SystemServices::FILE_FS_DEVICE_INFORMATION;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree, NTSTATUS,
    OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE, STATUS_OBJECT_NAME_NOT_FOUND, UNICODE_STRING,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    CreateRestrictedToken, DACL_SECURITY_INFORMATION, DuplicateTokenEx, GetKernelObjectSecurity,
    GetLengthSid, GetTokenInformation, LABEL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, RevertToSelf, SECURITY_ATTRIBUTES, SID_AND_ATTRIBUTES,
    SecurityImpersonation, SetKernelObjectSecurity, TOKEN_USER, TokenImpersonation, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, CreateHardLinkW, CreateSymbolicLinkW, DeleteFileW,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
    FILE_CASE_SENSITIVE_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_ID_INFO, FILE_NAME_INFO, FILE_RENAME_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_STANDARD_INFO, FILE_TYPE_DISK, FileAttributeTagInfo,
    FileCaseSensitiveInfo, FileIdInfo, FileNameInfo, FileRenameInfoEx, FileStandardInfo,
    GetFileInformationByHandleEx, GetFileType, GetFullPathNameW, GetVolumeInformationByHandleW,
    MOVEFILE_REPLACE_EXISTING, MoveFileExW, OPEN_EXISTING, ReadFile, RemoveDirectoryW,
    SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE,
    SetFileInformationByHandle, WriteFile,
};
use windows_sys::Win32::System::IO::{DeviceIoControl, IO_STATUS_BLOCK};
use windows_sys::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken, SetThreadToken};

use crate::sd::{self, Kind, Place, Principals, Sid};

// Access rights (winnt.h).
const DELETE: u32 = 0x1_0000;
const READ_CONTROL: u32 = 0x2_0000;
const WRITE_DAC: u32 = 0x4_0000;
const SYNCHRONIZE: u32 = 0x10_0000;
const FILE_READ_DATA: u32 = 0x1;
const FILE_WRITE_DATA: u32 = 0x2;
const FILE_TRAVERSE: u32 = 0x20;
const FILE_READ_ATTRIBUTES: u32 = 0x80;
const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
const GENERIC_WRITE: u32 = 0x4000_0000;
const TOKEN_ASSIGN_PRIMARY: u32 = 0x1;
const TOKEN_DUPLICATE: u32 = 0x2;
const TOKEN_IMPERSONATE: u32 = 0x4;
const TOKEN_QUERY: u32 = 0x8;
const FILE_PERSISTENT_ACLS: u32 = 0x8;
const FILE_DEVICE_DISK: u32 = 0x7;
const FILE_REMOTE_DEVICE: u32 = 0x10;
const FILE_CS_FLAG_CASE_SENSITIVE_DIR: u32 = 0x1;
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
const STATUS_REPARSE_POINT_ENCOUNTERED: NTSTATUS = 0xC000_050B_u32 as i32;

/// The proposed handle access and sharing, per object kind.
const DIR_ACCESS: u32 = READ_CONTROL | FILE_READ_ATTRIBUTES | FILE_TRAVERSE | SYNCHRONIZE;
const DIR_SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;
const FILE_ACCESS: u32 = READ_CONTROL | FILE_READ_ATTRIBUTES | FILE_READ_DATA | SYNCHRONIZE;
const FILE_SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_DELETE;
const LIMIT: u64 = 1 << 20;

// ---------------------------------------------------------------------------
// Output.

struct Line(String);

impl Line {
    fn new(event: &str) -> Self {
        let mut line = Line(String::from("{"));
        line.str("event", event);
        line
    }
    fn key(&mut self, key: &str) {
        if self.0.len() > 1 {
            self.0.push(',');
        }
        let _ = write!(self.0, "\"{key}\":");
    }
    fn str(&mut self, key: &str, value: &str) -> &mut Self {
        self.key(key);
        self.0.push('"');
        for c in value.chars() {
            match c {
                '"' => self.0.push_str("\\\""),
                '\\' => self.0.push_str("\\\\"),
                c if (c as u32) < 0x20 => {
                    let _ = write!(self.0, "\\u{:04x}", c as u32);
                }
                c => self.0.push(c),
            }
        }
        self.0.push('"');
        self
    }
    fn num(&mut self, key: &str, value: impl std::fmt::Display) -> &mut Self {
        self.key(key);
        let _ = write!(self.0, "{value}");
        self
    }
    fn hex(&mut self, key: &str, value: u32) -> &mut Self {
        self.str(key, &format!("{value:#010x}"))
    }
    fn bool(&mut self, key: &str, value: bool) -> &mut Self {
        self.num(key, value)
    }
    fn raw(&mut self, key: &str, json: &str) -> &mut Self {
        self.key(key);
        self.0.push_str(json);
        self
    }
    fn emit(&mut self) {
        self.0.push('}');
        println!("{}", self.0);
    }
}

fn last_error() -> u32 {
    // SAFETY: no preconditions.
    unsafe { GetLastError() }
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(Some(0)).collect()
}

fn owned(raw: HANDLE) -> OwnedHandle {
    // SAFETY: callers pass a fresh, valid handle owned only here.
    unsafe { OwnedHandle::from_raw_handle(raw) }
}

fn h(handle: &OwnedHandle) -> HANDLE {
    handle.as_raw_handle()
}

// ---------------------------------------------------------------------------
// Opening.

/// `CreateFileW` that does not follow a final reparse point.
fn open_path(path: &Path, access: u32, share: u32) -> Result<OwnedHandle, u32> {
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
    if raw == INVALID_HANDLE_VALUE {
        Err(last_error())
    } else {
        Ok(owned(raw))
    }
}

/// `NtCreateFile` of `name` relative to `parent`: one component, never
/// following a reparse point (`FILE_OPEN_REPARSE_POINT`, and
/// `OBJ_DONT_REPARSE` when `dont_reparse`).
fn nt_open(
    parent: &OwnedHandle,
    name: &str,
    directory: bool,
    access: u32,
    share: u32,
    attributes: u32,
    open_reparse: bool,
) -> Result<OwnedHandle, NTSTATUS> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let bytes = u16::try_from(units.len() * 2).map_err(|_| 0xC000_0033_u32 as i32)?;
    let string = UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: units.as_ptr().cast_mut(),
    };
    let object = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: h(parent),
        ObjectName: &string,
        Attributes: attributes,
        SecurityDescriptor: null(),
        SecurityQualityOfService: null(),
    };
    let mut status_block = IO_STATUS_BLOCK::default();
    let mut raw: HANDLE = null_mut();
    let options = (if directory {
        FILE_DIRECTORY_FILE
    } else {
        FILE_NON_DIRECTORY_FILE
    }) | FILE_SYNCHRONOUS_IO_NONALERT
        | if open_reparse { FILE_OPEN_REPARSE_POINT } else { 0 };
    // SAFETY: every pointer is valid for the call; the name buffer and
    // string outlive it; the handle is owned on success.
    let status = unsafe {
        NtCreateFile(
            &mut raw,
            access,
            &object,
            &mut status_block,
            null(),
            0,
            share,
            FILE_OPEN,
            options,
            null(),
            0,
        )
    };
    if status >= 0 { Ok(owned(raw)) } else { Err(status) }
}

/// The proposed relative open.
fn open_child(parent: &OwnedHandle, name: &str, directory: bool) -> Result<OwnedHandle, NTSTATUS> {
    let (access, share) = if directory {
        (DIR_ACCESS, DIR_SHARE)
    } else {
        (FILE_ACCESS, FILE_SHARE)
    };
    nt_open(
        parent,
        name,
        directory,
        access,
        share,
        OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        true,
    )
}

// ---------------------------------------------------------------------------
// Facts about an opened object.

#[derive(Default)]
struct Facts {
    disk: bool,
    attributes: u32,
    reparse_tag: u32,
    end_of_file: i64,
    links: u32,
    delete_pending: bool,
    directory: bool,
    volume_serial: Option<u64>,
    file_id: Option<[u8; 16]>,
    id_error: u32,
    name: Option<String>,
    case_sensitive: Option<bool>,
}

fn info<T: Default>(handle: &OwnedHandle, class: i32) -> Result<T, u32> {
    let mut value = T::default();
    // SAFETY: `value` is a `T` sized buffer for the class that returns a `T`.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            h(handle),
            class,
            (&mut value as *mut T).cast(),
            size_of::<T>() as u32,
        )
    };
    if ok == 0 { Err(last_error()) } else { Ok(value) }
}

fn name_info(handle: &OwnedHandle) -> Result<String, u32> {
    let mut buffer = vec![0u64; 8 * 1024 / 8];
    // SAFETY: `buffer` holds 8 KiB, aligned for FILE_NAME_INFO.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            h(handle),
            FileNameInfo,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * 8) as u32,
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    let info = buffer.as_ptr().cast::<FILE_NAME_INFO>();
    // SAFETY: the call filled a FILE_NAME_INFO; its length is in bytes and
    // bounded by the buffer, which is checked.
    unsafe {
        let length = (*info).FileNameLength as usize / 2;
        if length * 2 + 4 > buffer.len() * 8 {
            return Err(0xFFFF_FFFF);
        }
        let start = std::ptr::addr_of!((*info).FileName).cast::<u16>();
        Ok(String::from_utf16_lossy(std::slice::from_raw_parts(start, length)))
    }
}

fn facts(handle: &OwnedHandle) -> Facts {
    let mut facts = Facts {
        // SAFETY: a live handle.
        disk: unsafe { GetFileType(h(handle)) } == FILE_TYPE_DISK,
        ..Facts::default()
    };
    if let Ok(tag) = info::<FILE_ATTRIBUTE_TAG_INFO>(handle, FileAttributeTagInfo) {
        facts.attributes = tag.FileAttributes;
        facts.reparse_tag = tag.ReparseTag;
    }
    if let Ok(standard) = info::<FILE_STANDARD_INFO>(handle, FileStandardInfo) {
        facts.end_of_file = standard.EndOfFile;
        facts.links = standard.NumberOfLinks;
        facts.delete_pending = standard.DeletePending;
        facts.directory = standard.Directory;
    }
    match info::<FILE_ID_INFO>(handle, FileIdInfo) {
        Ok(id) => {
            facts.volume_serial = Some(id.VolumeSerialNumber);
            facts.file_id = Some(id.FileId.Identifier);
        }
        Err(error) => facts.id_error = error,
    }
    facts.name = name_info(handle).ok();
    if facts.attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        facts.case_sensitive = info::<FILE_CASE_SENSITIVE_INFO>(handle, FileCaseSensitiveInfo)
            .ok()
            .map(|info| info.Flags & FILE_CS_FLAG_CASE_SENSITIVE_DIR != 0);
    }
    facts
}

fn facts_json(facts: &Facts) -> String {
    let mut line = Line(String::from("{"));
    line.bool("disk", facts.disk)
        .hex("attributes", facts.attributes)
        .hex("reparseTag", facts.reparse_tag)
        .num("endOfFile", facts.end_of_file)
        .num("links", facts.links)
        .bool("deletePending", facts.delete_pending)
        .bool("directory", facts.directory)
        .num("idError", facts.id_error);
    if let Some(serial) = facts.volume_serial {
        line.str("volumeSerial", &format!("{serial:016x}"));
    }
    if let Some(id) = facts.file_id {
        line.str("fileId", &id.iter().map(|b| format!("{b:02x}")).collect::<String>());
    }
    if let Some(name) = &facts.name {
        line.str("volumeRelativeName", name);
    }
    if let Some(case) = facts.case_sensitive {
        line.bool("caseSensitive", case);
    }
    line.0.push('}');
    line.0
}

struct Volume {
    file_system: String,
    flags: u32,
    device_type: u32,
    characteristics: u32,
    error: Option<String>,
}

fn volume(handle: &OwnedHandle) -> Volume {
    let mut name = [0u16; 64];
    let mut flags = 0u32;
    let mut serial = 0u32;
    let mut component = 0u32;
    let mut out = Volume {
        file_system: String::new(),
        flags: 0,
        device_type: 0,
        characteristics: 0,
        error: None,
    };
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
    let status = unsafe {
        NtQueryVolumeInformationFile(
            h(handle),
            &mut status_block,
            (&mut device as *mut FILE_FS_DEVICE_INFORMATION).cast(),
            size_of::<FILE_FS_DEVICE_INFORMATION>() as u32,
            FileFsDeviceInformation,
        )
    };
    if status < 0 {
        out.error = Some(format!("NtQueryVolumeInformationFile: {status:#x}"));
    } else {
        out.device_type = device.DeviceType;
        out.characteristics = device.Characteristics;
    }
    out
}

/// The self-relative descriptor with owner, DACL, and label, in a buffer
/// aligned for its header.
fn security(handle: &OwnedHandle) -> Result<Vec<u8>, u32> {
    let wanted = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION;
    let mut needed = 0u32;
    // SAFETY: a size query with no buffer.
    unsafe { GetKernelObjectSecurity(h(handle), wanted, null_mut(), 0, &mut needed) };
    if needed == 0 || needed > 1 << 20 {
        return Err(last_error());
    }
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
    // SAFETY: `buffer` holds at least `needed` bytes.
    let ok = unsafe {
        GetKernelObjectSecurity(
            h(handle),
            wanted,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * 8) as u32,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    // SAFETY: the buffer is initialized; reading it as bytes is valid.
    let bytes = unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), needed as usize) };
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
        GetTokenInformation(
            h(&token),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * 8) as u32,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(format!("GetTokenInformation: {}", last_error()));
    }
    // SAFETY: the buffer holds a TOKEN_USER whose SID points into it.
    unsafe {
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let length = GetLengthSid(user.User.Sid) as usize;
        let bytes = std::slice::from_raw_parts(user.User.Sid.cast::<u8>(), length);
        Sid::parse(bytes).map(|(sid, _)| sid).map_err(|m| m.0.to_owned())
    }
}

fn sid_string(sid: PSID) -> String {
    let mut text: windows_sys::core::PWSTR = null_mut();
    // SAFETY: a valid SID; the string is LocalAlloc'd and freed below.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return String::from("?");
    }
    // SAFETY: a NUL-terminated string from the call.
    let value = unsafe {
        let mut len = 0;
        while *text.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(text, len))
    };
    // SAFETY: allocated by ConvertSidToStringSidW.
    unsafe { LocalFree(text.cast()) };
    value
}

fn descriptor_json(bytes: &[u8]) -> String {
    match sd::parse(bytes) {
        Err(malformed) => format!("{{\"malformed\":\"{}\"}}", malformed.0),
        Ok(descriptor) => {
            let mut out = String::from("{");
            let _ = write!(out, "\"control\":\"{:#06x}\"", descriptor.control);
            if let Some(owner) = &descriptor.owner {
                let _ = write!(out, ",\"owner\":\"{owner}\"");
            }
            if let Some(rid) = descriptor.label_rid {
                let _ = write!(out, ",\"labelRid\":\"{rid:#x}\"");
            }
            match &descriptor.dacl {
                None => out.push_str(",\"dacl\":null"),
                Some(aces) => {
                    out.push_str(",\"dacl\":[");
                    for (index, ace) in aces.iter().enumerate() {
                        if index > 0 {
                            out.push(',');
                        }
                        let _ = write!(
                            out,
                            "{{\"type\":{},\"flags\":\"{:#04x}\",\"mask\":\"{:#010x}\",\"sid\":\"{}\"}}",
                            ace.ace_type,
                            ace.flags,
                            ace.mask,
                            ace.sid.as_ref().map(ToString::to_string).unwrap_or_default()
                        );
                    }
                    out.push(']');
                }
            }
            out.push('}');
            out
        }
    }
}

fn verdict(bytes: &[u8], kind: Kind, place: Place, principals: &Principals) -> String {
    match sd::evaluate(bytes, kind, place, principals) {
        Ok(passed) => format!(
            "{{\"ok\":true,\"toleratedForeign\":\"{:#x}\"}}",
            passed.tolerated_foreign
        ),
        Err(refusal) => {
            let mut line = Line(String::from("{"));
            line.bool("ok", false).str("refusal", &refusal.to_string());
            line.0.push('}');
            line.0
        }
    }
}

// ---------------------------------------------------------------------------
// dump

fn dump(paths: &[String], principals: &Principals) {
    for path in paths {
        let mut line = Line::new("dump");
        line.str("path", path);
        match open_path(Path::new(path), READ_CONTROL | FILE_READ_ATTRIBUTES | SYNCHRONIZE, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE) {
            Err(error) => {
                line.num("openError", error).emit();
                continue;
            }
            Ok(handle) => {
                let facts = facts(&handle);
                let volume = volume(&handle);
                line.raw("facts", &facts_json(&facts))
                    .str("fileSystem", &volume.file_system)
                    .hex("fsFlags", volume.flags)
                    .hex("deviceType", volume.device_type)
                    .hex("deviceCharacteristics", volume.characteristics);
                if let Some(error) = &volume.error {
                    line.str("volumeError", error);
                }
                match security(&handle) {
                    Err(error) => {
                        line.num("securityError", error);
                    }
                    Ok(bytes) => {
                        let kind = if facts.attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
                            Kind::Directory
                        } else {
                            Kind::File
                        };
                        line.raw("descriptor", &descriptor_json(&bytes))
                            .raw("asManaged", &verdict(&bytes, kind, Place::Managed, principals))
                            .raw("asAncestor", &verdict(&bytes, kind, Place::Ancestor, principals))
                            .raw("asVolumeRoot", &verdict(&bytes, kind, Place::VolumeRoot, principals));
                    }
                }
                line.emit();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// walk: the proposed algorithm, read-only.

/// Normalizes `path` with Win32 rules and splits `X:\a\b` into the drive
/// letter and components; refuses every other form.
fn split_drive_path(path: &str) -> Result<(char, Vec<String>), String> {
    let input = wide(OsStr::new(path));
    let mut buffer = vec![0u16; 32_768];
    // SAFETY: NUL-terminated input and an output buffer of the given size.
    let length = unsafe { GetFullPathNameW(input.as_ptr(), buffer.len() as u32, buffer.as_mut_ptr(), null_mut()) } as usize;
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
    let components: Vec<String> = chars
        .as_str()
        .split('\\')
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect();
    for part in &components {
        if part == "." || part == ".." || part.contains([':', '/', '\0']) {
            return Err(format!("{full} has an unsupported component {part:?}"));
        }
    }
    Ok((letter.to_ascii_uppercase(), components))
}

fn step(line: &mut Line, handle: &OwnedHandle, kind: Kind, place: Place, principals: &Principals, root_serial: Option<u64>) -> Result<Facts, String> {
    let facts = facts(handle);
    line.raw("facts", &facts_json(&facts));
    if !facts.disk {
        return Err("not a disk file".into());
    }
    if facts.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 || facts.reparse_tag != 0 {
        return Err(format!("is a reparse point (tag {:#x})", facts.reparse_tag));
    }
    let is_dir = facts.attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    if is_dir != (kind == Kind::Directory) {
        return Err("has the wrong type".into());
    }
    if facts.volume_serial.is_none() {
        return Err(format!("FileIdInfo failed: {}", facts.id_error));
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

/// Tri-state: whether `name` exists in `dir`, without following it.
fn has_entry(dir: &OwnedHandle, name: &str) -> Result<bool, NTSTATUS> {
    match nt_open(dir, name, false, FILE_READ_ATTRIBUTES | SYNCHRONIZE, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE, true) {
        Ok(_) => Ok(true),
        // A directory entry: FILE_NON_DIRECTORY_FILE fails with
        // STATUS_FILE_IS_A_DIRECTORY, which also means present.
        Err(status) if status == 0xC000_00BA_u32 as i32 => Ok(true),
        Err(status) if status == STATUS_OBJECT_NAME_NOT_FOUND => Ok(false),
        Err(status) => Err(status),
    }
}

fn walk(home: &str, file: Option<&str>, principals: &Principals) -> i32 {
    let (letter, components) = match split_drive_path(home) {
        Ok(split) => split,
        Err(reason) => {
            Line::new("walk").str("refused", &reason).emit();
            return 1;
        }
    };
    let root_path = format!(r"\\?\{letter}:\");
    let mut held: Vec<OwnedHandle> = Vec::new();
    let mut line = Line::new("walk-root");
    line.str("path", &root_path);
    let root = match open_path(Path::new(&root_path), DIR_ACCESS, DIR_SHARE) {
        Ok(root) => root,
        Err(error) => {
            line.num("openError", error).emit();
            return 1;
        }
    };
    let volume = volume(&root);
    line.str("fileSystem", &volume.file_system)
        .hex("fsFlags", volume.flags)
        .hex("deviceType", volume.device_type)
        .hex("deviceCharacteristics", volume.characteristics);
    let mut refusal = None;
    if volume.flags & FILE_PERSISTENT_ACLS == 0 {
        refusal = Some("the volume does not keep ACLs".to_owned());
    } else if volume.device_type != FILE_DEVICE_DISK || volume.characteristics & FILE_REMOTE_DEVICE != 0 {
        refusal = Some("the volume is not a local disk".to_owned());
    } else if !matches!(volume.file_system.as_str(), "NTFS" | "ReFS") {
        refusal = Some(format!("the file system {} is not qualified", volume.file_system));
    }
    let root_facts = match step(&mut line, &root, Kind::Directory, Place::VolumeRoot, principals, None) {
        Ok(facts) => Some(facts),
        Err(reason) => {
            refusal.get_or_insert(reason);
            None
        }
    };
    if let Some(facts) = &root_facts
        && facts.name.as_deref() != Some("\\")
    {
        refusal.get_or_insert("the drive letter does not name a volume root (subst?)".to_owned());
    }
    if let Some(reason) = &refusal {
        line.str("refused", reason).emit();
        return 1;
    }
    line.emit();
    let root_serial = root_facts.and_then(|facts| facts.volume_serial);
    held.push(root);
    let count = components.len();
    let mut shown = format!("{letter}:");
    for (index, name) in components.iter().enumerate() {
        shown.push('\\');
        shown.push_str(name);
        let place = if index + 1 == count { Place::Managed } else { Place::Ancestor };
        let mut line = Line::new("walk-dir");
        line.str("path", &shown).str("place", &format!("{place:?}"));
        let Some(parent) = held.last() else { return 1 };
        match open_child(parent, name, true) {
            Err(status) if status == STATUS_OBJECT_NAME_NOT_FOUND => {
                line.str("result", "absent").emit();
                return 0;
            }
            Err(status) => {
                line.hex("ntstatus", status as u32).str("refused", "open failed; not treated as absence").emit();
                return 1;
            }
            Ok(handle) => {
                if let Err(reason) = step(&mut line, &handle, Kind::Directory, place, principals, root_serial) {
                    line.str("refused", &reason).emit();
                    return 1;
                }
                match has_entry(&handle, ".git") {
                    Ok(git) => line.bool("hasGit", git),
                    Err(status) => line.hex("gitProbeStatus", status as u32),
                };
                line.emit();
                held.push(handle);
            }
        }
    }
    let Some(name) = file else { return 0 };
    let Some(home) = held.last() else { return 1 };
    let mut line = Line::new("walk-file");
    line.str("path", &format!("{shown}\\{name}"));
    let file = match open_child(home, name, false) {
        Err(status) if status == STATUS_OBJECT_NAME_NOT_FOUND => {
            line.str("result", "absent").emit();
            return 0;
        }
        Err(status) => {
            line.hex("ntstatus", status as u32).str("refused", "open failed; not treated as absence").emit();
            return 1;
        }
        Ok(file) => file,
    };
    let facts = match step(&mut line, &file, Kind::File, Place::Managed, principals, root_serial) {
        Ok(facts) => facts,
        Err(reason) => {
            line.str("refused", &reason).emit();
            return 1;
        }
    };
    if facts.end_of_file < 0 || facts.end_of_file as u64 > LIMIT {
        line.str("refused", "too large").emit();
        return 1;
    }
    let mut bytes = vec![0u8; (LIMIT + 1) as usize];
    let mut total = 0usize;
    loop {
        let mut read = 0u32;
        let want = (bytes.len() - total).min(1 << 16) as u32;
        if want == 0 {
            break;
        }
        // SAFETY: a live synchronous handle and the unread part of `bytes`.
        let ok = unsafe { ReadFile(h(&file), bytes[total..].as_mut_ptr(), want, &mut read, null_mut()) };
        if ok == 0 {
            line.num("readError", last_error()).str("refused", "read failed").emit();
            return 1;
        }
        if read == 0 {
            break;
        }
        total += read as usize;
    }
    line.num("bytesRead", total);
    if total as u64 > LIMIT || total as i64 != facts.end_of_file {
        line.str("refused", "the bytes read do not match the checked size").emit();
        return 1;
    }
    line.str("result", "trusted").emit();
    0
}

// ---------------------------------------------------------------------------
// mechanisms: only on objects created here.

struct Probe {
    dir: PathBuf,
    created: Vec<(PathBuf, bool)>,
    user: String,
}

impl Probe {
    fn sd(sddl: &str) -> Result<(Vec<u8>, usize), u32> {
        let text = wide(OsStr::new(sddl));
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        let mut size = 0u32;
        // SAFETY: NUL-terminated SDDL; the descriptor is LocalAlloc'd.
        if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(text.as_ptr(), SDDL_REVISION_1, &mut sd, &mut size) } == 0 {
            return Err(last_error());
        }
        // SAFETY: `size` bytes from the call; copied, then freed.
        let bytes = unsafe { std::slice::from_raw_parts(sd.cast::<u8>(), size as usize).to_vec() };
        // SAFETY: allocated by the call.
        unsafe { LocalFree(sd) };
        Ok((bytes, size as usize))
    }

    fn attributes(bytes: &mut [u8]) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: bytes.as_mut_ptr().cast(),
            bInheritHandle: 0,
        }
    }

    fn mkdir(&mut self, relative: &str, sddl: Option<&str>) -> Result<PathBuf, u32> {
        let path = self.dir.join(relative);
        let name = wide(path.as_os_str());
        let mut owned_sd = match sddl {
            Some(sddl) => Some(Self::sd(sddl)?.0),
            None => None,
        };
        let attributes = owned_sd.as_mut().map(|bytes| Self::attributes(bytes));
        // SAFETY: NUL-terminated path; attributes point at a live descriptor.
        let ok = unsafe { CreateDirectoryW(name.as_ptr(), attributes.as_ref().map_or(null(), |a| a as *const _)) };
        if ok == 0 {
            return Err(last_error());
        }
        self.created.push((path.clone(), true));
        Ok(path)
    }

    fn file(&mut self, relative: &str, sddl: Option<&str>, content: &[u8]) -> Result<PathBuf, u32> {
        let path = self.dir.join(relative);
        let name = wide(path.as_os_str());
        let mut owned_sd = match sddl {
            Some(sddl) => Some(Self::sd(sddl)?.0),
            None => None,
        };
        let attributes = owned_sd.as_mut().map(|bytes| Self::attributes(bytes));
        // SAFETY: as above; CREATE_NEW never opens an existing object.
        let raw = unsafe {
            CreateFileW(name.as_ptr(), GENERIC_WRITE, 0, attributes.as_ref().map_or(null(), |a| a as *const _), CREATE_NEW, 0, null_mut())
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(last_error());
        }
        self.created.push((path.clone(), false));
        let handle = owned(raw);
        let mut written = 0u32;
        // SAFETY: a live handle and the content buffer.
        if unsafe { WriteFile(h(&handle), content.as_ptr(), content.len() as u32, &mut written, null_mut()) } == 0 {
            return Err(last_error());
        }
        Ok(path)
    }

    fn private_sddl(&self) -> String {
        format!("D:P(A;OICI;FA;;;{})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)", self.user)
    }

    fn cleanup(&mut self) {
        while let Some((path, directory)) = self.created.pop() {
            let name = wide(path.as_os_str());
            // SAFETY: NUL-terminated paths this probe created. Removing a
            // junction with RemoveDirectoryW removes the link only.
            let ok = unsafe { if directory { RemoveDirectoryW(name.as_ptr()) } else { DeleteFileW(name.as_ptr()) } };
            if ok == 0 {
                Line::new("cleanup-left").str("path", &path.display().to_string()).num("error", last_error()).emit();
            }
        }
    }
}

fn result(line: &mut Line, key: &str, ok: bool) {
    if ok {
        line.str(key, "ok");
    } else {
        line.num(key, last_error());
    }
}

fn move_file(from: &Path, to: &Path, replace: bool) -> bool {
    let (from, to) = (wide(from.as_os_str()), wide(to.as_os_str()));
    // SAFETY: NUL-terminated paths.
    unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), if replace { MOVEFILE_REPLACE_EXISTING } else { 0 }) != 0 }
}

/// M1: a directory handle without delete sharing pins the directory only
/// when it holds a data access such as FILE_TRAVERSE (MS-FSA 2.1.5.1.2.1).
fn m1(probe: &mut Probe) -> Result<(), u32> {
    let base = probe.mkdir("m1", None)?;
    let d = probe.mkdir(r"m1\d", None)?;
    let moved = base.join("d2");
    for (label, access, share) in [
        ("traverseNoShareDelete", DIR_ACCESS, DIR_SHARE),
        ("noDataAccessNoShareDelete", READ_CONTROL | FILE_READ_ATTRIBUTES | SYNCHRONIZE, DIR_SHARE),
        ("traverseShareDelete", DIR_ACCESS, DIR_SHARE | FILE_SHARE_DELETE),
    ] {
        let mut line = Line::new("m1-pin-directory");
        line.str("case", label);
        let held = open_path(&d, access, share)?;
        let renamed = move_file(&d, &moved, false);
        result(&mut line, "renameWhileHeld", renamed);
        drop(held);
        if renamed {
            result(&mut line, "restore", move_file(&moved, &d, false));
        }
        line.emit();
    }
    Ok(())
}

/// M2: an open file pins every directory above it against rename
/// (FILE_RENAME_INFORMATION: "A directory cannot be renamed if it or any of
/// its subdirectories contains a file that has open handles").
fn m2(probe: &mut Probe) -> Result<(), u32> {
    probe.mkdir("m2", None)?;
    let a = probe.mkdir(r"m2\a", None)?;
    probe.mkdir(r"m2\a\b", None)?;
    let f = probe.file(r"m2\a\b\f", None, b"x")?;
    let a2 = probe.dir.join(r"m2\a2");
    let mut line = Line::new("m2-open-file-pins-ancestors");
    let held = open_path(&f, FILE_ACCESS, FILE_SHARE)?;
    result(&mut line, "renameAncestorWhileFileOpen", move_file(&a, &a2, false));
    drop(held);
    let renamed = move_file(&a, &a2, false);
    result(&mut line, "renameAncestorAfterClose", renamed);
    if renamed {
        result(&mut line, "restore", move_file(&a2, &a, false));
    }
    line.emit();
    Ok(())
}

fn set_junction(link: &Path, target: &Path) -> bool {
    let substitute: Vec<u16> = OsStr::new(&format!(r"\??\{}", target.display())).encode_wide().collect();
    let print: Vec<u16> = target.as_os_str().encode_wide().collect();
    let path_bytes = (substitute.len() + 1 + print.len() + 1) * 2;
    let data_length = 8 + path_bytes;
    let mut buffer: Vec<u8> = Vec::with_capacity(8 + data_length);
    buffer.extend_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
    buffer.extend_from_slice(&(data_length as u16).to_le_bytes());
    buffer.extend_from_slice(&0u16.to_le_bytes());
    buffer.extend_from_slice(&0u16.to_le_bytes()); // SubstituteNameOffset
    buffer.extend_from_slice(&((substitute.len() * 2) as u16).to_le_bytes());
    buffer.extend_from_slice(&(((substitute.len() + 1) * 2) as u16).to_le_bytes()); // PrintNameOffset
    buffer.extend_from_slice(&((print.len() * 2) as u16).to_le_bytes());
    for unit in substitute.iter().chain(&[0]).chain(print.iter()).chain(&[0]) {
        buffer.extend_from_slice(&unit.to_le_bytes());
    }
    let Ok(handle) = open_path(link, FILE_WRITE_DATA | FILE_WRITE_ATTRIBUTES | SYNCHRONIZE, 0) else {
        return false;
    };
    let mut returned = 0u32;
    // SAFETY: a live directory handle and a well-formed REPARSE_DATA_BUFFER.
    unsafe {
        DeviceIoControl(h(&handle), FSCTL_SET_REPARSE_POINT, buffer.as_ptr().cast(), buffer.len() as u32, null_mut(), 0, &mut returned, null_mut()) != 0
    }
}

/// M3: reparse points are opened, not followed, and refused.
fn m3(probe: &mut Probe, principals: &Principals) -> Result<(), u32> {
    let base = probe.mkdir("m3", None)?;
    let target = probe.mkdir(r"m3\target", None)?;
    probe.mkdir(r"m3\target\sub", None)?;
    let file = probe.file(r"m3\target\file", None, b"x")?;
    let junction = probe.mkdir(r"m3\j", None)?;
    let mut line = Line::new("m3-reparse");
    let set = set_junction(&junction, &target);
    result(&mut line, "setJunction", set);
    let base_handle = open_path(&base, DIR_ACCESS, DIR_SHARE)?;
    match nt_open(&base_handle, "j", true, DIR_ACCESS, DIR_SHARE, OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE, true) {
        Ok(handle) => {
            line.raw("openJunctionItself", &facts_json(&facts(&handle)));
        }
        Err(status) => {
            line.hex("openJunctionItself", status as u32);
        }
    }
    for (label, attributes) in [("throughWithDontReparse", OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE), ("throughWithoutDontReparse", OBJ_CASE_INSENSITIVE)] {
        match nt_open(&base_handle, r"j\sub", true, DIR_ACCESS, DIR_SHARE, attributes, false) {
            Ok(_) => line.str(label, "opened"),
            Err(status) => line.hex(label, status as u32),
        };
    }
    line.bool("dontReparseStatusMatches", matches!(nt_open(&base_handle, r"j\sub", true, DIR_ACCESS, DIR_SHARE, OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE, false), Err(status) if status == STATUS_REPARSE_POINT_ENCOUNTERED));
    let symlink = base.join("fl");
    let (link_name, target_name) = (wide(symlink.as_os_str()), wide(file.as_os_str()));
    // SAFETY: NUL-terminated paths.
    let linked = unsafe { CreateSymbolicLinkW(link_name.as_ptr(), target_name.as_ptr(), SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE) };
    result(&mut line, "createFileSymlink", linked);
    if linked {
        probe.created.push((symlink, false));
        match nt_open(&base_handle, "fl", false, FILE_ACCESS, FILE_SHARE, OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE, true) {
            Ok(handle) => line.raw("openSymlinkItself", &facts_json(&facts(&handle))),
            Err(status) => line.hex("openSymlinkItself", status as u32),
        };
    }
    let hard = base.join("hl");
    let (hard_name, file_name) = (wide(hard.as_os_str()), wide(file.as_os_str()));
    // SAFETY: NUL-terminated paths.
    let hard_linked = unsafe { CreateHardLinkW(hard_name.as_ptr(), file_name.as_ptr(), null()) } != 0;
    result(&mut line, "createHardLink", hard_linked);
    if hard_linked {
        probe.created.push((hard, false));
        if let Ok(handle) = nt_open(&base_handle, "hl", false, FILE_ACCESS, FILE_SHARE, OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE, true) {
            line.raw("hardLinkFacts", &facts_json(&facts(&handle)));
        }
    }
    drop(base_handle);
    line.emit();
    let junction_text = junction.display().to_string();
    walk(&junction_text, None, principals);
    Ok(())
}

struct SendHandle(HANDLE);
// SAFETY: a token handle is usable from any thread.
unsafe impl Send for SendHandle {}

fn restricted_impersonation_token(sid: &str) -> Result<OwnedHandle, String> {
    let mut process: HANDLE = null_mut();
    // SAFETY: the pseudo-handle and an output slot.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_IMPERSONATE, &mut process) } == 0 {
        return Err(format!("OpenProcessToken {}", last_error()));
    }
    let process = owned(process);
    let text = wide(OsStr::new(sid));
    let mut psid: PSID = null_mut();
    // SAFETY: NUL-terminated SID text; the SID is LocalAlloc'd.
    if unsafe { windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW(text.as_ptr(), &mut psid) } == 0 {
        return Err(format!("ConvertStringSidToSidW {}", last_error()));
    }
    let restrict = SID_AND_ATTRIBUTES { Sid: psid, Attributes: 0 };
    let mut restricted: HANDLE = null_mut();
    // SAFETY: one restricting SID that lives across the call.
    let ok = unsafe { CreateRestrictedToken(h(&process), 0, 0, null(), 0, null(), 1, &restrict, &mut restricted) };
    // SAFETY: allocated by ConvertStringSidToSidW.
    unsafe { LocalFree(psid) };
    if ok == 0 {
        return Err(format!("CreateRestrictedToken {}", last_error()));
    }
    let restricted = owned(restricted);
    let mut impersonation: HANDLE = null_mut();
    // SAFETY: a live token and an output slot.
    if unsafe { DuplicateTokenEx(h(&restricted), TOKEN_IMPERSONATE | TOKEN_QUERY, null(), SecurityImpersonation, TokenImpersonation, &mut impersonation) } == 0 {
        return Err(format!("DuplicateTokenEx {}", last_error()));
    }
    Ok(owned(impersonation))
}

fn as_other<T: Send>(token: &OwnedHandle, work: impl FnOnce() -> T + Send) -> Result<T, String> {
    let token = SendHandle(h(token));
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let token = token;
                // SAFETY: NULL means the current thread; a live impersonation token.
                if unsafe { SetThreadToken(null(), token.0) } == 0 {
                    return Err(format!("SetThreadToken {}", last_error()));
                }
                let value = work();
                // SAFETY: ends this thread's impersonation.
                unsafe { RevertToSelf() };
                Ok(value)
            })
            .join()
            .map_err(|_| "the impersonating thread panicked".to_owned())?
    })
}

fn nt_link(existing: &Path, link: &Path) -> String {
    let Ok(handle) = open_path(existing, FILE_READ_ATTRIBUTES | SYNCHRONIZE, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE) else {
        return format!("open {}", last_error());
    };
    let name: Vec<u16> = OsStr::new(&format!(r"\??\{}", link.display())).encode_wide().collect();
    let header = std::mem::offset_of!(FILE_LINK_INFORMATION, FileName);
    let mut buffer = vec![0u64; (header + name.len() * 2).div_ceil(8) + 1];
    let info = buffer.as_mut_ptr().cast::<FILE_LINK_INFORMATION>();
    // SAFETY: the buffer holds the header and the name.
    unsafe {
        (*info).Anonymous.ReplaceIfExists = false;
        (*info).RootDirectory = null_mut();
        (*info).FileNameLength = (name.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(name.as_ptr(), std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(), name.len());
    }
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: a live handle and the structure above.
    let status = unsafe { NtSetInformationFile(h(&handle), &mut status_block, info.cast(), (buffer.len() * 8) as u32, FileLinkInformation) };
    if status >= 0 { "ok".into() } else { format!("{:#010x}", status as u32) }
}

/// M4: another principal that may only add entries cannot remove, rename,
/// or replace an existing one; with FILE_DELETE_CHILD it can.
fn m4(probe: &mut Probe) -> Result<(), String> {
    let mut random = [0u32; 2];
    for (i, slot) in random.iter_mut().enumerate() {
        *slot = (std::process::id() ^ (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(7)).rotate_left(i as u32 * 13)) | 1;
    }
    let other = format!("S-1-4-{}-{}-77", random[0], random[1]);
    let user = probe.user.clone();
    let dir_sddl = format!("D:P(A;OICI;FA;;;{user})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;;0x1200a7;;;{other})(A;OIIO;FA;;;{other})");
    let victim_sddl = format!("D:P(A;;FA;;;{user})(A;;FA;;;SY)(A;;0x120089;;;{other})");
    let e = |error: u32| format!("setup {error}");
    let dir = probe.mkdir("m4", Some(&dir_sddl)).map_err(e)?;
    let victim = probe.file(r"m4\victim", Some(&victim_sddl), b"victim").map_err(e)?;
    let victim2 = probe.file(r"m4\victim2", Some(&victim_sddl), b"victim2").map_err(e)?;
    let token = restricted_impersonation_token(&other)?;
    let new = dir.join("new.txt");
    let new2 = dir.join("new2.txt");
    let planted = dir.join("planted");
    let planted_nt = dir.join("planted-nt");
    let (n, n2, v, v2, p, pn) = (new.clone(), new2.clone(), victim.clone(), victim2.clone(), planted.clone(), planted_nt.clone());
    let line = as_other(&token, move || {
        let mut line = Line::new("m4-add-not-replace");
        let create = |path: &Path| {
            let name = wide(path.as_os_str());
            // SAFETY: NUL-terminated path; CREATE_NEW.
            let raw = unsafe { CreateFileW(name.as_ptr(), SYNCHRONIZE | FILE_READ_ATTRIBUTES, 0, null(), CREATE_NEW, 0, null_mut()) };
            if raw == INVALID_HANDLE_VALUE {
                Err(last_error())
            } else {
                // SAFETY: a fresh handle.
                unsafe { CloseHandle(raw) };
                Ok(())
            }
        };
        let added = create(&n);
        line.str("addNewFile", &format!("{added:?}"));
        line.str("addSecondFile", &format!("{:?}", create(&n2)));
        result(&mut line, "replaceExistingByRename", move_file(&n, &v, true));
        let name = wide(v.as_os_str());
        // SAFETY: NUL-terminated path.
        result(&mut line, "deleteExisting", unsafe { DeleteFileW(name.as_ptr()) } != 0);
        result(&mut line, "renameExisting", move_file(&v, &v.with_extension("moved"), false));
        line.str("writeExisting", &match open_path(&v, FILE_WRITE_DATA | SYNCHRONIZE, FILE_SHARE_READ) { Ok(_) => "ok".into(), Err(e) => e.to_string() });
        let (pw, vw) = (wide(p.as_os_str()), wide(v.as_os_str()));
        // SAFETY: NUL-terminated paths.
        result(&mut line, "plantHardLinkWin32", unsafe { CreateHardLinkW(pw.as_ptr(), vw.as_ptr(), null()) } != 0);
        line.str("plantHardLinkNt", &nt_link(&v, &pn));
        let _ = v2;
        line
    })?;
    let mut line = line;
    for path in [&new, &new2, &planted, &planted_nt] {
        if std::fs::symlink_metadata(path).is_ok() {
            probe.created.push((path.clone(), false));
        }
    }
    line.emit();
    // Negative control: the same principal with FILE_DELETE_CHILD.
    let control_sddl = format!("D:P(A;OICI;FA;;;{user})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;;0x1200e7;;;{other})(A;OIIO;FA;;;{other})");
    let (bytes, _) = Probe::sd(&control_sddl).map_err(e)?;
    let handle = open_path(&dir, WRITE_DAC | READ_CONTROL | SYNCHRONIZE, FILE_SHARE_READ | FILE_SHARE_WRITE).map_err(e)?;
    let mut bytes = bytes;
    // SAFETY: a live handle opened for WRITE_DAC and a self-relative descriptor.
    if unsafe { SetKernelObjectSecurity(h(&handle), DACL_SECURITY_INFORMATION, bytes.as_mut_ptr().cast()) } == 0 {
        return Err(format!("SetKernelObjectSecurity {}", last_error()));
    }
    drop(handle);
    let (n2, v2) = (new2.clone(), victim2.clone());
    let mut control = as_other(&token, move || {
        let mut line = Line::new("m4-control-delete-child");
        result(&mut line, "replaceExistingByRename", move_file(&n2, &v2, true));
        line
    })?;
    if std::fs::symlink_metadata(&new2).is_err() {
        // new2 now carries victim2's name; it was recorded as created.
        probe.created.retain(|(path, _)| path != &new2);
    }
    control.emit();
    Ok(())
}

/// M5: how a reader's handle interacts with an atomic replacement.
fn m5(probe: &mut Probe) -> Result<(), u32> {
    probe.mkdir("m5", None)?;
    let cfg = probe.file(r"m5\cfg", None, b"old")?;
    for (label, share) in [("readerSharesDelete", FILE_SHARE), ("readerDeniesDelete", FILE_SHARE_READ)] {
        let mut line = Line::new("m5-replace-under-reader");
        line.str("case", label);
        let reader = open_path(&cfg, FILE_ACCESS, share)?;
        let tmp1 = probe.file(&format!(r"m5\{label}-1"), None, b"new1")?;
        let classic = move_file(&tmp1, &cfg, true);
        result(&mut line, "moveFileExReplace", classic);
        if classic {
            probe.created.retain(|(path, _)| path != &tmp1);
        }
        let tmp2 = probe.file(&format!(r"m5\{label}-2"), None, b"new2")?;
        let posix = posix_rename(&tmp2, &cfg);
        line.str("posixRenameReplace", &posix);
        if posix == "ok" {
            probe.created.retain(|(path, _)| path != &tmp2);
        }
        let mut bytes = [0u8; 16];
        let mut read = 0u32;
        // SAFETY: a live handle and a 16-byte buffer.
        let ok = unsafe { ReadFile(h(&reader), bytes.as_mut_ptr(), 16, &mut read, null_mut()) };
        line.str("readerSees", &if ok == 0 { format!("error {}", last_error()) } else { String::from_utf8_lossy(&bytes[..read as usize]).into_owned() });
        drop(reader);
        line.emit();
    }
    Ok(())
}

fn posix_rename(from: &Path, to: &Path) -> String {
    let Ok(handle) = open_path(from, DELETE | SYNCHRONIZE, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE) else {
        return format!("open {}", last_error());
    };
    let name: Vec<u16> = to.as_os_str().encode_wide().collect();
    let header = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    let size = header + name.len() * 2 + 2;
    let mut buffer = vec![0u64; size.div_ceil(8)];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // SAFETY: the buffer holds the header and the NUL-terminated name.
    unsafe {
        (*info).Anonymous.Flags = 0x1 | 0x2; // REPLACE_IF_EXISTS | POSIX_SEMANTICS
        (*info).RootDirectory = null_mut();
        (*info).FileNameLength = (name.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(name.as_ptr(), std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(), name.len());
    }
    // SAFETY: a handle opened with DELETE and the structure above.
    let ok = unsafe { SetFileInformationByHandle(h(&handle), FileRenameInfoEx, info.cast::<c_void>(), size as u32) };
    if ok == 0 { format!("{}", last_error()) } else { "ok".into() }
}

/// M8: whether relative opens need OBJ_CASE_INSENSITIVE.
fn m8(probe: &mut Probe) -> Result<(), u32> {
    let base = probe.mkdir("m8", None)?;
    probe.file(r"m8\lower.txt", None, b"x")?;
    let handle = open_path(&base, DIR_ACCESS, DIR_SHARE)?;
    let mut line = Line::new("m8-case");
    for (label, attributes) in [("withCaseInsensitive", OBJ_CASE_INSENSITIVE), ("withoutCaseInsensitive", 0)] {
        match nt_open(&handle, "LOWER.TXT", false, FILE_ACCESS, FILE_SHARE, attributes, true) {
            Ok(_) => line.str(label, "opened"),
            Err(status) => line.hex(label, status as u32),
        };
    }
    line.emit();
    Ok(())
}

fn mechanisms(base: &str, principals: &Principals, user: &Sid) -> i32 {
    let mut suffix = [0u8; 8];
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) ^ u128::from(std::process::id());
    for (i, byte) in suffix.iter_mut().enumerate() {
        *byte = (seed >> (i * 8)) as u8;
    }
    let name: String = suffix.iter().map(|b| format!("{b:02x}")).collect();
    let mut probe = Probe {
        dir: PathBuf::from(base).join(format!("bake-trust-probe-{name}")),
        created: Vec::new(),
        user: user.to_string(),
    };
    let sddl = probe.private_sddl();
    let dir = probe.dir.clone();
    let Ok((mut bytes, _)) = Probe::sd(&sddl) else {
        Line::new("mechanisms").str("error", "SDDL").emit();
        return 1;
    };
    let attributes = Probe::attributes(&mut bytes);
    let name = wide(dir.as_os_str());
    // SAFETY: NUL-terminated path and a live descriptor; fails if it exists.
    if unsafe { CreateDirectoryW(name.as_ptr(), &attributes) } == 0 {
        Line::new("mechanisms").str("error", &format!("create {}: {}", dir.display(), last_error())).emit();
        return 1;
    }
    probe.created.push((dir.clone(), true));
    probe.dir = dir;
    Line::new("mechanisms").str("dir", &probe.dir.display().to_string()).emit();
    let report = |name: &str, outcome: Result<(), String>| {
        if let Err(error) = outcome {
            Line::new("mechanism-error").str("mechanism", name).str("error", &error).emit();
        }
    };
    report("m1", m1(&mut probe).map_err(|e| e.to_string()));
    report("m2", m2(&mut probe).map_err(|e| e.to_string()));
    report("m3", m3(&mut probe, principals).map_err(|e| e.to_string()));
    report("m4", m4(&mut probe));
    report("m5", m5(&mut probe).map_err(|e| e.to_string()));
    report("m8", m8(&mut probe).map_err(|e| e.to_string()));
    probe.cleanup();
    0
}

// ---------------------------------------------------------------------------

pub fn main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let user = match token_user() {
        Ok(user) => user,
        Err(error) => {
            eprintln!("cannot read the token user: {error}");
            return 2;
        }
    };
    Line::new("token").str("user", &user.to_string()).emit();
    let principals = Principals::new(user.clone());
    match args.first().map(String::as_str) {
        Some("dump") => {
            dump(&args[1..], &principals);
            0
        }
        Some("walk") if args.len() == 2 || args.len() == 3 => walk(&args[1], args.get(2).map(String::as_str), &principals),
        Some("mechanisms") if args.len() == 2 => mechanisms(&args[1], &principals, &user),
        _ => {
            eprintln!("usage: probe dump <path>... | walk <home> [<file>] | mechanisms <base>");
            2
        }
    }
}

#[allow(dead_code)]
fn _sid_string_is_used(sid: PSID) -> String {
    sid_string(sid)
}
