use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use uuid::Uuid;

use seoul_terminal_proto::resources::*;

// ── Public API ──────────────────────────────────────

/// Demand-driven resource monitor with caching.
/// Only collects metrics when `snapshot()` is called; zero cost when idle.
pub struct ResourceMonitor {
    /// Previous CPU time samples for delta-based CPU% calculation.
    cpu_samples: HashMap<u32, CpuSample>,
    /// Cached snapshot to avoid redundant syscalls within TTL.
    cached_snapshot: Option<ResourceSnapshot>,
    last_refresh: Instant,
    cache_ttl: Duration,
}

struct CpuSample {
    user_time_us: u64,
    sys_time_us: u64,
    wall_time: Instant,
}

/// Info about a session that the monitor needs to collect metrics for.
pub struct SessionInfo {
    pub session_id: Uuid,
    pub workspace_id: Uuid,
    pub child_pid: u32,
    pub foreground_process: Option<String>,
}

impl ResourceMonitor {
    pub fn new() -> Self {
        Self {
            cpu_samples: HashMap::new(),
            cached_snapshot: None,
            last_refresh: Instant::now() - Duration::from_secs(60),
            cache_ttl: Duration::from_millis(2500),
        }
    }

    /// Invalidate the cache (call on session create/kill).
    pub fn invalidate_cache(&mut self) {
        self.cached_snapshot = None;
    }

    /// Collect or return cached resource snapshot.
    pub fn snapshot(&mut self, sessions: &[SessionInfo]) -> ResourceSnapshot {
        if let Some(cached) = &self.cached_snapshot
            && self.last_refresh.elapsed() < self.cache_ttl
        {
            return cached.clone();
        }

        let snapshot = self.collect(sessions);
        self.cached_snapshot = Some(snapshot.clone());
        self.last_refresh = Instant::now();

        // Prune stale CPU samples (PIDs not seen in 5 minutes)
        let stale_cutoff = Instant::now() - Duration::from_secs(300);
        self.cpu_samples
            .retain(|_, sample| sample.wall_time > stale_cutoff);

        snapshot
    }

    fn collect(&mut self, sessions: &[SessionInfo]) -> ResourceSnapshot {
        let timestamp_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Build global parent→children map for process tree traversal
        let (parent_map, all_bsd_info) = build_process_tree();

        // Global visited set to prevent double-counting shared processes
        let mut visited: HashSet<u32> = HashSet::new();
        let mut inaccessible_pids: u32 = 0;

        // Collect per-session metrics
        let mut session_metrics = Vec::with_capacity(sessions.len());
        let mut total_cpu = 0.0;
        let mut total_mem: u64 = 0;

        for session in sessions {
            let subtree_pids = get_subtree_pids(session.child_pid, &parent_map, &mut visited);
            let child_process_count = subtree_pids.len() as u32;

            let mut session_cpu = 0.0;
            let mut session_mem: u64 = 0;

            for &pid in &subtree_pids {
                // Skip zombie processes
                if let Some(info) = all_bsd_info.get(&pid)
                    && info.status == ProcessStatus::Zombie
                {
                    continue;
                }

                match get_process_metrics(pid) {
                    Some(metrics) => {
                        let cpu = self.compute_cpu_percent(
                            pid,
                            metrics.user_time_us,
                            metrics.sys_time_us,
                        );
                        session_cpu += cpu;
                        session_mem += metrics.phys_footprint.unwrap_or(metrics.resident_size);
                    }
                    None => {
                        inaccessible_pids += 1;
                    }
                }
            }

            total_cpu += session_cpu;
            total_mem += session_mem;

            session_metrics.push(SessionResourceMetrics {
                session_id: session.session_id,
                workspace_id: session.workspace_id,
                foreground_process: session.foreground_process.clone(),
                cpu_percent: session_cpu,
                memory_bytes: session_mem,
                child_process_count,
            });
        }

        // Daemon self-metrics
        let daemon_pid = std::process::id();
        let daemon = match get_process_metrics(daemon_pid) {
            Some(m) => {
                let cpu = self.compute_cpu_percent(daemon_pid, m.user_time_us, m.sys_time_us);
                DaemonMetrics {
                    pid: daemon_pid,
                    cpu_percent: cpu,
                    memory_bytes: m.phys_footprint.unwrap_or(m.resident_size),
                }
            }
            None => DaemonMetrics {
                pid: daemon_pid,
                cpu_percent: 0.0,
                memory_bytes: 0,
            },
        };

        let system = get_system_metrics();

        ResourceSnapshot {
            timestamp_secs,
            daemon,
            sessions: session_metrics,
            system,
            total_cpu_percent: total_cpu,
            total_memory_bytes: total_mem,
            inaccessible_pids,
        }
    }

