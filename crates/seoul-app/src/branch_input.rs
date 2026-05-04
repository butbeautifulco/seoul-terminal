//! Single-line text input for branch names in the "new workspace" modal.

use gpui::*;

use crate::text_input::{TextInput, TextInputEvent};

#[derive(Debug, Clone)]
pub enum BranchInputEvent {
    /// Enter pressed — caller reads `BranchInput::text()` to get the value.
    Submitted,
    Cancelled,
}

impl EventEmitter<BranchInputEvent> for BranchInput {}

pub struct BranchInput {
    input: Entity<TextInput>,
    text: String,
    #[allow(dead_code)]
    subscription: Subscription,
}

impl BranchInput {
    pub fn new(
        initial: impl Into<String>,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        let text = initial.into();
        let input = cx.new(|cx| TextInput::single_line(text.clone(), placeholder, cx));
        let subscription = cx.subscribe(&input, |this: &mut Self, input, event, cx| match event {
            TextInputEvent::Edited => {
                this.text = input.read(cx).text().to_string();
                cx.notify();
            }
            TextInputEvent::Submitted => cx.emit(BranchInputEvent::Submitted),
            TextInputEvent::Cancelled => cx.emit(BranchInputEvent::Cancelled),
        });

        Self {
            input,
            text,
            subscription,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

impl Render for BranchInput {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.input.clone()
    }
}

impl Focusable for BranchInput {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}
