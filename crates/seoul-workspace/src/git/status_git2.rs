//! Fast git status using libgit2 (no subprocess overhead).
//!
//! This module provides a faster alternative to the subprocess-based status
//! computation for file-level status queries. The full `compute_status` function
//! in `status.rs` still uses git CLI for features not well-supported by libgit2
//! (ahead/behind counts relative to remote, commit log parsing, etc.).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::git::types::{BranchInfo, ChangedFile, FileStatus};

/// Open a git2 repository at the given path.
pub fn open_repo(path: &Path) -> Result<git2::Repository> {
    git2::Repository::open(path)
        .with_context(|| format!("failed to open git repo at {}", path.display()))
}

/// Get file statuses using libgit2.
/// This is significantly faster than spawning `git status --porcelain=v2`.
pub fn file_statuses(
    repo: &git2::Repository,
) -> Result<(Vec<ChangedFile>, Vec<ChangedFile>, Vec<ChangedFile>)> {
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        .renames_head_to_index(true);

    let statuses = repo
        .statuses(Some(&mut opts))
        .context("failed to get git statuses")?;

    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    let mut untracked = Vec::new();

    for entry in statuses.iter() {
        let path = entry.path().unwrap_or("").to_string();
        let status = entry.status();

        // Index (staged) changes
        if status.intersects(
            git2::Status::INDEX_NEW
                | git2::Status::INDEX_MODIFIED
                | git2::Status::INDEX_DELETED
                | git2::Status::INDEX_RENAMED,
        ) {
            let file_status = if status.contains(git2::Status::INDEX_NEW) {
                FileStatus::Added
            } else if status.contains(git2::Status::INDEX_MODIFIED) {
                FileStatus::Modified
            } else if status.contains(git2::Status::INDEX_DELETED) {
                FileStatus::Deleted
            } else {
                FileStatus::Renamed
            };
            staged.push(ChangedFile {
                path: path.clone(),
                old_path: None,
                status: file_status,
                additions: 0,
                deletions: 0,
            });
        }

        // Worktree (unstaged) changes
        if status.intersects(
            git2::Status::WT_MODIFIED | git2::Status::WT_DELETED | git2::Status::WT_RENAMED,
        ) {
            let file_status = if status.contains(git2::Status::WT_MODIFIED) {
                FileStatus::Modified
            } else if status.contains(git2::Status::WT_DELETED) {
                FileStatus::Deleted
            } else {
                FileStatus::Renamed
            };
            unstaged.push(ChangedFile {
                path: path.clone(),
                old_path: None,
                status: file_status,
                additions: 0,
                deletions: 0,
            });
        }

        // Untracked files
        if status.contains(git2::Status::WT_NEW) {
            untracked.push(ChangedFile {
                path,
                old_path: None,
                status: FileStatus::Untracked,
                additions: 0,
                deletions: 0,
            });
        }
    }

    Ok((staged, unstaged, untracked))
}

/// Get the current branch name using libgit2.
pub fn current_branch(repo: &git2::Repository) -> Result<String> {
    let head = repo.head().context("failed to get HEAD")?;
    if let Some(name) = head.shorthand() {
        Ok(name.to_string())
    } else {
        Ok("HEAD".to_string())
    }
}

/// List local branches using libgit2.
pub fn list_branches(repo: &git2::Repository) -> Result<Vec<BranchInfo>> {
    let branches = repo
        .branches(Some(git2::BranchType::Local))
        .context("failed to list branches")?;

    let head = repo.head().ok();
    let current_name = head.as_ref().and_then(|h| h.shorthand()).unwrap_or("");

    let mut result = Vec::new();
    for branch in branches {
        let (branch, _) = branch.context("failed to read branch")?;
        let name = branch.name()?.unwrap_or("").to_string();
        let is_current = name == current_name;

        let last_commit_date = branch.get().peel_to_commit().ok().map(|c| {
            let time = c.time();
            let secs = time.seconds();
            // Simple ISO-like format

            chrono_from_epoch(secs)
        });

        result.push(BranchInfo {
            name,
            is_current,
            last_commit_date,
        });
    }

    Ok(result)
}

/// Get a file-status HashMap for decorating the file tree (fast path).
pub fn file_status_map(repo: &git2::Repository) -> HashMap<String, FileStatus> {
    let mut map = HashMap::new();
    if let Ok((staged, unstaged, untracked)) = file_statuses(repo) {
        for f in staged {
            map.insert(f.path, f.status);
        }
        for f in unstaged {
            map.insert(f.path, f.status);
        }
        for f in untracked {
            map.insert(f.path, f.status);
        }
    }
    map
}

fn chrono_from_epoch(secs: i64) -> String {
    // RFC 3339 / ISO 8601 UTC timestamp without pulling in chrono.
    let secs = if secs < 0 { 0 } else { secs };
    let days_since_epoch = secs / 86_400;
    let secs_of_day = (secs % 86_400) as u32;
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    let (year, month, day) = civil_from_days(days_since_epoch);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Convert days-since-1970-01-01 to (year, month, day) using Howard Hinnant's
/// algorithm (https://howardhinnant.github.io/date_algorithms.html).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m as u32, d as u32)
}
