//! Named objects, the registry, the null device, and loopback through the
//! unqualified AppContainer runner: the scope-04 proofs that need Win32
//! objects rather than files.
//!
//! As in [`super`], every case decides from what this unconfined process
//! sees, beside an unconfined control that opens the same names. A fact
//! that depends on the host is printed as an `appcontainer-observation:`
//! line, and a proof the host cannot give as an `appcontainer-unproven:`
//! line, never as a silent pass. A connect is decided from the TCP table
//! and the listener, never from a timeout alone.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    ERROR_INSUFFICIENT_BUFFER, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP_STATE_ESTAB, MIB_TCP_STATE_LISTEN, MIB_TCP_STATE_SYN_SENT,
    TCP_TABLE_OWNER_PID_ALL,
};
use windows_sys::Win32::Security::FreeSid;
use windows_sys::Win32::Security::Isolation::{
    DeriveAppContainerSidFromAppContainerName, GetAppContainerNamedObjectPath,
    GetAppContainerRegistryLocation,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FileNameInfo, GetFileInformationByHandleEx, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, OpenFileMappingW, PAGE_READWRITE,
};
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_ALL_ACCESS, KEY_READ, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE,
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW,
};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::{
    CreateMutexW, OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE,
};

use super::super::unconfined;
use super::{
    Fixture, NetworkGrant, Scratch, assert_ok, observe, package_sid, ro, user_scratch, ww,
};

pub(super) const SCENARIOS: &[(&str, fn())] = &[
    (
        "named_objects_and_the_registry_stay_closed",
        named_objects_and_the_registry_stay_closed,
    ),
    (
        "objects_made_after_resume_end_with_the_run",
        objects_made_after_resume_end_with_the_run,
    ),
    ("the_null_device_decides_git", the_null_device_decides_git),
];

/// The probe operations behind `PROBE_ARG appcontainer objects ...`. Each
/// prints its verdicts and exits 0, so a denial is read from its output.
pub(super) fn probe(ops: &[&str]) -> std::io::Result<i32> {
    match ops {
        // One verdict line per `kind=name`, each object opened and closed.
        ["open", items @ ..] => {
            for item in items {
                let (kind, name) = item.split_once('=').expect("kind=name");
                println!("{}", verdict(open_object(kind, name)));
            }
            Ok(0)
        }
        ["registry", action, subkey] => {
            println!("{}", verdict(registry(action, subkey)));
            Ok(0)
        }
        ["registry-own"] => {
            println!("{}", own_registry_write());
            Ok(0)
        }
        ["hold", name] => hold(name),
        ["nul"] => {
            null_device();
            Ok(0)
        }
        ["connect-hold", address] => connect_hold(address),
        other => panic!("unknown appcontainer objects operation {other:?}"),
    }
}

/// A Win32 error as a verdict: `allowed` for success.
fn verdict(code: u32) -> String {
    match code {
        0 => "allowed".to_owned(),
        2 | 3 => "absent".to_owned(),
        5 => "denied".to_owned(),
        231 => "busy".to_owned(),
        other => format!("error:{other}"),
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// `handle` owned, or the thread's last error when it is null or invalid.
fn owned(handle: HANDLE) -> Result<OwnedHandle, u32> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        // SAFETY: plain call.
        return Err(unsafe { GetLastError() });
    }
    // SAFETY: a live handle this process just received, closed once.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

fn code_of<T>(result: &Result<T, u32>) -> u32 {
    result.as_ref().err().copied().unwrap_or(0)
}

fn create_mutex(name: &str) -> Result<OwnedHandle, u32> {
    let name = wide(name);
    // SAFETY: default security and a NUL-terminated name.
    owned(unsafe { CreateMutexW(null(), 0, name.as_ptr()) })
}

/// A one-page, pagefile-backed section named `name`.
fn create_section(name: &str) -> Result<OwnedHandle, u32> {
    let name = wide(name);
    // SAFETY: no file, default security, and a NUL-terminated name.
    owned(unsafe {
        CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            null(),
            PAGE_READWRITE,
            0,
            4096,
            name.as_ptr(),
        )
    })
}

/// A local byte pipe named `name` with one listening instance.
fn create_pipe(name: &str) -> Result<OwnedHandle, u32> {
    let name = wide(name);
    // SAFETY: default security and a NUL-terminated name.
    owned(unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            0,
            0,
            0,
            null(),
        )
    })
}

