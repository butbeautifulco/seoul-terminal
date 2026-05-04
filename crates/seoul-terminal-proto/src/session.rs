use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type SessionId = Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionStatus {
    Spawning,
    Alive,
    Exited { code: i32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub session_id: SessionId,
    pub workspace_id: Uuid,
    pub cwd: PathBuf,
    pub cols: u16,
    pub rows: u16,
    pub shell: String,
    pub created_at: String,
    pub last_attached_at: String,
    pub foreground_process: Option<String>,
    pub status: SessionStatus,
}
