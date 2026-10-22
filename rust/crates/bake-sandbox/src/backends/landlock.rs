//! The Landlock helper: apply a Landlock ruleset to this process, then exec
//! the wrapped command, which inherits it with every descendant.
//!
//! A port of the `landlock-run` launcher,
//! `native/system/packages/entry/src/main.c` (BSD-3-Clause, see the crate
//! `NOTICE`), with its CLI contract from `native/system/docs/cli-contract.md`
//! and the constants of `native/system/packages/entry/src/index.ts`. Bake's
//! Rust binary serves as its own helper: `bake` dispatches a first
//! argument of [`HELPER_ARG`] to [`helper_main`] before anything else runs, so
//! the release ships one executable and the helper is always the build that
//! wrapped the command.
//!
//! ```text
//! <exe> __bake-landlock-run [--ro <path>]... [--rw <path>]... -- <argv>...
//! <exe> __bake-landlock-run --probe
//! ```
//!
//! `--ro` grants read and execute beneath a path, `--rw` every access the
//! kernel's ABI governs. Everything else is denied. Every helper failure
//! prints one `landlock-run: ` line on stderr and exits
//! [`LAUNCHER_FAILURE_EXIT`] without running the command; the wire texts are
//! the launcher's, so the TypeScript runner-failure rule applies unchanged.
//! A kernel below ABI 5 governs a subset of the accesses: the helper reports
//! [`PARTIAL_ENFORCEMENT_LINE`] and runs the command confined for the rest.
//!
//! Two hardenings go beyond the C launcher:
//!
//! - Every `--ro` and `--rw` path is opened without following any symbolic
//!   link, in any component: `openat2` with `RESOLVE_NO_SYMLINKS` (Linux
//!   5.6 and later), else `O_PATH | O_NOFOLLOW` one component at a time. A
//!   link fails the run with a `cannot open rule path` line, so a workspace
//!   replaced by a link never grants the link's target.
//! - Before the ruleset, the helper leaves Bake's terminal session: `setsid`,
//!   or `TIOCNOTTY` on its controlling terminal when it already leads a
//!   process group and so cannot start a session. The command then has no
//!   controlling terminal to inject keystrokes into with `TIOCSTI`. If it
//!   cannot leave, it fails without running the command.

use std::ffi::OsString;

/// The hidden first argument that turns a Bake executable into the helper.
pub const HELPER_ARG: &str = "__bake-landlock-run";

/// The launcher's name in its diagnostics (`LAUNCHER_BIN`).
pub const LAUNCHER_BIN: &str = "landlock-run";

/// The exit status of every helper failure (`LAUNCHER_FAILURE_EXIT`). After a
/// successful exec the command's own status passes through, 125 included, so
/// a runner failure needs this status and a `landlock-run: ` line.
pub const LAUNCHER_FAILURE_EXIT: i32 = 125;

/// The stderr line a confined run prints under an older Landlock ABI.
pub const PARTIAL_ENFORCEMENT_LINE: &str = "landlock-run: partial enforcement (older Landlock ABI)";

/// The probe's stdout line when the kernel governs every access.
pub const PROBE_FULL_LINE: &str = "landlock: fully enforced";

/// The probe's stdout line under an older ABI.
pub const PROBE_PARTIAL_LINE: &str = "landlock: partially enforced (older ABI)";

const NOT_ENFORCED_MESSAGE: &str =
    "landlock is not enforced by this kernel (ABI unsupported or disabled)";

/// Runs the helper over `args`, the arguments after [`HELPER_ARG`], and never
/// returns: it execs the command, or exits with the probe's status or
/// [`LAUNCHER_FAILURE_EXIT`]. Call it before the process starts any thread:
/// Landlock confines the calling thread, and the exec carries that thread's
/// domain into the command.
pub fn helper_main(args: impl IntoIterator<Item = OsString>) -> ! {
    let code = run(args.into_iter().collect());
    use std::io::Write;
    let _ = std::io::stdout().flush();
    std::process::exit(code)
}

