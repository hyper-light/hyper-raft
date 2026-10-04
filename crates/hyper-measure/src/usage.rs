//! What the operating system charged this process: CPU time, instructions and cycles, and its
//! memory footprint, read per process without privilege (`docs/tails.md` §1a).
//!
//! - macOS: `proc_pid_rusage(getpid(), RUSAGE_INFO_V6)` (`<libproc.h>`; the structure is
//!   `rusage_info_v6` of the SDK's `<sys/resource.h>`, declared here because libc stops at V4).
//!   `ri_user_time` and `ri_system_time` count Mach absolute time units, converted to nanoseconds
//!   by `mach_timebase_info` (`<mach/mach_time.h>`); `ri_instructions` and `ri_cycles` are the
//!   process's retired instructions and cycles; `ri_phys_footprint` and
//!   `ri_lifetime_max_phys_footprint` its footprint now and at its highest, in bytes.
//! - Other Unix: `getrusage(RUSAGE_SELF)`'s user and system times and `ru_maxrss`, the most
//!   resident memory, in KiB on Linux and the BSDs (getrusage(2)); no instruction or cycle count
//!   (Linux has them only through `perf_event_open`, not read here).
//! - Windows: `GetProcessTimes` (100 ns units), `QueryProcessCycleTime`, and
//!   `GetProcessMemoryInfo`'s `PeakWorkingSetSize` and `WorkingSetSize`; no instruction count.
//!
//! The counts are cumulative since the process began; a measurement takes the difference of two
//! reads ([`Usage::since`]). They are the whole process's: a measurement of one piece of work runs
//! it with no other thread of the process busy.
#![allow(unsafe_code)]

use crate::faults::FaultsError;

/// What the process was charged so far, or between two reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// CPU time in user mode, in nanoseconds.
    pub user_ns: u64,
    /// CPU time in the kernel on the process's behalf, in nanoseconds.
    pub system_ns: u64,
    /// Instructions retired, where the OS counts them.
    pub instructions: Option<u64>,
    /// Cycles, where the OS counts them.
    pub cycles: Option<u64>,
    /// The memory footprint now, in bytes (macOS's physical footprint; Windows's working set).
    pub footprint: Option<u64>,
    /// The highest footprint the process reached since it began, in bytes; a difference keeps the
    /// later read's, a high-water mark having no difference.
    pub peak: Option<u64>,
}

impl Usage {
    /// What was charged between `earlier` and `self`.
    pub fn since(&self, earlier: &Self) -> Self {
        let less = |now: Option<u64>, then: Option<u64>| {
            now.zip(then).map(|(now, then)| now.saturating_sub(then))
        };
        Self {
            user_ns: self.user_ns.saturating_sub(earlier.user_ns),
            system_ns: self.system_ns.saturating_sub(earlier.system_ns),
            instructions: less(self.instructions, earlier.instructions),
            cycles: less(self.cycles, earlier.cycles),
            footprint: self.footprint,
            peak: self.peak,
        }
    }
    /// User and system time together.
    pub fn cpu_ns(&self) -> u64 {
        self.user_ns.saturating_add(self.system_ns)
    }
}

/// What the process was charged since it began.
pub fn read() -> Result<Usage, FaultsError> {
    // Miri interprets no foreign call: under it the OS says nothing, as on an OS this reads none.
    if cfg!(miri) {
        return Err(FaultsError::Unsupported);
    }
    os::read()
}

/// The machine's one-minute load average, recorded beside every cost (`docs/tails.md` §2.1):
/// getloadavg(3) on Unix; `None` on Windows, which keeps no load average, or where the call fails.
pub fn load() -> Option<f64> {
    if cfg!(miri) {
        return None;
    }
    load_average()
}

#[cfg(unix)]
fn load_average() -> Option<f64> {
    let mut averages = [0f64; 1];
    // SAFETY: `averages` is a writable array of one `double` that lives across the call, and the
    // count passed is one, which bounds what getloadavg(3) writes.
    let filled = unsafe { libc::getloadavg(averages.as_mut_ptr(), 1) };
    if filled == 1 {
        averages.first().copied()
    } else {
        None
    }
}

#[cfg(not(unix))]
fn load_average() -> Option<f64> {
    None
}

#[cfg(target_vendor = "apple")]
mod os {
    use super::{FaultsError, Usage};

