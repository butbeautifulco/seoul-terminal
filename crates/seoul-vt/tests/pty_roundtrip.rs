use std::io::Write;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use seoul_vt::TerminalBuilder;
use seoul_vt::config::TerminalConfig;
use seoul_vt::terminal::TerminalBounds;

#[test]
fn test_pty_spawn_and_echo_roundtrip() {
    let config = TerminalConfig {
        cols: 80,
        rows: 24,
        ..Default::default()
    };

    let builder = TerminalBuilder::new(config).shell("/bin/sh");
    let (mut terminal, mut reader) = builder.build().expect("Failed to spawn terminal");

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match std::io::Read::read(&mut reader, &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Drain initial shell output
    drain_channel(&rx, &mut terminal, Duration::from_millis(500));

    // Write "echo hello" to PTY
    {
        let writer = terminal.pty_writer();
        let mut w = writer.lock().unwrap();
        w.write_all(b"echo hello\n").unwrap();
        w.flush().unwrap();
    }

    // Wait for output
    drain_channel(&rx, &mut terminal, Duration::from_secs(2));

    // Sync and check content
    terminal.sync();
    let content = &terminal.last_content;
    let screen_text: String = content
        .cells
        .iter()
        .flatten()
        .map(|cell| {
            if cell.graphemes.is_empty() {
                ' '
            } else {
                cell.graphemes[0]
            }
        })
        .collect();

    assert!(
        screen_text.contains("hello"),
        "Expected 'hello' in terminal output"
    );
}

#[test]
fn test_pty_resize() {
    let config = TerminalConfig {
        cols: 80,
        rows: 24,
        ..Default::default()
    };

    let builder = TerminalBuilder::new(config).shell("/bin/sh");
    let (mut terminal, _reader) = builder.build().expect("Failed to spawn terminal");

    terminal
        .resize(TerminalBounds {
            cols: 120,
            rows: 40,
            cell_width: 8.0,
            line_height: 16.0,
        })
        .expect("Failed to resize");

    assert_eq!(terminal.last_content.terminal_bounds.cols, 120);
    assert_eq!(terminal.last_content.terminal_bounds.rows, 40);
}

#[test]
fn test_feed_pty_data() {
    let config = TerminalConfig {
        cols: 80,
        rows: 24,
        ..Default::default()
    };

    let builder = TerminalBuilder::new(config).shell("/bin/sh");
    let (mut terminal, _reader) = builder.build().expect("Failed to spawn terminal");

    // Feed data directly (simulates daemon mode data)
    terminal.feed_pty_data(b"Hello, World!");
    terminal.sync();

    let content = &terminal.last_content;
    let first_row_text: String = content
        .cells
        .first()
        .map(|row| {
            row.iter()
                .map(|c| {
                    if c.graphemes.is_empty() {
                        ' '
                    } else {
                        c.graphemes[0]
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    assert!(
        first_row_text.contains("Hello, World!"),
        "Expected text in first row, got: '{}'",
        first_row_text.trim()
    );
}

fn drain_channel(
    rx: &mpsc::Receiver<Vec<u8>>,
    terminal: &mut seoul_vt::Terminal,
    timeout: Duration,
) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match rx.recv_timeout(remaining.min(Duration::from_millis(100))) {
            Ok(data) => terminal.feed_pty_data(&data),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                match rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(data) => terminal.feed_pty_data(&data),
                    Err(_) => break,
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}
