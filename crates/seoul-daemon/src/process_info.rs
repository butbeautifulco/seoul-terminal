use std::path::PathBuf;

/// Get the name of the foreground process for a given child PID.
///
/// Uses `tcgetpgrp`-style lookup via sysinfo. Falls back to the child process
/// name if the foreground group cannot be determined.
pub fn foreground_process_name(child_pid: u32) -> Option<String> {
    // On macOS, use libproc to get process name
    #[cfg(target_os = "macos")]
    {
        macos_process_name(child_pid)
    }

    #[cfg(not(target_os = "macos"))]
    {
        linux_process_name(child_pid)
    }
}

/// Get the current working directory of a process.
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        // On macOS, use proc_pidinfo
        macos_process_cwd(pid)
    }

    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/{}/cwd", pid)).ok()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        None
    }
}

#[cfg(target_os = "macos")]
fn macos_process_name(pid: u32) -> Option<String> {
    use std::ffi::CStr;
    use std::mem;

    unsafe {
        let mut info: libc::proc_bsdinfo = mem::zeroed();
        let size = libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            mem::size_of::<libc::proc_bsdinfo>() as i32,
        );
        if size > 0 {
            let name = CStr::from_ptr(info.pbi_comm.as_ptr())
                .to_string_lossy()
                .into_owned();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn macos_process_cwd(pid: u32) -> Option<PathBuf> {
    use std::mem;

    unsafe {
        let mut vpi: libc::proc_vnodepathinfo = mem::zeroed();
        let size = libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            &mut vpi as *mut _ as *mut libc::c_void,
            mem::size_of::<libc::proc_vnodepathinfo>() as i32,
        );
        if size > 0 {
            let cwd = std::ffi::CStr::from_ptr(vpi.pvi_cdir.vip_path.as_ptr() as *const i8)
                .to_string_lossy()
                .into_owned();
            if !cwd.is_empty() {
                return Some(PathBuf::from(cwd));
            }
        }
    }
    None
}

#[cfg(all(not(target_os = "macos"), not(target_os = "linux")))]
fn linux_process_name(pid: u32) -> Option<String> {
    // Read from /proc/{pid}/comm
    std::fs::read_to_string(format!("/proc/{}/comm", pid))
        .ok()
        .map(|s| s.trim().to_string())
}
