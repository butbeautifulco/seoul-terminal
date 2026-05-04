//! Lightweight terminal mode tracker.
//!
//! Parses PTY output for DECSET/DECRST (`ESC[?...h`/`ESC[?...l`) mode changes
//! and OSC-7 (`ESC]7;file://...BEL`) CWD updates. Tracks 14 terminal modes
//! that affect input behavior and can generate rehydration escape sequences
//! to restore modes on warm attach.

const MAX_ESC_BUFFER_SIZE: usize = 1024;

/// The 14 terminal modes we track for rehydration.
#[derive(Debug, Clone)]
pub struct TerminalModes {
    pub application_cursor_keys: bool, // DECSET 1
    pub origin_mode: bool,             // DECSET 6
    pub auto_wrap: bool,               // DECSET 7  (default: true)
    pub cursor_visible: bool,          // DECSET 25 (default: true)
    pub mouse_x10: bool,               // DECSET 9
    pub mouse_normal: bool,            // DECSET 1000
    pub mouse_highlight: bool,         // DECSET 1001
    pub mouse_button_event: bool,      // DECSET 1002
    pub mouse_any_event: bool,         // DECSET 1003
    pub focus_reporting: bool,         // DECSET 1004
    pub mouse_utf8: bool,              // DECSET 1005
    pub mouse_sgr: bool,               // DECSET 1006
    pub alternate_screen: bool,        // DECSET 47/1049
    pub bracketed_paste: bool,         // DECSET 2004
}

