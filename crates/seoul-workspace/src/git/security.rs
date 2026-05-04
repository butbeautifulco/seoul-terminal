use std::path::{Component, Path};

use anyhow::{Result, bail};

/// Validate that a relative file path is safe for git commands.
///
/// Rejects:
/// - Absolute paths
/// - Path traversal (`..` segments)
/// - Flag-like strings (starting with `-`)
/// - NUL bytes
pub fn validate_git_path(path: &str) -> Result<()> {
    // Reject empty paths
    if path.is_empty() {
        return Ok(()); // Empty path is allowed (means root/current dir)
    }

    // Reject NUL bytes
    if path.contains('\0') {
        bail!("path contains NUL byte");
    }

    // Reject flag-like paths (prevent flag injection)
    if path.starts_with('-') {
        bail!("path cannot start with '-': {path}");
    }

    // Reject absolute paths
    let p = Path::new(path);
    if p.is_absolute() {
        bail!("absolute paths are not allowed: {path}");
    }

    // Reject path traversal
    for component in p.components() {
        if matches!(component, Component::ParentDir) {
            bail!("path traversal not allowed: {path}");
        }
    }

    Ok(())
}

/// Validate multiple git paths.
pub fn validate_git_paths(paths: &[&str]) -> Result<()> {
    for path in paths {
        validate_git_path(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_paths() {
        assert!(validate_git_path("").is_ok());
        assert!(validate_git_path("src/main.rs").is_ok());
        assert!(validate_git_path("file.txt").is_ok());
        assert!(validate_git_path("a/b/c/d.rs").is_ok());
        assert!(validate_git_path("..foo").is_ok()); // not traversal
        assert!(validate_git_path("foo..bar").is_ok());
    }

    #[test]
    fn test_absolute_path_rejected() {
        assert!(validate_git_path("/etc/passwd").is_err());
        assert!(validate_git_path("/home/user/file").is_err());
    }

    #[test]
    fn test_path_traversal_rejected() {
        assert!(validate_git_path("../etc/passwd").is_err());
        assert!(validate_git_path("foo/../../bar").is_err());
        assert!(validate_git_path("..").is_err());
    }

    #[test]
    fn test_flag_injection_rejected() {
        assert!(validate_git_path("-rf").is_err());
        assert!(validate_git_path("--force").is_err());
        assert!(validate_git_path("-").is_err());
    }

    #[test]
    fn test_nul_byte_rejected() {
        assert!(validate_git_path("file\0.txt").is_err());
    }

    #[test]
    fn test_validate_multiple() {
        assert!(validate_git_paths(&["src/a.rs", "src/b.rs"]).is_ok());
        assert!(validate_git_paths(&["src/a.rs", "../etc/passwd"]).is_err());
    }
}
