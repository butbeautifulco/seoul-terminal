use std::cell::Cell;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::*;
use unicode_segmentation::UnicodeSegmentation;

use crate::editor_buffer::{CursorPosition, EditorBuffer};
use crate::editor_element::{EditorRenderParams, render_editor_content};
use crate::syntax::SyntaxHighlighter;
use crate::theme;
use seoul_workspace::settings::SettingsStore;

// -- Actions --

actions!(
    editor,
    [
        Save,
        SelectAll,
        EditorCopy,
        EditorPaste,
        EditorCut,
        Backspace,
        Delete,
        Tab,
        MoveLeft,
        MoveRight,
        MoveUp,
        MoveDown,
        MoveToLineStart,
        MoveToLineEnd,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        MoveWordLeft,
        MoveWordRight,
    ]
);

// -- Events --

#[derive(Clone, Debug)]
pub enum EditorEvent {
    DirtyChanged {
        #[allow(dead_code)]
        is_dirty: bool,
    },
}

impl EventEmitter<EditorEvent> for EditorView {}

// -- Cursor blink epoch helper --

/// Monotonic epoch counter for cursor-blink timer callbacks.
///
/// Each `bump()` invalidates any in-flight timer scheduled with the
/// previous epoch — when its callback finally fires, `should_tick`
/// returns `false` and the callback no-ops. This is the same pattern
/// used by `terminal_view.rs` (see `bump_blink_epoch`/`tick_blink`).
#[derive(Default)]
struct BlinkState {
    epoch: u64,
}

impl BlinkState {
    fn bump(&mut self) -> u64 {
        self.epoch += 1;
        self.epoch
    }

    fn should_tick(&self, scheduled_epoch: u64) -> bool {
        scheduled_epoch == self.epoch
    }
}

// -- EditorView --

pub struct EditorView {
    pub file_path: PathBuf,
    buffer: EditorBuffer,
    dirty: bool,
    // Cursor & selection
    cursor: CursorPosition,
    selection_anchor: Option<CursorPosition>,
    desired_col: Option<usize>,
    is_selecting_with_mouse: bool,
    // IME
    ime_preedit: String,
    // Scroll
    scroll_offset: f32,
    // Visual
    focus_handle: FocusHandle,
    font_family: SharedString,
    font_size: f32,
    line_height: f32,
    gutter_width: f32,
    viewport_height: Rc<Cell<Option<f32>>>,
    // Syntax highlighting
    highlighter: SyntaxHighlighter,
    // Element bounds (captured during render for mouse hit-testing)
    element_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    // Cursor blink — see `show_cursor_now` and `tick_blink`.
    // `blink` epoch discriminates current vs. stale timer callbacks.
    cursor_blink_visible: bool,
    last_edit_epoch: std::time::Instant,
    last_blink_toggle: std::time::Instant,
    blink: BlinkState,
}

impl EditorView {
    pub fn new(cx: &mut Context<Self>, file_path: PathBuf) -> Self {
        let content = std::fs::read_to_string(&file_path).unwrap_or_default();
        let buffer = EditorBuffer::from_str(&content);
        let focus_handle = cx.focus_handle();

        let gutter_width = Self::compute_gutter_width(buffer.line_count());

        let mut highlighter = SyntaxHighlighter::new();
        highlighter.configure_for_file(&file_path);
        highlighter.parse(&content);

        let editor_settings = cx.global::<SettingsStore>().global().editor.clone();
        let font_size = editor_settings.font_size;

        cx.observe_global::<SettingsStore>(|this, cx| {
            let s = &cx.global::<SettingsStore>().global().editor;
            this.font_family = s.font_family.clone().into();
            this.font_size = s.font_size;
            this.line_height = s.font_size * 1.6;
            cx.notify();
        })
        .detach();

        let mut this = Self {
            file_path,
            buffer,
            dirty: false,
            cursor: CursorPosition::zero(),
            selection_anchor: None,
            desired_col: None,
            is_selecting_with_mouse: false,
            ime_preedit: String::new(),
            scroll_offset: 0.0,
            focus_handle,
            font_family: editor_settings.font_family.into(),
            font_size,
            line_height: font_size * 1.6,
            gutter_width,
            viewport_height: Rc::new(Cell::new(None)),
            highlighter,
            element_bounds: Rc::new(Cell::new(None)),
            cursor_blink_visible: true,
            last_edit_epoch: std::time::Instant::now(),
            last_blink_toggle: std::time::Instant::now(),
            blink: BlinkState::default(),
        };
        // Bootstrap the blink cycle: pauses for BLINK_PAUSE so the first
        // toggle fires at +BLINK_PAUSE, matching activity-driven behavior.
        this.show_cursor_now(cx);
        this
    }