    /// `struct rusage_info_v6` of the macOS SDK's `<sys/resource.h>`, field for field.
    #[repr(C)]
    struct RusageInfoV6 {
        ri_uuid: [u8; 16],
        ri_user_time: u64,
        ri_system_time: u64,
        ri_pkg_idle_wkups: u64,
        ri_interrupt_wkups: u64,
        ri_pageins: u64,
        ri_wired_size: u64,
        ri_resident_size: u64,
        ri_phys_footprint: u64,
        ri_proc_start_abstime: u64,
        ri_proc_exit_abstime: u64,
        ri_child_user_time: u64,
        ri_child_system_time: u64,
        ri_child_pkg_idle_wkups: u64,
        ri_child_interrupt_wkups: u64,
        ri_child_pageins: u64,
        ri_child_elapsed_abstime: u64,
        ri_diskio_bytesread: u64,
        ri_diskio_byteswritten: u64,
        ri_cpu_time_qos_default: u64,
        ri_cpu_time_qos_maintenance: u64,
        ri_cpu_time_qos_background: u64,
        ri_cpu_time_qos_utility: u64,
        ri_cpu_time_qos_legacy: u64,
        ri_cpu_time_qos_user_initiated: u64,
        ri_cpu_time_qos_user_interactive: u64,
        ri_billed_system_time: u64,
        ri_serviced_system_time: u64,
        ri_logical_writes: u64,
        ri_lifetime_max_phys_footprint: u64,
        ri_instructions: u64,
        ri_cycles: u64,
        ri_billed_energy: u64,
        ri_serviced_energy: u64,
        ri_interval_max_phys_footprint: u64,
        ri_runnable_time: u64,
        ri_flags: u64,
        ri_user_ptime: u64,
        ri_system_ptime: u64,
        ri_pinstructions: u64,
        ri_pcycles: u64,
        ri_energy_nj: u64,
        ri_penergy_nj: u64,
        ri_secure_time_in_system: u64,
        ri_secure_ptime_in_system: u64,
        ri_neural_footprint: u64,
        ri_lifetime_max_neural_footprint: u64,
        ri_interval_max_neural_footprint: u64,
        ri_reserved: [u64; 9],
    }

    /// `RUSAGE_INFO_V6` in `<sys/resource.h>`.
    const RUSAGE_INFO_V6: libc::c_int = 6;

    /// `struct mach_timebase_info` of `<mach/mach_time.h>`: absolute time units times `numer`
    /// over `denom` are nanoseconds.
    #[repr(C)]
    #[derive(Default)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }

    unsafe extern "C" {
        /// `<mach/mach_time.h>`; libc's declaration is deprecated in favour of declaring it.
        fn mach_timebase_info(info: *mut Timebase) -> libc::c_int;
    }

    fn nanos(ticks: u64, base: &Timebase) -> Result<u64, FaultsError> {
        let wide = u128::from(ticks)
            .checked_mul(u128::from(base.numer))
            .and_then(|product| product.checked_div(u128::from(base.denom)))
            .ok_or(FaultsError::Range)?;
        u64::try_from(wide).map_err(|_| FaultsError::Range)
    }

    pub(super) fn read() -> Result<Usage, FaultsError> {
        let mut base = Timebase::default();
        // SAFETY: `base` is a writable `mach_timebase_info` that lives across the call, which
        // writes exactly one (`<mach/mach_time.h>`).
        let code = unsafe { mach_timebase_info(&mut base) };
        if code != libc::KERN_SUCCESS || base.denom == 0 {
            return Err(FaultsError::Call(i64::from(code)));
        }
        let mut info = std::mem::MaybeUninit::<RusageInfoV6>::zeroed();
        // SAFETY: `getpid` has no precondition. `info` is a writable `rusage_info_v6` that lives
        // across the call, and the flavor `RUSAGE_INFO_V6` makes the kernel write exactly that
        // structure (`<libproc.h>`, `<sys/resource.h>`). libc types the buffer as a pointer to
        // `rusage_info_t` (itself `void *`), as the C prototype does; the pointer passed is the
        // structure's address, as C callers pass it.
        let code = unsafe {
            libc::proc_pid_rusage(
                libc::getpid(),
                RUSAGE_INFO_V6,
                info.as_mut_ptr().cast::<libc::rusage_info_t>(),
            )
        };
        if code != 0 {
            return Err(FaultsError::Call(i64::from(
                std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(code),
            )));
        }
        // SAFETY: the call returned 0, so it filled `info`; and a zeroed `rusage_info_v6` is a
        // valid one in any case, all its fields being integers.
        let info = unsafe { info.assume_init() };
        Ok(Usage {
            user_ns: nanos(info.ri_user_time, &base)?,
            system_ns: nanos(info.ri_system_time, &base)?,
            instructions: Some(info.ri_instructions),
            cycles: Some(info.ri_cycles),
            footprint: Some(info.ri_phys_footprint),
            peak: Some(info.ri_lifetime_max_phys_footprint),
        })
    }
}

#[cfg(all(unix, not(target_vendor = "apple")))]
mod os {
    use super::{FaultsError, Usage};

