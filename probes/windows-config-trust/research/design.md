# Windows configuration trust for `mcp.json` and installed Wasm plugins: research and design

Research snapshot prepared by Pi (Opus 5.5, medium) on 2026-10-17. This document describes the earlier prototype in `prototype/`, not the unfinished native qualification refactor in `../src/`. Neither has native Windows qualification. The [probe README](../README.md) owns continuation status.

**Status.** This is a design. The rules and API sequence below come from primary Microsoft documentation and specifications. The pure security-descriptor evaluator was tested on Linux. The Windows FFI probe only type-checks: it passes `cargo check` and `clippy -D warnings` for `x86_64-pc-windows-gnu` with toolchain 1.99.0, and **it has not run on Windows**. Linux cross-checking is type evidence only. **No native Windows evidence exists yet**: every "expected" outcome in §10 and §11 still needs to be observed on a native Windows runner, and Windows is not finished until that happens.

## 1. Sources and pins

| What | Pin | Use |
|---|---|---|
| Pi latest official release | v1.3.0, `71023aec1546d6d677a65669c5665c0927ce0dd0` (re-checked with `git ls-remote`; no later tag) | Pi reads `~/.pi/agent/mcp.json` and, for a trusted project, `.pi/mcp.json` (`src/extensions/mcp/config.ts`, `core/project-trust.ts`). Pi writes `auth.json` with mode `0o600` and handles `EPERM` on win32 (`core/auth-storage.ts`). **Pi has no owner or ACL verification on any platform, so nothing is ported.** The rule is Bake's own, as the Unix rule is. |
| Rust std (Rust 1.99.0, the pinned toolchain) | tag `1.99.0` → `b940084d7eb6a299eb4bfeb8e34901bc051e7ac4` (= `rustc -Vv` commit-hash) | `library/std/src/sys/fs/windows/remove_dir_all.rs` `open_link_no_reparse` is the precedent: `NtOpenFile` relative to a parent handle, with `FILE_OPEN_REPARSE_POINT` and `OBJ_DONT_REPARSE`, and a retry without it on `ERROR_INVALID_PARAMETER`. The pinned upstream files are research references; their source copies are not included here. `std::fs::Dir` is unstable (`dirfd`, #120426), so it cannot be used. If an implementation adapts std code, keep Rust's MIT/Apache-2.0 notice in the crate `NOTICE`. The design below re-implements the pattern rather than copying it. |
| `std::fs::rename` on Windows (1.99.0) | same | `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` first, then `FileRenameInfoEx` with `REPLACE_IF_EXISTS \| POSIX_SEMANTICS` only after `ERROR_ACCESS_DENIED`. |
| `windows-sys` | `=0.61.2` (Bake's pin) | Has `NtCreateFile`, `NtQueryVolumeInformationFile`, `NtSetInformationFile`, `OBJ_DONT_REPARSE`, `STATUS_REPARSE_POINT_ENCOUNTERED`, `FileIdInfo`, `FileCaseSensitiveInfo`, and `FILE_FS_DEVICE_INFORMATION`. Features needed: `Wdk_Foundation`, `Wdk_Storage_FileSystem`, `Wdk_System_SystemServices`, `Win32_Security`, `Win32_Storage_FileSystem`, `Win32_System_IO` (plus `Win32_System_Threading` for the token, and test-only `Win32_Security_Authorization` and `Win32_System_Ioctl`). |
| Reviewed consumers (read-only) | `bake-rust-mcp-session-adapter` `mcp/home.rs`; `bake-rust-wasm-session-veto` `plugins/installed/trust.rs` at `6a15f74cd4` | Unix contract being matched. |

Primary Microsoft sources:

- `OBJECT_ATTRIBUTES` (ntdef.h): https://learn.microsoft.com/en-us/windows/win32/api/ntdef/ns-ntdef-_object_attributes. `RootDirectory` makes the name relative. `OBJ_DONT_REPARSE`: "no reparse points will be followed … fail and return STATUS_REPARSE_POINT_ENCOUNTERED … in security scenarios". `OBJ_CASE_INSENSITIVE`: without it, "names are compared using the default system settings".
- `NtCreateFile`: https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/nf-ntifs-ntcreatefile. Covers `FILE_OPEN_REPARSE_POINT` ("bypass normal reparse point processing"), `FILE_DIRECTORY_FILE` and `FILE_NON_DIRECTORY_FILE`, `FILE_SYNCHRONOUS_IO_NONALERT` (requires `SYNCHRONIZE`), and the share-access semantics.
- `CreateFileW`: https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew. Covers `FILE_FLAG_BACKUP_SEMANTICS` (needed to open a directory; "appropriate security checks still apply" without the privileges), `FILE_FLAG_OPEN_REPARSE_POINT`, and `FILE_SHARE_DELETE` ("Delete access allows both delete and rename").
- MS-FSA 2.1.5.1.2.1 "Algorithm to Check Access to an Existing File": https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fsa/82b364ce-6d7b-422f-8d88-4db32eea809a. `DELETE` is granted by `DELETE` on the file **or** `FILE_DELETE_CHILD` on the parent. A new `DELETE` open fails with `STATUS_SHARING_VIOLATION` only against an existing open that lacks `FILE_SHARE_DELETE` **and** holds one of `FILE_EXECUTE | FILE_READ_DATA | FILE_WRITE_DATA | FILE_APPEND_DATA | DELETE`.
- MS-FSA FileRenameInformation: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fsa/87f86c9b-6c2a-4803-84b7-131a74a434fa. A rename needs `DELETE` on the source and `FILE_ADD_FILE` or `FILE_ADD_SUBDIRECTORY` on the destination directory. Replacing an existing target needs `DELETE` on it **or** `FILE_DELETE_CHILD` on the destination directory. A directory containing open files cannot be renamed (2.1.4.2; Appendix A <186> describes how NTFS implements this).
- `FILE_RENAME_INFORMATION`: https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_rename_information. Its rules: "A volume's root directory cannot be renamed"; "A directory cannot be renamed if it or any of its subdirectories contains a file that has open handles"; "A file cannot be renamed if a file with the same name exists and has open handles"; and `FILE_RENAME_POSIX_SEMANTICS` "allow replacing a file even if there are existing handles to it. Existing handles … continue to be valid".
- MS-FSA FileLinkInformation: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fsa/891bb8eb-89f8-46ca-80b7-9f5d4e8b5583. Creating a hard link needs `FILE_ADD_FILE` on the destination directory, and the spec places **no access requirement on the source open**.
- MS-FSA FileDispositionInformation: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fsa/386d9ec5-e0f6-4853-b175-c05be01419e0 (needs `DELETE`; a non-empty directory fails). Delete on close: https://learn.microsoft.com/en-us/windows-hardware/drivers/ifs/delete-on-close ("delete permission on the file or delete child permission on the parent").
- MS-FSA FSCTL_SET_REPARSE_POINT: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fsa/4aeefef8-92c3-4abc-af7a-a610caf8a165. Needs `FILE_WRITE_DATA` **or** `FILE_WRITE_ATTRIBUTES`. A directory must be empty. A symlink tag needs the create-symlink right.
- MS-DTYP 2.5.3.2 Access Check Algorithm: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-dtyp/4f1bbcbb-814a-4c70-a11e-2a5b8779a6f9. The owner always gets `READ_CONTROL | WRITE_DAC` unless the DACL has owner (OWNER RIGHTS) ACEs. `INHERIT_ONLY_ACE` entries are skipped. A callback ACE applies when its condition evaluates to TRUE. Object ACEs apply only through an object-type list. A NULL or absent DACL grants everything, and an empty DACL grants nothing beyond the owner's implicit rights. Central access policy only intersects.
- OWNER RIGHTS (S-1-3-4): MS-DTYP Well-Known SIDs, https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-dtyp/81d92bba-d22b-4a8c-908a-554ab29148ab. Owner of a new object: https://learn.microsoft.com/en-us/windows/win32/secauthz/owner-of-a-new-object.
- Null and empty DACLs: https://learn.microsoft.com/en-us/windows/win32/secauthz/null-dacls-and-empty-dacls.
- File Security and Access Rights: https://learn.microsoft.com/en-us/windows/win32/fileio/file-security-and-access-rights. Gives the file generic mapping and two facts the design relies on: "a default security descriptor is assigned only when a file or directory is newly created, and not when it is renamed or moved", and "the security descriptor of a parent directory is not used to control access to any child". File Access Rights Constants: https://learn.microsoft.com/en-us/windows/win32/fileio/file-access-rights-constants.
- `ACE_HEADER` (types and flags): https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-ace_header. How AccessCheck works: https://learn.microsoft.com/en-us/windows/win32/secauthz/how-dacls-control-access-to-an-object.
- `GetSecurityInfo`: https://learn.microsoft.com/en-us/windows/win32/api/aclapi/nf-aclapi-getsecurityinfo. `GetKernelObjectSecurity`: https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-getkernelobjectsecurity (returns a self-relative copy; needs `READ_CONTROL`). `SECURITY_INFORMATION`: https://learn.microsoft.com/en-us/windows/win32/secauthz/security-information (`LABEL_SECURITY_INFORMATION` needs only `READ_CONTROL`). `SYSTEM_MANDATORY_LABEL_ACE`: https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-system_mandatory_label_ace.
- `FILE_ID_INFO`: https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_id_info ("the file identifier and the volume serial number uniquely identify a file on a single computer"). `GetVolumeInformationByHandleW` (`FILE_PERSISTENT_ACLS`): https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getvolumeinformationbyhandlew. `FILE_FS_DEVICE_INFORMATION`: https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdm/ns-wdm-_file_fs_device_information and MS-FSCC 2.5.10, https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fscc/616b66d5-b335-4e1c-8f87-b4a55e8d3e4a.
- `CreateRestrictedToken`: https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-createrestrictedtoken ("two access checks … Access is granted only if both access checks allow"). `RemoveDirectoryW` removes a junction, never its target: https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-removedirectoryw.

## 2. Contract and threat model

The contract is the Unix one, restated for Windows. A configuration file is read only when its bytes come from an object that no principal other than a trusted one can change, and that object is reached through directories no such principal can redirect. "Trusted" means the token user, `S-1-5-18` (LocalSystem), `S-1-5-32-544` (Administrators), and `S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464` (TrustedInstaller); these are the analogue of "the user or root". Bake's own sandbox principals are foreign: AppContainer and capability SIDs (`S-1-15-*`), the restricted-token runner's `S-1-4-*` capabilities, logon-session SIDs, Everyone, Authenticated Users, and Users. Processes running as the same user or as an administrator can change owner files, as on Unix, and that is out of scope. Lower-integrity processes of the same user are in scope through the label rule (§6).

Placement is unchanged:

- No opened directory below the user's profile is the session workspace, and none holds a `.git` entry.
- The workspace is not inside the managed root (the plugin rule).
- These are compared by `FILE_ID_INFO` identity on opened handles.

## 3. Supported locations (explicit)

The Bake home is still resolved by `home.rs`: `BAKE_HOME`, then `DSH_HOME`, then `~\.bake`, where a blank value counts as unset, and the result is absolute. Trust is then decided on the resolved path.

| Location | Phase 1 result |
|---|---|
| `%USERPROFILE%\.bake` on the system NTFS volume, with stock profile ACLs | **Expected supported; native evidence pending.** The profile, `C:\Users`, and `C:\` must pass the ancestor rules below. |
| `BAKE_HOME` or `DSH_HOME` on a local NTFS or ReFS volume with a drive letter, the home's own DACL free of foreign change rights | Supported. Example: `D:\bake` after `icacls D:\bake /inheritance:r /grant:r "%USERNAME%:(OI)(CI)F" "SYSTEM:(OI)(CI)F" "Administrators:(OI)(CI)F"`. |
| A home created directly under a drive root without tightening | Refused: it inherits Authenticated Users Modify. The message names the ACE and gives the `icacls` fix. |
| Secondary volume whose **root** grants others Modify, including `WRITE_ATTRIBUTES` | Refused (conservative). The stock DACL of non-system volumes is unverified; the probe dumps `D:\` on the runner. |
| Any reparse point from the drive root to the file (symlink, junction, volume mount point, OneDrive or cloud placeholder, dedup, WCI, AppExecLink) | Refused, naming the component. To use a linked location, set `BAKE_HOME` to the real target. |
| UNC (`\\server\share`, `\\?\UNC\…`, `\\wsl$`), mapped network drive, or `FILE_REMOTE_DEVICE` | Refused. UNC is rejected from the spelling, with no network I/O. Mapped drives are rejected by the root handle's device characteristics. |
| `subst` drive (the letter's root is not a volume root) | Refused: the root handle's volume-relative name (`FileNameInfo`) is not `\`. |
| `\\?\Volume{GUID}\…`, `\\.\…`, `\\?\GLOBALROOT\…`, reserved device names | Refused before any open. |
| FAT, exFAT, or any volume without `FILE_PERSISTENT_ACLS`, or a file system other than NTFS or ReFS | Refused (no ACLs, or unqualified). |
| Corporate profiles whose DACL grants other principals write (for example Domain Admins or Backup Operators) | Refused (conservative). A later allowlist could relax this, but only after review. |
| Missing file, or a missing directory on the walk | Absent: an empty configuration, or no plugins. This applies **only** to `STATUS_OBJECT_NAME_NOT_FOUND` from a checked relative open. |

The path is normalized once with `GetFullPathNameW`, which applies Win32 rules: `/` becomes `\`, `.` and `..` are resolved, and trailing dots and spaces are stripped. The reader then walks those components, and writers must use the same normalized path, so readers and writers never disagree about which object is meant. A component containing `:` (alternate data stream syntax) or NUL, or one that is `.` or `..`, is refused. 8.3 short names resolve through the relative open, and the opened object is what gets checked.

**There is no canonicalize-then-reopen step.** The walk opens the spelled, normalized components one at a time, each relative to the already-checked parent handle and never following a reparse point. `GetFinalPathNameByHandleW` and `FileNameInfo` are used only for messages and the root check, never to open anything again.

## 4. Proposed API

A shared **module** inside `bake-coding-agent`, not a new crate. Both consumers (`mcp/home.rs` and `plugins/installed`) and `json_store` live in that crate. The Unix code is currently duplicated: two `openat` walkers and two macOS ACL readers. No other crate needs the helper; if `bake-mcp` or `bake-sandbox` ever does, promote it then. Suggested layout: `src/owner_files/{mod.rs, rules.rs, unix.rs, macos_acl.rs, windows/{mod.rs, nt.rs, security.rs}}`. `rules.rs` holds the pure evaluators: the Windows descriptor rules and the macOS ACL step rules. They are compiled and tested on every platform.

```rust
/// A name of one entry: no separator, ':', NUL, "." or "..". Constants
/// are checked at compile time; store hashes at runtime.
pub(crate) struct EntryName(&'static str /* or Cow */);
impl EntryName { pub(crate) const fn new(name: &'static str) -> Self; pub(crate) fn parse(name: &str) -> Result<Self, Refusal>; }

/// Where the configuration may not live (identity, not spelling).
pub(crate) struct Placement<'a> { pub workspace: Option<&'a Path>, pub user_home: Option<&'a Path> }

/// A directory opened and checked with every directory above it. Holds the
/// whole chain's handles (`Arc<Chain>`), so while any `TrustedDir` from a
/// walk lives, no directory of it can be renamed or deleted (§7). `!Sync`
/// is not needed: handles are only read. Blocking; call from
/// `spawn_blocking`.
pub(crate) struct TrustedDir { chain: Arc<Chain>, depth: usize }

impl TrustedDir {
    /// Walks from the volume root (Windows) or `/` (Unix) to `home`,
    /// checking each object; `Ok(None)` only when an entry is observed
    /// missing. The home and below are `Managed`; above, `Ancestor`.
    pub(crate) fn open_home(home: &Path, placement: &Placement<'_>) -> Result<Option<TrustedDir>, Refusal>;
    pub(crate) fn dir(&self, name: &EntryName) -> Result<Option<TrustedDir>, Refusal>;
    /// Reads at most `limit` bytes of the regular file `name` through the
    /// checked handle; size, identity and read length must agree.
    pub(crate) fn read(&self, name: &EntryName, limit: u64) -> Result<Option<TrustedBytes>, Refusal>;
    /// For messages only.
    pub(crate) fn display_path(&self) -> &Path;
    pub(crate) fn id(&self) -> ObjectId; // (dev, ino) or (VolumeSerialNumber, FileId128)
}

pub(crate) struct TrustedBytes { pub bytes: Vec<u8>, pub id: ObjectId, pub links: u32 }

/// Why something was refused: one variant per cause, each rendering the
/// path, the reason, and what to change. Never "absent".
#[non_exhaustive]
pub(crate) enum Refusal {
    Unsupported { path: PathBuf, why: Unsupported },              // UNC, subst, FAT, platform, ...
    Link { path: PathBuf, tag: u32 },                             // reparse point / symlink
    NotRegular { path: PathBuf }, TooLarge { path: PathBuf, limit: u64 },
    ForeignOwner { path: PathBuf, owner: String },
    ForeignWriter { path: PathBuf, principal: String, rights: u32, ace: usize },
    NullDacl { path: PathBuf }, UnreadableSecurity { path: PathBuf, detail: String },
    LowIntegrity { path: PathBuf, rid: u32 },
    Placement { path: PathBuf, why: PlacementWhy },               // workspace / git work tree
    InUse { path: PathBuf },                                      // sharing violation / delete pending
    Changed { path: PathBuf },                                    // size or identity disagreed
    Io { path: PathBuf, op: &'static str, code: OsCode },         // every other OS/NT status
}
```

Misuse-resistance: the only way to read is through `TrustedDir::read`. `EntryName` cannot carry a path. There is no API that takes a path below the home. Handles are private, so callers cannot turn a check result into a path and open it again.

## 5. Exact Windows read sequence

1. **Spelling.** Normalize with `GetFullPathNameW`. Accept only `X:\…` or `\\?\X:\…`; anything else is `Unsupported`. Validate each component.
2. **Token.** `OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY)` and `GetTokenInformation(TokenUser)` give the user SID. Use the **process** token, not a thread's impersonation token.
3. **Root.** `CreateFileW("\\?\X:\", READ_CONTROL|FILE_READ_ATTRIBUTES|FILE_TRAVERSE|SYNCHRONIZE, FILE_SHARE_READ|FILE_SHARE_WRITE, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS|FILE_FLAG_OPEN_REPARSE_POINT|SECURITY_SQOS_PRESENT|SECURITY_IDENTIFICATION)`. Then require all of:
   - `GetFileType == FILE_TYPE_DISK`.
   - `GetFileInformationByHandleEx(FileAttributeTagInfo)`: a directory with no reparse tag.
   - `FileNameInfo == "\"` (a volume root, which rules out `subst`).
   - `NtQueryVolumeInformationFile(FileFsDeviceInformation)`: `DeviceType == FILE_DEVICE_DISK` and `FILE_REMOTE_DEVICE` clear.
   - `GetVolumeInformationByHandleW`: `FILE_PERSISTENT_ACLS` set, and the file-system name is `NTFS` or `ReFS`.
   - `FileIdInfo` succeeds; record the `VolumeSerialNumber`.
   - The security rule at `VolumeRoot`.
4. **Each directory component**, relative to the previous handle: `NtCreateFile(&h, READ_CONTROL|FILE_READ_ATTRIBUTES|FILE_TRAVERSE|SYNCHRONIZE, OBJECT_ATTRIBUTES{RootDirectory: parent, ObjectName: <one component>, Attributes: OBJ_CASE_INSENSITIVE|OBJ_DONT_REPARSE}, &iosb, NULL, 0, FILE_SHARE_READ|FILE_SHARE_WRITE, FILE_OPEN, FILE_DIRECTORY_FILE|FILE_OPEN_REPARSE_POINT|FILE_SYNCHRONOUS_IO_NONALERT, NULL, 0)`. Then check, in order, on the handle:
   - `GetFileType` and `FileAttributeTagInfo`: a directory, with `FILE_ATTRIBUTE_REPARSE_POINT` clear and tag 0.
   - `FileStandardInfo.DeletePending` false.
   - `FileIdInfo` on the same volume serial as the root.
   - `GetKernelObjectSecurity(h, OWNER|DACL|LABEL)`, evaluated by the pure rules (§6) at `Ancestor`, or at `Managed` for the home and below.
   - Placement: identity against the workspace and the profile ancestors, and a tri-state `.git` probe relative to the handle (`FILE_READ_ATTRIBUTES|SYNCHRONIZE`, no reparse). There, `STATUS_OBJECT_NAME_NOT_FOUND` means no `.git`, `STATUS_FILE_IS_A_DIRECTORY` or success means present, and **anything else refuses**.
5. **The file**, relative to the home or a managed subdirectory: `NtCreateFile(READ_CONTROL|FILE_READ_ATTRIBUTES|FILE_READ_DATA|SYNCHRONIZE, …, FILE_SHARE_READ|FILE_SHARE_DELETE, FILE_OPEN, FILE_NON_DIRECTORY_FILE|FILE_OPEN_REPARSE_POINT|FILE_SYNCHRONOUS_IO_NONALERT)`. Then:
   - Same type, reparse, and volume checks.
   - `FileStandardInfo`: `EndOfFile` is non-negative and at most `limit`, and `DeletePending` is false.
   - Security rule at `Managed`.
   - `ReadFile` loop up to `limit + 1` bytes. The byte count must equal `EndOfFile`, or the result is `Changed`.
   - Record `NumberOfLinks`.
6. Return the bytes. The file handle closes when the read returns; the directory chain stays open while its `TrustedDir`s live.

`OBJ_DONT_REPARSE` is redundant for single-component names, since `FILE_OPEN_REPARSE_POINT` plus the attribute check already refuses a link. It is defense in depth. If it returns `STATUS_INVALID_PARAMETER` on an old build, retry without it, as std does. That is safe only because names are single components.

## 6. Security rule (pure; probe `src/sd.rs`)

The descriptor is parsed as **bytes** with bounds checks: revision 1, `SE_SELF_RELATIVE`, offsets inside the buffer, ACL revision 2 or 4, `AceSize` at least 8, a multiple of 4, and inside `AclSize`, and SIDs with at most 15 sub-authorities. FFI only fetches the bytes, so the same evaluator runs and is tested on every OS. In the probe, a 20,000-mutation pass ended with a refusal and never a panic.

- **Owner** must be trusted. A foreign owner is a foreign writer, because of its implicit `WRITE_DAC` (MS-DTYP 2.5.3.2). A missing owner refuses.
- **DACL**: `SE_DACL_PRESENT` clear, or present with no ACL, is a NULL DACL and refuses. An empty DACL passes: only the owner has implicit access.
- **Label** (LABEL_SECURITY_INFORMATION, fetched with `READ_CONTROL` only): a mandatory label below Medium (`S-1-16-x`, x < 0x2000) refuses, because lower-integrity same-user processes could write. The rule does not depend on the label's policy bits.
- **ACEs**: an `INHERIT_ONLY_ACE` is skipped (it does not control this object). Each other ACE is handled by type:
  - Allow, allowed-object, allowed-callback, and allowed-callback-object ACEs **count as grants**: conditions are assumed true and object types are assumed to match.
  - Deny ACEs of every kind are ignored, along with their order. They can only narrow access, so ignoring them can only refuse more.
  - Any other type in a DACL refuses: audit, alarm, compound, label, resource-attribute, scoped-policy, and unknown types.
  - The mask is mapped with the file generic mapping, and `MAXIMUM_ALLOWED` is counted as every right.
  - A trusted SID, or OWNER RIGHTS (`S-1-3-4`, given that the owner is trusted), may hold anything.
- **Change rights**: `FILE_WRITE_DATA` (ADD_FILE), `FILE_APPEND_DATA` (ADD_SUBDIRECTORY), `FILE_WRITE_EA`, `FILE_DELETE_CHILD`, `FILE_WRITE_ATTRIBUTES`, `DELETE`, `WRITE_DAC`, `WRITE_OWNER`, and `ACCESS_SYSTEM_SECURITY`. Evidence for including `FILE_WRITE_ATTRIBUTES`: it is enough for `FSCTL_SET_REPARSE_POINT` on a non-empty data file (non-symlink tags), and for setting per-directory case sensitivity (`NtSetInformationFile` `FileCaseSensitiveInformation` requires a handle opened with `FILE_WRITE_ATTRIBUTES`: https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/nf-ntifs-ntsetinformationfile).
- **What a foreign principal may hold**:
  - `Managed` (the home and below, files anywhere): nothing.
  - `Ancestor` directory: only `FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY`. Adding needs only those rights, while removing, renaming, or replacing an existing entry needs `DELETE` on that entry, which the entry's own check refuses, or `FILE_DELETE_CHILD` on the parent, which this rule refuses (MS-FSA rename and delete). An entry another principal adds is theirs, so it fails the owner rule if the walk ever reaches it. The walk only opens names that already exist on our path, and holds them open. This tolerance is what lets stock `C:\` pass: Authenticated Users have "create folders" on the root itself, and Modify only as inherit-only. Tolerating adds is safe **only above the home**. In the managed region, adding would let a principal plant a hard link to any victim-owned file it can open under an absent name (FileLinkInformation has no source access check), or pre-create the home or a lock file.
  - `VolumeRoot`: as `Ancestor`, plus `DELETE` is ignored, because "a volume's root directory cannot be renamed" and a root holding our chain is never empty.
- **Inheritable ACEs are ignored on reads.** They affect only objects created later, and every such object is checked when it is read. Writers do not rely on inheritance (§8).

Seven seeded mutations of the evaluator, covering each rule (refusal check, inherit-only skip, deny handling, owner check, label threshold, root `DELETE`, and generic mapping), each made the test suite fail (`prototype/` `cargo test`).

## 7. Lifetimes, sharing, and errors

- **Pinning.**
  - Each directory handle holds `FILE_TRAVERSE`, a data right in the MS-FSA sharing rule, and **does not share delete**. While it lives, any `DELETE` open of that directory gets `STATUS_SHARING_VIOLATION`, so the directory cannot be renamed or deleted (MS-FSA 2.1.5.1.2.1).
  - Without a data right, denying delete sharing would pin nothing; the probe's M1 has that negative control.
  - On NTFS, a directory with an open file beneath it also cannot be renamed (`FILE_RENAME_INFORMATION`, MS-FSA <186>). This is the documented property that pins ancestors while the file is open.
  - The cost: a concurrent opener that already holds `DELETE` on a chain directory makes the walk fail with `InUse`. That is reported, never treated as absence.
- **Stable bytes.**
  - The file handle shares read and delete but **not write**. While it is open, nothing can open the file for writing, and an existing writer makes the open fail (`InUse`, "close the program editing it").
  - Sharing delete lets a writer's POSIX-semantics replacement proceed; the reader keeps the old object.
  - The read length must equal `EndOfFile`.
  - Accepted residual risk: Windows checks access at open time. A handle or a writable section mapping obtained **before** a DACL was tightened keeps working. Deny-write sharing detects live handles but not an orphaned writable mapping. Guidance: recreate a file whose ACL was ever loose. Bake's writers always create new files.
- **Identity.**
  - `FILE_ID_INFO` (`VolumeSerialNumber` plus `FileId128`) of the handles, compared only between objects that are open at the same moment.
  - On ReFS, `FileId128` includes the parent directory index (MS-FSA <48>), so it is not stable across renames. Never persist an ID.
  - `GetFinalPathNameByHandleW` is used for display only.
- **Error mapping. Nothing but `STATUS_OBJECT_NAME_NOT_FOUND` is ever "absent".**

| NTSTATUS / Win32 | Outcome |
|---|---|
| `STATUS_OBJECT_NAME_NOT_FOUND` on a relative single-component open | `Ok(None)` (absent) |
| `STATUS_OBJECT_PATH_NOT_FOUND` (should not occur for one component) | `Io` refusal |
| `STATUS_ACCESS_DENIED` | `Io` refusal ("cannot open; Bake cannot verify it"). This includes an empty DACL denying `FILE_READ_DATA`. |
| `STATUS_SHARING_VIOLATION`, `STATUS_DELETE_PENDING`, `DeletePending` | `InUse` |
| `STATUS_REPARSE_POINT_ENCOUNTERED`, reparse attribute or tag | `Link` |
| `STATUS_NOT_A_DIRECTORY`, `STATUS_FILE_IS_A_DIRECTORY` | `NotRegular` |
| `STATUS_INVALID_PARAMETER` with `OBJ_DONT_REPARSE` | One retry without the flag, then `Io` |
| `GetKernelObjectSecurity` failure, a needed size above 1 MiB, malformed bytes | `UnreadableSecurity` |
| Any other status, including network and unknown codes | `Io`, carrying the raw value |

Teardown: dropping a `TrustedDir` closes its handles. There are no callbacks, threads, or late work.

## 8. Management writes

The plugin fix on main already refuses an untrusted existing configuration before `JsonFileStore::update`. On Windows the following steps compose that check with a locked atomic update **without assuming the lock secures path identity**. Identity comes from the held, checked directory handle. The lock only serializes cooperating Bake writers.

1. `TrustedDir::open_home` and then `dir(PLUGINS_DIR)`, creating missing directories **relative to the checked parent** with `NtCreateFile(FILE_CREATE, FILE_DIRECTORY_FILE)` and an explicit protected DACL (`D:P(A;OICI;FA;;;<user>)(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)`). Each new directory is re-checked as `Managed`. If the create returns `STATUS_OBJECT_NAME_COLLISION`, the existing object is opened and checked like any other: a pre-planted directory owned by another principal is refused, never written into.
2. The lock: `.<name>.lock` is opened relative to the checked directory (`FILE_OPEN_IF`, no reparse, explicit DACL), its owner and type are checked, and `LockFileEx` is taken through `File::try_lock`.
3. **Under the lock**, the current bytes are read with `TrustedDir::read`, which checks them. A `Refusal` aborts with no write and says why. The change closure receives the object parsed from **those** bytes, not from a separate path read, so an untrusted record is never re-endorsed.
4. The temporary file `.<name>.<16 hex>.tmp` is created relative to the directory (`FILE_CREATE`). Its DACL is either the checked original's DACL, copied handle to handle with `GetKernelObjectSecurity` then `SetKernelObjectSecurity` (DACL only, before any bytes are written), or the explicit private DACL for a new file. The bytes are written and `FlushFileBuffers` is called.
5. **Rename by handle into the pinned directory**: `SetFileInformationByHandle(temp, FileRenameInfoEx, {Flags: REPLACE_IF_EXISTS|POSIX_SEMANTICS, RootDirectory: <checked dir handle>, FileName: <name>})`. A session reading the file blocks a classic replace ("A file cannot be renamed if a file with the same name exists and has open handles"); POSIX semantics allows it. Then reopen the name relatively and compare `FileIdInfo` with the temp's. A mismatch is a committed-but-unverified error, reported. Windows has no directory flush, so durability after power loss relies on NTFS metadata logging. The Windows store reports no `Unsynced`, and documents that.
6. Plugin store install: create the temporary directory relative to the checked `store` handle with the explicit DACL, write both files relative to it, set read-only through `FileBasicInfo`, **close the file handles** (an open file beneath blocks a directory rename), then rename the directory with `FileRenameInfoEx` (no replace, `RootDirectory` = the store handle). A collision means another install won; read and verify that copy.

The same descriptor-relative shape also fits Unix (`openat`, `renameat`, `fchmod`), so one `JsonFileStore::update_in(&TrustedDir, &EntryName, limit, change)` can serve both platforms.

## 9. Integration steps

As a PR stack against `develop`. Each PR needs native Windows CI, a macOS preflight, the doc-sync, and the inventory. Windows reading stays disabled until PR 2's native evidence passes.

1. **`owner_files` module.**
   - Unix: merge the two walkers. Every placement or `.git` lookup becomes tri-state (an error refuses).
   - Windows: §5 to §7, pure rules plus FFI. Add the `windows-sys` features listed in §1.
   - Tests: §10. No consumer changes yet, so Windows behavior is unchanged.
2. **MCP consumer.** `mcp/home.rs::load_home_mcp_config`:
   - Becomes `open_home(home, &Placement{workspace, user_home})?.read(MCP_CONFIG_FILE, MAX_CONFIG_BYTES)`. `Ok(None)` gives `McpConfig::default()`, a `Refusal` gives `refused(path, reason)`, and the bytes go to `read_config`.
   - Delete the `#[cfg(not(unix))]` `UNCHECKED` branch for Windows. Keep `UNCHECKED_PLATFORM` for other targets. That constant is public API, so update the `docs/development.md` crate row, the roadmap Dev Note, `gen-rust-migration-inventory`, `verify-rust-migration-inventory`, and `doc-sync`.
   - Update the module documentation: replace "Other platforms" with the Windows rules and §3's unsupported list.
3. **Plugins consumer.**
   - `trust::SUPPORTED` covers Windows only after native qualification.
   - `read_config`, `read_copy`, and `check_outside` move to `owner_files` (identity-based placement in place of `canonicalize` plus `starts_with`).
   - The non-Unix `read_regular` path is deleted.
4. **Writes.** `JsonFileStore::update_in` (§8) for `plugins.json`, and the store install path; `update` takes the trusted read under the lock on every platform. The path-based Windows `copy_dacl` in `json_store` is replaced by the handle-based copy.
5. **Docs.**
   - Module docs, `docs/development.md`, and the support register entry for Windows configuration.
   - State plainly which locations are unsupported (§3).
   - Model-visible effect: on Windows, configured MCP servers start, which changes the tool list only where `mcp.json` exists. The standard eval host has no `mcp.json`. Say that in the PR, rather than inventing an eval delta.

## 10. Test matrix (with negative controls)

Every Windows test creates **its own** directory under `std::env::temp_dir()`, with a random name and an explicit protected DACL. It changes ACLs only on objects it created, and deletes exactly those objects, by path and in reverse order; `RemoveDirectoryW` removes junctions without following them. No wildcard deletion, no profile or host ACL changes, and no module mutex standing in for host ownership. A skipped case (for example, symlink privilege unavailable) reports `SKIPPED` with the reason. It never passes silently.

| # | Case | Expected | Negative control |
|---|---|---|---|
| P1–P12 | Pure evaluator (probe `sd.rs` tests, port to `rules.rs`): stock profile; stock `C:\` as root, ancestor, and managed; inherited Modify under a drive root; owner and NULL/empty DACL; every change right on files; ancestor add-versus-replace; `VolumeRoot` `DELETE`; inherit-only and deny; callback, object, and unknown ACE types; sandbox SIDs (S-1-15, S-1-4, logon); labels; malformed and mutated bytes | As in §6 (passing on Linux today) | Seeded mutations, each caught (§6) |
| W1 | Default-like home under a private test directory | Trusted bytes equal the file | — |
| W2 | Test directory grants Everyone, Authenticated Users, an `S-1-4-*` or an `S-1-15-2-1` SID `FILE_WRITE_DATA` on the file or the home | `ForeignWriter` naming the SID | The same with read-only grants passes |
| W3 | Null DACL set on own file; empty DACL | `NullDacl`; `Io(ACCESS_DENIED)` (**not** absent) | — |
| W4 | Low label on own file | `LowIntegrity` | Medium label passes |
| W5 | Home component replaced by a junction (`FSCTL_SET_REPARSE_POINT` on an own empty directory) | `Link` | The same open without `FILE_OPEN_REPARSE_POINT` follows it (probe M3) |
| W6 | `mcp.json` as a file symlink | `Link`, or `SKIPPED` without privilege or Developer Mode | — |
| W7 | Hard link to an own file in the home | Read passes, `links == 2` recorded | — |
| W8 | File larger than the limit; directory named `mcp.json` | `TooLarge`; `NotRegular` | — |
| W9 | Another handle holds the file open for write | `InUse` | A read-only holder passes |
| W10 | File delete pending | `InUse` | — |
| W11 | Home is the workspace, or holds `.git` (file or directory); workspace inside the managed root | `Placement` | Profile and its ancestors exempt |
| W12 | Spelled `\\server\x`, `\\?\UNC\…`, `\\?\Volume{…}\`, `C:\x\nul`, `C:\x\a:b` | `Unsupported`, no I/O | — |
| W13 | Seam after component *k* is opened; the hook tries to rename component *k* and to swap component *k+1* for a junction | Rename fails (sharing violation); the swap is refused or never seen | A test-only weakened walker (shares delete, no `FILE_TRAVERSE`): the rename succeeds, **showing the test detects loss of pinning** |
| W14 | Restricted-token "other principal" (synthetic `S-1-4-<rand>` restricting SID, impersonating thread): with only ADD on an ancestor-like directory, it can add but not replace, rename, or delete the existing entry; the ancestor rule passes | As probe M4 | Grant `FILE_DELETE_CHILD`: replace succeeds and the ancestor rule refuses |
| W15 | Concurrent `enable` and session read: a reader holds `plugins.json` while `update_in` replaces it | The POSIX rename succeeds; the reader sees old bytes; the next read sees new bytes and the checked identity | Classic `MoveFileExW` under a reader fails (probe M5) |
| W16 | `plugin disable` on a home whose `plugins.json` grants Everyone write | Refused; the file and its bytes are unchanged (no laundering) | — |
| W17 | `update_in` when the plugins directory already exists with a foreign change grant (the test grants its synthetic `S-1-4-*` SID `FILE_ADD_FILE` on its own directory; a test cannot create an object owned by another user without privilege) | Refused before any write; no lock or temporary file created | Without the grant, the update succeeds |
| W18 | `BAKE_HOME`, `DSH_HOME`, and default fallback: resolution unchanged; trust applies to the resolved path | As `home.rs` tests | — |

Native qualification, on a GitHub `windows-latest` runner, after review:

- The probe's `dump` on `C:\`, `C:\Users`, `%USERPROFILE%`, `D:\` (if present), `%RUNNER_TEMP%`, and `%GITHUB_WORKSPACE%` records stock DACLs and the verdicts. This is what confirms or corrects the C:\ assumption in §6.
- `walk` of `%USERPROFILE%\.bake` `mcp.json`.
- `mechanisms` on `%RUNNER_TEMP%`.

Runner caveat: the runner account is an elevated administrator, so objects it creates are owned by Administrators, which is trusted. **A standard-user run is still unqualified.** It needs an ephemeral local standard user created on the runner, a host-wide mutation that needs explicit review, or manual evidence from a non-admin machine.

## 11. Probe (`prototype/`)

- `src/sd.rs`: the pure evaluator and 12 tests (pass on Linux).
- `src/win.rs`: the walk prototype (§5), `dump`, and the mechanisms:
  - M1: pinning by sharing, with the data-access negative control.
  - M2: an open file pins its ancestors.
  - M3: junction opened, not followed; `OBJ_DONT_REPARSE` status; file symlink; hard link.
  - M4: restricted-token principal, add-not-replace, Win32 and NT hard-link planting, with the `DELETE_CHILD` control.
  - M5: classic versus POSIX replace under readers that share or deny delete.
  - M8: `OBJ_CASE_INSENSITIVE` needed or not.
- Checked: `cargo +1.99.0 check` and `clippy -D warnings` for `x86_64-pc-windows-gnu`, plus Linux `clippy --all-targets` and `test`. Linking with `zcc-win` fails: the zig toolchain has no mingw `msvcrt` import library. That is expected; build it on the native runner with MSVC.
- Proposed CI job: see `ci-qualification.yml`. Do **not** add it to the product workflow without review.

## 12. Ambiguities and unsupported cases (pending native evidence)

1. The stock DACLs of `C:\`, `C:\Users`, and profiles on current Windows 11 and Server images are not in Microsoft documentation. The ancestor tolerance (ADD only) is designed from commonly reported defaults and must be confirmed by `dump`.
2. `FILE_ID_INFO`'s page says "Minimum supported client: None supported", but `FileIdInfo` is believed to work on Windows 10 and 11. The probe records `idError`. Fallback options: `NtQueryInformationFile(FileIdInformation)` or `BY_HANDLE_FILE_INFORMATION`, which is 64-bit and enough for NTFS.
3. Whether `MoveFileExW` uses POSIX rename semantics on current builds, and whether a POSIX replacement requires every reader to share delete (M5). The design does not depend on the answer: writers use `FileRenameInfoEx` explicitly and readers share delete.
4. Whether ReFS refuses renaming a directory with open files beneath it, as NTFS does. The MS-FSA rule is generic, but <186> documents NTFS only. The design also pins each directory directly through sharing, so it does not rely on this.
5. Whether the per-directory case-sensitivity flag can be set on a non-empty directory (community reports conflict). The design refuses foreign `FILE_WRITE_ATTRIBUTES` on every chain directory, so it does not depend on this.
6. Hard-link creation without write access by a non-sandboxed caller (M4 records both the Win32 and NT outcomes). The design forbids foreign ADD in the managed region, so it does not depend on this.
7. Whether generic bits are stored unmapped in ACEs (they are mapped conservatively), and object ACEs on files (they grant nothing without a type list, but are treated as grants). Both choices can only refuse more.
8. `OBJ_DONT_REPARSE` availability on older builds (retry without it for single components).
9. Standard (non-admin) users, domain-joined profiles, roaming or redirected profiles, Dev Drive, BitLocker, and EFS are all unqualified.
10. Directory durability: Windows has no directory flush.
11. Orphaned writable section mappings that predate an ACL tightening cannot be detected (§7).
12. Unsupported by design in phase 1: links on the home path, UNC and network locations, `subst`, FAT and exFAT, and volume-GUID paths.

## 13. Observations on the current Unix implementations (for main; not edited)

1. `mcp/home.rs` `has_entry` returns `fstatat(...) == 0`, so `EACCES`, `EIO`, and similar errors read as "no `.git`": an unknown error is taken as absence. Make it tri-state, refusing on errors other than `ENOENT`.
2. `plugins/installed/trust.rs` `check_outside` compares `canonicalize`d paths by `starts_with`. Its `.git` probe is path-based `symlink_metadata(...).is_ok()`, so errors read as absent. It also runs separately from the descriptor walk. MCP compares `(dev, ino)` of opened handles. Unify both on handle identity.
3. Plugins `update` runs `read_config()` (the trusted read) **before** `JsonFileStore::update` takes the lock, and the store then reads the file again by path. Only trusted principals can exploit that window, given the directory rules, but the check belongs inside the lock and on the bytes the change uses (§8 step 3).
4. `json_store`'s Windows `copy_dacl` is path-based (`GetFileSecurityW`/`SetFileSecurityW`) and copies the DACL of whatever is at the path at that moment. Once Windows trust lands, copy from the checked handle.
5. `TrustedDir::read` (plugins, Unix) does not compare the bytes read with `fstat` size. That is minor; MCP's `read_config` bound covers the size limit.
