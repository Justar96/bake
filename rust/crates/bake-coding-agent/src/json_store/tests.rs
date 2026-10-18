//! The locked JSON store: refusals, cleanup, kept access, durability
//! errors, and updates from independent processes.
//!
//! The cross-process tests run this test binary again, filtered to
//! [`child_process`], with its role in the child's own environment. Children
//! report readiness on stdout and wait for a line on stdin, so the parent
//! releases them together without sleeping. Every wait on a child is
//! bounded, and a child is killed, reaped, and its output reader joined
//! when a test fails.

use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;

use bake_ai::ModelThinkingLevel;
use serde_json::json;

use super::*;
use crate::auth_storage::{AuthStorage, Credential};
use crate::settings::{Settings, SettingsStore};
use crate::test_support::TempDir;

const LIMIT: u64 = 1024;

/// The directory's entries, sorted.
fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("read dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("read")).expect("json")
}

/// Sets `a` to 1; the object as stored, for an update that was flushed.
fn set_a(store: &JsonFileStore) -> Result<Map<String, Value>, StoreError> {
    let updated = store.update(|object| set_member(object, "a", json!(1)))?;
    assert!(updated.unsynced.is_none(), "{:?}", updated.unsynced);
    Ok(updated.object)
}

#[test]
fn creates_a_missing_file_and_keeps_other_members() {
    let dir = TempDir::new("json-store-create");
    let path = dir.join("nested/data.json");
    let store = JsonFileStore::new(path.clone(), LIMIT);
    // A change that changes nothing writes nothing.
    assert_eq!(store.update(|_| false).expect("noop").object, Map::new());
    assert!(!path.exists());
    set_a(&store).expect("create");
    assert_eq!(fs::read_to_string(&path).expect("read"), "{\n  \"a\": 1\n}");

    fs::write(&path, "\u{feff}{\"z\":{\"deep\":[1,2]},\"a\":0,\"m\":null}").expect("edit");
    let stored = set_a(&store).expect("update");
    assert_eq!(
        Value::Object(stored),
        json!({ "z": { "deep": [1, 2] }, "a": 1, "m": null })
    );
    let keys: Vec<String> = read_json(&path)
        .as_object()
        .expect("object")
        .keys()
        .cloned()
        .collect();
    assert_eq!(keys, ["z", "a", "m"]);
    assert!(remove_member(&mut Map::new(), "a").eq(&false));
    store
        .update(|object| remove_member(object, "z"))
        .expect("remove");
    let keys: Vec<String> = read_json(&path)
        .as_object()
        .expect("object")
        .keys()
        .cloned()
        .collect();
    assert_eq!(keys, ["a", "m"]);
    assert_eq!(
        entries(&dir.join("nested")),
        [".data.json.lock", "data.json"]
    );
}

// Pi: "does not overwrite malformed auth files", extended to every refusal.
#[test]
fn refuses_to_replace_a_file_it_cannot_read_as_an_object() {
    let dir = TempDir::new("json-store-refuse");
    let path = dir.join("data.json");
    let store = JsonFileStore::new(path.clone(), LIMIT);
    let large = format!("{{\"a\":\"{}\"}}", "x".repeat(2000));
    let cases: [&[u8]; 6] = [
        b"{invalid-json",
        b"[1,2]",
        b"\"text\"",
        b"",
        b"{\"a\":\"\xff\"}",
        large.as_bytes(),
    ];
    for bytes in cases {
        fs::write(&path, bytes).expect("write");
        let error = set_a(&store).expect_err("refused");
        assert!(
            matches!(error, StoreError::Refused { .. }),
            "{error:?} for {bytes:?}"
        );
        assert!(error.to_string().starts_with("Refusing to update "));
        assert_eq!(fs::read(&path).expect("read"), bytes);
        assert_eq!(entries(dir.path()), [".data.json.lock", "data.json"]);
    }

    fs::remove_file(&path).expect("remove");
    fs::create_dir(&path).expect("dir");
    assert!(matches!(set_a(&store), Err(StoreError::Refused { .. })));
    assert!(path.is_dir());

    // An update whose result would exceed the limit is refused too.
    fs::remove_dir(&path).expect("remove dir");
    let error = store
        .update(|object| set_member(object, "a", "x".repeat(2000).into()))
        .expect_err("too large");
    assert!(
        error
            .to_string()
            .contains("would be larger than 1024 bytes")
    );
    assert!(!path.exists());
}

