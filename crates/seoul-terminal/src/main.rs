mod app_view;
mod assets;
mod branch_input;
mod daemon_client;
mod diff_view;
mod editor_buffer;
mod editor_element;
mod editor_view;
mod file_tree_view;
mod git_panel_view;
mod git_state_provider;
mod icons;
mod item;
mod pane;
mod pane_group;
mod process_metrics;
mod resource_indicator;
mod settings_view;
mod syntax;
mod tab_kind;
mod terminal_element;
mod terminal_render_cache;
mod terminal_view;
mod text_input;
mod theme;
mod titlebar;
mod toast;
mod undo_history;

use app_view::{
    AddProject, AppView, CloseActiveTab, NewTab, OpenSettings, Quit, SplitDown, SplitRight,
    ToggleFileTree, ToggleSidebar,
};
use editor_view::{
    Backspace, Delete, EditorCopy, EditorCut, EditorPaste, MoveDown, MoveLeft, MoveRight,
    MoveToLineEnd, MoveToLineStart, MoveUp, MoveWordLeft, MoveWordRight, Save, SelectAll,
    SelectDown, SelectLeft, SelectRight, SelectUp, Tab,
};
use gpui::*;
use resource_indicator::ToggleResourceMonitor;
use seoul_workspace::settings::SettingsStore;
use terminal_view::{Copy, Paste};
use text_input::{
    TextBackspace, TextCopy, TextCut, TextDelete, TextMoveDown, TextMoveLeft, TextMoveRight,
    TextMoveToEnd, TextMoveToStart, TextMoveUp, TextPaste, TextSelectAll, TextSelectDown,
    TextSelectLeft, TextSelectRight, TextSelectUp, TextSubmit,
};

fn main() {
    tracing_subscriber::fmt::init();

    let app = Application::with_platform(gpui_platform::current_platform(false))
        .with_assets(assets::SeoulAssets);

    app.on_reopen(|cx| {
        open_app_window(cx);
    });

    app.run(|cx: &mut App| {
        // Initialize settings before anything else
        let state = seoul_workspace::persistence::load_state().unwrap_or_default();
        SettingsStore::init(&state.projects, cx);

        cx.bind_keys([
            // Terminal-scoped
            KeyBinding::new("cmd-v", Paste, Some("terminal")),
            KeyBinding::new("cmd-c", Copy, Some("terminal")),
            // Text input-scoped
            KeyBinding::new("backspace", TextBackspace, Some("text-input")),
            KeyBinding::new("delete", TextDelete, Some("text-input")),
            KeyBinding::new("left", TextMoveLeft, Some("text-input")),
            KeyBinding::new("right", TextMoveRight, Some("text-input")),
            KeyBinding::new("up", TextMoveUp, Some("text-input")),
            KeyBinding::new("down", TextMoveDown, Some("text-input")),
            KeyBinding::new("shift-left", TextSelectLeft, Some("text-input")),
            KeyBinding::new("shift-right", TextSelectRight, Some("text-input")),
            KeyBinding::new("shift-up", TextSelectUp, Some("text-input")),
            KeyBinding::new("shift-down", TextSelectDown, Some("text-input")),
            KeyBinding::new("home", TextMoveToStart, Some("text-input")),
            KeyBinding::new("end", TextMoveToEnd, Some("text-input")),
            KeyBinding::new("cmd-a", TextSelectAll, Some("text-input")),
            KeyBinding::new("cmd-c", TextCopy, Some("text-input")),
            KeyBinding::new("cmd-v", TextPaste, Some("text-input")),
            KeyBinding::new("cmd-x", TextCut, Some("text-input")),
            KeyBinding::new("cmd-enter", TextSubmit, Some("text-input")),
            KeyBinding::new("ctrl-enter", TextSubmit, Some("text-input")),
            // Editor-scoped
            KeyBinding::new("cmd-s", Save, Some("editor")),
            KeyBinding::new("cmd-a", SelectAll, Some("editor")),
            KeyBinding::new("cmd-c", EditorCopy, Some("editor")),
            KeyBinding::new("cmd-v", EditorPaste, Some("editor")),
            KeyBinding::new("cmd-x", EditorCut, Some("editor")),
            KeyBinding::new("backspace", Backspace, Some("editor")),
            KeyBinding::new("delete", Delete, Some("editor")),
            KeyBinding::new("tab", Tab, Some("editor")),
            KeyBinding::new("left", MoveLeft, Some("editor")),
            KeyBinding::new("right", MoveRight, Some("editor")),
            KeyBinding::new("up", MoveUp, Some("editor")),
            KeyBinding::new("down", MoveDown, Some("editor")),
            KeyBinding::new("shift-left", SelectLeft, Some("editor")),
            KeyBinding::new("shift-right", SelectRight, Some("editor")),
            KeyBinding::new("shift-up", SelectUp, Some("editor")),
            KeyBinding::new("shift-down", SelectDown, Some("editor")),
            KeyBinding::new("home", MoveToLineStart, Some("editor")),
            KeyBinding::new("end", MoveToLineEnd, Some("editor")),
            KeyBinding::new("alt-left", MoveWordLeft, Some("editor")),
            KeyBinding::new("alt-right", MoveWordRight, Some("editor")),
            // App-scoped
            KeyBinding::new("cmd-t", NewTab, Some("app")),
            KeyBinding::new("cmd-w", CloseActiveTab, Some("app")),
            KeyBinding::new("cmd-b", ToggleSidebar, Some("app")),
            KeyBinding::new("cmd-e", ToggleFileTree, Some("app")),
            KeyBinding::new("cmd-n", AddProject, Some("app")),
            KeyBinding::new("cmd-d", SplitRight, Some("app")),
            KeyBinding::new("cmd-shift-d", SplitDown, Some("app")),
            KeyBinding::new("cmd-q", Quit, Some("app")),
            KeyBinding::new("cmd-,", OpenSettings, Some("app")),
            KeyBinding::new("cmd-shift-r", ToggleResourceMonitor, Some("app")),
        ]);

        open_app_window(cx);
    });
}

fn open_app_window(cx: &mut App) {
    // Open window IMMEDIATELY — daemon connection happens in background
    let state = seoul_workspace::persistence::load_state().unwrap_or_default();

    let app_settings = &cx.global::<SettingsStore>().global().app;
    let win_w = app_settings.window_width;
    let win_h = app_settings.window_height;

    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(win_w), px(win_h)),
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                // Title is set dynamically by the custom titlebar via
                // `window.set_window_title`. Leaving it `None` here avoids
                // a duplicated string when AppView hasn't yet computed one.
                title: None,
                // `traffic_light_position` is honored only when the titlebar
                // appears transparent — keep these two in sync.
                appears_transparent: true,
                traffic_light_position: Some(point(px(9.0), px(9.0))),
            }),
            ..Default::default()
        },
        |window, cx| {
            let view = cx.new(|cx| AppView::new(window, cx, state, None));
            view.focus_handle(cx).focus(window, cx);
            view
        },
    )
    .expect("Failed to open window");

    cx.activate(true);
}