    fn nanos(time: libc::timeval) -> Result<u64, FaultsError> {
        let seconds = u64::try_from(time.tv_sec).map_err(|_| FaultsError::Range)?;
        let micros = u64::try_from(time.tv_usec).map_err(|_| FaultsError::Range)?;
        seconds
            .checked_mul(1_000_000_000)
            .and_then(|ns| ns.checked_add(micros.checked_mul(1_000)?))
            .ok_or(FaultsError::Range)
    }

    pub(super) fn read() -> Result<Usage, FaultsError> {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: `usage` is a writable `rusage` that lives across the call, and `RUSAGE_SELF`
        // is a valid `who` (getrusage(2)); the call writes at most one `rusage`.
        let code = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
        if code != 0 {
            return Err(FaultsError::Call(i64::from(
                std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(code),
            )));
        }
        // SAFETY: getrusage returned 0, so it filled `usage`; and a zeroed `rusage` is a valid
        // one in any case, all its fields being integers.
        let usage = unsafe { usage.assume_init() };
        // `ru_maxrss` is in KiB on Linux and the BSDs (getrusage(2)).
        let peak = u64::try_from(usage.ru_maxrss)
            .ok()
            .and_then(|kib| kib.checked_mul(1_024));
        Ok(Usage {
            user_ns: nanos(usage.ru_utime)?,
            system_ns: nanos(usage.ru_stime)?,
            instructions: None,
            cycles: None,
            footprint: None,
            peak,
        })
    }
}

#[cfg(windows)]
mod os {
    use super::{FaultsError, Usage};
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::{
        ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
        Threading::{GetCurrentProcess, GetProcessTimes},
        WindowsProgramming::QueryProcessCycleTime,
    };

    fn failed() -> FaultsError {
        FaultsError::Call(i64::from(
            std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
        ))
    }

    /// A `FILETIME` duration, in 100 ns units, in nanoseconds.
    fn nanos(time: &FILETIME) -> Result<u64, FaultsError> {
        let units = u64::from(time.dwHighDateTime) << 32 | u64::from(time.dwLowDateTime);
        units.checked_mul(100).ok_or(FaultsError::Range)
    }

    pub(super) fn read() -> Result<Usage, FaultsError> {
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
        // SAFETY: `GetCurrentProcess` returns this process's pseudo-handle and has no
        // precondition; each `FILETIME` is writable and lives across the call, which writes one
        // into each.
        let done = unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
        };
        if done == 0 {
            return Err(failed());
        }
        let mut cycles = 0u64;
        // SAFETY: the pseudo-handle as above; `cycles` is a writable `u64` that lives across the
        // call, which writes one.
        let counted = unsafe { QueryProcessCycleTime(GetCurrentProcess(), &mut cycles) };
        let size = u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS>())
            .map_err(|_| FaultsError::Range)?;
        // SAFETY: PROCESS_MEMORY_COUNTERS is plain integers, for which all zeroes is a valid
        // value; `cb` is then set to its size as the call requires.
        let mut counters: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
        counters.cb = size;
        // SAFETY: the pseudo-handle as above; `counters` is a writable `PROCESS_MEMORY_COUNTERS`
        // that lives across the call and `size` is its size, which bounds what the call writes.
        let measured = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) };
        let bytes = |size: usize| u64::try_from(size).ok();
        Ok(Usage {
            user_ns: nanos(&user)?,
            system_ns: nanos(&kernel)?,
            instructions: None,
            cycles: (counted != 0).then_some(cycles),
            footprint: (measured != 0)
                .then(|| bytes(counters.WorkingSetSize))
                .flatten(),
            peak: (measured != 0)
                .then(|| bytes(counters.PeakWorkingSetSize))
                .flatten(),
        })
    }
}

#[cfg(not(any(unix, windows)))]
mod os {
    use super::{FaultsError, Usage};

    pub(super) fn read() -> Result<Usage, FaultsError> {
        Err(FaultsError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    /// Work done between two reads is charged to the process: its CPU time grows, and on macOS
    /// its instructions and cycles do, and the peak is at least the footprint.
    #[test]
    fn work_between_two_reads_is_charged() {
        let before = super::read().unwrap();
        let mut sum = 0u64;
        for step in 0..20_000_000u64 {
            sum = std::hint::black_box(sum.wrapping_add(step ^ (sum >> 3)));
        }
        std::hint::black_box(sum);
        let after = super::read().unwrap();
        let spent = after.since(&before);
        assert!(spent.cpu_ns() > 0, "{spent:?}");
        if cfg!(target_vendor = "apple") {
            assert!(spent.instructions.unwrap() >= 20_000_000, "{spent:?}");
            assert!(spent.cycles.unwrap() > 0, "{spent:?}");
            assert!(after.peak.unwrap() >= after.footprint.unwrap(), "{after:?}");
        }
    }
}
