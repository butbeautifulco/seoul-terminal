use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use seoul_workspace::persistence::{PersistedTab, PersistedTabKind};
use uuid::Uuid;

use crate::icons::{Icon, IconName};
use crate::item::ItemHandle;
use crate::theme;

const MAX_CLOSED_TABS: usize = 10;

// ---------------------------------------------------------------------------
// TabEntry — lives inside Pane
// ---------------------------------------------------------------------------

pub struct TabEntry {
    pub id: Uuid,
    pub item: Box<dyn ItemHandle>,
    pub kind_id: &'static str,
    pub path: Option<PathBuf>,
    pub restore: Option<PersistedTabKind>,
}

pub struct TabMetadata {
    pub kind_id: &'static str,
    pub path: Option<PathBuf>,
    pub restore: Option<PersistedTabKind>,
}

impl TabMetadata {
    pub fn new(
        kind_id: &'static str,
        path: Option<PathBuf>,
        restore: Option<PersistedTabKind>,
    ) -> Self {
        Self {
            kind_id,
            path,
            restore,
        }
    }
}

impl TabEntry {
    pub fn persisted_tabs(tabs: &[Self]) -> Vec<PersistedTab> {
        tabs.iter()
            .filter_map(|tab| {
                tab.restore
                    .clone()
                    .map(|kind| PersistedTab { id: tab.id, kind })
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// ClosedTab — for undo-close within the Pane
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub struct ClosedTab {
    pub tab_id: Uuid,
    pub kind_id: &'static str,
}

// ---------------------------------------------------------------------------
// PaneEvent — emitted to parent (AppView)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum PaneEvent {
    ActivateItem(#[allow(dead_code)] Uuid),
    CloseItem {
        tab_id: Uuid,
        kind_id: &'static str,
    },
    ItemAdded,
    #[allow(dead_code)]
    NewTabRequested,
    Empty,
}

impl EventEmitter<PaneEvent> for Pane {}

// ---------------------------------------------------------------------------
// Pane
// ---------------------------------------------------------------------------

pub struct Pane {
    pub tabs: Vec<TabEntry>,
    pub active_tab_id: Option<Uuid>,
    closed_tabs: Vec<ClosedTab>,
    focus_handle: FocusHandle,
}

impl Pane {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            tabs: Vec::new(),
            active_tab_id: None,
            closed_tabs: Vec::new(),
            focus_handle: cx.focus_handle(),
        }
    }

    /// Add a new tab and activate it.
    pub fn add_item(
        &mut self,
        id: Uuid,
        item: Box<dyn ItemHandle>,
        metadata: TabMetadata,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let fh = item.focus_handle(cx);
        self.tabs.push(TabEntry {
            id,
            item,
            kind_id: metadata.kind_id,
            path: metadata.path,
            restore: metadata.restore,
        });
        self.active_tab_id = Some(id);
        fh.focus(window, cx);
        cx.emit(PaneEvent::ItemAdded);
        cx.notify();
    }

    /// Add a new tab and activate it without immediately focusing its item.
    pub fn add_item_without_focus(
        &mut self,
        id: Uuid,
        item: Box<dyn ItemHandle>,
        metadata: TabMetadata,
        cx: &mut Context<Self>,
    ) {
        self.tabs.push(TabEntry {
            id,
            item,
            kind_id: metadata.kind_id,
            path: metadata.path,
            restore: metadata.restore,
        });
        self.active_tab_id = Some(id);
        cx.emit(PaneEvent::ItemAdded);
        cx.notify();
    }

    /// Activate an existing tab by id.
    pub fn activate_item(&mut self, tab_id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.iter().find(|t| t.id == tab_id) {
            tab.item.focus_handle(cx).focus(window, cx);
            self.active_tab_id = Some(tab_id);
            cx.emit(PaneEvent::ActivateItem(tab_id));
            cx.notify();
        }
    }

    /// Focus the currently active tab's item.
    pub fn focus_active_item(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Uuid> {
        let tab = self.active_tab()?;
        tab.item.focus_handle(cx).focus(window, cx);
        Some(tab.id)
    }

    /// Replace an existing tab's item in-place.
    #[allow(dead_code)]
    pub fn replace_item(
        &mut self,
        tab_id: Uuid,
        new_item: Box<dyn ItemHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) else {
            return false;
        };
        tab.item = new_item;
        if self.active_tab_id == Some(tab_id) {
            tab.item.focus_handle(cx).focus(window, cx);
        }
        cx.notify();
        true
    }

    /// Close a tab by id. Returns the removed TabEntry's kind_id.
    pub fn close_item(&mut self, tab_id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };

        let kind_id = self.tabs[idx].kind_id;

        // Remember for undo-close
        self.closed_tabs.push(ClosedTab { tab_id, kind_id });
        while self.closed_tabs.len() > MAX_CLOSED_TABS {
            self.closed_tabs.remove(0);
        }

        self.tabs.remove(idx);
        cx.emit(PaneEvent::CloseItem { tab_id, kind_id });

        // Activate adjacent tab
        if !self.tabs.is_empty() {
            let new_idx = idx.min(self.tabs.len() - 1);
            let new_id = self.tabs[new_idx].id;
            self.active_tab_id = Some(new_id);
            self.tabs[new_idx].item.focus_handle(cx).focus(window, cx);
        } else {
            self.active_tab_id = None;
            cx.emit(PaneEvent::Empty);
        }
        cx.notify();
    }

    /// Close the currently active tab.
    pub fn close_active_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(active_id) = self.active_tab_id {
            self.close_item(active_id, window, cx);
        }
    }