impl Default for TerminalModes {
    fn default() -> Self {
        Self {
            application_cursor_keys: false,
            origin_mode: false,
            auto_wrap: true,
            cursor_visible: true,
            mouse_x10: false,
            mouse_normal: false,
            mouse_highlight: false,
            mouse_button_event: false,
            mouse_any_event: false,
            focus_reporting: false,
            mouse_utf8: false,
            mouse_sgr: false,
            alternate_screen: false,
            bracketed_paste: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ParseState {
    Normal,
    Escape,       // saw ESC
    Csi,          // saw ESC[
    CsiQuestion,  // saw ESC[?
    CsiDigits,    // saw ESC[?<digits> (accumulating mode numbers)
    Osc,          // saw ESC]
    OscDigit,     // saw ESC]<digit> (accumulating OSC number)
    OscSemicolon, // saw ESC]7; (reading OSC-7 content)
}

pub struct ModeTracker {
    modes: TerminalModes,
    cwd: Option<String>,
    state: ParseState,
    /// Buffer for accumulating digits in CSI or OSC sequences.
    digit_buf: Vec<u8>,
    /// Buffer for OSC-7 content (the URI).
    osc_content: Vec<u8>,
    /// OSC number being parsed.
    osc_number: u16,
    /// Tracks buffer length to prevent unbounded growth.
    seq_len: usize,
}

impl ModeTracker {
    pub fn new() -> Self {
        Self {
            modes: TerminalModes::default(),
            cwd: None,
            state: ParseState::Normal,
            digit_buf: Vec::new(),
            osc_content: Vec::new(),
            osc_number: 0,
            seq_len: 0,
        }
    }

    #[cfg(test)]
    pub fn modes(&self) -> &TerminalModes {
        &self.modes
    }

    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    /// Process a chunk of PTY output, updating tracked modes and CWD.
    pub fn process(&mut self, data: &[u8]) {
        for &byte in data {
            self.seq_len += 1;
            if self.seq_len > MAX_ESC_BUFFER_SIZE {
                self.reset_state();
                continue;
            }

            match self.state {
                ParseState::Normal => {
                    if byte == 0x1b {
                        self.state = ParseState::Escape;
                        self.seq_len = 1;
                    }
                }
                ParseState::Escape => match byte {
                    b'[' => self.state = ParseState::Csi,
                    b']' => {
                        self.state = ParseState::Osc;
                        self.digit_buf.clear();
                    }
                    _ => self.reset_state(),
                },
                ParseState::Csi => {
                    if byte == b'?' {
                        self.state = ParseState::CsiQuestion;
                        self.digit_buf.clear();
                    } else {
                        self.reset_state();
                    }
                }
                ParseState::CsiQuestion => {
                    if byte.is_ascii_digit() {
                        self.state = ParseState::CsiDigits;
                        self.digit_buf.clear();
                        self.digit_buf.push(byte);
                    } else {
                        self.reset_state();
                    }
                }
                ParseState::CsiDigits => {
                    if byte.is_ascii_digit() {
                        self.digit_buf.push(byte);
                    } else if byte == b';' {
                        // Multi-mode sequence: process current number, continue
                        // We don't know h/l yet, so just continue accumulating
                        self.digit_buf.push(byte);
                    } else if byte == b'h' || byte == b'l' {
                        let enabled = byte == b'h';
                        self.apply_modes(enabled);
                        self.reset_state();
                    } else {
                        self.reset_state();
                    }
                }
                ParseState::Osc => {
                    if byte.is_ascii_digit() {
                        self.digit_buf.push(byte);
                        self.state = ParseState::OscDigit;
                    } else {
                        self.reset_state();
                    }
                }
                ParseState::OscDigit => {
                    if byte.is_ascii_digit() {
                        self.digit_buf.push(byte);
                    } else if byte == b';' {
                        self.osc_number = parse_u16(&self.digit_buf);
                        self.osc_content.clear();
                        self.state = ParseState::OscSemicolon;
                    } else if byte == 0x07 || byte == 0x1b {
                        // BEL or ST terminator without content
                        self.reset_state();
                    } else {
                        self.reset_state();
                    }
                }
                ParseState::OscSemicolon => {
                    if byte == 0x07 {
                        // BEL terminates OSC
                        self.handle_osc();
                        self.reset_state();
                    } else if byte == 0x1b {
                        // Could be start of ST (ESC \), but also just handle it
                        self.handle_osc();
                        self.reset_state();
                    } else {
                        self.osc_content.push(byte);
                    }
                }
            }
        }
    }

    /// Generate ANSI escape sequences to restore non-default modes.
    pub fn generate_rehydrate_sequences(&self) -> Vec<u8> {
        let mut seq = Vec::new();
        let m = &self.modes;
        let d = TerminalModes::default();

        let mut emit = |mode_num: u16, current: bool, default: bool| {
            if current != default {
                seq.extend_from_slice(b"\x1b[?");
                seq.extend_from_slice(mode_num.to_string().as_bytes());
                seq.push(if current { b'h' } else { b'l' });
            }
        };

        emit(1, m.application_cursor_keys, d.application_cursor_keys);
        emit(6, m.origin_mode, d.origin_mode);
        emit(7, m.auto_wrap, d.auto_wrap);
        emit(9, m.mouse_x10, d.mouse_x10);
        emit(25, m.cursor_visible, d.cursor_visible);
        emit(1000, m.mouse_normal, d.mouse_normal);
        emit(1001, m.mouse_highlight, d.mouse_highlight);
        emit(1002, m.mouse_button_event, d.mouse_button_event);
        emit(1003, m.mouse_any_event, d.mouse_any_event);
        emit(1004, m.focus_reporting, d.focus_reporting);
        emit(1005, m.mouse_utf8, d.mouse_utf8);
        emit(1006, m.mouse_sgr, d.mouse_sgr);
        emit(1049, m.alternate_screen, d.alternate_screen);
        emit(2004, m.bracketed_paste, d.bracketed_paste);

        seq
    }

    fn apply_modes(&mut self, enabled: bool) {
        // digit_buf contains digits and semicolons, e.g. "1" or "1;2004"
        let buf = std::mem::take(&mut self.digit_buf);
        for part in buf.split(|&b| b == b';') {
            if part.is_empty() {
                continue;
            }
            let mode_num = parse_u16(part);
            self.set_mode(mode_num, enabled);
        }
    }

    fn set_mode(&mut self, mode: u16, enabled: bool) {
        match mode {
            1 => self.modes.application_cursor_keys = enabled,
            6 => self.modes.origin_mode = enabled,
            7 => self.modes.auto_wrap = enabled,
            9 => self.modes.mouse_x10 = enabled,
            25 => self.modes.cursor_visible = enabled,
            47 | 1047 | 1049 => self.modes.alternate_screen = enabled,
            1000 => self.modes.mouse_normal = enabled,
            1001 => self.modes.mouse_highlight = enabled,
            1002 => self.modes.mouse_button_event = enabled,
            1003 => self.modes.mouse_any_event = enabled,
            1004 => self.modes.focus_reporting = enabled,
            1005 => self.modes.mouse_utf8 = enabled,
            1006 => self.modes.mouse_sgr = enabled,
            2004 => self.modes.bracketed_paste = enabled,
            _ => {} // Untracked mode
        }
    }

    fn handle_osc(&mut self) {
        if self.osc_number == 7 {
            // OSC 7: file://hostname/path
            if let Ok(uri) = std::str::from_utf8(&self.osc_content)
                && let Some(path) = extract_osc7_path(uri)
            {
                self.cwd = Some(path);
            }
        }
    }

    fn reset_state(&mut self) {
        self.state = ParseState::Normal;
        self.seq_len = 0;
    }
}

/// Extract the path from an OSC-7 file URI.
/// Input: "file://hostname/path/to/dir" or "file:///path/to/dir"
/// Output: "/path/to/dir"
fn extract_osc7_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    // Skip hostname (everything before the first '/')
    let path = if let Some(idx) = rest.find('/') {
        &rest[idx..]
    } else {
        return None;
    };
    Some(percent_decode(path))
}

/// Simple percent-decoding for file paths.
fn percent_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_digit(bytes[i + 1]), hex_digit(bytes[i + 2]))
        {
            result.push((hi << 4 | lo) as char);
            i += 3;
            continue;
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn parse_u16(digits: &[u8]) -> u16 {
    let mut n: u16 = 0;
    for &d in digits {
        if d.is_ascii_digit() {
            n = n.saturating_mul(10).saturating_add((d - b'0') as u16);
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decset_single_mode() {
        let mut t = ModeTracker::new();
        assert!(!t.modes().bracketed_paste);

        t.process(b"\x1b[?2004h");
        assert!(t.modes().bracketed_paste);

        t.process(b"\x1b[?2004l");
        assert!(!t.modes().bracketed_paste);
    }

    #[test]
    fn decset_multi_mode() {
        let mut t = ModeTracker::new();
        t.process(b"\x1b[?1;2004h");
        assert!(t.modes().application_cursor_keys);
        assert!(t.modes().bracketed_paste);
    }

    #[test]
    fn decset_split_across_chunks() {
        let mut t = ModeTracker::new();
        t.process(b"\x1b[?20");
        assert!(!t.modes().bracketed_paste);
        t.process(b"04h");
        assert!(t.modes().bracketed_paste);
    }

    #[test]
    fn osc7_cwd() {
        let mut t = ModeTracker::new();
        t.process(b"\x1b]7;file://localhost/home/user\x07");
        assert_eq!(t.cwd(), Some("/home/user"));
    }

    #[test]
    fn osc7_percent_encoded() {
        let mut t = ModeTracker::new();
        t.process(b"\x1b]7;file://host/path%20with%20spaces\x07");
        assert_eq!(t.cwd(), Some("/path with spaces"));
    }

    #[test]
    fn rehydrate_non_default_modes() {
        let mut t = ModeTracker::new();
        t.process(b"\x1b[?1h"); // application cursor keys ON
        t.process(b"\x1b[?25l"); // cursor invisible
        t.process(b"\x1b[?2004h"); // bracketed paste ON

        let seq = t.generate_rehydrate_sequences();
        let s = String::from_utf8(seq).unwrap();
        assert!(s.contains("\x1b[?1h"));
        assert!(s.contains("\x1b[?25l"));
        assert!(s.contains("\x1b[?2004h"));
    }

    #[test]
    fn rehydrate_default_modes_empty() {
        let t = ModeTracker::new();
        let seq = t.generate_rehydrate_sequences();
        assert!(seq.is_empty());
    }

    #[test]
    fn alternate_screen_variants() {
        let mut t = ModeTracker::new();
        t.process(b"\x1b[?1049h");
        assert!(t.modes().alternate_screen);
        t.process(b"\x1b[?1049l");
        assert!(!t.modes().alternate_screen);

        t.process(b"\x1b[?47h");
        assert!(t.modes().alternate_screen);
    }

    #[test]
    fn normal_text_does_not_affect_modes() {
        let mut t = ModeTracker::new();
        let initial = t.modes().clone();
        t.process(b"Hello, world! Some normal terminal output.\r\n");
        // All modes should be unchanged
        assert_eq!(t.modes().bracketed_paste, initial.bracketed_paste);
        assert_eq!(t.modes().alternate_screen, initial.alternate_screen);
    }

    #[test]
    fn extract_osc7_path_works() {
        assert_eq!(
            extract_osc7_path("file://localhost/home/user"),
            Some("/home/user".to_string())
        );
        assert_eq!(
            extract_osc7_path("file:///tmp/test"),
            Some("/tmp/test".to_string())
        );
        assert_eq!(extract_osc7_path("http://example.com"), None);
    }
}
