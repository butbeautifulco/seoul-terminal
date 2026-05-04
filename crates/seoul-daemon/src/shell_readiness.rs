use std::path::Path;
use std::time::{Duration, Instant};

/// OSC 777 marker emitted by shell integration hooks once the first prompt is ready.
pub const SHELL_READY_MARKER: &[u8] = b"\x1b]777;seoul-shell-ready\x07";

const SHELL_READY_TIMEOUT: Duration = Duration::from_secs(3);
const SUPPORTED_SHELLS: &[&str] = &["zsh", "bash", "fish"];

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReadyState {
    /// Shell is initializing; user input is buffered, stale escape responses are dropped.
    Pending,
    /// Marker detected — shell is ready for input.
    Ready,
    /// Marker never arrived within the timeout window.
    TimedOut,
    /// Shell does not support the readiness marker (e.g., sh, ksh).
    Unsupported,
}

/// Result of scanning PTY output for the readiness marker.
#[must_use]
pub struct ScanResult {
    /// Bytes to forward to clients (marker bytes stripped).
    pub forward: Vec<u8>,
    /// True if the shell just transitioned to Ready.
    pub became_ready: bool,
}

pub struct ShellReadinessTracker {
    state: ReadyState,
    /// Position in SHELL_READY_MARKER that we've matched so far.
    marker_match_pos: usize,
    /// Bytes withheld during partial marker matching.
    held_bytes: Vec<u8>,
    created_at: Instant,
    /// When false, `buffer_input` passes all data through without dropping
    /// terminal query responses. Used for cold restore where ghostty generates
    /// fresh responses to the new shell's queries (not stale leftovers).
    filter_stale_responses: bool,
}

