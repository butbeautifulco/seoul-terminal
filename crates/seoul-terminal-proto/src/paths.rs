use std::path::PathBuf;

use anyhow::{Context, Result};
use uuid::Uuid;

pub fn seoul_dir() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("home directory not found")?
        .join(".seoul"))
}

pub fn socket_path() -> Result<PathBuf> {
    Ok(seoul_dir()?.join("terminal-host.sock"))
}

pub fn pid_path() -> Result<PathBuf> {
    Ok(seoul_dir()?.join("terminal-host.pid"))
}

pub fn token_path() -> Result<PathBuf> {
    Ok(seoul_dir()?.join("terminal-host.token"))
}

pub fn lock_path() -> Result<PathBuf> {
    Ok(seoul_dir()?.join("terminal-host.lock"))
}

pub fn daemon_log_path() -> Result<PathBuf> {
    Ok(seoul_dir()?.join("daemon.log"))
}

pub fn terminal_history_dir() -> Result<PathBuf> {
    Ok(seoul_dir()?.join("terminal-history"))
}

pub fn session_history_dir(session_id: Uuid) -> Result<PathBuf> {
    Ok(terminal_history_dir()?.join(session_id.to_string()))
}

pub fn scrollback_path(session_id: Uuid) -> Result<PathBuf> {
    Ok(session_history_dir(session_id)?.join("scrollback.bin"))
}

pub fn meta_path(session_id: Uuid) -> Result<PathBuf> {
    Ok(session_history_dir(session_id)?.join("meta.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Verifies that `seoul_dir()` propagates rather than panics when no home
    // directory is found.
    //
    // Ignored because `dirs::home_dir()` on Unix falls back to `getpwuid_r`
    // when `$HOME` is unset, so removing `$HOME` alone does not make it return
    // `None` on macOS / Linux dev machines. There's no portable way to force
    // the failure path without mocking `dirs`. The behavior change is still
    // exercised at the type level: every caller must handle the `Result`, so
    // the daemon can no longer panic on a missing home dir at runtime.
    //
    // The test also mutates the process-wide `$HOME` env var, which is not
    // thread-safe under cargo's default parallel test harness — another reason
    // to keep it ignored unless run explicitly via `--ignored`.
    #[test]
    #[ignore = "dirs::home_dir() falls back to getpwuid_r; cannot trigger Err without mocking"]
    fn seoul_dir_returns_err_when_home_unset() {
        let saved = std::env::var_os("HOME");
        // SAFETY: Mutating environment is unsafe in multi-threaded programs
        // because libc getenv is not thread-safe. This test is single-threaded
        // (no spawned threads observe $HOME), so this is sound here.
        unsafe {
            std::env::remove_var("HOME");
        }
        let r = seoul_dir();
        if let Some(h) = saved {
            unsafe {
                std::env::set_var("HOME", h);
            }
        }
        assert!(r.is_err(), "expected Err when $HOME is unset");
    }
}
