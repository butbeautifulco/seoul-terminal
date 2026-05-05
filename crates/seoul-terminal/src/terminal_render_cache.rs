use gpui::*;
use libghostty_vt::style::RgbColor;
use seoul_vt::config::TerminalConfig;
use seoul_vt::terminal::{CellWidthKind, RenderedCell, TerminalContent};

#[derive(Clone, Default)]
pub struct CachedCellRun {
    pub text: SharedString,
    pub fg: Hsla,
    pub bg: Hsla,
    pub col_start: u16,
    pub cols: u16,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    #[allow(dead_code)]
    pub faint: bool,
    pub has_wide: bool,
    pub link_id: Option<u64>,
}

#[derive(Default)]
struct CachedRow {
    generation: Option<u64>,
    runs: Vec<CachedCellRun>,
}

#[derive(Default)]
pub struct TerminalRenderCache {
    rows: Vec<CachedRow>,
    last_generation: u64,
}

impl TerminalRenderCache {
    pub fn update(&mut self, content: &TerminalContent, config: &TerminalConfig) {
        if self.rows.len() != content.cells.len() {
            self.rows.clear();
            self.rows
                .resize_with(content.cells.len(), CachedRow::default);
        }

        let full_refresh =
            self.last_generation != content.content_generation && content.dirty_rows.is_empty();
        for row_idx in 0..content.cells.len() {
            let row_dirty = full_refresh
                || content.dirty_rows.contains(&(row_idx as u16))
                || self.rows[row_idx].generation.is_none();
            if row_dirty {
                self.rows[row_idx].runs = build_runs_for_row(&content.cells[row_idx]);
                self.rows[row_idx].generation = Some(content.content_generation);
            }
        }
        self.last_generation = content.content_generation;

        // Reserved for future render-cache behavior that depends on terminal
        // configuration; kept in the signature so callers do not change again.
        let _ = config;
    }

    pub fn rows(&self) -> impl ExactSizeIterator<Item = &[CachedCellRun]> {
        self.rows.iter().map(|row| row.runs.as_slice())
    }
}

fn build_runs_for_row(row_cells: &[RenderedCell]) -> Vec<CachedCellRun> {
    #[derive(Default)]
    struct PendingCellRun {
        text: String,
        fg: Hsla,
        bg: Hsla,
        col_start: u16,
        cols: u16,
        bold: bool,
        italic: bool,
        underline: bool,
        strikethrough: bool,
        faint: bool,
        has_wide: bool,
        link_id: Option<u64>,
    }

    let mut runs: Vec<PendingCellRun> = Vec::new();
    for cell in row_cells {
        if matches!(
            cell.wide,
            CellWidthKind::SpacerTail | CellWidthKind::SpacerHead
        ) {
            continue;
        }

        let fg_raw = rgb_to_hsla(cell.fg);
        let bg = rgb_to_hsla(cell.bg);
        let fg = if cell.faint {
            let mut f = fg_raw;
            f.a *= 0.5;
            f
        } else {
            fg_raw
        };
        let is_wide = cell.wide == CellWidthKind::Wide;

        if !is_wide
            && let Some(last) = runs.last_mut()
            && !last.has_wide
            && last.fg == fg
            && last.bg == bg
            && last.bold == cell.bold
            && last.italic == cell.italic
            && last.underline == cell.underline
            && last.strikethrough == cell.strikethrough
            && last.faint == cell.faint
            && last.link_id == cell.link_id
        {
            append_cell_text(&mut last.text, cell);
            last.cols = last.cols.saturating_add(1);
            continue;
        }

        let mut text = String::new();
        append_cell_text(&mut text, cell);
        runs.push(PendingCellRun {
            text,
            fg,
            bg,
            col_start: cell.col,
            cols: if is_wide { 2 } else { 1 },
            bold: cell.bold,
            italic: cell.italic,
            underline: cell.underline,
            strikethrough: cell.strikethrough,
            faint: cell.faint,
            has_wide: is_wide,
            link_id: cell.link_id,
        });
    }

    runs.into_iter()
        .map(|run| CachedCellRun {
            text: SharedString::from(run.text),
            fg: run.fg,
            bg: run.bg,
            col_start: run.col_start,
            cols: run.cols,
            bold: run.bold,
            italic: run.italic,
            underline: run.underline,
            strikethrough: run.strikethrough,
            faint: run.faint,
            has_wide: run.has_wide,
            link_id: run.link_id,
        })
        .collect()
}

fn append_cell_text(text: &mut String, cell: &RenderedCell) {
    if cell.graphemes.is_empty() || cell.graphemes.as_slice() == [' '] {
        text.push(' ');
    } else {
        for grapheme in &cell.graphemes {
            text.push(*grapheme);
        }
    }
}

fn rgb_to_hsla(c: RgbColor) -> Hsla {
    let v = ((c.r as u32) << 24) | ((c.g as u32) << 16) | ((c.b as u32) << 8) | 0xff;
    Hsla::from(rgba(v))
}

#[cfg(test)]
mod tests {
    use libghostty_vt::style::RgbColor;
    use seoul_vt::config::TerminalConfig;
    use seoul_vt::terminal::{CellWidthKind, RenderedCell, TerminalBounds, TerminalContent};

    use super::TerminalRenderCache;

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

    fn content(
        rows: Vec<Vec<RenderedCell>>,
        generation: u64,
        dirty_rows: Vec<u16>,
    ) -> TerminalContent {
        TerminalContent {
            terminal_bounds: TerminalBounds {
                cols: rows.iter().map(Vec::len).max().unwrap_or_default() as u16,
                rows: rows.len() as u16,
                ..TerminalBounds::default()
            },
            cells: rows,
            content_generation: generation,
            dirty_rows,
            ..TerminalContent::default()
        }
    }

