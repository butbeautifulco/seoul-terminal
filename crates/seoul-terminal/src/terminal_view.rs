use std::cell::{Cell, RefCell};
use std::io::Write;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

use futures::FutureExt as _;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use libghostty_vt::terminal::ScrollViewport;
use libghostty_vt::{key, key as gkey, mouse};
use seoul_vt::config::TerminalConfig;
use seoul_vt::terminal::TerminalBounds;
use seoul_vt::{Terminal, TerminalBuilder};
use seoul_workspace::settings::SettingsStore;

use crate::daemon_client::{DaemonClient, DaemonClientWriter, DaemonSessionHandle};
use crate::terminal_element::render_terminal;
use crate::terminal_render_cache::TerminalRenderCache;

actions!(terminal, [Paste, Copy]);

fn restore_trace_enabled() -> bool {
    std::env::var_os("SEOUL_RESTORE_TRACE").is_some()
}

enum BootstrapState {
    #[allow(dead_code)]
    Local { cwd: Option<PathBuf> },
    Attached {
        session_id: seoul_terminal_proto::session::SessionId,
        attached_msg: seoul_terminal_proto::messages::SessionAttachedMsg,
        data_rx: async_channel::Receiver<Vec<u8>>,
        resize_ack_rx: async_channel::Receiver<(u16, u16)>,
        writer: Box<dyn Write + Send>,
        daemon_client_writer: DaemonClientWriter,
    },
}

pub struct TerminalView {
    terminal: Terminal,
    data_rx: async_channel::Receiver<Vec<u8>>,
    focus_handle: FocusHandle,
    _focus_subscriptions: [Subscription; 2],
    config: TerminalConfig,
    cell_width: f32,
    cell_height: f32,
    last_cols: u16,
    /// Pending mode: waiting for daemon connection, no PTY spawned.
    pending: bool,
    last_rows: u16,
    // Cursor blink — event-driven via background_executor timers.
    // `blink_epoch` discriminates current vs. stale timer callbacks: every
    // state transition that should invalidate in-flight timers bumps the
    // epoch, and stale callbacks no-op on mismatch. `blink_paused_until`
    // is set by user-perceptible activity (keystroke, scroll, PTY output)
    // to suppress toggling for BLINK_PAUSE.
    cursor_visible: bool,
    blink_epoch: u64,
    blink_paused_until: Option<Instant>,
    // Scroll
    scroll_px: f32,
    // Scrollbar — `scrollbar_visible` is the rendered state; the fade timer
    // is event-driven (see poke_scrollbar). `scrollbar_fade_epoch` is bumped
    // by every poke so any in-flight fade timer becomes stale and no-ops on
    // its callback.
    scrollbar_visible: bool,
    scrollbar_fade_epoch: u64,
    // IME
    ime_preedit: String,
    // Element bounds for resize (shared with paint callback via Rc, no Entity access)
    element_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    mouse_buttons_pressed: MouseButtonsPressed,
    // Daemon session info (None for local PTY mode)
    session_id: Option<seoul_terminal_proto::session::SessionId>,
    daemon_client_writer: Option<DaemonClientWriter>,
    bootstrap: Option<BootstrapState>,
    // Resize debounce — coalesces rapid resize events during window drag.
    // Every check_resize that detects a geometry change writes the latest
    // target into `pending_resize` and bumps `resize_debounce_epoch`, then
    // arms a fresh RESIZE_DEBOUNCE timer. Only the latest timer (matching
    // epoch) flushes; earlier timers no-op on stale epoch. The pending
    // value is the trailing-edge target — once the user pauses for the
    // debounce window, it gets applied.
    pending_resize: Option<(u16, u16)>,
    resize_debounce_epoch: u64,
    // In daemon mode, the local terminal resize is held until ResizeAck
    // arrives. `pending_bounds` is the in-flight target. If the daemon
    // never ACKs (lost packet, crash), the ACK-timeout timer fires after
    // RESIZE_ACK_TIMEOUT and applies the resize locally as a fallback.
    // `resize_ack_timeout_epoch` discriminates current vs. stale timer
    // callbacks; every arm or successful ACK bumps it.
    pending_bounds: Option<TerminalBounds>,
    resize_ack_timeout_epoch: u64,
    resize_ack_rx: async_channel::Receiver<(u16, u16)>,
    // Per-view terminal render cache rebuilt only for dirty rows.
    render_cache: Rc<RefCell<TerminalRenderCache>>,
    // Background task that drains PTY bytes from `data_rx`, batches them with
    // a short coalescing window, and feeds them into the terminal on the main
    // thread. `None` until a real data_rx is wired up (e.g. local PTY ready
    // or a daemon session attached). Drop = automatic cancel — replacing
    // `data_rx` always means replacing this task too.
    data_drain_task: Option<gpui::Task<()>>,
    // Background task that drains ResizeAck messages from `resize_ack_rx`
    // and forwards them to `apply_resize_ack` on the main thread. Only
    // spawned for daemon-attached sessions; local-PTY mode has no ACK
    // protocol. Drop = automatic cancel.
    resize_ack_drain_task: Option<gpui::Task<()>>,
}

#[derive(Clone, Copy, Default)]
struct MouseButtonsPressed {
    left: bool,
    right: bool,
    middle: bool,
}

impl MouseButtonsPressed {
    fn set(&mut self, button: MouseButton, pressed: bool) {
        match button {
            MouseButton::Left => self.left = pressed,
            MouseButton::Right => self.right = pressed,
            MouseButton::Middle => self.middle = pressed,
            _ => {}
        }
    }

    fn any(self) -> bool {
        self.left || self.right || self.middle
    }
}

enum MappedKeystroke {
    Encoded {
        key: gkey::Key,
        mods: gkey::Mods,
        utf8: Option<String>,
        unshifted: Option<char>,
    },
    Raw(&'static [u8]),
}

impl TerminalView {
    fn config_from_settings(cx: &App) -> TerminalConfig {
        let s = cx.global::<SettingsStore>().global().terminal.clone();
        TerminalConfig {
            font_family: s.font_family,
            font_size: s.font_size,
            scrollback_lines: s.scrollback_lines,
            padding: s.padding,
            ..TerminalConfig::default()
        }
    }

    fn measure_cells(window: &mut Window, config: &TerminalConfig) -> (f32, f32) {
        let text_system = window.text_system();
        let font_obj = font(config.font_family.clone());
        let font_id = text_system.resolve_font(&font_obj);
        let font_size_px = px(config.font_size);
        let cell_width: f32 = text_system
            .advance(font_id, font_size_px, 'm')
            .map(|size| f32::from(size.width))
            .unwrap_or(config.font_size * 0.6);
        let ascent = f32::from(text_system.ascent(font_id, font_size_px));
        let descent = f32::from(text_system.descent(font_id, font_size_px));
        let cell_height = ((ascent + descent.abs()) * config.line_height_multiplier).ceil();
        (cell_width, cell_height)
    }

    fn empty_data_rx() -> async_channel::Receiver<Vec<u8>> {
        let (_tx, rx) = async_channel::unbounded::<Vec<u8>>();
        drop(_tx);
        rx
    }

    /// Closed-sender placeholder receiver for `resize_ack_rx`.
    ///
    /// Used in local-PTY mode (no daemon, no ACK protocol) and as the
    /// stand-in left behind when `spawn_resize_ack_drain` takes ownership
    /// of the real receiver. Mirrors `empty_data_rx`. Any call to
    /// `recv().await` on this receiver returns `Err` immediately, so
    /// drain tasks accidentally wired to it exit cleanly.
    fn empty_ack_rx() -> async_channel::Receiver<(u16, u16)> {
        let (_tx, rx) = async_channel::unbounded::<(u16, u16)>();
        drop(_tx);
        rx
    }