    fn compute_gutter_width(line_count: usize) -> f32 {
        let digit_count = if line_count == 0 {
            1
        } else {
            (line_count as f32).log10().floor() as usize + 1
        };
        (digit_count as f32 + 2.0) * 8.0
    }

    fn update_gutter_width(&mut self) {
        self.gutter_width = Self::compute_gutter_width(self.buffer.line_count());
    }

    // -- Dirty state --

    fn mark_dirty(&mut self, cx: &mut Context<Self>) {
        if !self.dirty {
            self.dirty = true;
            cx.emit(EditorEvent::DirtyChanged { is_dirty: true });
        }
        self.show_cursor_now(cx);
    }

    fn mark_clean(&mut self, cx: &mut Context<Self>) {
        if self.dirty {
            self.dirty = false;
            cx.emit(EditorEvent::DirtyChanged { is_dirty: false });
        }
    }

    /// Cursor blink half-period: cursor toggles every BLINK_INTERVAL.
    const BLINK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

    /// Pause blinking for this duration after user-perceptible activity
    /// (keystroke, scroll, IME). Cursor stays visible during the pause;
    /// the next toggle fires once the pause window has elapsed.
    const BLINK_PAUSE: std::time::Duration = std::time::Duration::from_millis(500);

    /// Show the cursor immediately and start a fresh blink cycle.
    ///
    /// Called from any user-perceptible activity: keystrokes, edits,
    /// scroll, IME composition, mouse selection. Bumps the blink epoch
    /// so any in-flight timer becomes stale and no-ops on its callback,
    /// then schedules a fresh blink cycle to start after `BLINK_PAUSE`.
    ///
    /// Detached tasks are safe here: the epoch counter discriminates
    /// stale callbacks, and view drop turns `upgrade()` into None.
    fn show_cursor_now(&mut self, cx: &mut Context<Self>) {
        if !self.cursor_blink_visible {
            self.cursor_blink_visible = true;
            cx.notify();
        }
        self.last_edit_epoch = std::time::Instant::now();
        self.last_blink_toggle = self.last_edit_epoch;
        let epoch = self.blink.bump();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Self::BLINK_PAUSE).await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| this.tick_blink(epoch, cx));
            }
        })
        .detach();
    }

    /// Toggle cursor visibility and reschedule the next toggle.
    ///
    /// `epoch` is the epoch this callback was scheduled with; if the
    /// view's current epoch has advanced (e.g. another `show_cursor_now`
    /// fired in the meantime), this callback is stale and exits.
    fn tick_blink(&mut self, epoch: u64, cx: &mut Context<Self>) {
        if !self.blink.should_tick(epoch) {
            return;
        }
        if self.last_edit_epoch.elapsed() < Self::BLINK_PAUSE {
            return;
        }
        self.cursor_blink_visible = !self.cursor_blink_visible;
        self.last_blink_toggle = std::time::Instant::now();
        cx.notify();
        let next = self.blink.bump();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Self::BLINK_INTERVAL).await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| this.tick_blink(next, cx));
            }
        })
        .detach();
    }

    // -- Selection helpers --

    fn has_selection(&self) -> bool {
        self.selection_anchor.is_some_and(|a| a != self.cursor)
    }

    fn ordered_selection(&self) -> Option<(CursorPosition, CursorPosition)> {
        self.selection_anchor.and_then(|anchor| {
            if anchor == self.cursor {
                None
            } else if anchor < self.cursor {
                Some((anchor, self.cursor))
            } else {
                Some((self.cursor, anchor))
            }
        })
    }

    fn selection_byte_range(&self) -> Range<usize> {
        if let Some((start, end)) = self.ordered_selection() {
            self.buffer.cursor_to_byte(start)..self.buffer.cursor_to_byte(end)
        } else {
            let b = self.buffer.cursor_to_byte(self.cursor);
            b..b
        }
    }

    fn delete_selection_if_any(&mut self) -> bool {
        if let Some((start, end)) = self.ordered_selection() {
            let byte_range = self.buffer.cursor_to_byte(start)..self.buffer.cursor_to_byte(end);
            let edit = self.buffer.make_input_edit(byte_range.clone(), "");
            self.buffer.remove_byte_range(byte_range);
            self.highlighter.apply_edit(&edit);
            self.cursor = start;
            self.selection_anchor = None;
            true
        } else {
            self.selection_anchor = None;
            false
        }
    }

    // -- Text editing --

    fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.delete_selection_if_any();
        let byte_offset = self.buffer.cursor_to_byte(self.cursor);
        let edit = self.buffer.make_input_edit(byte_offset..byte_offset, text);
        self.buffer.insert_at_byte(byte_offset, text);
        self.highlighter.apply_edit(&edit);
        self.cursor = self.buffer.byte_to_cursor(byte_offset + text.len());
        self.desired_col = None;
        self.finalize_edit(cx);
    }

    /// Common post-edit ceremony: mark dirty, reparse, update gutter, scroll, notify.
    fn finalize_edit(&mut self, cx: &mut Context<Self>) {
        self.mark_dirty(cx);
        self.reparse_sync();
        self.update_gutter_width();
        self.ensure_cursor_visible();
        cx.notify();
    }

    fn reparse_sync(&mut self) {
        self.highlighter.reparse_with_rope(self.buffer.rope());
    }

    // -- Cursor movement --

    fn move_cursor(
        &mut self,
        new_pos: CursorPosition,
        extend_selection: bool,
        cx: &mut Context<Self>,
    ) {
        let clamped = self.buffer.clamp_cursor(new_pos);
        if extend_selection {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor);
            }
        } else {
            self.selection_anchor = None;
        }
        self.cursor = clamped;
        self.show_cursor_now(cx);
        self.ensure_cursor_visible();
        cx.notify();
    }

    fn next_grapheme_boundary(&self, pos: CursorPosition) -> CursorPosition {
        let line_text = self.buffer.line_text(pos.row);
        let col = pos.col;
        if col >= line_text.len() {
            // Move to next line
            if pos.row + 1 < self.buffer.line_count() {
                return CursorPosition {
                    row: pos.row + 1,
                    col: 0,
                };
            }
            return pos;
        }
        let remainder = &line_text[col..];
        let mut graphemes = remainder.grapheme_indices(true);
        graphemes.next(); // skip current
        if let Some((offset, _)) = graphemes.next() {
            CursorPosition {
                row: pos.row,
                col: col + offset,
            }
        } else {
            CursorPosition {
                row: pos.row,
                col: line_text.len(),
            }
        }
    }

    fn prev_grapheme_boundary(&self, pos: CursorPosition) -> CursorPosition {
        if pos.col == 0 {
            if pos.row > 0 {
                let prev_line_len = self.buffer.line_len_bytes(pos.row - 1);
                return CursorPosition {
                    row: pos.row - 1,
                    col: prev_line_len,
                };
            }
            return pos;
        }
        let line_text = self.buffer.line_text(pos.row);
        let prefix = &line_text[..pos.col];
        if let Some((idx, _)) = prefix.grapheme_indices(true).next_back() {
            CursorPosition {
                row: pos.row,
                col: idx,
            }
        } else {
            CursorPosition {
                row: pos.row,
                col: 0,
            }
        }
    }

    fn next_word_boundary(&self, pos: CursorPosition) -> CursorPosition {
        let line_text = self.buffer.line_text(pos.row);
        if pos.col >= line_text.len() {
            if pos.row + 1 < self.buffer.line_count() {
                return CursorPosition {
                    row: pos.row + 1,
                    col: 0,
                };
            }
            return pos;
        }
        let rest = &line_text[pos.col..];
        let words: Vec<(usize, &str)> = rest.split_word_bound_indices().collect();
        // Skip current word boundary, find next one
        let mut offset = 0;
        let mut found_word = false;
        for (idx, word) in &words {
            offset = *idx + word.len();
            if !found_word && word.chars().any(|c| c.is_alphanumeric() || c == '_') {
                found_word = true;
            } else if found_word {
                offset = *idx;
                break;
            }
        }
        CursorPosition {
            row: pos.row,
            col: pos.col + offset,
        }
    }

    fn prev_word_boundary(&self, pos: CursorPosition) -> CursorPosition {
        if pos.col == 0 {
            if pos.row > 0 {
                let prev_len = self.buffer.line_len_bytes(pos.row - 1);
                return CursorPosition {
                    row: pos.row - 1,
                    col: prev_len,
                };
            }
            return pos;
        }
        let line_text = self.buffer.line_text(pos.row);
        let prefix = &line_text[..pos.col];
        let words: Vec<(usize, &str)> = prefix.split_word_bound_indices().collect();
        // Walk backwards to find previous word start
        let mut idx = pos.col;
        let mut found_word = false;
        for (word_idx, word) in words.iter().rev() {
            if !found_word && word.chars().any(|c| c.is_alphanumeric() || c == '_') {
                found_word = true;
                idx = *word_idx;
            } else if found_word {
                break;
            } else {
                idx = *word_idx;
            }
        }
        CursorPosition {
            row: pos.row,
            col: idx,
        }
    }

    // -- Scroll --

    fn max_scroll(&self) -> f32 {
        let viewport_h = self.viewport_height.get().unwrap_or(400.0);
        let total_h = self.buffer.line_count() as f32 * self.line_height;
        (total_h - viewport_h).max(0.0)
    }

    fn clamp_scroll(&mut self) {
        let max = self.max_scroll();
        self.scroll_offset = self.scroll_offset.clamp(0.0, max);
    }

    fn ensure_cursor_visible(&mut self) {
        let cursor_y = self.cursor.row as f32 * self.line_height;
        let viewport_h = self.viewport_height.get().unwrap_or(400.0);

        if cursor_y < self.scroll_offset {
            self.scroll_offset = cursor_y;
        } else if cursor_y + self.line_height > self.scroll_offset + viewport_h {
            self.scroll_offset = cursor_y + self.line_height - viewport_h;
        }
        self.clamp_scroll();
    }

    // -- Mouse → cursor position --

    fn cursor_for_pixel(
        &self,
        position: gpui::Point<Pixels>,
        bounds_origin: gpui::Point<Pixels>,
    ) -> CursorPosition {
        let x: f32 = (position.x - bounds_origin.x).into();
        let y: f32 = (position.y - bounds_origin.y).into();

        let row = ((y + self.scroll_offset) / self.line_height) as usize;
        let row = row.min(self.buffer.line_count().saturating_sub(1));

        // Approximate column from x position
        // Code area starts after gutter
        let code_x = (x - self.gutter_width - 8.0).max(0.0);
        let approx_char_width = self.font_size * 0.6; // monospace approximation
        let approx_col = (code_x / approx_char_width) as usize;

        // Clamp to actual line byte length using grapheme boundaries
        let line_text = self.buffer.line_text(row);
        let mut byte_col = 0;
        for (i, (idx, _)) in line_text.grapheme_indices(true).enumerate() {
            byte_col = idx;
            if i >= approx_col {
                break;
            }
            byte_col = idx
                + line_text[idx..]
                    .graphemes(true)
                    .next()
                    .map_or(0, |g| g.len());
        }
        byte_col = byte_col.min(line_text.len());

        CursorPosition { row, col: byte_col }
    }

    // -- Action handlers --

    fn save(&mut self, _: &Save, _window: &mut Window, cx: &mut Context<Self>) {
        let text = self.buffer.contents();
        if let Err(e) = std::fs::write(&self.file_path, &text) {
            tracing::error!("failed to save {}: {e}", self.file_path.display());
        } else {
            self.mark_clean(cx);
        }
        cx.notify();
    }

    fn select_all(&mut self, _: &SelectAll, _window: &mut Window, cx: &mut Context<Self>) {
        self.selection_anchor = Some(CursorPosition::zero());
        let last_line = self.buffer.line_count().saturating_sub(1);
        let last_col = self.buffer.line_len_bytes(last_line);
        self.cursor = CursorPosition {
            row: last_line,
            col: last_col,
        };
        cx.notify();
    }

    fn editor_copy(&mut self, _: &EditorCopy, _window: &mut Window, cx: &mut Context<Self>) {
        if self.has_selection() {
            let range = self.selection_byte_range();
            let text = self.buffer.contents();
            let selected = &text[range];
            cx.write_to_clipboard(ClipboardItem::new_string(selected.to_string()));
        }
    }

    fn editor_paste(&mut self, _: &EditorPaste, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.insert_text(&text, cx);
        }
    }

    fn editor_cut(&mut self, _: &EditorCut, _window: &mut Window, cx: &mut Context<Self>) {
        if self.has_selection() {
            let range = self.selection_byte_range();
            let text = self.buffer.contents();
            let selected = &text[range];
            cx.write_to_clipboard(ClipboardItem::new_string(selected.to_string()));
            self.delete_selection_if_any();
            self.finalize_edit(cx);
        }
    }

    fn backspace(&mut self, _: &Backspace, _window: &mut Window, cx: &mut Context<Self>) {
        if self.delete_selection_if_any() {
            self.finalize_edit(cx);
            return;
        }
        let prev = self.prev_grapheme_boundary(self.cursor);
        if prev == self.cursor {
            return;
        }
        let start_byte = self.buffer.cursor_to_byte(prev);
        let end_byte = self.buffer.cursor_to_byte(self.cursor);
        let edit = self.buffer.make_input_edit(start_byte..end_byte, "");
        self.buffer.remove_byte_range(start_byte..end_byte);
        self.highlighter.apply_edit(&edit);
        self.cursor = prev;
        self.desired_col = None;
        self.finalize_edit(cx);
    }

    fn delete(&mut self, _: &Delete, _window: &mut Window, cx: &mut Context<Self>) {
        if self.delete_selection_if_any() {
            self.finalize_edit(cx);
            return;
        }
        let next = self.next_grapheme_boundary(self.cursor);
        if next == self.cursor {
            return;
        }
        let start_byte = self.buffer.cursor_to_byte(self.cursor);
        let end_byte = self.buffer.cursor_to_byte(next);
        let edit = self.buffer.make_input_edit(start_byte..end_byte, "");
        self.buffer.remove_byte_range(start_byte..end_byte);
        self.highlighter.apply_edit(&edit);
        self.desired_col = None;
        self.finalize_edit(cx);
    }

    fn tab(&mut self, _: &Tab, _window: &mut Window, cx: &mut Context<Self>) {
        self.insert_text("    ", cx);
        cx.notify();
    }

    // -- Cursor movement actions --

    fn move_left(&mut self, _: &MoveLeft, _window: &mut Window, cx: &mut Context<Self>) {
        if self.has_selection() {
            let (start, _) = self.ordered_selection().unwrap();
            self.move_cursor(start, false, cx);
        } else {
            let prev = self.prev_grapheme_boundary(self.cursor);
            self.move_cursor(prev, false, cx);
        }
        self.desired_col = None;
    }

    fn move_right(&mut self, _: &MoveRight, _window: &mut Window, cx: &mut Context<Self>) {
        if self.has_selection() {
            let (_, end) = self.ordered_selection().unwrap();
            self.move_cursor(end, false, cx);
        } else {
            let next = self.next_grapheme_boundary(self.cursor);
            self.move_cursor(next, false, cx);
        }
        self.desired_col = None;
    }

    fn move_up(&mut self, _: &MoveUp, _window: &mut Window, cx: &mut Context<Self>) {
        if self.cursor.row == 0 {
            self.move_cursor(CursorPosition { row: 0, col: 0 }, false, cx);
            return;
        }
        let col = self.desired_col.unwrap_or(self.cursor.col);
        let new_row = self.cursor.row - 1;
        let max_col = self.buffer.line_len_bytes(new_row);
        self.desired_col = Some(col);
        self.move_cursor(
            CursorPosition {
                row: new_row,
                col: col.min(max_col),
            },
            false,
            cx,
        );
    }

    fn move_down(&mut self, _: &MoveDown, _window: &mut Window, cx: &mut Context<Self>) {
        if self.cursor.row + 1 >= self.buffer.line_count() {
            let last_col = self.buffer.line_len_bytes(self.cursor.row);
            self.move_cursor(
                CursorPosition {
                    row: self.cursor.row,
                    col: last_col,
                },
                false,
                cx,
            );
            return;
        }
        let col = self.desired_col.unwrap_or(self.cursor.col);
        let new_row = self.cursor.row + 1;
        let max_col = self.buffer.line_len_bytes(new_row);
        self.desired_col = Some(col);
        self.move_cursor(
            CursorPosition {
                row: new_row,
                col: col.min(max_col),
            },
            false,
            cx,
        );
    }

    fn move_to_line_start(
        &mut self,
        _: &MoveToLineStart,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_cursor(
            CursorPosition {
                row: self.cursor.row,
                col: 0,
            },
            false,
            cx,
        );
        self.desired_col = None;
    }

    fn move_to_line_end(
        &mut self,
        _: &MoveToLineEnd,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let max_col = self.buffer.line_len_bytes(self.cursor.row);
        self.move_cursor(
            CursorPosition {
                row: self.cursor.row,
                col: max_col,
            },
            false,
            cx,
        );
        self.desired_col = None;
    }

    fn select_left(&mut self, _: &SelectLeft, _window: &mut Window, cx: &mut Context<Self>) {
        let prev = self.prev_grapheme_boundary(self.cursor);
        self.move_cursor(prev, true, cx);
        self.desired_col = None;
    }

    fn select_right(&mut self, _: &SelectRight, _window: &mut Window, cx: &mut Context<Self>) {
        let next = self.next_grapheme_boundary(self.cursor);
        self.move_cursor(next, true, cx);
        self.desired_col = None;
    }

    fn select_up(&mut self, _: &SelectUp, _window: &mut Window, cx: &mut Context<Self>) {
        if self.cursor.row == 0 {
            return;
        }
        let col = self.desired_col.unwrap_or(self.cursor.col);
        let new_row = self.cursor.row - 1;
        let max_col = self.buffer.line_len_bytes(new_row);
        self.desired_col = Some(col);
        self.move_cursor(
            CursorPosition {
                row: new_row,
                col: col.min(max_col),
            },
            true,
            cx,
        );
    }

    fn select_down(&mut self, _: &SelectDown, _window: &mut Window, cx: &mut Context<Self>) {
        if self.cursor.row + 1 >= self.buffer.line_count() {
            return;
        }
        let col = self.desired_col.unwrap_or(self.cursor.col);
        let new_row = self.cursor.row + 1;
        let max_col = self.buffer.line_len_bytes(new_row);
        self.desired_col = Some(col);
        self.move_cursor(
            CursorPosition {
                row: new_row,
                col: col.min(max_col),
            },
            true,
            cx,
        );
    }

    fn move_word_left(&mut self, _: &MoveWordLeft, _window: &mut Window, cx: &mut Context<Self>) {
        let prev = self.prev_word_boundary(self.cursor);
        self.move_cursor(prev, false, cx);
        self.desired_col = None;
    }

    fn move_word_right(&mut self, _: &MoveWordRight, _window: &mut Window, cx: &mut Context<Self>) {
        let next = self.next_word_boundary(self.cursor);
        self.move_cursor(next, false, cx);
        self.desired_col = None;
    }

    // -- Mouse handlers --

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.is_selecting_with_mouse = true;
        window.focus(&self.focus_handle, cx);
        let bounds_origin = self
            .element_bounds
            .get()
            .map(|b| b.origin)
            .unwrap_or(point(Pixels::ZERO, Pixels::ZERO));
        let new_pos = self.cursor_for_pixel(event.position, bounds_origin);
        if event.modifiers.shift {
            self.move_cursor(new_pos, true, cx);
        } else {
            self.move_cursor(new_pos, false, cx);
        }
        self.desired_col = None;
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_selecting_with_mouse {
            let bounds_origin = self
                .element_bounds
                .get()
                .map(|b| b.origin)
                .unwrap_or(point(Pixels::ZERO, Pixels::ZERO));
            let new_pos = self.cursor_for_pixel(event.position, bounds_origin);
            self.move_cursor(new_pos, true, cx);
        }
    }

    fn on_mouse_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.is_selecting_with_mouse = false;
    }

    fn on_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta = match event.delta {
            ScrollDelta::Lines(pt) => pt.y * self.line_height,
            ScrollDelta::Pixels(pt) => f32::from(pt.y),
        };
        self.scroll_offset -= delta;
        self.clamp_scroll();
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key.as_str() == "enter" {
            self.insert_text("\n", cx);
            cx.notify();
        }
    }

    // -- InputHandler byte-range helpers --

    fn marked_byte_range(&self) -> Option<Range<usize>> {
        if self.ime_preedit.is_empty() {
            return None;
        }
        let cursor_byte = self.buffer.cursor_to_byte(self.cursor);
        Some(cursor_byte..cursor_byte + self.ime_preedit.len())
    }
}

