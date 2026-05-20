use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use seoul_workspace::git::types::FileStatus;

use crate::icons::{Icon, IconName};
use crate::theme;

const DEFAULT_EXCLUDE_PATTERNS: &[&str] = &[
    ".git",
    "node_modules",
    "dist",
    "build",
    ".next",
    ".turbo",
    "coverage",
    ".cache",
    ".parcel-cache",
    ".vite",
    ".svelte-kit",
    ".vercel",
    "target",
    "out",
    "*.tsbuildinfo",
    ".DS_Store",
    ".seoul",
];

#[derive(Clone, Debug)]
struct IgnorePattern {
    pattern: String,
    negated: bool,
    directory_only: bool,
    anchored: bool,
    has_slash: bool,
}

pub struct FileTreeEntry {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
}

pub enum FileTreeEvent {
    FileSelected(PathBuf),
}

pub struct FileTreeView {
    entries: Vec<FileTreeEntry>,
    expanded_dirs: HashSet<PathBuf>,
    selected_path: Option<PathBuf>,
    root_path: Option<PathBuf>,
    focus_handle: FocusHandle,
    /// Git status per relative path (from repo root).
    git_status: HashMap<String, FileStatus>,
    respect_gitignore: bool,
    extra_excludes: Vec<String>,
    ignore_patterns: Vec<IgnorePattern>,
}

impl EventEmitter<FileTreeEvent> for FileTreeView {}

impl FileTreeView {
    pub fn new(
        cx: &mut Context<Self>,
        root_path: Option<PathBuf>,
        respect_gitignore: bool,
        extra_excludes: Vec<String>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let mut view = Self {
            entries: Vec::new(),
            expanded_dirs: HashSet::new(),
            selected_path: None,
            root_path: root_path.clone(),
            focus_handle,
            git_status: HashMap::new(),
            respect_gitignore,
            extra_excludes,
            ignore_patterns: Vec::new(),
        };
        if let Some(ref root) = root_path {
            view.expanded_dirs.insert(root.clone());
        }
        view.rebuild_ignore_patterns();
        view.rebuild_entries();
        view
    }

    pub fn set_root_path(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        self.root_path = path.clone();
        self.expanded_dirs.clear();
        self.selected_path = None;
        self.git_status.clear();
        if let Some(ref root) = path {
            self.expanded_dirs.insert(root.clone());
        }
        self.rebuild_ignore_patterns();
        self.rebuild_entries();
        cx.notify();
    }

    pub fn set_filter_settings(
        &mut self,
        respect_gitignore: bool,
        extra_excludes: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if self.respect_gitignore == respect_gitignore && self.extra_excludes == extra_excludes {
            return;
        }
        self.respect_gitignore = respect_gitignore;
        self.extra_excludes = extra_excludes;
        self.rebuild_ignore_patterns();
        self.rebuild_entries();
        cx.notify();
    }

    /// Update git status decorations for files. Keys are relative paths from repo root.
    pub fn set_git_status(&mut self, status: HashMap<String, FileStatus>, cx: &mut Context<Self>) {
        self.git_status = status;
        cx.notify();
    }

    fn rebuild_entries(&mut self) {
        self.entries.clear();
        if let Some(root) = self.root_path.clone() {
            self.collect_entries(&root, 0);
        }
    }

    fn rebuild_ignore_patterns(&mut self) {
        self.ignore_patterns.clear();
        for pattern in DEFAULT_EXCLUDE_PATTERNS {
            if let Some(pattern) = IgnorePattern::parse(pattern) {
                self.ignore_patterns.push(pattern);
            }
        }
        for pattern in &self.extra_excludes {
            if let Some(pattern) = IgnorePattern::parse(pattern) {
                self.ignore_patterns.push(pattern);
            }
        }
        if self.respect_gitignore
            && let Some(root) = &self.root_path
            && let Ok(text) = std::fs::read_to_string(root.join(".gitignore"))
        {
            for line in text.lines() {
                if let Some(pattern) = IgnorePattern::parse(line) {
                    self.ignore_patterns.push(pattern);
                }
            }
        }
    }