    /// Compute CPU% as delta from previous sample.
    /// Returns 0.0 on first call for a given PID (no previous sample to diff).
    fn compute_cpu_percent(&mut self, pid: u32, user_us: u64, sys_us: u64) -> f64 {
        let now = Instant::now();
        let cpu_total_us = user_us + sys_us;

        if let Some(prev) = self.cpu_samples.get(&pid) {
            let wall_delta = now.duration_since(prev.wall_time).as_micros() as f64;
            if wall_delta > 0.0 {
                let prev_total = prev.user_time_us + prev.sys_time_us;
                let cpu_delta = cpu_total_us.saturating_sub(prev_total) as f64;
                let percent = (cpu_delta / wall_delta) * 100.0;

                self.cpu_samples.insert(
                    pid,
                    CpuSample {
                        user_time_us: user_us,
                        sys_time_us: sys_us,
                        wall_time: now,
                    },
                );

                return percent.clamp(0.0, 10000.0); // Cap at 100 cores
            }
        }

        // First sample: store and return 0%
        self.cpu_samples.insert(
            pid,
            CpuSample {
                user_time_us: user_us,
                sys_time_us: sys_us,
                wall_time: now,
            },
        );
        0.0
    }
}

// ── Process tree ────────────────────────────────────

#[derive(Debug)]
struct BsdInfo {
    #[allow(dead_code)]
    ppid: u32,
    status: ProcessStatus,
}

#[derive(Debug, PartialEq)]
enum ProcessStatus {
    Alive,
    Zombie,
}

/// Build a map of PID → BsdInfo (ppid + status) for all system processes.
/// Also returns parent→children map for tree traversal.
fn build_process_tree() -> (HashMap<u32, Vec<u32>>, HashMap<u32, BsdInfo>) {
    let mut parent_to_children: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut bsd_info_map: HashMap<u32, BsdInfo> = HashMap::new();

    #[cfg(target_os = "macos")]
    {
        use std::mem;

        // Get all PIDs
        let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if count <= 0 {
            return (parent_to_children, bsd_info_map);
        }

        let mut pids = vec![0i32; count as usize];
        let actual = unsafe {
            libc::proc_listallpids(
                pids.as_mut_ptr() as *mut libc::c_void,
                (pids.len() * mem::size_of::<i32>()) as i32,
            )
        };
        if actual <= 0 {
            return (parent_to_children, bsd_info_map);
        }
        pids.truncate(actual as usize);

        // Get BSD info for each PID (ppid, status)
        for &pid in &pids {
            if pid <= 0 {
                continue;
            }
            let pid_u32 = pid as u32;

            unsafe {
                let mut info: libc::proc_bsdinfo = mem::zeroed();
                let size = libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    &mut info as *mut _ as *mut libc::c_void,
                    mem::size_of::<libc::proc_bsdinfo>() as i32,
                );
                if size > 0 {
                    let status = if info.pbi_status == libc::SZOMB {
                        ProcessStatus::Zombie
                    } else {
                        ProcessStatus::Alive
                    };

                    let ppid = info.pbi_ppid;
                    parent_to_children.entry(ppid).or_default().push(pid_u32);

                    bsd_info_map.insert(pid_u32, BsdInfo { ppid, status });
                }
            }
        }
    }

    (parent_to_children, bsd_info_map)
}

/// Get all PIDs in a process's subtree (including itself).
/// Uses global `visited` set to prevent double-counting.
fn get_subtree_pids(
    root_pid: u32,
    parent_to_children: &HashMap<u32, Vec<u32>>,
    visited: &mut HashSet<u32>,
) -> Vec<u32> {
    let mut result = Vec::new();
    let mut stack = vec![root_pid];

    while let Some(pid) = stack.pop() {
        if !visited.insert(pid) {
            continue; // Already counted by another session
        }
        result.push(pid);

        if let Some(children) = parent_to_children.get(&pid) {
            stack.extend(children);
        }
    }

    result
}

// ── Per-process metrics ─────────────────────────────

struct RawProcessMetrics {
    user_time_us: u64,
    sys_time_us: u64,
    resident_size: u64,
    /// macOS physical footprint (matches Activity Monitor). None on other platforms.
    phys_footprint: Option<u64>,
}

