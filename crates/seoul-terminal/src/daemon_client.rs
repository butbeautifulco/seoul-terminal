use std::collections::HashMap;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_channel::TrySendError;
use uuid::Uuid;

use seoul_terminal_proto::frame::{Frame, MessageType};
use seoul_terminal_proto::messages::*;
use seoul_terminal_proto::paths;
use seoul_terminal_proto::pr::{PrInfo, PrUnavailableReason};
use seoul_terminal_proto::resources::ResourceSnapshot;
use seoul_terminal_proto::session::SessionId;

/// PR status events forwarded from the daemon to the app.
#[derive(Debug, Clone)]
pub enum PrEvent {
    Updated {
        workspace_id: Uuid,
        pr_info: Option<PrInfo>,
    },
    Unavailable {
        workspace_id: Uuid,
        reason: PrUnavailableReason,
    },
}

/// A session handle given to TerminalView for reading data and writing input.
pub struct DaemonSessionHandle {
    pub session_id: SessionId,
    pub data_rx: async_channel::Receiver<Vec<u8>>,
    pub resize_ack_rx: async_channel::Receiver<(u16, u16)>,
    pub attached_msg: SessionAttachedMsg,
}

/// Writes input data to the daemon as Write frames for a specific session.
/// All writes are forwarded to the central daemon-writer thread via the
/// shared channel, so `Write::write` never blocks on socket I/O. Adjacent
/// same-session writes are concatenated by the writer thread's coalescing
/// pass (see `coalesce_outgoing`).
pub struct DaemonWriter {
    session_id: SessionId,
    writer_tx: mpsc::Sender<OutgoingMessage>,
}

impl DaemonWriter {
    fn new(session_id: SessionId, writer_tx: mpsc::Sender<OutgoingMessage>) -> Self {
        Self {
            session_id,
            writer_tx,
        }
    }
}

impl Write for DaemonWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.writer_tx
            .send(OutgoingMessage::Write(WriteMsg {
                session_id: self.session_id,
                data: buf.to_vec(),
            }))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::BrokenPipe, e.to_string()))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

type SessionSenders = Arc<Mutex<HashMap<SessionId, async_channel::Sender<Vec<u8>>>>>;

/// Per-session channels for ResizeAck messages from the daemon.
type ResizeAckSenders = Arc<Mutex<HashMap<SessionId, async_channel::Sender<(u16, u16)>>>>;

/// Per-request oneshot channel for routing RPC responses by session_id.
type PendingRequests = Arc<Mutex<HashMap<SessionId, mpsc::Sender<Frame>>>>;
const INITIAL_ATTACH_SCROLLBACK_LIMIT_BYTES: usize = 128 * 1024;
const APP_SESSION_DATA_CHANNEL_CAPACITY: usize = 512;

fn remove_session_sender_if_same(
    session_senders: &SessionSenders,
    session_id: SessionId,
    failed_tx: &async_channel::Sender<Vec<u8>>,
) -> bool {
    let mut senders = session_senders.lock().unwrap();
    if senders
        .get(&session_id)
        .is_some_and(|current| current.same_channel(failed_tx))
    {
        senders.remove(&session_id);
        return true;
    }
    false
}

/// Messages enqueued for the central daemon-writer thread.
/// All daemon-bound IPC routes through this enum so that encode/lock/flush
/// runs on a dedicated thread, keeping the UI render loop responsive.
pub(crate) enum OutgoingMessage {
    /// Resize a PTY. Coalesced: if multiple Resize for the same session are
    /// queued, only the latest is actually sent to the daemon.
    Resize(ResizeMsg),
    /// Write input to a PTY. Adjacent Writes for the same session are
    /// concatenated into a single frame.
    Write(WriteMsg),
    /// Kill a session. Not coalesced.
    Kill(KillMsg),
    /// Detach from a session. Not coalesced.
    Detach(DetachMsg),
    /// Request a resource snapshot. Not coalesced (caller awaits response).
    GetResourceSnapshot,
    /// Pre-encoded frame (used for handshake, create-or-attach, etc.).
    Raw(Frame),
}

