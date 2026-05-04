use anyhow::{Result, bail};

use crate::git::runner::GitCommandRunner;

pub use seoul_terminal_proto::pr::{ChecksStatus, PrInfo, PrState, ReviewDecision};

/// PR merge strategy.
#[derive(Debug, Clone, Copy)]
pub enum MergeStrategy {
    Merge,
    Squash,
    Rebase,
}

/// Create a PR for the current branch using GitHub CLI.
/// Returns the PR URL.
pub fn create_pr(runner: &GitCommandRunner) -> Result<String> {
    // Ensure current branch is pushed
    let branch = runner.run(&["rev-parse", "--abbrev-ref", "HEAD"])?;
    if branch == "HEAD" {
        bail!("cannot create PR from detached HEAD");
    }

    // Try `gh pr create`
    let output = std::process::Command::new("gh")
        .args(["pr", "create", "--fill", "--web"])
        .current_dir(runner.repo_path())
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
            Ok(url)
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            // Check if PR already exists
            if stderr.contains("already exists") {
                // Get existing PR URL
                return pr_url(runner);
            }
            bail!("gh pr create failed: {}", stderr.trim());
        }
        Err(e) => bail!("gh command not found: {e}. Install GitHub CLI: https://cli.github.com"),
    }
}

/// Merge a PR using GitHub CLI.
pub fn merge_pr(runner: &GitCommandRunner, strategy: MergeStrategy) -> Result<()> {
    let flag = match strategy {
        MergeStrategy::Merge => "--merge",
        MergeStrategy::Squash => "--squash",
        MergeStrategy::Rebase => "--rebase",
    };

    let output = std::process::Command::new("gh")
        .args(["pr", "merge", flag])
        .current_dir(runner.repo_path())
        .output();

    match output {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            bail!("gh pr merge failed: {}", stderr.trim());
        }
        Err(e) => bail!("gh command not found: {e}"),
    }
}

/// Get the PR URL for the current branch.
pub fn pr_url(runner: &GitCommandRunner) -> Result<String> {
    let output = std::process::Command::new("gh")
        .args(["pr", "view", "--json", "url", "-q", ".url"])
        .current_dir(runner.repo_path())
        .output();

    match output {
        Ok(out) if out.status.success() => {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            bail!("no PR found: {}", stderr.trim());
        }
        Err(e) => bail!("gh command not found: {e}"),
    }
}

// PR fetching for the rich `PrInfo` model lives in `crate::git::providers`,
// behind the `HostingProvider` trait. This module retains only PR creation /
// merge / URL helpers that wrap the `gh` CLI.
