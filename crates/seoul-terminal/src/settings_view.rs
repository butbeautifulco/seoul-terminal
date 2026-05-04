use gpui::*;
use seoul_workspace::settings::SettingsStore;
use uuid::Uuid;

use crate::theme;

// -- Events --

#[derive(Clone, Debug)]
pub enum SettingsEvent {
    OpenSettingsFile { path: std::path::PathBuf },
}

impl EventEmitter<SettingsEvent> for SettingsView {}

// -- SettingsView --

pub struct SettingsView {
    focus_handle: FocusHandle,
    /// Which layer to display: None = Global, Some(id) = project
    selected_project: Option<Uuid>,
    /// Available projects: (id, name, path)
    projects: Vec<(Uuid, String, std::path::PathBuf)>,
    #[allow(dead_code)]
    _settings_observer: Subscription,
}

impl SettingsView {
    pub fn new(cx: &mut Context<Self>, projects: Vec<(Uuid, String, std::path::PathBuf)>) -> Self {
        let focus_handle = cx.focus_handle();
        let observer = cx.observe_global::<SettingsStore>(|_this, cx| {
            cx.notify();
        });
        Self {
            focus_handle,
            selected_project: None,
            projects,
            _settings_observer: observer,
        }
    }

    fn render_section_header(&self, title: &str, t: &theme::ThemeColors) -> Div {
        div()
            .mt(px(20.))
            .mb(px(8.))
            .mx(px(24.))
            .child(
                div()
                    .text_size(px(11.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(t.overlay0))
                    .child(title.to_string()),
            )
            .child(div().mt(px(4.)).h(px(1.)).bg(rgb(t.surface0)))
    }

    fn render_setting_row(
        &self,
        label: &str,
        value: String,
        is_overridden: bool,
        t: &theme::ThemeColors,
    ) -> Div {
        div()
            .px(px(24.))
            .py(px(5.))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(t.subtext0))
                    .min_w(px(160.))
                    .child(label.to_string()),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(if is_overridden {
                        rgb(t.blue)
                    } else {
                        rgb(t.text)
                    })
                    .child(value),
            )
    }
}

impl crate::item::Item for SettingsView {
    fn tab_title(&self, _cx: &App) -> String {
        "Settings".into()
    }