/// Opens the existing `kind` object `name` (`mutex`, `section`, or `pipe`)
/// and closes it at once: 0, or the error.
fn open_object(kind: &str, name: &str) -> u32 {
    let name = wide(name);
    let handle = match kind {
        // SAFETY: a NUL-terminated name.
        "mutex" => unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr()) },
        // SAFETY: a NUL-terminated name.
        "section" => unsafe { OpenFileMappingW(FILE_MAP_READ, 0, name.as_ptr()) },
        // SAFETY: a NUL-terminated name, no security attributes or template.
        "pipe" => unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            )
        },
        other => panic!("unknown object kind {other:?}"),
    };
    code_of(&owned(handle))
}

/// The name the named-pipe file system holds pipe `handle` under, or its
/// error as a verdict.
fn pipe_name(handle: &OwnedHandle) -> String {
    // FILE_NAME_INFO: a byte length, then that many bytes of UTF-16.
    let mut buffer = vec![0_u32; 1 + 512];
    let size = u32::try_from(buffer.len() * 4).expect("a small buffer");
    // SAFETY: a live handle and a writable, u32-aligned buffer of `size`
    // bytes.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle.as_raw_handle(),
            FileNameInfo,
            buffer.as_mut_ptr().cast(),
            size,
        )
    };
    if ok == 0 {
        // SAFETY: plain call.
        return verdict(unsafe { GetLastError() });
    }
    let units = (buffer[0] / 2) as usize;
    let name: Vec<u16> = buffer[1..]
        .iter()
        .flat_map(|pair| [*pair as u16, (*pair >> 16) as u16])
        .take(units)
        .collect();
    String::from_utf16_lossy(&name)
}

/// An open registry key, closed on drop.
struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: an open key, closed once.
        unsafe { RegCloseKey(self.0) };
    }
}

/// `action` on `HKEY_CURRENT_USER\<subkey>`: `write` opens it for
/// `KEY_SET_VALUE`, `read` for `KEY_READ`, and `create` creates it. 0, or
/// the error.
fn registry(action: &str, subkey: &str) -> u32 {
    let name = wide(subkey);
    let mut key: HKEY = null_mut();
    let status = match action {
        // SAFETY: a predefined root, a NUL-terminated name, and an out
        // pointer.
        "write" => unsafe {
            RegOpenKeyExW(HKEY_CURRENT_USER, name.as_ptr(), 0, KEY_SET_VALUE, &mut key)
        },
        // SAFETY: as above.
        "read" => unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, name.as_ptr(), 0, KEY_READ, &mut key) },
        // SAFETY: as above, with no class or security attributes.
        "create" => unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                name.as_ptr(),
                0,
                null(),
                REG_OPTION_NON_VOLATILE,
                KEY_READ,
                null(),
                &mut key,
                null_mut(),
            )
        },
        other => panic!("unknown registry action {other:?}"),
    };
    if status == 0 {
        drop(Key(key));
    }
    status
}

/// Creates a subkey in this container's own registry storage
/// (`GetAppContainerRegistryLocation`): a verdict, or the failing HRESULT.
fn own_registry_write() -> String {
    let mut root: HKEY = null_mut();
    // SAFETY: an out pointer.
    let status = unsafe { GetAppContainerRegistryLocation(KEY_ALL_ACCESS, &mut root) };
    if status < 0 {
        return format!("hresult:{status:#010x}");
    }
    let root = Key(root);
    let name = wide("bake-probe");
    let mut key: HKEY = null_mut();
    // SAFETY: an open key, a NUL-terminated name, and an out pointer.
    let status = unsafe {
        RegCreateKeyExW(
            root.0,
            name.as_ptr(),
            0,
            null(),
            REG_OPTION_NON_VOLATILE,
            KEY_READ,
            null(),
            &mut key,
            null_mut(),
        )
    };
    if status == 0 {
        drop(Key(key));
    }
    verdict(status)
}

/// A key under `HKEY_CURRENT_USER\Software` a case creates, deleted with
/// its subtree on drop.
struct TestKey(String);

impl TestKey {
    fn new(name: &str) -> Self {
        let subkey = format!(r"Software\{name}");
        assert_eq!(registry("create", &subkey), 0, r"create HKCU\{subkey}");
        Self(subkey)
    }
}

