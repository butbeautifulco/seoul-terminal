use anyhow::Result;

use crate::git::parse::{
    apply_numstat, parse_log_output, parse_name_status, parse_numstat, parse_porcelain_v2,
};
use crate::git::runner::GitCommandRunner;
use crate::git::types::GitChangesStatus;

/// Compute the full git changes status for a worktree.
///
/// This runs multiple git commands and aggregates the results.
/// Should be called from a background thread to avoid blocking the UI.
pub fn compute_status(runner: &GitCommandRunner, default_branch: &str) -> Result<GitChangesStatus> {
    // 1. Parse git status
    let status_output = runner.run_bytes(&["status", "--porcelain=v2", "-z"])?;
    let (branch, mut staged, mut unstaged, untracked) = parse_porcelain_v2(&status_output);

    // 2. Get ahead/behind counts against default branch
    let (ahead, behind) = get_ahead_behind(runner, default_branch);

    // 3. Get commit log
    let commits = get_commit_log(runner, default_branch);

    // 4. Get against-base file changes
    let mut against_base = get_against_base(runner, default_branch);

    // 5. Apply numstat to staged and unstaged
    apply_staged_unstaged_numstat(runner, &mut staged, &mut unstaged);

    // 6. Apply numstat to against-base
    if !against_base.is_empty()
        && let Ok(output) = runner.run(&[
            "diff",
            "--numstat",
            &format!("origin/{default_branch}...HEAD"),
        ])
    {
        let numstat = parse_numstat(&output);
        apply_numstat(&mut against_base, &numstat);
    }

    // 7. Get tracking branch status
    let (push_count, pull_count, has_upstream) = get_tracking_status(runner);

    Ok(GitChangesStatus {
        branch,
        default_branch: default_branch.to_string(),
        against_base,
        commits,
        staged,
        unstaged,
        untracked,
        ahead,
        behind,
        push_count,
        pull_count,
        has_upstream,
    })
}

fn get_ahead_behind(runner: &GitCommandRunner, default_branch: &str) -> (u32, u32) {
    let range = format!("origin/{default_branch}...HEAD");
    let Ok(output) = runner.run(&["rev-list", "--left-right", "--count", &range]) else {
        return (0, 0);
    };

    let parts: Vec<&str> = output.split_whitespace().collect();
    if parts.len() >= 2 {
        let behind = parts[0].parse().unwrap_or(0);
        let ahead = parts[1].parse().unwrap_or(0);
        (ahead, behind)
    } else {
        (0, 0)
    }
}

fn get_commit_log(
    runner: &GitCommandRunner,
    default_branch: &str,
) -> Vec<crate::git::types::CommitInfo> {
    let range = format!("origin/{default_branch}..HEAD");
    let Ok(output) = runner.run(&[
        "log",
        &range,
        "--max-count=500",
        "--format=%H|%h|%s|%an|%aI",
    ]) else {
        return Vec::new();
    };

    parse_log_output(&output)
}

fn get_against_base(
    runner: &GitCommandRunner,
    default_branch: &str,
) -> Vec<crate::git::types::ChangedFile> {
    let range = format!("origin/{default_branch}...HEAD");
    let Ok(output) = runner.run(&["diff", "--name-status", &range]) else {
        return Vec::new();
    };

    parse_name_status(&output)
}

fn apply_staged_unstaged_numstat(
    runner: &GitCommandRunner,
    staged: &mut [crate::git::types::ChangedFile],
    unstaged: &mut [crate::git::types::ChangedFile],
) {
    // Staged numstat
    if !staged.is_empty()
        && let Ok(output) = runner.run(&["diff", "--cached", "--numstat"])
    {
        let numstat = parse_numstat(&output);
        apply_numstat(staged, &numstat);
    }

    // Unstaged numstat
    if !unstaged.is_empty()
        && let Ok(output) = runner.run(&["diff", "--numstat"])
    {
        let numstat = parse_numstat(&output);
        apply_numstat(unstaged, &numstat);
    }
}

fn get_tracking_status(runner: &GitCommandRunner) -> (u32, u32, bool) {
    // Check if upstream exists
    let Ok(upstream) = runner.run(&["rev-parse", "--abbrev-ref", "@{upstream}"]) else {
        return (0, 0, false);
    };

    if upstream.trim().is_empty() {
        return (0, 0, false);
    }

    // Get push/pull counts
    let Ok(output) = runner.run(&["rev-list", "--left-right", "--count", "@{upstream}...HEAD"])
    else {
        return (0, 0, true);
    };

    let parts: Vec<&str> = output.split_whitespace().collect();
    if parts.len() >= 2 {
        let pull_count = parts[0].parse().unwrap_or(0);
        let push_count = parts[1].parse().unwrap_or(0);
        (push_count, pull_count, true)
    } else {
        (0, 0, true)
    }
}
