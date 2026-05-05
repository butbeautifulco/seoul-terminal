use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use seoul_terminal_proto::messages::{
    DataMsg, ExitMsg, PrStatusUnavailableMsg, PrStatusUpdatedMsg, SessionAttachedMsg,
};
use seoul_terminal_proto::paths;
use seoul_terminal_proto::session::{SessionId, SessionMeta, SessionStatus};

use crate::mode_tracker::ModeTracker;
use crate::process_info;
use crate::scrollback::ScrollbackWriter;
use crate::shell_readiness::{ReadyState, ShellReadinessTracker};
use crate::transient_errors::{self, TransientErrorWindow};

/// Backpressure channel capacities.
///
/// `SESSION_EVENT_CHANNEL_CAPACITY` is per-session: each `DaemonSession`
/// owns its own bounded queue from its PTY reader/kill task back to the
/// host's drain. With one channel per session, a slow consumer for session
/// A no longer blocks session B's PTY reader on `blocking_send` (the
/// previous "BROADCAST_CHANNEL_CAPACITY" hop was shared and could
/// head-of-line-block all sessions on a single backed-up draw cycle).
pub const SESSION_EVENT_CHANNEL_CAPACITY: usize = 256;
const CLIENT_EVENT_CHANNEL_CAPACITY: usize = 128;
const PTY_WRITER_CHANNEL_CAPACITY: usize = 2048;
/// Consecutive drain cycles where data is dropped before disconnecting a stalled client.
const CLIENT_STALL_DISCONNECT_THRESHOLD: u32 = 10;

/// Handle for sending frames to a connected client.
pub struct ClientHandle {
    pub id: Uuid,
    pub tx: mpsc::Sender<ClientEvent>,
    consecutive_drops: u32,
}

impl ClientHandle {
    pub fn new(id: Uuid, tx: mpsc::Sender<ClientEvent>) -> Self {
        Self {
            id,
            tx,
            consecutive_drops: 0,
        }
    }
}

/// Create a bounded per-client event channel.
pub fn client_event_channel() -> (mpsc::Sender<ClientEvent>, mpsc::Receiver<ClientEvent>) {
    mpsc::channel(CLIENT_EVENT_CHANNEL_CAPACITY)
}

/// Events sent from a session or other daemon-side worker to attached clients.
#[derive(Debug, Clone)]
pub enum ClientEvent {
    Data(DataMsg),
    Exit(ExitMsg),
    PrStatusUpdated(PrStatusUpdatedMsg),
    PrStatusUnavailable(PrStatusUnavailableMsg),
}

/// Handle for sending write data to a session without host lock.
/// When `shell_ready` is false, callers should fall back to the host-locked path
/// so that input is buffered by the readiness tracker.
#[derive(Clone)]
pub struct SessionWriteHandle {
    #[allow(dead_code)]
    pub session_id: SessionId,
    pub tx: mpsc::Sender<Vec<u8>>,
    pub shell_ready: Arc<AtomicBool>,
}

/// Kill escalation constants.
const KILL_ESCALATION_POLL_INTERVAL: Duration = Duration::from_millis(50);
const KILL_SIGTERM_TIMEOUT_POLLS: u32 = 40; // 2 seconds
const KILL_SIGKILL_TIMEOUT_POLLS: u32 = 20; // 1 second

/// Send a signal to a process group, with safe pid_t conversion.
/// Returns false if the pid overflows i32 (should never happen on macOS/Linux
/// with default pid_max, but guarding against undefined behavior).
#[cfg(unix)]
pub(crate) fn send_signal_to_group(pid: u32, signal: libc::c_int) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        tracing::error!(pid, "pid overflows pid_t, cannot send signal");
        return false;
    };
    unsafe { libc::killpg(pid, signal) == 0 }
}

