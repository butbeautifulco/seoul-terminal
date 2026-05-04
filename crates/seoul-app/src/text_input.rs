use std::ops::Range;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementInputHandler, EntityInputHandler,
    EventEmitter, FocusHandle, Focusable, Font, FontFeatures, FontStyle, FontWeight, Hsla,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Render, ShapedLine, SharedString, StyleRefinement, Styled,
    TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, actions, canvas, div, fill, point,
    px, rgb, rgba, size,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::theme::{self, opaque};

actions!(
    text_input,
    [
        TextBackspace,
        TextDelete,
        TextMoveLeft,
        TextMoveRight,
        TextMoveUp,
        TextMoveDown,
        TextSelectLeft,
        TextSelectRight,
        TextSelectUp,
        TextSelectDown,
        TextMoveToStart,
        TextMoveToEnd,
        TextSelectAll,
        TextCopy,
        TextPaste,
        TextCut,
        TextSubmit,
    ]
);

#[derive(Clone, Debug)]
pub enum TextInputEvent {
    Edited,
    Submitted,
    Cancelled,
}

impl EventEmitter<TextInputEvent> for TextInput {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextInputMode {
    SingleLine,
    MultiLine,
}

#[derive(Clone, Debug)]
struct LineSpan {
    start: usize,
    end: usize,
    text: String,
}

#[derive(Clone)]
struct LineLayout {
    start: usize,
    end: usize,
    shaped: ShapedLine,
}

pub struct TextInputBuffer {
    text: String,
    mode: TextInputMode,
    cursor: usize,
    selection_anchor: Option<usize>,
    marked_range: Option<Range<usize>>,
}

impl TextInputBuffer {
    pub fn single_line(initial: impl Into<String>) -> Self {
        Self::new(initial, TextInputMode::SingleLine)
    }

    pub fn multi_line(initial: impl Into<String>) -> Self {
        Self::new(initial, TextInputMode::MultiLine)
    }

    fn new(initial: impl Into<String>, mode: TextInputMode) -> Self {
        let mut text = initial.into();
        if mode == TextInputMode::SingleLine {
            text = normalize_single_line(&text);
        }
        let cursor = text.len();
        Self {
            text,
            mode,
            cursor,
            selection_anchor: None,
            marked_range: None,
        }
    }

    pub fn text_ref(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn insert_text(&mut self, text: &str) {
        let text = self.normalize_inserted_text(text);
        let range = self
            .marked_range
            .clone()
            .unwrap_or_else(|| self.selected_range());
        self.replace_range(range, &text);
    }

    pub fn replace_range_utf16(&mut self, range_utf16: Option<Range<usize>>, text: &str) {
        let range = range_utf16
            .map(|range| self.range_from_utf16(&range))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selected_range());
        let text = self.normalize_inserted_text(text);
        self.replace_range(range, &text);
    }

    pub fn replace_and_mark_range_utf16(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        selected_range_utf16: Option<Range<usize>>,
    ) {
        let range = range_utf16
            .map(|range| self.range_from_utf16(&range))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selected_range());
        let text = self.normalize_inserted_text(text);
        let start = range.start;
        self.text.replace_range(range, &text);
        if text.is_empty() {
            self.marked_range = None;
            self.cursor = start;
            self.selection_anchor = None;
            return;
        }

        let mark_end = start + text.len();
        self.marked_range = Some(start..mark_end);
        if let Some(selected_range) = selected_range_utf16 {
            let selected_start = start + utf16_to_byte_in_str(&text, selected_range.start);
            let selected_end = start + utf16_to_byte_in_str(&text, selected_range.end);
            self.cursor = selected_end;
            self.selection_anchor = (selected_start != selected_end).then_some(selected_start);
        } else {
            self.cursor = mark_end;
            self.selection_anchor = None;
        }
    }

    pub fn unmark_text(&mut self) {
        self.marked_range = None;
    }

    pub fn backspace(&mut self) {
        if !self.selected_range().is_empty() || self.marked_range.is_some() {
            self.insert_text("");
            return;
        }
        let previous = self.previous_boundary(self.cursor);
        if previous == self.cursor {
            return;
        }
        self.replace_range(previous..self.cursor, "");
    }

    pub fn delete(&mut self) {
        if !self.selected_range().is_empty() || self.marked_range.is_some() {
            self.insert_text("");
            return;
        }
        let next = self.next_boundary(self.cursor);
        if next == self.cursor {
            return;
        }
        self.replace_range(self.cursor..next, "");
    }

