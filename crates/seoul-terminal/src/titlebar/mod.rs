mod chrome;
pub mod constants;

use gpui::*;

use crate::app_view::AppView;
use crate::icons::{Icon, IconName};
use crate::theme;
use crate::titlebar::chrome::WindowChrome;
use crate::titlebar::constants::MAX_BRANCH_NAME_LENGTH;

const DAEMON_GLYPH_CONNECTED: &str = "\u{25CE}"; // ◎
const DAEMON_GLYPH_DISCONNECTED: &str = "\u{2298}"; // ⊘

/// Window-level titlebar content. Composes a `WindowChrome` (drag/spacing/theme)
/// with project metadata (workspace name, branch summary, daemon status).
pub struct TitleBar {
    chrome: Entity<WindowChrome>,
    app_view: WeakEntity<AppView>,
    last_os_title: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl TitleBar {
    pub fn new(app_view: Entity<AppView>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let chrome = cx.new(|_cx| WindowChrome::new());

        // Re-render whenever AppView notifies (workspace switch, daemon flip,
        // git provider attach/refresh — all funnel through AppView state).
        let app_view_sub = cx.observe(&app_view, |_, _, cx| cx.notify());
        // Re-render on focus/blur so active/inactive bg is correct.
        let activation_sub = cx.observe_window_activation(window, |_, _, cx| cx.notify());

        Self {
            chrome,
            app_view: app_view.downgrade(),
            last_os_title: None,
            _subscriptions: vec![app_view_sub, activation_sub],
        }
    }

    fn render_left_cluster(
        &self,
        workspace: Option<SharedString>,
        branch: Option<SharedString>,
        ahead: u32,
        behind: u32,
        cx: &App,
    ) -> AnyElement {
        let t = theme::theme(cx);

        let mut cluster = div()
            .id("titlebar-left")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .px(px(8.))
            .text_size(px(12.))
            // Block drag handler so clicking workspace/branch text doesn't
            // initiate a window move.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());

        if let Some(name) = workspace {
            cluster = cluster.child(div().text_color(rgb(t.text)).child(name));
        }

        if let Some(branch_label) = branch {
            cluster = cluster
                .child(div().text_color(rgb(t.overlay2)).child("·"))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(3.))
                        .child(Icon::new(IconName::GitBranch, rgb(t.blue)).size(px(12.)))
                        .child(div().text_color(rgb(t.subtext0)).child(branch_label)),
                );

            if ahead > 0 {
                cluster = cluster.child(
                    div()
                        .text_color(rgb(t.yellow))
                        .child(format!("\u{2191}{ahead}")),
                );
            }
            if behind > 0 {
                cluster = cluster.child(
                    div()
                        .text_color(rgb(t.yellow))
                        .child(format!("\u{2193}{behind}")),
                );
            }
        }

        cluster.into_any_element()
    }

    fn render_right_cluster(&self, daemon_connected: bool, cx: &App) -> AnyElement {
        let t = theme::theme(cx);

        let (glyph, color) = if daemon_connected {
            (DAEMON_GLYPH_CONNECTED, t.green)
        } else {
            (DAEMON_GLYPH_DISCONNECTED, t.peach)
        };
        let label: SharedString = if daemon_connected {
            "Daemon connected".into()
        } else {
            "Daemon disconnected".into()
        };

        div()
            .id("titlebar-right")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.))
            .px(px(8.))
            .ml_auto()
            .text_size(px(12.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().text_color(rgb(color)).child(glyph))
            .tooltip(move |_window, cx| -> AnyView {
                let label = label.clone();
                cx.new(|_| TitleBarTooltip(label)).into()
            })
            .into_any_element()
    }

    fn sync_os_title(
        &mut self,
        workspace: Option<&SharedString>,
        branch: Option<&SharedString>,
        window: &mut Window,
    ) {
        let title: SharedString = match (workspace, branch) {
            (Some(ws), Some(b)) => format!("{ws} \u{2014} {b}").into(),
            (Some(ws), None) => ws.clone(),
            (None, Some(b)) => b.clone(),
            (None, None) => SharedString::new_static("Seoul"),
        };
        if self.last_os_title.as_ref() == Some(&title) {
            return;
        }
        window.set_window_title(&title);
        self.last_os_title = Some(title);
    }
}

impl Render for TitleBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (workspace, branch, ahead, behind, daemon_connected) =
            if let Some(app_view) = self.app_view.upgrade() {
                let view = app_view.read(cx);
                let (a, b) = view.active_ahead_behind(cx);
                (
                    view.active_workspace_name(),
                    view.active_branch_label(cx).map(truncate_branch),
                    a,
                    b,
                    view.is_daemon_connected(),
                )
            } else {
                (None, None, 0, 0, false)
            };

        self.sync_os_title(workspace.as_ref(), branch.as_ref(), window);

        let left = self.render_left_cluster(workspace, branch, ahead, behind, cx);
        let right = self.render_right_cluster(daemon_connected, cx);

        self.chrome.update(cx, |chrome, _| {
            chrome.set_children([left, right]);
        });

        self.chrome.clone()
    }
}

/// Truncate long branch names with an ellipsis so they don't push the daemon
/// indicator off-screen on narrow windows or unusually long feature branches.
fn truncate_branch(branch: SharedString) -> SharedString {
    if branch.chars().count() <= MAX_BRANCH_NAME_LENGTH {
        return branch;
    }
    let mut truncated: String = branch.chars().take(MAX_BRANCH_NAME_LENGTH - 1).collect();
    truncated.push('\u{2026}');
    truncated.into()
}

/// Minimal tooltip view used for the daemon-status indicator. Avoids pulling in
/// a project-wide tooltip helper for one call site.
struct TitleBarTooltip(SharedString);

impl Render for TitleBarTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::theme(cx);
        div()
            .px(px(8.))
            .py(px(4.))
            .rounded(px(4.))
            .bg(rgb(t.surface1))
            .text_color(rgb(t.text))
            .text_size(px(11.))
            .border_1()
            .border_color(rgb(t.surface2))
            .child(self.0.clone())
    }
}
