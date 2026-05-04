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
    pub faint: bool,
    pub has_wide: bool,
}

#[derive(Default)]
struct CachedRow {
    generation: u64,
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
                || self.rows[row_idx].generation == 0;
            if row_dirty {
                self.rows[row_idx].runs = build_runs_for_row(&content.cells[row_idx]);
                self.rows[row_idx].generation = content.content_generation;
            }
        }
        self.last_generation = content.content_generation;

        let _ = config;
    }

    pub fn rows(&self) -> impl ExactSizeIterator<Item = &[CachedCellRun]> {
        self.rows.iter().map(|row| row.runs.as_slice())
    }
}

fn build_runs_for_row(row_cells: &[RenderedCell]) -> Vec<CachedCellRun> {
    let mut runs: Vec<CachedCellRun> = Vec::new();
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

        let text = if cell.graphemes.is_empty() || cell.graphemes.as_slice() == [' '] {
            " ".to_string()
        } else {
            cell.graphemes.iter().collect()
        };

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
        {
            let mut merged = last.text.to_string();
            merged.push_str(&text);
            last.text = SharedString::from(merged);
            last.cols = last.cols.saturating_add(1);
            continue;
        }

        runs.push(CachedCellRun {
            text: SharedString::from(text),
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
        });
    }
    runs
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

    fn cell(col: u16, grapheme: char, wide: CellWidthKind) -> RenderedCell {
        RenderedCell {
            col,
            row: 0,
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
        }
    }

    #[test]
    fn update_skips_wide_spacers_and_keeps_wide_run_width() {
        let content = TerminalContent {
            cells: vec![vec![
                cell(0, '界', CellWidthKind::Wide),
                cell(1, ' ', CellWidthKind::SpacerTail),
                cell(2, 'x', CellWidthKind::Narrow),
            ]],
            terminal_bounds: TerminalBounds {
                cols: 3,
                rows: 1,
                ..TerminalBounds::default()
            },
            content_generation: 1,
            dirty_rows: vec![0],
            ..TerminalContent::default()
        };

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
}
