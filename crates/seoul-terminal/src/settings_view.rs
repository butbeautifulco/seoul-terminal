use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use seoul_workspace::settings::{
    AppSettingsContent, BranchPrefixMode, EditorSettingsContent, ProjectFileTreeSettingsContent,
    ProjectGitSettingsContent, ProjectPrSettingsContent, ProjectSettingsContent,
    ProjectWorkspaceSettingsContent, SettingsContent, SettingsStore, SettingsTarget,
    TerminalSettingsContent, ThemeSettingsContent,
};
use uuid::Uuid;

use crate::icons::{Icon, IconName};
use crate::text_input::TextInput;
use crate::theme;

#[derive(Clone, Debug)]
pub enum SettingsEvent {
    OpenSettingsFile { path: PathBuf },
}

impl EventEmitter<SettingsEvent> for SettingsView {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsSection {
    Terminal,
    Editor,
    Theme,
    Application,
    ProjectDefaults,
    ProjectProfile,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingField {
    TerminalFontFamily,
    TerminalFontSize,
    TerminalScrollback,
    TerminalPadding,
    EditorFontFamily,
    EditorFontSize,
    EditorTabSize,
    ThemeName,
    AppSidebarWidth,
    AppWindowWidth,
    AppWindowHeight,
    ProjectDefaultBranch,
    ProjectWorktreeBaseDir,
    ProjectBranchPrefixCustom,
    ProjectExtraExcludes,
    EditorShowLineNumbers,
    EditorWordWrap,
    ProjectRespectGitignore,
    ProjectPrEnabled,
    ProjectBranchPrefixMode,
}

struct InlineEdit {
    target: SettingsTarget,
    field: SettingField,
    input: Entity<TextInput>,
}

pub struct SettingsView {
    focus_handle: FocusHandle,
    selected_project: Option<Uuid>,
    selected_section: SettingsSection,
    projects: Vec<(Uuid, String, PathBuf)>,
    inline_edit: Option<InlineEdit>,
    #[allow(dead_code)]
    _settings_observer: Subscription,
}

impl SettingsView {
    pub fn new(cx: &mut Context<Self>, projects: Vec<(Uuid, String, PathBuf)>) -> Self {
        let focus_handle = cx.focus_handle();
        let observer = cx.observe_global::<SettingsStore>(|_this, cx| {
            cx.notify();
        });
        Self {
            focus_handle,
            selected_project: None,
            selected_section: SettingsSection::Terminal,
            projects,
            inline_edit: None,
            _settings_observer: observer,
        }
    }

    fn current_target(&self) -> SettingsTarget {
        self.selected_project
            .map(SettingsTarget::Project)
            .unwrap_or(SettingsTarget::User)
    }

    fn effective_section(&self) -> SettingsSection {
        if self.selected_project.is_some() {
            SettingsSection::ProjectProfile
        } else if self.selected_section == SettingsSection::ProjectProfile {
            SettingsSection::Terminal
        } else {
            self.selected_section
        }
    }

    fn selected_project_label(&self) -> String {
        self.selected_project
            .and_then(|project_id| {
                self.projects
                    .iter()
                    .find(|(id, _, _)| *id == project_id)
                    .map(|(_, name, _)| name.clone())
            })
            .unwrap_or_else(|| "User".into())
    }

    fn selected_settings_path(&self) -> PathBuf {
        match self.current_target() {
            SettingsTarget::User => seoul_workspace::settings::user_settings_file_path(),
            SettingsTarget::Project(project_id) => self
                .projects
                .iter()
                .find(|(id, _, _)| *id == project_id)
                .map(|(_, _, path)| seoul_workspace::settings::project_settings_file_path(path))
                .unwrap_or_else(seoul_workspace::settings::user_settings_file_path),
        }
    }

    fn update_content(
        &self,
        target: SettingsTarget,
        update: impl FnOnce(&mut SettingsContent),
        cx: &mut Context<Self>,
    ) {
        let result =
            cx.update_global::<SettingsStore, _>(|store, _cx| store.update_content(target, update));
        if let Err(err) = result {
            tracing::warn!("failed to write settings: {err:#}");
        }
    }

    fn begin_edit(
        &mut self,
        target: SettingsTarget,
        field: SettingField,
        value: String,
        placeholder: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| TextInput::single_line(value, placeholder, cx));
        input.read(cx).focus_handle(cx).focus(window, cx);
        self.inline_edit = Some(InlineEdit {
            target,
            field,
            input,
        });
        cx.notify();
    }

    fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        self.inline_edit = None;
        cx.notify();
    }

    fn commit_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.inline_edit.take() else {
            return;
        };
        let text = edit.input.read(cx).text().trim().to_string();
        self.apply_text_field(edit.target, edit.field, &text, cx);
        cx.notify();
    }

    fn apply_text_field(
        &self,
        target: SettingsTarget,
        field: SettingField,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        match field {
            SettingField::TerminalFontFamily => self.update_content(
                target,
                |content| terminal_content(content).font_family = non_empty_string(text),
                cx,
            ),
            SettingField::TerminalFontSize => {
                if let Ok(value) = text.parse::<f32>() {
                    self.update_content(
                        target,
                        |content| terminal_content(content).font_size = Some(value.max(1.0)),
                        cx,
                    );
                }
            }
            SettingField::TerminalScrollback => {
                if let Ok(value) = text.parse::<usize>() {
                    self.update_content(
                        target,
                        |content| terminal_content(content).scrollback_lines = Some(value),
                        cx,
                    );
                }
            }
            SettingField::TerminalPadding => {
                if let Ok(value) = text.parse::<f32>() {
                    self.update_content(
                        target,
                        |content| terminal_content(content).padding = Some(value.max(0.0)),
                        cx,
                    );
                }
            }
            SettingField::EditorFontFamily => self.update_content(
                target,
                |content| editor_content(content).font_family = non_empty_string(text),
                cx,
            ),
            SettingField::EditorFontSize => {
                if let Ok(value) = text.parse::<f32>() {
                    self.update_content(
                        target,
                        |content| editor_content(content).font_size = Some(value.max(1.0)),
                        cx,
                    );
                }
            }
            SettingField::EditorTabSize => {
                if let Ok(value) = text.parse::<usize>() {
                    self.update_content(
                        target,
                        |content| editor_content(content).tab_size = Some(value.max(1)),
                        cx,
                    );
                }
            }
            SettingField::ThemeName => self.update_content(
                target,
                |content| theme_content(content).name = non_empty_string(text),
                cx,
            ),
            SettingField::AppSidebarWidth => {
                if let Ok(value) = text.parse::<f32>() {
                    self.update_content(
                        target,
                        |content| app_content(content).sidebar_width = Some(value.max(120.0)),
                        cx,
                    );
                }
            }
            SettingField::AppWindowWidth => {
                if let Ok(value) = text.parse::<f32>() {
                    self.update_content(
                        target,
                        |content| app_content(content).window_width = Some(value.max(400.0)),
                        cx,
                    );
                }
            }
            SettingField::AppWindowHeight => {
                if let Ok(value) = text.parse::<f32>() {
                    self.update_content(
                        target,
                        |content| app_content(content).window_height = Some(value.max(300.0)),
                        cx,
                    );
                }
            }
            SettingField::ProjectDefaultBranch => self.update_content(
                target,
                |content| project_git_content(content).default_branch = non_empty_string(text),
                cx,
            ),
            SettingField::ProjectWorktreeBaseDir => self.update_content(
                target,
                |content| {
                    project_workspace_content(content).worktree_base_dir =
                        non_empty_string(text).map(PathBuf::from)
                },
                cx,
            ),
            SettingField::ProjectBranchPrefixCustom => self.update_content(
                target,
                |content| {
                    project_workspace_content(content).branch_prefix_custom = non_empty_string(text)
                },
                cx,
            ),
            SettingField::ProjectExtraExcludes => self.update_content(
                target,
                |content| {
                    let values = split_list(text);
                    project_file_tree_content(content).extra_excludes =
                        (!values.is_empty()).then_some(values);
                },
                cx,
            ),
            _ => {}
        }
    }

    fn toggle_bool(
        &self,
        target: SettingsTarget,
        field: SettingField,
        current: bool,
        cx: &mut Context<Self>,
    ) {
        let next = !current;
        self.update_content(
            target,
            move |content| match field {
                SettingField::EditorShowLineNumbers => {
                    editor_content(content).show_line_numbers = Some(next)
                }
                SettingField::EditorWordWrap => editor_content(content).word_wrap = Some(next),
                SettingField::ProjectRespectGitignore => {
                    project_file_tree_content(content).respect_gitignore = Some(next)
                }
                SettingField::ProjectPrEnabled => project_pr_content(content).enabled = Some(next),
                _ => {}
            },
            cx,
        );
    }

    fn set_branch_prefix_mode(
        &self,
        target: SettingsTarget,
        mode: BranchPrefixMode,
        cx: &mut Context<Self>,
    ) {
        self.update_content(
            target,
            move |content| project_workspace_content(content).branch_prefix_mode = Some(mode),
            cx,
        );
    }

    fn reset_field(&self, target: SettingsTarget, field: SettingField, cx: &mut Context<Self>) {
        self.update_content(
            target,
            move |content| match field {
                SettingField::TerminalFontFamily => terminal_content(content).font_family = None,
                SettingField::TerminalFontSize => terminal_content(content).font_size = None,
                SettingField::TerminalScrollback => {
                    terminal_content(content).scrollback_lines = None
                }
                SettingField::TerminalPadding => terminal_content(content).padding = None,
                SettingField::EditorFontFamily => editor_content(content).font_family = None,
                SettingField::EditorFontSize => editor_content(content).font_size = None,
                SettingField::EditorTabSize => editor_content(content).tab_size = None,
                SettingField::EditorShowLineNumbers => {
                    editor_content(content).show_line_numbers = None
                }
                SettingField::EditorWordWrap => editor_content(content).word_wrap = None,
                SettingField::ThemeName => theme_content(content).name = None,
                SettingField::AppSidebarWidth => app_content(content).sidebar_width = None,
                SettingField::AppWindowWidth => app_content(content).window_width = None,
                SettingField::AppWindowHeight => app_content(content).window_height = None,
                SettingField::ProjectDefaultBranch => {
                    project_git_content(content).default_branch = None
                }
                SettingField::ProjectWorktreeBaseDir => {
                    project_workspace_content(content).worktree_base_dir = None
                }
                SettingField::ProjectBranchPrefixMode => {
                    project_workspace_content(content).branch_prefix_mode = None
                }
                SettingField::ProjectBranchPrefixCustom => {
                    project_workspace_content(content).branch_prefix_custom = None
                }
                SettingField::ProjectRespectGitignore => {
                    project_file_tree_content(content).respect_gitignore = None
                }
                SettingField::ProjectExtraExcludes => {
                    project_file_tree_content(content).extra_excludes = None
                }
                SettingField::ProjectPrEnabled => project_pr_content(content).enabled = None,
            },
            cx,
        );
    }

    fn reset_scope(&mut self, target: SettingsTarget, cx: &mut Context<Self>) {
        self.inline_edit = None;
        self.update_content(target, |content| *content = SettingsContent::default(), cx);
        cx.notify();
    }

    fn render_nav_button(
        &self,
        id: impl Into<ElementId>,
        label: String,
        active: bool,
        t: &theme::ThemeColors,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .px(px(10.))
            .py(px(7.))
            .rounded(px(4.))
            .cursor_pointer()
            .text_size(px(12.))
            .text_color(if active { rgb(t.text) } else { rgb(t.subtext0) })
            .when(active, |el| el.bg(rgb(t.surface0)))
            .hover(|s| s.bg(rgb(t.surface1)))
            .child(label)
    }

    fn render_sidebar(
        &self,
        section: SettingsSection,
        t: &theme::ThemeColors,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut scopes = div().flex().flex_col().gap(px(3.));
        scopes = scopes.child(
            self.render_nav_button(
                "settings-scope-user",
                "User".into(),
                self.selected_project.is_none(),
                t,
            )
            .on_click(cx.listener(|this, _, _window, cx| {
                this.selected_project = None;
                this.selected_section = SettingsSection::Terminal;
                this.inline_edit = None;
                cx.notify();
            })),
        );

        for (project_id, name, _) in &self.projects {
            let id = *project_id;
            scopes = scopes.child(
                self.render_nav_button(
                    ElementId::Name(format!("settings-scope-project-{id}").into()),
                    name.clone(),
                    self.selected_project == Some(id),
                    t,
                )
                .on_click(cx.listener(move |this, _, _window, cx| {
                    this.selected_project = Some(id);
                    this.selected_section = SettingsSection::ProjectProfile;
                    this.inline_edit = None;
                    cx.notify();
                })),
            );
        }

        let mut sections = div().flex().flex_col().gap(px(3.));
        if self.selected_project.is_some() {
            sections = sections.child(self.render_nav_button(
                "settings-section-project",
                "Project Profile".into(),
                true,
                t,
            ));
        } else {
            for (section_id, label) in [
                (SettingsSection::Terminal, "Terminal"),
                (SettingsSection::Editor, "Editor"),
                (SettingsSection::Theme, "Theme"),
                (SettingsSection::Application, "Application"),
                (SettingsSection::ProjectDefaults, "Project Defaults"),
            ] {
                sections = sections.child(
                    self.render_nav_button(
                        ElementId::Name(format!("settings-section-{section_id:?}").into()),
                        label.into(),
                        section == section_id,
                        t,
                    )
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.selected_section = section_id;
                        this.inline_edit = None;
                        cx.notify();
                    })),
                );
            }
        }

        div()
            .w(px(220.))
            .min_w(px(220.))
            .h_full()
            .border_r_1()
            .border_color(rgb(t.surface0))
            .bg(rgb(t.mantle))
            .px(px(10.))
            .py(px(12.))
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(sidebar_label("SCOPE", t))
            .child(scopes)
            .child(sidebar_label("SECTION", t))
            .child(sections)
    }

    fn render_header(
        &self,
        target: SettingsTarget,
        t: &theme::ThemeColors,
        cx: &mut Context<Self>,
    ) -> Div {
        let path = self.selected_settings_path();
        div()
            .flex_none()
            .px(px(22.))
            .py(px(16.))
            .border_b_1()
            .border_color(rgb(t.surface0))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .text_size(px(18.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(t.text))
                            .child(self.selected_project_label()),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(t.overlay0))
                            .child(path.display().to_string()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .id("edit-settings-json")
                            .cursor_pointer()
                            .px(px(10.))
                            .py(px(6.))
                            .rounded(px(4.))
                            .bg(rgb(t.surface0))
                            .hover(|s| s.bg(rgb(t.surface1)))
                            .text_size(px(12.))
                            .text_color(rgb(t.text))
                            .child("Edit JSON")
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                let path = this.selected_settings_path();
                                let target = this.current_target();
                                let _ = cx.update_global::<SettingsStore, _>(|store, _cx| {
                                    store.update_content(target, |_| {})
                                });
                                cx.emit(SettingsEvent::OpenSettingsFile { path });
                            })),
                    )
                    .child(
                        div()
                            .id("reset-settings-scope")
                            .cursor_pointer()
                            .px(px(10.))
                            .py(px(6.))
                            .rounded(px(4.))
                            .bg(rgb(t.surface0))
                            .hover(|s| s.bg(rgb(t.surface1)))
                            .text_size(px(12.))
                            .text_color(rgb(t.red))
                            .child("Reset Scope")
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.reset_scope(target, cx);
                            })),
                    ),
            )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_text_row(
        &self,
        target: SettingsTarget,
        field: SettingField,
        label: &'static str,
        value: String,
        placeholder: &'static str,
        overridden: bool,
        t: &theme::ThemeColors,
        cx: &mut Context<Self>,
    ) -> Div {
        self.render_text_row_with_edit_value(
            target,
            field,
            label,
            value.clone(),
            value,
            placeholder,
            overridden,
            t,
            cx,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_text_row_with_edit_value(
        &self,
        target: SettingsTarget,
        field: SettingField,
        label: &'static str,
        value: String,
        edit_value: String,
        placeholder: &'static str,
        overridden: bool,
        t: &theme::ThemeColors,
        cx: &mut Context<Self>,
    ) -> Div {
        let editing = self
            .inline_edit
            .as_ref()
            .filter(|edit| edit.target == target && edit.field == field);

        let mut control = div().flex().flex_row().items_center().gap(px(6.));
        if let Some(edit) = editing {
            control = control
                .child(div().w(px(260.)).child(edit.input.clone()))
                .child(
                    icon_button("save-setting", IconName::Check, rgb(t.green), t).on_click(
                        cx.listener(|this, _, _window, cx| {
                            this.commit_edit(cx);
                        }),
                    ),
                )
                .child(
                    icon_button("cancel-setting", IconName::X, rgb(t.overlay0), t).on_click(
                        cx.listener(|this, _, _window, cx| {
                            this.cancel_edit(cx);
                        }),
                    ),
                );
        } else {
            control = control.child(value_pill(value, overridden, t)).child(
                div()
                    .id(ElementId::Name(format!("edit-{field:?}").into()))
                    .cursor_pointer()
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(4.))
                    .bg(rgb(t.surface0))
                    .hover(|s| s.bg(rgb(t.surface1)))
                    .text_size(px(11.))
                    .text_color(rgb(t.subtext0))
                    .child("Edit")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.begin_edit(target, field, edit_value.clone(), placeholder, window, cx);
                    })),
            );
            if overridden {
                control = control.child(reset_button(field, target, t, cx));
            }
        }

        setting_row(label, t).child(control)
    }

    #[allow(clippy::too_many_arguments)]
    fn render_bool_row(
        &self,
        target: SettingsTarget,
        field: SettingField,
        label: &'static str,
        value: bool,
        overridden: bool,
        t: &theme::ThemeColors,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut control = div().flex().flex_row().items_center().gap(px(6.)).child(
            div()
                .id(ElementId::Name(format!("toggle-{field:?}").into()))
                .cursor_pointer()
                .w(px(54.))
                .h(px(24.))
                .rounded(px(12.))
                .bg(if value { rgb(t.blue) } else { rgb(t.surface0) })
                .border_1()
                .border_color(if overridden {
                    rgb(t.blue)
                } else {
                    rgb(t.surface1)
                })
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.))
                .text_color(if value { rgb(t.base) } else { rgb(t.subtext0) })
                .child(if value { "On" } else { "Off" })
                .on_click(cx.listener(move |this, _, _window, cx| {
                    this.toggle_bool(target, field, value, cx);
                })),
        );
        if overridden {
            control = control.child(reset_button(field, target, t, cx));
        }
        setting_row(label, t).child(control)
    }

    fn render_branch_prefix_row(
        &self,
        target: SettingsTarget,
        value: BranchPrefixMode,
        overridden: bool,
        t: &theme::ThemeColors,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut control = div().flex().flex_row().items_center().gap(px(4.));
        for (mode, label) in [
            (BranchPrefixMode::Github, "GitHub"),
            (BranchPrefixMode::Author, "Author"),
            (BranchPrefixMode::Custom, "Custom"),
            (BranchPrefixMode::None, "None"),
        ] {
            let active = value == mode;
            control = control.child(
                div()
                    .id(ElementId::Name(format!("prefix-mode-{mode:?}").into()))
                    .cursor_pointer()
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(4.))
                    .bg(if active { rgb(t.blue) } else { rgb(t.surface0) })
                    .hover(|s| s.bg(if active { rgb(t.blue) } else { rgb(t.surface1) }))
                    .text_size(px(11.))
                    .text_color(if active { rgb(t.base) } else { rgb(t.subtext0) })
                    .child(label)
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.set_branch_prefix_mode(target, mode, cx);
                    })),
            );
        }
        if overridden {
            control = control.child(reset_button(
                SettingField::ProjectBranchPrefixMode,
                target,
                t,
                cx,
            ));
        }
        setting_row("Branch Prefix Mode", t).child(control)
    }

    fn render_terminal_section(
        &self,
        settings: &seoul_workspace::settings::Settings,
        content: &SettingsContent,
        target: SettingsTarget,
        t: &theme::ThemeColors,
        cx: &mut Context<Self>,
    ) -> Div {
        let term = &settings.terminal;
        div()
            .flex()
            .flex_col()
            .child(section_title("Terminal", t))
            .child(
                self.render_text_row(
                    target,
                    SettingField::TerminalFontFamily,
                    "Font Family",
                    term.font_family.clone(),
                    "Menlo",
                    content
                        .terminal
                        .as_ref()
                        .is_some_and(|value| value.font_family.is_some()),
                    t,
                    cx,
                ),
            )
            .child(
                self.render_text_row(
                    target,
                    SettingField::TerminalFontSize,
                    "Font Size",
                    format_float(term.font_size),
                    "13",
                    content
                        .terminal
                        .as_ref()
                        .is_some_and(|value| value.font_size.is_some()),
                    t,
                    cx,
                ),
            )
            .child(
                self.render_text_row(
                    target,
                    SettingField::TerminalScrollback,
                    "Scrollback Lines",
                    term.scrollback_lines.to_string(),
                    "10000",
                    content
                        .terminal
                        .as_ref()
                        .is_some_and(|value| value.scrollback_lines.is_some()),
                    t,
                    cx,
                ),
            )
            .child(
                self.render_text_row(
                    target,
                    SettingField::TerminalPadding,
                    "Padding",
                    format_float(term.padding),
                    "4",
                    content
                        .terminal
                        .as_ref()
                        .is_some_and(|value| value.padding.is_some()),
                    t,
                    cx,
                ),
            )
    }

    fn render_editor_section(
        &self,
        settings: &seoul_workspace::settings::Settings,
        content: &SettingsContent,
        target: SettingsTarget,
        t: &theme::ThemeColors,
        cx: &mut Context<Self>,
    ) -> Div {
        let editor = &settings.editor;
        div()
            .flex()
            .flex_col()
            .child(section_title("Editor", t))
            .child(
                self.render_text_row(
                    target,
                    SettingField::EditorFontFamily,
                    "Font Family",
                    editor.font_family.clone(),
                    "Menlo",
                    content
                        .editor
                        .as_ref()
                        .is_some_and(|value| value.font_family.is_some()),
                    t,
                    cx,
                ),
            )
            .child(
                self.render_text_row(
                    target,
                    SettingField::EditorFontSize,
                    "Font Size",
                    format_float(editor.font_size),
                    "13",
                    content
                        .editor
                        .as_ref()
                        .is_some_and(|value| value.font_size.is_some()),
                    t,
                    cx,
                ),
            )
            .child(
                self.render_text_row(
                    target,
                    SettingField::EditorTabSize,
                    "Tab Size",
                    editor.tab_size.to_string(),
                    "4",
                    content
                        .editor
                        .as_ref()
                        .is_some_and(|value| value.tab_size.is_some()),
                    t,
                    cx,
                ),
            )
            .child(
                self.render_bool_row(
                    target,
                    SettingField::EditorShowLineNumbers,
                    "Line Numbers",
                    editor.show_line_numbers,
                    content
                        .editor
                        .as_ref()
                        .is_some_and(|value| value.show_line_numbers.is_some()),
                    t,
                    cx,
                ),
            )
            .child(
                self.render_bool_row(
                    target,
                    SettingField::EditorWordWrap,
                    "Word Wrap",
                    editor.word_wrap,
                    content
                        .editor
                        .as_ref()
                        .is_some_and(|value| value.word_wrap.is_some()),
                    t,
                    cx,
                ),
            )
    }

    fn render_theme_section(
        &self,
        settings: &seoul_workspace::settings::Settings,
        content: &SettingsContent,
        target: SettingsTarget,
        t: &theme::ThemeColors,
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .flex()
            .flex_col()
            .child(section_title("Theme", t))
            .child(
                self.render_text_row(
                    target,
                    SettingField::ThemeName,
                    "Theme Name",
                    settings.theme.name.clone(),
                    "modern-dark",
                    content
                        .theme
                        .as_ref()
                        .is_some_and(|value| value.name.is_some()),
                    t,
                    cx,
                ),
            )
    }

    fn render_application_section(
        &self,
        settings: &seoul_workspace::settings::Settings,
        content: &SettingsContent,
        target: SettingsTarget,
        t: &theme::ThemeColors,
        cx: &mut Context<Self>,
    ) -> Div {
        let app = &settings.app;
        div()
            .flex()
            .flex_col()
            .child(section_title("Application", t))
            .child(
                self.render_text_row(
                    target,
                    SettingField::AppSidebarWidth,
                    "Sidebar Width",
                    format_float(app.sidebar_width),
                    "220",
                    content
                        .app
                        .as_ref()
                        .is_some_and(|value| value.sidebar_width.is_some()),
                    t,
                    cx,
                ),
            )
            .child(
                self.render_text_row(
                    target,
                    SettingField::AppWindowWidth,
                    "Window Width",
                    format_float(app.window_width),
                    "1200",
                    content
                        .app
                        .as_ref()
                        .is_some_and(|value| value.window_width.is_some()),
                    t,
                    cx,
                ),
            )
            .child(
                self.render_text_row(
                    target,
                    SettingField::AppWindowHeight,
                    "Window Height",
                    format_float(app.window_height),
                    "700",
                    content
                        .app
                        .as_ref()
                        .is_some_and(|value| value.window_height.is_some()),
                    t,
                    cx,
                ),
            )
    }

    fn render_project_section(
        &self,
        settings: &seoul_workspace::settings::Settings,
        content: &SettingsContent,
        target: SettingsTarget,
        title: &'static str,
        t: &theme::ThemeColors,
        cx: &mut Context<Self>,
    ) -> Div {
        let project = &settings.project;
        let project_content = content.project.as_ref();
        let workspace_content = project_content.and_then(|value| value.workspaces.as_ref());
        let file_tree_content = project_content.and_then(|value| value.file_tree.as_ref());
        let branch = project
            .git
            .default_branch
            .clone()
            .unwrap_or_else(|| "Project default".into());
        let branch_edit = project.git.default_branch.clone().unwrap_or_default();
        let worktree_base_dir = project
            .workspaces
            .worktree_base_dir
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "~/.seoul/worktrees".into());
        let worktree_base_dir_edit = project
            .workspaces
            .worktree_base_dir
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let extra_excludes = project.file_tree.extra_excludes.join(", ");

        div()
            .flex()
            .flex_col()
            .child(section_title(title, t))
            .child(
                self.render_text_row_with_edit_value(
                    target,
                    SettingField::ProjectDefaultBranch,
                    "Default Branch",
                    branch,
                    branch_edit,
                    "main",
                    project_content
                        .and_then(|value| value.git.as_ref())
                        .is_some_and(|value| value.default_branch.is_some()),
                    t,
                    cx,
                ),
            )
            .child(self.render_text_row_with_edit_value(
                target,
                SettingField::ProjectWorktreeBaseDir,
                "Worktree Base Dir",
                worktree_base_dir,
                worktree_base_dir_edit,
                "~/.seoul/worktrees",
                workspace_content.is_some_and(|value| value.worktree_base_dir.is_some()),
                t,
                cx,
            ))
            .child(self.render_branch_prefix_row(
                target,
                project.workspaces.branch_prefix_mode,
                workspace_content.is_some_and(|value| value.branch_prefix_mode.is_some()),
                t,
                cx,
            ))
            .child(self.render_text_row_with_edit_value(
                target,
                SettingField::ProjectBranchPrefixCustom,
                "Custom Prefix",
                empty_fallback(&project.workspaces.branch_prefix_custom, "None"),
                project.workspaces.branch_prefix_custom.clone(),
                "feature",
                workspace_content.is_some_and(|value| value.branch_prefix_custom.is_some()),
                t,
                cx,
            ))
            .child(self.render_bool_row(
                target,
                SettingField::ProjectRespectGitignore,
                "Respect .gitignore",
                project.file_tree.respect_gitignore,
                file_tree_content.is_some_and(|value| value.respect_gitignore.is_some()),
                t,
                cx,
            ))
            .child(self.render_text_row_with_edit_value(
                target,
                SettingField::ProjectExtraExcludes,
                "Extra Excludes",
                empty_fallback(&extra_excludes, "None"),
                extra_excludes,
                "tmp, logs, *.snap",
                file_tree_content.is_some_and(|value| value.extra_excludes.is_some()),
                t,
                cx,
            ))
            .child(
                self.render_bool_row(
                    target,
                    SettingField::ProjectPrEnabled,
                    "Pull Request Sync",
                    project.pr.enabled,
                    project_content
                        .and_then(|value| value.pr.as_ref())
                        .is_some_and(|value| value.enabled.is_some()),
                    t,
                    cx,
                ),
            )
    }
}