/// A daemon-owned terminal session with its PTY.
pub struct DaemonSession {
    pub id: SessionId,
    pub meta: SessionMeta,
    /// Channel sender for PTY write data (lock-free path from client_read_loop)
    write_tx: mpsc::Sender<Vec<u8>>,
    #[allow(dead_code)]
    pty_master: Box<dyn MasterPty + Send>,
    child_pid: u32,
    attached_clients: Vec<ClientHandle>,
    scrollback_writer: ScrollbackWriter,
    status: SessionStatus,
    /// Shell readiness tracker for input gating during shell initialization.
    readiness: ShellReadinessTracker,
    /// Shared flag for lock-free write handles to check shell readiness.
    shell_ready: Arc<AtomicBool>,
    /// Terminal mode tracker for DECSET/DECRST and OSC-7 CWD.
    mode_tracker: ModeTracker,
    /// Per-session event sender — cloned into the PTY reader and kill
    /// escalation tasks. Receivers are drained by the host (`event_rx`).
    /// One channel per session means a backed-up consumer for session A
    /// cannot block session B's PTY reader on `blocking_send`.
    event_tx: mpsc::Sender<ClientEvent>,
    /// Per-session event receiver, drained by `TerminalHost::drain_broadcast`.
    pub(crate) event_rx: mpsc::Receiver<ClientEvent>,
    /// Tokio task handle for the PTY reader loop
    _reader_task: tokio::task::JoinHandle<()>,
    /// Tokio task handle for the PTY writer loop
    _writer_task: tokio::task::JoinHandle<()>,
    /// Kill escalation task handle (if kill is in progress).
    _kill_task: Option<tokio::task::JoinHandle<()>>,
}

impl DaemonSession {
    /// Spawn a new PTY session.
    pub fn spawn(
        id: SessionId,
        workspace_id: Uuid,
        cols: u16,
        rows: u16,
        cwd: Option<std::path::PathBuf>,
        shell: Option<String>,
    ) -> Result<Self> {
        let pty_system = native_pty_system();
        let pty_size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = pty_system.openpty(pty_size).context("failed to open PTY")?;

        let shell_path = shell
            .unwrap_or_else(|| std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string()));