impl Drop for TestKey {
    fn drop(&mut self) {
        let name = wide(&self.0);
        // SAFETY: a predefined root and a NUL-terminated name.
        let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, name.as_ptr()) };
        if status != 0 {
            eprintln!(r"cannot delete HKCU\{}: error {status}", self.0);
        }
    }
}

fn session_id() -> u32 {
    let mut session = 0;
    // SAFETY: an out pointer.
    let ok = unsafe { ProcessIdToSessionId(std::process::id(), &mut session) };
    assert_ne!(ok, 0, "ProcessIdToSessionId");
    session
}

/// `name` in session `session`'s own namespace, as a process in any
/// namespace of that session names it.
fn in_session(session: u32, name: &str) -> String {
    if session == 0 {
        format!(r"Global\{name}")
    } else {
        format!(r"Session\{session}\{name}")
    }
}

/// The prefix `GetAppContainerNamedObjectPath` gives for profile
/// `profile`'s package SID, under which that container's `Local\` objects
/// live, or the error.
fn named_object_path(profile: &str) -> Result<String, u32> {
    let name = wide(profile);
    let mut sid = null_mut();
    // SAFETY: a NUL-terminated name and an out pointer.
    let status = unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) };
    assert!(
        status >= 0 && !sid.is_null(),
        "DeriveAppContainerSidFromAppContainerName({profile}): HRESULT {status:#010x}"
    );
    let mut buffer = vec![0_u16; 1024];
    let capacity = u32::try_from(buffer.len()).expect("a small buffer");
    let mut length = 0;
    // SAFETY: no token, a live SID, and a writable buffer of `capacity`
    // units.
    let ok = unsafe {
        GetAppContainerNamedObjectPath(null_mut(), sid, capacity, buffer.as_mut_ptr(), &mut length)
    };
    // SAFETY: plain call, before any other.
    let error = unsafe { GetLastError() };
    // SAFETY: documented as freed with `FreeSid`, and freed once.
    unsafe { FreeSid(sid) };
    if ok == 0 {
        return Err(error);
    }
    let end = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    Ok(String::from_utf16_lossy(&buffer[..end]))
}

/// A pipe under `LOCAL\` and a `Local\` mutex, both named `name`, reported
/// after this process's `LOCALAPPDATA` (with the pipe's name in the
/// named-pipe file system), then a pipe outside `LOCAL\`, reported and
/// closed; `held`, then all kept until stdin closes.
fn hold(name: &str) -> std::io::Result<i32> {
    println!(
        "local {}",
        std::env::var("LOCALAPPDATA").unwrap_or_default()
    );
    let pipe = create_pipe(&format!(r"\\.\pipe\LOCAL\{name}"));
    match &pipe {
        Ok(handle) => println!("pipe allowed {}", pipe_name(handle)),
        Err(code) => println!("pipe {}", verdict(*code)),
    }
    let mutex = create_mutex(&format!(r"Local\{name}"));
    println!("mutex {}", verdict(code_of(&mutex)));
    let outside = create_pipe(&format!(r"\\.\pipe\{name}-outside"));
    println!("outside-pipe {}", verdict(code_of(&outside)));
    drop(outside);
    println!("held");
    std::io::stdout().flush()?;
    let _ = std::io::stdin().read_to_end(&mut Vec::new());
    drop((pipe, mutex));
    Ok(0)
}

/// Every null-device open a program may make, by both names, one line
/// each: `<name> <mode> <verdict>`; a write writes one byte.
fn null_device() {
    for name in ["NUL", r"\\.\NUL"] {
        for (mode, read, write) in [("r", true, false), ("w", false, true), ("rw", true, true)] {
            let result = std::fs::OpenOptions::new()
                .read(read)
                .write(write)
                .open(name)
                .and_then(|mut file| if write { file.write_all(b"x") } else { Ok(()) });
            let code = match result {
                Ok(()) => 0,
                Err(error) => error
                    .raw_os_error()
                    .and_then(|code| u32::try_from(code).ok())
                    .unwrap_or(u32::MAX),
            };
            println!("{name} {mode} {}", verdict(code));
        }
    }
}

/// How long the confined connect may wait, inside [`super::super::BOUND`].
const CONNECT_BUDGET: Duration = Duration::from_secs(20);

