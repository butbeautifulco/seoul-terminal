use std::sync::OnceLock;

use regex::Regex;

use crate::terminal::{CellWidthKind, RenderedCell, TerminalContent};

const URL_PATTERN: &str = r#"(ipfs:|ipns:|magnet:|mailto:|gemini://|gopher://|https://|http://|news:|file://|git://|ssh:|ftp://)[^\u{0000}-\u{001F}\u{007F}-\u{009F}<>"\s{-}\^⟨⟩`']+"#;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TerminalGridPoint {
    pub row: u16,
    pub col: u16,
}

impl TerminalGridPoint {
    pub const fn new(row: u16, col: u16) -> Self {
        Self { row, col }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TerminalRowInfo {
    pub is_wrapped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionMode {
    Cell,
    Word,
    Line,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCellRange {
    pub start: TerminalGridPoint,
    pub end: TerminalGridPoint,
}

impl TerminalCellRange {
    pub const fn new(start: TerminalGridPoint, end: TerminalGridPoint) -> Self {
        Self { start, end }
    }

    pub fn is_empty(&self) -> bool {
        self.start >= self.end
    }

    pub fn contains_point(&self, point: TerminalGridPoint) -> bool {
        self.start <= point && point < self.end
    }

    pub fn intersects_cell(&self, row: u16, col: u16, width: u16) -> bool {
        let cell_start = TerminalGridPoint::new(row, col);
        let cell_end = TerminalGridPoint::new(row, col.saturating_add(width.max(1)));
        self.start < cell_end && cell_start < self.end
    }

    pub fn intersects_range(&self, other: TerminalCellRange) -> bool {
        self.start < other.end && other.start < self.end
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSelection {
    pub anchor: TerminalGridPoint,
    pub active: TerminalGridPoint,
    pub mode: SelectionMode,
}

impl TerminalSelection {
    pub fn new(anchor: TerminalGridPoint, active: TerminalGridPoint, mode: SelectionMode) -> Self {
        Self {
            anchor,
            active,
            mode,
        }
    }

    pub fn set_active(&mut self, active: TerminalGridPoint) {
        self.active = active;
    }

    pub fn normalized_range(&self) -> TerminalCellRange {
        if self.anchor <= self.active {
            TerminalCellRange::new(self.anchor, self.active)
        } else {
            TerminalCellRange::new(self.active, self.anchor)
        }
    }

    pub fn expanded_range(&self, content: &TerminalContent) -> TerminalCellRange {
        let range = self.normalized_range();
        match self.mode {
            SelectionMode::Cell => range,
            SelectionMode::Word => expand_word_range(content, range),
            SelectionMode::Line => expand_line_range(content, range),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalLink {
    pub id: u64,
    pub uri: String,
    pub range: TerminalCellRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalHyperlinkCandidate {
    pub uri: String,
    pub text: String,
}

impl TerminalContent {
    pub fn link_at(&self, point: TerminalGridPoint) -> Option<&TerminalLink> {
        self.links
            .iter()
            .find(|link| link.range.contains_point(point))
    }
}

pub fn selected_text_for_selection(
    content: &TerminalContent,
    selection: &TerminalSelection,
) -> String {
    let range = selection.expanded_range(content);
    if range.is_empty() {
        return String::new();
    }

    let mut text = String::new();
    for row_idx in range.start.row..=range.end.row {
        if row_idx == range.end.row && range.end.col == 0 {
            break;
        }

        let start_col = if row_idx == range.start.row {
            range.start.col
        } else {
            0
        };
        let end_col = if row_idx == range.end.row {
            range.end.col
        } else {
            content.terminal_bounds.cols
        };

        let mut segment = collect_row_segment(content, row_idx, start_col, end_col);
        let wrapped = row_info(content, row_idx).is_wrapped;
        if row_idx != range.end.row && !wrapped {
            trim_trailing_spaces(&mut segment);
        }
        text.push_str(&segment);

        if row_idx != range.end.row && !wrapped {
            text.push('\n');
        }
    }

    text
}

pub fn detect_plain_links(content: &TerminalContent) -> Vec<TerminalLink> {
    let mut links = Vec::new();
    let mut row_idx = 0usize;
    while row_idx < content.cells.len() {
        let mut text = String::new();
        let mut spans = Vec::new();
        let mut group_row = row_idx;

        loop {
            append_row_text_for_links(content, group_row as u16, &mut text, &mut spans);
            let wrapped = row_info(content, group_row as u16).is_wrapped;
            group_row += 1;
            if !wrapped || group_row >= content.cells.len() {
                break;
            }
        }

        for matched in url_regex().find_iter(&text) {
            let trimmed_end = trim_url_match(&text[matched.start()..matched.end()]);
            let end = matched.start() + trimmed_end;
            if end <= matched.start() {
                continue;
            }
            let Some(start_point) = point_for_byte_offset(&spans, matched.start()) else {
                continue;
            };
            let Some(end_point) = point_for_byte_offset(&spans, end) else {
                continue;
            };
            let uri = text[matched.start()..end].to_string();
            let id = links.len() as u64 + 1;
            links.push(TerminalLink {
                id,
                uri,
                range: TerminalCellRange::new(start_point, end_point),
            });
        }

        row_idx = group_row;
    }

    links
}

pub fn refresh_plain_links(content: &mut TerminalContent) {
    refresh_links(content, &[]);
}

pub fn refresh_links(
    content: &mut TerminalContent,
    hyperlink_candidates: &[TerminalHyperlinkCandidate],
) {
    for row in &mut content.cells {
        for cell in row {
            cell.link_id = None;
        }
    }

    let mut links = detect_osc8_links(content, hyperlink_candidates);
    let plain_links = detect_plain_links(content);
    for mut link in plain_links {
        if links
            .iter()
            .any(|existing| existing.range.intersects_range(link.range))
        {
            continue;
        }
        link.id = links.len() as u64 + 1;
        links.push(link);
    }

    for link in &links {
        for row in &mut content.cells {
            for cell in row {
                let width = cell_selection_width(cell);
                if link.range.intersects_cell(cell.row, cell.col, width) {
                    cell.link_id = Some(link.id);
                }
            }
        }
    }
    content.links = links;
}

fn detect_osc8_links(
    content: &TerminalContent,
    candidates: &[TerminalHyperlinkCandidate],
) -> Vec<TerminalLink> {
    let groups = detect_osc8_cell_groups(content);
    if groups.is_empty() || candidates.is_empty() {
        return Vec::new();
    }

    let mut assigned = vec![None::<String>; groups.len()];
    for group_idx in 0..groups.len() {
        if assigned[group_idx].is_some() {
            continue;
        }

        let text = &groups[group_idx].text;
        let matching_groups: Vec<usize> = (group_idx..groups.len())
            .filter(|idx| groups[*idx].text == *text)
            .collect();
        let matching_candidates: Vec<&TerminalHyperlinkCandidate> = candidates
            .iter()
            .filter(|candidate| candidate.text == *text)
            .collect();
        if matching_candidates.is_empty() {
            continue;
        }

        let start = matching_candidates
            .len()
            .saturating_sub(matching_groups.len());
        for (idx, candidate) in matching_groups
            .into_iter()
            .zip(matching_candidates[start..].iter())
        {
            assigned[idx] = Some(candidate.uri.clone());
        }
    }

    groups
        .into_iter()
        .zip(assigned)
        .filter_map(|(group, uri)| {
            let uri = uri?;
            Some(TerminalLink {
                id: 0,
                uri,
                range: group.range,
            })
        })
        .enumerate()
        .map(|(idx, mut link)| {
            link.id = idx as u64 + 1;
            link
        })
        .collect()
}

fn detect_osc8_cell_groups(content: &TerminalContent) -> Vec<HyperlinkCellGroup> {
    let mut groups = Vec::new();
    let mut current: Option<HyperlinkCellGroup> = None;

    for row in &content.cells {
        for cell in row {
            if matches!(
                cell.wide,
                CellWidthKind::SpacerTail | CellWidthKind::SpacerHead
            ) {
                continue;
            }

            if cell.hyperlink {
                let cell_width = cell_selection_width(cell);
                if let Some(group) = &mut current
                    && can_extend_hyperlink_group(content, group.range.end, cell)
                {
                    append_cell_text(&mut group.text, cell);
                    group.range.end =
                        TerminalGridPoint::new(cell.row, cell.col.saturating_add(cell_width));
                    continue;
                }

                if let Some(group) = current.take()
                    && !group.text.is_empty()
                {
                    groups.push(group);
                }
                let mut text = String::new();
                append_cell_text(&mut text, cell);
                current = Some(HyperlinkCellGroup {
                    text,
                    range: TerminalCellRange::new(
                        TerminalGridPoint::new(cell.row, cell.col),
                        TerminalGridPoint::new(cell.row, cell.col.saturating_add(cell_width)),
                    ),
                });
            } else if let Some(group) = current.take()
                && !group.text.is_empty()
            {
                groups.push(group);
            }
        }

        if let Some(group) = &current
            && !row_info(content, group.range.end.row).is_wrapped
            && let Some(group) = current.take()
            && !group.text.is_empty()
        {
            groups.push(group);
        }
    }

    if let Some(group) = current
        && !group.text.is_empty()
    {
        groups.push(group);
    }

    groups
}

fn can_extend_hyperlink_group(
    content: &TerminalContent,
    current_end: TerminalGridPoint,
    cell: &RenderedCell,
) -> bool {
    if current_end.row == cell.row && current_end.col == cell.col {
        return true;
    }
    current_end.row.saturating_add(1) == cell.row
        && current_end.col == content.terminal_bounds.cols
        && cell.col == 0
        && row_info(content, current_end.row).is_wrapped
}

fn collect_row_segment(
    content: &TerminalContent,
    row_idx: u16,
    start_col: u16,
    end_col: u16,
) -> String {
    let Some(row) = content.cells.get(row_idx as usize) else {
        return String::new();
    };
    let range = TerminalCellRange::new(
        TerminalGridPoint::new(row_idx, start_col),
        TerminalGridPoint::new(row_idx, end_col),
    );
    let mut text = String::new();
    for cell in row {
        if !range.intersects_cell(row_idx, cell.col, cell_selection_width(cell)) {
            continue;
        }
        append_cell_text(&mut text, cell);
    }
    text
}

fn append_row_text_for_links(
    content: &TerminalContent,
    row_idx: u16,
    text: &mut String,
    spans: &mut Vec<TextCellSpan>,
) {
    let Some(row) = content.cells.get(row_idx as usize) else {
        return;
    };

    for cell in row {
        if matches!(
            cell.wide,
            CellWidthKind::SpacerTail | CellWidthKind::SpacerHead
        ) {
            continue;
        }

        let start = text.len();
        append_cell_text(text, cell);
        let end = text.len();
        if start != end {
            spans.push(TextCellSpan {
                byte_start: start,
                byte_end: end,
                range: TerminalCellRange::new(
                    TerminalGridPoint::new(row_idx, cell.col),
                    TerminalGridPoint::new(
                        row_idx,
                        cell.col.saturating_add(cell_selection_width(cell)),
                    ),
                ),
            });
        }
    }
}

fn append_cell_text(text: &mut String, cell: &RenderedCell) {
    if matches!(
        cell.wide,
        CellWidthKind::SpacerTail | CellWidthKind::SpacerHead
    ) {
        return;
    }
    if cell.graphemes.is_empty() {
        text.push(' ');
    } else {
        text.extend(cell.graphemes.iter().copied());
    }
}

fn cell_selection_width(cell: &RenderedCell) -> u16 {
    if cell.wide == CellWidthKind::Wide {
        2
    } else {
        1
    }
}

fn expand_line_range(content: &TerminalContent, range: TerminalCellRange) -> TerminalCellRange {
    let end_row = if range.end.col == 0 && range.end.row > range.start.row {
        range.end.row.saturating_sub(1)
    } else {
        range.end.row
    };
    TerminalCellRange::new(
        TerminalGridPoint::new(range.start.row, 0),
        TerminalGridPoint::new(end_row, line_end_col(content, end_row)),
    )
}

fn expand_word_range(content: &TerminalContent, range: TerminalCellRange) -> TerminalCellRange {
    if range.is_empty() {
        return range;
    }

    let start = word_range_at(content, range.start).map_or(range.start, |word| word.start);
    let active_end = if range.end.col > 0 {
        TerminalGridPoint::new(range.end.row, range.end.col - 1)
    } else if range.end.row > 0 {
        let prev_row = range.end.row - 1;
        TerminalGridPoint::new(prev_row, line_end_col(content, prev_row).saturating_sub(1))
    } else {
        range.end
    };
    let end = word_range_at(content, active_end).map_or(range.end, |word| word.end);

    TerminalCellRange::new(start, end)
}

fn word_range_at(content: &TerminalContent, point: TerminalGridPoint) -> Option<TerminalCellRange> {
    let row = content.cells.get(point.row as usize)?;
    let mut spans = Vec::new();
    let mut text = String::new();
    for cell in row {
        if matches!(
            cell.wide,
            CellWidthKind::SpacerTail | CellWidthKind::SpacerHead
        ) {
            continue;
        }
        let start = text.len();
        append_cell_text(&mut text, cell);
        let end = text.len();
        spans.push(TextCellSpan {
            byte_start: start,
            byte_end: end,
            range: TerminalCellRange::new(
                TerminalGridPoint::new(point.row, cell.col),
                TerminalGridPoint::new(
                    point.row,
                    cell.col.saturating_add(cell_selection_width(cell)),
                ),
            ),
        });
    }

    let span_idx = spans
        .iter()
        .position(|span| span.range.intersects_cell(point.row, point.col, 1))?;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let char_idx = chars
        .iter()
        .position(|(byte_idx, _)| *byte_idx == spans[span_idx].byte_start)?;
    if chars
        .get(char_idx)
        .map(|(_, ch)| ch.is_whitespace())
        .unwrap_or(true)
    {
        return Some(spans[span_idx].range);
    }

    let mut start_char = char_idx;
    while start_char > 0 {
        if chars[start_char - 1].1.is_whitespace() {
            break;
        }
        start_char -= 1;
    }

    let mut end_char = char_idx + 1;
    while end_char < chars.len() {
        if chars[end_char].1.is_whitespace() {
            break;
        }
        end_char += 1;
    }

    let start_byte = chars[start_char].0;
    let end_byte = chars
        .get(end_char)
        .map(|(byte_idx, _)| *byte_idx)
        .unwrap_or(text.len());
    Some(TerminalCellRange::new(
        point_for_byte_offset(&spans, start_byte)?,
        point_for_byte_offset(&spans, end_byte)?,
    ))
}

fn line_end_col(content: &TerminalContent, row_idx: u16) -> u16 {
    content
        .cells
        .get(row_idx as usize)
        .and_then(|row| {
            row.iter()
                .map(|cell| cell.col.saturating_add(cell_selection_width(cell)))
                .max()
        })
        .unwrap_or(content.terminal_bounds.cols)
}

fn row_info(content: &TerminalContent, row_idx: u16) -> TerminalRowInfo {
    content
        .row_info
        .get(row_idx as usize)
        .copied()
        .unwrap_or_default()
}

fn trim_trailing_spaces(text: &mut String) {
    let trimmed = text.trim_end_matches(' ').len();
    text.truncate(trimmed);
}

fn trim_url_match(value: &str) -> usize {
    let mut end = value.len();
    while end > 0 {
        let current = &value[..end];
        let Some(last) = current.chars().next_back() else {
            break;
        };
        if matches!(last, '.' | ',' | ':' | ';') || has_unmatched_closing_delimiter(current, last) {
            end -= last.len_utf8();
            continue;
        }
        break;
    }
    end
}

fn has_unmatched_closing_delimiter(value: &str, delimiter: char) -> bool {
    let (open, close) = match delimiter {
        ')' => ('(', ')'),
        ']' => ('[', ']'),
        '}' => ('{', '}'),
        _ => return false,
    };
    let open_count = value.chars().filter(|ch| *ch == open).count();
    let close_count = value.chars().filter(|ch| *ch == close).count();
    close_count > open_count
}

fn url_regex() -> &'static Regex {
    static URL_REGEX: OnceLock<Regex> = OnceLock::new();
    URL_REGEX.get_or_init(|| Regex::new(URL_PATTERN).expect("terminal URL regex must compile"))
}

fn point_for_byte_offset(spans: &[TextCellSpan], offset: usize) -> Option<TerminalGridPoint> {
    for span in spans {
        if offset <= span.byte_start {
            return Some(span.range.start);
        }
        if offset <= span.byte_end {
            return Some(span.range.end);
        }
    }
    spans.last().map(|span| span.range.end)
}

#[derive(Debug, Clone, Copy)]
struct TextCellSpan {
    byte_start: usize,
    byte_end: usize,
    range: TerminalCellRange,
}

#[derive(Debug, Clone)]
struct HyperlinkCellGroup {
    text: String,
    range: TerminalCellRange,
}
