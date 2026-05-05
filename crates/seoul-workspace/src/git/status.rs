use anyhow::Result;

use crate::git::parse::{
    apply_numstat, parse_log_output, parse_name_status, parse_numstat, parse_porcelain_v2,
};
use crate::git::runner::GitCommandRunner;
use crate::git::types::GitChangesStatus;

/// Compute the full git changes status for a worktree.
///
/// Runs all required git subprocesses concurrently via `std::thread::scope`
/// and aggregates the results. Each individual `git` invocation still blocks
/// (it forks/execs and waits), but they fan out across OS threads so the
/// total wall-clock time is bounded by the slowest call rather than the sum.
///
/// Should be called from a background thread (the caller already does this
/// via `smol::unblock` in the GPUI app); the function itself is sync.
pub fn compute_status(runner: &GitCommandRunner, default_branch: &str) -> Result<GitChangesStatus> {
    // We launch every independent git command up front. A few of the numstat
    // queries might be redundant (their target list could end up empty), but
    // each fork is single-digit ms and running them speculatively in parallel
    // is still a net win versus sequential execution.
    //
    // Tradeoff vs `tokio::join!`: we use `std::thread::scope` because
    // `seoul-workspace` does not depend on tokio (only `seoul-daemon` does)
    // and the GPUI caller offloads via `smol::unblock`, so there is no ambient
    // tokio runtime to piggyback on. Scoped threads also let us borrow
    // `&runner` directly without cloning.
    let against_base_range = format!("origin/{default_branch}...HEAD");
    let log_range = format!("origin/{default_branch}..HEAD");

    let (
        status_output,
        ahead_behind_output,
        log_output,
        against_base_output,
        upstream_output,
        staged_numstat_output,
        unstaged_numstat_output,
        against_base_numstat_output,
    ) = std::thread::scope(|scope| {
        let status_h = scope.spawn(|| runner.run_bytes(&["status", "--porcelain=v2", "-z"]));
        let ahead_behind_h = scope
            .spawn(|| runner.run(&["rev-list", "--left-right", "--count", &against_base_range]));
        let log_h = scope.spawn(|| {
            runner.run(&[
                "log",
                &log_range,
                "--max-count=500",
                "--format=%H|%h|%s|%an|%aI",
            ])
        });
        let against_base_h =
            scope.spawn(|| runner.run(&["diff", "--name-status", &against_base_range]));
        let upstream_h = scope.spawn(|| runner.run(&["rev-parse", "--abbrev-ref", "@{upstream}"]));
        let staged_numstat_h = scope.spawn(|| runner.run(&["diff", "--cached", "--numstat"]));
        let unstaged_numstat_h = scope.spawn(|| runner.run(&["diff", "--numstat"]));
        let against_base_numstat_h =
            scope.spawn(|| runner.run(&["diff", "--numstat", &against_base_range]));

        // `join` on a scoped thread only fails if the closure panicked. We
        // don't expect that here; propagate via expect since a panic in any
        // git fork is a real bug we want to surface.
        (
            status_h.join().expect("status thread panicked"),
            ahead_behind_h.join().expect("ahead/behind thread panicked"),
            log_h.join().expect("log thread panicked"),
            against_base_h.join().expect("against-base thread panicked"),
            upstream_h.join().expect("upstream thread panicked"),
            staged_numstat_h
                .join()
                .expect("staged numstat thread panicked"),
            unstaged_numstat_h
                .join()
                .expect("unstaged numstat thread panicked"),
            against_base_numstat_h
                .join()
                .expect("against-base numstat thread panicked"),
        )
    });

    // Status is the only call we hard-fail on; the rest mirror the previous
    // graceful-degradation behavior (e.g. an empty repo has no upstream and
    // `log` returns an error, which we silently treat as "no commits").
    let status_output = status_output?;
    let (branch, mut staged, mut unstaged, untracked) = parse_porcelain_v2(&status_output);

    // Ahead/behind counts.
    let (ahead, behind) = parse_ahead_behind(ahead_behind_output.as_deref().ok());

    // Commit log.
    let commits = log_output
        .as_deref()
        .map(parse_log_output)
        .unwrap_or_default();

    // Against-base file changes.
    let mut against_base = against_base_output
        .as_deref()
        .map(parse_name_status)
        .unwrap_or_default();

    // Apply numstat to staged/unstaged/against_base. We always have the
    // numstat outputs at this point (they ran in parallel); we just skip the
    // merge when the target list is empty, matching the original behavior.
    if !staged.is_empty()
        && let Ok(out) = staged_numstat_output.as_deref()
    {
        let numstat = parse_numstat(out);
        apply_numstat(&mut staged, &numstat);
    }
    if !unstaged.is_empty()
        && let Ok(out) = unstaged_numstat_output.as_deref()
    {
        let numstat = parse_numstat(out);
        apply_numstat(&mut unstaged, &numstat);
    }
    if !against_base.is_empty()
        && let Ok(out) = against_base_numstat_output.as_deref()
    {
        let numstat = parse_numstat(out);
        apply_numstat(&mut against_base, &numstat);
    }

    // Tracking branch status: needs the upstream check first, then a second
    // rev-list call. We can't fan this in with the rest because the second
    // call must not run when there is no upstream (it would fail/spam).
    let (push_count, pull_count, has_upstream) =
        compute_tracking_status(runner, upstream_output.as_deref().ok());

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

/// Parse `git rev-list --left-right --count` output.
///
/// Format is `<left>\t<right>` where left is commits reachable from the
/// first ref but not the second (= behind) and right is the inverse (= ahead).
fn parse_ahead_behind(output: Option<&str>) -> (u32, u32) {
    let Some(output) = output else {
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

/// Compute push/pull counts against the upstream tracking branch.
///
/// Kept sequential because the second rev-list call is meaningless (and
/// would log noise) when no upstream is configured.
fn compute_tracking_status(runner: &GitCommandRunner, upstream: Option<&str>) -> (u32, u32, bool) {
    let Some(upstream) = upstream else {
        return (0, 0, false);
    };
    if upstream.trim().is_empty() {
        return (0, 0, false);
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    use crate::git::types::FileStatus;

    fn init_repo() -> (TempDir, GitCommandRunner) {
        let dir = TempDir::new().unwrap();
        let path = dir.path();
        for args in [
            ["init", "--initial-branch=main"].as_slice(),
            ["config", "user.email", "test@test.com"].as_slice(),
            ["config", "user.name", "Test"].as_slice(),
            ["config", "commit.gpgsign", "false"].as_slice(),
        ] {
            StdCommand::new("git")
                .args(args)
                .current_dir(path)
                .output()
                .unwrap();
        }
        let runner = GitCommandRunner::new(path);
        (dir, runner)
    }

    fn git(runner: &GitCommandRunner, args: &[&str]) {
        let out = StdCommand::new("git")
            .args(args)
            .current_dir(runner.repo_path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn compute_status_categorizes_changes() {
        let (dir, runner) = init_repo();
        let path = dir.path();

        // Initial commit so `log` has something to compare against.
        fs::write(path.join("base.txt"), "base\n").unwrap();
        git(&runner, &["add", "base.txt"]);
        git(&runner, &["commit", "-m", "init"]);

        // Stage a new file.
        fs::write(path.join("staged.txt"), "staged\n").unwrap();
        git(&runner, &["add", "staged.txt"]);

        // Modify the base file but don't stage it.
        fs::write(path.join("base.txt"), "base modified\n").unwrap();

        // Untracked file.
        fs::write(path.join("untracked.txt"), "u\n").unwrap();

        let status = compute_status(&runner, "main").expect("compute_status");

        // `branch` is left as the parser default ("HEAD") because the status
        // command does not pass `--branch`; downstream code (app_view.rs)
        // looks up the real name via a separate symbolic-ref fallback. This
        // test pins the *current* contract of `compute_status` rather than
        // the eventual displayed branch name.
        assert_eq!(status.branch, "HEAD");
        assert_eq!(status.default_branch, "main");
        assert!(!status.has_upstream, "no remote configured");
        assert_eq!(status.push_count, 0);
        assert_eq!(status.pull_count, 0);

        let staged_paths: Vec<&str> = status.staged.iter().map(|f| f.path.as_str()).collect();
        let unstaged_paths: Vec<&str> = status.unstaged.iter().map(|f| f.path.as_str()).collect();
        let untracked_paths: Vec<&str> = status.untracked.iter().map(|f| f.path.as_str()).collect();

        assert_eq!(staged_paths, ["staged.txt"]);
        assert_eq!(unstaged_paths, ["base.txt"]);
        assert_eq!(untracked_paths, ["untracked.txt"]);

        // Numstat should have been merged in for the modified unstaged file.
        let unstaged_modified = &status.unstaged[0];
        assert_eq!(unstaged_modified.status, FileStatus::Modified);
        assert!(
            unstaged_modified.additions >= 1 || unstaged_modified.deletions >= 1,
            "expected numstat to populate additions/deletions, got {:?}",
            unstaged_modified
        );
    }

    #[test]
    fn compute_status_clean_repo() {
        let (dir, runner) = init_repo();
        let path = dir.path();

        fs::write(path.join("file.txt"), "x\n").unwrap();
        git(&runner, &["add", "file.txt"]);
        git(&runner, &["commit", "-m", "init"]);

        let status = compute_status(&runner, "main").expect("compute_status");

        // See note in compute_status_categorizes_changes: branch is "HEAD"
        // since the status command isn't run with `--branch`.
        assert_eq!(status.branch, "HEAD");
        assert!(status.staged.is_empty());
        assert!(status.unstaged.is_empty());
        assert!(status.untracked.is_empty());
        assert_eq!(status.ahead, 0);
        assert_eq!(status.behind, 0);
        assert!(!status.has_upstream);
    }
}
