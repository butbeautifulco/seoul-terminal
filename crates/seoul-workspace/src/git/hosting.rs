//! Hosting provider abstraction for fetching pull-request status.
//!
//! Each hosting service (GitHub, GitLab, Gitea, …) implements [`HostingProvider`].
//! [`HostingRegistry`] dispatches based on the parsed remote URL host.
//!
//! Adding a new provider:
//! 1. Implement [`HostingProvider`] in a new module under `git::providers`.
//! 2. Register it via `HostingRegistry::with_provider`.

use std::sync::Arc;

use async_trait::async_trait;
use seoul_terminal_proto::pr::{PrInfo, PrUnavailableReason};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRemote {
    pub host: String,
    pub owner: String,
    pub repo: String,
}

#[derive(Debug, Clone)]
pub enum ProviderError {
    NotAuthenticated,
    RateLimited { reset_unix: i64 },
    Network(String),
    Other(String),
}

impl ProviderError {
    pub fn into_unavailable(self) -> PrUnavailableReason {
        match self {
            Self::NotAuthenticated => PrUnavailableReason::NotAuthenticated,
            Self::RateLimited { reset_unix } => PrUnavailableReason::RateLimited { reset_unix },
            Self::Network(message) => {
                let _ = message;
                PrUnavailableReason::Network
            }
            Self::Other(message) => PrUnavailableReason::Other { message },
        }
    }
}

#[async_trait]
pub trait HostingProvider: Send + Sync {
    fn host_id(&self) -> &str;
    fn matches_host(&self, host: &str) -> bool;
    fn create_pr_web_url(&self, remote: &ParsedRemote, branch: &str) -> String;
    async fn resolve_pr_for_branch(
        &self,
        remote: &ParsedRemote,
        branch: &str,
        head_sha: &str,
    ) -> Result<Option<PrInfo>, ProviderError>;
}

#[derive(Default)]
pub struct HostingRegistry {
    providers: Vec<Arc<dyn HostingProvider>>,
}

impl HostingRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_provider(mut self, provider: Arc<dyn HostingProvider>) -> Self {
        self.providers.push(provider);
        self
    }

    pub fn provider_for_remote(
        &self,
        remote_url: &str,
    ) -> Option<(Arc<dyn HostingProvider>, ParsedRemote)> {
        let parsed = parse_remote_url(remote_url)?;
        let provider = self
            .providers
            .iter()
            .find(|p| p.matches_host(&parsed.host))?;
        Some((Arc::clone(provider), parsed))
    }
}

/// Parse a git remote URL into `(host, owner, repo)`. Supports the two forms git uses:
/// - `https://github.com/owner/repo` (with optional `.git` suffix and trailing slash)
/// - `git@github.com:owner/repo` (SSH, with optional `.git`)
pub fn parse_remote_url(url: &str) -> Option<ParsedRemote> {
    let url = url.trim();
    let url = url.strip_suffix('/').unwrap_or(url);
    let url = url.strip_suffix(".git").unwrap_or(url);

    if let Some(rest) = url.strip_prefix("git@") {
        // git@host:owner/repo
        let (host, path) = rest.split_once(':')?;
        let (owner, repo) = path.split_once('/')?;
        // Trim sub-paths (e.g. ".../repo/wiki") — GitHub repos aren't nested.
        let repo = repo.split('/').next().unwrap_or(repo);
        if owner.is_empty() || repo.is_empty() {
            return None;
        }
        return Some(ParsedRemote {
            host: host.to_string(),
            owner: owner.to_string(),
            repo: repo.to_string(),
        });
    }

    if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    {
        // host/owner/repo  OR  user@host/owner/repo
        let after_at = rest.split_once('@').map(|(_, b)| b).unwrap_or(rest);
        let mut parts = after_at.splitn(3, '/');
        let host = parts.next()?;
        let owner = parts.next()?;
        let repo = parts.next()?;
        // Trim sub-paths (e.g. ".../repo/wiki") — GitHub repos aren't nested.
        let repo = repo.split('/').next().unwrap_or(repo);
        if host.is_empty() || owner.is_empty() || repo.is_empty() {
            return None;
        }
        return Some(ParsedRemote {
            host: host.to_string(),
            owner: owner.to_string(),
            repo: repo.to_string(),
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_https() {
        let r = parse_remote_url("https://github.com/owner/repo.git").unwrap();
        assert_eq!(r.host, "github.com");
        assert_eq!(r.owner, "owner");
        assert_eq!(r.repo, "repo");
    }

    #[test]
    fn parse_https_no_dot_git() {
        let r = parse_remote_url("https://github.com/owner/repo").unwrap();
        assert_eq!(r.repo, "repo");
    }

    #[test]
    fn parse_https_trailing_slash() {
        let r = parse_remote_url("https://github.com/owner/repo/").unwrap();
        assert_eq!(r.repo, "repo");
    }

    #[test]
    fn parse_ssh() {
        let r = parse_remote_url("git@github.com:owner/repo.git").unwrap();
        assert_eq!(r.host, "github.com");
        assert_eq!(r.owner, "owner");
        assert_eq!(r.repo, "repo");
    }

    #[test]
    fn parse_invalid() {
        assert!(parse_remote_url("not-a-url").is_none());
        assert!(parse_remote_url("https://github.com/owner").is_none());
    }

    #[test]
    fn parse_https_strips_subpath() {
        let r = parse_remote_url("https://github.com/owner/repo/wiki").unwrap();
        assert_eq!(r.host, "github.com");
        assert_eq!(r.owner, "owner");
        assert_eq!(r.repo, "repo");
    }

    #[test]
    fn parse_ssh_strips_subpath() {
        let r = parse_remote_url("git@github.com:owner/repo/wiki").unwrap();
        assert_eq!(r.repo, "repo");
    }
}