    pub fn move_left(&mut self, selecting: bool) {
        if !selecting && !self.selected_range().is_empty() {
            let start = self.selected_range().start;
            self.move_to(start, false);
        } else {
            self.move_to(self.previous_boundary(self.cursor), selecting);
        }
    }

    pub fn move_right(&mut self, selecting: bool) {
        if !selecting && !self.selected_range().is_empty() {
            let end = self.selected_range().end;
            self.move_to(end, false);
        } else {
            self.move_to(self.next_boundary(self.cursor), selecting);
        }
    }

    pub fn move_to_start(&mut self, selecting: bool) {
        let line_start = if self.mode == TextInputMode::MultiLine {
            self.line_start_for_offset(self.cursor)
        } else {
            0
        };
        self.move_to(line_start, selecting);
    }

    pub fn move_to_end(&mut self, selecting: bool) {
        let line_end = if self.mode == TextInputMode::MultiLine {
            self.line_end_for_offset(self.cursor)
        } else {
            self.text.len()
        };
        self.move_to(line_end, selecting);
    }

    pub fn move_up(&mut self, selecting: bool) {
        let (row, col) = self.point_for_offset(self.cursor);
        if row == 0 {
            self.move_to(0, selecting);
            return;
        }
        self.move_to(self.offset_for_point(row - 1, col), selecting);
    }

    pub fn move_down(&mut self, selecting: bool) {
        let lines = self.line_spans();
        let (row, col) = self.point_for_offset(self.cursor);
        if row + 1 >= lines.len() {
            self.move_to(self.text.len(), selecting);
            return;
        }
        self.move_to(self.offset_for_point(row + 1, col), selecting);
    }

    pub fn select_all(&mut self) {
        self.cursor = self.text.len();
        self.selection_anchor = Some(0);
        self.marked_range = None;
    }

    pub fn selected_text(&self) -> String {
        self.text[self.selected_range()].to_string()
    }

    pub fn selected_range(&self) -> Range<usize> {
        if let Some(anchor) = self.selection_anchor {
            anchor.min(self.cursor)..anchor.max(self.cursor)
        } else {
            self.cursor..self.cursor
        }
    }

    pub fn selected_range_utf16(&self) -> UTF16Selection {
        let range = self.selected_range();
        UTF16Selection {
            range: self.byte_to_utf16(range.start)..self.byte_to_utf16(range.end),
            reversed: self
                .selection_anchor
                .is_some_and(|anchor| anchor > self.cursor),
        }
    }

