use std::io::Write;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use libghostty_vt::render::{CellIterator, CursorVisualStyle, Dirty, RenderState, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::RgbColor;
use libghostty_vt::terminal::{Mode, ScrollViewport};
use libghostty_vt::{Terminal as GhosttyTerminal, TerminalOptions, focus, key, mouse, paste};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use smallvec::SmallVec;

use crate::config::TerminalConfig;
use crate::effects;

/// Terminal bounds in pixel and cell dimensions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerminalBounds {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: f32,
    pub line_height: f32,
}

impl Default for TerminalBounds {
    fn default() -> Self {
        Self {
            cols: 80,
            rows: 24,
            cell_width: 8.0,
            line_height: 16.0,
        }
    }
}

/// Width classification for terminal cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellWidthKind {
    /// Normal single-width character.
    Narrow,
    /// Wide character (CJK, emoji) occupying 2 columns.
    Wide,
    /// Spacer after a wide character — should not be rendered.
    SpacerTail,
    /// Spacer at end of soft-wrapped line for a wide character.
    SpacerHead,
}

/// Rendered cell data extracted from libghostty's render state.
#[derive(Debug, Clone)]
pub struct RenderedCell {
    pub col: u16,
    pub row: u16,
    pub graphemes: SmallVec<[char; 2]>,
    pub fg: RgbColor,
    pub bg: RgbColor,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub faint: bool,
    pub wide: CellWidthKind,
}

/// Cursor information for rendering.
#[derive(Debug, Clone, Copy)]
pub struct CursorInfo {
    pub col: u16,
    pub row: u16,
    pub visible: bool,
    pub blinking: bool,
    pub style: CursorVisualStyle,
    pub color: Option<RgbColor>,
    /// True when the cursor is on a wide character (render at 2x cell width).
    pub is_wide: bool,
}

impl Default for CursorInfo {
    fn default() -> Self {
        Self {
            col: 0,
            row: 0,
            visible: true,
            blinking: true,
            style: CursorVisualStyle::Block,
            color: None,
            is_wide: false,
        }
    }
}

/// Scrollbar state for rendering.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScrollbarState {
    pub total: u64,
    pub offset: u64,
    pub visible: u64,
}

impl ScrollbarState {
    pub fn has_scrollback(&self) -> bool {
        self.total > self.visible
    }

    /// Thumb position as fraction [0.0, 1.0] from top.
    pub fn thumb_top_fraction(&self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        self.offset as f32 / self.total as f32
    }

    /// Thumb height as fraction [0.0, 1.0] of total.
    pub fn thumb_height_fraction(&self) -> f32 {
        if self.total == 0 {
            return 1.0;
        }
        (self.visible as f32 / self.total as f32).clamp(0.02, 1.0)
    }
}

/// Terminal content snapshot for rendering. Produced by `Terminal::sync()`.
///
/// `cells` is row-major: `cells[row][col_run_idx]`. Inner Vec capacities are
/// preserved across `sync()` calls so that partial updates only re-fill the
/// rows that libghostty's render state marks dirty.
pub struct TerminalContent {
    pub cells: Vec<Vec<RenderedCell>>,
    pub cursor: CursorInfo,
    pub fg_color: RgbColor,
    pub bg_color: RgbColor,
    pub cursor_color: Option<RgbColor>,
    pub terminal_bounds: TerminalBounds,
    pub scrollbar: Option<ScrollbarState>,
    pub bell_count: u64,
    pub dirty_rows: Vec<u16>,
    pub content_generation: u64,
}

impl Default for TerminalContent {
    fn default() -> Self {
        Self {
            cells: Vec::new(),
            cursor: CursorInfo::default(),
            fg_color: RgbColor {
                r: 0xcd,
                g: 0xd6,
                b: 0xf4,
            },
            bg_color: RgbColor {
                r: 0x1e,
                g: 0x1e,
                b: 0x2e,
            },
            cursor_color: None,
            terminal_bounds: TerminalBounds::default(),
            scrollbar: None,
            bell_count: 0,
            dirty_rows: Vec::new(),
            content_generation: 0,
        }
    }
}

/// Selection tracking state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionPhase {
    Idle,
    Selecting,
    Ended,
}

/// The shared PTY writer handle, used by callbacks and input methods.
pub type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