        let mut cmd = CommandBuilder::new(&shell_path);
        cmd.arg("-l");
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "seoul");

        let effective_cwd = cwd
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/")));
        cmd.cwd(&effective_cwd);

        let child = pair
            .slave
            .spawn_command(cmd)
            .context("failed to spawn shell")?;

        let child_pid = child.process_id().unwrap_or(0);

        let writer = pair
            .master
            .take_writer()
            .context("failed to take PTY writer")?;
        let reader = pair
            .master
            .try_clone_reader()
            .context("failed to clone PTY reader")?;

        let scrollback_writer =
            ScrollbackWriter::new(id).context("failed to create scrollback writer")?;

        let now = epoch_secs_string();
        let meta = SessionMeta {
            session_id: id,
            workspace_id,
            cwd: effective_cwd,
            cols,
            rows,
            shell: shell_path,
            created_at: now.clone(),
            last_attached_at: now,
            foreground_process: None,
            status: SessionStatus::Alive,
        };

        let readiness = ShellReadinessTracker::new(&meta.shell);
        let shell_ready = Arc::new(AtomicBool::new(readiness.state() != ReadyState::Pending));

        // Save initial metadata
        save_meta(&meta);

        // Per-session event channel. Producers: PTY reader, kill escalation
        // task. Consumer: `TerminalHost::drain_broadcast`. Each session has
        // its own channel so a backed-up drain for one session can't block
        // another session's PTY reader on `blocking_send`.
        let (event_tx, event_rx) = mpsc::channel::<ClientEvent>(SESSION_EVENT_CHANNEL_CAPACITY);

        // Spawn PTY reader task
        let reader_event_tx = event_tx.clone();
        let session_id_for_reader = id;
        let reader_task = tokio::task::spawn_blocking(move || {
            pty_reader_loop(session_id_for_reader, reader, reader_event_tx);
        });

        // Spawn per-session PTY writer task (owns pty_writer, receives data via channel)
        let (write_tx, mut write_rx) = mpsc::channel::<Vec<u8>>(PTY_WRITER_CHANNEL_CAPACITY);
        let writer_session_id = id;
        let writer_task = tokio::task::spawn_blocking(move || {
            pty_writer_loop(writer_session_id, writer, &mut write_rx);
        });

        info!(session_id = %id, pid = child_pid, "session spawned");

        Ok(Self {
            id,
            meta,
            write_tx,
            pty_master: pair.master,
            child_pid,
            attached_clients: Vec::new(),
            event_tx,
            event_rx,
            scrollback_writer,
            readiness,
            shell_ready,
            mode_tracker: ModeTracker::new(),
            status: SessionStatus::Alive,
            _reader_task: reader_task,
            _writer_task: writer_task,
            _kill_task: None,
        })
    }

    pub fn status(&self) -> SessionStatus {
        self.status
    }

    pub fn child_pid(&self) -> u32 {
        self.child_pid
    }

    /// Flush scrollback data to disk.
    pub fn flush_scrollback(&mut self) {
        if let Err(e) = self.scrollback_writer.flush() {
            warn!(session_id = %self.id, "scrollback flush failed: {e}");
        }
        save_meta(&self.meta);
    }

    /// Disable stale response filtering in the readiness tracker.
    /// For cold restore, the client's ghostty generates fresh responses to the
    /// new shell's queries — these should not be dropped as stale.
    pub fn disable_stale_response_filter(&mut self) {
        self.readiness.disable_stale_response_filter();
    }

    /// Attach a client to receive data from this session.
    /// Auto-detaches any previously attached client with the same ID.
    /// Uses in-memory ring buffer for scrollback (no disk I/O) and cached process info.
    pub fn attach(
        &mut self,
        client: ClientHandle,
        scrollback_limit_bytes: Option<usize>,
    ) -> SessionAttachedMsg {
        // Auto-detach previous clients (single client per session policy)
        if !self.attached_clients.is_empty() {
            debug!(session_id = %self.id, "auto-detaching {} previous client(s)", self.attached_clients.len());
            self.attached_clients.clear();
        }
        self.meta.last_attached_at = epoch_secs_string();
        self.attached_clients.push(client);

        // Use in-memory ring buffer instead of disk read (warm attach optimization)
        self.scrollback_writer.flush().ok();
        let scrollback = self.scrollback_writer.get_recent_scrollback();
        let scrollback_data = match scrollback_limit_bytes {
            Some(0) => Vec::new(),
            Some(limit) if scrollback.len() > limit => {
                scrollback[scrollback.len() - limit..].to_vec()
            }
            _ => scrollback.to_vec(),
        };

        let rehydrate_sequences = self.mode_tracker.generate_rehydrate_sequences();

        SessionAttachedMsg {
            session_id: self.id,
            is_new: false,
            was_recovered: false,
            scrollback_data,
            cols: self.meta.cols,
            rows: self.meta.rows,
            cwd: Some(self.meta.cwd.to_string_lossy().into_owned()),
            // Use cached process info instead of syscall (refreshed every 5s)
            foreground_process: self.meta.foreground_process.clone(),
            rehydrate_sequences,
        }
    }

    /// Detach a client. PTY remains alive.
    pub fn detach(&mut self, client_id: Uuid) {
        self.attached_clients.retain(|c| c.id != client_id);
        debug!(session_id = %self.id, client_id = %client_id, "client detached");
    }

    /// Write input data to the PTY via the writer task (lock-free from caller's perspective).
    /// During shell initialization, input is buffered by the readiness tracker.
    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        if let Some(data) = self.readiness.buffer_input(data.to_vec()) {
            self.write_tx.try_send(data).map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => anyhow::anyhow!("PTY write queue full"),
                mpsc::error::TrySendError::Closed(_) => {
                    anyhow::anyhow!("PTY writer task closed")
                }
            })?;
        }
        Ok(())
    }

    /// Get a write handle for lock-free writes from client_read_loop.
    pub fn write_handle(&self) -> SessionWriteHandle {
        SessionWriteHandle {
            session_id: self.id,
            tx: self.write_tx.clone(),
            shell_ready: self.shell_ready.clone(),
        }
    }

    /// Resize the PTY.
    pub fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        pixel_width: u32,
        pixel_height: u32,
    ) -> Result<()> {
        self.pty_master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: pixel_width.min(u16::MAX as u32) as u16,
                pixel_height: pixel_height.min(u16::MAX as u32) as u16,
            })
            .context("failed to resize PTY")?;
        self.meta.cols = cols;
        self.meta.rows = rows;
        Ok(())
    }

    /// Handle PTY output data: write to scrollback and broadcast to attached clients.
    pub fn on_pty_data(&mut self, data: bytes::Bytes) {
        // Fast path: skip marker scanning when shell is already ready
        let forward: bytes::Bytes = if self.readiness.is_active() {
            let scan = self.readiness.scan_output(&data);
            if scan.became_ready {
                debug!(session_id = %self.id, "shell ready detected");
                self.shell_ready.store(true, Ordering::Release);
            }
            if scan.forward.is_empty() {
                return; // marker consumed entire chunk
            }
            // scan.forward is a freshly-built Vec<u8>; wrap it in Bytes (no copy).
            bytes::Bytes::from(scan.forward)
        } else {
            data
        };

        // Track terminal modes and CWD from escape sequences
        self.mode_tracker.process(&forward);
        if let Some(cwd) = self.mode_tracker.cwd() {
            self.meta.cwd = std::path::PathBuf::from(cwd);
        }

        // Write to scrollback for cold restore
        if let Err(e) = self.scrollback_writer.write(&forward) {
            warn!(session_id = %self.id, "scrollback write failed: {e}");
        }

        // Broadcast to attached clients with backpressure.
        // Bytes clone is a refcount bump (cheap) so each client gets its own
        // handle without copying the chunk bytes.
        let mut msg = Some(DataMsg {
            session_id: self.id,
            data: forward,
        });
        let last_idx = self.attached_clients.len().saturating_sub(1);
        let mut idx = 0;
        self.attached_clients.retain_mut(|client| {
            let event_data = if idx == last_idx {
                msg.take().unwrap()
            } else {
                msg.as_ref().unwrap().clone()
            };
            idx += 1;
            match client.tx.try_send(ClientEvent::Data(event_data)) {
                Ok(()) => {
                    client.consecutive_drops = 0;
                    true
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    client.consecutive_drops += 1;
                    if client.consecutive_drops > CLIENT_STALL_DISCONNECT_THRESHOLD {
                        warn!(session_id = %self.id, client_id = %client.id,
                              "disconnecting stalled client after {} drops", client.consecutive_drops);
                        false
                    } else {
                        true // keep client, drop this data
                    }
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            }
        });
    }

    /// Mark the session as exited.
    pub fn on_exit(&mut self, code: i32) {
        self.status = SessionStatus::Exited { code };
        self.meta.status = self.status;
        save_meta(&self.meta);

        let msg = ExitMsg {
            session_id: self.id,
            status: self.status,
        };
        for client in &self.attached_clients {
            client.tx.try_send(ClientEvent::Exit(msg.clone())).ok();
        }
        self.attached_clients.clear();

        // If this exit was triggered by a kill request, clean up on-disk resources.
        if self._kill_task.is_some() {
            self.cleanup_on_kill();
        }

        info!(session_id = %self.id, code, "session exited");
    }

    /// Start async kill escalation: SIGTERM → 2s → SIGKILL → 1s → force exit.
    /// The session stays Alive until the exit event arrives via broadcast.
    pub fn start_kill(&mut self) {
        if !matches!(self.status, SessionStatus::Alive) {
            return;
        }

        let pid = self.child_pid;
        let session_id = self.id;
        let event_tx = self.event_tx.clone();

        // Send SIGTERM immediately
        #[cfg(unix)]
        send_signal_to_group(pid, libc::SIGTERM);
        debug!(session_id = %self.id, "sent SIGTERM, starting kill escalation");

        // Spawn async escalation task
        let task = tokio::spawn(async move {
            // Stage 1: poll for exit after SIGTERM (2 seconds)
            for _ in 0..KILL_SIGTERM_TIMEOUT_POLLS {
                tokio::time::sleep(KILL_ESCALATION_POLL_INTERVAL).await;
                if !is_process_alive(pid) {
                    return; // Process exited — reader will send Exit event
                }
            }

            // Stage 2: SIGKILL
            #[cfg(unix)]
            send_signal_to_group(pid, libc::SIGKILL);
            warn!(session_id = %session_id, "escalated to SIGKILL");

            // Poll for exit after SIGKILL (1 second)
            for _ in 0..KILL_SIGKILL_TIMEOUT_POLLS {
                tokio::time::sleep(KILL_ESCALATION_POLL_INTERVAL).await;
                if !is_process_alive(pid) {
                    return;
                }
            }

            // Stage 3: force synthetic exit
            error!(session_id = %session_id, "process did not exit after SIGKILL, forcing");
            let msg = ExitMsg {
                session_id,
                status: SessionStatus::Exited { code: -9 },
            };
            event_tx.send(ClientEvent::Exit(msg)).await.ok();
        });

        self._kill_task = Some(task);
    }

    /// Immediate SIGKILL without escalation. Used during graceful daemon shutdown.
    pub fn force_kill(&mut self) {
        if !matches!(self.status, SessionStatus::Alive) {
            return;
        }
        #[cfg(unix)]
        send_signal_to_group(self.child_pid, libc::SIGKILL);
        self.status = SessionStatus::Exited { code: -9 };
        self.meta.status = self.status;
        self.attached_clients.clear();
        debug!(session_id = %self.id, "force killed session");
    }

    /// Clean up all on-disk resources for a killed session.
    /// Called by TerminalHost right before the session is dropped.
    pub fn cleanup_on_kill(&mut self) {
        save_meta(&self.meta);
        let scrollback_path = paths::scrollback_path(self.id);
        std::fs::remove_file(&scrollback_path).ok();
    }

    /// Check if shell readiness has timed out.
    pub fn check_readiness_timeout(&mut self) {
        let was_pending = self.readiness.is_active();
        self.readiness.check_timeout();
        if was_pending && !self.readiness.is_active() {
            debug!(session_id = %self.id, "shell readiness timed out");
            self.shell_ready.store(true, Ordering::Release);
        }
    }

    /// Update foreground process info (called periodically).
    pub fn refresh_process_info(&mut self) {
        self.meta.foreground_process = process_info::foreground_process_name(self.child_pid);
        if let Some(cwd) = process_info::process_cwd(self.child_pid) {
            self.meta.cwd = cwd;
        }
    }
}

