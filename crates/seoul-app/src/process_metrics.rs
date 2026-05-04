//! Self-measurement of the seoul-app client process.
//!
//! Mirrors the macOS path used by `seoul-daemon::resource_monitor` so the
//! client can report its own CPU + memory in the resource panel without
//! routing through the daemon. If the platform-specific syscall fails or
//! we're not on macOS, callers see `None` and treat the metrics as
//! unavailable.

/// Raw CPU time + resident memory for the current process.
///
/// `user_time_us` / `sys_time_us` are cumulative since process start; turn
/// them into a percentage by diffing two samples against wall-clock delta.
#[derive(Debug, Clone, Copy)]
pub struct SelfMetrics {
    pub user_time_us: u64,
    pub sys_time_us: u64,
    pub memory_bytes: u64,
}

pub fn measure_self() -> Option<SelfMetrics> {
    #[cfg(target_os = "macos")]
    {
        macos_measure_self()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn macos_measure_self() -> Option<SelfMetrics> {
    use std::mem;

    let pid = std::process::id() as i32;

    // Prefer proc_pid_rusage: ri_user_time / ri_system_time are in nanoseconds
    // and ri_phys_footprint matches Activity Monitor's memory column.
    unsafe {
        let mut rusage: libc::rusage_info_v4 = mem::zeroed();
        let ret = libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V4,
            &mut rusage as *mut _ as *mut libc::rusage_info_t,
        );
        if ret == 0 {
            return Some(SelfMetrics {
                user_time_us: rusage.ri_user_time / 1000,
                sys_time_us: rusage.ri_system_time / 1000,
                memory_bytes: rusage.ri_phys_footprint,
            });
        }
    }

    // Fallback: proc_taskinfo. CPU time is in Mach absolute time on Intel and
    // approximate-nanoseconds on Apple silicon — good enough for a sanity row.
    unsafe {
        let mut info: libc::proc_taskinfo = mem::zeroed();
        let size = libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            mem::size_of::<libc::proc_taskinfo>() as i32,
        );
        if size <= 0 {
            return None;
        }
        Some(SelfMetrics {
            user_time_us: info.pti_total_user / 1000,
            sys_time_us: info.pti_total_system / 1000,
            memory_bytes: info.pti_resident_size,
        })
    }
}
