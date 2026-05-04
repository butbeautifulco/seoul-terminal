use gpui::prelude::FluentBuilder as _;
use gpui::*;
use seoul_workspace::git::types::{ChangeCategory, ChangedFile, FileStatus, GitChangesStatus};

use crate::icons::{Icon, IconName};
use crate::text_input::{TextInput, TextInputEvent};
use crate::theme;

#[allow(dead_code)]
pub enum GitPanelEvent {
    OpenDiff {
        path: String,
        category: ChangeCategory,
    },
    StageFile(String),
    UnstageFile(String),
    DiscardFile(String),
    StageAll,
    UnstageAll,
    Commit(String),
    Push,
    Pull,
    Sync,
    Fetch,
}

pub struct GitPanelView {
    focus_handle: FocusHandle,
    status: GitChangesStatus,
    commit_input: Entity<TextInput>,
    #[allow(dead_code)]
    commit_input_subscription: Subscription,
    staged_expanded: bool,
    unstaged_expanded: bool,
    untracked_expanded: bool,
    is_busy: bool,
}

impl EventEmitter<GitPanelEvent> for GitPanelView {}

impl GitPanelView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let commit_input = cx.new(|cx| TextInput::multi_line("", "Commit message...", cx));
        let commit_input_subscription = cx.subscribe_in(
            &commit_input,
            window,
            |this, _input, event, window, cx| match event {
                TextInputEvent::Edited => cx.notify(),
                TextInputEvent::Submitted => this.do_commit(cx),
                TextInputEvent::Cancelled => window.focus(&this.focus_handle, cx),
            },
        );

        Self {
            focus_handle: cx.focus_handle(),
            status: GitChangesStatus::default(),
            commit_input,
            commit_input_subscription,
            staged_expanded: true,
            unstaged_expanded: true,
            untracked_expanded: true,
            is_busy: false,
        }
    }

    /// Update the displayed status.
    pub fn set_status(&mut self, status: GitChangesStatus, cx: &mut Context<Self>) {
        self.status = status;
        cx.notify();
    }

    /// Update busy state.
    pub fn set_busy(&mut self, busy: bool, cx: &mut Context<Self>) {
        self.is_busy = busy;
        cx.notify();
    }

    fn do_commit(&mut self, cx: &mut Context<Self>) {
        let msg = self.commit_input.read(cx).text().to_string();
        if msg.trim().is_empty() || self.status.staged.is_empty() || self.is_busy {
            return;
        }
        self.commit_input.update(cx, |input, cx| input.clear(cx));
        cx.emit(GitPanelEvent::Commit(msg));
        cx.notify();
    }

    fn status_icon(status: FileStatus) -> &'static str {
        match status {
            FileStatus::Added => "A",
            FileStatus::Modified => "M",
            FileStatus::Deleted => "D",
            FileStatus::Renamed => "R",
            FileStatus::Copied => "C",
            FileStatus::Untracked => "?",
        }
    }

    fn status_color(status: FileStatus, t: &theme::ThemeColors) -> Rgba {
        match status {
            FileStatus::Added | FileStatus::Untracked => rgb(t.green),
            FileStatus::Modified => rgb(t.yellow),
            FileStatus::Deleted => rgb(t.red),
            FileStatus::Renamed | FileStatus::Copied => rgb(t.blue),
        }
    }

    fn render_section_header(
        &self,
        label: &str,
        count: usize,
        expanded: bool,
        toggle_id: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme::theme(cx);
        let toggle_id_owned = toggle_id.to_string();
        let is_staged = toggle_id == "staged-header";
        let is_unstaged = toggle_id == "unstaged-header";

        div()
            .id(ElementId::Name(toggle_id.to_string().into()))
            .h(px(28.))
            .w_full()
            .px(px(8.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.))
            .cursor_pointer()
            .hover(|s: StyleRefinement| s.bg(rgb(t.hover_bg_subtle)))
            .child(
                div()
                    .w(px(12.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Icon::new(
                            if expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            },
                            rgb(t.overlay0),
                        )
                        .size(px(12.)),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(px(11.))
                    .text_color(rgb(t.text))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("{label} ({count})")),
            )
            // Stage All / Unstage All button
            .when(is_staged, |el| {
                el.child(
                    div()
                        .id("unstage-all-btn")
                        .px(px(4.))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(2.))
                        .cursor_pointer()
                        .hover(|s: StyleRefinement| s.text_color(rgb(t.text)))
                        .child(Icon::new(IconName::Minus, rgb(t.overlay0)).size(px(11.)))
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(rgb(t.overlay0))
                                .child("all"),
                        )
                        .on_click(cx.listener(|_this, _, _window, cx| {
                            cx.emit(GitPanelEvent::UnstageAll);
                        })),
                )
            })
            .when(is_unstaged, |el| {
                el.child(
                    div()
                        .id("stage-all-btn")
                        .px(px(4.))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(2.))
                        .cursor_pointer()
                        .hover(|s: StyleRefinement| s.text_color(rgb(t.text)))
                        .child(Icon::new(IconName::Plus, rgb(t.overlay0)).size(px(11.)))
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(rgb(t.overlay0))
                                .child("all"),
                        )
                        .on_click(cx.listener(|_this, _, _window, cx| {
                            cx.emit(GitPanelEvent::StageAll);
                        })),
                )
            })
            .on_click(cx.listener(move |this, _, _window, cx| {
                match toggle_id_owned.as_str() {
                    "staged-header" => this.staged_expanded = !this.staged_expanded,
                    "unstaged-header" => this.unstaged_expanded = !this.unstaged_expanded,
                    "untracked-header" => this.untracked_expanded = !this.untracked_expanded,
                    _ => {}
                }
                cx.notify();
            }))
            .into_any_element()
    }

    fn render_file_entry(
        &self,
        file: &ChangedFile,
        category: ChangeCategory,
        index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme::theme(cx);
        let path = file.path.clone();
        let status = file.status;
        let additions = file.additions;
        let deletions = file.deletions;
        let cat_prefix = match category {
            ChangeCategory::Staged => "s",
            ChangeCategory::Unstaged => "u",
            _ => "o",
        };

        let action_path = path.clone();
        let discard_path = path.clone();
        let diff_path = path.clone();
        let is_unstaged = matches!(category, ChangeCategory::Unstaged);

        div()
            .id(ElementId::Name(format!("{cat_prefix}-file-{index}").into()))
            .h(px(26.))
            .w_full()
            .pl(px(24.))
            .pr(px(8.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.))
            .cursor_pointer()
            .hover(|s: StyleRefinement| s.bg(rgb(t.hover_bg_subtle)))
            // Status icon
            .child(
                div()
                    .w(px(14.))
                    .text_size(px(11.))
                    .text_color(Self::status_color(status, &t))
                    .font_weight(FontWeight::BOLD)
                    .child(Self::status_icon(status)),
            )
            // File name
            .child(
                div()
                    .flex_1()
                    .text_size(px(11.))
                    .text_color(rgb(t.subtext0))
                    .overflow_hidden()
                    .child(file_name_from_path(&path)),
            )
            // +/- counts
            .when(additions > 0 || deletions > 0, |el| {
                el.child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(2.))
                        .when(additions > 0, |el| {
                            el.child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(rgb(t.green))
                                    .child(format!("+{additions}")),
                            )
                        })
                        .when(deletions > 0, |el| {
                            el.child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(rgb(t.red))
                                    .child(format!("-{deletions}")),
                            )
                        }),
                )
            })
            // Action button (stage/unstage)
            .child(
                div()
                    .id(ElementId::Name(
                        format!("{cat_prefix}-action-{index}").into(),
                    ))
                    .w(px(18.))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s: StyleRefinement| s.text_color(rgb(t.text)))
                    .child(
                        Icon::new(
                            match category {
                                ChangeCategory::Staged => IconName::Minus,
                                _ => IconName::Plus,
                            },
                            rgb(t.overlay0),
                        )
                        .size(px(12.)),
                    )
                    .on_click(cx.listener(move |_this, _, _window, cx| match category {
                        ChangeCategory::Staged => {
                            cx.emit(GitPanelEvent::UnstageFile(action_path.clone()));
                        }
                        _ => {
                            cx.emit(GitPanelEvent::StageFile(action_path.clone()));
                        }
                    })),
            )
            // Discard button (unstaged files only)
            .when(is_unstaged, |el| {
                el.child(
                    div()
                        .id(ElementId::Name(
                            format!("{cat_prefix}-discard-{index}").into(),
                        ))
                        .w(px(18.))
                        .h(px(18.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|s: StyleRefinement| s.text_color(rgb(t.red)))
                        .child(Icon::new(IconName::X, rgb(t.surface2)).size(px(12.)))
                        .on_click(cx.listener(move |_this, _, _window, cx| {
                            cx.emit(GitPanelEvent::DiscardFile(discard_path.clone()));
                        })),
                )
            })
            // Click on file row opens diff
            .on_click(cx.listener(move |_this, _, _window, cx| {
                cx.emit(GitPanelEvent::OpenDiff {
                    path: diff_path.clone(),
                    category,
                });
            }))
            .into_any_element()
    }

    fn render_commit_area(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = theme::theme(cx);
        let has_staged = !self.status.staged.is_empty();
        let can_commit =
            has_staged && !self.commit_input.read(cx).text().trim().is_empty() && !self.is_busy;
        let commit_input = self.commit_input.clone();

        div()
            .id("commit-area")
            .flex_none()
            .w_full()
            .p(px(8.))
            .flex()
            .flex_col()
            .gap(px(6.))
            // Commit message input
            .child(commit_input)
            // Commit button
            .child(
                div()
                    .id("commit-btn")
                    .w_full()
                    .h(px(28.))
                    .bg(if self.is_busy {
                        rgb(t.surface1)
                    } else if has_staged {
                        rgb(t.blue)
                    } else {
                        rgb(t.surface1)
                    })
                    .rounded(px(4.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor(if can_commit {
                        CursorStyle::PointingHand
                    } else {
                        CursorStyle::default()
                    })
                    .text_size(px(11.))
                    .text_color(if self.is_busy {
                        rgb(t.surface2)
                    } else if has_staged {
                        rgb(t.mantle)
                    } else {
                        rgb(t.surface2)
                    })
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(if self.is_busy { "..." } else { "Commit" })
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.do_commit(cx);
                    })),
            )
            .into_any_element()
    }

    fn render_action_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = theme::theme(cx);
        let busy = self.is_busy;

        let actions: Vec<(&str, &str)> = vec![
            ("push-btn", "Push"),
            ("pull-btn", "Pull"),
            ("sync-btn", "Sync"),
            ("fetch-btn", "Fetch"),
        ];

        let mut bar = div()
            .id("action-bar")
            .flex_none()
            .w_full()
            .px(px(8.))
            .pb(px(8.))
            .flex()
            .flex_row()
            .gap(px(4.));

        for (btn_id, label) in actions {
            let label_owned = label.to_string();
            bar = bar.child(
                div()
                    .id(ElementId::Name(btn_id.to_string().into()))
                    .flex_1()
                    .h(px(24.))
                    .bg(rgb(t.surface0))
                    .rounded(px(3.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor(if busy {
                        CursorStyle::default()
                    } else {
                        CursorStyle::PointingHand
                    })
                    .text_size(px(10.))
                    .text_color(if busy {
                        rgb(t.surface2)
                    } else {
                        rgb(t.subtext0)
                    })
                    .when(!busy, |el| {
                        el.hover(|s: StyleRefinement| s.bg(rgb(t.surface1)))
                    })
                    .child(label)
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        if this.is_busy {
                            return;
                        }
                        match label_owned.as_str() {
                            "Push" => cx.emit(GitPanelEvent::Push),
                            "Pull" => cx.emit(GitPanelEvent::Pull),
                            "Sync" => cx.emit(GitPanelEvent::Sync),
                            "Fetch" => cx.emit(GitPanelEvent::Fetch),
                            _ => {}
                        }
                    })),
            );
        }

        bar.into_any_element()
    }
}

