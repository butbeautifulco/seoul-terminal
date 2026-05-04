use std::fs;

use anyhow::{Result, bail};

use crate::git::runner::GitCommandRunner;
use crate::git::types::{ChangeCategory, FileContents};

const MAX_FILE_SIZE: usize = 2 * 1024 * 1024; // 2 MB

/// Get the original and modified contents of a file for diff viewing.
pub fn get_file_diff(
    runner: &GitCommandRunner,
    path: &str,
    category: ChangeCategory,
    default_branch: &str,
    commit_hash: Option<&str>,
) -> Result<FileContents> {
    let language = detect_language(path);

    match category {
        ChangeCategory::Staged => {
            // original = HEAD version, modified = index (staged) version
            let original = git_show(runner, &format!("HEAD:{path}")).unwrap_or_default();
            let modified = git_show(runner, &format!(":{path}")).unwrap_or_default();
            Ok(FileContents {
                original,
                modified,
                language,
            })
        }
        ChangeCategory::Unstaged => {
            // original = index version, modified = working tree
            let original = git_show(runner, &format!(":{path}")).unwrap_or_default();
            let modified = read_working_file(runner, path)?;
            Ok(FileContents {
                original,
                modified,
                language,
            })
        }
        ChangeCategory::AgainstBase => {
            // original = default branch version, modified = working tree
            let original =
                git_show(runner, &format!("origin/{default_branch}:{path}")).unwrap_or_default();
            let modified = read_working_file(runner, path)?;
            Ok(FileContents {
                original,
                modified,
                language,
            })
        }
        ChangeCategory::Committed => {
            // original = parent commit, modified = commit version
            let hash = commit_hash.unwrap_or("HEAD");
            let original = git_show(runner, &format!("{hash}~1:{path}")).unwrap_or_default();
            let modified = git_show(runner, &format!("{hash}:{path}")).unwrap_or_default();
            Ok(FileContents {
                original,
                modified,
                language,
            })
        }
    }
}

/// Get unified diff output for a file.
pub fn get_unified_diff(
    runner: &GitCommandRunner,
    path: &str,
    category: ChangeCategory,
    default_branch: &str,
) -> Result<String> {
    match category {
        ChangeCategory::Staged => runner.run(&["diff", "--cached", "--", path]),
        ChangeCategory::Unstaged => runner.run(&["diff", "--", path]),
        ChangeCategory::AgainstBase => {
            let range = format!("origin/{default_branch}...HEAD");
            runner.run(&["diff", &range, "--", path])
        }
        ChangeCategory::Committed => {
            bail!("use get_file_diff for committed category")
        }
    }
}

fn git_show(runner: &GitCommandRunner, ref_path: &str) -> Result<String> {
    runner.run(&["show", ref_path])
}

fn read_working_file(runner: &GitCommandRunner, rel_path: &str) -> Result<String> {
    let abs_path = runner.repo_path().join(rel_path);
    let content = fs::read_to_string(&abs_path)?;
    if content.len() > MAX_FILE_SIZE {
        bail!("file too large for diff: {} bytes", content.len());
    }
    Ok(content)
}

fn detect_language(path: &str) -> String {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => "rust",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "jsx" => "jsx",
        "py" => "python",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" => "cpp",
        "java" => "java",
        "rb" => "ruby",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" => "markdown",
        "html" | "htm" => "html",
        "css" => "css",
        "sh" | "bash" | "zsh" => "bash",
        _ => "text",
    }
    .to_string()
}