struct SilentReplayGuard {
    pty_writer: SharedWriter,
    real_writer: Option<Box<dyn Write + Send>>,
    effect_state: Arc<Mutex<effects::TerminalEffectState>>,
    previous_suppression: Option<bool>,
}

impl SilentReplayGuard {
    fn new(
        pty_writer: SharedWriter,
        effect_state: Arc<Mutex<effects::TerminalEffectState>>,
    ) -> Self {
        let real_writer = {
            let mut writer = pty_writer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::replace(&mut *writer, Box::new(std::io::sink()))
        };
        let previous_suppression = if let Ok(mut state) = effect_state.lock() {
            let previous = Some(state.suppress_side_effects);
            state.suppress_side_effects = true;
            previous
        } else {
            None
        };

        Self {
            pty_writer,
            real_writer: Some(real_writer),
            effect_state,
            previous_suppression,
        }
    }
}

impl Drop for SilentReplayGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous_suppression
            && let Ok(mut state) = self.effect_state.lock()
        {
            state.suppress_side_effects = previous;
        }
        if let Some(real_writer) = self.real_writer.take() {
            let mut writer = self
                .pty_writer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *writer = real_writer;
        }
    }
}

// ── TerminalResizer trait ────────────────────────────────────

/// Abstraction for resizing the backing PTY or sending a resize message to the daemon.
pub trait TerminalResizer: Send {
    fn resize(&mut self, cols: u16, rows: u16, pixel_width: u32, pixel_height: u32) -> Result<()>;
}

/// Resizes a local PTY via the master handle.
struct PtyResizer {
    master: Box<dyn MasterPty + Send>,
}

impl TerminalResizer for PtyResizer {
    fn resize(&mut self, cols: u16, rows: u16, pixel_width: u32, pixel_height: u32) -> Result<()> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: pixel_width as u16,
                pixel_height: pixel_height as u16,
            })
            .context("PTY resize failed")
    }
}

/// No-op resizer for daemon-attached terminals (resize is sent via IPC separately).
pub struct DaemonResizer;

impl TerminalResizer for DaemonResizer {
    fn resize(
        &mut self,
        _cols: u16,
        _rows: u16,
        _pixel_width: u32,
        _pixel_height: u32,
    ) -> Result<()> {
        // Daemon client sends resize messages via IPC; nothing to do here.
        Ok(())
    }
}

// ── Terminal ─────────────────────────────────────────────────

/// Core terminal model. Owns libghostty terminal, render state, and an input abstraction.
///
/// Architecture follows Zed's Terminal pattern:
/// - async event loop batches PTY data (4ms)
/// - `sync()` produces TerminalContent for rendering
/// - Key/mouse encoding via libghostty encoders
pub struct Terminal {
    // Boxed to prevent moves — libghostty stores internal self-pointers
    // (e.g. for on_pty_write callback trampolines) that become dangling on move.
    ghostty: Box<GhosttyTerminal<'static, 'static>>,
    render_state: RenderState<'static>,
    row_iterator: RowIterator<'static>,
    cell_iterator: CellIterator<'static>,
    key_encoder: key::Encoder<'static>,
    mouse_encoder: mouse::Encoder<'static>,
    key_event: key::Event<'static>,
    mouse_event: mouse::Event<'static>,
    effect_state: Arc<Mutex<effects::TerminalEffectState>>,

    pty_writer: SharedWriter,
    resizer: Box<dyn TerminalResizer>,

    // Kept alive to prevent the child process from being killed (local PTY mode).
    #[allow(dead_code)]
    _child: Option<Box<dyn portable_pty::Child + Send + Sync>>,

    pub last_content: TerminalContent,

    selection_phase: SelectionPhase,
    breadcrumb_text: String,
    child_exited: Option<u32>,
}

impl Terminal {
    /// Try to handle a keystroke. Returns true if consumed.
    pub fn try_keystroke(
        &mut self,
        key: key::Key,
        mods: key::Mods,
        utf8: Option<&str>,
        unshifted: Option<char>,
    ) -> bool {
        self.key_event
            .set_action(key::Action::Press)
            .set_key(key)
            .set_mods(mods)
            .set_consumed_mods(key::Mods::empty())
            .set_composing(false)
            .set_unshifted_codepoint(unshifted.unwrap_or('\0'));

        if let Some(text) = utf8 {
            self.key_event.set_utf8(Some(text));
        } else {
            self.key_event.set_utf8(None::<String>);
        }

        self.key_encoder.set_options_from_terminal(&self.ghostty);

        let mut buf = Vec::new();
        if self
            .key_encoder
            .encode_to_vec(&self.key_event, &mut buf)
            .is_ok()
            && !buf.is_empty()
        {
            self.input(&buf);
            return true;
        }
        false
    }

