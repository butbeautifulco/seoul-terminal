use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use seoul_workspace::git::types::FileStatus;

use crate::icons::{Icon, IconName};
use crate::theme;

const IGNORED_NAMES: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".DS_Store",
    ".seoul",
    ".cache",
];

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
}

impl EventEmitter<FileTreeEvent> for FileTreeView {}

impl FileTreeView {
    pub fn new(cx: &mut Context<Self>, root_path: Option<PathBuf>) -> Self {
        let focus_handle = cx.focus_handle();
        let mut view = Self {
            entries: Vec::new(),
            expanded_dirs: HashSet::new(),
            selected_path: None,
            root_path: root_path.clone(),
            focus_handle,
            git_status: HashMap::new(),
        };
        if let Some(ref root) = root_path {
            view.expanded_dirs.insert(root.clone());
        }
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

    fn collect_entries(&mut self, dir: &PathBuf, depth: usize) {
        let Ok(read_dir) = std::fs::read_dir(dir) else {
            return;
        };

        let mut dirs: Vec<(String, PathBuf)> = Vec::new();
        let mut files: Vec<(String, PathBuf)> = Vec::new();

        for entry in read_dir.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if IGNORED_NAMES.contains(&name.as_str()) {
                continue;
            }
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            let path = entry.path();
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
