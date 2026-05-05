use gpui::prelude::FluentBuilder as _;
use gpui::*;

use crate::theme;
use crate::titlebar::constants::{TRAFFIC_LIGHT_PADDING, platform_title_bar_height};

/// OS-frame wrapper for the titlebar.
///
/// Owns drag state, traffic-light spacing, fullscreen branching, double-click
/// maximize, and active/inactive theming. Content is injected by `TitleBar`
/// each render via `set_children`.
pub struct WindowChrome {
    should_move: bool,
    children: Vec<AnyElement>,
}

impl WindowChrome {
    pub fn new() -> Self {
        Self {
            should_move: false,
            children: Vec::new(),
        }
    }

    pub fn set_children(&mut self, children: impl IntoIterator<Item = AnyElement>) {
        self.children = children.into_iter().collect();
    }
}

impl ParentElement for WindowChrome {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Render for WindowChrome {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::theme(cx);
        let active = window.is_window_active();
        let fullscreen = window.is_fullscreen();

        // Active titlebar sits slightly above the body (surface0); inactive
        // recedes to mantle so a focus loss is perceptible.
        let bg = if active { t.surface0 } else { t.mantle };

        let children = std::mem::take(&mut self.children);

        div()
            .id("window-chrome")
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .h(platform_title_bar_height(window))
            .bg(rgb(bg))
            .border_b_1()
            .border_color(rgb(t.surface0))
            .window_control_area(WindowControlArea::Drag)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, _window, _cx| {
                    this.should_move = true;
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _ev, _window, _cx| {
                    this.should_move = false;
                }),
            )
            .on_mouse_move(cx.listener(|this, _ev, window, _cx| {
                if this.should_move {
                    this.should_move = false;
                    window.start_window_move();
                }
            }))
            .on_click(cx.listener(|_this, ev: &ClickEvent, window, _cx| {
                if ev.click_count() == 2 {
                    #[cfg(target_os = "macos")]
                    window.titlebar_double_click();
                    #[cfg(not(target_os = "macos"))]
                    window.zoom_window();
                }
            }))
            .when(cfg!(target_os = "macos") && !fullscreen, |this| {
                this.pl(px(TRAFFIC_LIGHT_PADDING))
            })
            .children(children)
    }
}
