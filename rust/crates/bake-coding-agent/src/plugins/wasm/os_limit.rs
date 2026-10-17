//! The helper's own memory limit, established before it reads any input.
//!
//! - **Linux and macOS:** the helper measures its address space (`VmSize`
//!   from `/proc/self/statm`; `virtual_size` from `task_info` on macOS, where
//!   a process starts near 32 GiB), sets a hard `RLIMIT_AS` of that size
//!   plus the budget, and proves the kernel enforces it: a reservation one
//!   MiB past the remaining headroom must fail with `ENOMEM`, and a page
//!   must still succeed. The budget is an increment so that the room left
//!   for the plugin does not depend on how much the binary maps at startup
//!   (about 62 MiB for a release build on Linux x86_64, tens of GiB on
//!   macOS). On macOS the parent starts the helper with
//!   `MallocMediumZone=0`, because libmalloc's medium allocator, active on
//!   Macs with 32 GiB or more, reserves 128 MiB per region and CPU
//!   magazine and consumed the budget with untouched reservations. The
//!   helper requires the setting and checks that one medium-sized
//!   allocation adds less than 32 MiB; that check is narrow and proves no
//!   allocator state (`check_medium_allocator`). It also requires
//!   `MallocMaxMagazines=1`: libmalloc's per-CPU tiny and small magazines
//!   each map their own regions, so without it the reservations grow with
//!   how many CPUs the helper's threads ran on. The hard limit is the
//!   same either way. On Linux the startup size is also capped at
//!   [`MAX_LINUX_BASELINE`](super::MAX_LINUX_BASELINE), so the absolute
//!   limit is at most the budget plus 256 MiB. The probes show that the
//!   kernel refuses a mapping past the limit and allows a small one; they
//!   do not exercise every way the helper can allocate. The limit bounds
//!   every mapping the helper makes,
//!   including Wasmtime's reservations, JIT code, and the compiler's heap.
//!   It is an address-space bound, not a resident or committed-memory
//!   bound, and it does not cover kernel memory, the page cache, or pages
//!   written through a file descriptor without mapping them. The helper
//!   also sets `RLIMIT_CORE` and `RLIMIT_FSIZE` to zero, so an abort writes
//!   no core file and nothing in the helper can grow a file, memfds
//!   included.
//! - **Windows:** the parent created the helper inside a Job whose
//!   per-process committed-memory limit is the budget, with kill-on-close
//!   and a one-process limit. The helper checks that it runs in such a Job
//!   and proves the limit: committing one MiB past its headroom must fail,
//!   and committing a page must succeed.
//!
//! Any failure leaves the helper without a verified limit; it exits before
//! its `Hello`, and the parent loads nothing. A limit is never retried with
//! a larger budget.

use super::protocol::MemoryLimit;

/// One MiB, the probe's margin past the headroom.
#[cfg(any(unix, windows))]
const MARGIN: u64 = 1 << 20;

/// Establishes and verifies the limit for `budget` bytes.
pub(crate) fn establish(budget: u64) -> Result<MemoryLimit, String> {
    imp::establish(budget)
}

#[cfg(unix)]
mod imp {
    use super::super::protocol::{LimitMechanism, MemoryLimit};
    use super::MARGIN;

