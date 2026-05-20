use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context as _, Result};
use gpui::{App, BorrowAppContext, Global};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::project::Project;

// ---------------------------------------------------------------------------
// SettingsContent — override layer (all fields Option for merge semantics)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SettingsContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<TerminalSettingsContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor: Option<EditorSettingsContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<ThemeSettingsContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<AppSettingsContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectSettingsContent>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TerminalSettingsContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrollback_lines: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub padding: Option<f32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EditorSettingsContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_size: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_line_numbers: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_wrap: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThemeSettingsContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppSettingsContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar_width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_height: Option<f32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectSettingsContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<ProjectGitSettingsContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspaces: Option<ProjectWorkspaceSettingsContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_tree: Option<ProjectFileTreeSettingsContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<ProjectPrSettingsContent>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectGitSettingsContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectWorkspaceSettingsContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_base_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_prefix_mode: Option<BranchPrefixMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_prefix_custom: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectFileTreeSettingsContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub respect_gitignore: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_excludes: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectPrSettingsContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchPrefixMode {
    Github,
    Author,
    Custom,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTarget {
    User,
    Project(Uuid),
}

impl SettingsContent {
    /// Merge `other` on top of `self`. `Some` values in `other` override `self`.
    pub fn merge_from(&mut self, other: &SettingsContent) {
        // terminal
        if let Some(other_t) = &other.terminal {
            let t = self.terminal.get_or_insert_with(Default::default);
            if other_t.font_family.is_some() {
                t.font_family = other_t.font_family.clone();
            }
            if other_t.font_size.is_some() {
                t.font_size = other_t.font_size;
            }
            if other_t.scrollback_lines.is_some() {
                t.scrollback_lines = other_t.scrollback_lines;
            }
            if other_t.padding.is_some() {
                t.padding = other_t.padding;
            }
        }
        // editor
        if let Some(other_e) = &other.editor {
            let e = self.editor.get_or_insert_with(Default::default);
            if other_e.font_family.is_some() {
                e.font_family = other_e.font_family.clone();
            }
            if other_e.font_size.is_some() {
                e.font_size = other_e.font_size;
            }
            if other_e.tab_size.is_some() {
                e.tab_size = other_e.tab_size;
            }
            if other_e.show_line_numbers.is_some() {
                e.show_line_numbers = other_e.show_line_numbers;
            }
            if other_e.word_wrap.is_some() {
                e.word_wrap = other_e.word_wrap;
            }
        }
        // theme
        if let Some(other_th) = &other.theme {
            let th = self.theme.get_or_insert_with(Default::default);
            if other_th.name.is_some() {
                th.name = other_th.name.clone();
            }
        }
        // app
        if let Some(other_a) = &other.app {
            let a = self.app.get_or_insert_with(Default::default);
            if other_a.sidebar_width.is_some() {
                a.sidebar_width = other_a.sidebar_width;
            }
            if other_a.window_width.is_some() {
                a.window_width = other_a.window_width;
            }
            if other_a.window_height.is_some() {
                a.window_height = other_a.window_height;
            }
        }
        self.merge_project_from(other);
    }

    /// Merge only project-profile settings from `other`.
    ///
    /// Project-local settings files intentionally cannot override global
    /// editor, terminal, theme, or app settings.
    pub fn merge_project_from(&mut self, other: &SettingsContent) {
        if let Some(other_p) = &other.project {
            let p = self.project.get_or_insert_with(Default::default);
            if let Some(other_git) = &other_p.git {
                let git = p.git.get_or_insert_with(Default::default);
                if other_git.default_branch.is_some() {
                    git.default_branch = other_git.default_branch.clone();
                }
            }
            if let Some(other_ws) = &other_p.workspaces {
                let ws = p.workspaces.get_or_insert_with(Default::default);
                if other_ws.worktree_base_dir.is_some() {
                    ws.worktree_base_dir = other_ws.worktree_base_dir.clone();
                }
                if other_ws.branch_prefix_mode.is_some() {
                    ws.branch_prefix_mode = other_ws.branch_prefix_mode;
                }
                if other_ws.branch_prefix_custom.is_some() {
                    ws.branch_prefix_custom = other_ws.branch_prefix_custom.clone();
                }
            }
            if let Some(other_ft) = &other_p.file_tree {
                let ft = p.file_tree.get_or_insert_with(Default::default);
                if other_ft.respect_gitignore.is_some() {
                    ft.respect_gitignore = other_ft.respect_gitignore;
                }
                if other_ft.extra_excludes.is_some() {
                    ft.extra_excludes = other_ft.extra_excludes.clone();
                }
            }
            if let Some(other_pr) = &other_p.pr {
                let pr = p.pr.get_or_insert_with(Default::default);
                if other_pr.enabled.is_some() {
                    pr.enabled = other_pr.enabled;
                }
            }
        }
    }

