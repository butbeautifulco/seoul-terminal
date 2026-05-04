use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::git;
use crate::git::GitCommandRunner;
use crate::project::Project;
use crate::seoul_dong::SEOUL_DONG;
use crate::worktree;

/// Discriminates "main repo, no worktree" workspaces from feature-branch worktrees.
///
/// `Worktree` is the default so legacy `state.json` entries (which had no `kind`
/// field) deserialize into the existing behavior unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKind {
    #[default]
    Worktree,
    MainBranch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub branch: String,
    /// `None` iff `kind == MainBranch` — that case uses the project's main repo path.
    pub worktree_path: Option<PathBuf>,
    #[serde(default)]
    pub kind: WorkspaceKind,
}

impl Workspace {
    /// Create a new workspace by creating a git worktree.
    /// The worktree is placed at `~/.seoul/worktrees/{project_name}/{workspace_name}`.
    /// `branch_name` can differ from `name` if the user provides a custom branch.
    pub fn create(project: &Project, name: &str, branch_name: &str) -> Result<Self> {
        let base_dir = worktree_base_dir()?;
        let target_path = base_dir.join(&project.name).join(name);

        worktree::create_worktree(
            &project.path,
            branch_name,
            &project.default_branch,
            &target_path,
        )?;

        Ok(Self {
            id: Uuid::new_v4(),
            project_id: project.id,
            name: name.to_string(),
            branch: branch_name.to_string(),
            worktree_path: Some(target_path),
            kind: WorkspaceKind::Worktree,
        })
    }

    /// Construct the project's MainBranch workspace — the singleton tied to the
    /// main repo path itself (no separate worktree). The `branch` field is a
    /// snapshot of the current HEAD; callers refresh it on activation/focus.
    /// Falls back to `project.default_branch` if HEAD can't be read or is detached.
    pub fn main_branch(project: &Project) -> Self {
        let runner = GitCommandRunner::new(&project.path);
        let branch = git::branch::current_branch(&runner)
            .ok()
            .flatten()
            .unwrap_or_else(|| project.default_branch.clone());

        Self {
            id: Uuid::new_v4(),
            project_id: project.id,
            name: "default".into(),
            branch,
            worktree_path: None,
            kind: WorkspaceKind::MainBranch,
        }
    }

    /// Path the workspace operates in — main repo path for `MainBranch`,
    /// the worktree directory for `Worktree`. Single source of truth for
    /// daemon `cwd`, file-tree root, and git provider initialization.
    pub fn working_dir<'a>(&'a self, project: &'a Project) -> &'a Path {
        match self.kind {
            WorkspaceKind::MainBranch => &project.path,
            WorkspaceKind::Worktree => self
                .worktree_path
                .as_deref()
                .unwrap_or(project.path.as_path()),
        }
    }

    /// Remove the git worktree associated with this workspace.
    pub fn remove(&self, project: &Project) -> Result<()> {
        if self.kind == WorkspaceKind::MainBranch {
            bail!("cannot remove main-branch workspace");
        }
        let Some(path) = self.worktree_path.as_deref() else {
            bail!("worktree workspace has no worktree_path");
        };
        worktree::remove_worktree(&project.path, path)
    }

    /// Remove the git worktree and also delete the branch.
    pub fn remove_with_branch(&self, project: &Project) -> Result<()> {
        if self.kind == WorkspaceKind::MainBranch {
            bail!("cannot remove main-branch workspace");
        }
        let Some(path) = self.worktree_path.as_deref() else {
            bail!("worktree workspace has no worktree_path");
        };
        worktree::remove_worktree(&project.path, path)?;
        delete_branch(&project.path, &self.branch)?;
        Ok(())
    }
}

/// Delete a local git branch (non-force; won't delete if unmerged).
fn delete_branch(repo_path: &Path, branch: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["branch", "-D", branch])
        .current_dir(repo_path)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!("Failed to delete branch {branch}: {}", stderr.trim());
    }
    Ok(())
}

fn worktree_base_dir() -> Result<PathBuf> {
    Ok(crate::seoul_dir()?.join("worktrees"))
}

// ---------------------------------------------------------------------------
// Workspace name generation — {author_prefix}/{seoul_dong}
//
// Author prefix resolution: `gh api user` → `git config user.name` → None.
// Random dong selection draws from `seoul_dong::SEOUL_DONG`.
// Conflict resolution: up to MAX_RANDOM_ATTEMPTS fresh draws, then numeric
// `-2..=99` suffix on the last draw, then a uuid absolute fallback.
// ---------------------------------------------------------------------------

