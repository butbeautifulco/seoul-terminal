use std::collections::HashMap;

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::{debug, info};
use uuid::Uuid;

use seoul_terminal_proto::messages::*;
use seoul_terminal_proto::session::{SessionId, SessionStatus};

use crate::resource_monitor::{ResourceMonitor, SessionInfo};
use crate::session::{self, ClientEvent, ClientHandle, DaemonSession, SessionWriteHandle};

/// Result of the fast (lock-held) phase of create_or_attach.
pub enum AttachResult {
    /// Warm attach succeeded — scrollback from in-memory buffer, no disk I/O.
    Attached {
        attached: SessionAttachedMsg,
        write_handle: SessionWriteHandle,
    },
    /// Session needs to be spawned — disk I/O and PTY creation should happen outside the lock.
    NeedsSpawn {
        msg: CreateOrAttachMsg,
        client: ClientHandle,
    },
}

/// Result of the fast phase of ensure_session.
pub enum EnsureResult {
    /// Session is alive and ready to attach later.
    Ensured(SessionEnsuredMsg),
    /// Session must be spawned without attaching the client.
    NeedsSpawn { msg: EnsureSessionMsg },
}

/// Central manager of all terminal sessions.
pub struct TerminalHost {
    sessions: HashMap<SessionId, DaemonSession>,
    /// Channel for PTY reader tasks to send data back to the host.
    broadcast_tx: mpsc::Sender<(SessionId, ClientEvent)>,
    broadcast_rx: mpsc::Receiver<(SessionId, ClientEvent)>,
    resource_monitor: ResourceMonitor,
}

impl TerminalHost {
    pub fn new() -> Self {
        let (broadcast_tx, broadcast_rx) = mpsc::channel(session::BROADCAST_CHANNEL_CAPACITY);
        Self {
            sessions: HashMap::new(),
            broadcast_tx,
            broadcast_rx,
            resource_monitor: ResourceMonitor::new(),
        }
    }

    /// Fast path: attach to an existing alive session (no disk I/O, no PTY spawn).
    /// If the session doesn't exist or has exited, returns NeedsSpawn for the caller
    /// to handle outside the lock.
    pub fn create_or_attach_fast(
        &mut self,
        msg: CreateOrAttachMsg,
        client: ClientHandle,
    ) -> Result<AttachResult> {
        if let Some(session) = self.sessions.get_mut(&msg.session_id) {
            if matches!(session.status(), SessionStatus::Alive) {
                // Warm attach: in-memory scrollback, cached process info, no I/O
                info!(session_id = %msg.session_id, "attaching to existing session");
                let write_handle = session.write_handle();
                let attached = session.attach(client, msg.scrollback_limit_bytes);
                return Ok(AttachResult::Attached {
                    attached,
                    write_handle,
                });
            }
            // Session has exited — remove it; caller will spawn new
            debug!(session_id = %msg.session_id, "session exited, creating new");
            self.sessions.remove(&msg.session_id);
        }
        Ok(AttachResult::NeedsSpawn { msg, client })
    }

    /// Fast path: make sure a session exists without attaching this client.
    pub fn ensure_session_fast(&mut self, msg: EnsureSessionMsg) -> Result<EnsureResult> {
        if let Some(session) = self.sessions.get_mut(&msg.session_id) {
            if matches!(session.status(), SessionStatus::Alive) {
                let ensured = SessionEnsuredMsg {
                    session_id: session.id,
                    is_new: false,
                    was_recovered: false,
                    cols: session.meta.cols,
                    rows: session.meta.rows,
                    cwd: Some(session.meta.cwd.to_string_lossy().into_owned()),
                    foreground_process: session.meta.foreground_process.clone(),
                };
                return Ok(EnsureResult::Ensured(ensured));
            }
            debug!(session_id = %msg.session_id, "session exited, ensuring new");
            self.sessions.remove(&msg.session_id);
        }
        Ok(EnsureResult::NeedsSpawn { msg })
    }

