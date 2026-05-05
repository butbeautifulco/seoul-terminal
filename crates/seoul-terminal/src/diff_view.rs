use gpui::*;
use seoul_workspace::git::types::ChangeCategory;

use crate::theme;

/// Diff view renders a unified diff with colored line backgrounds.
pub struct DiffView {
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    path: String,
    #[allow(dead_code)]
    category: ChangeCategory,
    lines: Vec<DiffLine>,
    #[allow(dead_code)]
    scroll_offset: f32,
}

struct DiffLine {
    content: String,
    kind: DiffLineKind,
}

#[derive(Clone, Copy)]
enum DiffLineKind {
    Context,
    Added,
    Deleted,
    Header,
}

impl DiffView {
    pub fn new(
        cx: &mut Context<Self>,
        path: String,
        category: ChangeCategory,
        diff_text: String,
    ) -> Self {
        let lines = parse_diff_lines(&diff_text);
        Self {
            focus_handle: cx.focus_handle(),
            path,
            category,
            lines,
            scroll_offset: 0.0,
        }
    }

    pub fn title(&self) -> String {
        let name = self.path.rsplit('/').next().unwrap_or(&self.path);
        format!("{name} (diff)")
    }

    fn render_diff_line(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(line) = self.lines.get(index) else {
            return div().into_any_element();
        };
        let t = theme::theme(cx);

        let (bg, text_color) = match line.kind {
            DiffLineKind::Added => (Some(0x1a3a1a_u32), rgb(t.green)),
            DiffLineKind::Deleted => (Some(0x3a1a1a_u32), rgb(t.red)),
            DiffLineKind::Header => (None, rgb(t.blue)),
            DiffLineKind::Context => (None, rgb(t.subtext0)),
        };

        let mut row = div()
            .id(ElementId::Name(format!("diff-line-{index}").into()))
            .h(px(18.))
            .w_full()
            .pl(px(8.))
            .flex()
            .flex_row()
            .items_center();

        if let Some(bg_color) = bg {
            row = row.bg(rgb(bg_color));
        }

        // Line number
        row = row.child(
            div()
                .w(px(40.))
                .text_size(px(11.))
                .text_color(rgb(t.surface2))
                .flex_none()
                .child(format!("{}", index + 1)),
        );

        // Content
        row = row.child(
            div()
                .flex_1()
                .text_size(px(12.))
                .text_color(text_color)
                .overflow_hidden()
                .child(line.content.clone()),
        );

        row.into_any_element()
    }
}

fn parse_diff_lines(diff: &str) -> Vec<DiffLine> {
    diff.lines()
        .map(|line| {
            let kind = if line.starts_with('+') && !line.starts_with("+++") {
                DiffLineKind::Added
            } else if line.starts_with('-') && !line.starts_with("---") {
                DiffLineKind::Deleted
            } else if line.starts_with("@@")
                || line.starts_with("diff ")
                || line.starts_with("index ")
            {
                DiffLineKind::Header
            } else {
                DiffLineKind::Context
            };
            DiffLine {
                content: line.to_string(),
                kind,
            }
        })
        .collect()
}

impl crate::item::Item for DiffView {
    fn tab_title(&self, _cx: &App) -> String {
        self.title()
    }

    fn tab_kind(&self) -> crate::tab_kind::TabKind {
        crate::tab_kind::TabKind::Diff
    }
}

impl Focusable for DiffView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DiffView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::theme(cx);
        let line_count = self.lines.len();

        let mut container = div()
            .id("diff-view")
            .key_context("diff")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(t.base));

        // Header showing file path
        container = container.child(
            div()
                .flex_none()
                .px(px(12.))
                .py(px(8.))
                .bg(rgb(t.mantle))
                .border_b_1()
                .border_color(rgb(t.surface0))
                .text_size(px(12.))
                .text_color(rgb(t.text))
                .child(self.path.clone()),
        );

        if line_count == 0 {
            // Empty state
            container = container.child(
                div()
                    .px(px(12.))
                    .py(px(20.))
                    .text_size(px(12.))
                    .text_color(rgb(t.surface2))
                    .child("No differences found."),
            );
        } else {
            // Virtualized list — only the visible window is painted.
            // All diff rows have uniform 18px height (see render_diff_line).
            container = container.child(
                uniform_list(
                    "diff-lines",
                    line_count,
                    cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                        range
                            .map(|i| this.render_diff_line(i, cx))
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_grow()
                .into_any_element(),
            );
        }

        container
    }
}