    fn prune_empty(&mut self) {
        if self
            .terminal
            .as_ref()
            .is_some_and(TerminalSettingsContent::is_empty)
        {
            self.terminal = None;
        }
        if self
            .editor
            .as_ref()
            .is_some_and(EditorSettingsContent::is_empty)
        {
            self.editor = None;
        }
        if self
            .theme
            .as_ref()
            .is_some_and(ThemeSettingsContent::is_empty)
        {
            self.theme = None;
        }
        if self.app.as_ref().is_some_and(AppSettingsContent::is_empty) {
            self.app = None;
        }
        if let Some(project) = &mut self.project {
            project.prune_empty();
        }
        if self
            .project
            .as_ref()
            .is_some_and(ProjectSettingsContent::is_empty)
        {
            self.project = None;
        }
    }
}

impl TerminalSettingsContent {
    fn is_empty(&self) -> bool {
        self.font_family.is_none()
            && self.font_size.is_none()
            && self.scrollback_lines.is_none()
            && self.padding.is_none()
    }
}

impl EditorSettingsContent {
    fn is_empty(&self) -> bool {
        self.font_family.is_none()
            && self.font_size.is_none()
            && self.tab_size.is_none()
            && self.show_line_numbers.is_none()
            && self.word_wrap.is_none()
    }
}

impl ThemeSettingsContent {
    fn is_empty(&self) -> bool {
        self.name.is_none()
    }
}

impl AppSettingsContent {
    fn is_empty(&self) -> bool {
        self.sidebar_width.is_none() && self.window_width.is_none() && self.window_height.is_none()
    }
}

impl ProjectSettingsContent {
    fn prune_empty(&mut self) {
        if self
            .git
            .as_ref()
            .is_some_and(ProjectGitSettingsContent::is_empty)
        {
            self.git = None;
        }
        if self
            .workspaces
            .as_ref()
            .is_some_and(ProjectWorkspaceSettingsContent::is_empty)
        {
            self.workspaces = None;
        }
        if self
            .file_tree
            .as_ref()
            .is_some_and(ProjectFileTreeSettingsContent::is_empty)
        {
            self.file_tree = None;
        }
        if self
            .pr
            .as_ref()
            .is_some_and(ProjectPrSettingsContent::is_empty)
        {
            self.pr = None;
        }
    }

    fn is_empty(&self) -> bool {
        self.git.is_none()
            && self.workspaces.is_none()
            && self.file_tree.is_none()
            && self.pr.is_none()
    }
}

impl ProjectGitSettingsContent {
    fn is_empty(&self) -> bool {
        self.default_branch.is_none()
    }
}

impl ProjectWorkspaceSettingsContent {
    fn is_empty(&self) -> bool {
        self.worktree_base_dir.is_none()
            && self.branch_prefix_mode.is_none()
            && self.branch_prefix_custom.is_none()
    }
}

impl ProjectFileTreeSettingsContent {
    fn is_empty(&self) -> bool {
        self.respect_gitignore.is_none() && self.extra_excludes.is_none()
    }
}

impl ProjectPrSettingsContent {
    fn is_empty(&self) -> bool {
        self.enabled.is_none()
    }
}

// ---------------------------------------------------------------------------
// Settings — resolved concrete types (no Option fields)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Settings {
    pub terminal: TerminalSettings,
    pub editor: EditorSettings,
    pub theme: ThemeSettings,
    pub app: AppSettings,
    pub project: ProjectSettings,
}

#[derive(Debug, Clone)]
pub struct TerminalSettings {
    pub font_family: String,
    pub font_size: f32,
    pub scrollback_lines: usize,
    pub padding: f32,
}

#[derive(Debug, Clone)]
pub struct EditorSettings {
    pub font_family: String,
    pub font_size: f32,
    pub tab_size: usize,
    pub show_line_numbers: bool,
    pub word_wrap: bool,
}