impl crate::item::Item for SettingsView {
    fn tab_title(&self, _cx: &App) -> String {
        "Settings".into()
    }

    fn tab_kind(&self) -> crate::tab_kind::TabKind {
        crate::tab_kind::TabKind::Settings
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
        let target = self.current_target();
        let section = self.effective_section();
        let store = cx.global::<SettingsStore>();
        let settings = store
            .resolved_for_target(target)
            .unwrap_or_else(|| store.global())
            .clone();
        let content = store
            .content_for_target(target)
            .cloned()
            .unwrap_or_default();

        let body = match section {
            SettingsSection::Terminal => {
                self.render_terminal_section(&settings, &content, target, &t, cx)
            }
            SettingsSection::Editor => {
                self.render_editor_section(&settings, &content, target, &t, cx)
            }
            SettingsSection::Theme => {
                self.render_theme_section(&settings, &content, target, &t, cx)
            }
            SettingsSection::Application => {
                self.render_application_section(&settings, &content, target, &t, cx)
            }
            SettingsSection::ProjectDefaults => {
                self.render_project_section(&settings, &content, target, "Project Defaults", &t, cx)
            }
            SettingsSection::ProjectProfile => {
                self.render_project_section(&settings, &content, target, "Project Profile", &t, cx)
            }
        };

        div()
            .id("settings-view")
            .key_context("settings")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_row()
            .bg(rgb(t.base))
            .child(self.render_sidebar(section, &t, cx))
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.render_header(target, &t, cx))
                    .child(
                        div()
                            .id("settings-body-scroll")
                            .flex_1()
                            .overflow_y_scroll()
                            .px(px(22.))
                            .py(px(18.))
                            .child(body),
                    ),
            )
    }
}

