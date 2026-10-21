//! Real kernel faults for the crash tests, Linux only: a seccomp filter
//! that makes the kernel fail chosen write or sync system calls with
//! `EIO`, and the rule that decides whether a test may skip when its
//! kernel feature is unavailable.
//!
//! The filter is installed with `prctl(PR_SET_NO_NEW_PRIVS)` and
//! `seccomp(SECCOMP_SET_MODE_FILTER)` on the calling thread alone, and is
//! inherited by the threads it starts later; nothing in the writer under
//! test changes. It returns `SECCOMP_RET_ERRNO | EIO` for `write` and
//! `pwrite64`, or for `fsync` and `fdatasync`, whose descriptor (the first
//! argument) is in a range and, optionally, whose byte count (the third
//! argument of a write) is one value; every other call runs.

use std::io;
use std::ops::Range;

/// Set to `1` (CI sets it) to make a crash test whose kernel feature is
/// unavailable fail instead of skipping.
pub(crate) const REQUIRE_KERNEL_FAULTS: &str = "BAKE_REQUIRE_KERNEL_FAULTS";

/// A test needs `what`, which this kernel or sandbox refuses (`why`):
/// print that it skips, or fail when [`REQUIRE_KERNEL_FAULTS`] is `1`.
pub(crate) fn skip_unavailable(what: &str, why: &str) {
    let required = std::env::var_os(REQUIRE_KERNEL_FAULTS).is_some_and(|value| value == "1");
    assert!(
        !required,
        "{what} is unavailable ({why}), and {REQUIRE_KERNEL_FAULTS}=1 requires it"
    );
    eprintln!("skipped: {what} is unavailable ({why}); {REQUIRE_KERNEL_FAULTS}=1 requires it");
}

/// The system calls a filter fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Calls {
    /// `write` and `pwrite64`.
    Write,
    /// `fsync` and `fdatasync`.
    Sync,
}

impl Calls {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "write" => Some(Self::Write),
            "sync" => Some(Self::Sync),
            _ => None,
        }
    }

    fn numbers(self) -> [libc::c_long; 2] {
        match self {
            Self::Write => [libc::SYS_write, libc::SYS_pwrite64],
            Self::Sync => [libc::SYS_fsync, libc::SYS_fdatasync],
        }
    }
}

/// A filter failing `calls` on the descriptors in `fds` with `EIO`; with
/// `count`, only a write of exactly that many bytes.
#[derive(Debug, Clone)]
pub(crate) struct EioFilter {
    pub(crate) calls: Calls,
    pub(crate) fds: Range<u32>,
    pub(crate) count: Option<u32>,
}

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xC000_003E;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xC000_00B7;

/// Offsets into `struct seccomp_data`: the call number, the architecture,
/// and the low 32 bits of an argument on a little-endian machine.
const NR: u32 = 0;
const ARCH: u32 = 4;
const fn arg(index: u32) -> u32 {
    16 + 8 * index
}

fn load(offset: u32) -> libc::sock_filter {
    libc::sock_filter {
        code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
        jt: 0,
        jf: 0,
        k: offset,
    }
}

fn ret(value: u32) -> libc::sock_filter {
    libc::sock_filter {
        code: (libc::BPF_RET | libc::BPF_K) as u16,
        jt: 0,
        jf: 0,
        k: value,
    }
}

/// A conditional jump from instruction `at` to `yes` or `no`.
fn jump(op: u32, value: u32, at: usize, yes: usize, no: usize) -> libc::sock_filter {
    let offset = |to: usize| u8::try_from(to - at - 1).expect("a short filter");
    libc::sock_filter {
        code: (libc::BPF_JMP | op | libc::BPF_K) as u16,
        jt: offset(yes),
        jf: offset(no),
        k: value,
    }
}

impl EioFilter {
    /// The BPF program: allow a foreign architecture and every other call;
    /// fail a matching call with `EIO`.
    fn program(&self) -> Vec<libc::sock_filter> {
        let numbers = self.calls.numbers();
        let check = 3 + numbers.len();
        let fail = check + 3 + if self.count.is_some() { 2 } else { 0 };
        let allow = fail + 1;
        let mut program = vec![
            load(ARCH),
            jump(libc::BPF_JEQ, AUDIT_ARCH, 1, 2, allow),
            load(NR),
        ];
        for (index, number) in numbers.into_iter().enumerate() {
            let at = 3 + index;
            let no = if index + 1 == numbers.len() {
                allow
            } else {
                at + 1
            };
            let number = u32::try_from(number).expect("a call number");
            program.push(jump(libc::BPF_JEQ, number, at, check, no));
        }
        program.push(load(arg(0)));
        program.push(jump(
            libc::BPF_JGE,
            self.fds.start,
            check + 1,
            check + 2,
            allow,
        ));
        program.push(jump(
            libc::BPF_JGE,
            self.fds.end,
            check + 2,
            allow,
            check + 3,
        ));
        if let Some(count) = self.count {
            program.push(load(arg(2)));
            program.push(jump(libc::BPF_JEQ, count, check + 4, fail, allow));
        }
        program.push(ret(libc::SECCOMP_RET_ERRNO | libc::EIO as u32));
        program.push(ret(libc::SECCOMP_RET_ALLOW));
        assert_eq!(program.len(), allow + 1);
        program
    }

    /// Install the filter on the calling thread, for good.
    pub(crate) fn install(&self) -> io::Result<()> {
        let mut program = self.program();
        let fprog = libc::sock_fprog {
            len: u16::try_from(program.len()).expect("a short filter"),
            filter: program.as_mut_ptr(),
        };
        let none: libc::c_ulong = 0;
        // SAFETY: `prctl` takes plain values; `seccomp` reads `fprog` and
        // the instructions it points to, which both outlive the call, and
        // copies the program into the kernel.
        unsafe {
            if libc::prctl(
                libc::PR_SET_NO_NEW_PRIVS,
                1 as libc::c_ulong,
                none,
                none,
                none,
            ) != 0
            {
                return Err(io::Error::last_os_error());
            }
            if libc::syscall(
                libc::SYS_seccomp,
                libc::SECCOMP_SET_MODE_FILTER,
                0 as libc::c_uint,
                &raw const fprog,
            ) != 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

/// The descriptor the next `open` of this process returns, while no other
/// thread opens or closes one: the lowest free.
pub(crate) fn lowest_free_fd() -> u32 {
    use std::os::fd::AsRawFd;
    let probe = std::fs::File::open("/dev/null").expect("open /dev/null");
    u32::try_from(probe.as_raw_fd()).expect("a descriptor")
}