    /// Write input to PTY, scrolling to bottom and clearing selection.
    pub fn input(&mut self, data: &[u8]) {
        self.ghostty.scroll_viewport(ScrollViewport::Bottom);
        self.selection_phase = SelectionPhase::Idle;
        self.write_to_pty(data);
    }

    /// Write raw bytes to PTY without scrolling to bottom.
    pub fn input_raw(&mut self, data: &[u8]) {
        self.write_to_pty(data);
    }

    /// Check whether Ghostty considers paste text safe.
    pub fn paste_is_safe(text: &str) -> bool {
        paste::is_safe(text)
    }

    /// Paste text using Ghostty's sanitizer and bracketed paste encoder.
    pub fn paste(&mut self, text: &str) {
        if !Self::paste_is_safe(text) {
            tracing::warn!("pasting text that Ghostty marks unsafe; sanitizer will be applied");
        }

        let bracketed = self.ghostty.mode(Mode::BRACKETED_PASTE).unwrap_or(false);
        let mut input = text.as_bytes().to_vec();
        let mut out = vec![0; input.len() + 32];

        let written = match paste::encode(&mut input, bracketed, &mut out) {
            Ok(written) => written,
            Err(libghostty_vt::Error::OutOfSpace { required }) => {
                out.resize(required, 0);
                match paste::encode(&mut input, bracketed, &mut out) {
                    Ok(written) => written,
                    Err(e) => {
                        tracing::warn!("ghostty paste encode failed after resize: {e}");
                        return;
                    }
                }
            }
            Err(e) => {
                tracing::warn!("ghostty paste encode failed: {e}");
                return;
            }
        };

        self.input(&out[..written]);
    }

    /// Feed PTY output data into the terminal emulator.
    pub fn feed_pty_data(&mut self, data: &[u8]) {
        self.ghostty.vt_write(data);
    }

    /// Feed PTY output data without sending VT responses back to the PTY.
    /// Used for scrollback replay where DA/DSR responses must be suppressed.
    pub fn feed_pty_data_silently(&mut self, data: &[u8]) {
        let _guard = SilentReplayGuard::new(self.pty_writer.clone(), self.effect_state.clone());
        self.ghostty.vt_write(data);
    }

    /// Scroll the viewport.
    pub fn scroll(&mut self, scroll: ScrollViewport) {
        self.ghostty.scroll_viewport(scroll);
    }

    /// Query the scrollbar state from libghostty and update last_content.
    /// Only call when scrollbar is visible (expensive per API docs).
    pub fn update_scrollbar(&mut self) {
        self.last_content.scrollbar = self.ghostty.scrollbar().ok().map(|sb| ScrollbarState {
            total: sb.total,
            offset: sb.offset,
            visible: sb.len,
        });
    }

    /// Resize terminal and (if local) PTY.
    pub fn resize(&mut self, bounds: TerminalBounds) -> Result<()> {
        self.last_content.terminal_bounds = bounds;
        if let Ok(mut state) = self.effect_state.lock() {
            state.set_size(bounds);
        }
        self.ghostty
            .resize(
                bounds.cols,
                bounds.rows,
                bounds.cell_width.round() as u32,
                bounds.line_height.round() as u32,
            )
            .map_err(|e| anyhow::anyhow!("ghostty resize failed: {e}"))?;
        let pixel_width = (bounds.cols as f32 * bounds.cell_width).round() as u32;
        let pixel_height = (bounds.rows as f32 * bounds.line_height).round() as u32;
        self.resizer
            .resize(bounds.cols, bounds.rows, pixel_width, pixel_height)?;
        Ok(())
    }