/// Background loop that reads PTY output and sends it to the host via the
/// session's per-session event channel. Backpressure: when the channel is
/// full, `blocking_send` parks this thread → kernel PTY buffer fills →
/// subprocess stdout blocks. Per-session channels mean a slow drain for
/// session A can't park session B's reader here.
fn pty_reader_loop(
    session_id: SessionId,
    mut reader: Box<dyn std::io::Read + Send>,
    event_tx: mpsc::Sender<ClientEvent>,
) {
    let mut buf = [0u8; 4096];
    let mut transient_window = TransientErrorWindow::new();
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                // PTY closed — child exited
                let msg = ExitMsg {
                    session_id,
                    status: SessionStatus::Exited { code: 0 },
                };
                event_tx.blocking_send(ClientEvent::Exit(msg)).ok();
                break;
            }
            Ok(n) => {
                let msg = DataMsg {
                    session_id,
                    data: bytes::Bytes::copy_from_slice(&buf[..n]),
                };
                if event_tx.blocking_send(ClientEvent::Data(msg)).is_err() {
                    break;
                }
            }
            Err(e) if transient_errors::is_transient(&e) => {
                if transient_window.record() {
                    error!(session_id = %session_id, "too many transient errors, treating as fatal: {e}");
                    let msg = ExitMsg {
                        session_id,
                        status: SessionStatus::Exited { code: -1 },
                    };
                    event_tx.blocking_send(ClientEvent::Exit(msg)).ok();
                    break;
                }
                warn!(session_id = %session_id, "transient PTY read error (continuing): {e}");
                continue;
            }
            Err(e) => {
                error!(session_id = %session_id, "PTY read error: {e}");
                let msg = ExitMsg {
                    session_id,
                    status: SessionStatus::Exited { code: -1 },
                };
                event_tx.blocking_send(ClientEvent::Exit(msg)).ok();
                break;
            }
        }
    }
}