    /// Pop the most recently closed tab's info (for reopen by parent).
    #[allow(dead_code)]
    pub fn pop_closed(&mut self) -> Option<ClosedTab> {
        self.closed_tabs.pop()
    }

    /// Find an existing tab by kind + path match.
    pub fn find_tab_by_path(&self, kind_id: &str, path: &PathBuf) -> Option<Uuid> {
        self.tabs.iter().find_map(|t| {
            if t.kind_id == kind_id && t.path.as_ref() == Some(path) {
                Some(t.id)
            } else {
                None
            }
        })
    }

    /// Find an existing tab by kind_id.
    pub fn find_tab_by_kind(&self, kind_id: &str) -> Option<Uuid> {
        self.tabs
            .iter()
            .find(|t| t.kind_id == kind_id)
            .map(|t| t.id)
    }

    /// Get active tab entry.
    pub fn active_tab(&self) -> Option<&TabEntry> {
        let id = self.active_tab_id?;
        self.tabs.iter().find(|t| t.id == id)
    }

    /// Tab ids of given kind (for serialization).
    #[allow(dead_code)]
    pub fn tab_ids_of_kind(&self, kind_id: &str) -> Vec<Uuid> {
        self.tabs
            .iter()
            .filter(|t| t.kind_id == kind_id)
            .map(|t| t.id)
            .collect()
    }

    /// Render the tab bar.
    pub fn render_tab_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = theme::theme(cx);
        let active_tab_id = self.active_tab_id;

        let mut bar = div()
            .id("tab-bar")
            .flex_none()
            .h(px(36.))
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .bg(rgb(t.mantle))
            .border_b_1()
            .border_color(rgb(t.surface0))
            .overflow_x_scroll();

