use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::git::types::ChangeCategory;
use crate::project::Project;
use crate::workspace::{Workspace, WorkspaceKind};

/// Info about a closed terminal tab for undo-close recovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClosedTabInfo {
    pub tab_id: Uuid,
    pub workspace_id: Uuid,
    pub title: String,
}

/// Window position and size for persistence (Zed: SerializedWindowBounds).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowState {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedWorkspaceTabs {
    pub tabs: Vec<PersistedTab>,
    pub active_tab_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedTab {
    pub id: Uuid,
    pub kind: PersistedTabKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PersistedTabKind {
    Terminal {
        session_id: Uuid,
    },
    Editor {
        path: PathBuf,
    },
    Settings,
    Diff {
        path: String,
        category: ChangeCategory,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppState {
    pub version: u32,
    pub projects: Vec<Project>,
    pub workspaces: Vec<Workspace>,
    pub active_workspace_id: Option<Uuid>,
    pub sidebar_width: f32,
    pub sidebar_collapsed: bool,
    /// Workspace ID → canonical persisted tab order and active tab.
    #[serde(default)]
    pub workspace_tabs: HashMap<Uuid, PersistedWorkspaceTabs>,
    /// Tab ID → daemon session ID mapping for session persistence.
    #[serde(default, skip_serializing)]
    pub tab_sessions: HashMap<Uuid, Uuid>,
    /// Window position/size for restore.
    #[serde(default)]
    pub window_state: Option<WindowState>,
    /// Workspace ID → ordered list of tab IDs.
    #[serde(default, skip_serializing)]
    pub workspace_tab_order: HashMap<Uuid, Vec<Uuid>>,
    /// Right sidebar (file tree) collapsed state.
    #[serde(default)]
    pub right_sidebar_collapsed: bool,
    /// Right sidebar width in pixels.
    #[serde(default = "default_right_sidebar_width")]
    pub right_sidebar_width: f32,
    /// Recently closed terminal tabs for undo-close recovery.
    #[serde(default)]
    pub closed_tabs: Vec<ClosedTabInfo>,
    /// Workspace ID → active tab ID for restoring focused tab.
    #[serde(default, skip_serializing)]
    pub workspace_active_tab: HashMap<Uuid, Uuid>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            version: 2,
            projects: Vec::new(),
            workspaces: Vec::new(),
            active_workspace_id: None,
            sidebar_width: 220.0,
            sidebar_collapsed: false,
            workspace_tabs: HashMap::new(),
            tab_sessions: HashMap::new(),
            window_state: None,
            workspace_tab_order: HashMap::new(),
            right_sidebar_collapsed: false,
            right_sidebar_width: default_right_sidebar_width(),
            closed_tabs: Vec::new(),
            workspace_active_tab: HashMap::new(),
        }
    }
}

impl AppState {
    pub fn project_by_id(&self, id: Uuid) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    pub fn workspaces_for_project(&self, project_id: Uuid) -> Vec<&Workspace> {
        self.workspaces
            .iter()
            .filter(|ws| ws.project_id == project_id)
            .collect()
    }

    pub fn active_workspace(&self) -> Option<&Workspace> {
        self.active_workspace_id
            .and_then(|id| self.workspaces.iter().find(|ws| ws.id == id))
    }

    /// Resolve the on-disk working directory for `ws` — main repo path for
    /// `MainBranch`, the worktree directory for `Worktree`. Returns `None`
    /// if the workspace's project is missing from state.
    pub fn workspace_working_dir(&self, ws: &Workspace) -> Option<PathBuf> {
        let project = self.project_by_id(ws.project_id)?;
        Some(ws.working_dir(project).to_path_buf())
    }

    pub fn migrate_legacy_tabs(&mut self) -> bool {
        let mut changed = false;
        for (&ws_id, tab_ids) in &self.workspace_tab_order {
            if self.workspace_tabs.contains_key(&ws_id) {
                continue;
            }

            let tabs: Vec<PersistedTab> = tab_ids
                .iter()
                .filter_map(|tab_id| {
                    let session_id = *self.tab_sessions.get(tab_id)?;
                    Some(PersistedTab {
                        id: *tab_id,
                        kind: PersistedTabKind::Terminal { session_id },
                    })
                })
                .collect();

            if tabs.is_empty() {
                continue;
            }

            let active_tab_id = self
                .workspace_active_tab
                .get(&ws_id)
                .copied()
                .filter(|active_id| tabs.iter().any(|tab| tab.id == *active_id));

            self.workspace_tabs.insert(
                ws_id,
                PersistedWorkspaceTabs {
                    tabs,
                    active_tab_id,
                },
            );
            changed = true;
        }
        changed
    }
}

fn default_right_sidebar_width() -> f32 {
    250.0
}

fn state_file_path() -> Result<PathBuf> {
    Ok(crate::seoul_dir()?.join("state.json"))
}

pub fn load_state() -> Result<AppState> {
    let path = state_file_path()?;
    if !path.exists() {
        return Ok(AppState::default());
    }
    let contents = std::fs::read_to_string(&path).context("Failed to read state file")?;
    let mut state: AppState =
        serde_json::from_str(&contents).context("Failed to parse state file")?;
    let migrated_tabs = state.migrate_legacy_tabs();
    if ensure_main_workspaces(&mut state) || migrated_tabs {
        save_state(&state)?;
    }
    Ok(state)
}

/// Ensure every project has exactly one `MainBranch` workspace. Idempotent.
/// Returns `true` if any workspace was added (caller should persist).
///
/// Called from `load_state` to backfill existing installs that predate the
/// MainBranch concept. Also a safety net against manual `state.json` edits.
fn ensure_main_workspaces(state: &mut AppState) -> bool {
    let missing: Vec<Project> = state
        .projects
        .iter()
        .filter(|p| {
            !state
                .workspaces
                .iter()
                .any(|w| w.project_id == p.id && w.kind == WorkspaceKind::MainBranch)
        })
        .cloned()
        .collect();

    let mut changed = false;
    for project in missing {
        state.workspaces.push(Workspace::main_branch(&project));
        changed = true;
    }
    changed
}

/// Push `ws` into `state.workspaces`, refusing a second `MainBranch` for the
/// same project. Use this from any new code path that adds workspaces.
pub fn add_workspace(state: &mut AppState, ws: Workspace) -> Result<()> {
    if ws.kind == WorkspaceKind::MainBranch
        && state
            .workspaces
            .iter()
            .any(|w| w.project_id == ws.project_id && w.kind == WorkspaceKind::MainBranch)
    {
        anyhow::bail!(
            "MainBranch workspace already exists for project {}",
            ws.project_id
        );
    }
    state.workspaces.push(ws);
    Ok(())
}

pub fn save_state(state: &AppState) -> Result<()> {
    let path = state_file_path()?;
    save_state_to(state, &path)
}

/// Atomically write `state` as JSON to `path` via tmp+rename.
///
/// The tmp file's name is unique per (pid, uuid) so concurrent saves —
/// across processes (e.g., daemon + app) or across threads in the same
/// process — never collide on the same tmp slot. Without this, two
/// concurrent writers could both write to `state.json.tmp` and one would
/// observe a partially-written file or have its rename race with another
/// rename, leaving `state.json` corrupted or pointing at stale bytes.
fn save_state_to(state: &AppState, path: &std::path::Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create state directory")?;
    }
    let contents = serde_json::to_string(state).context("Failed to serialize state")?;
    let tmp_name = format!(
        "json.tmp.{}.{}",
        std::process::id(),
        Uuid::new_v4().simple()
    );
    let tmp = path.with_extension(tmp_name);
    std::fs::write(&tmp, &contents).context("Failed to write temp state file")?;
    std::fs::rename(&tmp, path).context("Failed to rename state file")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::types::ChangeCategory;

    fn fake_project(name: &str) -> Project {
        Project {
            id: Uuid::new_v4(),
            name: name.into(),
            path: PathBuf::from(format!("/tmp/seoul-test-{name}")),
            default_branch: "main".into(),
        }
    }

    #[test]
    fn ensure_main_workspaces_backfills_missing_and_is_idempotent() {
        let mut state = AppState::default();
        let p1 = fake_project("alpha");
        let p2 = fake_project("beta");
        state.projects.push(p1.clone());
        state.projects.push(p2.clone());
        // Pre-existing worktree for p1 — should NOT count as MainBranch.
        state.workspaces.push(Workspace {
            id: Uuid::new_v4(),
            project_id: p1.id,
            name: "feat".into(),
            branch: "feat/x".into(),
            worktree_path: Some(PathBuf::from("/tmp/seoul-test-alpha/wt")),
            kind: WorkspaceKind::Worktree,
        });

        assert!(ensure_main_workspaces(&mut state));
        let mains: Vec<&Workspace> = state
            .workspaces
            .iter()
            .filter(|w| w.kind == WorkspaceKind::MainBranch)
            .collect();
        assert_eq!(mains.len(), 2);
        assert!(mains.iter().any(|w| w.project_id == p1.id));
        assert!(mains.iter().any(|w| w.project_id == p2.id));

        // Second call must be a no-op.
        assert!(!ensure_main_workspaces(&mut state));
        assert_eq!(
            state
                .workspaces
                .iter()
                .filter(|w| w.kind == WorkspaceKind::MainBranch)
                .count(),
            2
        );
    }

    #[test]
    fn add_workspace_rejects_duplicate_main_branch() {
        let mut state = AppState::default();
        let p = fake_project("solo");
        state.projects.push(p.clone());
        state.workspaces.push(Workspace::main_branch(&p));

        let dup = Workspace::main_branch(&p);
        let err = add_workspace(&mut state, dup).unwrap_err();
        assert!(
            err.to_string()
                .contains("MainBranch workspace already exists"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn add_workspace_allows_multiple_worktrees_per_project() {
        let mut state = AppState::default();
        let p = fake_project("multi");
        state.projects.push(p.clone());

        let wt1 = Workspace {
            id: Uuid::new_v4(),
            project_id: p.id,
            name: "wt1".into(),
            branch: "feat/a".into(),
            worktree_path: Some(PathBuf::from("/tmp/seoul-test-multi/a")),
            kind: WorkspaceKind::Worktree,
        };
        let wt2 = Workspace {
            id: Uuid::new_v4(),
            ..wt1.clone()
        };
        assert!(add_workspace(&mut state, wt1).is_ok());
        assert!(add_workspace(&mut state, wt2).is_ok());
        assert_eq!(state.workspaces.len(), 2);
    }

    #[test]
    fn migrate_legacy_terminal_tabs_preserves_multiple_tabs_and_active_tab() {
        let workspace_id = Uuid::new_v4();
        let tab_1 = Uuid::new_v4();
        let tab_2 = Uuid::new_v4();
        let session_1 = Uuid::new_v4();
        let session_2 = Uuid::new_v4();

        let json = serde_json::json!({
            "version": 2,
            "projects": [],
            "workspaces": [],
            "active_workspace_id": null,
            "sidebar_width": 220.0,
            "sidebar_collapsed": false,
            "tab_sessions": {
                tab_1.to_string(): session_1,
                tab_2.to_string(): session_2,
            },
            "workspace_tab_order": {
                workspace_id.to_string(): [tab_1, tab_2],
            },
            "workspace_active_tab": {
                workspace_id.to_string(): tab_2,
            }
        });

        let mut state: AppState = serde_json::from_value(json).unwrap();
        state.migrate_legacy_tabs();

        let workspace_tabs = state.workspace_tabs.get(&workspace_id).unwrap();
        assert_eq!(workspace_tabs.active_tab_id, Some(tab_2));
        assert_eq!(
            workspace_tabs.tabs,
            vec![
                PersistedTab {
                    id: tab_1,
                    kind: PersistedTabKind::Terminal {
                        session_id: session_1,
                    },
                },
                PersistedTab {
                    id: tab_2,
                    kind: PersistedTabKind::Terminal {
                        session_id: session_2,
                    },
                },
            ]
        );
    }

    #[test]
    fn app_state_roundtrips_all_persisted_tab_kinds() {
        let workspace_id = Uuid::new_v4();
        let terminal_tab = Uuid::new_v4();
        let editor_tab = Uuid::new_v4();
        let settings_tab = Uuid::new_v4();
        let diff_tab = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        let mut state = AppState::default();
        state.workspace_tabs.insert(
            workspace_id,
            PersistedWorkspaceTabs {
                tabs: vec![
                    PersistedTab {
                        id: terminal_tab,
                        kind: PersistedTabKind::Terminal { session_id },
                    },
                    PersistedTab {
                        id: editor_tab,
                        kind: PersistedTabKind::Editor {
                            path: PathBuf::from("/tmp/example.rs"),
                        },
                    },
                    PersistedTab {
                        id: settings_tab,
                        kind: PersistedTabKind::Settings,
                    },
                    PersistedTab {
                        id: diff_tab,
                        kind: PersistedTabKind::Diff {
                            path: "src/main.rs".into(),
                            category: ChangeCategory::Staged,
                        },
                    },
                ],
                active_tab_id: Some(diff_tab),
            },
        );

        let json = serde_json::to_string(&state).unwrap();
        let roundtrip: AppState = serde_json::from_str(&json).unwrap();

        assert_eq!(roundtrip.workspace_tabs, state.workspace_tabs);
    }

    #[test]
    fn serializing_app_state_omits_legacy_terminal_layout_fields() {
        let mut state = AppState::default();
        state.tab_sessions.insert(Uuid::new_v4(), Uuid::new_v4());
        state
            .workspace_tab_order
            .insert(Uuid::new_v4(), vec![Uuid::new_v4()]);
        state
            .workspace_active_tab
            .insert(Uuid::new_v4(), Uuid::new_v4());

        let value = serde_json::to_value(&state).unwrap();
        assert!(value.get("workspace_tabs").is_some());
        assert!(value.get("tab_sessions").is_none());
        assert!(value.get("workspace_tab_order").is_none());
        assert!(value.get("workspace_active_tab").is_none());
    }

    /// Two threads racing on `save_state_to` for the same final path must
    /// produce a final file that is valid JSON and equals one of the inputs.
    /// Before the fix both threads would write to the same `state.json.tmp`
    /// slot and one rename could observe a half-written file.
    #[test]
    fn save_state_concurrent_does_not_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        // Build two states with a distinguishable field so we can confirm
        // the final file is one of them, not a corrupted hybrid.
        let mut s1 = AppState::default();
        let p1 = fake_project("concurrent-a");
        s1.projects.push(p1);
        let mut s2 = AppState::default();
        let p2 = fake_project("concurrent-b");
        s2.projects.push(p2);

        let p_a = path.clone();
        let p_b = path.clone();
        let s1_clone = s1.clone();
        let s2_clone = s2.clone();
        let h1 = std::thread::spawn(move || save_state_to(&s1_clone, &p_a).unwrap());
        let h2 = std::thread::spawn(move || save_state_to(&s2_clone, &p_b).unwrap());
        h1.join().unwrap();
        h2.join().unwrap();

        // Final file must exist and parse cleanly as JSON.
        let txt = std::fs::read_to_string(&path).expect("final state.json must exist");
        let parsed: AppState =
            serde_json::from_str(&txt).expect("file must remain valid JSON after concurrent save");

        // And it should match exactly one of the two inputs (no torn bytes).
        let names: Vec<&str> = parsed.projects.iter().map(|p| p.name.as_str()).collect();
        assert!(
            names == vec!["concurrent-a"] || names == vec!["concurrent-b"],
            "expected one of the saved states, got projects = {names:?}"
        );

        // No leftover tmp files should remain after both renames succeed.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "tmp files left behind: {leftovers:?}");
    }
}