/// Parsed helper arguments (`struct cli`).
#[derive(Debug, Default, PartialEq, Eq)]
struct Cli {
    probe: bool,
    ro: Vec<OsString>,
    rw: Vec<OsString>,
    command: Vec<OsString>,
}

fn fail(prefix: &str, detail: Option<&str>) -> i32 {
    match detail {
        None => eprintln!("{LAUNCHER_BIN}: {prefix}"),
        Some(detail) => eprintln!("{LAUNCHER_BIN}: {prefix}: {detail}"),
    }
    LAUNCHER_FAILURE_EXIT
}

fn fail_usage(message: &str, detail: &str) -> i32 {
    eprintln!("{LAUNCHER_BIN}: usage error: {message}{detail}");
    LAUNCHER_FAILURE_EXIT
}

/// `parse`: the grants and the command, or the exit code of a usage error
/// whose line is already printed.
fn parse(args: Vec<OsString>) -> Result<Cli, i32> {
    let total = args.len();
    let mut cli = Cli::default();
    let mut rest = args.into_iter();
    while let Some(arg) = rest.next() {
        match arg.to_str() {
            Some("--probe") => {
                if total != 1 {
                    return Err(fail_usage("--probe takes no other arguments", ""));
                }
                cli.probe = true;
            }
            Some(flag @ ("--ro" | "--rw")) => {
                let Some(path) = rest.next() else {
                    return Err(fail_usage(flag, " requires a path"));
                };
                if flag == "--ro" {
                    cli.ro.push(path);
                } else {
                    cli.rw.push(path);
                }
            }
            Some("--") => {
                cli.command = rest.collect();
                break;
            }
            _ => {
                return Err(fail_usage("unknown argument: ", &arg.to_string_lossy()));
            }
        }
    }
    if !cli.probe && cli.command.is_empty() {
        return Err(fail_usage("missing `-- <argv>...` command", ""));
    }
    Ok(cli)
}

/// The helper body; returns the exit status unless the exec succeeds.
fn run(args: Vec<OsString>) -> i32 {
    let cli = match parse(args) {
        Ok(cli) => cli,
        Err(code) => return code,
    };
    if cli.probe {
        // The functional probe: enforce a maximal ruleset in this
        // short-lived process; actually restricting is the only honest
        // signal that the kernel enforces.
        let probe = Cli {
            ro: vec!["/".into()],
            ..Cli::default()
        };
        return match sys::restrict_self(&probe) {
            Ok(partial) => {
                println!(
                    "{}",
                    if partial {
                        PROBE_PARTIAL_LINE
                    } else {
                        PROBE_FULL_LINE
                    }
                );
                0
            }
            Err(code) => code,
        };
    }
    if let Err(code) = sys::new_session() {
        return code;
    }
    match sys::restrict_self(&cli) {
        Ok(true) => eprintln!("{PARTIAL_ENFORCEMENT_LINE}"),
        Ok(false) => {}
        Err(code) => return code,
    }
    sys::exec(&cli.command)
}

/// The error text of an OS error code, as C's `strerror` words it: the
/// standard library's text without its ` (os error N)` suffix.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn strerror(error: &std::io::Error) -> String {
    let text = error.to_string();
    match text.rfind(" (os error ") {
        Some(index) => text[..index].to_owned(),
        None => text,
    }
}

#[cfg(target_os = "linux")]
mod sys {
    //! The raw Landlock UAPI, defined locally as `main.c` does: the kernel's
    //! user-space ABI is stable by contract, and the definitions are the
    //! audit record of exactly what this helper asks of the kernel.
    use std::ffi::{CString, OsString};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;

