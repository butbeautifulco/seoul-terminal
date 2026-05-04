//! Daemon-side PR status poller.
//!
//! Owns one tokio task that polls each registered workspace through its
//! [`HostingProvider`]. Active (focused) workspaces poll every 10 seconds,
//! others every 120 seconds. Results are broadcast to every connected client
//! through `broadcast_tx`; the per-client writer task in `server.rs` forwards
//! them as `PrStatusUpdated` / `PrStatusUnavailable` frames.
//!
//! Commands (register/unregister/focus/refresh) flow in over an `mpsc` so the
//! poller can react immediately to UI events without waiting for the next tick.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use octocrab::Octocrab;
use tokio::sync::{broadcast, mpsc};
use tokio::task;
use tracing::{debug, warn};
use uuid::Uuid;

use seoul_terminal_proto::messages::{PrStatusUnavailableMsg, PrStatusUpdatedMsg};
use seoul_terminal_proto::pr::{PrInfo, PrUnavailableReason};
use seoul_workspace::git::hosting::{HostingProvider, HostingRegistry, ParsedRemote};
use seoul_workspace::git::runner::GitCommandRunner;

use crate::session::ClientEvent;

const TICK_INTERVAL: Duration = Duration::from_secs(5);
const ACTIVE_POLL_INTERVAL: Duration = Duration::from_secs(10);
const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(120);
const ERROR_BACKOFF: Duration = Duration::from_secs(300);

#[derive(Debug)]
pub enum PollerCmd {
    Register {
        workspace_id: Uuid,
        working_dir: PathBuf,
        branch: String,
    },
    Unregister(Uuid),
    Focus(Option<Uuid>),
    Refresh(Uuid),
}

#[derive(Clone)]
pub struct PollerHandle {
    tx: mpsc::Sender<PollerCmd>,
}

impl PollerHandle {
    pub fn register(&self, workspace_id: Uuid, working_dir: PathBuf, branch: String) {
        let _ = self.tx.try_send(PollerCmd::Register {
            workspace_id,
            working_dir,
            branch,
        });
    }
    pub fn unregister(&self, workspace_id: Uuid) {
        let _ = self.tx.try_send(PollerCmd::Unregister(workspace_id));
    }
    pub fn focus(&self, workspace_id: Option<Uuid>) {
        let _ = self.tx.try_send(PollerCmd::Focus(workspace_id));
    }
    pub fn refresh(&self, workspace_id: Uuid) {
        let _ = self.tx.try_send(PollerCmd::Refresh(workspace_id));
    }
}

struct WorkspaceState {
    working_dir: PathBuf,
    branch: String,
    parsed_remote: Option<ParsedRemote>,
    provider: Option<Arc<dyn HostingProvider>>,
    last_pr: Option<PrInfo>,
    last_polled_at: Option<Instant>,
    backoff_until: Option<Instant>,
    /// Whether we've already broadcast an `Unavailable` reason. Avoids
    /// hammering the same toast every 5 minutes.
    last_unavailable_kind: Option<UnavailableKind>,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum UnavailableKind {
    Unsupported,
    NotAuthenticated,
    GhNotInstalled,
    RateLimited,
    Network,
    Other,
}

impl WorkspaceState {
    fn new(working_dir: PathBuf, branch: String) -> Self {
        Self {
            working_dir,
            branch,
            parsed_remote: None,
            provider: None,
            last_pr: None,
            last_polled_at: None,
            backoff_until: None,
            last_unavailable_kind: None,
        }
    }
}

pub fn spawn(
    octo: Arc<Octocrab>,
    registry: Arc<HostingRegistry>,
    broadcast_tx: broadcast::Sender<ClientEvent>,
) -> PollerHandle {
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<PollerCmd>(64);

    tokio::spawn(async move {
        let mut state: HashMap<Uuid, WorkspaceState> = HashMap::new();
        let mut active: Option<Uuid> = None;
        let mut tick = tokio::time::interval(TICK_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                biased;
                maybe_cmd = cmd_rx.recv() => {
                    let Some(cmd) = maybe_cmd else { break };
                    handle_cmd(cmd, &mut state, &mut active, &registry).await;
                }
                _ = tick.tick() => {
                    poll_due(&mut state, active, &octo, &broadcast_tx).await;
                }
            }
        }
        debug!("pr_poller task exited");
    });

    PollerHandle { tx: cmd_tx }
}

async fn handle_cmd(
    cmd: PollerCmd,
    state: &mut HashMap<Uuid, WorkspaceState>,
    active: &mut Option<Uuid>,
    registry: &HostingRegistry,
) {
    match cmd {
        PollerCmd::Register {
            workspace_id,
            working_dir,
            branch,
        } => {
            let mut ws = WorkspaceState::new(working_dir.clone(), branch);
            // Resolve origin → provider on registration; cheap blocking call.
            let working_dir_clone = working_dir.clone();
            let origin = task::spawn_blocking(move || read_origin_url(&working_dir_clone))
                .await
                .ok()
                .and_then(|r| r.ok());
            if let Some(origin) = origin {
                if let Some((provider, parsed)) = registry.provider_for_remote(&origin) {
                    ws.provider = Some(provider);
                    ws.parsed_remote = Some(parsed);
                } else {
                    debug!(?workspace_id, %origin, "no hosting provider for origin");
                }
            }
            state.insert(workspace_id, ws);
        }
        PollerCmd::Unregister(id) => {
            state.remove(&id);
            if *active == Some(id) {
                *active = None;
            }
        }
        PollerCmd::Focus(id) => {
            *active = id;
            // Force the newly-focused workspace to poll on the next tick.
            if let Some(id) = id
                && let Some(ws) = state.get_mut(&id)
            {
                ws.last_polled_at = None;
                ws.backoff_until = None;
            }
        }
        PollerCmd::Refresh(id) => {
            if let Some(ws) = state.get_mut(&id) {
                ws.last_polled_at = None;
                ws.backoff_until = None;
            }
        }
    }
}