/// Get CPU time and memory for a single process.
fn get_process_metrics(pid: u32) -> Option<RawProcessMetrics> {
    #[cfg(target_os = "macos")]
    {
        macos_process_metrics(pid)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        None
    }
}

#[cfg(target_os = "macos")]
fn macos_process_metrics(pid: u32) -> Option<RawProcessMetrics> {
    use std::mem;

    // Prefer proc_pid_rusage: ri_user_time/ri_system_time are always in nanoseconds,
    // unlike proc_taskinfo's pti_total_user which uses Mach absolute time units
    // (not nanoseconds on Intel Macs).
    unsafe {
        let mut rusage: libc::rusage_info_v4 = mem::zeroed();
        let ret = libc::proc_pid_rusage(
            pid as i32,
            libc::RUSAGE_INFO_V4,
            &mut rusage as *mut _ as *mut libc::rusage_info_t,
        );
        if ret == 0 {
            return Some(RawProcessMetrics {
                // ri_user_time/ri_system_time are in nanoseconds; convert to microseconds
                user_time_us: rusage.ri_user_time / 1000,
                sys_time_us: rusage.ri_system_time / 1000,
                resident_size: rusage.ri_resident_size,
                phys_footprint: Some(rusage.ri_phys_footprint),
            });
        }
    }

    // Fallback to proc_taskinfo (less accurate CPU time on Intel, no phys_footprint)
    unsafe {
        let mut info: libc::proc_taskinfo = mem::zeroed();
        let size = libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTASKINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            mem::size_of::<libc::proc_taskinfo>() as i32,
        );
        if size <= 0 {
            return None;
        }
        // pti_total_user/system are Mach absolute time units — approximate as nanoseconds
        Some(RawProcessMetrics {
            user_time_us: info.pti_total_user / 1000,
            sys_time_us: info.pti_total_system / 1000,
            resident_size: info.pti_resident_size,
            phys_footprint: None,
        })
    }
}

// ── System metrics ──────────────────────────────────

fn get_system_metrics() -> SystemMetrics {
    #[cfg(target_os = "macos")]
    {
        macos_system_metrics()
    }

    #[cfg(not(target_os = "macos"))]
    {
        SystemMetrics {
            total_memory_bytes: 0,
            used_memory_bytes: 0,
            cpu_count: 0,
            load_average_1m: 0.0,
        }
    }
}

#[cfg(target_os = "macos")]
fn macos_system_metrics() -> SystemMetrics {
    use std::mem;

    // Total physical memory via sysctl
    let total_memory_bytes = unsafe {
        let mut size: usize = mem::size_of::<u64>();
        let mut memsize: u64 = 0;
        let mut mib = [libc::CTL_HW, libc::HW_MEMSIZE];
        let ret = libc::sysctl(
            mib.as_mut_ptr(),
            2,
            &mut memsize as *mut _ as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        );
        if ret == 0 { memsize } else { 0 }
    };

    // Used memory via host_statistics64
    let used_memory_bytes = unsafe {
        #[allow(deprecated)]
        let host = libc::mach_host_self();
        let mut vm_info: libc::vm_statistics64 = mem::zeroed();
        let mut count = libc::HOST_VM_INFO64_COUNT;
        let ret = libc::host_statistics64(
            host,
            libc::HOST_VM_INFO64,
            &mut vm_info as *mut _ as libc::host_info64_t,
            &mut count,
        );
        if ret == 0 {
            let page_size = libc::vm_page_size as u64;
            // active + wired + compressed ≈ "memory used" (matches Activity Monitor)
            let active = vm_info.active_count as u64 * page_size;
            let wired = vm_info.wire_count as u64 * page_size;
            let compressed = vm_info.compressor_page_count as u64 * page_size;
            active + wired + compressed
        } else {
            0
        }
    };

    // CPU count (sysconf returns -1 on error)
    let cpu_count = unsafe {
        let n = libc::sysconf(libc::_SC_NPROCESSORS_ONLN);
        if n > 0 { n as u32 } else { 1 }
    };

    // Load average
    let load_average_1m = unsafe {
        let mut loadavg = [0.0f64; 1];
        let ret = libc::getloadavg(loadavg.as_mut_ptr(), 1);
        if ret >= 1 { loadavg[0] } else { 0.0 }
    };

    SystemMetrics {
        total_memory_bytes,
        used_memory_bytes,
        cpu_count,
        load_average_1m,
    }
}
