#![allow(dead_code)]
use std::ops::Range;
use std::time::{Duration, Instant};

use crate::editor_buffer::CursorPosition;

#[derive(Clone, Debug)]
struct EditOperation {
    byte_range: Range<usize>,
    old_text: String,
    new_text: String,
}

#[derive(Clone, Debug)]
struct EditGroup {
    operations: Vec<EditOperation>,
    cursor_before: CursorPosition,
    selection_anchor_before: Option<CursorPosition>,
    cursor_after: CursorPosition,
}

pub struct UndoResult {
    /// (byte_range_to_replace, replacement_text)
    pub operations: Vec<(Range<usize>, String)>,
    pub cursor: CursorPosition,
    pub selection_anchor: Option<CursorPosition>,
}

struct PendingTransaction {
    operations: Vec<EditOperation>,
    cursor_before: CursorPosition,
    selection_anchor_before: Option<CursorPosition>,
}

struct CoalesceState {
    last_was_single_char_insert: bool,
    next_expected_byte: usize,
    last_edit_time: Instant,
}

pub struct UndoHistory {
    undo_stack: Vec<EditGroup>,
    redo_stack: Vec<EditGroup>,
    pending: Option<PendingTransaction>,
    coalesce: CoalesceState,
}

impl UndoHistory {
    pub fn new() -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            pending: None,
            coalesce: CoalesceState {
                last_was_single_char_insert: false,
                next_expected_byte: 0,
                last_edit_time: Instant::now(),
            },
        }
    }

    pub fn begin_transaction(
        &mut self,
        cursor: CursorPosition,
        selection_anchor: Option<CursorPosition>,
    ) {
        self.pending = Some(PendingTransaction {
            operations: Vec::new(),
            cursor_before: cursor,
            selection_anchor_before: selection_anchor,
        });
    }

    pub fn record_edit(&mut self, byte_range: Range<usize>, old_text: String, new_text: String) {
        if let Some(ref mut pending) = self.pending {
            pending.operations.push(EditOperation {
                byte_range,
                old_text,
                new_text,
            });
        }
    }

    pub fn end_transaction(&mut self, cursor_after: CursorPosition) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        if pending.operations.is_empty() {
            return;
        }

        let now = Instant::now();
        let group = EditGroup {
            operations: pending.operations,
            cursor_before: pending.cursor_before,
            selection_anchor_before: pending.selection_anchor_before,
            cursor_after,
        };

        // Check if we can coalesce with previous group
        let should_coalesce = self.coalesce.last_was_single_char_insert
            && group.operations.len() == 1
            && group.operations[0].old_text.is_empty()
            && !group.operations[0].new_text.is_empty()
            && group.operations[0].new_text.len() <= 4
            && !group.operations[0].new_text.contains('\n')
            && group.operations[0].byte_range.start == self.coalesce.next_expected_byte
            && now.duration_since(self.coalesce.last_edit_time) < Duration::from_secs(1);

        if should_coalesce
            && let Some(last_group) = self.undo_stack.last_mut()
            && let Some(last_op) = last_group.operations.last_mut()
        {
            last_op.new_text.push_str(&group.operations[0].new_text);
            last_group.cursor_after = group.cursor_after;
            self.coalesce.next_expected_byte =
                group.operations[0].byte_range.start + group.operations[0].new_text.len();
            self.coalesce.last_edit_time = now;
            // Don't clear redo_stack on coalesce — it was already cleared
            // when the first character of this coalesced group was typed.
            return;
        }

        // Determine if this new group is eligible for future coalescing
        let is_single_char_insert = group.operations.len() == 1
            && group.operations[0].old_text.is_empty()
            && !group.operations[0].new_text.is_empty()
            && group.operations[0].new_text.len() <= 4
            && !group.operations[0].new_text.contains('\n')
            // Only coalesce pure inserts (no preceding selection delete)
            && group.selection_anchor_before.is_none();

        if is_single_char_insert {
            self.coalesce.last_was_single_char_insert = true;
            self.coalesce.next_expected_byte =
                group.operations[0].byte_range.start + group.operations[0].new_text.len();
        } else {
            self.coalesce.last_was_single_char_insert = false;
        }
        self.coalesce.last_edit_time = now;

        self.undo_stack.push(group);
        self.redo_stack.clear();

        // Cap undo stack size
        if self.undo_stack.len() > 1000 {
            self.undo_stack.remove(0);
        }
    }

    pub fn undo(&mut self) -> Option<UndoResult> {
        let group = self.undo_stack.pop()?;
        self.break_coalesce();

        // Build reverse operations: replay in reverse order
        // Each operation: replace new_text (at its post-edit position) with old_text
        let mut ops = Vec::with_capacity(group.operations.len());
        for op in group.operations.iter().rev() {
            // After the original edit, the text at [byte_range.start .. byte_range.start + new_text.len()]
            // is new_text. We need to replace it with old_text.
            let current_range = op.byte_range.start..op.byte_range.start + op.new_text.len();
            ops.push((current_range, op.old_text.clone()));
        }

        let result = UndoResult {
            operations: ops,
            cursor: group.cursor_before,
            selection_anchor: group.selection_anchor_before,
        };

        self.redo_stack.push(group);
        Some(result)
    }

    pub fn redo(&mut self) -> Option<UndoResult> {
        let group = self.redo_stack.pop()?;
        self.break_coalesce();

        // Replay operations in forward order
        let mut ops = Vec::with_capacity(group.operations.len());
        for op in &group.operations {
            ops.push((op.byte_range.clone(), op.new_text.clone()));
        }

        let result = UndoResult {
            operations: ops,
            cursor: group.cursor_after,
            selection_anchor: None,
        };

        self.undo_stack.push(group);
        Some(result)
    }

    pub fn break_coalesce(&mut self) {
        self.coalesce.last_was_single_char_insert = false;
    }
}