    /// Slow path: spawn a new session. Called with the host lock held, but
    /// cold restore disk I/O (scrollback read, metadata) should be done by the
    /// caller BEFORE acquiring the lock.
    pub fn spawn_and_attach(
        &mut self,
        msg: CreateOrAttachMsg,
        client: ClientHandle,
        scrollback_data: Vec<u8>,
        was_recovered: bool,
    ) -> Result<SessionAttachedMsg> {
        let mut session = DaemonSession::spawn(
            msg.session_id,
            msg.workspace_id,
            msg.cols,
            msg.rows,
            msg.cwd,
            msg.shell,
            self.broadcast_tx.clone(),
        )?;

        // Cold restore: the client's ghostty will generate fresh terminal query
        // responses (DA1, DSR) for the new shell — don't drop them as stale.
        if was_recovered {
            session.disable_stale_response_filter();
        }

        let mut attached_msg = session.attach(client, msg.scrollback_limit_bytes);
        attached_msg.is_new = true;
        attached_msg.was_recovered = was_recovered;
        attached_msg.scrollback_data = scrollback_data;

        self.sessions.insert(msg.session_id, session);
        self.resource_monitor.invalidate_cache();
        Ok(attached_msg)
    }

    /// Spawn a session without attaching a client or sending scrollback.
    pub fn spawn_unattached(
        &mut self,
        msg: EnsureSessionMsg,
        was_recovered: bool,
    ) -> Result<SessionEnsuredMsg> {
        let session = DaemonSession::spawn(
            msg.session_id,
            msg.workspace_id,
            msg.cols,
            msg.rows,
            msg.cwd,
            msg.shell,
            self.broadcast_tx.clone(),
        )?;

        let ensured = SessionEnsuredMsg {
            session_id: session.id,
            is_new: true,
            was_recovered,
            cols: session.meta.cols,
            rows: session.meta.rows,
            cwd: Some(session.meta.cwd.to_string_lossy().into_owned()),
            foreground_process: session.meta.foreground_process.clone(),
        };

        self.sessions.insert(msg.session_id, session);
        self.resource_monitor.invalidate_cache();
        Ok(ensured)
    }

    /// Get a reference to a session by ID.
    pub fn get_session(&self, session_id: SessionId) -> Option<&DaemonSession> {
        self.sessions.get(&session_id)
    }

    /// Get a mutable reference to a session by ID.
    pub fn get_session_mut(&mut self, session_id: SessionId) -> Option<&mut DaemonSession> {
        self.sessions.get_mut(&session_id)
    }

    /// Resize a session's PTY.
    pub fn resize(&mut self, msg: ResizeMsg) -> Result<()> {
        let session = self
            .sessions
            .get_mut(&msg.session_id)
            .ok_or_else(|| anyhow::anyhow!("session not found: {}", msg.session_id))?;
        session.resize(msg.cols, msg.rows, msg.pixel_width, msg.pixel_height)
    }

    /// Detach a client from a session. PTY stays alive.
    pub fn detach(&mut self, msg: DetachMsg, client_id: Uuid) {
        if let Some(session) = self.sessions.get_mut(&msg.session_id) {
            session.detach(client_id);
        }
    }

    /// Start kill escalation for a session (SIGTERM → SIGKILL → force exit).
    /// The session stays in the map until the exit event arrives via broadcast.
    pub fn kill(&mut self, msg: KillMsg) {
        if let Some(session) = self.sessions.get_mut(&msg.session_id) {
            session.start_kill();
            session.flush_scrollback();
        }
    }

    /// Send a signal to a session's child process group.
    pub fn signal(&mut self, msg: SignalMsg) {
        if let Some(session) = self.sessions.get(&msg.session_id) {
            #[cfg(unix)]
            crate::session::send_signal_to_group(session.child_pid(), msg.signal);
        }
    }

    /// List all sessions.
    pub fn list_sessions(&self) -> SessionListMsg {
        SessionListMsg {
            sessions: self.sessions.values().map(|s| s.meta.clone()).collect(),
        }
    }

