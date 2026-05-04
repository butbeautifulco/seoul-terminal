use std::cell::RefCell;
use std::rc::Rc;

use gpui::*;
use libghostty_vt::render::CursorVisualStyle;
use libghostty_vt::style::RgbColor;
use seoul_vt::config::TerminalConfig;
use seoul_vt::terminal::{CellWidthKind, TerminalContent};

/// A contiguous run of cells sharing the same style.
///
/// `pub` only to let `TerminalView` hold a reusable buffer of these. Fields
/// stay crate-private so callers treat the buffer as opaque storage.
///
/// `Default` is derived only so the buffer-growth path can write
/// `runs.push(CellRun::default())` without enumerating fields. Every default
/// value is overwritten before any read.
#[derive(Default)]
pub struct CellRun {
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
    /// True if this run contains a wide character (render with natural advance).
    has_wide: bool,
}

/// Reusable per-row run buffer. Owned by `TerminalView`, passed by reference
/// into `render_terminal()`. Outer Vec is row-indexed; inner Vec capacities
/// (and the `String` capacities inside `CellRun`) survive across frames so
/// steady-state rendering stops paying for fresh `Vec` / `String` allocations.
pub type RowRunsBuffer = Rc<RefCell<Vec<Vec<CellRun>>>>;

/// Render the terminal content as a GPUI canvas element.
///
/// `row_runs_buf` is a per-view buffer that retains its allocations across
/// frames: `Vec<Vec<CellRun>>` outer/inner capacities and `String` capacities
/// inside `CellRun.text` all survive between calls. We fill it from
/// `content.cells` here, then the canvas paint closure reads it via a borrow.
pub fn render_terminal(
    content: &TerminalContent,
    config: &TerminalConfig,
    cw: f32,
    ch: f32,
    cursor_blink_visible: bool,
    scrollbar_visible: bool,
    row_runs_buf: &RowRunsBuffer,
) -> impl IntoElement {
    let theme = &config.theme;
    let cursor_bg_hex = theme.cursor.to_u32();
    let font_family: SharedString = config.font_family.clone().into();
    let font_size = config.font_size;

    let scrollbar = content.scrollbar;

    // Fill the reusable buffer from `content.cells` (row-major). Both the
    // outer `Vec<Vec<CellRun>>` and each inner `Vec<CellRun>` are reused —
    // `clear()` drops elements but keeps capacity, and we use an index-based
    // write pattern (`runs[write_idx]`) so existing `CellRun.text` Strings
    // get `text.clear() + text.push_str(..)` instead of being dropped and
    // reallocated.
    {
        let mut runs_buf = row_runs_buf.borrow_mut();
        runs_buf.resize_with(content.cells.len(), Vec::new);

        for (row_idx, row_cells) in content.cells.iter().enumerate() {
            let runs = &mut runs_buf[row_idx];
            let mut write_idx: usize = 0;

            for cell in row_cells {
                // Skip spacer cells (tail/head of wide characters) — they occupy
                // grid space but have no renderable content. The wide character's
                // CellRun already accounts for the extra column width.
                if matches!(
                    cell.wide,
                    CellWidthKind::SpacerTail | CellWidthKind::SpacerHead
                ) {
                    continue;
                }

                let fg_raw = rgb_to_hsla(cell.fg);
                let bg = rgb_to_hsla(cell.bg);
                let is_wide = cell.wide == CellWidthKind::Wide;

                // Apply faint by reducing fg opacity
                let fg = if cell.faint {
                    let mut f = fg_raw;
                    f.a *= 0.5;
                    f
                } else {
                    fg_raw
                };

                // Try merging into the previous run for narrow cells with
                // matching style. write_idx-1 is the slot the previous CellRun
                // was written into in this row.
                if !is_wide && write_idx > 0 {
                    let last = &mut runs[write_idx - 1];
                    if !last.has_wide
                        && last.fg == fg
                        && last.bg == bg
                        && last.bold == cell.bold
                        && last.italic == cell.italic
                        && last.underline == cell.underline
                        && last.strikethrough == cell.strikethrough
                        && last.faint == cell.faint
                    {
                        if cell.graphemes.is_empty() || cell.graphemes.as_slice() == [' '] {
                            last.text.push(' ');
                        } else {
                            for g in &cell.graphemes {
                                last.text.push(*g);
                            }
                        }
                        last.cols += 1;
                        continue;
                    }
                }

                // Need a fresh run slot. Grow the Vec only if we've run out
                // of pre-existing slots.
                if write_idx >= runs.len() {
                    runs.push(CellRun::default());
                }
                let r = &mut runs[write_idx];
                r.text.clear();
                if cell.graphemes.is_empty() || cell.graphemes.as_slice() == [' '] {
                    r.text.push(' ');
                } else {
                    for g in &cell.graphemes {
                        r.text.push(*g);
                    }
                }
                r.fg = fg;
                r.bg = bg;
                r.col_start = cell.col;
                r.cols = if is_wide { 2 } else { 1 };
                r.bold = cell.bold;
                r.italic = cell.italic;
                r.underline = cell.underline;
                r.strikethrough = cell.strikethrough;
                r.faint = cell.faint;
                r.has_wide = is_wide;
                write_idx += 1;
            }

            // Drop excess CellRuns left over from a previous (longer) frame.
            // Truncate is O(n) on dropped elements only and keeps the
            // allocated capacity for next frame.
            runs.truncate(write_idx);
        }
    }

    // Extract cursor paint info
    let cursor = &content.cursor;
    let cursor_visible = cursor.visible && cursor_blink_visible;
    let cursor_col = cursor.col;
    let cursor_row = cursor.row;
    let cursor_style = cursor.style;
    let cursor_width = if cursor.is_wide { cw * 2.0 } else { cw };

    // Capture an Rc clone for the paint closure (the closure is 'static, so
    // it must own its handle on the buffer). This is just a pointer copy —
    // the underlying Vec<Vec<CellRun>> stays put.
    let buf_for_paint = row_runs_buf.clone();

    canvas(
        move |_bounds, _window, _cx| {},
        move |bounds, _, window, cx| {
            let row_runs = buf_for_paint.borrow();
            let origin = bounds.origin;
            let visible_h: f32 = bounds.size.height.into();
            let max_visible = (visible_h / ch).ceil() as usize + 1;
            let rows_to_render = row_runs.len().min(max_visible);

            // Pass 1: Background quads
            for (row_idx, runs) in row_runs.iter().enumerate().take(rows_to_render) {
                let y = origin.y + px(row_idx as f32 * ch);
                let mut merge_x = 0.0_f32;
                let mut merge_w = 0.0_f32;
                let mut merge_color: Option<Hsla> = None;

                for run in runs {
                    let rx = run.col_start as f32 * cw;
                    let rw = run.cols as f32 * cw;

                    if let Some(mc) = merge_color {
                        if mc == run.bg && (merge_x + merge_w - rx).abs() < 0.1 {
                            merge_w += rw;
                            continue;
                        }
                        window.paint_quad(fill(
                            Bounds::new(
                                point(origin.x + px(merge_x), y),
                                size(px(merge_w), px(ch)),
                            ),
                            mc,
                        ));
                    }
                    merge_x = rx;
                    merge_w = rw;
                    merge_color = Some(run.bg);
                }
                if let Some(mc) = merge_color {
                    window.paint_quad(fill(
                        Bounds::new(point(origin.x + px(merge_x), y), size(px(merge_w), px(ch))),
                        mc,
                    ));
                }
            }

            // Pass 2: Text. We iterate by reference so the buffer survives.
            // `SharedString::from(&str)` allocates an `Arc<str>`, which is
            // unavoidable here — `shape_line` takes ownership. The win is
            // that `run.text: String` itself is reused frame to frame.
            for (row_idx, runs) in row_runs.iter().enumerate().take(rows_to_render) {
                let y = origin.y + px(row_idx as f32 * ch);

                for run in runs {
                    let x = origin.x + px(run.col_start as f32 * cw);
                    let text_run = TextRun {
                        len: run.text.len(),
                        font: Font {
                            family: font_family.clone(),
                            weight: if run.bold {
                                FontWeight::BOLD
                            } else {
                                FontWeight::default()
                            },
                            style: if run.italic {
                                FontStyle::Italic
                            } else {
                                FontStyle::Normal
                            },
                            features: FontFeatures::default(),
                            fallbacks: None,
                        },
                        color: run.fg,
                        background_color: None,
                        underline: if run.underline {
                            Some(UnderlineStyle {
                                color: Some(run.fg),
                                thickness: px(1.0),
                                wavy: false,
                            })
                        } else {
                            None
                        },
                        strikethrough: if run.strikethrough {
                            Some(StrikethroughStyle {
                                color: Some(run.fg),
                                thickness: px(1.0),
                            })
                        } else {
                            None
                        },
                    };

                    // Wide character runs use natural glyph advance (None)
                    // so the glyph spans its full 2-cell width. Narrow runs use
                    // fixed cell_width to maintain monospace grid alignment.
                    let force_width = if run.has_wide { None } else { Some(px(cw)) };
                    // Clone the String so SharedString takes an owned value.
                    // We can't move run.text out of the buffer (we want to
                    // reuse it next frame), and `SharedString::from(&str)`
                    // routes through `SmolStr::from(&str)` which the borrow
                    // checker conservatively treats as keeping the borrow
                    // live across `shape_line` + `paint`. Cloning the String
                    // forces an owned `From<String>` path that internally
                    // does the same SmolStr copy without the lifetime tie.
                    let shaped = window.text_system().shape_line(
                        SharedString::from(run.text.clone()),
                        px(font_size),
                        &[text_run],
                        force_width,
                    );
                    let _ = shaped.paint(point(x, y), px(ch), TextAlign::Left, None, window, cx);
                }
            }

            // Pass 3: Cursor
            if cursor_visible && (cursor_row as usize) < rows_to_render {
                let cur_x = origin.x + px(cursor_col as f32 * cw);
                let cur_y = origin.y + px(cursor_row as f32 * ch);

                match cursor_style {
                    CursorVisualStyle::Block => {
                        window.paint_quad(fill(
                            Bounds::new(point(cur_x, cur_y), size(px(cursor_width), px(ch))),
                            rgb(cursor_bg_hex),
                        ));
                    }
                    CursorVisualStyle::Bar => {
                        window.paint_quad(fill(
                            Bounds::new(point(cur_x, cur_y), size(px(2.0), px(ch))),
                            rgb(cursor_bg_hex),
                        ));
                    }
                    CursorVisualStyle::Underline => {
                        let underline_y = cur_y + px(ch - 2.0);
                        window.paint_quad(fill(
                            Bounds::new(point(cur_x, underline_y), size(px(cursor_width), px(2.0))),
                            rgb(cursor_bg_hex),
                        ));
                    }
                    CursorVisualStyle::BlockHollow => {
                        // Top
                        window.paint_quad(fill(
                            Bounds::new(point(cur_x, cur_y), size(px(cursor_width), px(1.0))),
                            rgb(cursor_bg_hex),
                        ));
                        // Bottom
                        window.paint_quad(fill(
                            Bounds::new(
                                point(cur_x, cur_y + px(ch - 1.0)),
                                size(px(cursor_width), px(1.0)),
                            ),
                            rgb(cursor_bg_hex),
                        ));
                        // Left
                        window.paint_quad(fill(
                            Bounds::new(point(cur_x, cur_y), size(px(1.0), px(ch))),
                            rgb(cursor_bg_hex),
                        ));
                        // Right
                        window.paint_quad(fill(
                            Bounds::new(
                                point(cur_x + px(cursor_width - 1.0), cur_y),
                                size(px(1.0), px(ch)),
                            ),
                            rgb(cursor_bg_hex),
                        ));
                    }
                    // int_enum::IntEnum makes this non-exhaustive
                    _ => {
                        window.paint_quad(fill(
                            Bounds::new(point(cur_x, cur_y), size(px(cursor_width), px(ch))),
                            rgb(cursor_bg_hex),
                        ));
                    }
                }
            }

            // Pass 4: Scrollbar
            if scrollbar_visible
                && let Some(sb) = scrollbar
                && sb.has_scrollback()
            {
                let scrollbar_width = 6.0_f32;
                let scrollbar_margin = 2.0_f32;
                let track_height: f32 = bounds.size.height.into();

                let thumb_height = (track_height * sb.thumb_height_fraction()).max(20.0);
                let thumb_top =
                    (track_height * sb.thumb_top_fraction()).min(track_height - thumb_height);

                let sb_x =
                    bounds.origin.x + bounds.size.width - px(scrollbar_width + scrollbar_margin);
                let sb_y = bounds.origin.y + px(thumb_top);

                window.paint_quad(PaintQuad {
                    bounds: Bounds::new(
                        point(sb_x, sb_y),
                        size(px(scrollbar_width), px(thumb_height)),
                    ),
                    corner_radii: Corners::all(px(scrollbar_width / 2.0)),
                    background: hsla(0.0, 0.0, 1.0, 0.3).into(),
                    border_widths: Edges::all(px(0.0)),
                    border_color: transparent_black(),
                    border_style: BorderStyle::default(),
                });
            }
        },
    )
    .size_full()
}

fn rgb_to_hsla(c: RgbColor) -> Hsla {
    let v = ((c.r as u32) << 24) | ((c.g as u32) << 16) | ((c.b as u32) << 8) | 0xff;
    Hsla::from(rgba(v))
}