    fn collect_entries(&mut self, dir: &PathBuf, depth: usize) {
        let Ok(read_dir) = std::fs::read_dir(dir) else {
            return;
        };

        let mut dirs: Vec<(String, PathBuf)> = Vec::new();
        let mut files: Vec<(String, PathBuf)> = Vec::new();

        for entry in read_dir.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            let path = entry.path();
            if self.is_ignored(&path, &name, is_dir) {
                continue;
            }
            if is_dir {
                dirs.push((name, path));
            } else {
                files.push((name, path));
            }
        }

        dirs.sort_by_cached_key(|a| a.0.to_lowercase());
        files.sort_by_cached_key(|a| a.0.to_lowercase());

        for (name, path) in dirs {
            let is_expanded = self.expanded_dirs.contains(&path);
            self.entries.push(FileTreeEntry {
                path: path.clone(),
                name,
                depth,
                is_dir: true,
            });
            if is_expanded {
                self.collect_entries(&path, depth + 1);
            }
        }

        for (name, path) in files {
            self.entries.push(FileTreeEntry {
                path,
                name,
                depth,
                is_dir: false,
            });
        }
    }

    fn is_ignored(&self, path: &Path, name: &str, is_dir: bool) -> bool {
        let Some(root) = &self.root_path else {
            return false;
        };
        let rel = path.strip_prefix(root).unwrap_or(path);
        let mut ignored = false;
        for pattern in &self.ignore_patterns {
            if pattern.matches(rel, name, is_dir) {
                ignored = !pattern.negated;
            }
        }
        ignored
    }

    /// Get the git-status color for a file path, if it has git status.
    fn git_color_for_path(&self, abs_path: &Path, t: &theme::ThemeColors) -> Option<Rgba> {
        let root = self.root_path.as_ref()?;
        let rel_path = abs_path.strip_prefix(root).ok()?;
        let rel_str = rel_path.to_string_lossy();
        let status = self.git_status.get(rel_str.as_ref())?;
        Some(match status {
            FileStatus::Added | FileStatus::Untracked => rgb(t.green),
            FileStatus::Modified => rgb(t.yellow),
            FileStatus::Deleted => rgb(t.red),
            FileStatus::Renamed | FileStatus::Copied => rgb(t.blue),
        })
    }

    fn toggle_dir(&mut self, path: &PathBuf, cx: &mut Context<Self>) {
        if self.expanded_dirs.contains(path) {
            self.expanded_dirs.remove(path);
        } else {
            self.expanded_dirs.insert(path.clone());
        }
        self.rebuild_entries();
        cx.notify();
    }

    fn render_entry(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(entry) = self.entries.get(index) else {
            return div().into_any_element();
        };
        let t = theme::theme(cx);
        let path = entry.path.clone();
        let is_dir = entry.is_dir;
        let depth = entry.depth;
        let name = entry.name.clone();
        let is_selected = self.selected_path.as_ref() == Some(&entry.path);
        let is_expanded = is_dir && self.expanded_dirs.contains(&entry.path);
        // Compute git color once per entry (used for both icon and label).
        let git_color = if is_dir {
            None
        } else {
            self.git_color_for_path(&entry.path, &t)
        };

        div()
            .id(ElementId::Name(format!("ft-{index}").into()))
            .h(px(28.))
            .w_full()
            .pl(px(12. + depth as f32 * 16.))
            .pr(px(8.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.))
            .cursor_pointer()
            .when(is_selected, |el: Stateful<Div>| el.bg(rgb(t.surface0)))
            .hover(|s: StyleRefinement| s.bg(rgb(t.hover_bg_subtle)))
            .child(
                // Chevron / spacer
                div()
                    .w(px(12.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if is_dir {
                        let icon = if is_expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        };
                        Icon::new(icon, rgb(t.overlay0))
                            .size(px(12.))
                            .into_any_element()
                    } else {
                        div().w(px(12.)).into_any_element()
                    }),
            )
            .child(
                Icon::new(
                    if is_dir {
                        if is_expanded {
                            IconName::FolderOpen
                        } else {
                            IconName::Folder
                        }
                    } else {
                        IconName::File
                    },
                    if is_dir {
                        rgb(t.overlay2)
                    } else if let Some(c) = git_color {
                        c
                    } else {
                        rgb(t.overlay0)
                    },
                )
                .size(px(14.)),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(if is_dir {
                        rgb(t.text)
                    } else if let Some(c) = git_color {
                        c
                    } else {
                        rgb(t.subtext0)
                    })
                    .overflow_hidden()
                    .child(name),
            )
            .on_click(cx.listener(move |this, _, _window, cx| {
                if is_dir {
                    this.toggle_dir(&path, cx);
                } else {
                    this.selected_path = Some(path.clone());
                    cx.emit(FileTreeEvent::FileSelected(path.clone()));
                    cx.notify();
                }
            }))
            .into_any_element()
    }
}