const MAX_RANDOM_ATTEMPTS: usize = 10;
const MAX_AUTHOR_PREFIX_LEN: usize = 50;

/// Generate a unique workspace/branch name.
///
/// Output: `{author_prefix}/{dong}` when an author prefix is available,
/// otherwise `{dong}` alone. On conflict, the same dong gets a `-2`, `-3`, …
/// suffix. Comparison against `existing_names` is case-insensitive.
pub fn generate_workspace_name(repo_path: &Path, existing_names: &[String]) -> String {
    let prefix = resolve_author_prefix(repo_path);
    generate_with_prefix(prefix.as_deref(), existing_names)
}

/// Core generator — exposed for tests so the prefix can be controlled
/// independently of the host's git/gh configuration.
fn generate_with_prefix(prefix: Option<&str>, existing_names: &[String]) -> String {
    let mut rng = rand::thread_rng();

    let mut last_dong: &str = SEOUL_DONG.choose(&mut rng).copied().unwrap_or("workspace");
    for _ in 0..MAX_RANDOM_ATTEMPTS {
        last_dong = SEOUL_DONG.choose(&mut rng).copied().unwrap_or(last_dong);
        let candidate = compose_name(prefix, last_dong, None);
        if !collides(existing_names, &candidate) {
            return candidate;
        }
    }

    for suffix in 2..=99 {
        let candidate = compose_name(prefix, last_dong, Some(suffix));
        if !collides(existing_names, &candidate) {
            return candidate;
        }
    }

    let fallback = format!("ws-{}", Uuid::new_v4().as_fields().0);
    compose_name(prefix, &fallback, None)
}

fn compose_name(prefix: Option<&str>, dong: &str, suffix: Option<u32>) -> String {
    let tail = match suffix {
        Some(n) => format!("{dong}-{n}"),
        None => dong.to_string(),
    };
    match prefix {
        Some(p) if !p.is_empty() => format!("{p}/{tail}"),
        _ => tail,
    }
}

fn collides(existing: &[String], candidate: &str) -> bool {
    existing.iter().any(|n| n.eq_ignore_ascii_case(candidate))
}

/// Resolve the author prefix:
/// 1. `gh api user --jq .login` (GitHub CLI)
/// 2. `git config user.name` (first whitespace-separated token)
/// 3. None
fn resolve_author_prefix(repo_path: &Path) -> Option<String> {
    if let Some(login) = github_login(repo_path) {
        let slug = sanitize_author_prefix(&login);
        if !slug.is_empty() {
            return Some(slug);
        }
    }
    if let Some(name) = git_config_user_name(repo_path) {
        let slug = sanitize_author_prefix(&name);
        if !slug.is_empty() {
            return Some(slug);
        }
    }
    None
}

/// Timeout for `gh api user`. `gh` hits GitHub over the network, so a slow or
/// unreachable host would otherwise stall the UI thread indefinitely.
const GH_TIMEOUT: Duration = Duration::from_secs(2);
const GH_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Run `gh api user --jq .login` with a short timeout. Returns `None` if `gh`
/// is not installed, the user isn't authenticated, the call fails, or it
/// doesn't complete within `GH_TIMEOUT`.
fn github_login(repo_path: &Path) -> Option<String> {
    use std::process::Stdio;

    let mut child = Command::new("gh")
        .args(["api", "user", "--jq", ".login"])
        .current_dir(repo_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let deadline = Instant::now() + GH_TIMEOUT;
    loop {
        match child.try_wait().ok()? {
            Some(status) => {
                if !status.success() {
                    return None;
                }
                let output = child.wait_with_output().ok()?;
                let text = String::from_utf8(output.stdout).ok()?;
                let trimmed = text.trim().to_string();
                return (!trimmed.is_empty()).then_some(trimmed);
            }
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                thread::sleep(GH_POLL_INTERVAL);
            }
        }
    }
}

fn git_config_user_name(repo_path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["config", "user.name"])
        .current_dir(repo_path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8(output.stdout).ok()?;
    let first = raw.split_whitespace().next()?.to_string();
    (!first.is_empty()).then_some(first)
}