    fn set(resource: libc::c_int, value: u64, name: &str) -> Result<(), String> {
        let limit = libc::rlimit {
            rlim_cur: value as libc::rlim_t,
            rlim_max: value as libc::rlim_t,
        };
        // SAFETY: `limit` is a valid `rlimit`; the call has no other
        // preconditions. The resource type differs between libcs.
        if unsafe { libc::setrlimit(resource as _, &limit) } != 0 {
            return Err(format!(
                "setrlimit({name}) failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut read = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: `read` is valid output space.
        if unsafe { libc::getrlimit(resource as _, &mut read) } != 0 {
            return Err(format!(
                "getrlimit({name}) failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        // `rlim_t` is `u64` on the supported targets but not on every libc.
        #[allow(clippy::unnecessary_cast)]
        let (current, maximum) = (read.rlim_cur as u64, read.rlim_max as u64);
        if current != value || maximum != value {
            return Err(format!(
                "{name} reads back as {}/{}, not {value}",
                read.rlim_cur, read.rlim_max
            ));
        }
        Ok(())
    }

    /// The process's current address-space size in bytes.
    #[cfg(target_os = "linux")]
    fn address_space() -> Result<u64, String> {
        let statm = std::fs::read_to_string("/proc/self/statm")
            .map_err(|error| format!("cannot read /proc/self/statm: {error}"))?;
        let pages: u64 = statm
            .split_whitespace()
            .next()
            .and_then(|field| field.parse().ok())
            .ok_or_else(|| "cannot parse /proc/self/statm".to_owned())?;
        Ok(pages.saturating_mul(page_size()?))
    }

    /// The process's current address-space size in bytes.
    #[cfg(target_os = "macos")]
    fn address_space() -> Result<u64, String> {
        // SAFETY: a zeroed `mach_task_basic_info` is valid output space.
        let mut info: libc::mach_task_basic_info = unsafe { std::mem::zeroed() };
        let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
        // SAFETY: `info` holds `count` natural-sized words, as the flavor
        // requires; `mach_task_self` returns this task's port.
        #[allow(deprecated)]
        let status = unsafe {
            libc::task_info(
                libc::mach_task_self(),
                libc::MACH_TASK_BASIC_INFO,
                std::ptr::addr_of_mut!(info).cast(),
                &mut count,
            )
        };
        if status != libc::KERN_SUCCESS {
            return Err(format!("task_info failed: {status}"));
        }
        Ok(info.virtual_size)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn address_space() -> Result<u64, String> {
        Err("this platform has no verified address-space measurement".to_owned())
    }

    fn page_size() -> Result<u64, String> {
        // SAFETY: plain call.
        let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        u64::try_from(size)
            .ok()
            .filter(|size| *size > 0)
            .ok_or_else(|| "cannot read the page size".to_owned())
    }

    /// Maps `len` bytes of inaccessible anonymous memory, unmapping at once
    /// when it succeeds; the error is the `errno`.
    fn reserve(len: u64) -> Result<(), i32> {
        let len = usize::try_from(len).map_err(|_| libc::ENOMEM)?;
        // SAFETY: an anonymous private mapping at no fixed address affects
        // no existing memory.
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0));
        }
        // SAFETY: `address` and `len` are the mapping just created.
        unsafe {
            libc::munmap(address, len);
        }
        Ok(())
    }

    /// The size the medium-allocator probe allocates: inside libmalloc's
    /// medium size class (32 KiB to 8 MiB), where the helper's ordinary
    /// loads failed on macOS.
    #[cfg(target_os = "macos")]
    const MEDIUM_PROBE: usize = 64 << 10;

    /// The most address space that one [`MEDIUM_PROBE`] allocation may add.
    /// A small region is 8 MiB; a medium region is 128 MiB.
    #[cfg(target_os = "macos")]
    const MEDIUM_PROBE_LIMIT: u64 = 32 << 20;

    /// Requires the setting that turns libmalloc's medium allocator off,
    /// then runs one bounded check of it.
    ///
    /// On a Mac with at least 32 GiB of memory, libmalloc serves 32 KiB to
    /// 8 MiB allocations from 128 MiB regions, one set per magazine (up to
    /// one per logical CPU), so the first such allocation on a CPU reserves
    /// 128 MiB of address space while touching a few KiB. Under the
    /// `RLIMIT_AS` budget that turned ordinary allocations into aborts, at a
    /// point that depends on scheduling. The parent starts the helper with
    /// `MallocMediumZone=0`, which libmalloc reads before `main` (in its
    /// `_malloc_initialize`) and which sends those sizes to the small and
    /// large allocators instead. The variable is libmalloc's, not a
    /// documented interface.
    ///
    /// libmalloc also picks the tiny and small magazine for each allocation
    /// by the CPU the thread is running on (`rack_get_thread_index`), and
    /// each magazine maps its own regions: 1 MiB tiny, 8 MiB small. From
    /// libmalloc's source, on the 56-CPU Intel Mac (28 magazines after its
    /// hyperthread shift) a helper whose threads migrate can reserve up to
    /// about 250 MiB that way, depending only on scheduling. At a 128 MiB
    /// budget that left too little for a call's 64 MiB linear memory under
    /// the full test suite, and under heavier contention ordinary loads
    /// aborted as well. The parent therefore also sets
    /// `MallocMaxMagazines=1`, which libmalloc reads in the same place; the
    /// helper runs a few threads, and the cost of sharing one magazine was
    /// not measured.
    ///
    /// The helper refuses to start without those exact settings. The check
    /// after it is narrow: one medium-sized allocation must add less than
    /// [`MEDIUM_PROBE_LIMIT`] of address space. Passing shows only that this
    /// allocation did not create a medium region; it would also pass if one
    /// already existed, so it does not prove the allocator is off or rule
    /// out later reservations. What bounds the helper either way is the
    /// unchanged hard `RLIMIT_AS`: a reservation the setting fails to
    /// prevent still fails inside the limit, as an abort, never as memory
    /// past it.
    #[cfg(target_os = "macos")]
    fn check_medium_allocator() -> Result<(), String> {
        if std::env::var_os("MallocMediumZone").as_deref() != Some(std::ffi::OsStr::new("0")) {
            return Err("the helper must be started with MallocMediumZone=0".to_owned());
        }
        if std::env::var_os("MallocMaxMagazines").as_deref() != Some(std::ffi::OsStr::new("1")) {
            return Err("the helper must be started with MallocMaxMagazines=1".to_owned());
        }
        let before = address_space()?;
        // SAFETY: plain allocation; the block is freed below and never read.
        let block = unsafe { libc::malloc(MEDIUM_PROBE) };
        if block.is_null() {
            return Err("the medium-allocator probe could not allocate".to_owned());
        }
        // Touch the block so the allocation cannot be elided.
        // SAFETY: `block` is a live allocation of `MEDIUM_PROBE` bytes.
        unsafe { std::ptr::write_volatile(block.cast::<u8>(), 1) };
        let after = address_space();
        // SAFETY: `block` came from `malloc` and is freed once.
        unsafe { libc::free(block) };
        let added = after?.saturating_sub(before);
        if added >= MEDIUM_PROBE_LIMIT {
            return Err(format!(
                "a {MEDIUM_PROBE}-byte allocation added {added} bytes of address space \
                 although MallocMediumZone=0; libmalloc's medium allocator appears to be on"
            ));
        }
        Ok(())
    }

    pub(super) fn establish(budget: u64) -> Result<MemoryLimit, String> {
        set(libc::RLIMIT_CORE as libc::c_int, 0, "RLIMIT_CORE")?;
        set(libc::RLIMIT_FSIZE as libc::c_int, 0, "RLIMIT_FSIZE")?;
        // Before the baseline, so the probe's own small region is counted
        // in it rather than in the budget.
        #[cfg(target_os = "macos")]
        check_medium_allocator()?;
        let baseline = address_space()?;
        // On Linux the absolute limit is bounded as well: a baseline over
        // the cap means something unexpected is mapped, and the helper
        // refuses rather than grant the budget on top of it.
        #[cfg(target_os = "linux")]
        if baseline > super::super::MAX_LINUX_BASELINE {
            return Err(format!(
                "the helper's address space is {baseline} bytes at startup, over the {}-byte cap",
                super::super::MAX_LINUX_BASELINE
            ));
        }
        let limit = baseline
            .checked_add(budget)
            .ok_or_else(|| "the address-space limit overflows".to_owned())?;
        set(libc::RLIMIT_AS as libc::c_int, limit, "RLIMIT_AS")?;
        // The limit must refuse a mapping past the headroom left now.
        let headroom = limit.saturating_sub(address_space()?);
        match reserve(headroom.saturating_add(MARGIN)) {
            Err(errno) if errno == libc::ENOMEM => {}
            Err(errno) => {
                return Err(format!(
                    "the RLIMIT_AS probe failed with {} instead of ENOMEM",
                    std::io::Error::from_raw_os_error(errno)
                ));
            }
            Ok(()) => {
                return Err(format!(
                    "RLIMIT_AS of {limit} bytes is not enforced: a {} byte reservation succeeded",
                    headroom.saturating_add(MARGIN)
                ));
            }
        }
        reserve(page_size()?).map_err(|errno| {
            format!(
                "a one-page reservation failed under RLIMIT_AS: {}",
                std::io::Error::from_raw_os_error(errno)
            )
        })?;
        Ok(MemoryLimit {
            mechanism: LimitMechanism::AddressSpace,
            baseline,
            limit,
            budget,
        })
    }
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::System::JobObjects::{
        IsProcessInJob, JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, QueryInformationJobObject,
    };
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree,
    };
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    use super::super::protocol::{LimitMechanism, MemoryLimit};
    use super::MARGIN;

    fn committed() -> Result<u64, String> {
        // SAFETY: a zeroed counters struct is valid output space.
        let mut counters: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
        counters.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        // SAFETY: the pseudo-handle is valid; `counters` has the size given.
        if unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                std::ptr::addr_of_mut!(counters).cast(),
                counters.cb,
            )
        } == 0
        {
            return Err(format!(
                "GetProcessMemoryInfo failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(counters.PrivateUsage as u64)
    }

    /// Commits `len` bytes, releasing them at once when it succeeds.
    fn commit(len: u64) -> bool {
        let Ok(len) = usize::try_from(len) else {
            return false;
        };
        // SAFETY: a fresh allocation at no fixed address.
        let address = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                len,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            )
        };
        if address.is_null() {
            return false;
        }
        // SAFETY: `address` is the allocation just made.
        unsafe {
            VirtualFree(address, 0, MEM_RELEASE);
        }
        true
    }

    pub(super) fn establish(budget: u64) -> Result<MemoryLimit, String> {
        let mut in_job = 0;
        // SAFETY: the pseudo-handle is valid; a null job asks about any job.
        if unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job) } == 0
            || in_job == 0
        {
            return Err("the helper does not run inside a Job".to_owned());
        }
        // SAFETY: a zeroed struct is valid output space.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: a null handle names this process's Job; `info` has the
        // size given.
        if unsafe {
            QueryInformationJobObject(
                std::ptr::null_mut(),
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of_mut!(info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(format!(
                "QueryInformationJobObject failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let flags = info.BasicLimitInformation.LimitFlags;
        let required = JOB_OBJECT_LIMIT_PROCESS_MEMORY
            | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        if flags & required != required {
            return Err(format!(
                "the helper's Job lacks its limits (flags {flags:#x})"
            ));
        }
        let limit = info.ProcessMemoryLimit as u64;
        if limit != budget {
            return Err(format!(
                "the Job's process memory limit is {limit} bytes, not the {budget}-byte budget"
            ));
        }
        let baseline = committed()?;
        let headroom = limit.saturating_sub(baseline);
        if commit(headroom.saturating_add(MARGIN)) {
            return Err(format!(
                "the Job's {limit}-byte commit limit is not enforced: committing {} bytes succeeded",
                headroom.saturating_add(MARGIN)
            ));
        }
        if !commit(4096) {
            return Err("committing one page failed under the Job's limit".to_owned());
        }
        Ok(MemoryLimit {
            mechanism: LimitMechanism::JobCommit,
            baseline,
            limit,
            budget,
        })
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::super::protocol::MemoryLimit;

    pub(super) fn establish(_budget: u64) -> Result<MemoryLimit, String> {
        Err("this platform has no verified helper memory limit".to_owned())
    }
}