impl IgnorePattern {
    fn parse(line: &str) -> Option<Self> {
        let mut pattern = line.trim();
        if pattern.is_empty() {
            return None;
        }
        if let Some(rest) = pattern.strip_prefix("\\#") {
            pattern = rest;
        } else if pattern.starts_with('#') {
            return None;
        }

        let negated = pattern.starts_with('!');
        if negated {
            pattern = pattern[1..].trim_start();
        }
        if pattern.is_empty() {
            return None;
        }

        let directory_only = pattern.ends_with('/');
        if directory_only {
            pattern = pattern.trim_end_matches('/');
        }

        let anchored = pattern.starts_with('/');
        if anchored {
            pattern = pattern.trim_start_matches('/');
        }

        let pattern = pattern.trim();
        if pattern.is_empty() {
            return None;
        }

        Some(Self {
            pattern: pattern.to_string(),
            negated,
            directory_only,
            anchored,
            has_slash: pattern.contains('/'),
        })
    }

    fn matches(&self, rel: &Path, name: &str, is_dir: bool) -> bool {
        if self.directory_only && !is_dir {
            return false;
        }

        let rel_text = path_to_slash_string(rel);
        if self.anchored || self.has_slash {
            if self.contains_wildcard() {
                wildcard_match(&self.pattern, &rel_text)
            } else if self.directory_only {
                rel_text == self.pattern
                    || rel_text
                        .strip_prefix(&self.pattern)
                        .is_some_and(|rest| rest.starts_with('/'))
            } else {
                rel_text == self.pattern
            }
        } else if self.contains_wildcard() {
            wildcard_match(&self.pattern, name)
        } else {
            rel.components().any(|component| match component {
                Component::Normal(value) => value.to_string_lossy() == self.pattern.as_str(),
                _ => false,
            })
        }
    }

    fn contains_wildcard(&self) -> bool {
        self.pattern.contains('*') || self.pattern.contains('?')
    }
}

fn path_to_slash_string(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    let mut p = 0;
    let mut t = 0;
    let mut star = None;
    let mut star_text = 0;

    while t < text.len() {
        if p < pattern.len() && (pattern[p] == text[t] || pattern[p] == b'?') {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            star_text = t;
        } else if let Some(star_index) = star {
            p = star_index + 1;
            star_text += 1;
            t = star_text;
        } else {
            return false;
        }
    }

    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }

    p == pattern.len()
}

impl Focusable for FileTreeView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for FileTreeView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::theme(cx);
        let entry_count = self.entries.len();
        let mut container = div()
            .id("file-tree")
            .key_context("file-tree")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(t.mantle))
            // Header
            .child(
                div().flex_none().px(px(12.)).py(px(10.)).child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgb(t.overlay0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("FILES"),
                ),
            );

        if entry_count == 0 {
            // Empty state
            container = container.child(
                div()
                    .px(px(12.))
                    .py(px(20.))
                    .text_size(px(12.))
                    .text_color(rgb(t.surface2))
                    .child(if self.root_path.is_some() {
                        "Empty directory."
                    } else {
                        "Select a workspace to browse files."
                    }),
            );
        } else {
            // Virtualized list — only the visible window is painted.
            container = container.child(
                uniform_list(
                    "file-tree-entries",
                    entry_count,
                    cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                        range.map(|i| this.render_entry(i, cx)).collect::<Vec<_>>()
                    }),
                )
                .flex_grow()
                .into_any_element(),
            );
        }

        container
    }
}
