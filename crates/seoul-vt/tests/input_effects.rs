use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use libghostty_vt::{key, mouse};
use seoul_vt::config::TerminalConfig;
use seoul_vt::terminal::TerminalBounds;
use seoul_vt::{Terminal, TerminalBuilder};

#[derive(Clone, Default)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn terminal() -> (Terminal, Arc<Mutex<Vec<u8>>>) {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let writer = CaptureWriter(captured.clone());
    let terminal = TerminalBuilder::new(TerminalConfig {
        cols: 80,
        rows: 24,
        ..Default::default()
    })
    .build_attached(Box::new(writer))
    .unwrap();
    (terminal, captured)
}

fn take(captured: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
    std::mem::take(&mut *captured.lock().unwrap())
}

#[test]
fn paste_uses_ghostty_encoder() {
    let (mut term, captured) = terminal();

    term.feed_pty_data(b"\x1b[?2004h");
    term.paste("a\x1bb\x7fc\n");
    assert_eq!(take(&captured), b"\x1b[200~a b c\n\x1b[201~");

    term.feed_pty_data(b"\x1b[?2004l");
    term.paste("a\nb\r\nc\x03");
    assert_eq!(take(&captured), b"a\rb\r\rc ");
}

#[test]
fn focus_reporting_is_mode_gated() {
    let (mut term, captured) = terminal();

    term.focus_in();
    term.focus_out();
    assert!(take(&captured).is_empty());

    term.feed_pty_data(b"\x1b[?1004h");
    term.focus_in();
    term.focus_out();
    assert_eq!(take(&captured), b"\x1b[I\x1b[O");
}

#[test]
fn title_bell_queries_and_size_effects_work() {
    let (mut term, captured) = terminal();

    term.feed_pty_data(b"\x1b]2;Build\x07\x07");
    term.sync();
    assert_eq!(term.breadcrumb_text(), "Build");
    assert_eq!(term.last_content.bell_count, 1);

    term.feed_pty_data(b"\x05");
    assert_eq!(take(&captured), b"seoul");

    term.feed_pty_data(b"\x1b[>0q");
    assert_eq!(take(&captured), b"\x1bP>|seoul 0.1.0\x1b\\");

    term.resize(TerminalBounds {
        cols: 80,
        rows: 24,
        cell_width: 9.0,
        line_height: 18.0,
    })
    .unwrap();
    term.feed_pty_data(b"\x1b[18t");
    assert_eq!(take(&captured), b"\x1b[8;24;80t");
}

#[test]
fn silent_replay_does_not_mutate_user_visible_effects() {
    let (mut term, _captured) = terminal();

    term.feed_pty_data(b"\x1b]2;Live\x07\x07");
    term.sync();
    assert_eq!(term.breadcrumb_text(), "Live");
    assert_eq!(term.last_content.bell_count, 1);

    term.feed_pty_data_silently(b"\x1b]2;Replay\x07\x07");
    term.sync();
    assert_eq!(term.breadcrumb_text(), "Live");
    assert_eq!(term.last_content.bell_count, 1);
}

#[test]
fn wide_tail_cursor_is_rendered_at_wide_head() {
    let (mut term, _captured) = terminal();

    term.feed_pty_data("界".as_bytes());
    term.feed_pty_data(b"\x1b[1;2H");
    term.sync();

    assert_eq!(term.last_content.cursor.col, 0);
    assert!(term.last_content.cursor.is_wide);
}

#[test]
fn mouse_encoder_tracks_size_and_button_state() {
    let (mut term, captured) = terminal();

    term.feed_pty_data(b"\x1b[?1003h\x1b[?1006h");
    term.set_mouse_size(mouse::EncoderSize {
        screen_width: 800,
        screen_height: 400,
        cell_width: 10,
        cell_height: 20,
        padding_top: 0,
        padding_bottom: 0,
        padding_right: 0,
        padding_left: 0,
    });
    term.send_mouse_event(
        mouse::Action::Press,
        Some(mouse::Button::Left),
        key::Mods::empty(),
        15.0,
        25.0,
    );
    assert_eq!(take(&captured), b"\x1b[<0;2;2M");

    term.set_mouse_any_button_pressed(true);
    term.send_mouse_event(
        mouse::Action::Motion,
        Some(mouse::Button::Left),
        key::Mods::empty(),
        -1.0,
        -1.0,
    );

    assert_eq!(take(&captured), b"\x1b[<32;1;1M");
}