#[cfg(unix)]
#[test]
fn refuses_a_symbolic_link_and_leaves_its_target() {
    let dir = TempDir::new("json-store-link");
    let target = dir.join("target.json");
    fs::write(&target, "{}").expect("target");
    let path = dir.join("data.json");
    std::os::unix::fs::symlink(&target, &path).expect("link");
    let store = JsonFileStore::new(path.clone(), LIMIT);
    assert!(matches!(set_a(&store), Err(StoreError::Refused { .. })));
    assert_eq!(fs::read_to_string(&target).expect("read"), "{}");
    assert!(
        fs::symlink_metadata(&path)
            .expect("meta")
            .file_type()
            .is_symlink()
    );
}

#[test]
fn a_failed_replace_keeps_the_original_and_removes_its_temporary_file() {
    let dir = TempDir::new("json-store-fail");
    let path = dir.join("data.json");
    fs::write(&path, "{\"keep\":true}").expect("write");
    let failing = JsonFileStore::new(path.clone(), LIMIT).with_fault(Fault::BeforeRename);
    let error = set_a(&failing).expect_err("injected");
    assert!(error.to_string().contains("injected failure"));
    assert!(!error.is_committed());
    assert_eq!(fs::read_to_string(&path).expect("read"), "{\"keep\":true}");
    assert_eq!(entries(dir.path()), [".data.json.lock", "data.json"]);
    // The lock was released: the next update goes through.
    set_a(&JsonFileStore::new(path.clone(), LIMIT)).expect("retry");
    assert_eq!(read_json(&path), json!({ "keep": true, "a": 1 }));
}

#[test]
fn an_update_removes_temporary_files_left_by_a_dead_writer() {
    let dir = TempDir::new("json-store-orphans");
    let path = dir.join("data.json");
    fs::write(dir.join(".data.json.0123456789abcdef.tmp"), "{\"half\":").expect("orphan");
    // Names that are not this store's temporary files stay.
    for other in [".data.json.short.tmp", ".other.json.0123456789abcdef.tmp"] {
        fs::write(dir.join(other), "").expect("other");
    }
    set_a(&JsonFileStore::new(path, LIMIT)).expect("update");
    assert_eq!(
        entries(dir.path()),
        [
            ".data.json.lock",
            ".data.json.short.tmp",
            ".other.json.0123456789abcdef.tmp",
            "data.json"
        ]
    );
}

#[test]
fn a_held_lock_times_out_without_writing() {
    let dir = TempDir::new("json-store-held");
    let path = dir.join("data.json");
    fs::write(&path, "{}").expect("write");
    let store = JsonFileStore::new(path.clone(), LIMIT);
    let held = store.lock().expect("hold");
    let waiting = store.clone().with_lock_timeout(Duration::from_millis(20));
    let error = set_a(&waiting).expect_err("held");
    assert!(matches!(error, StoreError::Locked { .. }), "{error:?}");
    assert!(!error.is_committed());
    assert!(error.to_string().starts_with("Timed out after "));
    assert_eq!(fs::read_to_string(&path).expect("read"), "{}");
    drop(held);
    // The short timeout checks the held-lock refusal. The positive control
    // uses the ordinary acquisition deadline; a leaked lock still fails.
    set_a(&store).expect("released");
}