/// This process's pid, then one connect to `address` within
/// [`CONNECT_BUDGET`], reported as `result connected`, `result timeout`, or
/// `result failed:<code>`; then held, a stream included, until stdin
/// closes.
fn connect_hold(address: &str) -> std::io::Result<i32> {
    let address: SocketAddr = address.parse().expect("socket address");
    println!("pid {}", std::process::id());
    std::io::stdout().flush()?;
    let stream = std::net::TcpStream::connect_timeout(&address, CONNECT_BUDGET);
    match &stream {
        Ok(_) => println!("result connected"),
        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => println!("result timeout"),
        Err(error) => println!("result failed:{}", error.raw_os_error().unwrap_or(-1)),
    }
    std::io::stdout().flush()?;
    let _ = std::io::stdin().read_to_end(&mut Vec::new());
    drop(stream);
    Ok(0)
}

/// The user's named objects (a mutex, a section, a pipe) and registry stay
/// closed to a container, beside an unconfined control that opens the same
/// names; the container's own registry storage is recorded by mode.
fn named_objects_and_the_registry_stay_closed() {
    let fixture = Fixture::new();
    let workspace = user_scratch("ac-ws");
    let name = format!("bake-obj-{}", fixture.tag.as_str());
    let section = format!("{name}-section");
    let pipe = format!(r"\\.\pipe\{name}");
    let _mutex = create_mutex(&format!(r"Local\{name}")).expect("an unconfined mutex");
    let _section = create_section(&format!(r"Local\{section}")).expect("an unconfined section");
    let _pipe = create_pipe(&pipe).expect("an unconfined pipe");
    let session = session_id();
    let items = [
        format!("mutex={}", in_session(session, &name)),
        format!("section={}", in_session(session, &section)),
        format!("pipe={pipe}"),
    ];
    let mut ops = vec!["appcontainer", "objects", "open"];
    ops.extend(items.iter().map(String::as_str));
    // The container first: the pipe's one listening instance is then the
    // control's.
    let run = fixture.run(&ww(&workspace), &ops);
    assert_ok(&run, "the confined opens");
    let control = unconfined(&ops);
    let controls = String::from_utf8_lossy(&control.stdout).into_owned();
    assert_eq!(
        controls.lines().collect::<Vec<_>>(),
        ["allowed"; 3],
        "the unconfined control must open every object: {}",
        String::from_utf8_lossy(&control.stderr)
    );
    let verdicts: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(verdicts.len(), items.len(), "{}", run.stdout);
    for (item, verdict) in items.iter().zip(verdicts) {
        observe(format_args!(
            "case=named-object session={session} {item} verdict={verdict}"
        ));
        // A pipe outside `LOCAL\` resolves in the one global namespace, so
        // only its DACL can refuse it.
        let closed = if item.starts_with("pipe=") {
            verdict == "denied"
        } else {
            matches!(verdict, "denied" | "absent")
        };
        assert!(closed, "a container reached the user's {item}: {verdict}");
    }
    let key = TestKey::new(&format!("bake-test-{}", fixture.tag.as_str()));
    let child = format!(r"{}\child", key.0);
    for (action, subkey) in [("write", key.0.as_str()), ("create", child.as_str())] {
        let run = fixture.run(
            &ww(&workspace),
            &["appcontainer", "objects", "registry", action, subkey],
        );
        assert_ok(&run, action);
        let verdict = run.stdout.trim();
        observe(format_args!("case=registry-{action} verdict={verdict}"));
        assert_eq!(
            verdict, "denied",
            r"a container's {action} of HKCU\{subkey}"
        );
    }
    assert_eq!(
        registry("read", &child),
        2,
        r"a container created HKCU\{child}"
    );
    let read = fixture.run(
        &ww(&workspace),
        &["appcontainer", "objects", "registry", "read", &key.0],
    );
    observe(format_args!(
        "case=registry-read exit={:?} verdict={}",
        read.code,
        read.stdout.trim()
    ));
    // The profile's own registry storage, which the runner does not
    // restrict: under `read-only` a qualification blocker.
    for (mode, policy) in [
        ("read-only", ro(&workspace)),
        ("workspace-write", ww(&workspace)),
    ] {
        let run = fixture.run(&policy, &["appcontainer", "objects", "registry-own"]);
        observe(format_args!(
            "case=profile-registry-write mode={mode} exit={:?} verdict={}",
            run.code,
            run.stdout.trim()
        ));
    }
}

