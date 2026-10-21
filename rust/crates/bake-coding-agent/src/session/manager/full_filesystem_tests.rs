//! A filesystem that fills up under the session writer, Linux only. The
//! parent starts a child process re-running this test binary in a user
//! and mount namespace of its own (`unshare(CLONE_NEWUSER | CLONE_NEWNS)`
//! with uid and gid maps, between `fork` and `exec`), where a tmpfs a few
//! pages large is mounted over a directory. The child fills it with a
//! filler file and a growing session until the kernel refuses a write
//! with `ENOSPC`, part way through a line, and checks what follows: later
//! writes are refused, the file loads to its whole lines and rebuilds a
//! provider-valid request, and once space returns the next writer takes
//! it over. It copies the file out for the parent to check again.
//!
//! The mount lives in the child's namespace and vanishes with it. Where
//! unprivileged user namespaces are unavailable the test skips with a
//! note, unless `BAKE_REQUIRE_KERNEL_FAULTS=1`, which CI sets.

use std::ffi::CString;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::*;
use crate::session::manager::crash_support::{
    CHILD_WATCHDOG_EXIT, PATIENCE, TestChild, context_of, entries_of, lines, message,
    provider_problem, read, request, take_over, usage,
};
use crate::session::manager::kernel_faults::skip_unavailable;
use crate::session::{FailedLine, GroupEntry};
use crate::test_support::TempDir;

const CHILD_ENV: &str = "BAKE_TEST_FULL_FILESYSTEM";
const CHILD_TEST: &str = "session::manager::full_filesystem_tests::full_filesystem_child";
const CHECKED: &str = "bake-test-full-filesystem-checked";

/// The tmpfs size, in bytes: eight pages of 4 KiB.
const TMPFS_SIZE: usize = 32 * 1024;
/// The filler file the child removes to give space back.
const FILLER: usize = 8 * 1024;

/// `error` is the kernel's `ENOSPC`.
fn assert_enospc(error: &SessionError, case: &str) {
    let source = match error {
        SessionError::Io { source, .. } => source,
        SessionError::GroupWrite(error) => &error.source,
        other => panic!("{case}: not a write error: {other:?}"),
    };
    assert_eq!(source.raw_os_error(), Some(libc::ENOSPC), "{case}: {error}");
}

/// A user message, then an assistant answer, each about 1 KiB.
fn turn(manager: &mut SessionManager, index: usize) -> Result<(), SessionError> {
    let text = format!("{index:04} {}", "x".repeat(1000));
    manager.append_message(message(
        json!({"role": "user", "content": text, "timestamp": 1}),
    ))?;
    manager.append_message(message(json!({"role": "assistant",
        "content": [{"type": "text", "text": text}], "api": "faux", "provider": "faux",
        "model": "faux-1", "usage": usage(), "stopReason": "stop", "timestamp": 2})))?;
    Ok(())
}

/// Not a test of its own: the child of the test below, which re-runs this
/// test binary with [`CHILD_ENV`] set to `mount|copy`: the directory a
/// tmpfs of [`TMPFS_SIZE`] bytes is mounted on, and where to copy the
/// session file.
#[test]
fn full_filesystem_child() {
    let Some(spec) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    let spec = spec.to_string_lossy().into_owned();
    let Some((mount, copy)) = spec.split_once('|') else {
        panic!("bad spec {spec}");
    };
    let (mount, copy) = (Path::new(mount), Path::new(copy));
    std::thread::spawn(|| {
        std::thread::sleep(PATIENCE * 2);
        std::process::exit(CHILD_WATCHDOG_EXIT);
    });
    let filler = mount.join("filler");
    std::fs::write(&filler, vec![0u8; FILLER]).expect("filler");
    let mut manager = SessionManager::create(
        &mount.to_string_lossy(),
        Some(&mount.join("sessions")),
        NewSessionOptions::default(),
    )
    .expect("session");
    // Grow the session until the kernel refuses a write.
    let mut index = 0;
    let error = loop {
        if let Err(error) = turn(&mut manager, index) {
            break error;
        }
        index += 1;
        assert!(index < 100, "the tmpfs never filled");
    };
    assert!(index > 0, "not one turn fit");
    assert_enospc(&error, "the write that filled the tmpfs");
    let file = manager.session_file().expect("file").to_path_buf();
    let full = read(&file);
    let torn = full.last() != Some(&b'\n');
    println!(
        "filled at {} bytes after {index} turns, {}",
        full.len(),
        if torn { "mid-line" } else { "at a line's end" }
    );

    // Later writes are refused: a single append keeps its entry in memory
    // as in Pi, a group keeps nothing, and neither changes the file.
    let before = manager.entries().len();
    let error = manager
        .append_session_info("later")
        .expect_err("a later append");
    assert_enospc(&error, "a later append");
    assert_eq!(manager.entries().len(), before + 1);
    let leaf = manager.leaf_id().expect("leaf").to_owned();
    let result = manager.append_group(vec![GroupEntry::model_change("faux", "faux-2")]);
    let Err(error) = &result else {
        panic!("a later group was written");
    };
    assert_enospc(error, "a later group");
    let SessionError::GroupWrite(group) = error else {
        panic!("not a group write error: {error:?}");
    };
    assert_eq!(
        (group.written.len(), group.failed),
        (0, FailedLine::NotWritten)
    );
    assert_eq!(manager.leaf_id(), Some(leaf.as_str()));
    assert!(read(&file) == full, "a refused write changed the file");
    drop(manager);

    // The file loads to its whole lines, and its request is valid.
    let loaded = entries_of(&file).len();
    let whole = lines(&full)
        .iter()
        .filter(|line| line.ends_with(b"\n"))
        .count();
    assert_eq!(loaded, whole - 1, "every whole line but the header");
    assert_eq!(provider_problem(&request(&context_of(&file))), None);
    std::fs::write(copy, &full).expect("copy out");

    // A torn tail needs a newline the full tmpfs refuses, so the next
    // writer cannot open the file until space returns.
    match SessionManager::open(&file, None, None) {
        Err(error) if torn => assert_enospc(&error, "the next writer on a full tmpfs"),
        Err(error) => panic!("the next writer failed: {error}"),
        Ok(mut taken) => {
            assert!(!torn, "a torn file opened on a full tmpfs");
            let error = taken.append_session_info("full").expect_err("full");
            assert_enospc(&error, "an append by the next writer");
        }
    }
    std::fs::remove_file(&filler).expect("give space back");
    let mut taken = take_over(&file);
    assert_eq!(taken.entries().len(), loaded);
    taken.append_session_info("after").expect("append after");
    drop(taken);
    let after = read(&file);
    assert!(after.starts_with(&full));
    assert_eq!(entries_of(&file).len(), loaded + 1);
    assert_eq!(provider_problem(&request(&context_of(&file))), None);
    println!("{CHECKED}");
    let _ = std::io::stdout().flush();
    std::process::exit(0);
}