// -- EntityInputHandler --

impl EntityInputHandler for EditorView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let start = self.buffer.utf16_to_byte(range_utf16.start);
        let end = self.buffer.utf16_to_byte(range_utf16.end);
        let text = self.buffer.contents();
        let start = start.min(text.len());
        let end = end.min(text.len());
        actual_range.replace(self.buffer.byte_to_utf16(start)..self.buffer.byte_to_utf16(end));
        Some(text[start..end].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let range = self.selection_byte_range();
        Some(UTF16Selection {
            range: self.buffer.byte_to_utf16(range.start)..self.buffer.byte_to_utf16(range.end),
            reversed: self.selection_anchor.is_some_and(|a| a > self.cursor),
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        if self.ime_preedit.is_empty() {
            return None;
        }
        let cursor_byte = self.buffer.cursor_to_byte(self.cursor);
        let start_utf16 = self.buffer.byte_to_utf16(cursor_byte);
        let end_utf16 = start_utf16 + self.ime_preedit.encode_utf16().count();
        Some(start_utf16..end_utf16)
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.ime_preedit.is_empty() {
            let preedit = std::mem::take(&mut self.ime_preedit);
            self.insert_text(&preedit, cx);
        }
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.buffer.utf16_to_byte(r.start)..self.buffer.utf16_to_byte(r.end))
            .or_else(|| self.marked_byte_range())
            .unwrap_or_else(|| self.selection_byte_range());

        // Remove IME preedit from buffer if it was inserted
        if !self.ime_preedit.is_empty() {
            self.ime_preedit.clear();
        }

        let edit = self.buffer.make_input_edit(range.clone(), text);
        self.buffer.remove_byte_range(range.clone());
        self.buffer.insert_at_byte(range.start, text);
        self.highlighter.apply_edit(&edit);

        self.cursor = self.buffer.byte_to_cursor(range.start + text.len());
        self.selection_anchor = None;
        self.desired_col = None;

        if !text.is_empty() || !range.is_empty() {
            self.finalize_edit(cx);
        } else {
            cx.notify();
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.buffer.utf16_to_byte(r.start)..self.buffer.utf16_to_byte(r.end))
            .or_else(|| self.marked_byte_range())
            .unwrap_or_else(|| self.selection_byte_range());

        // Remove old preedit/selection
        if !range.is_empty() {
            let edit = self.buffer.make_input_edit(range.clone(), new_text);
            self.buffer.remove_byte_range(range.clone());
            self.buffer.insert_at_byte(range.start, new_text);
            self.highlighter.apply_edit(&edit);
        } else {
            let cursor_byte = self.buffer.cursor_to_byte(self.cursor);
            let edit = self
                .buffer
                .make_input_edit(cursor_byte..cursor_byte, new_text);
            self.buffer.insert_at_byte(cursor_byte, new_text);
            self.highlighter.apply_edit(&edit);
        }

        self.ime_preedit = new_text.to_string();
        self.selection_anchor = None;

        self.reparse_sync();
        self.show_cursor_now(cx);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let cursor_y = self.cursor.row as f32 * self.line_height - self.scroll_offset;
        let code_x_offset = self.gutter_width + 8.0;

        // Measure actual text width up to cursor using text shaping
        let line_text = self.buffer.line_text(self.cursor.row);
        let col = self.cursor.col.min(line_text.len());
        let cursor_x = if !line_text.is_empty() && col > 0 {
            let base_font = Font {
                family: self.font_family.clone(),
                weight: FontWeight::default(),
                style: FontStyle::Normal,
                features: FontFeatures::default(),
                fallbacks: None,
            };
            let run = TextRun {
                len: line_text.len(),
                font: base_font,
                color: Hsla::from(rgba(theme::opaque(theme::theme(cx).text))),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let shaped = window.text_system().shape_line(
                SharedString::from(line_text),
                px(self.font_size),
                &[run],
                None,
            );
            let x: f32 = shaped.x_for_index(col).into();
            code_x_offset + x
        } else {
            code_x_offset
        };

        Some(Bounds::new(
            point(
                element_bounds.origin.x + px(cursor_x),
                element_bounds.origin.y + px(cursor_y),
            ),
            size(px(self.font_size * 0.6), px(self.line_height)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

// -- Focusable --

impl crate::item::Item for EditorView {
    fn tab_title(&self, _cx: &App) -> String {
        let name = self
            .file_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".into());
        if self.dirty {
            format!("{name} \u{2022}")
        } else {
            name
        }
    }

    fn tab_kind_id(&self) -> &'static str {
        "editor"
    }

    fn is_dirty(&self) -> bool {
        self.dirty
    }

    fn can_save(&self) -> bool {
        true
    }
}

impl Focusable for EditorView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

// -- Render --

impl Render for EditorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Build snapshot data for rendering (Zed snapshot pattern — avoid borrow conflicts)
        let line_count = self.buffer.line_count();
        let cursor = self.cursor;
        let selection = self.ordered_selection();
        let scroll_offset = self.scroll_offset;
        let font_family = self.font_family.clone();
        let font_size = self.font_size;
        let line_height = self.line_height;
        let gutter_width = self.gutter_width;
        let viewport_height = self.viewport_height.clone();
        let cursor_blink_visible = self.cursor_blink_visible;
        let focus_handle = self.focus_handle.clone();
        let view_entity = cx.entity();
        let ime_preedit = self.ime_preedit.clone();

        // Compute visible line range for highlight query
        let viewport_h = self.viewport_height.get().unwrap_or(400.0);
        let first_line = (scroll_offset / line_height) as usize;
        let visible_count = (viewport_h / line_height).ceil() as usize + 2;
        let last_line = (first_line + visible_count).min(line_count);

        // Collect visible line texts and byte offsets
        let mut visible_lines: Vec<String> = Vec::with_capacity(last_line - first_line);
        let mut line_byte_offsets: Vec<(usize, usize)> = Vec::with_capacity(last_line - first_line);
        for line_idx in first_line..last_line {
            let text = self.buffer.line_text(line_idx);
            let start = self.buffer.line_to_byte(line_idx);
            let end = start + text.len();
            line_byte_offsets.push((start, end));
            visible_lines.push(text);
        }

        // Get highlight spans for visible range
        let visible_byte_range = if !line_byte_offsets.is_empty() {
            line_byte_offsets[0].0..line_byte_offsets.last().map_or(0, |l| l.1)
        } else {
            0..0
        };
        let highlight_spans = self.highlighter.highlight_lines(
            self.buffer.rope(),
            visible_byte_range,
            &line_byte_offsets,
        );

        let editor_canvas = render_editor_content(EditorRenderParams {
            visible_lines,
            highlight_spans,
            first_line,
            total_lines: line_count,
            cursor,
            selection,
            font_family: font_family.clone(),
            font_size,
            line_height,
            gutter_width,
            viewport_height_cell: viewport_height,
            cursor_visible: cursor_blink_visible,
            focus_handle,
            view_entity,
            ime_preedit,
            element_bounds_cell: self.element_bounds.clone(),
            theme: theme::theme(cx),
        });

        div()
            .id("editor-view")
            .key_context("editor")
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .size_full()
            .overflow_hidden()
            .bg(rgb(theme::theme(cx).base))
            .font_family(font_family.to_string())
            .text_size(px(font_size))
            // Actions
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::editor_copy))
            .on_action(cx.listener(Self::editor_paste))
            .on_action(cx.listener(Self::editor_cut))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::tab))
            .on_action(cx.listener(Self::move_left))
            .on_action(cx.listener(Self::move_right))
            .on_action(cx.listener(Self::move_up))
            .on_action(cx.listener(Self::move_down))
            .on_action(cx.listener(Self::move_to_line_start))
            .on_action(cx.listener(Self::move_to_line_end))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::move_word_left))
            .on_action(cx.listener(Self::move_word_right))
            // Mouse
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            // Scroll
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            // Enter key
            .on_key_down(cx.listener(Self::on_key_down))
            // Canvas
            .child(editor_canvas)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn blink_epoch_monotonic_bump() {
        let mut e = BlinkState::default();
        let a = e.bump();
        let b = e.bump();
        assert!(b > a);
    }

    #[::core::prelude::v1::test]
    fn blink_state_stale_callback_is_noop() {
        let mut e = BlinkState::default();
        let stale = e.bump();
        let _fresh = e.bump();
        assert!(!e.should_tick(stale));
    }
}