fn file_name_from_path(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

// ---------------------------------------------------------------------------
// Focusable, Render
// ---------------------------------------------------------------------------

impl Focusable for GitPanelView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for GitPanelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::theme(cx);

        // File list owns its own scroll viewport so the header, commit area, and
        // action bar stay pinned. A vertical-scroll root would also break flex
        // cross-axis stretch for those pinned children (commit input + action
        // buttons collapse to their intrinsic width).
        let mut scroll_area = div()
            .id("git-panel-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col();

        if !self.status.staged.is_empty() {
            scroll_area = scroll_area.child(self.render_section_header(
                "Staged",
                self.status.staged.len(),
                self.staged_expanded,
                "staged-header",
                cx,
            ));
            if self.staged_expanded {
                let staged = self.status.staged.clone();
                for (i, file) in staged.iter().enumerate() {
                    scroll_area = scroll_area.child(self.render_file_entry(
                        file,
                        ChangeCategory::Staged,
                        i,
                        cx,
                    ));
                }
            }
        }

        if !self.status.unstaged.is_empty() {
            scroll_area = scroll_area.child(self.render_section_header(
                "Unstaged",
                self.status.unstaged.len(),
                self.unstaged_expanded,
                "unstaged-header",
                cx,
            ));
            if self.unstaged_expanded {
                let unstaged = self.status.unstaged.clone();
                for (i, file) in unstaged.iter().enumerate() {
                    scroll_area = scroll_area.child(self.render_file_entry(
                        file,
                        ChangeCategory::Unstaged,
                        i,
                        cx,
                    ));
                }
            }
        }

        if !self.status.untracked.is_empty() {
            scroll_area = scroll_area.child(self.render_section_header(
                "Untracked",
                self.status.untracked.len(),
                self.untracked_expanded,
                "untracked-header",
                cx,
            ));
            if self.untracked_expanded {
                let untracked = self.status.untracked.clone();
                for (i, file) in untracked.iter().enumerate() {
                    scroll_area = scroll_area.child(self.render_file_entry(
                        file,
                        ChangeCategory::Unstaged, // untracked uses stage action
                        i + 1000,                 // offset to avoid ID collision
                        cx,
                    ));
                }
            }
        }

        if self.status.staged.is_empty()
            && self.status.unstaged.is_empty()
            && self.status.untracked.is_empty()
        {
            scroll_area = scroll_area.child(
                div()
                    .px(px(12.))
                    .py(px(20.))
                    .text_size(px(12.))
                    .text_color(rgb(t.surface2))
                    .child("No changes detected."),
            );
        }

        let header = div().flex_none().px(px(12.)).py(px(10.)).child(
            div()
                .text_size(px(11.))
                .text_color(rgb(t.overlay0))
                .font_weight(FontWeight::SEMIBOLD)
                .child("CHANGES"),
        );
        let commit_area = self.render_commit_area(window, cx);
        let action_bar = self.render_action_bar(cx);

        div()
            .id("git-panel")
            .key_context("git-panel")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(t.mantle))
            .child(header)
            .child(commit_area)
            .child(scroll_area)
            .child(action_bar)
    }
}