#[derive(Debug, Clone)]
pub struct ThemeSettings {
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct AppSettings {
    pub sidebar_width: f32,
    pub window_width: f32,
    pub window_height: f32,
}

#[derive(Debug, Clone)]
pub struct ProjectSettings {
    pub git: ProjectGitSettings,
    pub workspaces: ProjectWorkspaceSettings,
    pub file_tree: ProjectFileTreeSettings,
    pub pr: ProjectPrSettings,
}

#[derive(Debug, Clone)]
pub struct ProjectGitSettings {
    pub default_branch: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProjectWorkspaceSettings {
    pub worktree_base_dir: Option<PathBuf>,
    pub branch_prefix_mode: BranchPrefixMode,
    pub branch_prefix_custom: String,
}

#[derive(Debug, Clone)]
pub struct ProjectFileTreeSettings {
    pub respect_gitignore: bool,
    pub extra_excludes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ProjectPrSettings {
    pub enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            terminal: TerminalSettings {
                font_family: "Menlo".into(),
                font_size: 13.0,
                scrollback_lines: 10_000,
                padding: 4.0,
            },
            editor: EditorSettings {
                font_family: "Menlo".into(),
                font_size: 13.0,
                tab_size: 4,
                show_line_numbers: true,
                word_wrap: false,
            },
            theme: ThemeSettings {
                name: "modern-dark".into(),
            },
            app: AppSettings {
                sidebar_width: 220.0,
                window_width: 1200.0,
                window_height: 700.0,
            },
            project: ProjectSettings {
                git: ProjectGitSettings {
                    default_branch: None,
                },
                workspaces: ProjectWorkspaceSettings {
                    worktree_base_dir: None,
                    branch_prefix_mode: BranchPrefixMode::Github,
                    branch_prefix_custom: String::new(),
                },
                file_tree: ProjectFileTreeSettings {
                    respect_gitignore: true,
                    extra_excludes: Vec::new(),
                },
                pr: ProjectPrSettings { enabled: true },
            },
        }
    }
}

impl Settings {
    /// Build resolved settings by applying a `SettingsContent` on top of defaults.
    fn from_content(content: &SettingsContent) -> Self {
        let mut s = Self::default();
        if let Some(t) = &content.terminal {
            if let Some(v) = &t.font_family {
                s.terminal.font_family = v.clone();
            }
            if let Some(v) = t.font_size {
                s.terminal.font_size = v;
            }
            if let Some(v) = t.scrollback_lines {
                s.terminal.scrollback_lines = v;
            }
            if let Some(v) = t.padding {
                s.terminal.padding = v;
            }
        }
        if let Some(e) = &content.editor {
            if let Some(v) = &e.font_family {
                s.editor.font_family = v.clone();
            }
            if let Some(v) = e.font_size {
                s.editor.font_size = v;
            }
            if let Some(v) = e.tab_size {
                s.editor.tab_size = v;
            }
            if let Some(v) = e.show_line_numbers {
                s.editor.show_line_numbers = v;
            }
            if let Some(v) = e.word_wrap {
                s.editor.word_wrap = v;
            }
        }
        if let Some(th) = &content.theme
            && let Some(v) = &th.name
        {
            s.theme.name = v.clone();
        }
        if let Some(a) = &content.app {
            if let Some(v) = a.sidebar_width {
                s.app.sidebar_width = v;
            }
            if let Some(v) = a.window_width {
                s.app.window_width = v;
            }
            if let Some(v) = a.window_height {
                s.app.window_height = v;
            }
        }
        if let Some(p) = &content.project {
            if let Some(git) = &p.git
                && let Some(v) = &git.default_branch
            {
                let trimmed = v.trim();
                s.project.git.default_branch = (!trimmed.is_empty()).then(|| trimmed.to_string());
            }
            if let Some(ws) = &p.workspaces {
                if let Some(v) = &ws.worktree_base_dir {
                    s.project.workspaces.worktree_base_dir = Some(v.clone());
                }
                if let Some(v) = ws.branch_prefix_mode {
                    s.project.workspaces.branch_prefix_mode = v;
                }
                if let Some(v) = &ws.branch_prefix_custom {
                    s.project.workspaces.branch_prefix_custom = v.trim().to_string();
                }
            }
            if let Some(ft) = &p.file_tree {
                if let Some(v) = ft.respect_gitignore {
                    s.project.file_tree.respect_gitignore = v;
                }
                if let Some(v) = &ft.extra_excludes {
                    s.project.file_tree.extra_excludes =
                        normalized_non_empty_strings(v.iter().map(String::as_str));
                }
            }
            if let Some(pr) = &p.pr
                && let Some(v) = pr.enabled
            {
                s.project.pr.enabled = v;
            }
        }
        s
    }