    /// Spawn the PTY-data drain task.
    ///
    /// Takes ownership of the current `data_rx` (replacing it with a dead
    /// receiver — kept only as a placeholder so the field's type stays
    /// valid; nothing reads from it after the swap) and runs an event loop
    /// that wakes only when bytes arrive. The loop reads one chunk
    /// blocking, then opens a 4 ms coalescing window during which more
    /// chunks (up to 100) are batched. Each batch is delivered to the
    /// terminal in a single `this.update(...)`, which also calls `sync()`,
    /// optionally updates the scrollbar, and notifies for repaint.
    ///
    /// The returned `Task<()>` is stored in `data_drain_task`; dropping the
    /// view (or replacing the task) cancels it. This pattern follows zed's
    /// terminal event_loop_task in `crates/terminal/src/terminal.rs`.
    fn spawn_data_drain(&mut self, cx: &mut Context<Self>) {
        let data_rx = std::mem::replace(&mut self.data_rx, Self::empty_data_rx());
        self.data_drain_task = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            loop {
                let first = match data_rx.recv().await {
                    Ok(d) => d,
                    Err(_) => break,
                };
                let mut batch: Vec<Vec<u8>> = vec![first];
                // 4 ms coalescing window — short enough not to add visible
                // latency, long enough to batch fast-output bursts (e.g. `cat
                // bigfile`) into a handful of feed_pty_data calls per frame.
                let timer = cx
                    .background_executor()
                    .timer(Duration::from_millis(4))
                    .fuse();
                futures::pin_mut!(timer);
                loop {
                    futures::select_biased! {
                        _ = timer.as_mut() => break,
                        next = data_rx.recv().fuse() => match next {
                            Ok(d) => {
                                batch.push(d);
                                if batch.len() > 100 {
                                    break;
                                }
                            }
                            // Channel closed mid-batch (sender dropped, e.g. daemon
                            // hot-reload). Break the inner loop so the partial batch
                            // is still flushed below; the outer loop's recv().await
                            // will then return Err and exit cleanly.
                            Err(_) => break,
                        },
                    }
                }
                if this
                    .update(cx, |this, cx| {
                        for chunk in &batch {
                            this.terminal.feed_pty_data(chunk);
                        }
                        this.terminal.sync();
                        if this.scrollbar_visible {
                            this.terminal.update_scrollbar();
                        }
                        cx.notify();
                        // PTY output is user-perceptible activity: pause the
                        // blink so the cursor stays visible while data streams.
                        this.show_cursor_now(cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn placeholder_terminal(config: &TerminalConfig) -> Terminal {
        let mut terminal = TerminalBuilder::new(config.clone())
            .build_attached(Box::new(std::io::sink()))
            .expect("Failed to create placeholder terminal");
        terminal.last_content.cursor.visible = false;
        terminal
    }

    fn spawn_reader_thread(
        mut reader: Box<dyn std::io::Read + Send>,
    ) -> async_channel::Receiver<Vec<u8>> {
        let (data_tx, data_rx) = async_channel::unbounded::<Vec<u8>>();
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match std::io::Read::read(&mut reader, &mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if data_tx.send_blocking(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        data_rx
    }

    fn bounds_for_viewport(&self, viewport: Size<Pixels>) -> Option<TerminalBounds> {
        let cw = self.cell_width;
        let ch = self.cell_height;
        if cw <= 0.0 || ch <= 0.0 {
            return None;
        }

        let pad = self.config.padding * 2.0;
        let w = (f32::from(viewport.width) - pad).max(0.0);
        let h = (f32::from(viewport.height) - pad).max(0.0);
        if w <= 0.0 || h <= 0.0 {
            return None;
        }

        Some(TerminalBounds {
            cols: (w / cw).next_up().floor().max(2.0) as u16,
            rows: (h / ch).next_up().floor().max(1.0) as u16,
            cell_width: cw,
            line_height: ch,
        })
    }

    fn is_interactive(&self) -> bool {
        !self.pending && self.bootstrap.is_none()
    }

    #[allow(dead_code)]
    pub fn new(window: &mut Window, cx: &mut Context<Self>, cwd: Option<PathBuf>) -> Self {
        let config = Self::config_from_settings(cx);
        let terminal = Self::placeholder_terminal(&config);
        let data_rx = Self::empty_data_rx();
        let (cell_width, cell_height) = Self::measure_cells(window, &config);

        let focus_handle = cx.focus_handle();
        let _focus_subscriptions = [
            cx.on_focus_in(&focus_handle, window, |this, _window, cx| {
                if this.is_interactive() {
                    this.terminal.focus_in();
                }
                cx.notify();
            }),
            cx.on_focus_out(&focus_handle, window, |this, _event, _window, cx| {
                if this.is_interactive() {
                    this.terminal.focus_out();
                }
                cx.notify();
            }),
        ];

        let mut this = Self {
            terminal,
            data_rx,
            focus_handle,
            _focus_subscriptions,
            last_cols: config.cols,
            last_rows: config.rows,
            cell_width,
            cell_height,
            config,
            cursor_visible: true,
            blink_epoch: 0,
            blink_paused_until: None,
            scroll_px: 0.0,
            scrollbar_visible: false,
            scrollbar_fade_epoch: 0,
            ime_preedit: String::new(),
            element_bounds: Rc::new(Cell::new(None)),
            mouse_buttons_pressed: MouseButtonsPressed::default(),
            session_id: None,
            daemon_client_writer: None,
            bootstrap: Some(BootstrapState::Local { cwd }),
            pending: false,
            pending_resize: None,
            resize_debounce_epoch: 0,
            pending_bounds: None,
            resize_ack_timeout_epoch: 0,
            resize_ack_rx: Self::empty_ack_rx(),
            render_cache: Rc::new(RefCell::new(TerminalRenderCache::default())),
            data_drain_task: None,
            resize_ack_drain_task: None,
        };
        // Bootstrap the blink cycle: pauses for BLINK_PAUSE so the first
        // toggle fires at +BLINK_PAUSE, matching activity-driven behavior.
        this.show_cursor_now(cx);
        this
    }

    /// Create a pending terminal view — no PTY, no shell process.
    /// Shows "Reconnecting..." until `attach_session()` is called.
    pub fn new_pending(
        window: &mut Window,
        cx: &mut Context<Self>,
        session_id: seoul_terminal_proto::session::SessionId,
    ) -> Self {
        let config = Self::config_from_settings(cx);
        let terminal = Self::placeholder_terminal(&config);
        let data_rx = Self::empty_data_rx();
        let (cell_width, cell_height) = Self::measure_cells(window, &config);

        let focus_handle = cx.focus_handle();
        let _focus_subscriptions = [
            cx.on_focus_in(&focus_handle, window, |this, _window, cx| {
                if this.is_interactive() {
                    this.terminal.focus_in();
                }
                cx.notify();
            }),
            cx.on_focus_out(&focus_handle, window, |this, _event, _window, cx| {
                if this.is_interactive() {
                    this.terminal.focus_out();
                }
                cx.notify();
            }),
        ];

        // No bootstrap of the blink cycle here: pending mode shows a
        // "Restoring session..." overlay with no visible cursor. The
        // attach path will call show_cursor_now once a real session is
        // wired up.
        Self {
            terminal,
            data_rx,
            focus_handle,
            _focus_subscriptions,
            last_cols: config.cols,
            last_rows: config.rows,
            cell_width,
            cell_height,
            config,
            cursor_visible: true,
            blink_epoch: 0,
            blink_paused_until: None,
            scroll_px: 0.0,
            scrollbar_visible: false,
            scrollbar_fade_epoch: 0,
            ime_preedit: String::new(),
            element_bounds: Rc::new(Cell::new(None)),
            mouse_buttons_pressed: MouseButtonsPressed::default(),
            session_id: Some(session_id),
            daemon_client_writer: None,
            bootstrap: None,
            pending: true,
            pending_resize: None,
            resize_debounce_epoch: 0,
            pending_bounds: None,
            resize_ack_timeout_epoch: 0,
            resize_ack_rx: Self::empty_ack_rx(),
            render_cache: Rc::new(RefCell::new(TerminalRenderCache::default())),
            data_drain_task: None,
            resize_ack_drain_task: None,
        }
    }

    pub fn mark_pending_restore(&mut self) {
        if self.session_id.is_some() {
            self.pending = true;
        }
    }

    pub fn is_pending_restore(&self) -> bool {
        self.pending
    }

    fn initialize_local_terminal(
        &mut self,
        bounds: TerminalBounds,
        cwd: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let mut config = self.config.clone();
        config.cols = bounds.cols;
        config.rows = bounds.rows;

        let mut builder = TerminalBuilder::new(config);
        if let Some(cwd) = cwd {
            builder = builder.cwd(cwd);
        }
        let (mut terminal, reader) = builder.build().expect("Failed to spawn terminal");
        let data_rx = Self::spawn_reader_thread(reader);
        let _ = terminal.resize(bounds);

        self.data_rx = data_rx;
        self.resize_ack_rx = Self::empty_ack_rx();
        // Drop any in-flight ACK drain task — local-PTY mode has no daemon ACKs.
        self.resize_ack_drain_task = None;
        self.daemon_client_writer = None;
        self.bootstrap = None;
        self.last_cols = bounds.cols;
        self.last_rows = bounds.rows;
        self.terminal = terminal;
        self.pending = false;
        self.scroll_px = 0.0;
        self.pending_resize = None;
        self.pending_bounds = None;
        // Bumping the epochs invalidates any in-flight debounce/ack-timeout
        // callback that the prior bootstrap may have armed.
        let _ = self.bump_resize_debounce_epoch();
        let _ = self.bump_resize_ack_timeout_epoch();
        self.update_mouse_size_from_bounds();
        // Replaces any prior drain task — the old Task is dropped and cancels.
        self.spawn_data_drain(cx);
    }

    #[allow(clippy::too_many_arguments)]
    fn initialize_attached_terminal(
        &mut self,
        bounds: TerminalBounds,
        session_id: seoul_terminal_proto::session::SessionId,
        attached_msg: seoul_terminal_proto::messages::SessionAttachedMsg,
        data_rx: async_channel::Receiver<Vec<u8>>,
        resize_ack_rx: async_channel::Receiver<(u16, u16)>,
        writer: Box<dyn Write + Send>,
        daemon_client_writer: DaemonClientWriter,
        cx: &mut Context<Self>,
    ) {
        let trace_enabled = restore_trace_enabled();
        let started_at = Instant::now();
        let mut config = self.config.clone();
        config.cols = bounds.cols;
        config.rows = bounds.rows;

        let mut terminal = TerminalBuilder::new(config)
            .build_attached(writer)
            .expect("Failed to create attached terminal");
        if trace_enabled {
            tracing::info!(
                session_id = %session_id,
                elapsed_ms = started_at.elapsed().as_millis(),
                "restore trace: built attached ghostty terminal"
            );
        }
        let _ = terminal.resize(bounds);
        if trace_enabled {
            tracing::info!(
                session_id = %session_id,
                elapsed_ms = started_at.elapsed().as_millis(),
                cols = bounds.cols,
                rows = bounds.rows,
                "restore trace: resized attached ghostty terminal"
            );
        }

        if attached_msg.cols != bounds.cols || attached_msg.rows != bounds.rows {
            let pixel_w = (bounds.cols as f32 * bounds.cell_width).round() as u32;
            let pixel_h = (bounds.rows as f32 * bounds.line_height).round() as u32;
            daemon_client_writer.resize(session_id, bounds.cols, bounds.rows, pixel_w, pixel_h);
        }

        let replay_started_at = Instant::now();
        Self::replay_attached_state(&mut terminal, &attached_msg);
        if trace_enabled {
            tracing::info!(
                session_id = %session_id,
                replay_ms = replay_started_at.elapsed().as_millis(),
                elapsed_ms = started_at.elapsed().as_millis(),
                scrollback_bytes = attached_msg.scrollback_data.len(),
                rehydrate_bytes = attached_msg.rehydrate_sequences.len(),
                "restore trace: replayed attached terminal state"
            );
        }
        let mut received_live_data = false;
        while let Ok(data) = data_rx.try_recv() {
            terminal.feed_pty_data(&data);
            received_live_data = true;
        }
        if received_live_data {
            let live_sync_started_at = Instant::now();
            terminal.sync();
            if trace_enabled {
                tracing::info!(
                    session_id = %session_id,
                    live_sync_ms = live_sync_started_at.elapsed().as_millis(),
                    elapsed_ms = started_at.elapsed().as_millis(),
                    "restore trace: synced drained live data"
                );
            }
        }

        self.data_rx = data_rx;
        self.resize_ack_rx = resize_ack_rx;
        self.daemon_client_writer = Some(daemon_client_writer);
        self.bootstrap = None;
        self.last_cols = bounds.cols;
        self.last_rows = bounds.rows;
        self.terminal = terminal;
        self.pending = false;
        self.scroll_px = 0.0;
        self.pending_resize = None;
        self.pending_bounds = None;
        // Bumping the epochs invalidates any in-flight debounce/ack-timeout
        // callback that the prior bootstrap may have armed.
        let _ = self.bump_resize_debounce_epoch();
        let _ = self.bump_resize_ack_timeout_epoch();
        self.update_mouse_size_from_bounds();
        // Replaces any prior drain tasks — the old Tasks are dropped and cancel.
        self.spawn_data_drain(cx);
        self.spawn_resize_ack_drain(cx);
        if trace_enabled {
            tracing::info!(
                session_id = %session_id,
                elapsed_ms = started_at.elapsed().as_millis(),
                "restore trace: attached terminal ready"
            );
        }
    }

    fn bootstrap_from_bounds(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(bootstrap) = self.bootstrap.take() else {
            return false;
        };

        let Some(viewport) = self.element_bounds.get().map(|b| b.size) else {
            self.bootstrap = Some(bootstrap);
            return false;
        };
        let Some(bounds) = self.bounds_for_viewport(viewport) else {
            self.bootstrap = Some(bootstrap);
            return false;
        };

        match bootstrap {
            BootstrapState::Local { cwd } => {
                self.initialize_local_terminal(bounds, cwd, cx);
            }
            BootstrapState::Attached {
                session_id,
                attached_msg,
                data_rx,
                resize_ack_rx,
                writer,
                daemon_client_writer,
            } => {
                self.initialize_attached_terminal(
                    bounds,
                    session_id,
                    attached_msg,
                    data_rx,
                    resize_ack_rx,
                    writer,
                    daemon_client_writer,
                    cx,
                );
            }
        }
        true
    }

    fn replay_attached_state(
        terminal: &mut Terminal,
        attached_msg: &seoul_terminal_proto::messages::SessionAttachedMsg,
    ) {
        if !attached_msg.scrollback_data.is_empty() {
            terminal.feed_pty_data_silently(&attached_msg.scrollback_data);
            terminal.feed_pty_data_silently(b"\x1b[?25h");
        }

        if !attached_msg.rehydrate_sequences.is_empty() {
            terminal.feed_pty_data_silently(&attached_msg.rehydrate_sequences);
        }

        if attached_msg.was_recovered {
            if terminal.is_alternate_screen() {
                terminal.feed_pty_data_silently(b"\x1b[?1049l");
            }
            if terminal.is_bracketed_paste() {
                terminal.feed_pty_data_silently(b"\x1b[?2004l");
            }
            terminal.feed_pty_data_silently(b"\x1b[?9l");
            terminal.feed_pty_data_silently(b"\x1b[?1000l");
            terminal.feed_pty_data_silently(b"\x1b[?1002l");
            terminal.feed_pty_data_silently(b"\x1b[?1003l");
            terminal.feed_pty_data_silently(b"\x1b[?1004l");
            terminal.feed_pty_data_silently(b"\x1b[?1005l");
            terminal.feed_pty_data_silently(b"\x1b[?1006l");
            terminal.feed_pty_data_silently(b"\x1b[?1007h");
            terminal.feed_pty_data_silently(b"\x1b[?1015l");
            terminal.feed_pty_data_silently(b"\x1b[?1016l");
            terminal.feed_pty_data_silently(b"\x1b[?2026l");
            terminal.feed_pty_data_silently(b"\x1b[?2048l");
            terminal.feed_pty_data_silently(b"\x1b[>4m");
            terminal.feed_pty_data_silently(b"\x1b[<8u");
            terminal.feed_pty_data_silently(b"\x1b[=0;1u");
            terminal.feed_pty_data_silently(b"\x1b[?1l");
            terminal.feed_pty_data_silently(b"\x1b[?66l");
            terminal.feed_pty_data_silently(b"\x1b[?5l");
            terminal.feed_pty_data_silently(b"\x1b[?6l");
            terminal.feed_pty_data_silently(b"\x1b[?25h");
            let rows = terminal.last_content.terminal_bounds.rows.max(1);
            let live_prompt_boundary = format!("\x1b[0m\x1b[r\x1b[{};1H\r\n", rows);
            terminal.feed_pty_data_silently(live_prompt_boundary.as_bytes());
        }

        terminal.sync();
    }

    /// Attach a daemon session to a pending terminal view (in-place transition).
    pub fn attach_session(
        &mut self,
        daemon_client: &DaemonClient,
        session_handle: DaemonSessionHandle,
        cx: &mut Context<Self>,
    ) {
        let DaemonSessionHandle {
            session_id,
            data_rx,
            resize_ack_rx,
            attached_msg,
        } = session_handle;
        self.session_id = Some(session_id);
        self.pending_resize = None;
        self.pending_bounds = None;
        // Drop any in-flight resize timer state from a prior bootstrap so the
        // attached terminal starts with a clean slate. spawn_resize_ack_drain
        // is called from initialize_attached_terminal once bounds resolve.
        self.resize_ack_rx = Self::empty_ack_rx();
        self.resize_ack_drain_task = None;
        self.bootstrap = Some(BootstrapState::Attached {
            session_id,
            attached_msg,
            data_rx,
            resize_ack_rx,
            writer: Box::new(daemon_client.writer_for_session(session_id)),
            daemon_client_writer: daemon_client.clone_writer(),
        });
        if !self.bootstrap_from_bounds(cx) {
            if restore_trace_enabled() {
                tracing::info!(
                    session_id = %session_id,
                    "restore trace: attach waiting for terminal bounds"
                );
            }
            self.pending = true;
        }
        // Bootstrap the blink cycle: a pending view's constructor skipped
        // this since it had no live cursor, so the transition to attached
        // is the right place to start it.
        self.show_cursor_now(cx);
    }

    /// Create a terminal view attached to a daemon session.
    pub fn new_attached(
        window: &mut Window,
        cx: &mut Context<Self>,
        daemon_client: &DaemonClient,
        session_handle: DaemonSessionHandle,
    ) -> Self {
        let DaemonSessionHandle {
            session_id,
            data_rx,
            resize_ack_rx,
            attached_msg,
        } = session_handle;
        let config = Self::config_from_settings(cx);
        let terminal = Self::placeholder_terminal(&config);
        let data_placeholder = Self::empty_data_rx();
        let (cell_width, cell_height) = Self::measure_cells(window, &config);

        let focus_handle = cx.focus_handle();
        let _focus_subscriptions = [
            cx.on_focus_in(&focus_handle, window, |this, _window, cx| {
                if this.is_interactive() {
                    this.terminal.focus_in();
                }
                cx.notify();
            }),
            cx.on_focus_out(&focus_handle, window, |this, _event, _window, cx| {
                if this.is_interactive() {
                    this.terminal.focus_out();
                }
                cx.notify();
            }),
        ];

        let mut this = Self {
            terminal,
            data_rx: data_placeholder,
            focus_handle,
            _focus_subscriptions,
            last_cols: config.cols,
            last_rows: config.rows,
            cell_width,
            cell_height,
            config,
            cursor_visible: true,
            blink_epoch: 0,
            blink_paused_until: None,
            scroll_px: 0.0,
            scrollbar_visible: false,
            scrollbar_fade_epoch: 0,
            ime_preedit: String::new(),
            element_bounds: Rc::new(Cell::new(None)),
            mouse_buttons_pressed: MouseButtonsPressed::default(),
            session_id: Some(session_id),
            daemon_client_writer: None,
            bootstrap: Some(BootstrapState::Attached {
                session_id,
                attached_msg,
                data_rx,
                resize_ack_rx,
                writer: Box::new(daemon_client.writer_for_session(session_id)),
                daemon_client_writer: daemon_client.clone_writer(),
            }),
            pending: false,
            pending_resize: None,
            resize_debounce_epoch: 0,
            pending_bounds: None,
            resize_ack_timeout_epoch: 0,
            // The real resize_ack_rx lives inside `bootstrap` until
            // initialize_attached_terminal runs; until then this is a
            // closed-sender placeholder. spawn_resize_ack_drain is called
            // from initialize_attached_terminal once bounds resolve.
            resize_ack_rx: Self::empty_ack_rx(),
            render_cache: Rc::new(RefCell::new(TerminalRenderCache::default())),
            data_drain_task: None,
            resize_ack_drain_task: None,
        };
        this.show_cursor_now(cx);
        this
    }

    fn determine_scroll_lines(&mut self, e: &ScrollWheelEvent) -> i32 {
        let line_height = self.cell_height;
        let scroll_multiplier: f32 = 3.0;

        let pixel_y: f32 = match e.delta {
            ScrollDelta::Lines(pt) => pt.y * line_height,
            ScrollDelta::Pixels(pt) => f32::from(pt.y),
        };

        if matches!(e.touch_phase, TouchPhase::Started) {
            self.scroll_px = 0.0;
        }

        let old_offset = (self.scroll_px / line_height) as i32;
        self.scroll_px += pixel_y * scroll_multiplier;
        let new_offset = (self.scroll_px / line_height) as i32;

        // Reset at viewport boundary to stay responsive to direction changes
        let viewport_height = self.last_rows as f32 * line_height;
        if viewport_height > 0.0 {
            self.scroll_px %= viewport_height;
        }

        new_offset - old_offset
    }

    /// Pure trailing-edge debounce for resize events.
    ///
    /// Every new bounds update resets the debounce timer. The pending
    /// resize is only applied once this interval has elapsed without any
    /// further bounds change — i.e., the user has stopped dragging.
    ///
    /// Rationale: ghostty.resize() reflows the entire scrollback (~5-7 ms
    /// per call on a 10k-line history). With N visible panes all firing
    /// during window drag, throttling doesn't help enough — the floor is
    /// N × 5 ms × rate, and any non-zero rate saturates the main thread.
    /// Pure debounce produces zero main-thread resize work during the
    /// drag; the grid catches up within RESIZE_DEBOUNCE ms after the user
    /// stops.
    ///
    /// 200 ms is chosen to ride out direction-reversal pauses during
    /// "back-and-forth" drag gestures. A human wrist deceleration +
    /// re-acceleration takes 20–120 ms; 60 ms catches many of those and
    /// misfires → each misfire queues a resize → daemon redraws shell →
    /// data backlog cascades into frame-level lag. 200 ms is safely above
    /// the reversal window, so apply_resize fires zero times during
    /// continuous drag regardless of direction changes. Trade-off:
    /// end-of-drag catch-up latency is ~216 ms (200 + one frame ≈ 16 ms).
    const RESIZE_DEBOUNCE: Duration = Duration::from_millis(200);

    /// Fallback timeout for daemon ResizeAck — if ACK is lost, apply local
    /// resize after this delay to avoid permanently stale dimensions.
    const RESIZE_ACK_TIMEOUT: Duration = Duration::from_millis(500);

    /// Cursor blink half-period: cursor toggles every BLINK_INTERVAL.
    const BLINK_INTERVAL: Duration = Duration::from_millis(500);

    /// Pause blinking for this duration after user-perceptible activity
    /// (keystroke, scroll, PTY output). Cursor stays visible during the
    /// pause; the next toggle fires once the pause window has elapsed.
    const BLINK_PAUSE: Duration = Duration::from_millis(500);

    /// Auto-hide the scrollbar this long after the last scroll event. Each
    /// scroll bumps the fade epoch and arms a fresh one-shot timer; stale
    /// timers from earlier scrolls discriminate via the epoch and no-op.
    const SCROLLBAR_FADE: Duration = Duration::from_millis(1500);

    /// Called from the ime_canvas paint closure (deferred via `cx.spawn`)
    /// when the element's bounds change. Replaces the per-frame
    /// `element_bounds` poll that lived in `tick()`. Handles three cases:
    ///
    /// 1. Bootstrap not yet complete — try `bootstrap_from_bounds`. If it
    ///    still can't resolve (e.g. zero size during initial layout),
    ///    `bootstrap` stays `Some` and the next paint-time bounds change
    ///    will retry. On a successful bootstrap we sync and notify so
    ///    the first real frame paints.
    /// 2. Bootstrap complete, not pending — run `check_resize`. If the
    ///    geometry changed, arm the debounce flush; the actual resize is
    ///    applied by `flush_resize_debounce` after `RESIZE_DEBOUNCE`
    ///    quiet, not here.
    /// 3. Pending session — skip resize work; the attach path will
    ///    trigger bounds re-evaluation via the next paint when it
    ///    transitions to active.
    fn on_bounds_changed(&mut self, cx: &mut Context<Self>) {
        let resized = self.bootstrap_from_bounds(cx);
        if self.bootstrap.is_some() {
            return;
        }
        if !self.pending {
            let Some(size) = self.element_bounds.get().map(|b| b.size) else {
                return;
            };
            self.update_mouse_size_from_bounds();
            if self.check_resize(size) {
                self.schedule_resize_debounce_flush(cx);
            }
        }
        if resized {
            self.terminal.sync();
            if self.scrollbar_visible {
                self.terminal.update_scrollbar();
            }
            cx.notify();
        }
    }

    /// Apply a resize to both local terminal and daemon PTY.
    ///
    /// In daemon mode this only sends the Resize message and arms an
    /// ACK-timeout fallback timer; the local terminal grid is not resized
    /// until either `apply_resize_ack` runs (success path) or
    /// `fire_resize_ack_timeout` runs (lost-ACK fallback). In local mode
    /// this resizes the terminal synchronously since there's no daemon
    /// round-trip.
    fn apply_resize(&mut self, cols: u16, rows: u16, cx: &mut Context<Self>) {
        let cw = self.cell_width;
        let ch = self.cell_height;
        let bounds = TerminalBounds {
            cols,
            rows,
            cell_width: cw,
            line_height: ch,
        };

        if self.session_id.is_some() {
            // Daemon mode: send resize to daemon, wait for ResizeAck before
            // applying local ghostty resize (fallback timeout as safety net).
            if let (Some(session_id), Some(writer)) = (self.session_id, &self.daemon_client_writer)
            {
                let pixel_w = (cols as f32 * cw).round() as u32;
                let pixel_h = (rows as f32 * ch).round() as u32;
                writer.resize(session_id, cols, rows, pixel_w, pixel_h);
            }
            self.pending_bounds = Some(bounds);
            self.arm_resize_ack_timeout(cx);
        } else {
            // Local PTY mode: resize immediately (no race condition)
            let _ = self.terminal.resize(bounds);
        }
    }

    /// Called when daemon sends ResizeAck — apply pending local resize immediately.
    ///
    /// With resize debouncing, multiple Resize frames may still be in flight
    /// before any ACK arrives (e.g. after a mid-drag debounce trip). ACKs for
    /// stale (earlier) Resizes don't match the latest `pending_bounds` and
    /// must NOT clear the pending state — otherwise the local grid never
    /// catches up to the drag target. Only the ACK that matches the current
    /// `pending_bounds` applies the resize and clears the pending state;
    /// the fallback timeout is the safety net if the matching ACK is lost.
    ///
    /// On a matching ACK we bump `resize_ack_timeout_epoch` so any in-flight
    /// timeout callback becomes stale. The would-be fallback would no-op
    /// anyway (pending_bounds is None), but bumping is cheap and removes
    /// any chance of a future re-arm sliding under a stale callback.
    pub fn apply_resize_ack(&mut self, cols: u16, rows: u16) {
        let Some(bounds) = self.pending_bounds else {
            return;
        };
        if bounds.cols != cols || bounds.rows != rows {
            return;
        }
        self.pending_bounds = None;
        let _ = self.bump_resize_ack_timeout_epoch();
        let _ = self.terminal.resize(bounds);
        self.update_mouse_size_from_bounds();
    }

    fn check_resize(&mut self, viewport: Size<Pixels>) -> bool {
        let Some(bounds) = self.bounds_for_viewport(viewport) else {
            return false;
        };
        let new_cols = bounds.cols;
        let new_rows = bounds.rows;

        if new_cols == self.last_cols && new_rows == self.last_rows {
            return false;
        }
        self.last_cols = new_cols;
        self.last_rows = new_rows;

        // Pure trailing-edge debounce: never apply during active resize.
        // Every new bounds update resets the debounce timer (the caller is
        // expected to call schedule_resize_debounce_flush whenever this
        // returns true); the latest debounce timer flushes pending_resize
        // after RESIZE_DEBOUNCE of quiet.
        //
        // Rationale: ghostty.resize() reflows scrollback (~5-7 ms/call on
        // a 10k-line history). During a window drag, applying this on
        // every frame saturates the main thread. Debouncing produces zero
        // main-thread resize work during drag; the terminal catches up
        // within RESIZE_DEBOUNCE ms after the user stops.
        self.pending_resize = Some((new_cols, new_rows));
        true
    }

    fn bump_resize_debounce_epoch(&mut self) -> u64 {
        self.resize_debounce_epoch += 1;
        self.resize_debounce_epoch
    }

    fn bump_resize_ack_timeout_epoch(&mut self) -> u64 {
        self.resize_ack_timeout_epoch += 1;
        self.resize_ack_timeout_epoch
    }

    /// Arm the debounce timer that eventually flushes `pending_resize`.
    ///
    /// Called whenever `check_resize` reports a geometry change. Bumps the
    /// debounce epoch so any in-flight debounce timer becomes stale and
    /// no-ops on its callback, then schedules a fresh one-shot timer that
    /// fires after `RESIZE_DEBOUNCE` of quiet. Only the latest timer
    /// (matching the current epoch) actually applies the resize; earlier
    /// timers exit on epoch mismatch.
    ///
    /// Detached tasks are safe here: the epoch counter discriminates
    /// stale callbacks, and view drop turns `upgrade()` into None.
    fn schedule_resize_debounce_flush(&mut self, cx: &mut Context<Self>) {
        let epoch = self.bump_resize_debounce_epoch();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Self::RESIZE_DEBOUNCE).await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| this.flush_resize_debounce(epoch, cx));
            }
        })
        .detach();
    }

    /// Flush the latest `pending_resize` if this callback is still current.
    ///
    /// Stale (superseded) timers no-op via the epoch check. In daemon
    /// mode `apply_resize` arms the ACK-timeout fallback and the ACK
    /// drain triggers a repaint when the matching ACK arrives, so we
    /// don't notify here. In local mode `apply_resize` resizes the
    /// terminal synchronously, so we notify to schedule a repaint.
    fn flush_resize_debounce(&mut self, epoch: u64, cx: &mut Context<Self>) {
        if epoch != self.resize_debounce_epoch {
            return;
        }
        let Some((cols, rows)) = self.pending_resize.take() else {
            return;
        };
        let was_local = self.session_id.is_none();
        self.apply_resize(cols, rows, cx);
        if was_local {
            cx.notify();
        }
    }

    /// Arm the fallback timer that applies a pending daemon resize locally
    /// if the matching ResizeAck never arrives.
    ///
    /// Bumps `resize_ack_timeout_epoch` so any in-flight timeout callback
    /// from a prior `apply_resize` becomes stale; only the latest timer
    /// can fire `fire_resize_ack_timeout`. Mirrors the blink/scrollbar
    /// epoch+detached-task pattern.
    fn arm_resize_ack_timeout(&mut self, cx: &mut Context<Self>) {
        let epoch = self.bump_resize_ack_timeout_epoch();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Self::RESIZE_ACK_TIMEOUT)
                .await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| this.fire_resize_ack_timeout(epoch, cx));
            }
        })
        .detach();
    }

    /// Apply the pending bounds locally because the daemon never ACKed.
    ///
    /// Stale timers (superseded by a newer apply_resize, or invalidated by
    /// a successful apply_resize_ack which bumps the epoch) no-op via the
    /// epoch check. If `pending_bounds` is still set when we fire, the
    /// matching ACK was lost; we resize the local grid as a fallback so
    /// the view doesn't stay stuck at the pre-resize dimensions.
    fn fire_resize_ack_timeout(&mut self, epoch: u64, cx: &mut Context<Self>) {
        if epoch != self.resize_ack_timeout_epoch {
            return;
        }
        let Some(bounds) = self.pending_bounds.take() else {
            return;
        };
        tracing::debug!(
            cols = bounds.cols,
            rows = bounds.rows,
            "resize ack timeout fired; applying local resize as fallback"
        );
        let _ = self.terminal.resize(bounds);
        self.update_mouse_size_from_bounds();
        cx.notify();
    }

    fn update_mouse_size_from_bounds(&mut self) {
        let Some(bounds) = self.element_bounds.get() else {
            return;
        };
        self.terminal
            .set_mouse_size(Self::mouse_encoder_size_for_content_bounds(
                bounds.size,
                self.cell_width,
                self.cell_height,
            ));
        self.terminal.set_mouse_track_last_cell(true);
    }

    fn mouse_encoder_size_for_content_bounds(
        size: Size<Pixels>,
        cell_width: f32,
        cell_height: f32,
    ) -> mouse::EncoderSize {
        mouse::EncoderSize {
            screen_width: f32::from(size.width).round().max(1.0) as u32,
            screen_height: f32::from(size.height).round().max(1.0) as u32,
            cell_width: cell_width.round().max(1.0) as u32,
            cell_height: cell_height.round().max(1.0) as u32,
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        }
    }

    /// Spawn the ResizeAck drain task.
    ///
    /// Takes ownership of the current `resize_ack_rx` (replacing it with a
    /// closed-sender placeholder; nothing reads from it after the swap)
    /// and runs an event loop that wakes only when an ACK arrives. Each
    /// ACK is forwarded to `apply_resize_ack` on the main thread, which
    /// applies the pending local resize iff the ACK matches the latest
    /// pending bounds.
    ///
    /// Only spawned for daemon-attached sessions — local-PTY mode has no
    /// ACK protocol, so its `resize_ack_rx` stays a closed-sender placeholder
    /// and `recv().await` would return `Err` immediately. The returned
    /// `Task<()>` is stored in `resize_ack_drain_task`; dropping the view
    /// (or replacing the task) cancels it.
    fn spawn_resize_ack_drain(&mut self, cx: &mut Context<Self>) {
        let ack_rx = std::mem::replace(&mut self.resize_ack_rx, Self::empty_ack_rx());
        self.resize_ack_drain_task = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            loop {
                let (cols, rows) = match ack_rx.recv().await {
                    Ok(ack) => ack,
                    Err(_) => break,
                };
                if this
                    .update(cx, |this, cx| {
                        this.apply_resize_ack(cols, rows);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn bump_blink_epoch(&mut self) -> u64 {
        self.blink_epoch += 1;
        self.blink_epoch
    }

    /// Show cursor immediately and pause blinking for `BLINK_PAUSE`.
    ///
    /// Called from any user-perceptible activity: keystrokes, scroll,
    /// PTY output, IME composition. Bumps the epoch so any in-flight
    /// blink timer becomes stale and no-ops on its callback, then
    /// schedules a fresh blink cycle to start after `BLINK_PAUSE`.
    ///
    /// Detached tasks are safe here: the epoch counter discriminates
    /// stale callbacks, and view drop turns `upgrade()` into None.
    fn show_cursor_now(&mut self, cx: &mut Context<Self>) {
        if !self.cursor_visible {
            self.cursor_visible = true;
            cx.notify();
        }
        self.blink_paused_until = Some(Instant::now() + Self::BLINK_PAUSE);
        let epoch = self.bump_blink_epoch();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Self::BLINK_PAUSE).await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| this.tick_blink(epoch, cx));
            }
        })
        .detach();
    }

    /// Toggle cursor visibility and reschedule the next toggle.
    ///
    /// `epoch` is the epoch this callback was scheduled with; if the
    /// view's current epoch has advanced (e.g. another `show_cursor_now`
    /// fired in the meantime), this callback is stale and exits.
    fn tick_blink(&mut self, epoch: u64, cx: &mut Context<Self>) {
        if epoch != self.blink_epoch {
            return;
        }
        if let Some(until) = self.blink_paused_until
            && Instant::now() < until
        {
            return;
        }
        // Apps that disabled blinking (vim normal mode, fullscreen TUIs) leave
        // the cursor steady. Skip both the toggle and the timer reschedule so
        // an idle terminal in such an app reaches true 0 wakeups — the next
        // show_cursor_now (any input or PTY activity) will restart the cycle.
        if !self.terminal.last_content.cursor.blinking {
            return;
        }
        self.cursor_visible = !self.cursor_visible;
        cx.notify();
        let next_epoch = self.bump_blink_epoch();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Self::BLINK_INTERVAL).await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| this.tick_blink(next_epoch, cx));
            }
        })
        .detach();
    }

    fn bump_scrollbar_fade_epoch(&mut self) -> u64 {
        self.scrollbar_fade_epoch += 1;
        self.scrollbar_fade_epoch
    }

    /// Show the scrollbar and schedule a fade-out timer.
    ///
    /// Each call bumps the fade epoch so any in-flight fade timer from a
    /// previous scroll becomes stale and no-ops when it fires. Called from
    /// every user-initiated scroll event. PTY output that grows scrollback
    /// must NOT call this — fade is for user scroll only.
    ///
    /// Detached tasks are safe here for the same reason as `show_cursor_now`:
    /// the epoch counter discriminates stale callbacks, and view drop turns
    /// `upgrade()` into None.
    fn poke_scrollbar(&mut self, cx: &mut Context<Self>) {
        if !self.scrollbar_visible {
            self.scrollbar_visible = true;
            cx.notify();
        }
        let epoch = self.bump_scrollbar_fade_epoch();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Self::SCROLLBAR_FADE).await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| {
                    if epoch == this.scrollbar_fade_epoch && this.scrollbar_visible {
                        this.scrollbar_visible = false;
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    fn paste(&mut self, _: &Paste, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_interactive() {
            return;
        }
        if let Some(item) = cx.read_from_clipboard()
            && let Some(text) = item.text()
        {
            self.show_cursor_now(cx);
            self.terminal.paste(&text);
        }
    }

    fn copy(&mut self, _: &Copy, _window: &mut Window, _cx: &mut Context<Self>) {
        // TODO: implement text selection + copy
    }

    fn mouse_mods(modifiers: gpui::Modifiers) -> key::Mods {
        let mut mods = key::Mods::empty();
        if modifiers.shift {
            mods |= key::Mods::SHIFT;
        }
        if modifiers.control {
            mods |= key::Mods::CTRL;
        }
        if modifiers.alt {
            mods |= key::Mods::ALT;
        }
        if modifiers.platform {
            mods |= key::Mods::SUPER;
        }
        mods
    }

    fn ghostty_mouse_button(button: MouseButton) -> Option<mouse::Button> {
        match button {
            MouseButton::Left => Some(mouse::Button::Left),
            MouseButton::Right => Some(mouse::Button::Right),
            MouseButton::Middle => Some(mouse::Button::Middle),
            _ => None,
        }
    }

    fn terminal_surface_position(
        position: gpui::Point<Pixels>,
        bounds: Option<Bounds<Pixels>>,
    ) -> Option<(f32, f32)> {
        let origin = bounds?.origin;
        Some((
            f32::from(position.x - origin.x),
            f32::from(position.y - origin.y),
        ))
    }

    fn on_terminal_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        if !self.is_interactive() {
            return;
        }
        let Some(button) = Self::ghostty_mouse_button(event.button) else {
            return;
        };
        self.mouse_buttons_pressed.set(event.button, true);
        self.terminal
            .set_mouse_any_button_pressed(self.mouse_buttons_pressed.any());
        self.terminal.set_mouse_track_last_cell(true);
        if self.terminal.is_mouse_tracking() {
            let Some((pos_x, pos_y)) =
                Self::terminal_surface_position(event.position, self.element_bounds.get())
            else {
                return;
            };
            self.terminal.send_mouse_event(
                mouse::Action::Press,
                Some(button),
                Self::mouse_mods(event.modifiers),
                pos_x,
                pos_y,
            );
            self.show_cursor_now(cx);
            cx.notify();
        }
    }

    fn on_terminal_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_interactive() {
            return;
        }
        let Some(button) = Self::ghostty_mouse_button(event.button) else {
            return;
        };
        self.mouse_buttons_pressed.set(event.button, false);
        let any_pressed = self.mouse_buttons_pressed.any();
        self.terminal.set_mouse_any_button_pressed(any_pressed);
        self.terminal.set_mouse_track_last_cell(true);
        if self.terminal.is_mouse_tracking() {
            let Some((pos_x, pos_y)) =
                Self::terminal_surface_position(event.position, self.element_bounds.get())
            else {
                return;
            };
            self.terminal.send_mouse_event(
                mouse::Action::Release,
                Some(button),
                Self::mouse_mods(event.modifiers),
                pos_x,
                pos_y,
            );
            self.show_cursor_now(cx);
            cx.notify();
        }
    }

    fn on_terminal_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_interactive() || !self.terminal.is_mouse_tracking() {
            return;
        }
        let any_pressed = self.mouse_buttons_pressed.any();
        self.terminal.set_mouse_any_button_pressed(any_pressed);
        self.terminal.set_mouse_track_last_cell(true);
        let button = event.pressed_button.and_then(Self::ghostty_mouse_button);
        let Some((pos_x, pos_y)) =
            Self::terminal_surface_position(event.position, self.element_bounds.get())
        else {
            return;
        };
        self.terminal.send_mouse_event(
            mouse::Action::Motion,
            button,
            Self::mouse_mods(event.modifiers),
            pos_x,
            pos_y,
        );
        if any_pressed {
            self.show_cursor_now(cx);
            cx.notify();
        }
    }

    /// Map a GPUI keystroke to libghostty key + mods.
    ///
    /// Only handles special keys and modifier combos (Ctrl+X, Alt+X, Cmd+Arrow).
    /// Regular printable characters (no Ctrl/Alt) are left to the IME
    /// InputHandler path (replace_text_in_range) to avoid double input.
    fn map_keystroke(keystroke: &Keystroke) -> Option<MappedKeystroke> {
        let has_ctrl = keystroke.modifiers.control;
        let has_alt = keystroke.modifiers.alt;
        let has_platform = keystroke.modifiers.platform;

        if has_platform && !has_ctrl && !has_alt && !keystroke.modifiers.shift {
            match keystroke.key.as_str() {
                "left" | "up" => return Some(MappedKeystroke::Raw(b"\x01")),
                "right" | "down" => return Some(MappedKeystroke::Raw(b"\x05")),
                _ => {}
            }
        }

        let mut mods = gkey::Mods::empty();
        if has_ctrl {
            mods |= gkey::Mods::CTRL;
        }
        if has_alt {
            mods |= gkey::Mods::ALT;
        }
        if has_platform {
            mods |= gkey::Mods::SUPER;
        }
        if keystroke.modifiers.shift {
            mods |= gkey::Mods::SHIFT;
        }

        // Special keys — always handle in on_key_down
        let mut unshifted = None;
        let key = match keystroke.key.as_str() {
            "enter" => gkey::Key::Enter,
            "backspace" => gkey::Key::Backspace,
            "tab" => gkey::Key::Tab,
            "escape" => gkey::Key::Escape,
            "insert" => gkey::Key::Insert,
            "up" => gkey::Key::ArrowUp,
            "down" => gkey::Key::ArrowDown,
            "left" => gkey::Key::ArrowLeft,
            "right" => gkey::Key::ArrowRight,
            "home" => gkey::Key::Home,
            "end" => gkey::Key::End,
            "delete" => gkey::Key::Delete,
            "pageup" => gkey::Key::PageUp,
            "pagedown" => gkey::Key::PageDown,
            "space" if has_ctrl || has_alt => gkey::Key::Space,
            "f1" => gkey::Key::F1,
            "f2" => gkey::Key::F2,
            "f3" => gkey::Key::F3,
            "f4" => gkey::Key::F4,
            "f5" => gkey::Key::F5,
            "f6" => gkey::Key::F6,
            "f7" => gkey::Key::F7,
            "f8" => gkey::Key::F8,
            "f9" => gkey::Key::F9,
            "f10" => gkey::Key::F10,
            "f11" => gkey::Key::F11,
            "f12" => gkey::Key::F12,
            "f13" => gkey::Key::F13,
            "f14" => gkey::Key::F14,
            "f15" => gkey::Key::F15,
            "f16" => gkey::Key::F16,
            "f17" => gkey::Key::F17,
            "f18" => gkey::Key::F18,
            "f19" => gkey::Key::F19,
            "f20" => gkey::Key::F20,
            "f21" => gkey::Key::F21,
            "f22" => gkey::Key::F22,
            "f23" => gkey::Key::F23,
            "f24" => gkey::Key::F24,
            "f25" => gkey::Key::F25,
            "numpad0" => gkey::Key::Numpad0,
            "numpad1" => gkey::Key::Numpad1,
            "numpad2" => gkey::Key::Numpad2,
            "numpad3" => gkey::Key::Numpad3,
            "numpad4" => gkey::Key::Numpad4,
            "numpad5" => gkey::Key::Numpad5,
            "numpad6" => gkey::Key::Numpad6,
            "numpad7" => gkey::Key::Numpad7,
            "numpad8" => gkey::Key::Numpad8,
            "numpad9" => gkey::Key::Numpad9,
            "numpad_add" | "numpadadd" => gkey::Key::NumpadAdd,
            "numpad_subtract" | "numpadsubtract" => gkey::Key::NumpadSubtract,
            "numpad_multiply" | "numpadmultiply" => gkey::Key::NumpadMultiply,
            "numpad_divide" | "numpaddivide" => gkey::Key::NumpadDivide,
            "numpad_decimal" | "numpaddecimal" => gkey::Key::NumpadDecimal,
            "numpad_enter" | "numpadenter" => gkey::Key::NumpadEnter,
            "numpad_backspace" | "numpadbackspace" => gkey::Key::NumpadBackspace,
            "numpad_up" | "numpadup" => gkey::Key::NumpadUp,
            "numpad_down" | "numpaddown" => gkey::Key::NumpadDown,
            "numpad_left" | "numpadleft" => gkey::Key::NumpadLeft,
            "numpad_right" | "numpadright" => gkey::Key::NumpadRight,
            // Single-char keys: only handle with Ctrl or Alt modifier.
            // Without modifiers, let the IME InputHandler send the character.
            s if s.chars().count() == 1 && (has_ctrl || has_alt) => {
                let ch = s.chars().next().unwrap();
                unshifted = Some(if ch.is_ascii() {
                    ch.to_ascii_lowercase()
                } else {
                    ch
                });
                match ch {
                    'a' | 'A' => gkey::Key::A,
                    'b' | 'B' => gkey::Key::B,
                    'c' | 'C' => gkey::Key::C,
                    'd' | 'D' => gkey::Key::D,
                    'e' | 'E' => gkey::Key::E,
                    'f' | 'F' => gkey::Key::F,
                    'g' | 'G' => gkey::Key::G,
                    'h' | 'H' => gkey::Key::H,
                    'i' | 'I' => gkey::Key::I,
                    'j' | 'J' => gkey::Key::J,
                    'k' | 'K' => gkey::Key::K,
                    'l' | 'L' => gkey::Key::L,
                    'm' | 'M' => gkey::Key::M,
                    'n' | 'N' => gkey::Key::N,
                    'o' | 'O' => gkey::Key::O,
                    'p' | 'P' => gkey::Key::P,
                    'q' | 'Q' => gkey::Key::Q,
                    'r' | 'R' => gkey::Key::R,
                    's' | 'S' => gkey::Key::S,
                    't' | 'T' => gkey::Key::T,
                    'u' | 'U' => gkey::Key::U,
                    'v' | 'V' => gkey::Key::V,
                    'w' | 'W' => gkey::Key::W,
                    'x' | 'X' => gkey::Key::X,
                    'y' | 'Y' => gkey::Key::Y,
                    'z' | 'Z' => gkey::Key::Z,
                    '0' => gkey::Key::Digit0,
                    '1' => gkey::Key::Digit1,
                    '2' => gkey::Key::Digit2,
                    '3' => gkey::Key::Digit3,
                    '4' => gkey::Key::Digit4,
                    '5' => gkey::Key::Digit5,
                    '6' => gkey::Key::Digit6,
                    '7' => gkey::Key::Digit7,
                    '8' => gkey::Key::Digit8,
                    '9' => gkey::Key::Digit9,
                    '`' => gkey::Key::Backquote,
                    '-' => gkey::Key::Minus,
                    '=' => gkey::Key::Equal,
                    '[' => gkey::Key::BracketLeft,
                    ']' => gkey::Key::BracketRight,
                    '\\' => gkey::Key::Backslash,
                    ';' => gkey::Key::Semicolon,
                    '\'' => gkey::Key::Quote,
                    ',' => gkey::Key::Comma,
                    '.' => gkey::Key::Period,
                    '/' => gkey::Key::Slash,
                    _ => return None,
                }
            }
            _ => return None,
        };

        let utf8 = keystroke.key_char.as_ref().map(|s| s.to_string());
        Some(MappedKeystroke::Encoded {
            key,
            mods,
            utf8,
            unshifted,
        })
    }

    fn dispatch_mapped_keystroke(
        terminal: &mut Terminal,
        mapped: MappedKeystroke,
        ime_preedit: &mut String,
    ) -> bool {
        if !ime_preedit.is_empty() {
            let text = std::mem::take(ime_preedit);
            terminal.input(text.as_bytes());
        }

        match mapped {
            MappedKeystroke::Encoded {
                key,
                mods,
                utf8,
                unshifted,
            } => terminal.try_keystroke(key, mods, utf8.as_deref(), unshifted),
            MappedKeystroke::Raw(bytes) => {
                terminal.input(bytes);
                true
            }
        }
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl crate::item::Item for TerminalView {
    fn tab_title(&self, _cx: &App) -> String {
        "Terminal".into()
    }

    fn tab_kind(&self) -> crate::tab_kind::TabKind {
        crate::tab_kind::TabKind::Terminal
    }
}

struct TerminalInputHandler {
    view: Entity<TerminalView>,
    cursor_bounds: Option<Bounds<Pixels>>,
}

impl InputHandler for TerminalInputHandler {
    fn selected_text_range(
        &mut self,
        _ignore: bool,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(&mut self, _window: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        let view = self.view.read(cx);
        if view.ime_preedit.is_empty() {
            None
        } else {
            Some(0..view.ime_preedit.encode_utf16().count())
        }
    }

    fn text_for_range(
        &mut self,
        _range: Range<usize>,
        _adjusted: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<String> {
        None
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut App,
    ) {
        self.view.update(cx, |view, cx| {
            if !view.is_interactive() {
                view.ime_preedit.clear();
                cx.notify();
                return;
            }
            view.ime_preedit.clear();
            if !text.is_empty() {
                view.show_cursor_now(cx);
                view.terminal.input(text.as_bytes());
            }
            cx.notify();
        });
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut App,
    ) {
        self.view.update(cx, |view, cx| {
            if !view.is_interactive() {
                view.ime_preedit.clear();
                cx.notify();
                return;
            }
            view.ime_preedit = new_text.to_string();
            view.show_cursor_now(cx);
            cx.notify();
        });
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut App) {
        self.view.update(cx, |view, cx| {
            if !view.is_interactive() {
                view.ime_preedit.clear();
                cx.notify();
                return;
            }
            if !view.ime_preedit.is_empty() {
                let text = std::mem::take(&mut view.ime_preedit);
                view.show_cursor_now(cx);
                view.terminal.input(text.as_bytes());
            }
            cx.notify();
        });
    }

    fn bounds_for_range(
        &mut self,
        _range: Range<usize>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        self.cursor_bounds
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<usize> {
        None
    }

    fn apple_press_and_hold_enabled(&mut self) -> bool {
        false
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = &self.terminal.last_content;
        let config = &self.config;
        let cw = self.cell_width;
        let ch = self.cell_height;
        let cursor_visible = if content.cursor.blinking {
            self.cursor_visible
        } else {
            true
        };

        // IME input handler canvas
        let focus = self.focus_handle.clone();
        let view_entity = cx.entity();
        let weak_view = view_entity.downgrade();
        let bounds_cell = self.element_bounds.clone();
        let cursor_col = content.cursor.col;
        let cursor_row = content.cursor.row;
        let cw_ime = cw;
        let ch_ime = ch;
        let ime_canvas = canvas(
            move |_bounds, _window, _cx| {},
            move |bounds, _, window, cx| {
                // Capture element bounds for resize (no Entity access to avoid re-entrancy).
                // When the size actually changes, defer-spawn an entity update on the
                // view so on_bounds_changed runs after the paint phase completes;
                // we can't re-enter the view from inside paint.
                let prev = bounds_cell.replace(Some(bounds));
                if prev.map(|b| b.size) != Some(bounds.size) {
                    let weak = weak_view.clone();
                    cx.spawn(async move |cx| {
                        weak.update(cx, |this, cx| this.on_bounds_changed(cx)).ok();
                    })
                    .detach();
                }
                let cursor_x = bounds.origin.x + px(cursor_col as f32 * cw_ime);
                let cursor_y = bounds.origin.y + px(cursor_row as f32 * ch_ime);
                let cursor_bounds =
                    Bounds::new(point(cursor_x, cursor_y), size(px(cw_ime), px(ch_ime)));
                let input_handler = TerminalInputHandler {
                    view: view_entity,
                    cursor_bounds: Some(cursor_bounds),
                };
                window.handle_input(&focus, input_handler, cx);
            },
        )
        .absolute()
        .size_full();

        // Build the terminal canvas
        let terminal_canvas = render_terminal(
            content,
            config,
            cw,
            ch,
            cursor_visible,
            self.scrollbar_visible,
            &self.render_cache,
        );

        // IME preedit overlay
        let preedit_overlay = if !self.ime_preedit.is_empty() {
            let cursor_x = cursor_col as f32 * cw;
            let cursor_y = cursor_row as f32 * ch;
            let preedit_text = self.ime_preedit.clone();
            let cursor_bg = config.theme.cursor.to_u32();
            let bg = config.theme.background.to_u32();

            Some(
                anchored()
                    .position(point(px(cursor_x), px(cursor_y)))
                    .position_mode(AnchoredPositionMode::Local)
                    .child(
                        div()
                            .bg(rgb(cursor_bg))
                            .text_color(rgb(bg))
                            .underline()
                            .child(preedit_text),
                    ),
            )
        } else {
            None
        };

        let bg_hex = config.theme.background.to_u32();

        div()
            .id("terminal")
            .key_context("terminal")
            .track_focus(&self.focus_handle)
            .size_full()
            .overflow_hidden()
            .flex()
            .flex_col()
            .bg(rgb(bg_hex))
            .p(px(config.padding))
            .font_family(config.font_family.clone())
            .text_size(px(config.font_size))
            .line_height(px(ch))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::copy))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_terminal_mouse_down))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(Self::on_terminal_mouse_down),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(Self::on_terminal_mouse_down),
            )
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_terminal_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_terminal_mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_terminal_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_terminal_mouse_up))
            .on_mouse_up_out(MouseButton::Right, cx.listener(Self::on_terminal_mouse_up))
            .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::on_terminal_mouse_up))
            .on_mouse_move(cx.listener(Self::on_terminal_mouse_move))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                if !this.is_interactive() {
                    return;
                }
                if let Some(mapped) = Self::map_keystroke(&event.keystroke) {
                    this.show_cursor_now(cx);
                    if Self::dispatch_mapped_keystroke(
                        &mut this.terminal,
                        mapped,
                        &mut this.ime_preedit,
                    ) {
                        cx.stop_propagation();
                    }
                }
            }))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
                if !this.is_interactive() {
                    return;
                }
                let delta_lines = this.determine_scroll_lines(event);
                if delta_lines == 0 {
                    return;
                }

                if this.terminal.is_mouse_tracking() {
                    // Mouse tracking mode: send scroll as button 4/5 press+release
                    let (button, count) = if delta_lines > 0 {
                        (mouse::Button::Four, delta_lines) // scroll up
                    } else {
                        (mouse::Button::Five, -delta_lines) // scroll down
                    };
                    let Some((pos_x, pos_y)) =
                        Self::terminal_surface_position(event.position, this.element_bounds.get())
                    else {
                        return;
                    };
                    for _ in 0..count.min(5) {
                        this.terminal.send_mouse_event(
                            mouse::Action::Press,
                            Some(button),
                            key::Mods::empty(),
                            pos_x,
                            pos_y,
                        );
                        this.terminal.send_mouse_event(
                            mouse::Action::Release,
                            Some(button),
                            key::Mods::empty(),
                            pos_x,
                            pos_y,
                        );
                    }
                    this.show_cursor_now(cx);
                    cx.notify();
                } else if this.terminal.is_alternate_screen() && this.terminal.is_alt_scroll() {
                    // Alt screen + alt scroll: send arrow keys
                    let arrow = if delta_lines > 0 {
                        b"\x1b[A" // up arrow
                    } else {
                        b"\x1b[B" // down arrow
                    };
                    for _ in 0..delta_lines.unsigned_abs().min(5) {
                        this.terminal.input_raw(arrow);
                    }
                    this.show_cursor_now(cx);
                    cx.notify();
                } else {
                    // Normal: scroll viewport. Sync immediately since tick()
                    // no longer exists. We still call cx.notify() so GPUI's
                    // InputRateTracker counts this scroll toward "high-rate
                    // input" (it gates on invalidator.update_count growing
                    // during dispatch). That keeps ProMotion at full rate
                    // during inertial scroll and for 1 s after release,
                    // instead of underclocking.
                    this.terminal
                        .scroll(ScrollViewport::Delta(-(delta_lines as isize)));
                    this.terminal.sync();
                    if this.scrollbar_visible {
                        this.terminal.update_scrollbar();
                    }
                    this.poke_scrollbar(cx);
                    this.show_cursor_now(cx);
                    cx.notify();
                }
            }))
            .child(ime_canvas)
            .child(terminal_canvas)
            .children(preedit_overlay)
            .when(self.pending, |el: Stateful<Div>| {
                el.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .bg(rgb(bg_hex))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .text_size(px(13.))
                                .text_color(rgb(config.theme.foreground.to_u32()))
                                .opacity(0.4)
                                .child("Restoring session..."),
                        ),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{MappedKeystroke, TerminalView};
    use gpui::{Keystroke, px, size};
    use libghostty_vt::{key as gkey, mouse};
    use seoul_terminal_proto::messages::SessionAttachedMsg;
    use seoul_vt::TerminalBuilder;
    use seoul_vt::config::TerminalConfig;
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    #[derive(Clone, Default)]
    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn keystroke(key: &str) -> Keystroke {
        Keystroke {
            key: key.to_string(),
            ..Keystroke::default()
        }
    }

    fn ctrl_keystroke(key: &str) -> Keystroke {
        Keystroke {
            key: key.to_string(),
            modifiers: gpui::Modifiers {
                control: true,
                ..gpui::Modifiers::default()
            },
            ..Keystroke::default()
        }
    }

    fn cmd_keystroke(key: &str) -> Keystroke {
        Keystroke {
            key: key.to_string(),
            modifiers: gpui::Modifiers {
                platform: true,
                ..gpui::Modifiers::default()
            },
            ..Keystroke::default()
        }
    }

    fn attached_msg(
        scrollback_data: Vec<u8>,
        rehydrate_sequences: Vec<u8>,
        was_recovered: bool,
    ) -> SessionAttachedMsg {
        SessionAttachedMsg {
            session_id: Uuid::new_v4(),
            is_new: false,
            was_recovered,
            scrollback_data,
            cols: 80,
            rows: 24,
            cwd: None,
            foreground_process: None,
            rehydrate_sequences,
        }
    }

    fn attached_terminal() -> seoul_vt::Terminal {
        TerminalBuilder::new(TerminalConfig::default())
            .build_attached(Box::new(std::io::sink()))
            .expect("attached test terminal should build")
    }

    fn captured_attached_terminal() -> (seoul_vt::Terminal, Arc<Mutex<Vec<u8>>>) {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let writer = CaptureWriter(captured.clone());
        let terminal = TerminalBuilder::new(TerminalConfig::default())
            .build_attached(Box::new(writer))
            .expect("attached test terminal should build");
        (terminal, captured)
    }

    fn take(captured: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
        std::mem::take(&mut *captured.lock().unwrap())
    }

    fn terminal_text(terminal: &seoul_vt::Terminal) -> String {
        terminal
            .last_content
            .cells
            .iter()
            .map(|row| {
                row.iter()
                    .flat_map(|cell| cell.graphemes.iter())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn maps_additional_fixed_keys() {
        assert!(matches!(
            TerminalView::map_keystroke(&keystroke("insert")),
            Some(MappedKeystroke::Encoded {
                key: gkey::Key::Insert,
                unshifted: None,
                ..
            })
        ));
        assert!(matches!(
            TerminalView::map_keystroke(&keystroke("f13")),
            Some(MappedKeystroke::Encoded {
                key: gkey::Key::F13,
                unshifted: None,
                ..
            })
        ));
        assert!(matches!(
            TerminalView::map_keystroke(&keystroke("numpad_add")),
            Some(MappedKeystroke::Encoded {
                key: gkey::Key::NumpadAdd,
                unshifted: None,
                ..
            })
        ));
    }

    #[test]
    fn maps_unshifted_for_single_character_keys() {
        assert!(matches!(
            TerminalView::map_keystroke(&ctrl_keystroke("A")),
            Some(MappedKeystroke::Encoded {
                key: gkey::Key::A,
                unshifted: Some('a'),
                ..
            })
        ));
    }

    #[test]
    fn cmd_arrow_keys_map_to_shell_line_navigation() {
        assert!(matches!(
            TerminalView::map_keystroke(&cmd_keystroke("left")),
            Some(MappedKeystroke::Raw(b"\x01"))
        ));
        assert!(matches!(
            TerminalView::map_keystroke(&cmd_keystroke("up")),
            Some(MappedKeystroke::Raw(b"\x01"))
        ));
        assert!(matches!(
            TerminalView::map_keystroke(&cmd_keystroke("right")),
            Some(MappedKeystroke::Raw(b"\x05"))
        ));
        assert!(matches!(
            TerminalView::map_keystroke(&cmd_keystroke("down")),
            Some(MappedKeystroke::Raw(b"\x05"))
        ));
    }

    #[test]
    fn cmd_printable_keys_stay_reserved_for_keybindings() {
        assert!(TerminalView::map_keystroke(&cmd_keystroke("s")).is_none());
    }

    #[test]
    fn cmd_non_arrow_special_keys_keep_super_modifier() {
        assert!(matches!(
            TerminalView::map_keystroke(&cmd_keystroke("home")),
            Some(MappedKeystroke::Encoded {
                key: gkey::Key::Home,
                mods,
                ..
            }) if mods.contains(gkey::Mods::SUPER)
        ));
    }

    #[test]
    fn raw_mapped_keystrokes_write_to_terminal() {
        let (mut terminal, captured) = captured_attached_terminal();
        let mut ime_preedit = String::new();

        assert!(TerminalView::dispatch_mapped_keystroke(
            &mut terminal,
            MappedKeystroke::Raw(b"\x01"),
            &mut ime_preedit,
        ));
        assert_eq!(take(&captured), b"\x01");
    }

    #[test]
    fn mouse_encoder_size_for_content_bounds_uses_content_size_and_zero_padding() {
        let encoder_size =
            TerminalView::mouse_encoder_size_for_content_bounds(size(px(80.4), px(24.6)), 7.6, 0.2);

        assert_eq!(encoder_size.screen_width, 80);
        assert_eq!(encoder_size.screen_height, 25);
        assert_eq!(encoder_size.cell_width, 8);
        assert_eq!(encoder_size.cell_height, 1);
        assert_eq!(encoder_size.padding_top, 0);
        assert_eq!(encoder_size.padding_bottom, 0);
        assert_eq!(encoder_size.padding_left, 0);
        assert_eq!(encoder_size.padding_right, 0);
    }

    #[test]
    fn warm_attach_rehydrate_overrides_stale_scrollback_mouse_modes() {
        let mut terminal = attached_terminal();
        let msg = attached_msg(
            b"\x1b[?1000l".to_vec(),
            b"\x1b[?1000h\x1b[?1006h".to_vec(),
            false,
        );

        TerminalView::replay_attached_state(&mut terminal, &msg);

        assert!(terminal.is_mouse_tracking());
    }

    #[test]
    fn warm_attach_rehydrated_mouse_modes_emit_mouse_events() {
        let (mut terminal, captured) = captured_attached_terminal();
        let msg = attached_msg(Vec::new(), b"\x1b[?1003h\x1b[?1006h".to_vec(), false);

        TerminalView::replay_attached_state(&mut terminal, &msg);

        terminal.set_mouse_size(mouse::EncoderSize {
            screen_width: 800,
            screen_height: 400,
            cell_width: 10,
            cell_height: 20,
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        });
        terminal.send_mouse_event(
            mouse::Action::Press,
            Some(mouse::Button::Left),
            gkey::Mods::empty(),
            15.0,
            25.0,
        );

        assert_eq!(take(&captured), b"\x1b[<0;2;2M");
    }

    #[test]
    fn warm_attach_rehydrate_overrides_stale_scrollback_alt_scroll_modes() {
        let mut terminal = attached_terminal();
        let msg = attached_msg(b"\x1b[?1007h".to_vec(), b"\x1b[?1007l".to_vec(), false);

        TerminalView::replay_attached_state(&mut terminal, &msg);

        assert!(!terminal.is_alt_scroll());
    }

    #[test]
    fn cold_restore_clears_stale_scrollback_mouse_tracking() {
        let mut terminal = attached_terminal();
        let msg = attached_msg(b"\x1b[?1000h\x1b[?1006h".to_vec(), Vec::new(), true);

        TerminalView::replay_attached_state(&mut terminal, &msg);

        assert!(!terminal.is_mouse_tracking());
    }

    #[test]
    fn cold_restore_resets_keyboard_protocol_before_shell_input() {
        let (mut terminal, captured) = captured_attached_terminal();
        let msg = attached_msg(b"\x1b[>3u\x1b[>4;2m".to_vec(), Vec::new(), true);

        TerminalView::replay_attached_state(&mut terminal, &msg);

        terminal.try_keystroke(gkey::Key::ArrowLeft, gkey::Mods::empty(), None, None);

        assert_eq!(take(&captured), b"\x1b[D");
    }

    #[test]
    fn cold_restore_places_live_output_after_restored_scrollback() {
        let mut terminal = attached_terminal();
        let msg = attached_msg(
            b"\x1b[24;1HRESTORED-CONTENT\x1b[1;1H".to_vec(),
            Vec::new(),
            true,
        );

        TerminalView::replay_attached_state(&mut terminal, &msg);

        assert_eq!(
            terminal.last_content.cursor.row,
            terminal.last_content.terminal_bounds.rows - 1
        );

        terminal.feed_pty_data(b"NEW-SHELL-LINE");
        terminal.sync();

        let text = terminal_text(&terminal);
        assert!(text.contains("RESTORED-CONTENT"), "{text}");
        assert!(text.contains("NEW-SHELL-LINE"), "{text}");
    }
}
