//! GitHub hosting provider.
//!
//! Issues a single GraphQL query that bundles the three fallback strategies:
//! 1. `byHead`: pull requests on the repo with the given `headRefName` (any state).
//! 2. `bySha`: PRs found by searching for the current HEAD SHA (catches fork PRs
//!    and PRs whose head ref no longer matches the local branch name).
//!
//! The local branch is matched against each candidate's `headRefName` with
//! [`branch_matches_pr`] (handles `fork-owner/branch` style names that fork
//! workspaces sometimes use locally), then sorted by [`pick_best`] (open >
//! draft > merged > closed; newer wins ties).

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use octocrab::Octocrab;
use serde::Deserialize;
use serde_json::json;

use crate::git::hosting::{HostingProvider, ParsedRemote, ProviderError};
use seoul_terminal_proto::pr::{ChecksStatus, PrInfo, PrState, ReviewDecision};

const GITHUB_PR_LOOKUP_QUERY: &str = r#"
query SeoulPrLookup($owner: String!, $name: String!, $branch: String!, $searchQuery: String!) {
  byHead: repository(owner: $owner, name: $name) {
    pullRequests(
      headRefName: $branch,
      states: [OPEN, MERGED, CLOSED],
      orderBy: {field: CREATED_AT, direction: DESC},
      first: 10
    ) {
      nodes { ...PrCore }
    }
  }
  bySha: search(query: $searchQuery, type: ISSUE, first: 5) {
    nodes {
      __typename
      ... on PullRequest { ...PrCore }
    }
  }
}

fragment PrCore on PullRequest {
  number
  title
  url
  state
  isDraft
  additions
  deletions
  headRefName
  isCrossRepository
  reviewDecision
  commits(last: 1) {
    nodes {
      commit {
        statusCheckRollup { state }
      }
    }
  }
}
"#;

pub struct GitHubProvider {
    octo: Arc<Octocrab>,
}

impl GitHubProvider {
    pub fn new(octo: Arc<Octocrab>) -> Self {
        Self { octo }
    }
}

#[async_trait]
impl HostingProvider for GitHubProvider {
    fn host_id(&self) -> &str {
        "github"
    }

    fn matches_host(&self, host: &str) -> bool {
        host == "github.com"
    }

    fn create_pr_web_url(&self, remote: &ParsedRemote, branch: &str) -> String {
        format!(
            "https://github.com/{}/{}/pull/new/{}",
            remote.owner,
            remote.repo,
            urlencoding::encode(branch)
        )
    }

    async fn resolve_pr_for_branch(
        &self,
        remote: &ParsedRemote,
        branch: &str,
        head_sha: &str,
    ) -> Result<Option<PrInfo>, ProviderError> {
        // Reject inputs that would build a malformed Search query before we
        // burn an HTTP round-trip on a guaranteed `query attribute` error.
        if remote.owner.is_empty()
            || remote.repo.is_empty()
            || remote.repo.contains('/')
            || head_sha.is_empty()
        {
            return Err(ProviderError::Other(format!(
                "invalid remote/head for pr lookup: owner={:?} repo={:?} head_sha_len={}",
                remote.owner,
                remote.repo,
                head_sha.len()
            )));
        }

        let search_query = format!("repo:{}/{} is:pr {}", remote.owner, remote.repo, head_sha);
        let body = json!({
            "query": GITHUB_PR_LOOKUP_QUERY,
            "variables": {
                "owner": remote.owner,
                "name": remote.repo,
                "branch": branch,
                "searchQuery": search_query,
            },
        });

        let resp: GraphqlResponse<LookupData> =
            self.octo.graphql(&body).await.map_err(map_octocrab_err)?;

        if let Some(errors) = resp.errors
            && !errors.is_empty()
        {
            tracing::warn!(
                owner = %remote.owner,
                repo = %remote.repo,
                branch = %branch,
                head_sha = %head_sha,
                search_query = %search_query,
                "github pr lookup graphql errors: {errors:?}"
            );
            return Err(ProviderError::Other(format!(
                "graphql errors: {}",
                serde_json::to_string(&errors).unwrap_or_default()
            )));
        }
        let data = resp.data.ok_or_else(|| {
            ProviderError::Other("graphql response missing data field".to_string())
        })?;

        let mut candidates: Vec<&GqlPrCore> = Vec::new();
        if let Some(repo) = &data.by_head {
            candidates.extend(repo.pull_requests.nodes.iter());
        }
        if let Some(search) = &data.by_sha {
            for n in &search.nodes {
                if let GqlSearchNode::PullRequest { core } = n {
                    candidates.push(core);
                }
            }
        }
        candidates.retain(|pr| branch_matches_pr(branch, &pr.head_ref_name));

        Ok(pick_best(&candidates).map(|pr| pr.to_pr_info("github")))
    }
}

fn branch_matches_pr(local: &str, pr_head: &str) -> bool {
    local == pr_head || local.ends_with(&format!("/{pr_head}"))
}

fn pick_best<'a>(prs: &[&'a GqlPrCore]) -> Option<&'a GqlPrCore> {
    prs.iter()
        .copied()
        .min_by_key(|pr| (state_priority(pr), -(pr.number as i64)))
}

/// Lower is better.
fn state_priority(pr: &GqlPrCore) -> u8 {
    match pr.state.as_str() {
        "OPEN" if !pr.is_draft => 0,
        "OPEN" => 1, // draft
        "MERGED" => 2,
        "CLOSED" => 3,
        _ => 4,
    }
}

