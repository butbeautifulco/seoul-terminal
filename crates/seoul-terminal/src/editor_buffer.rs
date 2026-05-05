use std::ops::Range;

use ropey::Rope;

#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub struct CursorPosition {
    pub row: usize,
    pub col: usize, // byte offset within the line
}

impl CursorPosition {
    pub fn zero() -> Self {
        Self { row: 0, col: 0 }
    }
}

pub struct EditorBuffer {
    rope: Rope,
    version: u64,
}

impl EditorBuffer {
    pub fn from_str(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
            version: 0,
        }
    }

    #[allow(dead_code)]
    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    // -- Editing (O(log n)) --

    pub fn insert_at_byte(&mut self, byte_offset: usize, text: &str) {
        let char_idx = self.rope.byte_to_char(byte_offset);
        self.rope.insert(char_idx, text);
        self.version += 1;
    }

    pub fn remove_byte_range(&mut self, range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        let start_char = self.rope.byte_to_char(range.start);
        let end_char = self.rope.byte_to_char(range.end);
        self.rope.remove(start_char..end_char);
        self.version += 1;
    }

    // -- Queries (O(log n)) --

    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    pub fn line_to_byte(&self, line: usize) -> usize {
        self.rope.line_to_byte(line)
    }

    pub fn byte_to_line(&self, byte: usize) -> usize {
        self.rope.byte_to_line(byte)
    }

    /// Get the byte length of a specific line (excluding trailing \n).
    pub fn line_len_bytes(&self, line: usize) -> usize {
        if line >= self.line_count() {
            return 0;
        }
        let line_slice = self.rope.line(line);
        let len = line_slice.len_bytes();
        // Strip trailing newline if present
        if len > 0 {
            let last_char = line_slice.char(line_slice.len_chars().saturating_sub(1));
            if last_char == '\n' {
                return len - 1;
            }
        }
        len
    }

    /// Get line text as a String. Excludes trailing newline.
    pub fn line_text(&self, line: usize) -> String {
        if line >= self.line_count() {
            return String::new();
        }
        let slice = self.rope.line(line);
        let mut s: String = slice.into();
        if s.ends_with('\n') {
            s.pop();
        }
        s
    }

    // -- Coordinate conversion --

    pub fn cursor_to_byte(&self, pos: CursorPosition) -> usize {
        if pos.row >= self.line_count() {
            return self.len_bytes();
        }
        let line_start = self.line_to_byte(pos.row);
        let max_col = self.line_len_bytes(pos.row);
        line_start + pos.col.min(max_col)
    }

    pub fn byte_to_cursor(&self, byte: usize) -> CursorPosition {
        let byte = byte.min(self.len_bytes());
        let row = self.byte_to_line(byte);
        let line_start = self.line_to_byte(row);
        CursorPosition {
            row,
            col: byte - line_start,
        }
    }

    // -- UTF-8 ↔ UTF-16 conversion (for InputHandler) --

    pub fn byte_to_utf16(&self, byte_offset: usize) -> usize {
        let byte_offset = byte_offset.min(self.len_bytes());
        let char_idx = self.rope.byte_to_char(byte_offset);
        // Count UTF-16 code units from start to char_idx
        let mut utf16_offset = 0;
        for ch in self.rope.chars().take(char_idx) {
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    pub fn utf16_to_byte(&self, utf16_offset: usize) -> usize {
        let mut remaining = utf16_offset;
        let mut byte_offset = 0;
        for ch in self.rope.chars() {
            if remaining == 0 {
                break;
            }
            let utf16_len = ch.len_utf16();
            if remaining < utf16_len {
                break;
            }
            remaining -= utf16_len;
            byte_offset += ch.len_utf8();
        }
        byte_offset
    }

    // -- tree-sitter InputEdit generation --

    pub fn make_input_edit(
        &self,
        byte_range: Range<usize>,
        new_text: &str,
    ) -> tree_sitter::InputEdit {
        let start_byte = byte_range.start;
        let old_end_byte = byte_range.end;
        let new_end_byte = start_byte + new_text.len();

        let start_position = self.byte_to_ts_point(start_byte);
        let old_end_position = self.byte_to_ts_point(old_end_byte);
        let new_end_position = self.compute_new_end_point(start_position, new_text);

        tree_sitter::InputEdit {
            start_byte,
            old_end_byte,
            new_end_byte,
            start_position,
            old_end_position,
            new_end_position,
        }
    }

    fn byte_to_ts_point(&self, byte: usize) -> tree_sitter::Point {
        let pos = self.byte_to_cursor(byte);
        tree_sitter::Point::new(pos.row, pos.col)
    }

    fn compute_new_end_point(
        &self,
        start: tree_sitter::Point,
        new_text: &str,
    ) -> tree_sitter::Point {
        let mut row = start.row;
        let mut col = start.column;
        for ch in new_text.chars() {
            if ch == '\n' {
                row += 1;
                col = 0;
            } else {
                col += ch.len_utf8();
            }
        }
        tree_sitter::Point::new(row, col)
    }

    #[allow(dead_code)]
    pub fn text_for_byte_range(&self, range: Range<usize>) -> String {
        if range.is_empty() {
            return String::new();
        }
        let start_char = self.rope.byte_to_char(range.start);
        let end_char = self.rope.byte_to_char(range.end);
        self.rope.slice(start_char..end_char).to_string()
    }

    // -- Full text (for save / copy / initial parse) --

    pub fn contents(&self) -> String {
        self.rope.to_string()
    }

    /// Clamp a CursorPosition to valid buffer bounds.
    pub fn clamp_cursor(&self, pos: CursorPosition) -> CursorPosition {
        let max_row = self.line_count().saturating_sub(1);
        let row = pos.row.min(max_row);
        let max_col = self.line_len_bytes(row);
        CursorPosition {
            row,
            col: pos.col.min(max_col),
        }
    }
}