    /// Build project-context settings. User/global settings are the base, but
    /// project-local files only override the project-profile section.
    fn from_user_and_project_content(
        user_content: &SettingsContent,
        project_content: &SettingsContent,
    ) -> Self {
        let mut merged = user_content.clone();
        merged.merge_project_from(project_content);
        Self::from_content(&merged)
    }
}

fn normalized_non_empty_strings<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut result = Vec::new();
    for value in values {
        let trimmed = value.trim();
        if !trimmed.is_empty() && !result.iter().any(|v: &String| v == trimmed) {
            result.push(trimmed.to_string());
        }
    }
    result
}

// ---------------------------------------------------------------------------
// SettingsStore — GPUI Global
// ---------------------------------------------------------------------------

struct ProjectSettingsEntry {
    project_path: PathBuf,
    content: SettingsContent,
    last_modified: Option<SystemTime>,
}

pub struct SettingsStore {
    user_content: SettingsContent,
    project_settings: HashMap<Uuid, ProjectSettingsEntry>,
    global_resolved: Settings,
    project_resolved: HashMap<Uuid, Settings>,
    user_last_modified: Option<SystemTime>,
}

impl Global for SettingsStore {}

impl SettingsStore {
    /// Initialize settings store: load user settings + project settings, register as global.
    pub fn init(projects: &[Project], cx: &mut App) {
        let user_path = user_settings_path();

        // Write skeleton if file doesn't exist
        if !user_path.exists() {
            write_skeleton(&user_path);
        }

        // Load after potential skeleton write so mtime is consistent
        let (user_content, user_mtime) = load_content_with_mtime(&user_path);

        let mut project_settings = HashMap::new();
        let mut project_resolved = HashMap::new();

        for project in projects {
            let path = project_settings_path(&project.path);
            let (content, mtime) = load_content_with_mtime(&path);

            project_resolved.insert(
                project.id,
                Settings::from_user_and_project_content(&user_content, &content),
            );

            project_settings.insert(
                project.id,
                ProjectSettingsEntry {
                    project_path: project.path.clone(),
                    content,
                    last_modified: mtime,
                },
            );
        }

        let global_resolved = Settings::from_content(&user_content);

        let store = Self {
            user_content,
            project_settings,
            global_resolved,
            project_resolved,
            user_last_modified: user_mtime,
        };
        cx.set_global(store);
    }

    /// Construct a settings store backed only by `Settings::default()` —
    /// no filesystem access, no projects, no on-disk skeleton write.
    /// Use from unit tests that need a `SettingsStore` global without
    /// touching the user's real settings file.
    pub fn for_test() -> Self {
        Self {
            user_content: SettingsContent::default(),
            project_settings: HashMap::new(),
            global_resolved: Settings::default(),
            project_resolved: HashMap::new(),
            user_last_modified: None,
        }
    }

    /// Global settings (no project context).
    pub fn global(&self) -> &Settings {
        &self.global_resolved
    }

    /// Settings for a given project context. Falls back to global if project_id is None
    /// or not registered.
    pub fn get(&self, project_id: Option<Uuid>) -> &Settings {
        project_id
            .and_then(|id| self.project_resolved.get(&id))
            .unwrap_or(&self.global_resolved)
    }

    /// Register a new project and load its settings.
    pub fn register_project(&mut self, project: &Project) {
        let path = project_settings_path(&project.path);
        let (content, mtime) = load_content_with_mtime(&path);

        self.project_resolved.insert(
            project.id,
            Settings::from_user_and_project_content(&self.user_content, &content),
        );

        self.project_settings.insert(
            project.id,
            ProjectSettingsEntry {
                project_path: project.path.clone(),
                content,
                last_modified: mtime,
            },
        );
    }

    /// Unregister a project and clean up its settings.
    pub fn unregister_project(&mut self, project_id: Uuid) {
        self.project_settings.remove(&project_id);
        self.project_resolved.remove(&project_id);
    }

