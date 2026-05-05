use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use octocrab::Octocrab;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, broadcast, mpsc, watch};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use seoul_terminal_proto::frame::{Frame, HEADER_SIZE, MAX_FRAME_SIZE, MessageType};
use seoul_terminal_proto::messages::*;
use seoul_workspace::git::hosting::HostingRegistry;

use std::collections::HashMap;

use crate::host::{AttachResult, EnsureResult, TerminalHost};
use crate::pr_poller::{self, PollerHandle};
use crate::session::{ClientEvent, ClientHandle, SessionWriteHandle};

/// Capacity of the daemon-wide broadcast channel that fans PR status events
/// out to every connected client. Tuned to avoid `Lagged` errors during normal
/// poller bursts but small enough to drop messages if a client stalls.
const PR_BROADCAST_CAPACITY: usize = 256;

fn restore_trace_enabled() -> bool {
    std::env::var_os("SEOUL_RESTORE_TRACE").is_some()
}

/// Shared host handle type.
pub type SharedHost = Arc<Mutex<TerminalHost>>;

/// Run the daemon server loop. Returns the shared host handle for graceful shutdown.
pub async fn run(
    listener: UnixListener,
    host: TerminalHost,
    token: String,
    shutdown_tx: watch::Sender<bool>,
    octo: Arc<Octocrab>,
    registry: Arc<HostingRegistry>,
) -> SharedHost {
    let host: SharedHost = Arc::new(Mutex::new(host));

    // PR poller broadcast: one publisher (poller), many subscribers (per-client
    // writer tasks). Each client gets its own `Receiver` via `subscribe()`.
    let (pr_broadcast_tx, _) = broadcast::channel::<ClientEvent>(PR_BROADCAST_CAPACITY);
    let poller = pr_poller::spawn(octo, registry, pr_broadcast_tx.clone());

    // Periodic maintenance: drain broadcasts, refresh process info, cleanup.
    // Uses try_lock() to avoid blocking attach operations.
    let maintenance_host = host.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(16));
        let mut refresh_counter = 0u32;
        loop {
            interval.tick().await;

            // try_lock: skip this tick if host is busy (e.g., during attach)
            let Ok(mut h) = maintenance_host.try_lock() else {
                continue;
            };
            h.drain_broadcast();

            refresh_counter += 1;
            // Check shell readiness timeouts every ~1 second (1000ms / 16ms ≈ 62)
            if refresh_counter.is_multiple_of(62) {
                h.check_readiness_timeouts();
            }
            // Refresh process info every ~5 seconds (5000ms / 16ms ≈ 312)
            if refresh_counter.is_multiple_of(312) {
                h.refresh_all_process_info();
            }
            // Cleanup exited sessions every ~30 seconds
            if refresh_counter.is_multiple_of(1875) {
                h.cleanup_exited_sessions();
            }
            // Cleanup old scrollback files every ~10 minutes (600s / 0.016s = 37500)
            if refresh_counter.is_multiple_of(37500) {
                h.cleanup_old_scrollback();
                refresh_counter = 0;
            }
        }
    });

    info!("server accepting connections");

    let accept_host = host.clone();
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let host = accept_host.clone();
                    let token = token.clone();
                    let shutdown_tx = shutdown_tx.clone();
                    let pr_rx = pr_broadcast_tx.subscribe();
                    let poller = poller.clone();
                    tokio::spawn(async move {
                        if let Err(e) =
                            handle_connection(stream, host, token, shutdown_tx, pr_rx, poller).await
                        {
                            debug!("connection ended: {e}");
                        }
                    });
                }
                Err(e) => {
                    error!("accept error: {e}");
                }
            }
        }
    });

    host
}