    pub fn marked_text_range_utf16(&self) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.byte_to_utf16(range.start)..self.byte_to_utf16(range.end))
    }

    pub fn text_for_range_utf16(&self, range_utf16: Range<usize>) -> (String, Range<usize>) {
        let range = self.range_from_utf16(&range_utf16);
        let actual = self.byte_to_utf16(range.start)..self.byte_to_utf16(range.end);
        (self.text[range].to_string(), actual)
    }

    pub fn byte_to_utf16(&self, byte_offset: usize) -> usize {
        byte_to_utf16_in_str(&self.text, byte_offset)
    }

    pub fn utf16_to_byte(&self, utf16_offset: usize) -> usize {
        utf16_to_byte_in_str(&self.text, utf16_offset)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.utf16_to_byte(range.start)..self.utf16_to_byte(range.end)
    }

    fn normalize_inserted_text(&self, text: &str) -> String {
        match self.mode {
            TextInputMode::SingleLine => normalize_single_line(text),
            TextInputMode::MultiLine => text.to_string(),
        }
    }

    fn replace_range(&mut self, range: Range<usize>, text: &str) {
        let start = range.start.min(self.text.len());
        let end = range.end.min(self.text.len());
        self.text.replace_range(start..end, text);
        self.cursor = start + text.len();
        self.selection_anchor = None;
        self.marked_range = None;
    }

    fn move_to(&mut self, offset: usize, selecting: bool) {
        let offset = self.clamp_to_boundary(offset);
        if selecting {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor);
            }
        } else {
            self.selection_anchor = None;
        }
        self.cursor = offset;
        self.marked_range = None;
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.text.len())
    }

    fn clamp_to_boundary(&self, offset: usize) -> usize {
        let offset = offset.min(self.text.len());
        if self.text.is_char_boundary(offset) {
            return offset;
        }
        self.text
            .char_indices()
            .map(|(idx, _)| idx)
            .take_while(|idx| *idx < offset)
            .last()
            .unwrap_or(0)
    }

    fn line_spans(&self) -> Vec<LineSpan> {
        let mut spans = Vec::new();
        let mut start = 0;
        for (idx, ch) in self.text.char_indices() {
            if ch == '\n' {
                spans.push(LineSpan {
                    start,
                    end: idx,
                    text: self.text[start..idx].to_string(),
                });
                start = idx + ch.len_utf8();
            }
        }
        spans.push(LineSpan {
            start,
            end: self.text.len(),
            text: self.text[start..].to_string(),
        });
        spans
    }

    fn point_for_offset(&self, offset: usize) -> (usize, usize) {
        let offset = offset.min(self.text.len());
        for (row, line) in self.line_spans().iter().enumerate() {
            if offset <= line.end {
                return (row, offset.saturating_sub(line.start));
            }
        }
        let lines = self.line_spans();
        let row = lines.len().saturating_sub(1);
        (row, lines.last().map_or(0, |line| line.text.len()))
    }

    fn offset_for_point(&self, row: usize, col: usize) -> usize {
        let lines = self.line_spans();
        let Some(line) = lines.get(row) else {
            return self.text.len();
        };
        line.start + col.min(line.text.len())
    }

    fn line_start_for_offset(&self, offset: usize) -> usize {
        self.line_spans()
            .into_iter()
            .find_map(|line| (offset <= line.end).then_some(line.start))
            .unwrap_or(0)
    }

    fn line_end_for_offset(&self, offset: usize) -> usize {
        self.line_spans()
            .into_iter()
            .find_map(|line| (offset <= line.end).then_some(line.end))
            .unwrap_or(self.text.len())
    }
}

pub struct TextInput {
    focus_handle: FocusHandle,
    buffer: TextInputBuffer,
    placeholder: SharedString,
    is_selecting: bool,
    last_bounds: Option<Bounds<Pixels>>,
    last_layouts: Vec<LineLayout>,
    height: Pixels,
    font_size: Pixels,
    line_height: Pixels,
}

impl TextInput {
    pub fn single_line(
        initial: impl Into<String>,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new(
            TextInputBuffer::single_line(initial),
            placeholder,
            px(24.),
            px(11.),
            px(16.),
            cx,
        )
    }

    pub fn multi_line(
        initial: impl Into<String>,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new(
            TextInputBuffer::multi_line(initial),
            placeholder,
            px(72.),
            px(11.),
            px(16.),
            cx,
        )
    }

