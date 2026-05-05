//! Daemon singleton lock backed by `flock(2)`.
//!
//! The previous PID-file approach had a TOCTOU window: two daemons could each
//! observe a stale PID file, both decide it was safe to start, and race past
//! the `create_new` step using stale-cleanup paths. `flock` is acquired
//! atomically by the kernel and released when the file descriptor is closed
//! (i.e., on process exit, even via SIGKILL), so a crashed daemon never leaves
//! a wedged lock behind.

use std::fs::{File, OpenOptions};
use std::path::Path;

use anyhow::{Context, Result, bail};
use nix::fcntl::{Flock, FlockArg};

/// RAII lock holder. Dropping releases the advisory lock by closing the fd.
pub struct LockHandle(#[allow(dead_code)] Flock<File>);

/// Acquire an exclusive, non-blocking advisory lock on `path`.
///
/// Returns `Err` if another process already holds the lock. The file is
/// created if it doesn't exist; we keep the same path across acquires so the
/// lock is on the same inode the next daemon will try.
pub fn acquire(path: &Path) -> Result<LockHandle> {
    let f = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("open lock: {}", path.display()))?;
    match Flock::lock(f, FlockArg::LockExclusiveNonblock) {
        Ok(l) => Ok(LockHandle(l)),
        Err((_file, e)) => bail!("daemon already running ({e})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_blocks_second_acquire() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("daemon.lock");
        let _h1 = acquire(&lock).expect("first lock");
        assert!(
            acquire(&lock).is_err(),
            "second lock must fail while first is held"
        );
    }

    #[test]
    fn lock_releases_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("daemon.lock");
        {
            let _h1 = acquire(&lock).expect("first lock");
        }
        // After drop, a fresh acquire must succeed.
        let _h2 = acquire(&lock).expect("re-acquire after drop");
    }
}