async fn handle_connection(
    stream: UnixStream,
    host: Arc<Mutex<TerminalHost>>,
    expected_token: String,
    shutdown_tx: watch::Sender<bool>,
    mut pr_rx: broadcast::Receiver<ClientEvent>,
    poller: PollerHandle,
) -> Result<()> {
    let (mut reader, mut writer) = stream.into_split();
    let client_id = Uuid::new_v4();

    // Auth handshake
    let hello_frame = read_frame(&mut reader).await?;
    if hello_frame.message_type != MessageType::Hello {
        send_error(&mut writer, "expected Hello message", None).await?;
        anyhow::bail!("expected Hello");
    }
    let hello: HelloMsg = hello_frame.decode_msg()?;
    if hello.token != expected_token {
        send_error(&mut writer, "invalid token", None).await?;
        anyhow::bail!("invalid token");
    }
    if hello.protocol_version != PROTOCOL_VERSION {
        send_error(
            &mut writer,
            &format!(
                "protocol version mismatch: got {}, expected {}",
                hello.protocol_version, PROTOCOL_VERSION
            ),
            None,
        )
        .await?;
        anyhow::bail!("protocol version mismatch");
    }

    // Send HelloAck
    let ack = HelloAckMsg {
        protocol_version: PROTOCOL_VERSION,
        daemon_pid: std::process::id(),
        build_hash: BUILD_HASH.to_string(),
    };
    send_frame(&mut writer, MessageType::HelloAck, &ack).await?;

    if !hello.build_hash.is_empty() && hello.build_hash != BUILD_HASH {
        warn!(
            client_id = %client_id,
            client_hash = %hello.build_hash,
            daemon_hash = BUILD_HASH,
            "build hash mismatch with client"
        );
    }

    info!(client_id = %client_id, "client authenticated");

    // Channel for session events → client writer (bounded for backpressure)
    let (event_tx, mut event_rx) = crate::session::client_event_channel();

    // Spawn writer task: forwards session events to the client socket
    let writer = Arc::new(Mutex::new(writer));
    let writer_clone = writer.clone();
    let writer_task = tokio::spawn(async move {
        loop {
            // Per-session events take priority over PR broadcasts (terminal data
            // is latency-sensitive; PR updates can wait one tick if the client
            // is busy).
            let event = tokio::select! {
                biased;
                Some(e) = event_rx.recv() => e,
                broadcast_event = pr_rx.recv() => match broadcast_event {
                    Ok(e) => e,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("client lagged PR broadcast by {n} messages");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                else => break,
            };
            let w = &mut *writer_clone.lock().await;
            let result = match &event {
                ClientEvent::Data(msg) => send_frame(w, MessageType::Data, msg).await,
                ClientEvent::Exit(msg) => send_frame(w, MessageType::Exit, msg).await,
                ClientEvent::PrStatusUpdated(msg) => {
                    send_frame(w, MessageType::PrStatusUpdated, msg).await
                }
                ClientEvent::PrStatusUnavailable(msg) => {
                    send_frame(w, MessageType::PrStatusUnavailable, msg).await
                }
            };
            if result.is_err() {
                break;
            }
        }
    });

    // Main read loop: process client requests
    let result = client_read_loop(
        &mut reader,
        host.clone(),
        writer.clone(),
        client_id,
        event_tx,
        shutdown_tx,
        poller,
    )
    .await;

    writer_task.abort();

    // Detach from all sessions on disconnect
    // (The host doesn't track which sessions a client is attached to globally,
    //  but session-level detach happens when the client handle's channel closes)
    debug!(client_id = %client_id, "client disconnected");

    result
}

async fn client_read_loop(
    reader: &mut tokio::net::unix::OwnedReadHalf,
    host: Arc<Mutex<TerminalHost>>,
    writer: Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
    client_id: Uuid,
    event_tx: mpsc::Sender<ClientEvent>,
    shutdown_tx: watch::Sender<bool>,
    poller: PollerHandle,
) -> Result<()> {
    // Per-message decode helper: log and skip on failure (don't kill connection).
    macro_rules! decode_or_skip {
        ($frame:expr, $t:ty) => {
            match $frame.decode_msg::<$t>() {
                Ok(m) => m,
                Err(e) => {
                    warn!("failed to decode {}: {e}", stringify!($t));
                    continue;
                }
            }
        };
    }

    // Per-session write handles for lock-free Write operations
    let mut session_writers: HashMap<Uuid, SessionWriteHandle> = HashMap::new();

    loop {
        let frame = read_frame(reader).await?;

        match frame.message_type {
            MessageType::CreateOrAttach => {
                let msg: CreateOrAttachMsg = decode_or_skip!(frame, CreateOrAttachMsg);
                let session_id = msg.session_id;
                let client_handle = ClientHandle::new(client_id, event_tx.clone());
                let request_started_at = Instant::now();

                // Phase 1: try fast attach under lock (no I/O)
                let result = {
                    let mut h = host.lock().await;
                    h.create_or_attach_fast(msg, client_handle)
                }; // lock released

                match result {
                    Ok(AttachResult::Attached {
                        attached,
                        write_handle,
                    }) => {
                        if restore_trace_enabled() {
                            tracing::info!(
                                session_id = %session_id,
                                elapsed_ms = request_started_at.elapsed().as_millis(),
                                scrollback_bytes = attached.scrollback_data.len(),
                                "restore trace: daemon warm attach ready"
                            );
                        }
                        // Write handle obtained inside same lock scope — no race
                        session_writers.insert(session_id, write_handle);
                        let w = &mut *writer.lock().await;
                        send_frame(w, MessageType::SessionAttached, &attached).await?;
                    }
                    Ok(AttachResult::NeedsSpawn { msg, client }) => {
                        // Phase 2: disk I/O outside the lock
                        let disk_started_at = Instant::now();
                        let saved_meta = load_saved_meta_async(msg.session_id).await;
                        // Cold-restore unless the session exited normally (code >= 0,
                        // user-initiated `exit`). Signal-killed sessions (code < 0,
                        // e.g. -9 SIGKILL when the daemon was killed) keep their
                        // scrollback so a daemon restart doesn't lose the user's terminal.
                        let should_cold_restore = match &saved_meta {
                            Some(meta) => match meta.status {
                                seoul_terminal_proto::session::SessionStatus::Exited { code } => {
                                    code < 0
                                }
                                _ => true,
                            },
                            None => true,
                        };
                        let (scrollback_data, was_recovered) = if should_cold_restore {
                            let data =
                                load_scrollback_async(msg.session_id, msg.scrollback_limit_bytes)
                                    .await;
                            let recovered = !data.is_empty();
                            (data, recovered)
                        } else {
                            (Vec::new(), false)
                        };
                        if restore_trace_enabled() {
                            tracing::info!(
                                session_id = %session_id,
                                disk_ms = disk_started_at.elapsed().as_millis(),
                                elapsed_ms = request_started_at.elapsed().as_millis(),
                                meta_found = saved_meta.is_some(),
                                scrollback_bytes = scrollback_data.len(),
                                was_recovered,
                                "restore trace: daemon cold restore disk phase complete"
                            );
                        }
                        let (effective_cols, effective_rows) = if let Some(ref meta) = saved_meta {
                            if meta.cols > 0 && meta.rows > 0 {
                                (meta.cols, meta.rows)
                            } else {
                                (msg.cols, msg.rows)
                            }
                        } else {
                            (msg.cols, msg.rows)
                        };
                        let effective_cwd = if was_recovered {
                            saved_meta.map(|m| m.cwd).or(msg.cwd)
                        } else {
                            msg.cwd
                        };

                        // Re-acquire lock for session spawn + registration
                        let spawn_msg = CreateOrAttachMsg {
                            session_id,
                            workspace_id: msg.workspace_id,
                            cols: effective_cols,
                            rows: effective_rows,
                            cwd: effective_cwd,
                            shell: msg.shell,
                            scrollback_limit_bytes: msg.scrollback_limit_bytes,
                        };
                        let mut h = host.lock().await;
                        match h.spawn_and_attach(spawn_msg, client, scrollback_data, was_recovered)
                        {
                            Ok(attached) => {
                                if restore_trace_enabled() {
                                    tracing::info!(
                                        session_id = %session_id,
                                        elapsed_ms = request_started_at.elapsed().as_millis(),
                                        scrollback_bytes = attached.scrollback_data.len(),
                                        was_recovered = attached.was_recovered,
                                        "restore trace: daemon spawned and attached"
                                    );
                                }
                                // Get write handle for lock-free writes
                                if let Some(session) = h.get_session(session_id) {
                                    session_writers.insert(session_id, session.write_handle());
                                }
                                drop(h);
                                let w = &mut *writer.lock().await;
                                send_frame(w, MessageType::SessionAttached, &attached).await?;
                            }
                            Err(e) => {
                                drop(h);
                                let w = &mut *writer.lock().await;
                                send_error(w, &e.to_string(), Some(session_id)).await?;
                            }
                        }
                    }
                    Err(e) => {
                        let w = &mut *writer.lock().await;
                        send_error(w, &e.to_string(), Some(session_id)).await?;
                    }
                }
            }
            MessageType::EnsureSession => {
                let msg: EnsureSessionMsg = decode_or_skip!(frame, EnsureSessionMsg);
                let session_id = msg.session_id;
                let request_started_at = Instant::now();

                let result = {
                    let mut h = host.lock().await;
                    h.ensure_session_fast(msg)
                };

                match result {
                    Ok(EnsureResult::Ensured(ensured)) => {
                        if restore_trace_enabled() {
                            tracing::info!(
                                session_id = %session_id,
                                elapsed_ms = request_started_at.elapsed().as_millis(),
                                "restore trace: daemon session already ensured"
                            );
                        }
                        let w = &mut *writer.lock().await;
                        send_frame(w, MessageType::SessionEnsured, &ensured).await?;
                    }
                    Ok(EnsureResult::NeedsSpawn { msg }) => {
                        let disk_started_at = Instant::now();
                        let saved_meta = load_saved_meta_async(msg.session_id).await;
                        let should_recover = match &saved_meta {
                            Some(meta) => match meta.status {
                                seoul_terminal_proto::session::SessionStatus::Exited { code } => {
                                    code < 0
                                }
                                _ => true,
                            },
                            None => false,
                        };
                        let (effective_cols, effective_rows) = if let Some(ref meta) = saved_meta {
                            if meta.cols > 0 && meta.rows > 0 {
                                (meta.cols, meta.rows)
                            } else {
                                (msg.cols, msg.rows)
                            }
                        } else {
                            (msg.cols, msg.rows)
                        };
                        let meta_found = saved_meta.is_some();
                        let effective_cwd = if should_recover {
                            saved_meta.map(|m| m.cwd).or(msg.cwd)
                        } else {
                            msg.cwd
                        };
                        let spawn_msg = EnsureSessionMsg {
                            session_id,
                            workspace_id: msg.workspace_id,
                            cols: effective_cols,
                            rows: effective_rows,
                            cwd: effective_cwd,
                            shell: msg.shell,
                        };
                        if restore_trace_enabled() {
                            tracing::info!(
                                session_id = %session_id,
                                disk_ms = disk_started_at.elapsed().as_millis(),
                                elapsed_ms = request_started_at.elapsed().as_millis(),
                                meta_found,
                                should_recover,
                                "restore trace: daemon ensure disk phase complete"
                            );
                        }

                        let mut h = host.lock().await;
                        match h.spawn_unattached(spawn_msg, should_recover) {
                            Ok(ensured) => {
                                if restore_trace_enabled() {
                                    tracing::info!(
                                        session_id = %session_id,
                                        elapsed_ms = request_started_at.elapsed().as_millis(),
                                        was_recovered = ensured.was_recovered,
                                        "restore trace: daemon spawned session without attach"
                                    );
                                }
                                drop(h);
                                let w = &mut *writer.lock().await;
                                send_frame(w, MessageType::SessionEnsured, &ensured).await?;
                            }
                            Err(e) => {
                                drop(h);
                                let w = &mut *writer.lock().await;
                                send_error(w, &e.to_string(), Some(session_id)).await?;
                            }
                        }
                    }
                    Err(e) => {
                        let w = &mut *writer.lock().await;
                        send_error(w, &e.to_string(), Some(session_id)).await?;
                    }
                }
            }
            MessageType::Write => {
                let msg: WriteMsg = decode_or_skip!(frame, WriteMsg);
                // Lock-free write via per-session channel (bounded, with backpressure).
                // If shell is not yet ready, fall back to host-locked path for input buffering.
                if let Some(handle) = session_writers.get(&msg.session_id) {
                    if handle.tx.is_closed() {
                        // Session exited naturally — clean up stale entry
                        session_writers.remove(&msg.session_id);
                    } else if handle
                        .shell_ready
                        .load(std::sync::atomic::Ordering::Acquire)
                    {
                        if handle.tx.send(msg.data).await.is_err() {
                            session_writers.remove(&msg.session_id);
                        }
                    } else {
                        // Shell not ready — route through session for input buffering
                        let mut h = host.lock().await;
                        if let Some(session) = h.get_session_mut(msg.session_id)
                            && let Err(e) = session.write(&msg.data)
                        {
                            warn!("write failed: {e}");
                        }
                    }
                } else {
                    // Fallback: acquire host lock (first write before attach response)
                    let mut h = host.lock().await;
                    if let Some(session) = h.get_session_mut(msg.session_id)
                        && let Err(e) = session.write(&msg.data)
                    {
                        warn!("write failed: {e}");
                    }
                }
            }
            MessageType::Resize => {
                let msg: ResizeMsg = decode_or_skip!(frame, ResizeMsg);
                let sid = msg.session_id;
                let cols = msg.cols;
                let rows = msg.rows;
                let mut h = host.lock().await;
                if let Err(e) = h.resize(msg) {
                    warn!("resize failed: {e}");
                } else {
                    let ack = ResizeAckMsg {
                        session_id: sid,
                        cols,
                        rows,
                    };
                    let w = &mut *writer.lock().await;
                    if let Err(e) = send_frame(w, MessageType::ResizeAck, &ack).await {
                        warn!("failed to send ResizeAck: {e}");
                    }
                }
            }
            MessageType::Kill => {
                let msg: KillMsg = decode_or_skip!(frame, KillMsg);
                session_writers.remove(&msg.session_id);
                let mut h = host.lock().await;
                h.kill(msg);
            }
            MessageType::Detach => {
                let msg: DetachMsg = decode_or_skip!(frame, DetachMsg);
                session_writers.remove(&msg.session_id);
                let mut h = host.lock().await;
                h.detach(msg, client_id);
            }
            MessageType::ListSessions => {
                let h = host.lock().await;
                let list = h.list_sessions();
                let w = &mut *writer.lock().await;
                send_frame(w, MessageType::SessionList, &list).await?;
            }
            MessageType::GetResourceSnapshot => {
                let mut h = host.lock().await;
                let snapshot = h.get_resource_snapshot();
                let w = &mut *writer.lock().await;
                send_frame(w, MessageType::ResourceSnapshot, &snapshot).await?;
            }
            MessageType::Signal => {
                let msg: SignalMsg = decode_or_skip!(frame, SignalMsg);
                let mut h = host.lock().await;
                h.signal(msg);
            }
            MessageType::Ping => {
                let w = &mut *writer.lock().await;
                // Pong has no payload
                let frame = Frame::new(MessageType::Pong, Vec::new());
                write_raw_frame(w, &frame).await?;
            }
            MessageType::WorkspaceRegistered => {
                let msg: WorkspaceRegisteredMsg = decode_or_skip!(frame, WorkspaceRegisteredMsg);
                poller.register(
                    msg.workspace_id,
                    msg.working_dir,
                    msg.branch,
                    msg.default_branch,
                );
            }
            MessageType::WorkspaceUnregistered => {
                let msg: WorkspaceUnregisteredMsg =
                    decode_or_skip!(frame, WorkspaceUnregisteredMsg);
                poller.unregister(msg.workspace_id);
            }
            MessageType::WorkspaceFocused => {
                let msg: WorkspaceFocusedMsg = decode_or_skip!(frame, WorkspaceFocusedMsg);
                poller.focus(msg.workspace_id);
            }
            MessageType::RefreshPr => {
                let msg: RefreshPrMsg = decode_or_skip!(frame, RefreshPrMsg);
                poller.refresh(msg.workspace_id);
            }
            MessageType::Shutdown => {
                info!("shutdown requested by client");
                shutdown_tx.send(true).ok();
                return Ok(());
            }
            _ => {
                warn!(
                    "unexpected message type from client: {:?}",
                    frame.message_type
                );
            }
        }
    }
}

// ── Async frame I/O helpers ──────────────────────────────────

async fn read_frame(reader: &mut tokio::net::unix::OwnedReadHalf) -> Result<Frame> {
    let mut header_buf = [0u8; HEADER_SIZE];
    reader
        .read_exact(&mut header_buf)
        .await
        .context("failed to read frame header")?;

    let msg_type = MessageType::from_u8(header_buf[0])
        .context(format!("unknown message type: {}", header_buf[0]))?;
    let payload_len =
        u32::from_le_bytes([header_buf[1], header_buf[2], header_buf[3], header_buf[4]]) as usize;

    if payload_len > MAX_FRAME_SIZE {
        anyhow::bail!("frame too large: {payload_len}");
    }

    let mut payload = vec![0u8; payload_len];
    reader
        .read_exact(&mut payload)
        .await
        .context("failed to read frame payload")?;

    Ok(Frame {
        message_type: msg_type,
        payload,
    })
}

async fn send_frame<T: serde::Serialize>(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    msg_type: MessageType,
    msg: &T,
) -> Result<()> {
    let frame = Frame::from_msg(msg_type, msg)?;
    write_raw_frame(writer, &frame).await
}

async fn write_raw_frame(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    frame: &Frame,
) -> Result<()> {
    let mut buf = Vec::with_capacity(HEADER_SIZE + frame.payload.len());
    frame.encode(&mut buf)?;
    writer
        .write_all(&buf)
        .await
        .context("failed to write frame")?;
    writer.flush().await.context("failed to flush")?;
    Ok(())
}

/// Load saved session metadata from disk without blocking the tokio executor.
async fn load_saved_meta_async(
    session_id: uuid::Uuid,
) -> Option<seoul_terminal_proto::session::SessionMeta> {
    tokio::task::spawn_blocking(move || crate::host::load_saved_meta(session_id))
        .await
        .ok()
        .flatten()
}

async fn load_scrollback_async(session_id: uuid::Uuid, limit: Option<usize>) -> Vec<u8> {
    tokio::task::spawn_blocking(move || {
        match limit {
            Some(limit) => {
                crate::scrollback::ScrollbackWriter::read_scrollback_with_limit(session_id, limit)
            }
            None => crate::scrollback::ScrollbackWriter::read_scrollback(session_id),
        }
        .unwrap_or_default()
    })
    .await
    .unwrap_or_default()
}

async fn send_error(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    message: &str,
    session_id: Option<Uuid>,
) -> Result<()> {
    let msg = ErrorMsg {
        message: message.to_string(),
        session_id,
    };
    send_frame(writer, MessageType::Error, &msg).await
}
