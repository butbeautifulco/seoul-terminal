use std::cell::RefCell;
use std::rc::Rc;

use gpui::*;
use libghostty_vt::render::CursorVisualStyle;
use seoul_vt::config::TerminalConfig;
use seoul_vt::terminal::TerminalContent;

use crate::terminal_render_cache::{CachedCellRun, TerminalRenderCache};

/// Render the terminal content as a GPUI canvas element.
///
/// `render_cache` is a per-view cache that rebuilds only dirty rows from
/// `content.cells`; the canvas paint closure reads cached runs via a borrow.
pub fn render_terminal(
    content: &TerminalContent,
    config: &TerminalConfig,
    cw: f32,
    ch: f32,
    cursor_blink_visible: bool,
    scrollbar_visible: bool,
    render_cache: &Rc<RefCell<TerminalRenderCache>>,
) -> impl IntoElement {
    render_cache.borrow_mut().update(content, config);

    let theme = &config.theme;
    let cursor_bg_hex = theme.cursor.to_u32();
    let cursor_glyph_fg = Hsla::from(rgb(theme.background.to_u32()));
    let font_family: SharedString = config.font_family.clone().into();
    let font_size = config.font_size;

    let scrollbar = content.scrollbar;

    // Extract cursor paint info
    let cursor = &content.cursor;
    let cursor_visible = cursor.visible && cursor_blink_visible;
    let cursor_col = cursor.col;
    let cursor_row = cursor.row;
    let cursor_style = cursor.style;
    let cursor_width = if cursor.is_wide { cw * 2.0 } else { cw };

    let cache_for_paint = render_cache.clone();

    canvas(
        move |_bounds, _window, _cx| {},
        move |bounds, _, window, cx| {
            let render_cache = cache_for_paint.borrow();
            let origin = bounds.origin;
            let visible_h: f32 = bounds.size.height.into();
            let max_visible = (visible_h / ch).ceil() as usize + 1;
            let rows_to_render = render_cache.rows().len().min(max_visible);

            // Pass 1: Background quads
            for (row_idx, runs) in render_cache.rows().enumerate().take(rows_to_render) {
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
            for (row_idx, runs) in render_cache.rows().enumerate().take(rows_to_render) {
                let y = origin.y + px(row_idx as f32 * ch);

                for run in runs {
                    let x = origin.x + px(run.col_start as f32 * cw);
                    paint_run_text(
                        run,
                        point(x, y),
                        cw,
                        ch,
                        font_size,
                        font_family.clone(),
                        run.fg,
                        None,
                        window,
                        cx,
                    );
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
                        if let Some(runs) = render_cache.rows().nth(cursor_row as usize)
                            && let Some(run) = runs.iter().find(|run| {
                                cursor_col >= run.col_start
                                    && cursor_col < run.col_start.saturating_add(run.cols)
                            })
                        {
                            let run_x = origin.x + px(run.col_start as f32 * cw);
                            paint_run_text(
                                run,
                                point(run_x, cur_y),
                                cw,
                                ch,
                                font_size,
                                font_family.clone(),
                                cursor_glyph_fg,
                                Some(Bounds::new(
                                    point(cur_x, cur_y),
                                    size(px(cursor_width), px(ch)),
                                )),
                                window,
                                cx,
                            );
                        }
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

#[allow(clippy::too_many_arguments)]
fn paint_run_text(
    run: &CachedCellRun,
    position: Point<Pixels>,
    cw: f32,
    ch: f32,
    font_size: f32,
    font_family: SharedString,
    color: Hsla,
    clip_bounds: Option<Bounds<Pixels>>,
    window: &mut Window,
    cx: &mut App,
) {
    let text_run = TextRun {
        len: run.text.len(),
        font: Font {
            family: font_family,
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
        color,
        background_color: None,
        underline: if run.underline {
            Some(UnderlineStyle {
                color: Some(color),
                thickness: px(1.0),
                wavy: false,
            })
        } else {
            None
        },
        strikethrough: if run.strikethrough {
            Some(StrikethroughStyle {
                color: Some(color),
                thickness: px(1.0),
            })
        } else {
            None
        },
    };

    // Wide character runs use natural glyph advance (None) so the glyph spans
    // its full 2-cell width. Narrow runs use fixed cell width to maintain grid
    // alignment.
    let force_width = if run.has_wide { None } else { Some(px(cw)) };
    let shaped =
        window
            .text_system()
            .shape_line(run.text.clone(), px(font_size), &[text_run], force_width);
    if let Some(bounds) = clip_bounds {
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            let _ = shaped.paint(position, px(ch), TextAlign::Left, None, window, cx);
        });
    } else {
        let _ = shaped.paint(position, px(ch), TextAlign::Left, None, window, cx);
    }
}