    use super::{Cli, NOT_ENFORCED_MESSAGE, fail, strerror};

    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
    }

    /// `struct open_how` from `linux/openat2.h`.
    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }

    /// `RESOLVE_NO_SYMLINKS`: fail with `ELOOP` on a link in any component.
    const RESOLVE_NO_SYMLINKS: u64 = 0x04;

    #[repr(C, packed)]
    struct PathBeneathAttr {
        allowed_access: u64,
        parent_fd: i32,
    }

    const LANDLOCK_CREATE_RULESET_VERSION: libc::c_uint = 1 << 0;
    const LANDLOCK_RULE_PATH_BENEATH: libc::c_uint = 1;

    const FS_EXECUTE: u64 = 1 << 0; // ABI 1
    const FS_WRITE_FILE: u64 = 1 << 1;
    const FS_READ_FILE: u64 = 1 << 2;
    const FS_READ_DIR: u64 = 1 << 3;
    const FS_REFER: u64 = 1 << 13; // ABI 2
    const FS_TRUNCATE: u64 = 1 << 14; // ABI 3 (ABI 4 added network bits only)
    const FS_IOCTL_DEV: u64 = 1 << 15; // ABI 5
    /// Bits 0 to 12: every ABI 1 access.
    const ABI1_MASK: u64 = FS_REFER - 1;

    /// The newest ABI whose filesystem accesses this helper governs.
    const MAX_ABI: libc::c_long = 5;

    /// The filesystem accesses `abi` can govern (`fs_mask_for_abi`).
    pub(super) fn fs_mask_for_abi(abi: libc::c_long) -> u64 {
        let mut mask = ABI1_MASK;
        if abi >= 2 {
            mask |= FS_REFER;
        }
        if abi >= 3 {
            mask |= FS_TRUNCATE;
        }
        if abi >= 5 {
            mask |= FS_IOCTL_DEV;
        }
        mask
    }

    fn last_error() -> String {
        strerror(&std::io::Error::last_os_error())
    }

    fn owned(fd: libc::c_long) -> std::io::Result<OwnedFd> {
        match i32::try_from(fd) {
            // SAFETY: the kernel returned a new descriptor this code owns.
            Ok(fd) if fd >= 0 => Ok(unsafe { OwnedFd::from_raw_fd(fd) }),
            _ => Err(std::io::Error::last_os_error()),
        }
    }

    /// Opens `path` as an `O_PATH` descriptor without following a symbolic
    /// link in any component: `openat2(RESOLVE_NO_SYMLINKS)`, or
    /// [`open_by_components`] where the kernel (before 5.6) or a seccomp
    /// filter refuses `openat2`. A link fails with `ELOOP`.
    pub(super) fn open_no_follow(path: &[u8]) -> std::io::Result<OwnedFd> {
        let c_path =
            CString::new(path).map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
        let how = OpenHow {
            flags: (libc::O_PATH | libc::O_CLOEXEC) as u64,
            mode: 0,
            resolve: RESOLVE_NO_SYMLINKS,
        };
        // SAFETY: `c_path` is NUL-terminated and `how` matches the kernel's
        // `open_how`; both outlive the call.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                libc::AT_FDCWD,
                c_path.as_ptr(),
                &how as *const OpenHow,
                std::mem::size_of::<OpenHow>(),
            )
        };
        match owned(fd) {
            Ok(fd) => Ok(fd),
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(libc::ENOSYS | libc::EPERM | libc::E2BIG)
                ) =>
            {
                open_by_components(path)
            }
            Err(error) => Err(error),
        }
    }

    /// The `openat2` fallback: walks `path` one component at a time with
    /// `O_PATH | O_NOFOLLOW`, refusing any component that is a link.
    pub(super) fn open_by_components(path: &[u8]) -> std::io::Result<OwnedFd> {
        if path.contains(&0) {
            return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
        }
        let start: &std::ffi::CStr = if path.first() == Some(&b'/') {
            c"/"
        } else {
            c"."
        };
        // SAFETY: `start` is a valid NUL-terminated string.
        let mut current = owned(libc::c_long::from(unsafe {
            libc::open(
                start.as_ptr(),
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        }))?;
        for component in path.split(|byte| *byte == b'/') {
            if component.is_empty() || component == b"." {
                continue;
            }
            let name = CString::new(component)
                .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
            // SAFETY: `current` is open and `name` is NUL-terminated.
            let next = owned(libc::c_long::from(unsafe {
                libc::openat(
                    current.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            }))?;
            // With `O_PATH`, `O_NOFOLLOW` opens a final link itself rather
            // than failing, so the type decides.
            // SAFETY: an all-zero `stat` is a valid value for `fstat` to fill.
            let mut stat: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: `next` is open and `stat` is writable.
            if unsafe { libc::fstat(next.as_raw_fd(), &mut stat) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if (stat.st_mode & libc::S_IFMT) == libc::S_IFLNK {
                return Err(std::io::Error::from_raw_os_error(libc::ELOOP));
            }
            current = next;
        }
        Ok(current)
    }

    /// Leaves Bake's terminal session before the command runs, so it has no
    /// controlling terminal to inject input into with `TIOCSTI`.
    pub(super) fn new_session() -> Result<(), i32> {
        // SAFETY: no arguments; fails only with EPERM for a group leader.
        if unsafe { libc::setsid() } >= 0 {
            return Ok(());
        }
        // SAFETY: plain queries of this process.
        if unsafe { libc::getsid(0) == libc::getpid() } {
            // Already a session leader: whatever terminal it controls was
            // given to its own session, not shared with Bake's.
            return Ok(());
        }
        // A process-group leader cannot start a session; give up the
        // controlling terminal instead.
        // SAFETY: a valid NUL-terminated path.
        let fd = unsafe {
            libc::open(
                c"/dev/tty".as_ptr(),
                libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
            )
        };
        let tty = match owned(libc::c_long::from(fd)) {
            Ok(tty) => tty,
            Err(error) if error.raw_os_error() == Some(libc::ENXIO) => return Ok(()),
            Err(error) => {
                if controlling_tty_number() == Some(0) {
                    return Ok(());
                }
                return Err(fail("cannot start a new session", Some(&strerror(&error))));
            }
        };
        // SAFETY: `tty` is open; TIOCNOTTY takes no argument.
        if unsafe { libc::ioctl(tty.as_raw_fd(), libc::TIOCNOTTY) } != 0 {
            return Err(fail("cannot start a new session", Some(&last_error())));
        }
        Ok(())
    }

    /// `tty_nr`, field 7 of `/proc/self/stat`: 0 without a controlling
    /// terminal.
    fn controlling_tty_number() -> Option<i64> {
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        let (_, rest) = stat.rsplit_once(") ")?;
        rest.split_ascii_whitespace().nth(4)?.parse().ok()
    }

    /// Adds one path-beneath rule (`add_rule`). An unopenable root, or one
    /// reached through a link, fails closed rather than silently changing
    /// the grant.
    fn add_rule(ruleset: &OwnedFd, path: &OsString, access: u64) -> Result<(), i32> {
        let fd = open_no_follow(path.as_bytes());
        let fd = match fd {
            Ok(fd) => fd,
            Err(error) => {
                eprintln!(
                    "{}: cannot open rule path: {}: {}",
                    super::LAUNCHER_BIN,
                    path.to_string_lossy(),
                    strerror(&error)
                );
                return Err(super::LAUNCHER_FAILURE_EXIT);
            }
        };
        // The kernel rejects directory-only accesses on a file rule, so a
        // file grant keeps its file-compatible bits: how `--rw /dev/null`
        // works.
        let mut access = access;
        // SAFETY: an all-zero `stat` is a valid value for `fstat` to fill.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is open and `stat` is writable.
        let stated = unsafe { libc::fstat(fd.as_raw_fd(), &mut stat) } == 0;
        if stated && (stat.st_mode & libc::S_IFMT) != libc::S_IFDIR {
            access &= FS_EXECUTE | FS_WRITE_FILE | FS_READ_FILE | FS_TRUNCATE | FS_IOCTL_DEV;
        }
        let attr = PathBeneathAttr {
            allowed_access: access,
            parent_fd: fd.as_raw_fd(),
        };
        // SAFETY: the attribute struct matches the kernel layout and outlives
        // the call; both descriptors are open.
        let added = unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                ruleset.as_raw_fd(),
                LANDLOCK_RULE_PATH_BENEATH,
                &attr as *const PathBeneathAttr,
                0 as libc::c_uint,
            )
        };
        if added != 0 {
            return Err(fail("landlock ruleset error", Some(&last_error())));
        }
        Ok(())
    }

    /// Installs the ruleset on this thread, negotiating the ABI down from
    /// [`MAX_ABI`] (`restrict_self`). `Ok(true)` reports partial enforcement.
    pub(super) fn restrict_self(cli: &Cli) -> Result<bool, i32> {
        // SAFETY: the version query takes a null attribute and size 0.
        let abi = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::null::<RulesetAttr>(),
                0usize,
                LANDLOCK_CREATE_RULESET_VERSION,
            )
        };
        if abi < 0 {
            // ENOSYS: no Landlock in this kernel; EOPNOTSUPP: disabled.
            // Either way, fail closed and never exec unconfined.
            return Err(fail(NOT_ENFORCED_MESSAGE, None));
        }
        let partial = abi < MAX_ABI;
        let handled = fs_mask_for_abi(abi.min(MAX_ABI));
        let attr = RulesetAttr {
            handled_access_fs: handled,
        };
        // SAFETY: `attr` matches the kernel's first field and outlives the call.
        let ruleset = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                &attr as *const RulesetAttr,
                std::mem::size_of::<RulesetAttr>(),
                0 as libc::c_uint,
            )
        };
        let Ok(ruleset) = i32::try_from(ruleset) else {
            return Err(fail("landlock ruleset error", Some(&last_error())));
        };
        if ruleset < 0 {
            return Err(fail("landlock ruleset error", Some(&last_error())));
        }
        // SAFETY: the kernel returned a new descriptor this code owns.
        let ruleset = unsafe { OwnedFd::from_raw_fd(ruleset) };
        let read_side = FS_EXECUTE | FS_READ_FILE | FS_READ_DIR;
        for path in &cli.ro {
            add_rule(&ruleset, path, read_side & handled)?;
        }
        for path in &cli.rw {
            add_rule(&ruleset, path, handled)?;
        }
        // SAFETY: plain integer arguments.
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
            return Err(fail("landlock ruleset error", Some(&last_error())));
        }
        // SAFETY: `ruleset` is an open Landlock ruleset descriptor.
        let restricted = unsafe {
            libc::syscall(
                libc::SYS_landlock_restrict_self,
                ruleset.as_raw_fd(),
                0 as libc::c_uint,
            )
        };
        if restricted != 0 {
            return Err(fail("landlock ruleset error", Some(&last_error())));
        }
        Ok(partial)
    }

    /// Execs the command with `PATH` lookup and the standard library's
    /// child setup (default `SIGPIPE`), returning only on failure.
    pub(super) fn exec(command: &[OsString]) -> i32 {
        let Some((program, args)) = command.split_first() else {
            return fail("exec failed", Some("empty command"));
        };
        let error = std::process::Command::new(program).args(args).exec();
        fail("exec failed", Some(&strerror(&error)))
    }
}