impl OutgoingMessage {
    fn into_frame(self) -> Option<Frame> {
        match self {
            OutgoingMessage::Resize(m) => Frame::from_msg(MessageType::Resize, &m).ok(),
            OutgoingMessage::Write(m) => Frame::from_msg(MessageType::Write, &m).ok(),
            OutgoingMessage::Kill(m) => Frame::from_msg(MessageType::Kill, &m).ok(),
            OutgoingMessage::Detach(m) => Frame::from_msg(MessageType::Detach, &m).ok(),
            OutgoingMessage::GetResourceSnapshot => {
                Frame::from_msg(MessageType::GetResourceSnapshot, &GetResourceSnapshotMsg {}).ok()
            }
            OutgoingMessage::Raw(f) => Some(f),
        }
    }
}

/// Coalesce a batch of outgoing messages drained from the writer channel.
///
/// Rules:
/// - Keep only the LATEST Resize per session (drop earlier ones).
/// - Concatenate adjacent same-session Writes into a single frame.
/// - Preserve the relative order of surviving messages.
fn coalesce_outgoing(batch: Vec<OutgoingMessage>) -> Vec<OutgoingMessage> {
    let mut latest_resize_idx: HashMap<SessionId, usize> = HashMap::new();
    let mut keep = vec![true; batch.len()];
    for (i, msg) in batch.iter().enumerate() {
        if let OutgoingMessage::Resize(r) = msg {
            if let Some(&old) = latest_resize_idx.get(&r.session_id) {
                keep[old] = false;
            }
            latest_resize_idx.insert(r.session_id, i);
        }
    }

    let mut out: Vec<OutgoingMessage> = Vec::with_capacity(batch.len());
    for (i, msg) in batch.into_iter().enumerate() {
        if !keep[i] {
            continue;
        }
        match msg {
            OutgoingMessage::Write(mut w) => {
                if let Some(OutgoingMessage::Write(prev)) = out.last_mut()
                    && prev.session_id == w.session_id
                {
                    prev.data.append(&mut w.data);
                    continue;
                }
                out.push(OutgoingMessage::Write(w));
            }
            other => out.push(other),
        }
    }
    out
}

/// Central writer thread body: owns the sole lock on the socket writer.
/// Drains the channel in batches, coalesces, and writes each batch under
/// a single lock acquisition to minimize churn.
fn writer_loop(rx: mpsc::Receiver<OutgoingMessage>, socket_writer: Arc<Mutex<UnixStream>>) {
    while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        while let Ok(more) = rx.try_recv() {
            batch.push(more);
        }
        let coalesced = coalesce_outgoing(batch);

        let Ok(mut writer) = socket_writer.lock() else {
            return;
        };
        for msg in coalesced {
            let Some(frame) = msg.into_frame() else {
                continue;
            };
            if frame.encode(&mut *writer).is_err() {
                return;
            }
        }
        if writer.flush().is_err() {
            return;
        }
    }
}

/// Thin clone of the DaemonClient's writer channel for sending resize
/// (and other fire-and-forget) commands from any thread without blocking.
#[derive(Clone)]
pub struct DaemonClientWriter {
    writer_tx: mpsc::Sender<OutgoingMessage>,
}

impl DaemonClientWriter {
    #[allow(dead_code)]
    pub fn kill(&self, session_id: SessionId) {
        let _ = self
            .writer_tx
            .send(OutgoingMessage::Kill(KillMsg { session_id }));
    }

    pub fn resize(
        &self,
        session_id: SessionId,
        cols: u16,
        rows: u16,
        pixel_width: u32,
        pixel_height: u32,
    ) {
        let _ = self.writer_tx.send(OutgoingMessage::Resize(ResizeMsg {
            session_id,
            cols,
            rows,
            pixel_width,
            pixel_height,
        }));
    }
}

/// Shared inner state that can be sent to background threads.
pub(crate) struct DaemonClientInner {
    /// Channel into the central writer thread. All daemon-bound IPC goes here.
    pub(crate) writer_tx: mpsc::Sender<OutgoingMessage>,
    /// Retained solely so the writer thread's Arc clone keeps the socket alive
    /// for its lifetime. Do NOT use this to write from any other thread —
    /// always route through `writer_tx`.
    _socket_writer: Arc<Mutex<UnixStream>>,
    session_senders: SessionSenders,
    resize_ack_senders: ResizeAckSenders,
    /// Per-request response channels, keyed by session_id.
    pending_requests: PendingRequests,
    /// Dedicated channel for resource snapshots (avoids contention with RPC).
    resource_rx: Arc<Mutex<mpsc::Receiver<ResourceSnapshot>>>,
    /// PR status events from the daemon's PrPoller.
    pr_event_rx: Arc<Mutex<mpsc::Receiver<PrEvent>>>,
    /// Set to false when the reader thread exits (daemon died or socket error).
    alive: Arc<AtomicBool>,
}

