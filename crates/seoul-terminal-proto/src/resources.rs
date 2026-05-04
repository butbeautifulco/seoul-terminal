use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Resource metrics for a single terminal session's process tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionResourceMetrics {
    pub session_id: Uuid,
    pub workspace_id: Uuid,
    pub foreground_process: Option<String>,
    /// CPU usage as percentage (0.0 – N*100.0 for N cores).
    pub cpu_percent: f64,
    /// Memory in bytes (phys_footprint on macOS, RSS elsewhere).
    pub memory_bytes: u64,
    /// Number of processes in this session's subtree.
    pub child_process_count: u32,
}

/// Resource metrics for the daemon process itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonMetrics {
    pub pid: u32,
    pub cpu_percent: f64,
    pub memory_bytes: u64,
}

/// System-wide resource metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemMetrics {
    pub total_memory_bytes: u64,
    pub used_memory_bytes: u64,
    pub cpu_count: u32,
    pub load_average_1m: f64,
}

/// Complete resource snapshot sent from daemon to client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceSnapshot {
    pub timestamp_secs: u64,
    pub daemon: DaemonMetrics,
    pub sessions: Vec<SessionResourceMetrics>,
    pub system: SystemMetrics,
    /// Pre-computed totals for quick badge display.
    pub total_cpu_percent: f64,
    pub total_memory_bytes: u64,
    /// Number of PIDs that could not be read (permission denied, etc.).
    pub inaccessible_pids: u32,
}