/// What a command creates after its resume (a pipe under `LOCAL\`, a
/// `Local\` mutex) lives in its run's own namespace: another run's
/// container cannot open it by any name this process tries, and each name
/// that reached it during the run finds nothing once the run has ended.
fn objects_made_after_resume_end_with_the_run() {
    let fixture = Fixture::new();
    let workspace = user_scratch("ac-ws");
    let name = format!("bake-held-{}", fixture.tag.as_str());
    let mut held = fixture.start(
        &ww(&workspace),
        &["appcontainer", "objects", "hold", &name],
        true,
    );
    let lines = held.lines_until("held");
    let field = |key: &str| -> String {
        lines
            .iter()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix(' '))
            .unwrap_or_else(|| panic!("no {key} line: {lines:?}"))
            .to_owned()
    };
    let (_, profile) = fixture.run_profile(&field("local"));
    let sid = package_sid(&profile);
    let pipe = field("pipe");
    let (created, reported) = pipe.split_once(' ').unwrap_or((pipe.as_str(), ""));
    assert_eq!(created, "allowed", "a pipe under LOCAL: {lines:?}");
    assert_eq!(field("mutex"), "allowed", "a Local mutex: {lines:?}");
    observe(format_args!(
        "case=held-objects profile={profile} pipe_name={reported:?} outside_pipe={}",
        field("outside-pipe")
    ));
    let session = session_id();
    let mut candidates = Vec::new();
    match named_object_path(&profile) {
        Ok(path) => candidates.push(format!(r"mutex={path}\{name}")),
        Err(code) => observe(format_args!(
            "case=held-objects named_object_path_error={code}"
        )),
    }
    if reported.starts_with('\\') {
        candidates.push(format!(r"pipe=\\.\pipe{reported}"));
    }
    let computed =
        format!(r"pipe=\\.\pipe\Sessions\{session}\AppContainerNamedObjects\{sid}\{name}");
    if !candidates
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(&computed))
    {
        candidates.push(computed);
    }
    // Another run, in a profile of its own, first: neither by these names
    // nor by its own `Local\` and `LOCAL\` names.
    let own = [
        format!(r"mutex=Local\{name}"),
        format!(r"pipe=\\.\pipe\LOCAL\{name}"),
    ];
    let tried: Vec<&String> = candidates.iter().chain(&own).collect();
    let mut ops = vec!["appcontainer", "objects", "open"];
    ops.extend(tried.iter().map(|item| item.as_str()));
    let other = fixture.run(&ww(&workspace), &ops);
    assert_ok(&other, "another run's opens");
    let verdicts: Vec<&str> = other.stdout.lines().collect();
    assert_eq!(verdicts.len(), tried.len(), "{}", other.stdout);
    for (item, verdict) in tried.iter().zip(verdicts) {
        observe(format_args!(
            "case=held-object-other-run {item} verdict={verdict}"
        ));
        assert!(
            matches!(verdict, "denied" | "absent"),
            "another run's container reached {item}: {verdict}"
        );
    }
    // This unconfined process, while the run holds them, then after its
    // end. An open that is denied still shows the object exists.
    let open = |item: &str| {
        let (kind, path) = item.split_once('=').expect("kind=name");
        (kind.to_owned(), open_object(kind, path))
    };
    let during: Vec<(&String, String, u32)> = candidates
        .iter()
        .map(|item| {
            let (kind, code) = open(item);
            (item, kind, code)
        })
        .collect();
    held.close_stdin();
    let ended = held.settle();
    assert_eq!(ended.code, Some(0), "the holding run: {}", ended.stderr);
    let mut proven = std::collections::BTreeSet::new();
    for (item, kind, code) in during {
        let (_, after) = open(item);
        observe(format_args!(
            "case=held-object {item} during={} after={}",
            verdict(code),
            verdict(after)
        ));
        if matches!(code, 0 | 5) {
            assert!(
                matches!(after, 2 | 3),
                "{item} outlived its run: {}",
                verdict(after)
            );
            proven.insert(kind);
        }
    }
    for kind in ["mutex", "pipe"] {
        if !proven.contains(kind) {
            eprintln!(
                "appcontainer-unproven: case=objects-after-resume kind={kind} no name this process tried reached the run's {kind}"
            );
        }
    }
}

