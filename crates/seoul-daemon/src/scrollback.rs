use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use uuid::Uuid;

use seoul_terminal_proto::paths;

/// Maximum scrollback file size on disk: 5 MB.
const MAX_SCROLLBACK_SIZE: usize = 5 * 1024 * 1024;

/// Maximum scrollback sent to clients on attach: 500 KB.
pub const MAX_SCROLLBACK_SEND_SIZE: usize = 500 * 1024;

/// Writes raw PTY output to disk for cold restore, and maintains an in-memory
/// ring buffer of the most recent output for fast warm-attach (no disk I/O).
///
/// When the file exceeds `MAX_SCROLLBACK_SIZE`, it is truncated to the most
/// recent half (keeping the tail). This keeps the file bounded while preserving
/// the most useful output for replay.
pub struct ScrollbackWriter {
    file: File,
    path: PathBuf,
    bytes_written: usize,
    /// In-memory ring buffer of recent PTY output (max `MAX_SCROLLBACK_SEND_SIZE`).
    /// Uses VecDeque for O(1) front-eviction on the hot path.
    ring_buffer: VecDeque<u8>,
}

impl ScrollbackWriter {
    pub fn new(session_id: Uuid) -> io::Result<Self> {
        let dir = paths::session_history_dir(session_id).map_err(io::Error::other)?;
        fs::create_dir_all(&dir)?;

        let path = paths::scrollback_path(session_id).map_err(io::Error::other)?;
        let file = OpenOptions::new().create(true).append(true).open(&path)?;

        let bytes_written = file.metadata().map(|m| m.len() as usize).unwrap_or(0);

        Ok(Self {
            file,
            path,
            bytes_written,
            ring_buffer: VecDeque::new(),
        })
    }

    pub fn write(&mut self, data: &[u8]) -> io::Result<()> {
        // Write to disk
        self.file.write_all(data)?;
        self.bytes_written += data.len();

        if self.bytes_written > MAX_SCROLLBACK_SIZE {
            self.truncate_to_tail()?;
        }

        // Write to in-memory ring buffer (prefix drain is cheap for Copy types)
        self.ring_buffer.extend(data.iter().copied());
        if self.ring_buffer.len() > MAX_SCROLLBACK_SEND_SIZE {
            let excess = self.ring_buffer.len() - MAX_SCROLLBACK_SEND_SIZE;
            self.ring_buffer.drain(..excess);
        }

        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }

    /// Get recent scrollback from the in-memory ring buffer (no disk I/O).
    /// Used for warm attach when the session is alive.
    /// Returns contiguous slices via `make_contiguous` (VecDeque may wrap internally).
    /// Skips any partial ANSI escape sequence at the start caused by ring buffer truncation.
    pub fn get_recent_scrollback(&mut self) -> &[u8] {
        let data = self.ring_buffer.make_contiguous();
        let safe = find_safe_start(data);
        &data[safe..]
    }

    /// Read scrollback from disk for cold restore, capped to send size.
    pub fn read_scrollback(session_id: Uuid) -> io::Result<Vec<u8>> {
        Self::read_scrollback_with_limit(session_id, MAX_SCROLLBACK_SEND_SIZE)
    }

    /// Read scrollback from disk, capped by the requested send size and the
    /// daemon's hard maximum.
    pub fn read_scrollback_with_limit(session_id: Uuid, limit: usize) -> io::Result<Vec<u8>> {
        let path = paths::scrollback_path(session_id).map_err(io::Error::other)?;
        if !path.exists() {
            return Ok(Vec::new());
        }
        let send_size = limit.min(MAX_SCROLLBACK_SEND_SIZE);
        if send_size == 0 {
            return Ok(Vec::new());
        }
        let mut file = File::open(&path)?;
        let len = file.metadata()?.len() as usize;

        if len <= send_size {
            let mut data = Vec::with_capacity(len);
            file.read_to_end(&mut data)?;
            return Ok(data);
        }

        file.seek(SeekFrom::End(-(send_size as i64)))?;
        let mut tail = vec![0u8; send_size];
        file.read_exact(&mut tail)?;
        let safe = find_safe_start(&tail);
        Ok(tail[safe..].to_vec())
    }

    /// Truncate to the most recent half of the file.
    fn truncate_to_tail(&mut self) -> io::Result<()> {
        let data = fs::read(&self.path)?;
        let keep_from = data.len() / 2;
        let safe = find_safe_start(&data[keep_from..]);
        let tail = &data[keep_from + safe..];

        // Rewrite with only the tail
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)?;
        file.write_all(tail)?;
        file.flush()?;

        self.bytes_written = tail.len();
        // Reopen in append mode
        self.file = OpenOptions::new().append(true).open(&self.path)?;

        Ok(())
    }
}

impl Drop for ScrollbackWriter {
    fn drop(&mut self) {
        self.flush().ok();
    }
}

/// Find the first safe byte offset in truncated scrollback data.
///
/// When scrollback is truncated at an arbitrary byte boundary, the first bytes
/// may be the tail of a partial ANSI escape sequence (e.g. `31m` from `\x1b[31m`)
/// or a partial UTF-8 character. Scanning forward to the first newline guarantees
/// we start at a line boundary where no sequence is in progress.
fn find_safe_start(data: &[u8]) -> usize {
    // Only scan a bounded prefix — if there's no newline in the first 4 KB,
    // the data is likely binary/unusual; fall back to the raw start.
    let limit = data.len().min(4096);
    for (i, &b) in data[..limit].iter().enumerate() {
        if b == b'\n' {
            return i + 1;
        }
    }
    0
}