async fn poll_due(
    state: &mut HashMap<Uuid, WorkspaceState>,
    active: Option<Uuid>,
    octo: &Arc<Octocrab>,
    broadcast_tx: &broadcast::Sender<ClientEvent>,
) {
    let now = Instant::now();
    let due_ids: Vec<Uuid> = state
        .iter()
        .filter_map(|(id, ws)| {
            if ws.backoff_until.map(|t| now < t).unwrap_or(false) {
                return None;
            }
            let interval = if Some(*id) == active {
                ACTIVE_POLL_INTERVAL
            } else {
                IDLE_POLL_INTERVAL
            };
            match ws.last_polled_at {
                None => Some(*id),
                Some(t) if now.duration_since(t) >= interval => Some(*id),
                _ => None,
            }
        })
        .collect();

    for id in due_ids {
        poll_one(id, state, octo, broadcast_tx).await;
    }
}

async fn poll_one(
    workspace_id: Uuid,
    state: &mut HashMap<Uuid, WorkspaceState>,
    _octo: &Arc<Octocrab>,
    broadcast_tx: &broadcast::Sender<ClientEvent>,
) {
    let Some(ws) = state.get_mut(&workspace_id) else {
        return;
    };
    ws.last_polled_at = Some(Instant::now());

    let Some(provider) = ws.provider.clone() else {
        // No supported provider — emit Unsupported once.
        let host = ws
            .parsed_remote
            .as_ref()
            .map(|r| r.host.clone())
            .unwrap_or_else(|| "unknown".to_string());
        if ws.last_unavailable_kind != Some(UnavailableKind::Unsupported) {
            ws.last_unavailable_kind = Some(UnavailableKind::Unsupported);
            send_unavailable(
                broadcast_tx,
                workspace_id,
                PrUnavailableReason::UnsupportedHost { host },
            );
        }
        // Long backoff so we don't keep re-checking origin every tick.
        ws.backoff_until = Some(Instant::now() + Duration::from_secs(3600));
        return;
    };
    let Some(remote) = ws.parsed_remote.clone() else {
        return;
    };
    let working_dir = ws.working_dir.clone();
    let branch = ws.branch.clone();

    let head_sha_res = task::spawn_blocking(move || read_head_sha(&working_dir)).await;
    let head_sha = match head_sha_res {
        Ok(Ok(sha)) => sha,
        Ok(Err(e)) => {
            warn!(?workspace_id, "failed to read HEAD sha: {e}");
            return;
        }
        Err(e) => {
            warn!(?workspace_id, "spawn_blocking join error: {e}");
            return;
        }
    };

    match provider
        .resolve_pr_for_branch(&remote, &branch, &head_sha)
        .await
    {
        Ok(pr) => {
            // Reset error tracking on success.
            ws.last_unavailable_kind = None;
            ws.backoff_until = None;
            if pr != ws.last_pr {
                ws.last_pr = pr.clone();
                let _ = broadcast_tx.send(ClientEvent::PrStatusUpdated(PrStatusUpdatedMsg {
                    workspace_id,
                    pr_info: pr,
                }));
            }
        }
        Err(err) => {
            let reason = err.into_unavailable();
            let kind = classify(&reason);
            ws.backoff_until = Some(Instant::now() + ERROR_BACKOFF);
            // Only broadcast when the error class changes — otherwise we'd
            // toast the user every 5 minutes for the same condition.
            if ws.last_unavailable_kind != Some(kind) {
                ws.last_unavailable_kind = Some(kind);
                send_unavailable(broadcast_tx, workspace_id, reason);
            }
        }
    }
}

fn classify(reason: &PrUnavailableReason) -> UnavailableKind {
    match reason {
        PrUnavailableReason::GhNotInstalled => UnavailableKind::GhNotInstalled,
        PrUnavailableReason::NotAuthenticated => UnavailableKind::NotAuthenticated,
        PrUnavailableReason::RateLimited { .. } => UnavailableKind::RateLimited,
        PrUnavailableReason::UnsupportedHost { .. } => UnavailableKind::Unsupported,
        PrUnavailableReason::Network => UnavailableKind::Network,
        PrUnavailableReason::Other { .. } => UnavailableKind::Other,
    }
}

fn send_unavailable(
    broadcast_tx: &broadcast::Sender<ClientEvent>,
    workspace_id: Uuid,
    reason: PrUnavailableReason,
) {
    let _ = broadcast_tx.send(ClientEvent::PrStatusUnavailable(PrStatusUnavailableMsg {
        workspace_id,
        reason,
    }));
}

fn read_origin_url(working_dir: &std::path::Path) -> anyhow::Result<String> {
    GitCommandRunner::new(working_dir).run(&["remote", "get-url", "origin"])
}

fn read_head_sha(working_dir: &std::path::Path) -> anyhow::Result<String> {
    GitCommandRunner::new(working_dir).run(&["rev-parse", "HEAD"])
}
