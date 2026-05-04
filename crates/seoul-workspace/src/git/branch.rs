use anyhow::{Result, bail};

use crate::git::runner::GitCommandRunner;
use crate::git::types::BranchInfo;

/// Get the current branch name. Returns None if HEAD is detached.
pub fn current_branch(runner: &GitCommandRunner) -> Result<Option<String>> {
    let output = runner.run_raw(&["rev-parse", "--abbrev-ref", "HEAD"])?;
    if !output.success {
        return Ok(None);
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch == "HEAD" {
        // Detached HEAD
        Ok(None)
    } else {
        Ok(Some(branch))
    }
}

/// List local branches with last commit dates.
pub fn list_local_branches(runner: &GitCommandRunner) -> Result<Vec<BranchInfo>> {
    let output = runner.run(&[
        "for-each-ref",
        "--format=%(refname:short)|%(HEAD)|%(committerdate:iso8601)",
        "refs/heads/",
        "--sort=-committerdate",
    ])?;

    let mut branches = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.splitn(3, '|').collect();
        if parts.is_empty() {
            continue;
        }
        let name = parts[0].to_string();
        let is_current = parts.get(1).is_some_and(|s| s.trim() == "*");
        let last_commit_date = parts.get(2).map(|s| s.trim().to_string());

        branches.push(BranchInfo {
            name,
            is_current,
            last_commit_date,
        });
    }

    Ok(branches)
}

/// List remote branches (names only).
pub fn list_remote_branches(runner: &GitCommandRunner) -> Result<Vec<String>> {
    let output = runner.run(&[
        "for-each-ref",
        "--format=%(refname:short)",
        "refs/remotes/",
        "--sort=-committerdate",
    ])?;

    Ok(output
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.ends_with("/HEAD"))
        .collect())
}

/// Switch to a branch. Uses `git switch` with `git checkout` fallback for older git.
pub fn switch_branch(runner: &GitCommandRunner, name: &str) -> Result<()> {
    if name.trim().is_empty() {
        bail!("branch name cannot be empty");
    }
    if name.starts_with('-') {
        bail!("invalid branch name: cannot start with '-'");
    }

    // Try `git switch` first (git 2.23+)
    let output = runner.run_raw(&["switch", name])?;
    if output.success {
        return Ok(());
    }

    // Check if `switch` command is unsupported
    if output.stderr.contains("is not a git command") {
        // Fallback: git checkout <branch> (no --)
        runner.run(&["checkout", name])?;
        return Ok(());
    }

    bail!("git switch failed: {}", output.stderr.trim());
}

/// Create a new branch from a base branch.
pub fn create_branch(runner: &GitCommandRunner, name: &str, base: &str) -> Result<()> {
    if name.trim().is_empty() {
        bail!("branch name cannot be empty");
    }
    if name.starts_with('-') {
        bail!("invalid branch name: cannot start with '-'");
    }
    runner.run(&["branch", name, base])?;
    Ok(())
}

/// Delete a local branch.
pub fn delete_branch(runner: &GitCommandRunner, name: &str, force: bool) -> Result<()> {
    if name.trim().is_empty() {
        bail!("branch name cannot be empty");
    }
    let flag = if force { "-D" } else { "-d" };
    runner.run(&["branch", flag, name])?;
    Ok(())
}

/// Detect the default branch name (main or master).
/// Reuses the logic from worktree.rs but via GitCommandRunner.
pub fn detect_default_branch(runner: &GitCommandRunner) -> Result<String> {
    // Try symbolic-ref first
    if let Ok(output) = runner.run(&["symbolic-ref", "refs/remotes/origin/HEAD"])
        && let Some(branch) = output.rsplit('/').next()
    {
        return Ok(branch.to_string());
    }

    // Fallback: check if "main" exists
    if runner.run_raw(&["rev-parse", "--verify", "main"])?.success {
        return Ok("main".into());
    }

    // Fallback: check "master"
    if runner
        .run_raw(&["rev-parse", "--verify", "master"])?
        .success
    {
        return Ok("master".into());
    }

    Ok("main".into())
}
