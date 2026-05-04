//! Pull-request status types shared between the daemon (which polls GitHub) and
//! the app (which renders the sidebar badges and detail cards).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    Open,
    Draft,
    Merged,
    Closed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
    None,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChecksStatus {
    Success,
    Failure,
    Pending,
    None,
}

/// Rich PR info aggregated across hosting providers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrInfo {
    pub provider_id: String,
    pub number: u32,
    pub title: String,
    pub url: String,
    pub state: PrState,
    pub review_decision: ReviewDecision,
    pub checks_status: ChecksStatus,
    pub additions: u32,
    pub deletions: u32,
    pub head_ref_name: String,
    pub is_cross_repository: bool,
    pub last_refreshed_unix: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrUnavailableReason {
    GhNotInstalled,
    NotAuthenticated,
    RateLimited { reset_unix: i64 },
    UnsupportedHost { host: String },
    Network,
    Other { message: String },
}