#[cfg(not(target_os = "linux"))]
mod sys {
    //! No Landlock off Linux: every confined run and probe fails closed.
    use std::ffi::OsString;

    use super::{Cli, NOT_ENFORCED_MESSAGE, fail};

    pub(super) fn restrict_self(_cli: &Cli) -> Result<bool, i32> {
        Err(fail(NOT_ENFORCED_MESSAGE, None))
    }

    pub(super) fn new_session() -> Result<(), i32> {
        Err(fail(NOT_ENFORCED_MESSAGE, None))
    }

    pub(super) fn exec(_command: &[OsString]) -> i32 {
        fail(NOT_ENFORCED_MESSAGE, None)
    }
}

#[cfg(test)]
mod tests {
    //! Argument parsing and the ABI masks, after `main.c`'s `parse` and
    //! `fs_mask_for_abi`; the helper's real enforcement and its printed
    //! lines run in `tests/enforcement.rs`.
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_grants_and_the_command() {
        assert_eq!(
            parse(args(&["--ro", "/", "--rw", "/w", "--", "bash", "-c", "x"])),
            Ok(Cli {
                probe: false,
                ro: args(&["/"]),
                rw: args(&["/w"]),
                command: args(&["bash", "-c", "x"]),
            })
        );
        assert_eq!(parse(args(&["--probe"])).map(|cli| cli.probe), Ok(true));
        // Flags after `--` belong to the command.
        assert_eq!(
            parse(args(&["--", "--ro", "--probe"])).map(|cli| cli.command),
            Ok(args(&["--ro", "--probe"]))
        );
    }