/// Background loop that writes input data to the PTY.
/// Drains the channel and batches writes for efficiency.
fn pty_writer_loop(
    session_id: SessionId,
    mut writer: Box<dyn Write + Send>,
    rx: &mut mpsc::Receiver<Vec<u8>>,
) {
    let mut transient_window = TransientErrorWindow::new();
    while let Some(data) = rx.blocking_recv() {
        if let Err(e) = writer.write_all(&data) {
            if transient_errors::is_transient(&e) && !transient_window.record() {
                warn!(session_id = %session_id, "transient PTY write error (continuing): {e}");
                continue;
            }
            warn!(session_id = %session_id, "PTY write error: {e}");
            break;
        }
        // Drain additional pending writes for batching
        while let Ok(more) = rx.try_recv() {
            if let Err(e) = writer.write_all(&more) {
                if transient_errors::is_transient(&e) && !transient_window.record() {
                    warn!(session_id = %session_id, "transient PTY write error (continuing): {e}");
                    continue;
                }
                warn!(session_id = %session_id, "PTY write error: {e}");
                return;
            }
        }
        if let Err(e) = writer.flush() {
            if transient_errors::is_transient(&e) && !transient_window.record() {
                warn!(session_id = %session_id, "transient PTY flush error (continuing): {e}");
                continue;
            }
            warn!(session_id = %session_id, "PTY flush error: {e}");
            break;
        }
    }
}