#[cfg(unix)]
#[test]
fn new_files_are_private_and_replacements_keep_the_mode() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |path: &Path| fs::metadata(path).expect("meta").permissions().mode() & 0o777;
    let dir = TempDir::new("json-store-mode");
    let home = dir.join("home");
    let path = home.join("data.json");
    let store = JsonFileStore::new(path.clone(), LIMIT);
    set_a(&store).expect("create");
    assert_eq!(mode(&home), 0o700);
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&home.join(".data.json.lock")), 0o600);
    // Pi: "preserves the mode of an existing auth file".
    fs::set_permissions(&path, fs::Permissions::from_mode(0o660)).expect("chmod");
    store
        .update(|object| set_member(object, "b", json!(2)))
        .expect("update");
    assert_eq!(mode(&path), 0o660);
}

// A failed directory flush comes after the replacement: the error says the
// update is in place, and the file, the cleanup, and the lock show it.
#[test]
fn a_failed_directory_flush_reports_a_committed_update() {
    let dir = TempDir::new("json-store-unsynced");
    let path = dir.join("data.json");
    fs::write(&path, "{\"keep\":true}").expect("write");
    let store = JsonFileStore::new(path.clone(), LIMIT).with_fault(Fault::DirectorySync);
    let updated = store
        .update(|object| set_member(object, "a", json!(1)))
        .expect("replaced");
    assert_eq!(
        Value::Object(updated.object),
        json!({ "keep": true, "a": 1 })
    );
    let error = updated.unsynced.expect("unsynced");
    assert!(error.is_committed());
    assert!(matches!(error, StoreError::Unsynced { .. }), "{error:?}");
    let message = error.to_string();
    assert!(
        message.starts_with("Updated ")
            && message.contains("may not survive a power loss: injected failure"),
        "{message}"
    );
    assert!(!message.contains("keep"), "{message}");
    assert_eq!(read_json(&path), json!({ "keep": true, "a": 1 }));
    assert_eq!(entries(dir.path()), [".data.json.lock", "data.json"]);
    set_a(&JsonFileStore::new(path, LIMIT)).expect("lock released");
}

// Pi writes in place, which a read-only file refuses; a rename would not.
#[test]
fn refuses_a_read_only_file() {
    let dir = TempDir::new("json-store-read-only");
    let path = dir.join("data.json");
    fs::write(&path, "{}").expect("write");
    let writable = fs::metadata(&path).expect("meta").permissions();
    let mut read_only = writable.clone();
    read_only.set_readonly(true);
    fs::set_permissions(&path, read_only).expect("read-only");
    // Restored, so the test's directory can be removed.
    let restore = || fs::set_permissions(&path, writable.clone()).expect("restore");
    #[cfg(unix)]
    // SAFETY: `geteuid` has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        // Permission bits do not bind root.
        restore();
        return;
    }
    let error = set_a(&JsonFileStore::new(path.clone(), LIMIT)).expect_err("read-only");
    restore();
    assert!(
        matches!(
            error,
            StoreError::Io {
                operation: "open for reading and writing",
                ..
            }
        ),
        "{error:?}"
    );
    assert!(!error.is_committed());
    assert_eq!(fs::read_to_string(&path).expect("read"), "{}");
    assert_eq!(entries(dir.path()), [".data.json.lock", "data.json"]);
}

#[cfg(unix)]
#[test]
fn a_replacement_keeps_the_group_of_the_file() {
    use std::os::unix::fs::MetadataExt;
    let dir = TempDir::new("json-store-group");
    let path = dir.join("data.json");
    fs::write(&path, "{}").expect("write");
    let own = fs::metadata(&path).expect("meta").gid();
    // SAFETY: a size query, then a buffer of that size.
    let groups = unsafe {
        let count = libc::getgroups(0, std::ptr::null_mut());
        let mut groups = vec![0; usize::try_from(count).unwrap_or(0)];
        let count = libc::getgroups(count, groups.as_mut_ptr());
        groups.truncate(usize::try_from(count).unwrap_or(0));
        groups
    };
    let Some(other) = groups.into_iter().find(|group| *group != own) else {
        eprintln!("skipped: the test user belongs to no second group");
        return;
    };
    std::os::unix::fs::chown(&path, None, Some(other)).expect("chgrp");
    set_a(&JsonFileStore::new(path.clone(), LIMIT)).expect("update");
    assert_eq!(fs::metadata(&path).expect("meta").gid(), other);
}