/// Author-prefix sanitizer (preserves case):
/// - collapse whitespace runs to `-`
/// - keep `[A-Za-z0-9.+@-]`, drop anything else
/// - collapse consecutive `-`
/// - trim leading/trailing `-` or `.`
/// - cap at 50 chars
fn sanitize_author_prefix(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_dash = false;

    for ch in raw.chars() {
        if ch.is_whitespace() {
            if !prev_dash && !out.is_empty() {
                out.push('-');
                prev_dash = true;
            }
            continue;
        }

        let keep = ch.is_ascii_alphanumeric() || ch == '.' || ch == '+' || ch == '@' || ch == '-';
        if !keep {
            continue;
        }

        if ch == '-' {
            if prev_dash || out.is_empty() {
                continue;
            }
            out.push('-');
            prev_dash = true;
        } else {
            out.push(ch);
            prev_dash = false;
        }
    }

    // Trim leading/trailing `-` and `.` in one pass.
    let mut out: String = out.trim_matches(|c: char| c == '-' || c == '.').to_string();

    if out.chars().count() > MAX_AUTHOR_PREFIX_LEN {
        out = out.chars().take(MAX_AUTHOR_PREFIX_LEN).collect();
        // Only the trailing side can dangle after the cap.
        out.truncate(out.trim_end_matches(['-', '.']).len());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_preserves_case_and_replaces_spaces() {
        assert_eq!(sanitize_author_prefix("John Doe"), "John-Doe");
    }

    #[test]
    fn sanitize_strips_non_allowed_chars() {
        // Parentheses, whitespace inside them, and non-ASCII letters all dropped.
        assert_eq!(sanitize_author_prefix("Jane Doe (αβγ)"), "Jane-Doe");
    }

    #[test]
    fn sanitize_collapses_dashes_and_trims() {
        assert_eq!(sanitize_author_prefix("  --foo--bar--  "), "foo-bar");
        assert_eq!(sanitize_author_prefix("...john..."), "john");
    }

    #[test]
    fn sanitize_enforces_50_char_cap() {
        let raw = "a".repeat(80);
        let out = sanitize_author_prefix(&raw);
        assert_eq!(out.len(), MAX_AUTHOR_PREFIX_LEN);
    }

    #[test]
    fn compose_with_prefix_uses_slash() {
        assert_eq!(
            compose_name(Some("seongmin"), "hongdae", None),
            "seongmin/hongdae"
        );
        assert_eq!(
            compose_name(Some("seongmin"), "hongdae", Some(2)),
            "seongmin/hongdae-2"
        );
    }

    #[test]
    fn compose_without_prefix_is_bare_dong() {
        assert_eq!(compose_name(None, "hongdae", None), "hongdae");
        assert_eq!(compose_name(Some(""), "hongdae", None), "hongdae");
        assert_eq!(compose_name(None, "hongdae", Some(3)), "hongdae-3");
    }

    #[test]
    fn collides_is_case_insensitive() {
        let existing = vec!["Seongmin/Hongdae".to_string()];
        assert!(collides(&existing, "seongmin/hongdae"));
        assert!(collides(&existing, "SEONGMIN/HONGDAE"));
        assert!(!collides(&existing, "seongmin/ikseon"));
    }

    #[test]
    fn generate_produces_variety_without_prefix() {
        let existing: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..50 {
            seen.insert(generate_with_prefix(None, &existing));
        }
        assert!(
            seen.len() >= 30,
            "expected ≥30 distinct names in 50 draws, got {}",
            seen.len()
        );
    }

    #[test]
    fn generate_produces_variety_with_prefix() {
        let existing: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..50 {
            seen.insert(generate_with_prefix(Some("tester"), &existing));
        }
        assert!(
            seen.len() >= 30,
            "expected ≥30 distinct names in 50 draws, got {}",
            seen.len()
        );
        for name in &seen {
            assert!(name.starts_with("tester/"), "missing prefix: {name}");
        }
    }

    #[test]
    fn generate_avoids_existing_exact_matches_without_prefix() {
        let banned: Vec<String> = SEOUL_DONG.iter().take(50).map(|s| s.to_string()).collect();
        for _ in 0..10 {
            let name = generate_with_prefix(None, &banned);
            assert!(
                !banned.iter().any(|b| b.eq_ignore_ascii_case(&name)),
                "got banned name: {name}"
            );
        }
    }

    #[test]
    fn generate_avoids_existing_exact_matches_with_prefix() {
        let banned: Vec<String> = SEOUL_DONG
            .iter()
            .take(50)
            .map(|d| format!("tester/{d}"))
            .collect();
        for _ in 0..10 {
            let name = generate_with_prefix(Some("tester"), &banned);
            assert!(
                !banned.iter().any(|b| b.eq_ignore_ascii_case(&name)),
                "got banned name: {name}"
            );
        }
    }

    #[test]
    fn generate_falls_back_to_numeric_suffix_when_pool_exhausted() {
        // Ban every possible bare dong so the generator must fall through.
        let banned: Vec<String> = SEOUL_DONG.iter().map(|s| s.to_string()).collect();
        let name = generate_with_prefix(None, &banned);
        assert!(
            name.rsplit('-')
                .next()
                .and_then(|n| n.parse::<u32>().ok())
                .is_some(),
            "expected numeric suffix, got {name}"
        );
    }

    // -- MainBranch / WorkspaceKind --------------------------------------

    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    fn project_with_initial_commit(branch: &str) -> (TempDir, Project) {
        let dir = TempDir::new().unwrap();
        let path = dir.path();

        let run = |args: &[&str]| {
            let out = StdCommand::new("git")
                .args(args)
                .current_dir(path)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };

        run(&["init", "-b", branch]);
        run(&["config", "user.email", "test@test.com"]);
        run(&["config", "user.name", "Test"]);
        run(&["commit", "--allow-empty", "-m", "init"]);

        let project = Project {
            id: Uuid::new_v4(),
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "test".into()),
            path: path.to_path_buf(),
            default_branch: branch.to_string(),
        };
        (dir, project)
    }

    #[test]
    fn main_branch_workspace_uses_current_head_and_no_worktree() {
        let (_dir, project) = project_with_initial_commit("trunk");
        let ws = Workspace::main_branch(&project);
        assert_eq!(ws.kind, WorkspaceKind::MainBranch);
        assert_eq!(ws.branch, "trunk");
        assert_eq!(ws.worktree_path, None);
        assert_eq!(ws.name, "default");
        assert_eq!(ws.project_id, project.id);
    }

    #[test]
    fn main_branch_falls_back_to_project_default_branch_outside_repo() {
        // No git repo at this path → current_branch() fails → fallback.
        let dir = TempDir::new().unwrap();
        let project = Project {
            id: Uuid::new_v4(),
            name: "no-repo".into(),
            path: dir.path().to_path_buf(),
            default_branch: "main".into(),
        };
        let ws = Workspace::main_branch(&project);
        assert_eq!(ws.kind, WorkspaceKind::MainBranch);
        assert_eq!(ws.branch, "main");
    }

    #[test]
    fn working_dir_main_branch_returns_project_path() {
        let (_dir, project) = project_with_initial_commit("main");
        let ws = Workspace::main_branch(&project);
        assert_eq!(ws.working_dir(&project), project.path.as_path());
    }

    #[test]
    fn working_dir_worktree_returns_worktree_path() {
        let (_dir, project) = project_with_initial_commit("main");
        let wt_path = std::path::PathBuf::from("/tmp/wt-test");
        let ws = Workspace {
            id: Uuid::new_v4(),
            project_id: project.id,
            name: "feat".into(),
            branch: "feat/x".into(),
            worktree_path: Some(wt_path.clone()),
            kind: WorkspaceKind::Worktree,
        };
        assert_eq!(ws.working_dir(&project), wt_path.as_path());
    }

    #[test]
    fn remove_refuses_main_branch() {
        let (_dir, project) = project_with_initial_commit("main");
        let ws = Workspace::main_branch(&project);
        assert!(ws.remove(&project).is_err());
        assert!(ws.remove_with_branch(&project).is_err());
    }

    #[test]
    fn legacy_workspace_json_deserializes_as_worktree() {
        let legacy = r#"{
            "id":"00000000-0000-0000-0000-000000000001",
            "project_id":"00000000-0000-0000-0000-000000000002",
            "name":"foo","branch":"feat/x","worktree_path":"/tmp/foo"
        }"#;
        let ws: Workspace = serde_json::from_str(legacy).unwrap();
        assert_eq!(ws.kind, WorkspaceKind::Worktree);
        assert_eq!(
            ws.worktree_path.as_deref(),
            Some(std::path::Path::new("/tmp/foo"))
        );
        assert_eq!(ws.branch, "feat/x");
    }
}