/// Write `bytes` to the `/proc` file at `path` from a forked child: raw
/// calls only, which allocate nothing.
fn write_proc(path: &std::ffi::CStr, bytes: &[u8]) -> std::io::Result<()> {
    // SAFETY: `path` is a NUL-terminated string and `bytes` a live buffer,
    // both borrowed for the calls; the descriptor is closed before return.
    unsafe {
        let fd = libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let written = libc::write(fd, bytes.as_ptr().cast(), bytes.len());
        let error = std::io::Error::last_os_error();
        libc::close(fd);
        if usize::try_from(written).ok() != Some(bytes.len()) {
            return Err(error);
        }
    }
    Ok(())
}

/// Make `command` start in a user and mount namespace of its own, mapped
/// to the caller's uid and gid, with a tmpfs of [`TMPFS_SIZE`] bytes on
/// `mount`.
fn in_namespace_with_tmpfs(command: &mut std::process::Command, mount: &Path) {
    // SAFETY: `getuid` and `getgid` take no arguments and cannot fail.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let uid_map = format!("0 {uid} 1").into_bytes();
    let gid_map = format!("0 {gid} 1").into_bytes();
    let target = CString::new(mount.as_os_str().as_encoded_bytes()).expect("mount path");
    let options = CString::new(format!("size={TMPFS_SIZE},mode=0700")).expect("options");
    // SAFETY: the hook runs in the forked child, which has one thread, and
    // makes only system calls on buffers built before the fork.
    unsafe {
        command.pre_exec(move || {
            if libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            write_proc(c"/proc/self/setgroups", b"deny")?;
            write_proc(c"/proc/self/uid_map", &uid_map)?;
            write_proc(c"/proc/self/gid_map", &gid_map)?;
            let none = std::ptr::null();
            if libc::mount(
                c"none".as_ptr(),
                c"/".as_ptr(),
                none,
                libc::MS_REC | libc::MS_PRIVATE,
                none.cast(),
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            if libc::mount(
                c"tmpfs".as_ptr(),
                target.as_ptr(),
                c"tmpfs".as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV,
                options.as_ptr().cast(),
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// A session grown on a size-capped tmpfs until the kernel's `ENOSPC`
/// stops a write mid-line leaves a file that loads to its whole lines,
/// refuses later writes while the tmpfs is full, and is taken over once
/// space returns; the copy the child leaves loads here too, and its next
/// writer appends on a line of its own.
#[test]
fn a_session_on_a_full_tmpfs_keeps_a_loadable_prefix_and_refuses_later_writes() {
    let dir = TempDir::new("full-tmpfs");
    let mount = dir.join("mnt");
    std::fs::create_dir_all(&mount).expect("mount point");
    let copy: PathBuf = dir.join("copy.jsonl");
    let spec = format!("{}|{}", mount.display(), copy.display());
    let spawned = TestChild::try_spawn(CHILD_TEST, CHILD_ENV, &spec, |command| {
        in_namespace_with_tmpfs(command, &mount);
    });
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            skip_unavailable(
                "an unprivileged user and mount namespace with a tmpfs",
                &error.to_string(),
            );
            return;
        }
    };
    let (status, output) = child.wait();
    assert!(status.success(), "{status}: {output:?}");
    for line in output.iter().filter(|line| line.contains("filled at")) {
        eprintln!("child: {line}");
    }
    assert!(
        output.iter().any(|line| line.contains(CHECKED)),
        "{output:?}"
    );
    // Nothing was written outside the namespace's tmpfs but the copy.
    assert!(
        std::fs::read_dir(&mount)
            .expect("mount point")
            .next()
            .is_none()
    );

    let full = read(&copy);
    let loaded = entries_of(&copy).len();
    assert!(loaded >= 2, "{loaded} entries");
    assert_eq!(provider_problem(&request(&context_of(&copy))), None);
    let mut taken = take_over(&copy);
    assert_eq!(taken.entries().len(), loaded);
    taken.append_session_info("after").expect("append after");
    drop(taken);
    let after = read(&copy);
    assert!(after.starts_with(&full));
    let appended = lines(&after[full.len()..]);
    let info = match appended.as_slice() {
        [newline, info] if *newline == b"\n" => info,
        [info] => info,
        other => panic!("{} lines appended", other.len()),
    };
    let info: Value = serde_json::from_slice(info).expect("the next writer's line");
    assert_eq!(info["type"], "session_info");
}
