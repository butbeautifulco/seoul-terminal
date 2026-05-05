use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use gpui::{App, BorrowAppContext, Global};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::project::Project;

// ---------------------------------------------------------------------------
// SettingsContent — override layer (all fields Option for merge semantics)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SettingsContent {
    #[serde(default)]
    pub terminal: Option<TerminalSettingsContent>,
    #[serde(default)]
    pub editor: Option<EditorSettingsContent>,
    #[serde(default)]
    pub theme: Option<ThemeSettingsContent>,
    #[serde(default)]
    pub app: Option<AppSettingsContent>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TerminalSettingsContent {
    pub font_family: Option<String>,
    pub font_size: Option<f32>,
    pub scrollback_lines: Option<usize>,
    pub padding: Option<f32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EditorSettingsContent {
    pub font_family: Option<String>,
    pub font_size: Option<f32>,
    pub tab_size: Option<usize>,
    pub show_line_numbers: Option<bool>,
    pub word_wrap: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThemeSettingsContent {
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppSettingsContent {
    pub sidebar_width: Option<f32>,
    pub window_width: Option<f32>,
    pub window_height: Option<f32>,
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
        s
    }
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

            // Compute merged content: user + project
            let mut merged = user_content.clone();
            merged.merge_from(&content);
            project_resolved.insert(project.id, Settings::from_content(&merged));

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

        let mut merged = self.user_content.clone();
        merged.merge_from(&content);
        self.project_resolved
            .insert(project.id, Settings::from_content(&merged));

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
            let mut merged = self.user_content.clone();
            merged.merge_from(&entry.content);
            self.project_resolved
                .insert(project_id, Settings::from_content(&merged));
        }
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