    /// Collect resource usage snapshot for all alive sessions.
    pub fn get_resource_snapshot(&mut self) -> seoul_terminal_proto::resources::ResourceSnapshot {
        let session_infos: Vec<SessionInfo> = self
            .sessions
            .iter()
            .filter(|(_, s)| matches!(s.status(), SessionStatus::Alive))
            .map(|(_, s)| SessionInfo {
                session_id: s.meta.session_id,
                workspace_id: s.meta.workspace_id,
                child_pid: s.child_pid(),
                foreground_process: s.meta.foreground_process.clone(),
            })
            .collect();

        self.resource_monitor.snapshot(&session_infos)
    }

    /// Check shell readiness timeouts for all pending sessions.
    pub fn check_readiness_timeouts(&mut self) {
        for session in self.sessions.values_mut() {
            if matches!(session.status(), SessionStatus::Alive) {
                session.check_readiness_timeout();
            }
        }
    }

    /// Process pending PTY output events. Call this in the main loop.
    pub fn drain_broadcast(&mut self) {
        while let Ok((session_id, event)) = self.broadcast_rx.try_recv() {
            match event {
                ClientEvent::Data(data_msg) => {
                    if let Some(session) = self.sessions.get_mut(&session_id) {
                        session.on_pty_data(data_msg.data);
                    }
                }
                ClientEvent::Exit(exit_msg) => {
                    if let Some(session) = self.sessions.get_mut(&session_id) {
                        let code = match exit_msg.status {
                            SessionStatus::Exited { code } => code,
                            _ => 0,
                        };
                        session.on_exit(code);
                    }
                }
                // PR status events flow directly from PrPoller → client writers
                // and don't pass through the host's broadcast channel; if one
                // ends up here it's a routing bug — drop it silently.
                ClientEvent::PrStatusUpdated(_) | ClientEvent::PrStatusUnavailable(_) => {}
            }
        }
    }

    /// Refresh process info for all alive sessions. Call periodically.
    pub fn refresh_all_process_info(&mut self) {
        for session in self.sessions.values_mut() {
            if matches!(session.status(), SessionStatus::Alive) {
                session.refresh_process_info();
            }
        }
    }

    /// Remove exited sessions.
    pub fn cleanup_exited_sessions(&mut self) {
        self.sessions
            .retain(|_, s| !matches!(s.status(), SessionStatus::Exited { .. }));
    }

    /// Graceful shutdown: flush all scrollback to disk and force-kill all sessions.
    /// Uses immediate SIGKILL (no escalation) since the daemon is exiting.
    pub fn graceful_shutdown(&mut self) {
        // Drain any pending broadcast events first
        self.drain_broadcast();

        for (id, session) in &mut self.sessions {
            info!(session_id = %id, "shutting down session");
            session.flush_scrollback();
            session.force_kill();
        }
    }

    /// Clean up old scrollback files (older than 7 days).
    pub fn cleanup_old_scrollback(&self) {
        let history_dir = seoul_terminal_proto::paths::terminal_history_dir();
        if !history_dir.exists() {
            return;
        }

        let seven_days = std::time::Duration::from_secs(7 * 24 * 3600);
        let cutoff = std::time::SystemTime::now() - seven_days;

        if let Ok(entries) = std::fs::read_dir(&history_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }

                // Skip active sessions
                let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if let Ok(session_id) = dir_name.parse::<uuid::Uuid>()
                    && self.sessions.contains_key(&session_id)
                {
                    continue;
                }

                // Check modification time
                if let Ok(metadata) = std::fs::metadata(&path)
                    && let Ok(modified) = metadata.modified()
                    && modified < cutoff
                {
                    info!(path = %path.display(), "removing old scrollback");
                    std::fs::remove_dir_all(&path).ok();
                }
            }
        }
    }
}

pub fn load_saved_meta(
    session_id: SessionId,
) -> Option<seoul_terminal_proto::session::SessionMeta> {
    let meta_path = seoul_terminal_proto::paths::meta_path(session_id);
    let contents = std::fs::read_to_string(meta_path).ok()?;
    serde_json::from_str(&contents).ok()
}
