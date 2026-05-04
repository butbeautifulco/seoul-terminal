use std::cell::Cell;
use std::rc::Rc;

use gpui::*;

use crate::editor_buffer::CursorPosition;
use crate::editor_view::EditorView;
use crate::syntax::HighlightSpan;
use crate::theme::{ThemeColors, opaque};

pub struct EditorRenderParams {
    pub visible_lines: Vec<String>,
    pub highlight_spans: Vec<Vec<HighlightSpan>>,
    pub first_line: usize,
    pub total_lines: usize,
    pub cursor: CursorPosition,
    pub selection: Option<(CursorPosition, CursorPosition)>,
    pub font_family: SharedString,
    pub font_size: f32,
    pub line_height: f32,
    pub gutter_width: f32,
    pub viewport_height_cell: Rc<Cell<Option<f32>>>,
    pub cursor_visible: bool,
    pub focus_handle: FocusHandle,
    pub view_entity: Entity<EditorView>,
    pub ime_preedit: String,
    pub element_bounds_cell: Rc<Cell<Option<Bounds<Pixels>>>>,
    pub theme: ThemeColors,
}

/// Render editor content as a GPUI canvas with syntax highlighting, cursor, and selection.
pub fn render_editor_content(params: EditorRenderParams) -> impl IntoElement {
    let EditorRenderParams {
        visible_lines,
        highlight_spans,
        first_line,
        total_lines,
        cursor,
        selection,
        font_family,
        font_size,
        line_height,
        gutter_width,
        viewport_height_cell,
        cursor_visible,
        focus_handle,
        view_entity,
        ime_preedit,
        element_bounds_cell,
        theme: t,
    } = params;
    canvas(
        move |_bounds, _window, _cx| {},
        move |bounds, _, window, cx| {
            let origin = bounds.origin;
            let viewport_h: f32 = bounds.size.height.into();
            viewport_height_cell.set(Some(viewport_h));
            element_bounds_cell.set(Some(bounds));

            if total_lines == 0 {
                // Register InputHandler even for empty files
                let handler = ElementInputHandler::new(bounds, view_entity);
                window.handle_input(&focus_handle, handler, cx);
                return;
            }

            let last_line = first_line + visible_lines.len();
            let code_x_offset = gutter_width + 8.0;

            let base_font = Font {
                family: font_family.clone(),
                weight: FontWeight::default(),
                style: FontStyle::Normal,
                features: FontFeatures::default(),
                fallbacks: None,
            };

            let gutter_color = Hsla::from(rgba(opaque(t.surface2)));
            let active_line_num_color = Hsla::from(rgba(opaque(t.text)));
            let default_text_color = Hsla::from(rgba(opaque(t.text)));
            let cursor_color = Hsla::from(rgba(opaque(t.rosewater)));
            let selection_color = Hsla::from(rgba(t.selection_bg));
            let active_line_bg = Hsla::from(rgba(t.active_line_bg));

            // === Pass 1: Backgrounds ===

            // Gutter background
            window.paint_quad(fill(
                Bounds::new(origin, size(px(gutter_width), px(viewport_h))),
                rgb(t.mantle),
            ));

            // Gutter divider
            window.paint_quad(fill(
                Bounds::new(
                    point(origin.x + px(gutter_width), origin.y),
                    size(px(1.0), px(viewport_h)),
                ),
                rgb(t.surface0),
            ));

            // Active line highlight
            if cursor.row >= first_line && cursor.row < last_line {
                let active_y = origin.y + px((cursor.row - first_line) as f32 * line_height);
                window.paint_quad(fill(
                    Bounds::new(
                        point(origin.x + px(gutter_width + 1.0), active_y),
                        size(bounds.size.width - px(gutter_width + 1.0), px(line_height)),
                    ),
                    active_line_bg,
                ));
            }

            // === Pass 2: Shape all visible lines once (reused for selection, text, cursor) ===

            let mut shaped_lines: Vec<Option<ShapedLine>> = Vec::with_capacity(visible_lines.len());

            for (vis_idx, line_text) in visible_lines.iter().enumerate() {
                if line_text.is_empty() {
                    shaped_lines.push(None);
                    continue;
                }
                let spans = highlight_spans.get(vis_idx);
                let runs = build_text_runs(line_text, spans, &base_font, default_text_color);
                let shaped = window.text_system().shape_line(
                    SharedString::from(line_text.clone()),
                    px(font_size),
                    &runs,
                    None,
                );
                shaped_lines.push(Some(shaped));
            }

            // === Pass 3: Selection (using pre-shaped lines) ===

            if let Some((sel_start, sel_end)) = selection {
                for (vis_idx, line_text) in visible_lines.iter().enumerate() {
                    let line_idx = first_line + vis_idx;
                    if line_idx < sel_start.row || line_idx > sel_end.row {
                        continue;
                    }
                    let y = origin.y + px(vis_idx as f32 * line_height);

                    let sel_col_start = if line_idx == sel_start.row {
                        sel_start.col
                    } else {
                        0
                    };
                    let sel_col_end = if line_idx == sel_end.row {
                        sel_end.col
                    } else {
                        line_text.len()
                    };

                    if sel_col_start >= sel_col_end && line_idx != sel_end.row {
                        let x_start = origin.x + px(code_x_offset);
                        let line_width = if let Some(Some(shaped)) = shaped_lines.get(vis_idx) {
                            shaped.width() + px(font_size * 0.6)
                        } else {
                            px(font_size * 0.6)
                        };
                        window.paint_quad(fill(
                            Bounds::new(point(x_start, y), size(line_width, px(line_height))),
                            selection_color,
                        ));
                        continue;
                    }

                    if sel_col_start >= sel_col_end {
                        continue;
                    }

                    if let Some(Some(shaped)) = shaped_lines.get(vis_idx) {
                        let x_start = shaped.x_for_index(sel_col_start);
                        let x_end = shaped.x_for_index(sel_col_end);
                        window.paint_quad(fill(
                            Bounds::new(
                                point(origin.x + px(code_x_offset) + x_start, y),
                                size(x_end - x_start, px(line_height)),
                            ),
                            selection_color,
                        ));
                    }
                }
            }

            // === Pass 4: Text + line numbers (paint pre-shaped lines) ===

            for vis_idx in 0..visible_lines.len() {
                let line_idx = first_line + vis_idx;
                let y_offset = vis_idx as f32 * line_height;
                let y = origin.y + px(y_offset);

                // -- Line number --
                let line_num = format!("{}", line_idx + 1);
                let is_active_line = line_idx == cursor.row;
                let num_color = if is_active_line {
                    active_line_num_color
                } else {
                    gutter_color
                };
                let num_run = TextRun {
                    len: line_num.len(),
                    font: base_font.clone(),
                    color: num_color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let num_shaped = window.text_system().shape_line(
                    SharedString::from(line_num),
                    px(font_size),
                    &[num_run],
                    None,
                );
                let num_width: f32 = num_shaped.width().into();
                let num_x = origin.x + px(gutter_width - num_width - 8.0);
                let _ = num_shaped.paint(
                    point(num_x, y),
                    px(line_height),
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );

                // Paint pre-shaped code text
                if let Some(Some(shaped)) = shaped_lines.get(vis_idx) {
                    let _ = shaped.paint(
                        point(origin.x + px(code_x_offset), y),
                        px(line_height),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
            }

            // === Pass 5: Cursor ===

            if cursor_visible && cursor.row >= first_line && cursor.row < last_line {
                let vis_idx = cursor.row - first_line;
                let y = origin.y + px(vis_idx as f32 * line_height);

                let cursor_x = if let Some(Some(shaped)) = shaped_lines.get(vis_idx) {
                    let idx = cursor.col.min(visible_lines[vis_idx].len());
                    shaped.x_for_index(idx)
                } else {
                    Pixels::ZERO
                };

                window.paint_quad(fill(
                    Bounds::new(
                        point(origin.x + px(code_x_offset) + cursor_x, y),
                        size(px(2.0), px(line_height)),
                    ),
                    cursor_color,
                ));
            }

            // === Pass 6: IME preedit underline ===

            if !ime_preedit.is_empty() && cursor.row >= first_line && cursor.row < last_line {
                let vis_idx = cursor.row - first_line;
                let y = origin.y + px(vis_idx as f32 * line_height) + px(line_height - 2.0);

                let preedit_x = if let Some(Some(shaped)) = shaped_lines.get(vis_idx) {
                    let idx = cursor.col.min(visible_lines[vis_idx].len());
                    shaped.x_for_index(idx)
                } else {
                    Pixels::ZERO
                };

                // Measure preedit width using text shaping for CJK accuracy
                let preedit_run = TextRun {
                    len: ime_preedit.len(),
                    font: base_font,
                    color: default_text_color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let preedit_shaped = window.text_system().shape_line(
                    SharedString::from(ime_preedit.clone()),
                    px(font_size),
                    &[preedit_run],
                    None,
                );
                let preedit_width = preedit_shaped.width();
                window.paint_quad(fill(
                    Bounds::new(
                        point(origin.x + px(code_x_offset) + preedit_x, y),
                        size(preedit_width, px(2.0)),
                    ),
                    cursor_color,
                ));
            }

            // === Pass 7: Register InputHandler ===

            let handler = ElementInputHandler::new(bounds, view_entity);
            window.handle_input(&focus_handle, handler, cx);
        },
    )
    .size_full()
}

/// Convert highlight spans into TextRun array for a single line.
fn build_text_runs(
    line_text: &str,
    spans: Option<&Vec<HighlightSpan>>,
    base_font: &Font,
    default_color: Hsla,
) -> Vec<TextRun> {
    let line_len = line_text.len();
    if line_len == 0 {
        return vec![];
    }

    let Some(spans) = spans else {
        return vec![TextRun {
            len: line_len,
            font: base_font.clone(),
            color: default_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        }];
    };

    if spans.is_empty() {
        return vec![TextRun {
            len: line_len,
            font: base_font.clone(),
            color: default_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        }];
    }

    // Merge-walk spans and produce runs directly (no per-byte allocation).
    // Spans may overlap; last-writer-wins via sorted order.
    let mut runs = Vec::new();
    let mut pos = 0;

    for span in spans {
        let start = span.byte_start.min(line_len);
        let end = span.byte_end.min(line_len);
        if start >= end {
            continue;
        }
        // Gap before this span: default color
        if pos < start {
            runs.push(TextRun {
                len: start - pos,
                font: base_font.clone(),
                color: default_color,
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }
        // Span region: merge with previous run if same color, otherwise new run
        let span_start = start.max(pos);
        if span_start < end {
            if let Some(last) = runs.last_mut() {
                if last.color == span.color {
                    last.len += end - span_start;
                } else {
                    runs.push(TextRun {
                        len: end - span_start,
                        font: base_font.clone(),
                        color: span.color,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    });
                }
            } else {
                runs.push(TextRun {
                    len: end - span_start,
                    font: base_font.clone(),
                    color: span.color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                });
            }
        }
        pos = pos.max(end);
    }

    // Trailing default region
    if pos < line_len {
        runs.push(TextRun {
            len: line_len - pos,
            font: base_font.clone(),
            color: default_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }

    runs
}
