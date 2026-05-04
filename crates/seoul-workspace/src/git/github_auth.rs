//! GitHub auth token loader.
//!
//! Resolution order:
//! 1. `SEOUL_GITHUB_TOKEN` env var (test/CI override).
//! 2. `GITHUB_TOKEN` env var.
//! 3. `gh auth token` (most common — relies on the user having run `gh auth login`).
//!
//! Returns [`GhAuthError::GhNotInstalled`] if `gh` isn't on `PATH`, or
//! [`GhAuthError::NotAuthenticated`] if `gh auth status` is missing/invalid.

use std::process::Command;

#[derive(Debug)]
pub enum GhAuthError {
    GhNotInstalled,
    NotAuthenticated,
    Decode,
}

impl std::fmt::Display for GhAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GhNotInstalled => write!(f, "gh CLI not installed"),
            Self::NotAuthenticated => write!(f, "gh CLI is not authenticated"),
            Self::Decode => write!(f, "failed to decode gh auth token"),
        }
    }
}

impl std::error::Error for GhAuthError {}

pub fn load_github_token() -> Result<String, GhAuthError> {
    if let Ok(t) = std::env::var("SEOUL_GITHUB_TOKEN")
        && !t.is_empty()
    {
        return Ok(t);
    }
    if let Ok(t) = std::env::var("GITHUB_TOKEN")
        && !t.is_empty()
    {
        return Ok(t);
    }

    let out = Command::new("gh")
        .args(["auth", "token"])
        .output()
        .map_err(|_| GhAuthError::GhNotInstalled)?;
    if !out.status.success() {
        return Err(GhAuthError::NotAuthenticated);
    }
    let token = String::from_utf8(out.stdout)
        .map_err(|_| GhAuthError::Decode)?
        .trim()
        .to_string();
    if token.is_empty() {
        return Err(GhAuthError::NotAuthenticated);
    }
    Ok(token)
}