        for tab in &self.tabs {
            let tab_id = tab.id;
            let is_active = active_tab_id == Some(tab_id);
            let title = tab.item.tab_title(cx);
            let dirty = tab.item.is_dirty(cx);
            let icon = match tab.kind_id {
                "terminal" => IconName::Terminal,
                "settings" => IconName::Settings,
                "diff" => IconName::FileCode,
                _ => IconName::File,
            };

            bar = bar.child(
                div()
                    .id(ElementId::Name(format!("tab-{tab_id}").into()))
                    .h_full()
                    .px(px(12.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.))
                    .cursor_pointer()
                    .when(is_active, |el: Stateful<Div>| {
                        el.bg(rgb(t.base)).border_b_2().border_color(rgb(t.blue))
                    })
                    .when(!is_active, |el: Stateful<Div>| {
                        el.hover(|s: StyleRefinement| s.bg(rgb(t.surface0)))
                    })
                    .child(
                        Icon::new(
                            icon,
                            if is_active {
                                rgb(t.text)
                            } else {
                                rgb(t.overlay0)
                            },
                        )
                        .size(px(14.)),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(if is_active {
                                rgb(t.text)
                            } else {
                                rgb(t.subtext0)
                            })
                            .child(title),
                    )
                    .when(dirty, |el| {
                        el.child(div().size(px(5.)).rounded_full().bg(rgb(t.blue)))
                    })
                    .child(
                        div()
                            .id(ElementId::Name(format!("tab-close-{tab_id}").into()))
                            .cursor_pointer()
                            .px(px(3.))
                            .rounded(px(2.))
                            .hover(|s| s.bg(rgb(t.surface2)))
                            .child(Icon::new(IconName::X, rgb(t.surface2)).size(px(12.)))
                            .on_click(cx.listener(move |this: &mut Pane, _, window, cx| {
                                this.close_item(tab_id, window, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this: &mut Pane, _, window, cx| {
                        this.activate_item(tab_id, window, cx);
                    })),
            );
        }

        // "+" new tab button — emits AddItem so parent can create terminal
        bar = bar.child(
            div()
                .id("new-tab-btn")
                .h_full()
                .px(px(10.))
                .flex()
                .items_center()
                .cursor_pointer()
                .hover(|s| s.bg(rgb(t.surface0)))
                .child(Icon::new(IconName::Plus, rgb(t.overlay0)).size(px(14.)))
                .on_click(cx.listener(|_this: &mut Pane, _, window, cx| {
                    // Dispatch NewTab action to parent AppView
                    window.dispatch_action(Box::new(crate::app_view::NewTab), cx);
                })),
        );

        bar.into_any_element()
    }
}

// ---------------------------------------------------------------------------
// GPUI integration
// ---------------------------------------------------------------------------

impl Focusable for Pane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Pane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::theme(cx);
        let tab_bar = self.render_tab_bar(cx);
        let content = if let Some(tab) = self.active_tab() {
            tab.item.to_any_view().into_any_element()
        } else {
            div()
                .id("pane-empty")
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(px(16.))
                                .text_color(rgb(t.surface2))
                                .child("Seoul"),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(t.surface1))
                                .child("Add a project and create a workspace to start."),
                        ),
                )
                .into_any_element()
        };

        div()
            .id("pane")
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(tab_bar)
            .child(
                div()
                    .id("pane-content")
                    .flex_1()
                    .overflow_hidden()
                    .child(content),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seoul_workspace::git::types::ChangeCategory;
    use seoul_workspace::persistence::{PersistedTab, PersistedTabKind};

    struct FocusableFakeItemHandle {
        focus_handle: FocusHandle,
    }

    impl ItemHandle for FocusableFakeItemHandle {
        fn tab_title(&self, _cx: &App) -> String {
            "Fake".into()
        }

        fn tab_kind_id(&self, _cx: &App) -> &'static str {
            "fake"
        }

        fn is_dirty(&self, _cx: &App) -> bool {
            false
        }

        fn can_save(&self, _cx: &App) -> bool {
            false
        }

        fn focus_handle(&self, _cx: &App) -> FocusHandle {
            self.focus_handle.clone()
        }

        fn to_any_view(&self) -> AnyView {
            unreachable!()
        }
    }

    struct FakeItemHandle;

    impl ItemHandle for FakeItemHandle {
        fn tab_title(&self, _cx: &App) -> String {
            unreachable!()
        }

        fn tab_kind_id(&self, _cx: &App) -> &'static str {
            unreachable!()
        }

        fn is_dirty(&self, _cx: &App) -> bool {
            unreachable!()
        }

        fn can_save(&self, _cx: &App) -> bool {
            unreachable!()
        }

        fn focus_handle(&self, _cx: &App) -> FocusHandle {
            unreachable!()
        }

        fn to_any_view(&self) -> AnyView {
            unreachable!()
        }
    }

    fn fake_tab(id: Uuid, restore: Option<PersistedTabKind>) -> TabEntry {
        TabEntry {
            id,
            item: Box::new(FakeItemHandle),
            kind_id: "fake",
            path: None,
            restore,
        }
    }

    fn focusable_fake_tab(id: Uuid, focus_handle: FocusHandle) -> TabEntry {
        TabEntry {
            id,
            item: Box::new(FocusableFakeItemHandle { focus_handle }),
            kind_id: "fake",
            path: None,
            restore: None,
        }
    }

    #[::core::prelude::v1::test]
    fn focus_active_item_focuses_active_tab_item() {
        let mut app = gpui::TestAppContext::single();
        let cx = app.add_empty_window();
        let pane = cx.update(|_, cx| cx.new(Pane::new));
        pane.update_in(cx, |pane, window, cx| {
            let inactive_id = Uuid::new_v4();
            let active_id = Uuid::new_v4();
            let inactive_focus = cx.focus_handle();
            let active_focus = cx.focus_handle();

            pane.tabs
                .push(focusable_fake_tab(inactive_id, inactive_focus.clone()));
            pane.tabs
                .push(focusable_fake_tab(active_id, active_focus.clone()));
            pane.active_tab_id = Some(active_id);

            assert_eq!(pane.focus_active_item(window, cx), Some(active_id));
            assert!(active_focus.is_focused(window));
            assert!(!inactive_focus.is_focused(window));
        });
    }

    #[::core::prelude::v1::test]
    fn focus_active_item_returns_none_without_active_tab() {
        let mut app = gpui::TestAppContext::single();
        let cx = app.add_empty_window();
        let pane = cx.update(|_, cx| cx.new(Pane::new));
        pane.update_in(cx, |pane, window, cx| {
            assert_eq!(pane.focus_active_item(window, cx), None);
        });
    }

    #[::core::prelude::v1::test]
    fn persisted_tabs_project_all_restorable_tab_kinds_in_order() {
        let terminal_tab = Uuid::new_v4();
        let editor_tab = Uuid::new_v4();
        let settings_tab = Uuid::new_v4();
        let diff_tab = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        let tabs = vec![
            fake_tab(
                terminal_tab,
                Some(PersistedTabKind::Terminal { session_id }),
            ),
            fake_tab(
                editor_tab,
                Some(PersistedTabKind::Editor {
                    path: PathBuf::from("/tmp/file.rs"),
                }),
            ),
            fake_tab(settings_tab, Some(PersistedTabKind::Settings)),
            fake_tab(
                diff_tab,
                Some(PersistedTabKind::Diff {
                    path: "src/lib.rs".into(),
                    category: ChangeCategory::Unstaged,
                }),
            ),
        ];

        assert_eq!(
            TabEntry::persisted_tabs(&tabs),
            vec![
                PersistedTab {
                    id: terminal_tab,
                    kind: PersistedTabKind::Terminal { session_id },
                },
                PersistedTab {
                    id: editor_tab,
                    kind: PersistedTabKind::Editor {
                        path: PathBuf::from("/tmp/file.rs"),
                    },
                },
                PersistedTab {
                    id: settings_tab,
                    kind: PersistedTabKind::Settings,
                },
                PersistedTab {
                    id: diff_tab,
                    kind: PersistedTabKind::Diff {
                        path: "src/lib.rs".into(),
                        category: ChangeCategory::Unstaged,
                    },
                },
            ]
        );
    }

    #[::core::prelude::v1::test]
    fn persisted_tabs_excludes_non_restorable_tabs() {
        let restorable_id = Uuid::new_v4();
        let ephemeral_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let tabs = vec![
            fake_tab(ephemeral_id, None),
            fake_tab(
                restorable_id,
                Some(PersistedTabKind::Terminal { session_id }),
            ),
        ];

        assert_eq!(
            TabEntry::persisted_tabs(&tabs),
            vec![PersistedTab {
                id: restorable_id,
                kind: PersistedTabKind::Terminal { session_id },
            }]
        );
    }
}