/// Git's blob id of empty content.
const EMPTY_BLOB: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";

/// Every null-device open a container makes, by both names, and whether
/// `git` starts in one. Git for Windows opens `/dev/null` (`NUL`) for
/// reading and writing at every start, and CI saw that denied ("could not
/// open '/dev/null' for reading and writing"). Git must follow the null
/// device's verdict; it stays a qualification blocker until it starts.
fn the_null_device_decides_git() {
    let fixture = Fixture::new();
    let workspace = user_scratch("ac-ws");
    let ops = ["appcontainer", "objects", "nul"];
    let control = unconfined(&ops);
    let controls = String::from_utf8_lossy(&control.stdout).into_owned();
    assert!(
        control.status.success()
            && controls.lines().count() == 6
            && controls.lines().all(|line| line.ends_with(" allowed")),
        "the unconfined null-device control: {controls}"
    );
    let run = fixture.run(&ww(&workspace), &ops);
    assert_ok(&run, "the confined null-device probe");
    for line in run.stdout.lines() {
        observe(format_args!("case=null-device {line}"));
    }
    assert_eq!(run.stdout.lines().count(), 6, "{}", run.stdout);
    // No open made a file of that name in the command's directory.
    let made: Vec<_> = std::fs::read_dir(&workspace.0)
        .expect("list the workspace")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .filter(|name| {
            name.to_str().is_some_and(|name| {
                let name = name.to_ascii_lowercase();
                name == "nul" || name.starts_with("nul.")
            })
        })
        .collect();
    assert!(made.is_empty(), "a null-device open made {made:?}");
    let read_write = run.stdout.lines().any(|line| line == "NUL rw allowed");
    let git = std::process::Command::new("git")
        .args(["hash-object", "/dev/null"])
        .current_dir(&workspace.0)
        .output();
    match git {
        Ok(output) if output.status.success() => {
            assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), EMPTY_BLOB);
        }
        other => {
            eprintln!("appcontainer-unproven: case=git-dev-null no unconfined git: {other:?}");
            return;
        }
    }
    let provider = fixture.provider(&[], &[]);
    let argv =
        |args: &[&str]| -> Vec<String> { args.iter().map(|arg| (*arg).to_owned()).collect() };
    let version = fixture.run_argv(&provider, &argv(&["git", "--version"]), &ww(&workspace));
    let hashed = fixture.run_argv(
        &provider,
        &argv(&["git", "hash-object", "/dev/null"]),
        &ww(&workspace),
    );
    for (what, run) in [("version", &version), ("hash-object", &hashed)] {
        observe(format_args!(
            "case=git-dev-null command={what} nul_rw={read_write} exit={:?} stdout={:?} stderr={:?}",
            run.code,
            run.stdout.trim(),
            run.stderr.trim()
        ));
    }
    if read_write {
        assert_ok(&version, "git where the null device opens");
        if hashed.code == Some(0) {
            assert_eq!(hashed.stdout.trim(), EMPTY_BLOB);
        }
    } else {
        assert!(
            version.code != Some(0) && version.stderr.contains("/dev/null"),
            "git without the null device must fail on it: exit {:?} stderr {:?}",
            version.code,
            version.stderr
        );
    }
}

/// One `GetExtendedTcpTable` IPv4 row: state, ports in host order, pid.
struct TcpRow {
    state: u32,
    local_port: u16,
    remote_port: u16,
    pid: u32,
}

/// Every IPv4 TCP row on the host with its owning pid.
fn tcp_rows() -> Vec<TcpRow> {
    const AF_INET: u32 = 2;
    let mut size = 0_u32;
    for _ in 0..8 {
        let mut buffer = vec![0_u32; (size as usize).div_ceil(4).max(1)];
        let mut capacity = u32::try_from(buffer.len() * 4).expect("a TCP table size");
        // SAFETY: a writable, u32-aligned buffer of `capacity` bytes.
        let status = unsafe {
            GetExtendedTcpTable(
                buffer.as_mut_ptr().cast(),
                &mut capacity,
                0,
                AF_INET,
                TCP_TABLE_OWNER_PID_ALL,
                0,
            )
        };
        if status == ERROR_INSUFFICIENT_BUFFER {
            size = capacity;
            continue;
        }
        assert_eq!(status, 0, "GetExtendedTcpTable");
        // MIB_TCPTABLE_OWNER_PID: a count, then rows of six u32s (state,
        // local address and port, remote address and port, pid), ports in
        // network order in the low 16 bits.
        let count = buffer[0] as usize;
        return buffer[1..]
            .as_chunks::<6>()
            .0
            .iter()
            .take(count)
            .map(|row| TcpRow {
                state: row[0],
                local_port: u16::from_be(row[2] as u16),
                remote_port: u16::from_be(row[4] as u16),
                pid: row[5],
            })
            .collect();
    }
    panic!("the TCP table kept growing");
}