    fn new(
        buffer: TextInputBuffer,
        placeholder: impl Into<SharedString>,
        height: Pixels,
        font_size: Pixels,
        line_height: Pixels,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            buffer,
            placeholder: placeholder.into(),
            is_selecting: false,
            last_bounds: None,
            last_layouts: Vec::new(),
            height,
            font_size,
            line_height,
        }
    }

    pub fn text(&self) -> &str {
        self.buffer.text_ref()
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.buffer = match self.buffer.mode {
            TextInputMode::SingleLine => TextInputBuffer::single_line(""),
            TextInputMode::MultiLine => TextInputBuffer::multi_line(""),
        };
        cx.emit(TextInputEvent::Edited);
        cx.notify();
    }

    fn edited(&mut self, cx: &mut Context<Self>) {
        cx.emit(TextInputEvent::Edited);
        cx.notify();
    }

    fn backspace(&mut self, _: &TextBackspace, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.backspace();
        self.edited(cx);
    }

    fn delete(&mut self, _: &TextDelete, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.delete();
        self.edited(cx);
    }

    fn move_left(&mut self, _: &TextMoveLeft, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_left(false);
        cx.notify();
    }

    fn move_right(&mut self, _: &TextMoveRight, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_right(false);
        cx.notify();
    }

    fn move_up(&mut self, _: &TextMoveUp, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_up(false);
        cx.notify();
    }

    fn move_down(&mut self, _: &TextMoveDown, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_down(false);
        cx.notify();
    }

    fn select_left(&mut self, _: &TextSelectLeft, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_left(true);
        cx.notify();
    }

    fn select_right(&mut self, _: &TextSelectRight, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_right(true);
        cx.notify();
    }

    fn select_up(&mut self, _: &TextSelectUp, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_up(true);
        cx.notify();
    }

    fn select_down(&mut self, _: &TextSelectDown, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_down(true);
        cx.notify();
    }

    fn move_to_start(&mut self, _: &TextMoveToStart, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_to_start(false);
        cx.notify();
    }

    fn move_to_end(&mut self, _: &TextMoveToEnd, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_to_end(false);
        cx.notify();
    }

    fn select_all(&mut self, _: &TextSelectAll, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.select_all();
        cx.notify();
    }

    fn copy(&mut self, _: &TextCopy, _window: &mut Window, cx: &mut Context<Self>) {
        let text = self.buffer.selected_text();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, _: &TextPaste, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.buffer.insert_text(&text);
            self.edited(cx);
        }
    }

    fn cut(&mut self, _: &TextCut, _window: &mut Window, cx: &mut Context<Self>) {
        let text = self.buffer.selected_text();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.buffer.insert_text("");
            self.edited(cx);
        }
    }

    fn submit(&mut self, _: &TextSubmit, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextInputEvent::Submitted);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" => {
                if self.buffer.mode == TextInputMode::SingleLine
                    || event.keystroke.modifiers.platform
                    || event.keystroke.modifiers.control
                {
                    cx.emit(TextInputEvent::Submitted);
                } else {
                    self.buffer.insert_text("\n");
                    self.edited(cx);
                }
            }
            "escape" => {
                cx.emit(TextInputEvent::Cancelled);
            }
            _ => {}
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.is_selecting = true;
        window.focus(&self.focus_handle, cx);
        let index = self.index_for_point(event.position);
        self.buffer.move_to(index, event.modifiers.shift);
        cx.notify();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_selecting {
            let index = self.index_for_point(event.position);
            self.buffer.move_to(index, true);
            cx.notify();
        }
    }

    fn on_mouse_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.is_selecting = false;
    }

    fn index_for_point(&self, position: gpui::Point<Pixels>) -> usize {
        let Some(bounds) = self.last_bounds else {
            return self.buffer.cursor();
        };
        if self.last_layouts.is_empty() {
            return 0;
        }
        let local_y: f32 = (position.y - bounds.top()).into();
        let line_height: f32 = self.line_height.into();
        let row = (local_y / line_height).floor().max(0.0) as usize;
        let row = row.min(self.last_layouts.len().saturating_sub(1));
        let layout = &self.last_layouts[row];
        if layout.start == layout.end {
            return layout.start;
        }
        let x = position.x - bounds.left();
        layout.start + layout.shaped.closest_index_for_x(x)
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let (text, actual) = self.buffer.text_for_range_utf16(range_utf16);
        actual_range.replace(actual);
        Some(text)
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(self.buffer.selected_range_utf16())
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.buffer.marked_text_range_utf16()
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.unmark_text();
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.buffer.replace_range_utf16(range_utf16, text);
        self.edited(cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.buffer
            .replace_and_mark_range_utf16(range_utf16, new_text, new_selected_range_utf16);
        self.edited(cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.buffer.utf16_to_byte(range_utf16.start)
            ..self.buffer.utf16_to_byte(range_utf16.end);
        bounds_for_byte_range(&self.last_layouts, range, bounds, self.line_height)
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.buffer.byte_to_utf16(self.index_for_point(point)))
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::theme(cx);
        let focus = self.focus_handle.clone();
        let is_focused = focus.is_focused(window);
        let entity = cx.entity();
        let placeholder = self.placeholder.clone();
        let text = self.buffer.text_ref().to_string();
        let mode = self.buffer.mode;
        let cursor = self.buffer.cursor();
        let selection = self.buffer.selected_range();
        let marked_range = self.buffer.marked_range.clone();
        let font_size = self.font_size;
        let line_height = self.line_height;
        let text_color = Hsla::from(rgba(opaque(t.text)));
        let placeholder_color = Hsla::from(rgba(opaque(t.surface2)));
        let selection_color = rgba(0x5539a7ff);
        let cursor_color = rgba(opaque(t.text));
        let font = Font {
            family: "monospace".into(),
            weight: FontWeight::default(),
            style: FontStyle::Normal,
            features: FontFeatures::default(),
            fallbacks: None,
        };

        div()
            .track_focus(&self.focus_handle)
            .key_context("text-input")
            .cursor(CursorStyle::IBeam)
            .w_full()
            .h(self.height)
            .px(px(6.))
            .py(px(4.))
            .bg(rgb(t.surface0))
            .rounded(px(4.))
            .border_1()
            .border_color(if is_focused {
                rgb(t.blue)
            } else {
                rgb(t.surface1)
            })
            .overflow_hidden()
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::move_left))
            .on_action(cx.listener(Self::move_right))
            .on_action(cx.listener(Self::move_up))
            .on_action(cx.listener(Self::move_down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::move_to_start))
            .on_action(cx.listener(Self::move_to_end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::submit))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .child(
                canvas(move |_bounds, _window, _cx| {}, {
                    move |bounds, _, window, cx| {
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, entity.clone()),
                            cx,
                        );

                        let lines = line_spans_for_text(&text);
                        let mut layouts = Vec::with_capacity(lines.len());
                        let mut shaped_lines = Vec::with_capacity(lines.len());

                        for line in &lines {
                            let is_placeholder = text.is_empty() && line.start == 0;
                            let display = if is_placeholder {
                                placeholder.to_string()
                            } else {
                                line.text.clone()
                            };
                            let base_run = TextRun {
                                len: display.len(),
                                font: font.clone(),
                                color: if is_placeholder {
                                    placeholder_color
                                } else {
                                    text_color
                                },
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            };
                            let runs = text_runs_for_marked_range(
                                &display,
                                &base_run,
                                marked_range.as_ref(),
                                line.start,
                                line.end,
                            );
                            let shaped = window.text_system().shape_line(
                                SharedString::from(display),
                                font_size,
                                &runs,
                                None,
                            );
                            layouts.push(LineLayout {
                                start: line.start,
                                end: line.end,
                                shaped: shaped.clone(),
                            });
                            shaped_lines.push((line.start, line.end, shaped));
                        }

                        for (row, (line_start, line_end, shaped)) in shaped_lines.iter().enumerate()
                        {
                            let y = bounds.top() + px(row as f32 * f32::from(line_height));
                            let line_bounds = Bounds::new(
                                point(bounds.left(), y),
                                size(bounds.size.width, line_height),
                            );

                            let overlap_start = selection.start.max(*line_start);
                            let overlap_end = selection.end.min(*line_end);
                            if is_focused && overlap_start < overlap_end {
                                let x_start = shaped.x_for_index(overlap_start - *line_start);
                                let x_end = shaped.x_for_index(overlap_end - *line_start);
                                window.paint_quad(fill(
                                    Bounds::from_corners(
                                        point(bounds.left() + x_start, y),
                                        point(bounds.left() + x_end, y + line_height),
                                    ),
                                    selection_color,
                                ));
                            }

                            let _ = shaped.paint(
                                line_bounds.origin,
                                line_height,
                                TextAlign::Left,
                                None,
                                window,
                                cx,
                            );
                        }

                        if is_focused
                            && selection.is_empty()
                            && let Some(cursor_bounds) =
                                bounds_for_byte_range(&layouts, cursor..cursor, bounds, line_height)
                        {
                            window.paint_quad(fill(
                                Bounds::new(
                                    cursor_bounds.origin,
                                    size(px(2.), cursor_bounds.size.height),
                                ),
                                cursor_color,
                            ));
                        }

                        entity.update(cx, |input, _cx| {
                            input.last_bounds = Some(bounds);
                            input.last_layouts = layouts;
                        });
                    }
                })
                .w_full()
                .h_full(),
            )
            .when(mode == TextInputMode::SingleLine, |element| {
                element.hover(|s: StyleRefinement| s.border_color(rgb(t.overlay0)))
            })
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn line_spans_for_text(text: &str) -> Vec<LineSpan> {
    let mut spans = Vec::new();
    let mut start = 0;
    for (idx, ch) in text.char_indices() {
        if ch == '\n' {
            spans.push(LineSpan {
                start,
                end: idx,
                text: text[start..idx].to_string(),
            });
            start = idx + ch.len_utf8();
        }
    }
    spans.push(LineSpan {
        start,
        end: text.len(),
        text: text[start..].to_string(),
    });
    spans
}