fn map_octocrab_err(e: octocrab::Error) -> ProviderError {
    let s = format!("{e}");
    let lower = s.to_lowercase();
    if lower.contains("401") || lower.contains("unauthorized") || lower.contains("bad credentials")
    {
        ProviderError::NotAuthenticated
    } else if lower.contains("rate limit") || lower.contains("429") {
        ProviderError::RateLimited { reset_unix: 0 }
    } else if lower.contains("dns")
        || lower.contains("connection")
        || lower.contains("network")
        || lower.contains("timeout")
    {
        ProviderError::Network(s)
    } else {
        ProviderError::Other(s)
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ── GraphQL response mapping ──────────────────────────────────────────────

#[derive(Deserialize)]
struct GraphqlResponse<T> {
    data: Option<T>,
    #[serde(default)]
    errors: Option<Vec<serde_json::Value>>,
}

#[derive(Deserialize)]
struct LookupData {
    #[serde(rename = "byHead")]
    by_head: Option<GqlRepository>,
    #[serde(rename = "bySha")]
    by_sha: Option<GqlSearch>,
}

#[derive(Deserialize)]
struct GqlRepository {
    #[serde(rename = "pullRequests")]
    pull_requests: GqlPullRequests,
}

#[derive(Deserialize)]
struct GqlPullRequests {
    nodes: Vec<GqlPrCore>,
}

#[derive(Deserialize)]
struct GqlSearch {
    nodes: Vec<GqlSearchNode>,
}

#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum GqlSearchNode {
    PullRequest {
        #[serde(flatten)]
        core: GqlPrCore,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct GqlPrCore {
    number: u32,
    title: String,
    url: String,
    state: String,
    #[serde(rename = "isDraft")]
    is_draft: bool,
    additions: u32,
    deletions: u32,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(rename = "isCrossRepository")]
    is_cross_repository: bool,
    #[serde(rename = "reviewDecision")]
    review_decision: Option<String>,
    commits: GqlCommits,
}

#[derive(Deserialize)]
struct GqlCommits {
    nodes: Vec<GqlCommitNode>,
}

#[derive(Deserialize)]
struct GqlCommitNode {
    commit: GqlCommit,
}

#[derive(Deserialize)]
struct GqlCommit {
    #[serde(rename = "statusCheckRollup")]
    rollup: Option<GqlRollup>,
}

#[derive(Deserialize)]
struct GqlRollup {
    state: String,
}

impl GqlPrCore {
    fn to_pr_info(&self, provider_id: &str) -> PrInfo {
        PrInfo {
            provider_id: provider_id.to_string(),
            number: self.number,
            title: self.title.clone(),
            url: self.url.clone(),
            state: map_state(&self.state, self.is_draft),
            review_decision: map_review(self.review_decision.as_deref()),
            checks_status: map_checks(
                self.commits
                    .nodes
                    .first()
                    .and_then(|c| c.commit.rollup.as_ref())
                    .map(|r| r.state.as_str()),
            ),
            additions: self.additions,
            deletions: self.deletions,
            head_ref_name: self.head_ref_name.clone(),
            is_cross_repository: self.is_cross_repository,
            last_refreshed_unix: now_unix(),
        }
    }
}

fn map_state(state: &str, is_draft: bool) -> PrState {
    match state {
        "MERGED" => PrState::Merged,
        "CLOSED" => PrState::Closed,
        "OPEN" if is_draft => PrState::Draft,
        _ => PrState::Open,
    }
}

fn map_review(decision: Option<&str>) -> ReviewDecision {
    match decision {
        Some("APPROVED") => ReviewDecision::Approved,
        Some("CHANGES_REQUESTED") => ReviewDecision::ChangesRequested,
        Some("REVIEW_REQUIRED") => ReviewDecision::ReviewRequired,
        _ => ReviewDecision::None,
    }
}

fn map_checks(state: Option<&str>) -> ChecksStatus {
    match state {
        Some("SUCCESS") => ChecksStatus::Success,
        Some("FAILURE") | Some("ERROR") => ChecksStatus::Failure,
        Some("PENDING") | Some("EXPECTED") => ChecksStatus::Pending,
        _ => ChecksStatus::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_match_exact() {
        assert!(branch_matches_pr("feature/x", "feature/x"));
    }

    #[test]
    fn branch_match_fork_suffix() {
        assert!(branch_matches_pr("alice/feature/x", "feature/x"));
    }

    #[test]
    fn branch_match_no_match() {
        assert!(!branch_matches_pr("feature/y", "feature/x"));
    }

    #[test]
    fn state_open_beats_draft() {
        let open = make_core("OPEN", false, 1);
        let draft = make_core("OPEN", true, 2);
        let cands = [&draft, &open];
        let best = pick_best(&cands).unwrap();
        assert_eq!(best.number, 1);
    }

    #[test]
    fn newer_open_wins_tie() {
        let a = make_core("OPEN", false, 5);
        let b = make_core("OPEN", false, 9);
        let cands = [&a, &b];
        let best = pick_best(&cands).unwrap();
        assert_eq!(best.number, 9);
    }

    fn make_core(state: &str, is_draft: bool, number: u32) -> GqlPrCore {
        GqlPrCore {
            number,
            title: String::new(),
            url: String::new(),
            state: state.to_string(),
            is_draft,
            additions: 0,
            deletions: 0,
            head_ref_name: String::new(),
            is_cross_repository: false,
            review_decision: None,
            commits: GqlCommits { nodes: vec![] },
        }
    }
}
