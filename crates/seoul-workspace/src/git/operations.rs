use anyhow::{Result, bail};

use crate::git::runner::GitCommandRunner;
use crate::git::security::validate_git_path;

// ---------------------------------------------------------------------------
// Staging
// ---------------------------------------------------------------------------

/// Stage a single file: `git add -- <path>`
pub fn stage_file(runner: &GitCommandRunner, path: &str) -> Result<()> {
    validate_git_path(path)?;
    runner.run(&["add", "--", path])?;
    Ok(())
}

/// Stage multiple files in a single command: `git add -- <paths...>`
pub fn stage_files(runner: &GitCommandRunner, paths: &[&str]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    for path in paths {
        validate_git_path(path)?;
    }
    let mut args = vec!["add", "--"];
    args.extend_from_slice(paths);
    runner.run(&args)?;
    Ok(())
}

/// Stage all changes: `git add -A`
pub fn stage_all(runner: &GitCommandRunner) -> Result<()> {
    runner.run(&["add", "-A"])?;
    Ok(())
}

/// Unstage a single file: `git reset HEAD -- <path>`
pub fn unstage_file(runner: &GitCommandRunner, path: &str) -> Result<()> {
    validate_git_path(path)?;
    runner.run(&["reset", "HEAD", "--", path])?;
    Ok(())
}

/// Unstage multiple files: `git reset HEAD -- <paths...>`
pub fn unstage_files(runner: &GitCommandRunner, paths: &[&str]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    for path in paths {
        validate_git_path(path)?;
    }
    let mut args = vec!["reset", "HEAD", "--"];
    args.extend_from_slice(paths);
    runner.run(&args)?;
    Ok(())
}

/// Unstage all changes: `git reset HEAD`
pub fn unstage_all(runner: &GitCommandRunner) -> Result<()> {
    runner.run(&["reset", "HEAD"])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Discard
// ---------------------------------------------------------------------------

/// Discard changes for a single file: `git checkout -- <path>`
pub fn discard_file(runner: &GitCommandRunner, path: &str) -> Result<()> {
    validate_git_path(path)?;
    runner.run(&["checkout", "--", path])?;
    Ok(())
}

/// Discard all unstaged changes: `git checkout -- .`
pub fn discard_all_unstaged(runner: &GitCommandRunner) -> Result<()> {
    runner.run(&["checkout", "--", "."])?;
    Ok(())
}

/// Discard all staged and unstaged changes: `git reset HEAD` + `git checkout -- .`
pub fn discard_all(runner: &GitCommandRunner) -> Result<()> {
    runner.run(&["reset", "HEAD"])?;
    runner.run(&["checkout", "--", "."])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Commit
// ---------------------------------------------------------------------------

/// Create a commit with the given message. Returns the commit hash.
pub fn commit(runner: &GitCommandRunner, message: &str) -> Result<String> {
    if message.trim().is_empty() {
        bail!("commit message cannot be empty");
    }
    runner.run(&["commit", "-m", message])?;
    // Get the hash of the commit we just made
    let hash = runner.run(&["rev-parse", "HEAD"])?;
    Ok(hash)
}

// ---------------------------------------------------------------------------
// Remote operations
// ---------------------------------------------------------------------------

/// Push the current branch. Optionally sets upstream.
pub fn push(runner: &GitCommandRunner, set_upstream: bool) -> Result<()> {
    if set_upstream {
        let branch = runner.run(&["rev-parse", "--abbrev-ref", "HEAD"])?;
        if branch == "HEAD" {
            bail!("cannot push: HEAD is detached (not on a branch)");
        }
        runner.run(&["push", "--set-upstream", "origin", &branch])?;
    } else {
        runner.run(&["push"])?;
    }
    Ok(())
}

/// Pull with rebase strategy.
pub fn pull(runner: &GitCommandRunner) -> Result<()> {
    runner.run(&["pull", "--rebase"])?;
    Ok(())
}

/// Fetch from remote.
pub fn fetch(runner: &GitCommandRunner) -> Result<()> {
    // Try branch-specific fetch first
    let output = runner.run_raw(&["fetch"])?;
    if !output.success {
        bail!("git fetch failed: {}", output.stderr.trim());
    }
    Ok(())
}

/// Sync: pull --rebase then push. Sets upstream on first push if needed.
pub fn sync(runner: &GitCommandRunner) -> Result<()> {
    // Check if upstream exists
    let has_upstream = runner
        .run(&["rev-parse", "--abbrev-ref", "@{upstream}"])
        .is_ok();

    if has_upstream {
        pull(runner)?;
        push(runner, false)?;
    } else {
        // No upstream — push with set-upstream
        push(runner, true)?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Stash
// ---------------------------------------------------------------------------

/// Stash current changes.
pub fn stash_push(runner: &GitCommandRunner, include_untracked: bool) -> Result<()> {
    if include_untracked {
        runner.run(&["stash", "push", "--include-untracked"])?;
    } else {
        runner.run(&["stash", "push"])?;
    }
    Ok(())
}

/// Pop the most recent stash.
pub fn stash_pop(runner: &GitCommandRunner) -> Result<()> {
    runner.run(&["stash", "pop"])?;
    Ok(())
}