/// Client for communicating with the seoul-daemon.
///
/// Architecture: a single reader thread reads ALL incoming frames and dispatches:
/// - Data/Exit → per-session mpsc channels
/// - SessionAttached/Error → per-request oneshot channels (by session_id)
/// - ResourceSnapshot → dedicated resource channel
pub struct DaemonClient {
    inner: Arc<DaemonClientInner>,
    _reader_thread: Option<thread::JoinHandle<()>>,
}

impl DaemonClient {
    /// Whether the daemon connection is still alive.
    pub fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::Relaxed)
    }

    /// Get a clonable Arc handle for sending create_or_attach from background threads.
    pub fn inner_handle(&self) -> Arc<DaemonClientInner> {
        self.inner.clone()
    }

    /// Connect to the daemon. If not running, spawn it first.
    /// On build hash mismatch, restarts the daemon automatically (max 1 retry).
    pub fn connect_or_spawn() -> Result<Self> {
        match Self::connect_or_spawn_inner() {
            Ok(client) => Ok(client),
            Err(e) if e.to_string().contains("daemon build mismatch") => {
                tracing::info!("respawning daemon after build mismatch");
                Self::spawn_daemon()?;
                Self::wait_and_connect()
            }
            Err(e) => Err(e),
        }
    }

    fn connect_or_spawn_inner() -> Result<Self> {
        let socket_path = paths::socket_path()?;

        // Try to connect to existing daemon
        if let Ok(stream) = UnixStream::connect(&socket_path) {
            return Self::from_stream(stream);
        }

        // Spawn daemon
        tracing::info!("spawning seoul-daemon");
        Self::spawn_daemon()?;
        Self::wait_and_connect()
    }

    /// Wait for daemon socket to appear, then connect (exponential backoff, max 3s).
    fn wait_and_connect() -> Result<Self> {
        let socket_path = paths::socket_path()?;
        let mut wait_ms = 10;
        let mut total_waited = 0;
        while total_waited < 3000 {
            thread::sleep(Duration::from_millis(wait_ms));
            total_waited += wait_ms;
            if let Ok(stream) = UnixStream::connect(&socket_path) {
                return Self::from_stream(stream);
            }
            wait_ms = (wait_ms * 2).min(200);
        }
        bail!(
            "timed out waiting for daemon socket at {}",
            socket_path.display()
        )
    }

    fn spawn_daemon() -> Result<()> {
        let daemon_name = "seoul-daemon";
        let daemon_path = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|dir| dir.join(daemon_name)))
            .filter(|p| p.exists())
            .unwrap_or_else(|| PathBuf::from(daemon_name));

        let mut cmd = Command::new(&daemon_path);
        cmd.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        // Detach from the app's process group so SIGINT delivered to the
        // foreground tty (Cmd+C on `just app`) does not also kill the daemon
        // and tear down every PTY along with it.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }

        cmd.spawn()
            .with_context(|| format!("failed to spawn {}", daemon_path.display()))?;

        Ok(())
    }

    fn from_stream(stream: UnixStream) -> Result<Self> {
        let reader_stream = stream.try_clone().context("failed to clone socket")?;
        let socket_writer = Arc::new(Mutex::new(stream));
        let session_senders: SessionSenders = Arc::new(Mutex::new(HashMap::new()));
        let pending_requests: PendingRequests = Arc::new(Mutex::new(HashMap::new()));

        // Auth handshake BEFORE starting reader thread (sequential reads are safe here)
        {
            let token = std::fs::read_to_string(paths::token_path()?)
                .context("failed to read daemon token")?;

            let hello = HelloMsg {
                token: token.trim().to_string(),
                protocol_version: PROTOCOL_VERSION,
                client_id: Uuid::new_v4(),
                build_hash: BUILD_HASH.to_string(),
            };

            let frame = Frame::from_msg(MessageType::Hello, &hello)?;
            {
                let mut writer = socket_writer.lock().unwrap();
                frame.encode(&mut *writer)?;
                writer.flush()?;
            }

            // Read HelloAck directly (reader thread not started yet)
            let mut reader_ref = &reader_stream;
            let ack_frame = Frame::decode(&mut reader_ref)?;

            if ack_frame.message_type == MessageType::Error {
                let err: ErrorMsg = ack_frame.decode_msg()?;
                bail!("daemon rejected hello: {}", err.message);
            }
            if ack_frame.message_type != MessageType::HelloAck {
                bail!("expected HelloAck, got {:?}", ack_frame.message_type);
            }
            let ack: HelloAckMsg = ack_frame.decode_msg()?;
            tracing::info!(daemon_pid = ack.daemon_pid, "connected to daemon");

            // Check build hash — if the daemon is from a different build, signal restart
            if !ack.build_hash.is_empty() && ack.build_hash != BUILD_HASH {
                tracing::warn!(
                    daemon_hash = %ack.build_hash,
                    app_hash = BUILD_HASH,
                    "daemon build mismatch, requesting shutdown"
                );
                // Ask daemon to shut down so we can spawn a matching one
                let shutdown_frame = Frame::new(MessageType::Shutdown, Vec::new());
                if let Ok(mut writer) = socket_writer.lock() {
                    shutdown_frame.encode(&mut *writer).ok();
                    writer.flush().ok();
                }
                // Brief wait — daemon deletes runtime files immediately on shutdown
                thread::sleep(Duration::from_millis(200));
                bail!("daemon build mismatch");
            }
        }

        // Spawn the central daemon-writer thread. It owns the sole lock on
        // socket_writer for the rest of the process lifetime, so no caller
        // ever blocks on encode/lock/flush from the UI hot path.
        let (writer_tx, writer_rx) = mpsc::channel::<OutgoingMessage>();
        {
            let socket_writer_clone = socket_writer.clone();
            thread::Builder::new()
                .name("daemon-writer".to_string())
                .spawn(move || writer_loop(writer_rx, socket_writer_clone))
                .context("failed to spawn daemon-writer thread")?;
        }

        // NOW start the reader thread — it owns the reader_stream exclusively
        let senders_clone = session_senders.clone();
        let resize_ack_senders: ResizeAckSenders = Arc::new(Mutex::new(HashMap::new()));
        let resize_ack_clone = resize_ack_senders.clone();
        let pending_clone = pending_requests.clone();
        let (resource_tx, resource_rx) = mpsc::channel::<ResourceSnapshot>();
        let (pr_event_tx, pr_event_rx) = mpsc::channel::<PrEvent>();
        let alive = Arc::new(AtomicBool::new(true));
        let alive_clone = alive.clone();
        let reader_thread = thread::spawn(move || {
            Self::reader_loop(
                reader_stream,
                senders_clone,
                resize_ack_clone,
                pending_clone,
                resource_tx,
                pr_event_tx,
            );
            alive_clone.store(false, Ordering::Relaxed);
        });

        let inner = Arc::new(DaemonClientInner {
            writer_tx,
            _socket_writer: socket_writer,
            session_senders,
            resize_ack_senders,
            pending_requests,
            resource_rx: Arc::new(Mutex::new(resource_rx)),
            pr_event_rx: Arc::new(Mutex::new(pr_event_rx)),
            alive,
        });

        Ok(Self {
            inner,
            _reader_thread: Some(reader_thread),
        })
    }

    /// Create or attach to a session. Returns a handle for TerminalView.
    /// This method is safe to call from background threads via inner_handle().
    pub fn create_or_attach(
        &self,
        session_id: SessionId,
        workspace_id: Uuid,
        cols: u16,
        rows: u16,
        cwd: Option<PathBuf>,
    ) -> Result<DaemonSessionHandle> {
        DaemonClientInner::create_or_attach(&self.inner, session_id, workspace_id, cols, rows, cwd)
    }

    /// Create a DaemonWriter for a session (used as Terminal's SharedWriter inner value).
    pub fn writer_for_session(&self, session_id: SessionId) -> DaemonWriter {
        DaemonWriter::new(session_id, self.inner.writer_tx.clone())
    }

    /// Clone the writer handle for resize commands from TerminalView.
    pub fn clone_writer(&self) -> DaemonClientWriter {
        DaemonClientWriter {
            writer_tx: self.inner.writer_tx.clone(),
        }
    }

    /// Create a ResourceRequester handle for the polling task.
    pub fn clone_resource_requester(&self) -> crate::resource_indicator::ResourceRequester {
        crate::resource_indicator::ResourceRequester {
            writer_tx: self.inner.writer_tx.clone(),
            resource_rx: self.inner.resource_rx.clone(),
        }
    }

    /// Kill a session (terminates the PTY process in daemon).
    pub fn kill(&self, session_id: SessionId) -> Result<()> {
        self.inner
            .writer_tx
            .send(OutgoingMessage::Kill(KillMsg { session_id }))
            .context("daemon writer channel closed")?;

        self.inner
            .session_senders
            .lock()
            .unwrap()
            .remove(&session_id);
        Ok(())
    }

    /// Drain at most one queued PR event (non-blocking). Called from the
    /// AppView poll task on a GPUI background timer.
    pub fn try_recv_pr_event(&self) -> Option<PrEvent> {
        self.inner.pr_event_rx.lock().ok()?.try_recv().ok()
    }

    /// Tell the daemon about a workspace it should start polling.
    pub fn register_workspace(
        &self,
        workspace_id: Uuid,
        working_dir: PathBuf,
        branch: String,
    ) -> Result<()> {
        let msg = WorkspaceRegisteredMsg {
            workspace_id,
            working_dir,
            branch,
        };
        let frame = Frame::from_msg(MessageType::WorkspaceRegistered, &msg)?;
        self.inner
            .writer_tx
            .send(OutgoingMessage::Raw(frame))
            .context("daemon writer channel closed")
    }

    /// Tell the daemon a workspace was removed (stop polling, drop cache).
    pub fn unregister_workspace(&self, workspace_id: Uuid) -> Result<()> {
        let msg = WorkspaceUnregisteredMsg { workspace_id };
        let frame = Frame::from_msg(MessageType::WorkspaceUnregistered, &msg)?;
        self.inner
            .writer_tx
            .send(OutgoingMessage::Raw(frame))
            .context("daemon writer channel closed")
    }

    /// Tell the daemon which workspace is currently focused (10s vs 120s polling).
    pub fn focus_workspace(&self, workspace_id: Option<Uuid>) -> Result<()> {
        let msg = WorkspaceFocusedMsg { workspace_id };
        let frame = Frame::from_msg(MessageType::WorkspaceFocused, &msg)?;
        self.inner
            .writer_tx
            .send(OutgoingMessage::Raw(frame))
            .context("daemon writer channel closed")
    }

    /// Force-refresh PR status for a workspace right now.
    pub fn refresh_pr(&self, workspace_id: Uuid) -> Result<()> {
        let msg = RefreshPrMsg { workspace_id };
        let frame = Frame::from_msg(MessageType::RefreshPr, &msg)?;
        self.inner
            .writer_tx
            .send(OutgoingMessage::Raw(frame))
            .context("daemon writer channel closed")
    }

    /// Detach from a session (PTY stays alive in daemon for later reattach).
    pub fn detach(&self, session_id: SessionId) -> Result<()> {
        self.inner
            .writer_tx
            .send(OutgoingMessage::Detach(DetachMsg { session_id }))
            .context("daemon writer channel closed")?;

        self.inner
            .session_senders
            .lock()
            .unwrap()
            .remove(&session_id);
        Ok(())
    }

    /// Request a resource usage snapshot from the daemon.
    #[allow(dead_code)]
    pub fn request_resource_snapshot(&self) -> Result<ResourceSnapshot> {
        self.inner
            .writer_tx
            .send(OutgoingMessage::GetResourceSnapshot)
            .context("daemon writer channel closed")?;

        self.inner
            .resource_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .context("timed out waiting for resource snapshot")
    }

    /// Reader thread: reads ALL frames from the socket and dispatches them.
    /// - Data/Exit → per-session channels
    /// - SessionAttached/Error → per-request channels (by session_id)
    /// - ResourceSnapshot → dedicated resource channel
    fn reader_loop(
        mut reader: UnixStream,
        session_senders: SessionSenders,
        resize_ack_senders: ResizeAckSenders,
        pending_requests: PendingRequests,
        resource_tx: mpsc::Sender<ResourceSnapshot>,
        pr_event_tx: mpsc::Sender<PrEvent>,
    ) {
        loop {
            let frame = match Frame::decode(&mut reader) {
                Ok(f) => f,
                Err(e) => {
                    tracing::debug!("daemon reader loop ended: {e}");
                    break;
                }
            };

            match frame.message_type {
                // Streaming events → per-session channels
                MessageType::Data => {
                    if let Ok(msg) = frame.decode_msg::<DataMsg>() {
                        let tx = {
                            let senders = session_senders.lock().unwrap();
                            senders.get(&msg.session_id).cloned()
                        };
                        if let Some(tx) = tx
                            && let Err(err) = tx.try_send(msg.data.to_vec())
                        {
                            let reason = match err {
                                TrySendError::Full(_) => "full",
                                TrySendError::Closed(_) => "closed",
                            };
                            let removed = remove_session_sender_if_same(
                                &session_senders,
                                msg.session_id,
                                &tx,
                            );
                            tracing::warn!(
                                session_id = %msg.session_id,
                                reason,
                                removed,
                                "dropping stalled app-side terminal data receiver"
                            );
                        }
                    }
                }
                MessageType::Exit => {
                    if let Ok(msg) = frame.decode_msg::<ExitMsg>() {
                        tracing::info!(session_id = %msg.session_id, "session exited");
                        session_senders.lock().unwrap().remove(&msg.session_id);
                    }
                }

                // Resource snapshots → dedicated channel
                MessageType::ResourceSnapshot => {
                    if let Ok(snapshot) = frame.decode_msg::<ResourceSnapshot>() {
                        resource_tx.send(snapshot).ok();
                    }
                }

                // RPC responses → per-request channels (routed by session_id)
                MessageType::SessionAttached => {
                    // Route by session_id to the waiting caller
                    if let Ok(msg) = frame.decode_msg::<SessionAttachedMsg>() {
                        let mut pending = pending_requests.lock().unwrap();
                        if let Some(tx) = pending.remove(&msg.session_id) {
                            // Re-encode to send the full frame
                            tx.send(frame).ok();
                        }
                    }
                }
                MessageType::SessionEnsured => {
                    if let Ok(msg) = frame.decode_msg::<SessionEnsuredMsg>() {
                        let mut pending = pending_requests.lock().unwrap();
                        if let Some(tx) = pending.remove(&msg.session_id) {
                            tx.send(frame).ok();
                        }
                    }
                }
                MessageType::Error => {
                    if let Ok(msg) = frame.decode_msg::<ErrorMsg>()
                        && let Some(sid) = msg.session_id
                    {
                        let mut pending = pending_requests.lock().unwrap();
                        if let Some(tx) = pending.remove(&sid) {
                            tx.send(frame).ok();
                            continue;
                        }
                    }
                    // Unroutable error — log it
                    tracing::debug!("unroutable error frame");
                }

                MessageType::ResizeAck => {
                    if let Ok(msg) = frame.decode_msg::<ResizeAckMsg>() {
                        let senders = resize_ack_senders.lock().unwrap();
                        if let Some(tx) = senders.get(&msg.session_id) {
                            tx.send_blocking((msg.cols, msg.rows)).ok();
                        }
                    }
                }

                MessageType::Pong | MessageType::SessionList => {
                    // Currently unused — ignore
                    tracing::debug!("received {:?}, ignoring", frame.message_type);
                }

                MessageType::PrStatusUpdated => {
                    if let Ok(msg) = frame.decode_msg::<PrStatusUpdatedMsg>() {
                        pr_event_tx
                            .send(PrEvent::Updated {
                                workspace_id: msg.workspace_id,
                                pr_info: msg.pr_info,
                            })
                            .ok();
                    }
                }
                MessageType::PrStatusUnavailable => {
                    if let Ok(msg) = frame.decode_msg::<PrStatusUnavailableMsg>() {
                        pr_event_tx
                            .send(PrEvent::Unavailable {
                                workspace_id: msg.workspace_id,
                                reason: msg.reason,
                            })
                            .ok();
                    }
                }

                other => {
                    tracing::debug!("unexpected frame type in reader: {:?}", other);
                }
            }
        }
    }
}

