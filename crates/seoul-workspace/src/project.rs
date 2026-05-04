use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::worktree;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub path: PathBuf,
    pub default_branch: String,
}

impl Project {
    /// Register a new project from a directory path.
    /// Detects the git repo root and default branch automatically.
    pub fn register(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let repo_root = detect_repo_root(path)?;
        let name = repo_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unnamed".into());
        let default_branch =
            worktree::detect_default_branch(&repo_root).unwrap_or_else(|_| "main".into());

        Ok(Self {
            id: Uuid::new_v4(),
            name,
            path: repo_root,
            default_branch,
        })
    }
}

fn detect_repo_root(path: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(path)
        .output()
        .context("Failed to execute git rev-parse")?;

    if !output.status.success() {
        bail!(
            "Not a git repository: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let root = String::from_utf8(output.stdout)
        .context("Invalid UTF-8 in git output")?
        .trim()
        .to_string();

    Ok(PathBuf::from(root))
}