impl ShellReadinessTracker {
    pub fn new(shell_path: &str) -> Self {
        let shell_name = Path::new(shell_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        let state = if SUPPORTED_SHELLS.contains(&shell_name) {
            ReadyState::Pending
        } else {
            ReadyState::Unsupported
        };

        Self {
            state,
            marker_match_pos: 0,
            held_bytes: Vec::new(),
            created_at: Instant::now(),
            filter_stale_responses: true,
        }
    }

    pub fn state(&self) -> ReadyState {
        self.state
    }

    /// Whether the tracker is actively scanning for the marker.
    /// Returns false once the shell is Ready, TimedOut, or Unsupported —
    /// callers can skip `scan_output` entirely in those states.
    pub fn is_active(&self) -> bool {
        self.state == ReadyState::Pending
    }

    /// Scan PTY output for the readiness marker or common shell integration OSCs.
    /// Returns bytes to forward and whether the shell just became ready.
    pub fn scan_output(&mut self, data: &[u8]) -> ScanResult {
        if self.state != ReadyState::Pending {
            return ScanResult {
                forward: data.to_vec(),
                became_ready: false,
            };
        }

        // Fast check: detect common shell integration OSC sequences as readiness.
        // If the shell is sending OSC 7 (CWD), OSC 133 (semantic prompt), or
        // OSC 1337 (iTerm2 integration), it's initialized and ready for input.
        if contains_shell_integration_osc(data) {
            self.state = ReadyState::Ready;
            self.held_bytes.clear();
            self.marker_match_pos = 0;
            return ScanResult {
                forward: data.to_vec(),
                became_ready: true,
            };
        }

        let mut forward = Vec::with_capacity(data.len());
        let mut became_ready = false;

        for &byte in data {
            // After marker matched, remaining bytes go straight to forward
            if became_ready {
                forward.push(byte);
                continue;
            }

            if byte == SHELL_READY_MARKER[self.marker_match_pos] {
                // Partial match — hold this byte
                self.held_bytes.push(byte);
                self.marker_match_pos += 1;

                if self.marker_match_pos == SHELL_READY_MARKER.len() {
                    // Full marker match! Transition to Ready.
                    self.state = ReadyState::Ready;
                    self.held_bytes.clear();
                    self.marker_match_pos = 0;
                    became_ready = true;
                }
            } else {
                // Mismatch — flush held bytes as regular output, then process current byte
                if !self.held_bytes.is_empty() {
                    forward.extend_from_slice(&self.held_bytes);
                    self.held_bytes.clear();
                    self.marker_match_pos = 0;
                }
                // Check if current byte starts a new match
                if byte == SHELL_READY_MARKER[0] {
                    self.held_bytes.push(byte);
                    self.marker_match_pos = 1;
                } else {
                    forward.push(byte);
                }
            }
        }

        ScanResult {
            forward,
            became_ready,
        }
    }

    /// Disable stale response filtering. Used for cold restore where the client's
    /// ghostty terminal generates fresh responses to the new shell's queries.
    pub fn disable_stale_response_filter(&mut self) {
        self.filter_stale_responses = false;
    }

    /// Filter input during shell initialization.
    /// Returns `Some(data)` to write to PTY, or `None` to drop (stale responses).
    /// User input always passes through immediately — only stale terminal query
    /// responses (DA1, DSR) are dropped during the Pending window.
    pub fn buffer_input(&mut self, data: Vec<u8>) -> Option<Vec<u8>> {
        match self.state {
            ReadyState::Pending if self.filter_stale_responses => {
                if is_terminal_query_response(&data) {
                    return None;
                }
                Some(data)
            }
            _ => Some(data),
        }
    }

    /// Check if the readiness timeout has expired.
    pub fn check_timeout(&mut self) {
        if self.state != ReadyState::Pending {
            return;
        }
        if self.created_at.elapsed() >= SHELL_READY_TIMEOUT {
            self.state = ReadyState::TimedOut;
            self.held_bytes.clear();
            self.marker_match_pos = 0;
        }
    }
}

/// Detect common shell integration OSC sequences in PTY output.
/// Any of these indicate the shell is initialized and interactive:
/// - OSC 7  (CWD update — zsh/bash/fish send on every prompt)
/// - OSC 133 (FinalTerm/semantic prompt markers)
/// - OSC 1337 (iTerm2 shell integration)
fn contains_shell_integration_osc(data: &[u8]) -> bool {
    // Look for ESC ] followed by known OSC numbers and ;
    const PATTERNS: &[&[u8]] = &[b"\x1b]7;", b"\x1b]133;", b"\x1b]1337;"];
    for pattern in PATTERNS {
        if data.windows(pattern.len()).any(|w| w == *pattern) {
            return true;
        }
    }
    false
}

/// Detect stale terminal query responses that should be dropped during shell init.
///
/// Terminal query responses follow the pattern: ESC [ ... <final-byte>
/// where the final byte is a letter indicating the response type:
/// - `c` = DA1 response (e.g., \x1b[?62;4;22c)
/// - `n` = DSR response (e.g., \x1b[0n)
/// - `R` = cursor position report (e.g., \x1b[24;1R)
///
/// User keypresses that start with ESC are NOT matched:
/// - Arrow keys: ESC[A/B/C/D (only 3 bytes, and A-D are handled)
/// - Alt+key: ESC + single byte (only 2 bytes)
/// - Function keys: ESC[15~ etc. (end with ~, not a letter)
fn is_terminal_query_response(data: &[u8]) -> bool {
    // Must be ESC[ + at least 2 more bytes (parameter + final)
    if data.len() < 4 || data[0] != 0x1b || data[1] != b'[' {
        return false;
    }
    // Check if it ends with a known response final byte
    let last = data[data.len() - 1];
    matches!(last, b'c' | b'n' | b'R' | b't' | b'x')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_in_one_chunk() {
        let mut tracker = ShellReadinessTracker::new("/bin/zsh");
        assert_eq!(tracker.state(), ReadyState::Pending);

        let result = tracker.scan_output(SHELL_READY_MARKER);
        assert!(result.became_ready);
        assert!(result.forward.is_empty()); // marker is stripped
        assert_eq!(tracker.state(), ReadyState::Ready);
    }

    #[test]
    fn marker_split_across_chunks() {
        let mut tracker = ShellReadinessTracker::new("/bin/bash");
        let mid = SHELL_READY_MARKER.len() / 2;

        let r1 = tracker.scan_output(&SHELL_READY_MARKER[..mid]);
        assert!(!r1.became_ready);
        assert!(r1.forward.is_empty()); // held bytes not yet forwarded

        let r2 = tracker.scan_output(&SHELL_READY_MARKER[mid..]);
        assert!(r2.became_ready);
        assert!(r2.forward.is_empty());
        assert_eq!(tracker.state(), ReadyState::Ready);
    }

    #[test]
    fn partial_match_then_mismatch() {
        let mut tracker = ShellReadinessTracker::new("/bin/zsh");
        // Feed partial marker followed by something else
        let mut data = SHELL_READY_MARKER[..5].to_vec();
        data.push(b'X');

        let result = tracker.scan_output(&data);
        assert!(!result.became_ready);
        // Held bytes + 'X' should all be forwarded
        assert_eq!(result.forward.len(), 6);
        assert_eq!(tracker.state(), ReadyState::Pending);
    }

    #[test]
    fn unsupported_shell_passes_through() {
        let mut tracker = ShellReadinessTracker::new("/bin/sh");
        assert_eq!(tracker.state(), ReadyState::Unsupported);

        let data = b"hello world";
        let result = tracker.scan_output(data);
        assert!(!result.became_ready);
        assert_eq!(result.forward, data.to_vec());
    }

    #[test]
    fn input_filtering_during_pending() {
        let mut tracker = ShellReadinessTracker::new("/bin/zsh");

        // DA1 response should be dropped (terminal query response)
        assert!(tracker.buffer_input(b"\x1b[?62;4c".to_vec()).is_none());
        // DSR response should be dropped
        assert!(tracker.buffer_input(b"\x1b[0n".to_vec()).is_none());
        // Normal input passes through immediately (no buffering)
        assert_eq!(
            tracker.buffer_input(b"ls\n".to_vec()),
            Some(b"ls\n".to_vec())
        );
        // Arrow keys pass through (user input, not query response)
        assert_eq!(
            tracker.buffer_input(b"\x1b[A".to_vec()),
            Some(b"\x1b[A".to_vec())
        );
        // Alt+key passes through (only 2 bytes)
        assert_eq!(
            tracker.buffer_input(b"\x1bx".to_vec()),
            Some(b"\x1bx".to_vec())
        );
    }

    #[test]
    fn osc_1337_triggers_ready() {
        let mut tracker = ShellReadinessTracker::new("/bin/zsh");
        assert_eq!(tracker.state(), ReadyState::Pending);

        let data = b"\x1b]1337;RemoteHost=user@host\x07";
        let result = tracker.scan_output(data);
        assert!(result.became_ready);
        assert_eq!(tracker.state(), ReadyState::Ready);
        assert_eq!(result.forward, data.to_vec());
    }

    #[test]
    fn osc_7_triggers_ready() {
        let mut tracker = ShellReadinessTracker::new("/bin/zsh");
        let data = b"\x1b]7;file:///home/user\x07";
        let result = tracker.scan_output(data);
        assert!(result.became_ready);
        assert_eq!(tracker.state(), ReadyState::Ready);
    }

    #[test]
    fn osc_133_triggers_ready() {
        let mut tracker = ShellReadinessTracker::new("/bin/bash");
        let data = b"\x1b]133;A\x07";
        let result = tracker.scan_output(data);
        assert!(result.became_ready);
        assert_eq!(tracker.state(), ReadyState::Ready);
    }

    #[test]
    fn terminal_query_response_detection() {
        // DA1 responses
        assert!(is_terminal_query_response(b"\x1b[?62;4;22c"));
        assert!(is_terminal_query_response(b"\x1b[?1;2c"));
        // DSR responses
        assert!(is_terminal_query_response(b"\x1b[0n"));
        // Cursor position reports
        assert!(is_terminal_query_response(b"\x1b[24;1R"));
        // NOT terminal responses:
        assert!(!is_terminal_query_response(b"\x1b[A")); // arrow key (3 bytes)
        assert!(!is_terminal_query_response(b"\x1bx")); // alt+key (2 bytes)
        assert!(!is_terminal_query_response(b"\x1b[15~")); // F5 (ends with ~)
        assert!(!is_terminal_query_response(b"hello")); // plain text
    }

    #[test]
    fn data_around_marker() {
        let mut tracker = ShellReadinessTracker::new("/bin/zsh");
        let mut data = b"before".to_vec();
        data.extend_from_slice(SHELL_READY_MARKER);
        data.extend_from_slice(b"after");

        let result = tracker.scan_output(&data);
        assert!(result.became_ready);
        // "before" and "after" should be forwarded, marker stripped
        let mut expected = b"before".to_vec();
        expected.extend_from_slice(b"after");
        assert_eq!(result.forward, expected);
        assert_eq!(tracker.state(), ReadyState::Ready);
    }
}