fn bounds_for_byte_range(
    layouts: &[LineLayout],
    range: Range<usize>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
) -> Option<Bounds<Pixels>> {
    let layout = layouts
        .iter()
        .find(|layout| range.start >= layout.start && range.start <= layout.end)
        .or_else(|| layouts.last())?;
    let row = layouts
        .iter()
        .position(|candidate| candidate.start == layout.start && candidate.end == layout.end)
        .unwrap_or(0);
    let start = range
        .start
        .saturating_sub(layout.start)
        .min(layout.end - layout.start);
    let end = range
        .end
        .saturating_sub(layout.start)
        .min(layout.end - layout.start);
    let x_start = layout.shaped.x_for_index(start);
    let x_end = layout.shaped.x_for_index(end);
    let y = bounds.top() + px(row as f32 * f32::from(line_height));
    Some(Bounds::from_corners(
        point(bounds.left() + x_start, y),
        point(bounds.left() + x_end.max(x_start + px(2.)), y + line_height),
    ))
}

fn text_runs_for_marked_range(
    display: &str,
    base_run: &TextRun,
    marked_range: Option<&Range<usize>>,
    line_start: usize,
    line_end: usize,
) -> Vec<TextRun> {
    let Some(marked_range) = marked_range else {
        return vec![base_run.clone()];
    };
    let mark_start = marked_range.start.max(line_start);
    let mark_end = marked_range.end.min(line_end);
    if mark_start >= mark_end {
        return vec![base_run.clone()];
    }

    let before = mark_start - line_start;
    let marked = mark_end - mark_start;
    let after = display.len().saturating_sub(before + marked);
    [
        TextRun {
            len: before,
            ..base_run.clone()
        },
        TextRun {
            len: marked,
            underline: Some(UnderlineStyle {
                color: Some(base_run.color),
                thickness: px(1.),
                wavy: false,
            }),
            ..base_run.clone()
        },
        TextRun {
            len: after,
            ..base_run.clone()
        },
    ]
    .into_iter()
    .filter(|run| run.len > 0)
    .collect()
}

