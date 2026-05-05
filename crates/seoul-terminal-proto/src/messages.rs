use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::pr::{PrInfo, PrUnavailableReason};
use crate::session::{SessionId, SessionMeta, SessionStatus};

pub const PROTOCOL_VERSION: u32 = 2;
pub const BUILD_HASH: &str = env!("SEOUL_PROTO_HASH");

// ── Client → Daemon ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloMsg {
    pub token: String,
    pub protocol_version: u32,
    pub client_id: Uuid,
    #[serde(default)]
    pub build_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateOrAttachMsg {
    pub session_id: SessionId,
    pub workspace_id: Uuid,
    pub cols: u16,
    pub rows: u16,
    pub cwd: Option<PathBuf>,
    pub shell: Option<String>,
    #[serde(default)]
    pub scrollback_limit_bytes: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsureSessionMsg {
    pub session_id: SessionId,
    pub workspace_id: Uuid,
    pub cols: u16,
    pub rows: u16,
    pub cwd: Option<PathBuf>,
    pub shell: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteMsg {
    pub session_id: SessionId,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResizeMsg {
    pub session_id: SessionId,
    pub cols: u16,
    pub rows: u16,
    #[serde(default)]
    pub pixel_width: u32,
    #[serde(default)]
    pub pixel_height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KillMsg {
    pub session_id: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetachMsg {
    pub session_id: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalMsg {
    pub session_id: SessionId,
    pub signal: i32,
}

// ── Daemon → Client ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloAckMsg {
    pub protocol_version: u32,
    pub daemon_pid: u32,
    #[serde(default)]
    pub build_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionAttachedMsg {
    pub session_id: SessionId,
    pub is_new: bool,
    pub was_recovered: bool,
    pub scrollback_data: Vec<u8>,
    pub cols: u16,
    pub rows: u16,
    pub cwd: Option<String>,
    pub foreground_process: Option<String>,
    /// ANSI escape sequences to restore terminal modes on warm attach.
    #[serde(default)]
    pub rehydrate_sequences: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEnsuredMsg {
    pub session_id: SessionId,
    pub is_new: bool,
    pub was_recovered: bool,
    pub cols: u16,
    pub rows: u16,
    pub cwd: Option<String>,
    pub foreground_process: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataMsg {
    pub session_id: SessionId,
    pub data: bytes::Bytes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitMsg {
    pub session_id: SessionId,
    pub status: SessionStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorMsg {
    pub message: String,
    pub session_id: Option<SessionId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionListMsg {
    pub sessions: Vec<SessionMeta>,
}

// ── Resource Monitoring ─────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetResourceSnapshotMsg {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResizeAckMsg {
    pub session_id: SessionId,
    pub cols: u16,
    pub rows: u16,
}

pub type ResourceSnapshotMsg = crate::resources::ResourceSnapshot;

// ── Workspace registration & PR status (Client → Daemon) ────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRegisteredMsg {
    pub workspace_id: Uuid,
    pub working_dir: PathBuf,
    pub branch: String,
    pub default_branch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceUnregisteredMsg {
    pub workspace_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceFocusedMsg {
    /// `None` = blur (no active workspace).
    pub workspace_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshPrMsg {
    pub workspace_id: Uuid,
}

// ── PR status (Daemon → Client) ────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrStatusUpdatedMsg {
    pub workspace_id: Uuid,
    /// `None` = no PR found for this workspace's branch.
    pub pr_info: Option<PrInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrStatusUnavailableMsg {
    pub workspace_id: Uuid,
    pub reason: PrUnavailableReason,
}
