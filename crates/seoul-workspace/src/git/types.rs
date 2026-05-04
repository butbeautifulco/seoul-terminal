use serde::{Deserialize, Serialize};

/// File status from git, matching short format codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    Untracked,
}

impl FileStatus {
    /// Whether this status represents a brand-new file (no previous version to diff against).
    pub fn is_new_file(self) -> bool {
        matches!(self, FileStatus::Added | FileStatus::Untracked)
    }
}

/// Change categories for organizing the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChangeCategory {
    AgainstBase,
    Committed,
    Staged,
    Unstaged,
}

impl ChangeCategory {
    /// Whether a diff category supports editing (saving changes back to disk).
    pub fn is_editable(self) -> bool {
        matches!(self, ChangeCategory::Staged | ChangeCategory::Unstaged)
    }
}

/// A changed file entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    /// Relative path from repo root.
    pub path: String,
    /// Original path for renames/copies.
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub additions: u32,
    pub deletions: u32,
}

/// A commit summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitInfo {
    pub hash: String,
    /// Short hash (7 chars).
    pub short_hash: String,
    /// Commit message (first line).
    pub message: String,
    pub author: String,
    /// ISO 8601 date string.
    pub date: String,
    pub files: Vec<ChangedFile>,
}

/// Full git changes status for a worktree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitChangesStatus {
    pub branch: String,
    pub default_branch: String,
    /// All files changed vs base branch.
    pub against_base: Vec<ChangedFile>,
    /// Individual commits on branch (not on default).
    pub commits: Vec<CommitInfo>,
    pub staged: Vec<ChangedFile>,
    pub unstaged: Vec<ChangedFile>,
    pub untracked: Vec<ChangedFile>,
    /// Commits ahead of default branch.
    pub ahead: u32,
    /// Commits behind default branch.
    pub behind: u32,
    /// Commits to push to tracking branch.
    pub push_count: u32,
    /// Commits to pull from tracking branch.
    pub pull_count: u32,
    /// Whether branch has an upstream tracking branch.
    pub has_upstream: bool,
}

/// Branch information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchInfo {
    pub name: String,
    pub is_current: bool,
    /// ISO 8601 date of last commit.
    pub last_commit_date: Option<String>,
}

/// File contents for diff viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileContents {
    /// Original content (before changes).
    pub original: String,
    /// Modified content (after changes).
    pub modified: String,
    /// Detected language for syntax highlighting.
    pub language: String,
}

/// Diff view mode toggle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffViewMode {
    SideBySide,
    Inline,
}
