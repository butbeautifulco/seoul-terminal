use libghostty_vt::style::RgbColor;
use seoul_vt::selection::{
    SelectionMode, TerminalGridPoint, TerminalRowInfo, TerminalSelection, detect_plain_links,
    refresh_plain_links, selected_text_for_selection,
};
use seoul_vt::terminal::{CellWidthKind, RenderedCell, TerminalBounds, TerminalContent};
use smallvec::SmallVec;

fn cell(row: u16, col: u16, grapheme: char, wide: CellWidthKind) -> RenderedCell {
    RenderedCell {
        col,
        row,
        graphemes: [grapheme].into_iter().collect(),
        fg: RgbColor {
            r: 255,
            g: 255,
            b: 255,
        },
        bg: RgbColor { r: 0, g: 0, b: 0 },
        bold: false,
        italic: false,
        underline: false,
        strikethrough: false,
        faint: false,
        wide,
        hyperlink: false,
        link_id: None,
    }
}

fn blank(row: u16, col: u16) -> RenderedCell {
    RenderedCell {
        col,
        row,
        graphemes: SmallVec::new(),
        fg: RgbColor {
            r: 255,
            g: 255,
            b: 255,
        },
        bg: RgbColor { r: 0, g: 0, b: 0 },
        bold: false,
        italic: false,
        underline: false,
        strikethrough: false,
        faint: false,
        wide: CellWidthKind::Narrow,
        hyperlink: false,
        link_id: None,
    }
}

fn row(row: u16, text: &str) -> Vec<RenderedCell> {
    text.chars()
        .enumerate()
        .map(|(col, ch)| cell(row, col as u16, ch, CellWidthKind::Narrow))
        .collect()
}

fn content(rows: Vec<Vec<RenderedCell>>, row_info: Vec<TerminalRowInfo>) -> TerminalContent {
    TerminalContent {
        terminal_bounds: TerminalBounds {
            cols: rows.iter().map(Vec::len).max().unwrap_or_default() as u16,
            rows: rows.len() as u16,
            ..TerminalBounds::default()
        },
        row_info,
        cells: rows,
        ..TerminalContent::default()
    }
}

#[test]
fn selected_text_joins_soft_wrapped_rows_without_newline() {
    let content = content(
        vec![row(0, "https://exa"), row(1, "mple.com")],
        vec![
            TerminalRowInfo { is_wrapped: true },
            TerminalRowInfo { is_wrapped: false },
        ],
    );
    let selection = TerminalSelection::new(
        TerminalGridPoint::new(0, 0),
        TerminalGridPoint::new(1, 8),
        SelectionMode::Cell,
    );

    assert_eq!(
        selected_text_for_selection(&content, &selection),
        "https://example.com"
    );
}

#[test]
fn selected_text_preserves_wide_glyph_once_when_range_touches_tail() {
    let content = content(
        vec![vec![
            cell(0, 0, 'A', CellWidthKind::Narrow),
            cell(0, 1, '界', CellWidthKind::Wide),
            cell(0, 2, ' ', CellWidthKind::SpacerTail),
            cell(0, 3, 'B', CellWidthKind::Narrow),
        ]],
        vec![TerminalRowInfo::default()],
    );
    let selection = TerminalSelection::new(
        TerminalGridPoint::new(0, 2),
        TerminalGridPoint::new(0, 4),
        SelectionMode::Cell,
    );

    assert_eq!(selected_text_for_selection(&content, &selection), "界B");
}

#[test]
fn selected_text_trims_terminal_padding_at_hard_line_breaks() {
    let content = content(
        vec![
            vec![
                cell(0, 0, 'a', CellWidthKind::Narrow),
                cell(0, 1, 'b', CellWidthKind::Narrow),
                blank(0, 2),
                blank(0, 3),
            ],
            row(1, "cd"),
        ],
        vec![TerminalRowInfo::default(), TerminalRowInfo::default()],
    );
    let selection = TerminalSelection::new(
        TerminalGridPoint::new(0, 0),
        TerminalGridPoint::new(1, 2),
        SelectionMode::Cell,
    );

    assert_eq!(selected_text_for_selection(&content, &selection), "ab\ncd");
}

#[test]
fn detect_plain_links_trims_sentence_punctuation() {
    let content = content(
        vec![row(0, "See https://example.com/path?q=1).")],
        vec![TerminalRowInfo::default()],
    );

    let links = detect_plain_links(&content);

    assert_eq!(links.len(), 1);
    assert_eq!(links[0].uri, "https://example.com/path?q=1");
    assert_eq!(links[0].range.start, TerminalGridPoint::new(0, 4));
    assert_eq!(links[0].range.end, TerminalGridPoint::new(0, 32));
}

#[test]
fn refresh_plain_links_marks_cells_with_link_id() {
    let mut content = content(
        vec![row(0, "open https://example.com now")],
        vec![TerminalRowInfo::default()],
    );

    refresh_plain_links(&mut content);

    let link = content
        .link_at(TerminalGridPoint::new(0, 5))
        .expect("link should be available under first URL cell");
    assert_eq!(link.uri, "https://example.com");
    assert_eq!(content.cells[0][4].link_id, None);
    assert_eq!(content.cells[0][5].link_id, Some(link.id));
    assert_eq!(content.cells[0][23].link_id, Some(link.id));
    assert_eq!(content.cells[0][24].link_id, None);
}