fn normalize_single_line(text: &str) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .map(|ch| if ch == '\n' || ch == '\r' { ' ' } else { ch })
        .collect()
}

fn byte_to_utf16_in_str(text: &str, byte_offset: usize) -> usize {
    text[..byte_offset.min(text.len())].encode_utf16().count()
}

fn utf16_to_byte_in_str(text: &str, utf16_offset: usize) -> usize {
    let mut utf16_count = 0;
    for (byte_idx, ch) in text.char_indices() {
        if utf16_count >= utf16_offset {
            return byte_idx;
        }
        utf16_count += ch.len_utf16();
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_line_normalizes_inserted_newlines_to_spaces() {
        let mut input = TextInputBuffer::single_line("feature");

        input.insert_text("\nbranch\nname");

        assert_eq!(input.text_ref(), "feature branch name");
    }

    #[test]
    fn multi_line_preserves_inserted_newlines() {
        let mut input = TextInputBuffer::multi_line("fix");

        input.insert_text("\ncommit body");

        assert_eq!(input.text_ref(), "fix\ncommit body");
    }

    #[test]
    fn backspace_removes_one_grapheme_cluster() {
        let mut input = TextInputBuffer::single_line("a🇰🇷");

        input.backspace();

        assert_eq!(input.text_ref(), "a");
    }

    #[test]
    fn delete_removes_one_grapheme_cluster_after_cursor() {
        let mut input = TextInputBuffer::single_line("a🇰🇷b");
        input.move_left(false);
        input.move_left(false);

        input.delete();

        assert_eq!(input.text_ref(), "ab");
    }

    #[test]
    fn selection_replacement_updates_text_and_cursor() {
        let mut input = TextInputBuffer::single_line("feature-login");
        input.select_all();

        input.insert_text("fix-auth");

        assert_eq!(input.text_ref(), "fix-auth");
        assert_eq!(input.selected_range_utf16().range, 8..8);
    }

    #[test]
    fn selected_text_tracks_shift_selection() {
        let mut input = TextInputBuffer::single_line("abc");
        input.move_left(true);
        input.move_left(true);

        assert_eq!(input.selected_text(), "bc");
    }

    #[test]
    fn utf16_ranges_account_for_hangul_and_emoji() {
        let mut input = TextInputBuffer::single_line("한🙂a");
        input.move_left(true);

        assert_eq!(input.selected_text(), "a");
        assert_eq!(input.selected_range_utf16().range, 3..4);

        input.replace_and_mark_range_utf16(None, "글", None);

        assert_eq!(input.text_ref(), "한🙂글");
        assert_eq!(input.marked_text_range_utf16(), Some(3..4));
    }
}