    fn row_text(cache: &TerminalRenderCache, row_idx: usize) -> String {
        cache.rows().nth(row_idx).unwrap()[0].text.to_string()
    }

    #[test]
    fn update_skips_wide_spacers_and_keeps_wide_run_width() {
        let content = content(
            vec![vec![
                cell(0, 0, '界', CellWidthKind::Wide),
                cell(0, 1, ' ', CellWidthKind::SpacerTail),
                cell(0, 2, 'x', CellWidthKind::Narrow),
            ]],
            1,
            vec![0],
        );

        let mut cache = TerminalRenderCache::default();
        cache.update(&content, &TerminalConfig::default());

        let rows: Vec<_> = cache.rows().collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), 2);
        assert_eq!(rows[0][0].text.as_ref(), "界");
        assert_eq!(rows[0][0].col_start, 0);
        assert_eq!(rows[0][0].cols, 2);
        assert!(rows[0][0].has_wide);
        assert_eq!(rows[0][1].text.as_ref(), "x");
        assert_eq!(rows[0][1].col_start, 2);
        assert_eq!(rows[0][1].cols, 1);
    }

    #[test]
    fn generation_change_with_no_dirty_rows_rebuilds_all_rows() {
        let mut cache = TerminalRenderCache::default();
        cache.update(
            &content(
                vec![
                    vec![cell(0, 0, 'a', CellWidthKind::Narrow)],
                    vec![cell(1, 0, 'b', CellWidthKind::Narrow)],
                ],
                1,
                vec![0, 1],
            ),
            &TerminalConfig::default(),
        );

        cache.update(
            &content(
                vec![
                    vec![cell(0, 0, 'x', CellWidthKind::Narrow)],
                    vec![cell(1, 0, 'y', CellWidthKind::Narrow)],
                ],
                2,
                Vec::new(),
            ),
            &TerminalConfig::default(),
        );

        assert_eq!(row_text(&cache, 0), "x");
        assert_eq!(row_text(&cache, 1), "y");
    }

    #[test]
    fn dirty_update_rebuilds_only_dirty_rows() {
        let mut cache = TerminalRenderCache::default();
        cache.update(
            &content(
                vec![
                    vec![cell(0, 0, 'a', CellWidthKind::Narrow)],
                    vec![cell(1, 0, 'b', CellWidthKind::Narrow)],
                ],
                1,
                vec![0, 1],
            ),
            &TerminalConfig::default(),
        );

        cache.update(
            &content(
                vec![
                    vec![cell(0, 0, 'x', CellWidthKind::Narrow)],
                    vec![cell(1, 0, 'y', CellWidthKind::Narrow)],
                ],
                2,
                vec![1],
            ),
            &TerminalConfig::default(),
        );

        assert_eq!(row_text(&cache, 0), "a");
        assert_eq!(row_text(&cache, 1), "y");
    }

    #[test]
    fn clean_cursor_only_frame_does_not_rebuild_cached_text() {
        let mut cache = TerminalRenderCache::default();
        cache.update(
            &content(
                vec![vec![cell(0, 0, 'a', CellWidthKind::Narrow)]],
                1,
                vec![0],
            ),
            &TerminalConfig::default(),
        );

        cache.update(
            &content(
                vec![vec![cell(0, 0, 'x', CellWidthKind::Narrow)]],
                1,
                Vec::new(),
            ),
            &TerminalConfig::default(),
        );

        assert_eq!(row_text(&cache, 0), "a");
    }

    #[test]
    fn row_count_change_resets_cached_geometry() {
        let mut cache = TerminalRenderCache::default();
        cache.update(
            &content(
                vec![
                    vec![cell(0, 0, 'a', CellWidthKind::Narrow)],
                    vec![cell(1, 0, 'b', CellWidthKind::Narrow)],
                ],
                1,
                vec![0, 1],
            ),
            &TerminalConfig::default(),
        );

        cache.update(
            &content(
                vec![vec![cell(0, 0, 'x', CellWidthKind::Narrow)]],
                1,
                Vec::new(),
            ),
            &TerminalConfig::default(),
        );

        let rows: Vec<_> = cache.rows().collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0].text.as_ref(), "x");
    }

    #[test]
    fn adjacent_link_and_plain_cells_do_not_merge() {
        let mut linked = cell(0, 0, 'a', CellWidthKind::Narrow);
        linked.link_id = Some(1);
        let plain = cell(0, 1, 'b', CellWidthKind::Narrow);
        let content = content(vec![vec![linked, plain]], 1, vec![0]);

        let mut cache = TerminalRenderCache::default();
        cache.update(&content, &TerminalConfig::default());

        let rows: Vec<_> = cache.rows().collect();
        assert_eq!(rows[0].len(), 2);
        assert_eq!(rows[0][0].text.as_ref(), "a");
        assert_eq!(rows[0][0].link_id, Some(1));
        assert_eq!(rows[0][1].text.as_ref(), "b");
        assert_eq!(rows[0][1].link_id, None);
    }

    #[test]
    fn generation_zero_initializes_once_without_rebuilding_clean_frames() {
        let mut cache = TerminalRenderCache::default();
        cache.update(
            &content(
                vec![vec![cell(0, 0, 'a', CellWidthKind::Narrow)]],
                0,
                Vec::new(),
            ),
            &TerminalConfig::default(),
        );

        cache.update(
            &content(
                vec![vec![cell(0, 0, 'x', CellWidthKind::Narrow)]],
                0,
                Vec::new(),
            ),
            &TerminalConfig::default(),
        );

        assert_eq!(row_text(&cache, 0), "a");
    }
}