/// How long the TCP table is polled while the confined connect waits.
const POLL_WINDOW: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The connect failures that are a refusal: `WSAEACCES`, `WSAENETUNREACH`,
/// `WSAECONNREFUSED`, `WSAEHOSTUNREACH`.
const REFUSALS: [&str; 4] = ["10013", "10051", "10061", "10065"];

/// One confined connect under `network` to a fresh loopback listener,
/// decided from the TCP table and the listener, never from a timeout alone.
/// Returns `refused:<code>` when the connect failed with a refusal, or
/// `dropped` when it timed out while the probe's SYN stayed unanswered:
/// `SYN_SENT` under its pid in two polls or more, no row on the listener's
/// side, and nothing to accept. Panics on any connection and on a bare
/// timeout.
pub(super) fn loopback_outcome(
    fixture: &Fixture,
    workspace: &Scratch,
    network: &[NetworkGrant],
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    listener
        .set_nonblocking(true)
        .expect("a nonblocking listener");
    let address = listener.local_addr().expect("listener address");
    let port = address.port();
    let argv = fixture.argv(&[
        "appcontainer",
        "objects",
        "connect-hold",
        &address.to_string(),
    ]);
    let (_, mut started) =
        fixture.start_with(&fixture.provider(&[], network), &argv, &ww(workspace), true);
    let line = started.line();
    let pid: u32 = line
        .strip_prefix("pid ")
        .and_then(|pid| pid.parse().ok())
        .unwrap_or_else(|| panic!("handshake {line:?}"));
    let (mut syn_sent, mut server_side, mut established, mut accepted) = (0, false, false, false);
    let deadline = Instant::now() + POLL_WINDOW;
    while Instant::now() < deadline {
        let rows = tcp_rows();
        let client = || {
            rows.iter()
                .filter(|row| row.pid == pid && row.remote_port == port)
        };
        established |= client().any(|row| row.state == MIB_TCP_STATE_ESTAB as u32);
        if client().any(|row| row.state == MIB_TCP_STATE_SYN_SENT as u32) {
            syn_sent += 1;
        }
        server_side |= rows
            .iter()
            .any(|row| row.local_port == port && row.state != MIB_TCP_STATE_LISTEN as u32);
        accepted |= listener.accept().is_ok();
        std::thread::sleep(POLL_INTERVAL);
    }
    let line = started.line();
    let result = line
        .strip_prefix("result ")
        .unwrap_or_else(|| panic!("result {line:?}"))
        .to_owned();
    accepted |= listener.accept().is_ok();
    started.close_stdin();
    let ended = started.settle();
    assert_eq!(ended.code, Some(0), "the connect probe: {}", ended.stderr);
    observe(format_args!(
        "case=loopback-table network={network:?} result={result} syn_sent_polls={syn_sent} server_side={server_side} established={established} accepted={accepted}"
    ));
    assert!(
        result != "connected" && !established && !accepted && !server_side,
        "loopback reached the listener with {network:?}: result={result} established={established} accepted={accepted} server_side={server_side}"
    );
    if let Some(code) = result.strip_prefix("failed:") {
        assert!(
            REFUSALS.contains(&code),
            "a connect failure that is not a refusal: {code}"
        );
        return format!("refused:{code}");
    }
    assert_eq!(result, "timeout", "the connect probe's result");
    // D41: a timeout is inconclusive even with the probe's SYN unanswered in
    // the TCP table, since a slow host, a filter driver, or a firewall rule
    // drops the same way. It is a named gap, not a denial and not a
    // failure: loopback denial stays unproven.
    observe(format_args!(
        "gap=loopback-denial-unproven network={network:?} syn_sent_polls={syn_sent}: the connect timed out instead of being refused"
    ));
    "inconclusive".to_owned()
}