    /// Sync render state from libghostty and produce content for rendering.
    ///
    /// Honours libghostty's two-tier dirty tracking: the global
    /// `Dirty::{Clean, Partial, Full}` controls how much work to do per call,
    /// and per-row dirty flags let `Partial` updates leave clean rows untouched.
    /// `update()` itself must always be invoked because that's what consumes
    /// the *terminal* dirty state into the *render-state* dirty state.
    pub fn sync(&mut self) {
        let snapshot = match self.render_state.update(&self.ghostty) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("render_state.update failed: {e}");
                return;
            }
        };

        // Cursor + colors are cheap and live independently of the row iterator,
        // so update them on every sync — even on Dirty::False — so that cursor
        // moves that don't dirty cells (rare) still propagate.
        let cursor_vp = snapshot.cursor_viewport().ok().flatten();
        let (cursor_col, cursor_wide_tail) = cursor_vp
            .map(|c| {
                if c.at_wide_tail && c.x > 0 {
                    (c.x - 1, true)
                } else {
                    (c.x, false)
                }
            })
            .unwrap_or((0, false));
        self.last_content.cursor = CursorInfo {
            col: cursor_col,
            row: cursor_vp.map(|c| c.y).unwrap_or(0),
            visible: cursor_vp.is_some() && snapshot.cursor_visible().unwrap_or(true),
            blinking: snapshot.cursor_blinking().unwrap_or(true),
            style: snapshot
                .cursor_visual_style()
                .unwrap_or(CursorVisualStyle::Block),
            color: snapshot.cursor_color().ok().flatten(),
            is_wide: false, // resolved after cell extraction below
        };
        self.last_content.cursor_color = self.last_content.cursor.color;

        if let Ok(colors) = snapshot.colors() {
            self.last_content.fg_color = colors.foreground;
            self.last_content.bg_color = colors.background;
        }

        if let Ok(state) = self.effect_state.lock() {
            self.breadcrumb_text = state.title.clone();
            self.last_content.bell_count = state.bell_count;
        }

        // sync() always resets scrollbar to None — caller (e.g. tick()) is
        // responsible for fetching it via update_scrollbar() when needed.
        // Reset early so the Dirty::Clean fast-path stays consistent.
        self.last_content.scrollbar = None;
        self.last_content.dirty_rows.clear();

        let dirty_state = snapshot.dirty().unwrap_or(Dirty::Full);
        let rows_count = snapshot.rows().unwrap_or(0) as usize;

        // Resize detection: if libghostty reports a row count different from
        // our cached buffer, treat as a full re-extract regardless of the
        // reported dirty state. Partial updates can't safely splice rows when
        // the geometry itself shifted.
        let geometry_changed = self.last_content.cells.len() != rows_count;
        let dirty_full = matches!(dirty_state, Dirty::Full) || geometry_changed;
        let do_partial = matches!(dirty_state, Dirty::Partial) && !geometry_changed;
        let skip_rows = matches!(dirty_state, Dirty::Clean) && !geometry_changed;

        if skip_rows {
            // Nothing changed at row level. Snapshot dirty must still be reset
            // so the next render_state.update() returns a fresh dirty signal
            // instead of a stale snapshot.
            let _ = snapshot.set_dirty(Dirty::Clean);
            if self.last_content.cursor.visible {
                let cursor_row = self.last_content.cursor.row as usize;
                let cursor_col = self.last_content.cursor.col;
                let scanned_wide_cell = self
                    .last_content
                    .cells
                    .get(cursor_row)
                    .map(|row| {
                        row.iter()
                            .any(|c| c.col == cursor_col && c.wide == CellWidthKind::Wide)
                    })
                    .unwrap_or(false);
                self.last_content.cursor.is_wide = cursor_wide_tail || scanned_wide_cell;
            }
            return;
        }

        // Take ownership of the row buffer to mutate freely without borrow
        // conflicts against `self.row_iterator` / `self.cell_iterator`.
        // `mem::take` is O(1) — only the outer Vec's heap pointer moves;
        // inner Vec allocations come along and stay reusable.
        let mut cells = std::mem::take(&mut self.last_content.cells);
        cells.resize_with(rows_count, Vec::new);

        let fg_default = self.last_content.fg_color;
        let bg_default = self.last_content.bg_color;
        let mut touched_rows = false;

        if let Ok(mut row_iter) = self.row_iterator.update(&snapshot) {
            let mut row_idx: usize = 0;
            while let Some(row) = row_iter.next() {
                if row_idx >= cells.len() {
                    break;
                }
                let row_dirty = if dirty_full {
                    true
                } else if do_partial {
                    row.dirty().unwrap_or(true)
                } else {
                    // Should be unreachable given the early-return above, but
                    // keep the buffer untouched if we ever land here.
                    false
                };

                if !row_dirty {
                    row_idx += 1;
                    continue;
                }

                self.last_content.dirty_rows.push(row_idx as u16);
                touched_rows = true;
                let row_cells = &mut cells[row_idx];
                row_cells.clear();

                if let Ok(mut cell_iter) = self.cell_iterator.update(row) {
                    let mut col_idx: u16 = 0;
                    while let Some(cell) = cell_iter.next() {
                        let cell_wide = cell
                            .raw_cell()
                            .ok()
                            .and_then(|rc| rc.wide().ok())
                            .unwrap_or(CellWide::Narrow);
                        let wide = match cell_wide {
                            CellWide::Narrow => CellWidthKind::Narrow,
                            CellWide::Wide => CellWidthKind::Wide,
                            CellWide::SpacerTail => CellWidthKind::SpacerTail,
                            CellWide::SpacerHead => CellWidthKind::SpacerHead,
                        };

                        let grapheme_len = cell.graphemes_len().unwrap_or(0);

                        if grapheme_len == 0 {
                            let bg = cell.bg_color().ok().flatten().unwrap_or(bg_default);
                            row_cells.push(RenderedCell {
                                col: col_idx,
                                row: row_idx as u16,
                                graphemes: SmallVec::new(),
                                fg: fg_default,
                                bg,
                                bold: false,
                                italic: false,
                                underline: false,
                                strikethrough: false,
                                faint: false,
                                wide,
                            });
                            col_idx += 1;
                            continue;
                        }

                        let mut graphemes = SmallVec::<[char; 2]>::new();
                        graphemes.resize(grapheme_len, '\0');
                        let _ = cell.graphemes_buf(&mut graphemes);
                        let style = cell.style().ok();
                        let fg = cell.fg_color().ok().flatten().unwrap_or(fg_default);
                        let bg = cell.bg_color().ok().flatten().unwrap_or(bg_default);

                        // libghostty fg_color/bg_color don't apply inverse;
                        // swap here so RenderedCell has effective display colors.
                        let inverse = style.as_ref().map(|s| s.inverse).unwrap_or(false);
                        let (fg, bg) = if inverse { (bg, fg) } else { (fg, bg) };

                        row_cells.push(RenderedCell {
                            col: col_idx,
                            row: row_idx as u16,
                            graphemes,
                            fg,
                            bg,
                            bold: style.as_ref().map(|s| s.bold).unwrap_or(false),
                            italic: style.as_ref().map(|s| s.italic).unwrap_or(false),
                            underline: style
                                .as_ref()
                                .map(|s| {
                                    !matches!(s.underline, libghostty_vt::style::Underline::None)
                                })
                                .unwrap_or(false),
                            strikethrough: style.as_ref().map(|s| s.strikethrough).unwrap_or(false),
                            faint: style.as_ref().map(|s| s.faint).unwrap_or(false),
                            wide,
                        });
                        col_idx += 1;
                    }
                }
                let _ = row.set_dirty(false);
                row_idx += 1;
            }
        }
        // Signal ghostty that we've consumed the current state.
        // Without this, render_state.update() returns stale snapshots.
        let _ = snapshot.set_dirty(Dirty::Clean);

        // Cursor wide detection — only the cursor's row needs scanning.
        if self.last_content.cursor.visible {
            let cursor_row = self.last_content.cursor.row as usize;
            let cursor_col = self.last_content.cursor.col;
            let scanned_wide_cell = cells
                .get(cursor_row)
                .map(|row| {
                    row.iter()
                        .any(|c| c.col == cursor_col && c.wide == CellWidthKind::Wide)
                })
                .unwrap_or(false);
            self.last_content.cursor.is_wide = cursor_wide_tail || scanned_wide_cell;
        }

        if touched_rows || geometry_changed {
            self.last_content.content_generation =
                self.last_content.content_generation.saturating_add(1);
        }

        self.last_content.cells = cells;
    }

    /// Check if the terminal is in mouse tracking mode.
    pub fn is_mouse_tracking(&self) -> bool {
        self.ghostty.is_mouse_tracking().unwrap_or(false)
    }

    /// Check if the terminal is in alternate screen mode (vim, less, etc.).
    pub fn is_alternate_screen(&self) -> bool {
        use libghostty_vt::terminal::Mode;
        self.ghostty.mode(Mode::ALT_SCREEN_SAVE).unwrap_or(false)
            || self.ghostty.mode(Mode::ALT_SCREEN).unwrap_or(false)
            || self.ghostty.mode(Mode::ALT_SCREEN_LEGACY).unwrap_or(false)
    }

    /// Check if alternate scroll mode (DEC 1007) is enabled.
    pub fn is_alt_scroll(&self) -> bool {
        use libghostty_vt::terminal::Mode;
        self.ghostty.mode(Mode::ALT_SCROLL).unwrap_or(false)
    }

    /// Check if bracketed paste mode is enabled.
    pub fn is_bracketed_paste(&self) -> bool {
        self.ghostty
            .mode(libghostty_vt::terminal::Mode::BRACKETED_PASTE)
            .unwrap_or(false)
    }

    /// Encode and send a mouse event to the PTY.
    pub fn send_mouse_event(
        &mut self,
        action: mouse::Action,
        button: Option<mouse::Button>,
        mods: key::Mods,
        x: f32,
        y: f32,
    ) {
        self.mouse_event
            .set_action(action)
            .set_button(button)
            .set_mods(mods)
            .set_position(mouse::Position { x, y });

        self.mouse_encoder.set_options_from_terminal(&self.ghostty);

        let mut buf = Vec::new();
        if self
            .mouse_encoder
            .encode_to_vec(&self.mouse_event, &mut buf)
            .is_ok()
            && !buf.is_empty()
        {
            self.write_to_pty(&buf);
        }
    }

    /// Set the mouse encoder size (for pixel-to-cell conversion).
    pub fn set_mouse_size(&mut self, size: mouse::EncoderSize) {
        self.mouse_encoder.set_size(size);
    }

    /// Track whether any mouse button is currently pressed.
    pub fn set_mouse_any_button_pressed(&mut self, pressed: bool) {
        self.mouse_encoder.set_any_button_pressed(pressed);
    }

    /// Enable or disable mouse motion deduplication by last cell.
    pub fn set_mouse_track_last_cell(&mut self, enabled: bool) {
        self.mouse_encoder.set_track_last_cell(enabled);
    }

    /// Send focus in event.
    pub fn focus_in(&mut self) {
        if self.ghostty.mode(Mode::FOCUS_EVENT).unwrap_or(false) {
            let mut buf = [0u8; 8];
            if let Ok(written) = focus::Event::Gained.encode(&mut buf) {
                self.input_raw(&buf[..written]);
            }
        }
    }

    /// Send focus out event.
    pub fn focus_out(&mut self) {
        if self.ghostty.mode(Mode::FOCUS_EVENT).unwrap_or(false) {
            let mut buf = [0u8; 8];
            if let Ok(written) = focus::Event::Lost.encode(&mut buf) {
                self.input_raw(&buf[..written]);
            }
        }
    }

    pub fn breadcrumb_text(&self) -> &str {
        &self.breadcrumb_text
    }

    pub fn child_exited(&self) -> Option<u32> {
        self.child_exited
    }

    pub fn selection_phase(&self) -> SelectionPhase {
        self.selection_phase
    }

    pub fn set_selection_phase(&mut self, phase: SelectionPhase) {
        self.selection_phase = phase;
    }

    pub fn pty_writer(&self) -> &SharedWriter {
        &self.pty_writer
    }

    /// Replace the inner PTY writer. The ghostty callback shares the same Arc,
    /// so subsequent on_pty_write calls will use the new writer automatically.
    pub fn set_pty_writer(&self, writer: Box<dyn Write + Send>) {
        let mut w = self.pty_writer.lock().unwrap();
        *w = writer;
    }

    fn write_to_pty(&self, data: &[u8]) {
        if let Ok(mut writer) = self.pty_writer.lock() {
            let _ = writer.write_all(data);
            let _ = writer.flush();
        }
    }
}

