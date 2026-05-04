use gpui::*;

use crate::icons::{Icon, IconName};
use crate::theme;

const AUTO_DISMISS_SECS: u64 = 5;

#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ToastKind {
    Success,
    Error,
    Info,
}

pub struct ToastManager {
    current: Option<(String, ToastKind)>,
    dismiss_task: Option<Task<()>>,
}

impl EventEmitter<()> for ToastManager {}

impl ToastManager {
    pub fn new() -> Self {
        Self {
            current: None,
            dismiss_task: None,
        }
    }

    pub fn show(&mut self, message: String, kind: ToastKind, cx: &mut Context<Self>) {
        self.current = Some((message, kind));
        self.dismiss_task = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(AUTO_DISMISS_SECS))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.current = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.current = None;
        self.dismiss_task = None;
        cx.notify();
    }
}

impl Render for ToastManager {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::theme(cx);

        let Some((message, kind)) = &self.current else {
            return div().id("toast-empty");
        };

        let (bg_color, icon) = match kind {
            ToastKind::Success => (t.green, IconName::Check),
            ToastKind::Error => (t.red, IconName::X),
            ToastKind::Info => (t.blue, IconName::Info),
        };
        let message = message.clone();

        div()
            .id("toast-overlay")
            .absolute()
            .bottom(px(48.))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .id("toast-content")
                    .px(px(16.))
                    .py(px(8.))
                    .bg(rgb(bg_color))
                    .rounded(px(6.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .shadow_md()
                    .child(Icon::new(icon, rgb(t.mantle)).size(px(14.)))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(t.mantle))
                            .font_weight(FontWeight::MEDIUM)
                            .child(message),
                    )
                    .child(
                        div()
                            .id("toast-dismiss")
                            .ml(px(8.))
                            .text_size(px(11.))
                            .text_color(rgb(t.mantle))
                            .cursor_pointer()
                            .hover(|s| s.opacity(0.7))
                            .child(Icon::new(IconName::X, rgb(t.mantle)).size(px(12.)))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.dismiss(cx);
                            })),
                    ),
            )
    }
}