impl DaemonClientInner {
    /// Create or attach to a session. Can be called from any thread.
    pub fn create_or_attach(
        inner: &Arc<Self>,
        session_id: SessionId,
        workspace_id: Uuid,
        cols: u16,
        rows: u16,
        cwd: Option<PathBuf>,
    ) -> Result<DaemonSessionHandle> {
        let msg = CreateOrAttachMsg {
            session_id,
            workspace_id,
            cols,
            rows,
            cwd,
            shell: None,
            scrollback_limit_bytes: Some(INITIAL_ATTACH_SCROLLBACK_LIMIT_BYTES),
        };

        // Register session data channel with a bounded app-side queue so a
        // stalled UI consumer cannot grow memory without limit.
        let (data_tx, data_rx) =
            async_channel::bounded::<Vec<u8>>(APP_SESSION_DATA_CHANNEL_CAPACITY);
        {
            let mut senders = inner.session_senders.lock().unwrap();
            senders.insert(session_id, data_tx);
        }

        // Register resize ACK channel. Unbounded async_channel so the
        // daemon-reader thread (which calls send_blocking) never stalls if
        // the UI drain task hasn't woken yet — bounded would risk blocking
        // the socket reader and stalling all sessions.
        let (resize_ack_tx, resize_ack_rx) = async_channel::unbounded();
        {
            let mut ack_senders = inner.resize_ack_senders.lock().unwrap();
            ack_senders.insert(session_id, resize_ack_tx);
        }

        // Register per-request response channel
        let (response_tx, response_rx) = mpsc::channel();
        {
            let mut pending = inner.pending_requests.lock().unwrap();
            pending.insert(session_id, response_tx);
        }

        // Send CreateOrAttach via the central writer thread.
        let frame = Frame::from_msg(MessageType::CreateOrAttach, &msg)?;
        inner
            .writer_tx
            .send(OutgoingMessage::Raw(frame))
            .context("daemon writer channel closed")?;

        // Wait for response routed by session_id (with timeout)
        let response = response_rx
            .recv_timeout(Duration::from_secs(5))
            .context("timed out waiting for session response")?;

        if response.message_type == MessageType::Error {
            let err: ErrorMsg = response.decode_msg()?;
            inner.session_senders.lock().unwrap().remove(&session_id);
            bail!("failed to create/attach session: {}", err.message);
        }

        if response.message_type != MessageType::SessionAttached {
            inner.session_senders.lock().unwrap().remove(&session_id);
            bail!("unexpected response type: {:?}", response.message_type);
        }

        let attached: SessionAttachedMsg = response.decode_msg()?;

        Ok(DaemonSessionHandle {
            session_id,
            data_rx,
            resize_ack_rx,
            attached_msg: attached,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_sender_removal_keeps_replacement_sender() {
        let session_id = Uuid::new_v4();
        let session_senders = Arc::new(Mutex::new(HashMap::new()));
        let (old_tx, _old_rx) = async_channel::bounded::<Vec<u8>>(1);
        let stale_tx = old_tx.clone();
        let (new_tx, _new_rx) = async_channel::bounded::<Vec<u8>>(1);

        session_senders.lock().unwrap().insert(session_id, old_tx);
        session_senders
            .lock()
            .unwrap()
            .insert(session_id, new_tx.clone());

        assert!(!remove_session_sender_if_same(
            &session_senders,
            session_id,
            &stale_tx
        ));
        let current = session_senders
            .lock()
            .unwrap()
            .get(&session_id)
            .cloned()
            .expect("replacement sender should remain");
        assert!(current.same_channel(&new_tx));
    }

    #[test]
    fn stale_sender_removal_removes_matching_sender() {
        let session_id = Uuid::new_v4();
        let session_senders = Arc::new(Mutex::new(HashMap::new()));
        let (tx, _rx) = async_channel::bounded::<Vec<u8>>(1);

        session_senders
            .lock()
            .unwrap()
            .insert(session_id, tx.clone());

        assert!(remove_session_sender_if_same(
            &session_senders,
            session_id,
            &tx
        ));
        assert!(!session_senders.lock().unwrap().contains_key(&session_id));
    }
}
