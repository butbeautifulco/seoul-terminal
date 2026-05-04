use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Create a git worktree at `target_path` on a new branch `branch_name`,
/// branching from `base_branch`.
pub fn create_worktree(
    repo_path: &Path,
    branch_name: &str,
    base_branch: &str,
    target_path: &Path,
) -> Result<()> {
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create worktree parent directory")?;
    }

    let output = Command::new("git")
        .args(["worktree", "add", "-b", branch_name])
        .arg(target_path)
        .arg(base_branch)
        .current_dir(repo_path)
        .output()
        .context("Failed to execute git worktree add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git worktree add failed: {}", stderr.trim());
    }
    Ok(())
}

/// Remove a git worktree.
pub fn remove_worktree(repo_path: &Path, target_path: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(target_path)
        .current_dir(repo_path)
        .output()
        .context("Failed to execute git worktree remove")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git worktree remove failed: {}", stderr.trim());
    }
    Ok(())
}

/// Detect the default branch name (main or master).
pub fn detect_default_branch(repo_path: &Path) -> Result<String> {
    // Try symbolic-ref first
    let output = Command::new("git")
        .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
        .current_dir(repo_path)
        .output()
        .context("Failed to execute git symbolic-ref")?;

    if output.status.success() {
        let refname = String::from_utf8(output.stdout)
            .context("Invalid UTF-8")?
            .trim()
            .to_string();
        // refs/remotes/origin/main → main
        if let Some(branch) = refname.rsplit('/').next() {
            return Ok(branch.to_string());
        }
    }

    // Fallback: check if "main" branch exists
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "main"])
        .current_dir(repo_path)
        .output()
        .context("Failed to verify main branch")?;

    if output.status.success() {
        return Ok("main".into());
    }

    // Fallback: check "master"
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "master"])
        .current_dir(repo_path)
        .output()
        .context("Failed to verify master branch")?;

    if output.status.success() {
        return Ok("master".into());
    }

    Ok("main".into())
}