    /// Check all settings files for modifications and reload if changed.
    /// Call this from a polling loop. Returns true if anything changed.
    pub fn check_and_reload(cx: &mut App) -> bool {
        let store = cx.global::<SettingsStore>();

        // Check user settings mtime
        let user_path = user_settings_path();
        let current_user_mtime = file_mtime(&user_path);
        let user_changed = current_user_mtime != store.user_last_modified;

        // Check project settings mtimes
        let mut changed_projects: Vec<Uuid> = Vec::new();
        for (id, entry) in &store.project_settings {
            let path = project_settings_path(&entry.project_path);
            let current_mtime = file_mtime(&path);
            if current_mtime != entry.last_modified {
                changed_projects.push(*id);
            }
        }

        if !user_changed && changed_projects.is_empty() {
            return false;
        }

        // Need mutable access — reload
        cx.update_global::<SettingsStore, _>(|store, _cx| {
            if user_changed {
                let (content, mtime) = load_content_with_mtime(&user_path);
                store.user_content = content;
                store.user_last_modified = mtime;
                store.global_resolved = Settings::from_content(&store.user_content);
            }

            // Recompute all project resolved settings if user changed,
            // or just the changed projects otherwise.
            if user_changed {
                // User settings affect all project resolved caches
                let ids: Vec<Uuid> = store.project_settings.keys().copied().collect();
                for id in ids {
                    store.recompute_project(id);
                }
            } else {
                for id in changed_projects {
                    // Reload project content
                    if let Some(entry) = store.project_settings.get_mut(&id) {
                        let path = project_settings_path(&entry.project_path);
                        let (content, mtime) = load_content_with_mtime(&path);
                        entry.content = content;
                        entry.last_modified = mtime;
                    }
                    store.recompute_project(id);
                }
            }
        });

        true
    }

    /// Recompute resolved settings for a single project.
    fn recompute_project(&mut self, project_id: Uuid) {
        if let Some(entry) = self.project_settings.get(&project_id) {
            self.project_resolved.insert(
                project_id,
                Settings::from_user_and_project_content(&self.user_content, &entry.content),
            );
        }
    }

    pub fn update_content(
        &mut self,
        target: SettingsTarget,
        update: impl FnOnce(&mut SettingsContent),
    ) -> Result<()> {
        match target {
            SettingsTarget::User => {
                update(&mut self.user_content);
                self.user_content.prune_empty();
                self.user_last_modified = write_content(&user_settings_path(), &self.user_content)?;
                self.global_resolved = Settings::from_content(&self.user_content);

                let ids: Vec<Uuid> = self.project_settings.keys().copied().collect();
                for id in ids {
                    self.recompute_project(id);
                }
            }
            SettingsTarget::Project(project_id) => {
                let entry = self
                    .project_settings
                    .get_mut(&project_id)
                    .with_context(|| {
                        format!("settings for project {project_id} are not registered")
                    })?;
                update(&mut entry.content);
                entry.content.prune_empty();

                let path = project_settings_path(&entry.project_path);
                entry.last_modified = write_content(&path, &entry.content)?;
                self.recompute_project(project_id);
            }
        }
        Ok(())
    }

    pub fn content_for_target(&self, target: SettingsTarget) -> Option<&SettingsContent> {
        match target {
            SettingsTarget::User => Some(&self.user_content),
            SettingsTarget::Project(project_id) => self.project_content(project_id),
        }
    }

    pub fn resolved_for_target(&self, target: SettingsTarget) -> Option<&Settings> {
        match target {
            SettingsTarget::User => Some(&self.global_resolved),
            SettingsTarget::Project(project_id) => self.project_resolved.get(&project_id),
        }
    }

    pub fn project_settings(&self, project_id: Uuid) -> Option<&ProjectSettings> {
        self.project_resolved
            .get(&project_id)
            .map(|settings| &settings.project)
    }

    pub fn effective_default_branch(&self, project_id: Uuid, fallback: &str) -> String {
        self.project_settings(project_id)
            .and_then(|settings| settings.git.default_branch.as_deref())
            .filter(|branch| !branch.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| fallback.to_string())
    }

    /// Get the user settings content (for display in settings UI).
    pub fn user_content(&self) -> &SettingsContent {
        &self.user_content
    }

    /// Get project settings content (for display in settings UI).
    pub fn project_content(&self, project_id: Uuid) -> Option<&SettingsContent> {
        self.project_settings.get(&project_id).map(|e| &e.content)
    }
}

// ---------------------------------------------------------------------------
// File paths and I/O
// ---------------------------------------------------------------------------

fn user_settings_path() -> PathBuf {
    crate::seoul_dir()
        .unwrap_or_else(|_| PathBuf::from(".seoul"))
        .join("settings.json")
}