/// Check if a process is still alive using kill(pid, 0).
fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        unsafe { libc::kill(pid, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn save_meta(meta: &SessionMeta) {
    let dir = paths::session_history_dir(meta.session_id);
    if std::fs::create_dir_all(&dir).is_ok() {
        let path = paths::meta_path(meta.session_id);
        if let Ok(json) = serde_json::to_string(meta) {
            std::fs::write(path, json).ok();
        }
    }
}

fn epoch_secs_string() -> String {
    use std::time::SystemTime;
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    secs.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use seoul_terminal_proto::messages::DataMsg;

    /// Each session owns its own bounded event channel. Filling one
    /// session's queue must not affect another session's queue — that
    /// independence is the entire point of moving away from the previous
    /// shared `(SessionId, ClientEvent)` mpsc channel, where a backed-up
    /// drain for any one session would block every other session's PTY
    /// reader on `blocking_send`.
    #[test]
    fn per_session_channels_are_independent() {
        // Build two channels at the same capacity used in production.
        let (tx_a, mut rx_a) = mpsc::channel::<ClientEvent>(SESSION_EVENT_CHANNEL_CAPACITY);
        let (tx_b, mut rx_b) = mpsc::channel::<ClientEvent>(SESSION_EVENT_CHANNEL_CAPACITY);
        let id_a = SessionId::new_v4();
        let id_b = SessionId::new_v4();

        // Fill A's channel to capacity. Every send must succeed.
        for _ in 0..SESSION_EVENT_CHANNEL_CAPACITY {
            tx_a.try_send(ClientEvent::Data(DataMsg {
                session_id: id_a,
                data: bytes::Bytes::from_static(b"a"),
            }))
            .expect("A: send within capacity must succeed");
        }
        // One more send to A would now fail — proves A is saturated.
        assert!(
            matches!(
                tx_a.try_send(ClientEvent::Data(DataMsg {
                    session_id: id_a,
                    data: bytes::Bytes::from_static(b"a"),
                })),
                Err(mpsc::error::TrySendError::Full(_))
            ),
            "A must be full at capacity"
        );

        // B is untouched — sending on it must still succeed even though A
        // is jammed. With a single shared channel this would have failed.
        tx_b.try_send(ClientEvent::Data(DataMsg {
            session_id: id_b,
            data: bytes::Bytes::from_static(b"b"),
        }))
        .expect("B's channel must be unaffected by A's saturation");

        // Drain B and confirm we get exactly the bytes we sent into B.
        let evt = rx_b.try_recv().expect("B has one pending event");
        match evt {
            ClientEvent::Data(d) => {
                assert_eq!(d.session_id, id_b);
                assert_eq!(d.data.as_ref(), b"b");
            }
            other => panic!("expected Data, got {other:?}"),
        }
        // And A still has all its events queued (untouched by B's drain).
        let mut drained = 0;
        while rx_a.try_recv().is_ok() {
            drained += 1;
        }
        assert_eq!(drained, SESSION_EVENT_CHANNEL_CAPACITY);
    }
}
