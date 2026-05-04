use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Raw output from a git command.
#[derive(Debug)]
pub struct CommandOutput {
    pub stdout: Vec<u8>,
    pub stderr: String,
    pub success: bool,
}

/// Safe git subprocess runner with path validation.
///
/// All commands are executed with `current_dir` set to `repo_path`.
pub struct GitCommandRunner {
    repo_path: PathBuf,
}

impl GitCommandRunner {
    pub fn new(repo_path: impl Into<PathBuf>) -> Self {
        Self {
            repo_path: repo_path.into(),
        }
    }

    pub fn repo_path(&self) -> &Path {
        &self.repo_path
    }

    /// Run a git command and return stdout as a trimmed string.
    /// Fails if the command exits with non-zero status.
    pub fn run(&self, args: &[&str]) -> Result<String> {
        let output = self.run_raw(args)?;
        if !output.success {
            bail!(
                "git {} failed: {}",
                args.first().unwrap_or(&""),
                output.stderr.trim()
            );
        }
        String::from_utf8(output.stdout)
            .map(|s| s.trim().to_string())
            .context("git output is not valid UTF-8")
    }

    /// Run a git command and return the raw output (stdout bytes, stderr, success).
    /// Does not fail on non-zero exit status.
    pub fn run_raw(&self, args: &[&str]) -> Result<CommandOutput> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.repo_path)
            .output()
            .with_context(|| format!("failed to execute git {}", args.first().unwrap_or(&"")))?;

        Ok(CommandOutput {
            stdout: output.stdout,
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            success: output.status.success(),
        })
    }

    /// Run a git command and return stdout as raw bytes.
    /// Fails if the command exits with non-zero status.
    pub fn run_bytes(&self, args: &[&str]) -> Result<Vec<u8>> {
        let output = self.run_raw(args)?;
        if !output.success {
            bail!(
                "git {} failed: {}",
                args.first().unwrap_or(&""),
                output.stderr.trim()
            );
        }
        Ok(output.stdout)
    }
}

impl Clone for GitCommandRunner {
    fn clone(&self) -> Self {
        Self {
            repo_path: self.repo_path.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    fn init_test_repo() -> (TempDir, GitCommandRunner) {
        let dir = TempDir::new().unwrap();
        StdCommand::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        StdCommand::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        StdCommand::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let runner = GitCommandRunner::new(dir.path());
        (dir, runner)
    }

    #[test]
    fn test_run_success() {
        let (_dir, runner) = init_test_repo();
        let result = runner.run(&["rev-parse", "--git-dir"]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), ".git");
    }

    #[test]
    fn test_run_failure() {
        let (_dir, runner) = init_test_repo();
        let result = runner.run(&["log"]); // empty repo, no commits
        assert!(result.is_err());
    }

    #[test]
    fn test_run_raw_no_panic_on_failure() {
        let (_dir, runner) = init_test_repo();
        let output = runner.run_raw(&["log"]).unwrap();
        assert!(!output.success);
    }
}