// ── TerminalBuilder ──────────────────────────────────────────

/// Builder for constructing a Terminal with PTY and callbacks.
pub struct TerminalBuilder {
    config: TerminalConfig,
    shell: Option<String>,
    cwd: Option<std::path::PathBuf>,
}

impl TerminalBuilder {
    pub fn new(config: TerminalConfig) -> Self {
        Self {
            config,
            shell: None,
            cwd: None,
        }
    }

    pub fn shell(mut self, shell: impl Into<String>) -> Self {
        self.shell = Some(shell.into());
        self
    }

    pub fn cwd(mut self, cwd: impl Into<std::path::PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Build with a local PTY. Returns the Terminal and the PTY reader.
    pub fn build(self) -> Result<(Terminal, Box<dyn std::io::Read + Send>)> {
        let pty_system = native_pty_system();
        let pty_size = PtySize {
            rows: self.config.rows,
            cols: self.config.cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = pty_system.openpty(pty_size).context("Failed to open PTY")?;

        let shell = self
            .shell
            .unwrap_or_else(|| std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string()));

        let mut cmd = CommandBuilder::new(&shell);
        cmd.arg("-l");
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "seoul");

        if let Some(cwd) = &self.cwd {
            cmd.cwd(cwd);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .context("Failed to spawn shell")?;

        let writer = pair
            .master
            .take_writer()
            .context("Failed to take PTY writer")?;
        let reader = pair
            .master
            .try_clone_reader()
            .context("Failed to clone PTY reader")?;

        let pty_writer: SharedWriter = Arc::new(Mutex::new(writer));

        let (ghostty, helpers, effect_state) =
            Self::setup_ghostty(&self.config, pty_writer.clone())?;

        let terminal = Terminal {
            ghostty,
            render_state: helpers.render_state,
            row_iterator: helpers.row_iterator,
            cell_iterator: helpers.cell_iterator,
            key_encoder: helpers.key_encoder,
            mouse_encoder: helpers.mouse_encoder,
            key_event: helpers.key_event,
            mouse_event: helpers.mouse_event,
            effect_state,
            pty_writer,
            resizer: Box::new(PtyResizer {
                master: pair.master,
            }),
            _child: Some(child),
            last_content: TerminalContent::default(),
            selection_phase: SelectionPhase::Idle,
            breadcrumb_text: String::new(),
            child_exited: None,
        };

        Ok((terminal, reader))
    }

    /// Build attached to a daemon session. Input/output goes through the daemon socket.
    ///
    /// `daemon_writer` is a Write handle that sends data to the daemon.
    /// The caller is responsible for feeding PTY output into `terminal.feed_pty_data()`.
    pub fn build_attached(self, daemon_writer: Box<dyn Write + Send>) -> Result<Terminal> {
        let pty_writer: SharedWriter = Arc::new(Mutex::new(daemon_writer));
        let (ghostty, helpers, effect_state) =
            Self::setup_ghostty(&self.config, pty_writer.clone())?;

        let terminal = Terminal {
            ghostty,
            render_state: helpers.render_state,
            row_iterator: helpers.row_iterator,
            cell_iterator: helpers.cell_iterator,
            key_encoder: helpers.key_encoder,
            mouse_encoder: helpers.mouse_encoder,
            key_event: helpers.key_event,
            mouse_event: helpers.mouse_event,
            effect_state,
            pty_writer,
            resizer: Box::new(DaemonResizer),
            _child: None,
            last_content: TerminalContent::default(),
            selection_phase: SelectionPhase::Idle,
            breadcrumb_text: String::new(),
            child_exited: None,
        };

        Ok(terminal)
    }

    /// Common setup: create ghostty terminal and register callbacks.
    fn setup_ghostty(
        config: &TerminalConfig,
        pty_writer: SharedWriter,
    ) -> Result<(
        Box<GhosttyTerminal<'static, 'static>>,
        GhosttyHelpers,
        Arc<Mutex<effects::TerminalEffectState>>,
    )> {
        let mut ghostty = Box::new(
            GhosttyTerminal::new(TerminalOptions {
                cols: config.cols,
                rows: config.rows,
                max_scrollback: config.scrollback_lines,
            })
            .map_err(|e| anyhow::anyhow!("Failed to create ghostty terminal: {e}"))?,
        );

        let palette = config.theme.build_palette_256();
        let _ = ghostty.set_default_color_palette(Some(palette));
        let _ = ghostty.set_default_fg_color(Some(config.theme.foreground.to_ghostty()));
        let _ = ghostty.set_default_bg_color(Some(config.theme.background.to_ghostty()));
        let _ = ghostty.set_default_cursor_color(Some(config.theme.cursor.to_ghostty()));

        let effect_state = Arc::new(Mutex::new(effects::TerminalEffectState::new(
            config.cols,
            config.rows,
            8.0,
            16.0,
        )));

        let pty_writer_cb = pty_writer.clone();
        ghostty
            .on_pty_write(move |_terminal, data| {
                if let Ok(mut w) = pty_writer_cb.lock() {
                    let _ = w.write_all(data);
                    let _ = w.flush();
                }
            })
            .map_err(|e| anyhow::anyhow!("Failed to set pty_write callback: {e}"))?;

        let state_for_bell = effect_state.clone();
        ghostty
            .on_bell(move |_terminal| {
                if let Ok(mut state) = state_for_bell.lock()
                    && !state.suppress_side_effects
                {
                    state.bell_count = state.bell_count.saturating_add(1);
                }
            })
            .map_err(|e| anyhow::anyhow!("Failed to set bell callback: {e}"))?;

        let state_for_title = effect_state.clone();
        ghostty
            .on_title_changed(move |terminal| {
                if let Ok(mut state) = state_for_title.lock()
                    && !state.suppress_side_effects
                    && let Ok(title) = terminal.title()
                {
                    state.title.clear();
                    state.title.push_str(title);
                }
            })
            .map_err(|e| anyhow::anyhow!("Failed to set title callback: {e}"))?;

        let state_for_size = effect_state.clone();
        ghostty
            .on_size(move |_terminal| state_for_size.lock().ok().map(|state| state.size))
            .map_err(|e| anyhow::anyhow!("Failed to set size callback: {e}"))?;
        ghostty
            .on_enquiry(|_terminal| Some(effects::ENQUIRY_RESPONSE))
            .map_err(|e| anyhow::anyhow!("Failed to set enquiry callback: {e}"))?;
        ghostty
            .on_xtversion(|_terminal| Some(effects::XTVERSION_RESPONSE))
            .map_err(|e| anyhow::anyhow!("Failed to set xtversion callback: {e}"))?;
        ghostty
            .on_color_scheme(|_terminal| Some(effects::color_scheme()))
            .map_err(|e| anyhow::anyhow!("Failed to set color scheme callback: {e}"))?;
        ghostty
            .on_device_attributes(|_terminal| Some(effects::device_attributes()))
            .map_err(|e| anyhow::anyhow!("Failed to set device attributes callback: {e}"))?;

        let render_state = RenderState::new()
            .map_err(|e| anyhow::anyhow!("Failed to create render state: {e}"))?;
        let row_iterator = RowIterator::new()
            .map_err(|e| anyhow::anyhow!("Failed to create row iterator: {e}"))?;
        let cell_iterator = CellIterator::new()
            .map_err(|e| anyhow::anyhow!("Failed to create cell iterator: {e}"))?;
        let key_encoder = key::Encoder::new()
            .map_err(|e| anyhow::anyhow!("Failed to create key encoder: {e}"))?;
        let mouse_encoder = mouse::Encoder::new()
            .map_err(|e| anyhow::anyhow!("Failed to create mouse encoder: {e}"))?;
        let key_event =
            key::Event::new().map_err(|e| anyhow::anyhow!("Failed to create key event: {e}"))?;
        let mouse_event = mouse::Event::new()
            .map_err(|e| anyhow::anyhow!("Failed to create mouse event: {e}"))?;

        Ok((
            ghostty,
            GhosttyHelpers {
                render_state,
                row_iterator,
                cell_iterator,
                key_encoder,
                mouse_encoder,
                key_event,
                mouse_event,
            },
            effect_state,
        ))
    }
}

/// Intermediate struct to pass all the libghostty helper objects from setup_ghostty.
struct GhosttyHelpers {
    render_state: RenderState<'static>,
    row_iterator: RowIterator<'static>,
    cell_iterator: CellIterator<'static>,
    key_encoder: key::Encoder<'static>,
    mouse_encoder: mouse::Encoder<'static>,
    key_event: key::Event<'static>,
    mouse_event: mouse::Event<'static>,
}
