#![allow(dead_code)]
use gpui::*;

// ---------------------------------------------------------------------------
// Item trait — implemented by each tab content type
// ---------------------------------------------------------------------------

/// Trait for any view that can be displayed as a tab in a Pane.
/// Inspired by Zed's `workspace::Item` trait, but simplified for Seoul's scale.
pub trait Item: Focusable + Render + 'static {
    /// Title shown in the tab bar.
    fn tab_title(&self, cx: &App) -> String;

    /// A string identifier for the tab kind, used for serialization.
    fn tab_kind_id(&self) -> &'static str;

    /// Whether this item has unsaved changes.
    fn is_dirty(&self) -> bool {
        false
    }

    /// Whether this item supports saving.
    fn can_save(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// ItemHandle — type-erased wrapper for Entity<T: Item>
// ---------------------------------------------------------------------------

/// Type-erased handle that can call Item methods through an Entity<T>.
/// Stored in collections where the concrete type is not known.
pub trait ItemHandle: 'static {
    fn tab_title(&self, cx: &App) -> String;
    fn tab_kind_id(&self, cx: &App) -> &'static str;
    fn is_dirty(&self, cx: &App) -> bool;
    fn can_save(&self, cx: &App) -> bool;
    fn focus_handle(&self, cx: &App) -> FocusHandle;
    fn to_any_view(&self) -> AnyView;
}

impl<T: Item> ItemHandle for Entity<T> {
    fn tab_title(&self, cx: &App) -> String {
        self.read(cx).tab_title(cx)
    }

    fn tab_kind_id(&self, cx: &App) -> &'static str {
        self.read(cx).tab_kind_id()
    }

    fn is_dirty(&self, cx: &App) -> bool {
        self.read(cx).is_dirty()
    }

    fn can_save(&self, cx: &App) -> bool {
        self.read(cx).can_save()
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        <T as Focusable>::focus_handle(self.read(cx), cx)
    }

    fn to_any_view(&self) -> AnyView {
        self.clone().into()
    }
}
