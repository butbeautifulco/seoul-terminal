use std::collections::VecDeque;
use std::time::{Duration, Instant};

const WINDOW_DURATION: Duration = Duration::from_secs(60);
const MAX_TRANSIENT_ERRORS: usize = 50;

/// Returns true if the I/O error is transient (the daemon should keep running).
pub fn is_transient(err: &std::io::Error) -> bool {
    matches!(
        err.raw_os_error(),
        Some(libc::ENOSPC) | Some(libc::ENOMEM) | Some(libc::EMFILE) | Some(libc::ENFILE)
    )
}

/// Sliding-window counter for transient errors.
/// If more than MAX errors occur within WINDOW, the daemon should shut down.
pub struct TransientErrorWindow {
    timestamps: VecDeque<Instant>,
}

impl TransientErrorWindow {
    pub fn new() -> Self {
        Self {
            timestamps: VecDeque::new(),
        }
    }

    /// Record a transient error. Returns `true` if the threshold is exceeded
    /// and the caller should treat this as fatal.
    pub fn record(&mut self) -> bool {
        let now = Instant::now();
        let cutoff = now - WINDOW_DURATION;

        // Prune old entries from the front
        while self.timestamps.front().is_some_and(|&t| t < cutoff) {
            self.timestamps.pop_front();
        }

        self.timestamps.push_back(now);
        self.timestamps.len() > MAX_TRANSIENT_ERRORS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_error_detection() {
        let enospc = std::io::Error::from_raw_os_error(libc::ENOSPC);
        let enomem = std::io::Error::from_raw_os_error(libc::ENOMEM);
        let emfile = std::io::Error::from_raw_os_error(libc::EMFILE);
        let enfile = std::io::Error::from_raw_os_error(libc::ENFILE);
        let ebadf = std::io::Error::from_raw_os_error(libc::EBADF);
        let epipe = std::io::Error::from_raw_os_error(libc::EPIPE);

        assert!(is_transient(&enospc));
        assert!(is_transient(&enomem));
        assert!(is_transient(&emfile));
        assert!(is_transient(&enfile));
        assert!(!is_transient(&ebadf));
        assert!(!is_transient(&epipe));
    }

    #[test]
    fn window_below_threshold() {
        let mut w = TransientErrorWindow::new();
        for _ in 0..MAX_TRANSIENT_ERRORS {
            assert!(!w.record());
        }
    }

    #[test]
    fn window_exceeds_threshold() {
        let mut w = TransientErrorWindow::new();
        for _ in 0..MAX_TRANSIENT_ERRORS {
            w.record();
        }
        // One more pushes over the limit
        assert!(w.record());
    }
}