fn terminal_content(content: &mut SettingsContent) -> &mut TerminalSettingsContent {
    content.terminal.get_or_insert_with(Default::default)
}

fn editor_content(content: &mut SettingsContent) -> &mut EditorSettingsContent {
    content.editor.get_or_insert_with(Default::default)
}

fn theme_content(content: &mut SettingsContent) -> &mut ThemeSettingsContent {
    content.theme.get_or_insert_with(Default::default)
}

fn app_content(content: &mut SettingsContent) -> &mut AppSettingsContent {
    content.app.get_or_insert_with(Default::default)
}

fn project_content(content: &mut SettingsContent) -> &mut ProjectSettingsContent {
    content.project.get_or_insert_with(Default::default)
}

fn project_git_content(content: &mut SettingsContent) -> &mut ProjectGitSettingsContent {
    project_content(content)
        .git
        .get_or_insert_with(Default::default)
}

fn project_workspace_content(
    content: &mut SettingsContent,
) -> &mut ProjectWorkspaceSettingsContent {
    project_content(content)
        .workspaces
        .get_or_insert_with(Default::default)
}

fn project_file_tree_content(content: &mut SettingsContent) -> &mut ProjectFileTreeSettingsContent {
    project_content(content)
        .file_tree
        .get_or_insert_with(Default::default)
}