    #[test]
    fn usage_errors_exit_125() {
        for bad in [
            &["--probe", "--ro", "/"][..],
            &["--ro"],
            &["--rw", "/"],
            &["--rw", "/", "--"],
            &["--bogus", "--", "true"],
            &[],
        ] {
            assert_eq!(parse(args(bad)), Err(LAUNCHER_FAILURE_EXIT), "{bad:?}");
        }
    }

    #[test]
    fn os_error_suffixes_are_removed() {
        let error = std::io::Error::from_raw_os_error(2);
        assert!(!strerror(&error).contains("os error"));
        assert!(!strerror(&error).is_empty());
    }

    /// Both no-follow openers refuse a link in the last or a middle
    /// component and open real paths.
    #[cfg(target_os = "linux")]
    #[test]
    fn rule_paths_never_follow_links() {
        use std::os::unix::ffi::OsStrExt;
        let base = crate::paths::canonical_path(&std::env::temp_dir())
            .join(format!("bake-landlock-nofollow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("real/sub")).expect("dirs");
        std::os::unix::fs::symlink(base.join("real"), base.join("link")).expect("link");
        let bytes = |path: &std::path::Path| path.as_os_str().as_bytes().to_vec();
        type Opener = fn(&[u8]) -> std::io::Result<std::os::fd::OwnedFd>;
        let openers: [(&str, Opener); 2] = [
            ("openat2", sys::open_no_follow),
            ("components", sys::open_by_components),
        ];
        let mut failures = Vec::new();
        for (name, open) in openers {
            for ok in [
                base.join("real"),
                base.join("real/sub"),
                "/".into(),
                "/dev/null".into(),
            ] {
                if let Err(error) = open(&bytes(&ok)) {
                    failures.push(format!("{name}: {} failed: {error}", ok.display()));
                }
            }
            for linked in [base.join("link"), base.join("link/sub")] {
                match open(&bytes(&linked)) {
                    Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {}
                    other => failures.push(format!("{name}: {} gave {other:?}", linked.display())),
                }
            }
        }
        let _ = std::fs::remove_dir_all(&base);
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn abi_masks_grow_as_main_c_s_do() {
        assert_eq!(sys::fs_mask_for_abi(1), (1 << 13) - 1);
        assert_eq!(sys::fs_mask_for_abi(2), (1 << 14) - 1);
        assert_eq!(sys::fs_mask_for_abi(3), (1 << 15) - 1);
        assert_eq!(sys::fs_mask_for_abi(4), (1 << 15) - 1);
        assert_eq!(sys::fs_mask_for_abi(5), (1 << 16) - 1);
    }
}