/// A POSIX ACL in Linux's `system.posix_acl_*` extended attribute format:
/// the owner, user 12345, the group, the mask, and others.
#[cfg(target_os = "linux")]
fn acl(owner: u16, user: u16, group: u16, mask: u16) -> Vec<u8> {
    const ANY: u32 = u32::MAX;
    let mut bytes = 2u32.to_le_bytes().to_vec();
    for (tag, perm, id) in [
        (0x01u16, owner, ANY),
        (0x02, user, 12345),
        (0x04, group, ANY),
        (0x10, mask, ANY),
        (0x20, 0, ANY),
    ] {
        bytes.extend(tag.to_le_bytes());
        bytes.extend(perm.to_le_bytes());
        bytes.extend(id.to_le_bytes());
    }
    bytes
}

#[cfg(target_os = "linux")]
fn xattr_name(name: &str) -> std::ffi::CString {
    std::ffi::CString::new(name).expect("name")
}

#[cfg(target_os = "linux")]
fn path_name(path: &Path) -> std::ffi::CString {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes()).expect("path")
}

#[cfg(target_os = "linux")]
fn set_xattr(path: &Path, name: &str, value: &[u8]) -> io::Result<()> {
    let (path, name) = (path_name(path), xattr_name(name));
    // SAFETY: NUL-terminated strings and `value.len()` bytes.
    let set = unsafe {
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    if set == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn get_xattr(path: &Path, name: &str) -> Option<Vec<u8>> {
    let (path, name) = (path_name(path), xattr_name(name));
    let mut value = vec![0u8; 4096];
    // SAFETY: NUL-terminated strings and `value.len()` bytes.
    let read = unsafe {
        libc::getxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            value.len(),
        )
    };
    let read = usize::try_from(read).ok()?;
    value.truncate(read);
    Some(value)
}

#[cfg(target_os = "linux")]
#[test]
fn a_replacement_keeps_the_access_acl_and_drops_an_inherited_one() {
    use std::os::unix::fs::PermissionsExt;
    const ACCESS: &str = "system.posix_acl_access";
    let dir = TempDir::new("json-store-acl");
    let path = dir.join("data.json");
    fs::write(&path, "{}").expect("write");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
    let narrowed = acl(6, 4, 0, 4);
    if let Err(error) = set_xattr(&path, ACCESS, &narrowed) {
        assert_eq!(error.raw_os_error(), Some(libc::EOPNOTSUPP), "{error}");
        eprintln!("skipped: the temporary directory has no POSIX ACLs");
        return;
    }
    let mode = |path: &Path| fs::metadata(path).expect("meta").permissions().mode() & 0o777;
    let before = mode(&path);
    set_a(&JsonFileStore::new(path.clone(), LIMIT)).expect("update");
    assert_eq!(get_xattr(&path, ACCESS), Some(narrowed));
    assert_eq!(mode(&path), before);

    // A default ACL on the directory gives each new file, the temporary one
    // included, an access ACL; a file without one keeps having none.
    let inherited = dir.join("inherited");
    fs::create_dir(&inherited).expect("dir");
    set_xattr(&inherited, "system.posix_acl_default", &acl(7, 6, 0, 6)).expect("default acl");
    let path = inherited.join("data.json");
    fs::write(&path, "{}").expect("write");
    let path_c = path_name(&path);
    let name = xattr_name(ACCESS);
    // SAFETY: NUL-terminated strings.
    assert_eq!(
        unsafe { libc::removexattr(path_c.as_ptr(), name.as_ptr()) },
        0
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
    set_a(&JsonFileStore::new(path.clone(), LIMIT)).expect("update");
    assert_eq!(get_xattr(&path, ACCESS), None);
    assert_eq!(mode(&path), 0o600);
}

#[cfg(target_os = "macos")]
#[test]
fn a_replacement_keeps_the_acl() {
    let dir = TempDir::new("json-store-acl");
    let path = dir.join("data.json");
    fs::write(&path, "{}").expect("write");
    let status = Command::new("/bin/chmod")
        .args(["+a", "nobody allow read"])
        .arg(&path)
        .status()
        .expect("chmod +a");
    assert!(status.success());
    // Compare the external ACL bytes: directory services can print an
    // identical qualifier as a user name or UUID on consecutive lookups.
    let acl = |path: &Path| {
        use std::ffi::c_void;
        use std::os::fd::AsRawFd;
        unsafe extern "C" {
            fn acl_get_fd(fd: libc::c_int) -> *mut c_void;
            fn acl_size(acl: *mut c_void) -> libc::ssize_t;
            fn acl_copy_ext(
                buf: *mut c_void,
                acl: *mut c_void,
                size: libc::ssize_t,
            ) -> libc::ssize_t;
            fn acl_free(acl: *mut c_void) -> libc::c_int;
        }
        struct Acl(*mut c_void);
        impl Drop for Acl {
            fn drop(&mut self) {
                // SAFETY: this ACL was allocated by acl_get_fd.
                unsafe { acl_free(self.0) };
            }
        }
        let file = File::open(path).expect("open ACL file");
        // SAFETY: the descriptor is open for this call.
        let raw = unsafe { acl_get_fd(file.as_raw_fd()) };
        if raw.is_null() {
            let error = io::Error::last_os_error();
            // Darwin reports ENOENT for an existing file without an ACL.
            if error.raw_os_error() == Some(libc::ENOENT) {
                return Vec::new();
            }
            panic!("acl_get_fd: {error}");
        }
        let acl = Acl(raw);
        // SAFETY: acl owns a valid ACL until this closure returns.
        let size = unsafe { acl_size(acl.0) };
        let mut bytes = vec![0; usize::try_from(size).expect("ACL size")];
        // SAFETY: the buffer is the size requested by acl_size.
        let copied = unsafe { acl_copy_ext(bytes.as_mut_ptr().cast(), acl.0, size) };
        bytes.truncate(usize::try_from(copied).expect("ACL copy"));
        bytes
    };
    let plain = dir.join("plain.json");
    fs::write(&plain, "{}").expect("plain");
    let before = acl(&path);
    assert_ne!(before, acl(&plain), "chmod must install an ACL entry");
    set_a(&JsonFileStore::new(path.clone(), LIMIT)).expect("update");
    assert_eq!(acl(&path), before);
}

#[cfg(windows)]
#[test]
fn a_replacement_keeps_the_dacl() {
    let icacls = |args: &[&std::ffi::OsStr]| {
        let output = Command::new("icacls").args(args).output().expect("icacls");
        assert!(output.status.success(), "{output:?}");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let dir = TempDir::new("json-store-dacl");
    let path = dir.join("data.json");
    let plain = dir.join("plain.json");
    fs::write(&path, "{}").expect("write");
    fs::write(&plain, "{}").expect("write");
    // Whether the DACL is protected from the directory's inheritance.
    let protected = |path: &Path| {
        use windows_sys::Win32::Security::{GetSecurityDescriptorControl, SE_DACL_PROTECTED};
        let mut descriptor = super::platform::read_dacl(path).expect("dacl");
        let (mut control, mut revision) = (0u16, 0u32);
        // SAFETY: a descriptor `GetFileSecurityW` returned.
        let ok = unsafe {
            GetSecurityDescriptorControl(
                descriptor.as_mut_ptr().cast(),
                &mut control,
                &mut revision,
            )
        };
        assert_ne!(ok, 0, "{}", io::Error::last_os_error());
        control & SE_DACL_PROTECTED != 0
    };
    // An explicit entry beside the inherited ones.
    icacls(&[
        path.as_os_str(),
        "/grant".as_ref(),
        "*S-1-5-19:(R)".as_ref(),
    ]);
    let before = icacls(&[path.as_os_str()]);
    assert!(!protected(&path));
    let plain_listing = icacls(&[plain.as_os_str()]);
    assert_ne!(
        before.replace("data.json", ""),
        plain_listing.replace("plain.json", ""),
        "the grant did not change the DACL"
    );
    set_a(&JsonFileStore::new(path.clone(), LIMIT)).expect("update");
    assert_eq!(icacls(&[path.as_os_str()]), before);
    assert!(!protected(&path));
    assert_eq!(read_json(&path), json!({ "a": 1 }));
    // A protected DACL must stay protected on the empty staging file and
    // after publication, even though its parent grants inherited access.
    icacls(&[path.as_os_str(), "/inheritance:d".as_ref()]);
    let before = icacls(&[path.as_os_str()]);
    assert!(protected(&path));
    let store = JsonFileStore::new(path.clone(), LIMIT);
    let current = store.read_current().expect("read").expect("existing");
    let temp = store.create_temp().expect("stage");
    let file = temp.file.as_ref().expect("open stage");
    store
        .keep_access(file, &temp.path, &current.original)
        .expect("copy access");
    assert_eq!(file.metadata().expect("metadata").len(), 0);
    assert!(protected(&temp.path));
    assert_eq!(
        icacls(&[temp.path.as_os_str()]).replace(
            temp.path
                .file_name()
                .expect("name")
                .to_str()
                .expect("utf8 name"),
            "data.json",
        ),
        before,
    );
    drop(temp);
    drop(current);
    store
        .update(|object| set_member(object, "a", json!(2)))
        .expect("protected update");
    assert_eq!(icacls(&[path.as_os_str()]), before);
    assert!(protected(&path));
    assert_eq!(read_json(&path), json!({ "a": 2 }));
}

const CHILD_ROLE: &str = "BAKE_JSON_STORE_TEST_ROLE";
const CHILD_HOME: &str = "BAKE_JSON_STORE_TEST_HOME";
const CHILD_INDEX: &str = "BAKE_JSON_STORE_TEST_INDEX";
const READY: &str = "bake-json-store-child-ready";
const WRITES_PER_CHILD: usize = 15;

/// How long a child may take to report ready, or to exit once released.
const CHILD_BOUND: Duration = Duration::from_secs(60);

/// Waits for the parent's go-ahead line.
fn wait_for_parent() {
    let mut line = String::new();
    let _ = io::stdin().read_line(&mut line);
}

/// A child of the cross-process tests; does nothing in an ordinary run.
#[test]
#[ignore = "run by the cross-process tests in a child process"]
fn child_process() {
    let (Ok(role), Ok(home)) = (std::env::var(CHILD_ROLE), std::env::var(CHILD_HOME)) else {
        return;
    };
    let home = PathBuf::from(home);
    let index = std::env::var(CHILD_INDEX).expect("index");
    match role.as_str() {
        "writer" => {
            // Loaded before the others write: stale for every write below.
            let mut auth = AuthStorage::load(&home.join("auth.json"));
            let settings = SettingsStore::new(&home);
            println!("{READY}");
            wait_for_parent();
            for write in 0..WRITES_PER_CHILD {
                auth.set_api_key(
                    &format!("child{index}-{write}"),
                    &format!("secret-{index}-{write}"),
                )
                .expect("auth write");
                settings
                    .set_model_thinking_level(
                        &format!("child{index}"),
                        &format!("model{write}"),
                        ModelThinkingLevel::Low,
                    )
                    .expect("settings write");
            }
        }
        "holder" => {
            let store = JsonFileStore::new(home.join("auth.json"), LIMIT);
            let _lock = store.lock().expect("lock");
            println!("{READY}");
            wait_for_parent();
        }
        // Never reports ready: the parent's bounded wait must end it.
        "silent" => wait_for_parent(),
        // Exits without reporting ready.
        "quits" => {}
        other => panic!("unknown role {other}"),
    }
}

/// What the output reader saw.
enum Output {
    Ready,
    Closed,
}

/// A child process re-running this test binary. Dropped while running, a
/// failed test included, it is killed, reaped within [`CHILD_BOUND`], and its
/// output reader joined.
struct ChildProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    output: Receiver<Output>,
    reader: Option<JoinHandle<()>>,
}

impl ChildProcess {
    fn spawn(role: &str, home: &Path, index: usize) -> Self {
        let mut child = Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "json_store::tests::child_process",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_ROLE, role)
            .env(CHILD_HOME, home)
            .env(CHILD_INDEX, index.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn child");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let (send, output) = mpsc::channel();
        // Drains the child's output until it closes, so the child never
        // writes to a closed pipe; libtest may print the marker after a
        // test's name on one line.
        let reader = std::thread::spawn(move || {
            if let Some(stdout) = stdout {
                for line in BufReader::new(stdout).lines() {
                    match line {
                        Ok(line) if line.contains(READY) => {
                            let _ = send.send(Output::Ready);
                        }
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
            }
            let _ = send.send(Output::Closed);
        });
        Self {
            child,
            stdin,
            output,
            reader: Some(reader),
        }
    }

    /// Waits up to `within` for the readiness line.
    fn wait_ready(&mut self, within: Duration) -> Result<(), String> {
        match self.output.recv_timeout(within) {
            Ok(Output::Ready) => Ok(()),
            Ok(Output::Closed) | Err(RecvTimeoutError::Disconnected) => {
                Err("the child closed its output before it was ready".to_owned())
            }
            Err(RecvTimeoutError::Timeout) => Err(format!(
                "the child did not report ready within {} ms",
                within.as_millis()
            )),
        }
    }

    fn release(&mut self) {
        let mut stdin = self.stdin.take().expect("stdin");
        stdin.write_all(b"go\n").expect("release child");
    }

    /// Polls for the child's exit until `within` has passed; once it has
    /// exited, joins the output reader, whose pipe the exit closed.
    fn wait_exit(&mut self, within: Duration) -> Result<ExitStatus, String> {
        let deadline = Instant::now() + within;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    if let Some(reader) = self.reader.take() {
                        reader
                            .join()
                            .map_err(|_| "the output reader panicked".to_owned())?;
                    }
                    return Ok(status);
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(None) => {
                    return Err(format!(
                        "the child did not exit within {} ms",
                        within.as_millis()
                    ));
                }
                Err(error) => return Err(format!("could not wait for the child: {error}")),
            }
        }
    }

    /// Kills the child and reaps it.
    fn kill(&mut self) -> ExitStatus {
        // Fails only when the child has already exited.
        let _ = self.child.kill();
        self.wait_exit(CHILD_BOUND)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    /// Waits for a released child to exit successfully.
    fn finish(mut self) {
        let status = self
            .wait_exit(CHILD_BOUND)
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(status.success(), "child failed: {status}");
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        if self.reader.is_none() {
            return;
        }
        drop(self.stdin.take());
        let _ = self.child.kill();
        // A reader whose child could not be reaped is left detached rather
        // than joined forever.
        let reaped = self.wait_exit(CHILD_BOUND);
        if !std::thread::panicking() {
            reaped.unwrap_or_else(|error| panic!("{error}"));
        }
    }
}

#[test]
fn a_child_that_never_reports_ready_is_bounded_and_cleaned_up() {
    let dir = TempDir::new("json-store-silent");
    let mut silent = ChildProcess::spawn("silent", dir.path(), 0);
    let error = silent
        .wait_ready(Duration::from_millis(200))
        .expect_err("silent");
    assert_eq!(error, "the child did not report ready within 200 ms");
    assert!(!silent.kill().success());
    assert!(silent.reader.is_none(), "the output reader was joined");

    let mut quits = ChildProcess::spawn("quits", dir.path(), 0);
    assert_eq!(
        quits.wait_ready(CHILD_BOUND),
        Err("the child closed its output before it was ready".to_owned())
    );
    quits.finish();
}

#[test]
fn independent_processes_with_stale_copies_lose_no_update() {
    const CHILDREN: usize = 4;
    let dir = TempDir::new("json-store-processes");
    let home = dir.join("home");
    fs::create_dir(&home).expect("home");
    let oauth = json!({
        "type": "oauth", "access": "access-token", "refresh": "refresh-token",
        "expires": 1_700_000_000_000u64, "accountId": "acct", "scopes": ["a", "b"],
    });
    let unknown = json!({ "type": "future", "blob": { "n": 1 } });
    fs::write(
        home.join("auth.json"),
        serde_json::to_string(&json!({ "oauth-provider": oauth, "future-provider": unknown }))
            .expect("auth"),
    )
    .expect("write auth");
    fs::write(
        home.join("settings.json"),
        r#"{"theme":"dark","packages":["npm:x"],"modelThinkingLevels":{"keep/me":"high"}}"#,
    )
    .expect("write settings");
    let yaml = b"provider: cliproxyapi\n";
    fs::write(home.join("settings.yaml"), yaml).expect("yaml");
    fs::write(home.join(".credentials.yaml"), yaml).expect("credentials yaml");

    let mut children: Vec<ChildProcess> = (0..CHILDREN)
        .map(|index| ChildProcess::spawn("writer", &home, index))
        .collect();
    for child in &mut children {
        child
            .wait_ready(CHILD_BOUND)
            .unwrap_or_else(|error| panic!("{error}"));
    }
    for child in &mut children {
        child.release();
    }
    for child in children {
        child.finish();
    }

    // A fresh instance after the writers exited: the restart view.
    let auth = read_json(&home.join("auth.json"));
    let auth = auth.as_object().expect("auth object");
    assert_eq!(auth.len(), 2 + CHILDREN * WRITES_PER_CHILD);
    assert_eq!(auth["oauth-provider"], oauth);
    assert_eq!(auth["future-provider"], unknown);
    let storage = AuthStorage::load(&home.join("auth.json"));
    for index in 0..CHILDREN {
        for write in 0..WRITES_PER_CHILD {
            assert_eq!(
                storage.read(&format!("child{index}-{write}")),
                Some(Credential::ApiKey {
                    key: Some(format!("secret-{index}-{write}")),
                    env: None
                })
            );
        }
    }
    let settings = read_json(&home.join("settings.json"));
    assert_eq!(settings["theme"], "dark");
    assert_eq!(settings["packages"], json!(["npm:x"]));
    let levels = settings["modelThinkingLevels"].as_object().expect("levels");
    assert_eq!(levels.len(), 1 + CHILDREN * WRITES_PER_CHILD);
    let loaded = Settings::load(&home);
    assert_eq!(
        loaded.model_thinking_level("keep", "me"),
        Some(ModelThinkingLevel::High)
    );
    assert_eq!(
        loaded.model_thinking_level("child3", "model14"),
        Some(ModelThinkingLevel::Low)
    );
    assert_eq!(fs::read(home.join("settings.yaml")).expect("yaml"), yaml);
    assert_eq!(
        fs::read(home.join(".credentials.yaml")).expect("yaml"),
        yaml
    );
    let mut names = entries(&home);
    names.retain(|name| name.ends_with(".tmp"));
    assert!(names.is_empty(), "temporary files left: {names:?}");
}

#[test]
fn a_lock_held_by_a_killed_process_does_not_block_later_updates() {
    let dir = TempDir::new("json-store-killed");
    let home = dir.path();
    let path = home.join("auth.json");
    fs::write(&path, "{}").expect("write");
    let mut holder = ChildProcess::spawn("holder", home, 0);
    holder
        .wait_ready(CHILD_BOUND)
        .unwrap_or_else(|error| panic!("{error}"));

    let waiting =
        JsonFileStore::new(path.clone(), LIMIT).with_lock_timeout(Duration::from_millis(20));
    assert!(matches!(set_a(&waiting), Err(StoreError::Locked { .. })));
    assert_eq!(fs::read_to_string(&path).expect("read"), "{}");

    assert!(!holder.kill().success());
    // The lock file remains, but the OS released the dead holder's lock.
    assert!(home.join(".auth.json.lock").exists());
    let mut storage = AuthStorage::load(&path);
    storage
        .set_api_key("after", "key")
        .expect("update after kill");
    assert_eq!(
        read_json(&path),
        json!({ "after": { "type": "api_key", "key": "key" } })
    );
}