fn project_settings_path(project_root: &Path) -> PathBuf {
    project_root.join(".seoul").join("settings.json")
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

fn load_content_with_mtime(path: &Path) -> (SettingsContent, Option<SystemTime>) {
    let mtime = file_mtime(path);
    let content = if path.exists() {
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("Failed to parse {}: {e}", path.display());
                    SettingsContent::default()
                }
            },
            Err(e) => {
                tracing::warn!("Failed to read {}: {e}", path.display());
                SettingsContent::default()
            }
        }
    } else {
        SettingsContent::default()
    };
    (content, mtime)
}

fn write_content(path: &Path, content: &SettingsContent) -> Result<Option<SystemTime>> {
    let mut content = content.clone();
    content.prune_empty();
    let json = serde_json::to_string_pretty(&content).context("serialize settings content")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create settings directory {}", parent.display()))?;
    }
    std::fs::write(path, format!("{json}\n"))
        .with_context(|| format!("write settings file {}", path.display()))?;
    Ok(file_mtime(path))
}

fn write_skeleton(path: &Path) {
    let skeleton = Settings::default();
    let content = SettingsContent {
        terminal: Some(TerminalSettingsContent {
            font_family: Some(skeleton.terminal.font_family),
            font_size: Some(skeleton.terminal.font_size),
            scrollback_lines: Some(skeleton.terminal.scrollback_lines),
            padding: None,
        }),
        editor: Some(EditorSettingsContent {
            font_family: Some(skeleton.editor.font_family),
            font_size: Some(skeleton.editor.font_size),
            tab_size: Some(skeleton.editor.tab_size),
            show_line_numbers: None,
            word_wrap: None,
        }),
        theme: Some(ThemeSettingsContent {
            name: Some(skeleton.theme.name),
        }),
        app: None,
        project: Some(ProjectSettingsContent {
            git: Some(ProjectGitSettingsContent {
                default_branch: skeleton.project.git.default_branch,
            }),
            workspaces: Some(ProjectWorkspaceSettingsContent {
                worktree_base_dir: skeleton.project.workspaces.worktree_base_dir,
                branch_prefix_mode: Some(skeleton.project.workspaces.branch_prefix_mode),
                branch_prefix_custom: None,
            }),
            file_tree: Some(ProjectFileTreeSettingsContent {
                respect_gitignore: Some(skeleton.project.file_tree.respect_gitignore),
                extra_excludes: None,
            }),
            pr: Some(ProjectPrSettingsContent {
                enabled: Some(skeleton.project.pr.enabled),
            }),
        }),
    };

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(&content) {
        Ok(json) => {
            if let Err(e) = std::fs::write(path, json) {
                tracing::warn!("Failed to write skeleton settings: {e}");
            }
        }
        Err(e) => tracing::warn!("Failed to serialize skeleton settings: {e}"),
    }
}

/// Write the default skeleton settings file.
pub fn write_default_skeleton() {
    write_skeleton(&user_settings_path());
}

/// Public helper: path to user settings file.
pub fn user_settings_file_path() -> PathBuf {
    user_settings_path()
}

/// Public helper: path to project settings file.
pub fn project_settings_file_path(project_root: &Path) -> PathBuf {
    project_settings_path(project_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_content_overrides_only_project_profile_settings() {
        let user = SettingsContent {
            editor: Some(EditorSettingsContent {
                font_size: Some(15.0),
                ..Default::default()
            }),
            project: Some(ProjectSettingsContent {
                git: Some(ProjectGitSettingsContent {
                    default_branch: Some("main".into()),
                }),
                pr: Some(ProjectPrSettingsContent {
                    enabled: Some(true),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let project = SettingsContent {
            editor: Some(EditorSettingsContent {
                font_size: Some(99.0),
                ..Default::default()
            }),
            project: Some(ProjectSettingsContent {
                git: Some(ProjectGitSettingsContent {
                    default_branch: Some("develop".into()),
                }),
                pr: Some(ProjectPrSettingsContent {
                    enabled: Some(false),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };

        let resolved = Settings::from_user_and_project_content(&user, &project);

        assert_eq!(resolved.editor.font_size, 15.0);
        assert_eq!(
            resolved.project.git.default_branch.as_deref(),
            Some("develop")
        );
        assert!(!resolved.project.pr.enabled);
    }

    #[test]
    fn project_profile_defaults_are_conservative_and_workspace_friendly() {
        let resolved = Settings::default();

        assert_eq!(
            resolved.project.workspaces.branch_prefix_mode,
            BranchPrefixMode::Github
        );
        assert_eq!(
            resolved.project.file_tree.extra_excludes,
            Vec::<String>::new()
        );
        assert!(resolved.project.file_tree.respect_gitignore);
        assert!(resolved.project.pr.enabled);
    }
}