fn project_pr_content(content: &mut SettingsContent) -> &mut ProjectPrSettingsContent {
    project_content(content)
        .pr
        .get_or_insert_with(Default::default)
}

fn non_empty_string(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn split_list(text: &str) -> Vec<String> {
    let mut values = Vec::new();
    for item in text.split([',', '\n']) {
        let item = item.trim();
        if !item.is_empty() && !values.iter().any(|value: &String| value == item) {
            values.push(item.to_string());
        }
    }
    values
}

fn format_float(value: f32) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

fn empty_fallback(value: &str, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

fn sidebar_label(label: &'static str, t: &theme::ThemeColors) -> Div {
    div()
        .text_size(px(10.))
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(t.overlay0))
        .child(label)
}

fn section_title(title: &'static str, t: &theme::ThemeColors) -> Div {
    div()
        .pb(px(10.))
        .mb(px(4.))
        .border_b_1()
        .border_color(rgb(t.surface0))
        .text_size(px(16.))
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(t.text))
        .child(title)
}

fn setting_row(label: &'static str, t: &theme::ThemeColors) -> Div {
    div()
        .min_h(px(42.))
        .py(px(7.))
        .border_b_1()
        .border_color(rgb(t.surface0))
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(14.))
        .child(
            div()
                .min_w(px(180.))
                .text_size(px(12.))
                .text_color(rgb(t.subtext0))
                .child(label),
        )
}