    fn tab_kind_id(&self) -> &'static str {
        "settings"
    }
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::theme(cx);
        let store = cx.global::<SettingsStore>();
        let settings = store.get(self.selected_project);

        // Determine which content layer has overrides
        let user_content = store.user_content();
        let project_content = self
            .selected_project
            .and_then(|id| store.project_content(id));

        // Terminal section
        let term = &settings.terminal;
        let term_overrides = user_content
            .terminal
            .as_ref()
            .map(|t| {
                (
                    t.font_family.is_some(),
                    t.font_size.is_some(),
                    t.scrollback_lines.is_some(),
                    t.padding.is_some(),
                )
            })
            .unwrap_or_default();

        // Editor section
        let ed = &settings.editor;
        let ed_overrides = user_content
            .editor
            .as_ref()
            .map(|e| {
                (
                    e.font_family.is_some(),
                    e.font_size.is_some(),
                    e.tab_size.is_some(),
                    e.show_line_numbers.is_some(),
                    e.word_wrap.is_some(),
                )
            })
            .unwrap_or_default();

        // Theme section
        let theme_name = &settings.theme.name;
        let theme_overridden = user_content
            .theme
            .as_ref()
            .map(|t| t.name.is_some())
            .unwrap_or(false);

        // App section
        let app = &settings.app;
        let app_overrides = user_content
            .app
            .as_ref()
            .map(|a| {
                (
                    a.sidebar_width.is_some(),
                    a.window_width.is_some(),
                    a.window_height.is_some(),
                )
            })
            .unwrap_or_default();

        // Clone values for closures
        let term_font = term.font_family.clone();
        let term_size = term.font_size;
        let term_scrollback = term.scrollback_lines;
        let term_padding = term.padding;
        let ed_font = ed.font_family.clone();
        let ed_size = ed.font_size;
        let ed_tab_size = ed.tab_size;
        let ed_line_numbers = ed.show_line_numbers;
        let ed_word_wrap = ed.word_wrap;
        let theme_name = theme_name.clone();
        let app_sidebar = app.sidebar_width;
        let app_w = app.window_width;
        let app_h = app.window_height;

        // Layer selector
        let layer_label = if let Some(pid) = self.selected_project {
            self.projects
                .iter()
                .find(|(id, _, _)| *id == pid)
                .map(|(_, name, _)| format!("Project: {name}"))
                .unwrap_or_else(|| "Global".into())
        } else {
            "Global".into()
        };

        let _ = project_content; // Will be used for project-level override detection later

        div()
            .id("settings-view")
            .key_context("settings")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(t.base))
            .overflow_y_scroll()
            // Header
            .child(
                div()
                    .px(px(24.))
                    .pt(px(20.))
                    .pb(px(8.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(18.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(t.text))
                            .child("Settings"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(t.overlay0))
                            .child(layer_label),
                    ),
            )
            // Terminal section
            .child(self.render_section_header("TERMINAL", &t))
            .child(self.render_setting_row("Font Family", term_font, term_overrides.0, &t))
            .child(self.render_setting_row(
                "Font Size",
                format!("{term_size}"),
                term_overrides.1,
                &t,
            ))
            .child(self.render_setting_row(
                "Scrollback Lines",
                format!("{term_scrollback}"),
                term_overrides.2,
                &t,
            ))
            .child(self.render_setting_row(
                "Padding",
                format!("{term_padding}"),
                term_overrides.3,
                &t,
            ))
            // Editor section
            .child(self.render_section_header("EDITOR", &t))
            .child(self.render_setting_row("Font Family", ed_font, ed_overrides.0, &t))
            .child(self.render_setting_row("Font Size", format!("{ed_size}"), ed_overrides.1, &t))
            .child(self.render_setting_row(
                "Tab Size",
                format!("{ed_tab_size}"),
                ed_overrides.2,
                &t,
            ))
            .child(self.render_setting_row(
                "Line Numbers",
                if ed_line_numbers { "On" } else { "Off" }.into(),
                ed_overrides.3,
                &t,
            ))
            .child(self.render_setting_row(
                "Word Wrap",
                if ed_word_wrap { "On" } else { "Off" }.into(),
                ed_overrides.4,
                &t,
            ))
            // Theme section
            .child(self.render_section_header("THEME", &t))
            .child(self.render_setting_row("Theme", theme_name, theme_overridden, &t))
            // Application section
            .child(self.render_section_header("APPLICATION", &t))
            .child(self.render_setting_row(
                "Sidebar Width",
                format!("{app_sidebar}"),
                app_overrides.0,
                &t,
            ))
            .child(self.render_setting_row(
                "Window Size",
                format!("{app_w} x {app_h}"),
                app_overrides.1 || app_overrides.2,
                &t,
            ))
            // Buttons
            .child(
                div()
                    .mt(px(24.))
                    .mb(px(20.))
                    .px(px(24.))
                    .flex()
                    .flex_row()
                    .gap(px(12.))
                    .child(
                        div()
                            .id("edit-settings-btn")
                            .cursor_pointer()
                            .px(px(16.))
                            .py(px(6.))
                            .rounded(px(4.))
                            .bg(rgb(t.surface0))
                            .text_size(px(12.))
                            .text_color(rgb(t.text))
                            .hover(|s| s.bg(rgb(t.surface1)))
                            .child("Edit settings.json")
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                let path = if let Some(pid) = this.selected_project {
                                    this.projects
                                        .iter()
                                        .find(|(id, _, _)| *id == pid)
                                        .map(|(_, _, p)| {
                                            seoul_workspace::settings::project_settings_file_path(p)
                                        })
                                        .unwrap_or_else(
                                            seoul_workspace::settings::user_settings_file_path,
                                        )
                                } else {
                                    seoul_workspace::settings::user_settings_file_path()
                                };
                                cx.emit(SettingsEvent::OpenSettingsFile { path });
                            })),
                    )
                    .child(
                        div()
                            .id("reset-defaults-btn")
                            .cursor_pointer()
                            .px(px(16.))
                            .py(px(6.))
                            .rounded(px(4.))
                            .bg(rgb(t.surface0))
                            .text_size(px(12.))
                            .text_color(rgb(t.red))
                            .hover(|s| s.bg(rgb(t.surface1)))
                            .child("Reset to Defaults")
                            .on_click(cx.listener(|_this, _, _window, cx| {
                                let path = seoul_workspace::settings::user_settings_file_path();
                                let _ = std::fs::remove_file(&path);
                                // Re-create skeleton with defaults
                                seoul_workspace::settings::write_default_skeleton();
                                // Trigger reload to pick up the fresh skeleton
                                SettingsStore::check_and_reload(cx);
                                cx.notify();
                            })),
                    ),
            )
    }
}