fn value_pill(value: String, overridden: bool, t: &theme::ThemeColors) -> Div {
    div()
        .max_w(px(320.))
        .px(px(8.))
        .py(px(4.))
        .rounded(px(4.))
        .bg(rgb(t.surface0))
        .border_1()
        .border_color(if overridden {
            rgb(t.blue)
        } else {
            rgb(t.surface1)
        })
        .text_size(px(11.))
        .text_color(if overridden { rgb(t.blue) } else { rgb(t.text) })
        .overflow_hidden()
        .child(value)
}

fn icon_button(
    id: impl Into<ElementId>,
    icon: IconName,
    color: Rgba,
    t: &theme::ThemeColors,
) -> Stateful<Div> {
    div()
        .id(id)
        .cursor_pointer()
        .size(px(24.))
        .rounded(px(4.))
        .bg(rgb(t.surface0))
        .hover(|s| s.bg(rgb(t.surface1)))
        .flex()
        .items_center()
        .justify_center()
        .child(Icon::new(icon, color).size(px(13.)))
}

fn reset_button(
    field: SettingField,
    target: SettingsTarget,
    t: &theme::ThemeColors,
    cx: &mut Context<SettingsView>,
) -> Stateful<Div> {
    icon_button(
        ElementId::Name(format!("reset-{field:?}").into()),
        IconName::X,
        rgb(t.red),
        t,
    )
    .on_click(cx.listener(move |this, _, _window, cx| {
        this.reset_field(target, field, cx);
    }))
}
