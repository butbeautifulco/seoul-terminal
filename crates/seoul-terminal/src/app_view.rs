use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use seoul_terminal_proto::pr::{
    ChecksStatus, PrInfo, PrState, PrUnavailableReason, ReviewDecision,
};
use seoul_workspace::persistence::{
    self, AppState, ClosedTabInfo, PersistedTab, PersistedTabKind, PersistedWorkspaceTabs,
};
use seoul_workspace::project::Project;
use seoul_workspace::workspace::{Workspace, WorkspaceKind};
use uuid::Uuid;

use crate::branch_input::{BranchInput, BranchInputEvent};
use crate::daemon_client::{DaemonClient, DaemonClientInner, DaemonSessionHandle, PrEvent};
use crate::editor_view::{EditorEvent, EditorView};
use crate::file_tree_view::{FileTreeEvent, FileTreeView};
use crate::git_state_provider::{GitStateEvent, GitStateProvider};
use crate::icons::{Icon, IconName};
use crate::resource_indicator::ResourceIndicator;
use crate::settings_view::{SettingsEvent, SettingsView};
use crate::terminal_view::TerminalView;
use crate::theme;
use crate::toast::{ToastKind, ToastManager};
use seoul_workspace::settings::SettingsStore;
use std::sync::Arc;

const SERIALIZATION_THROTTLE_MS: u64 = 200;
const MAX_CLOSED_TABS: usize = 10;
const DAEMON_HEALTH_CHECK_SECS: u64 = 1;
const DAEMON_RECONNECT_BACKOFF_INITIAL_MS: u64 = 1000;
const DAEMON_RECONNECT_BACKOFF_MAX_MS: u64 = 10_000;

const RESIZE_HANDLE_SIZE: f32 = 6.0;
const LEFT_SIDEBAR_MIN: f32 = 160.0;
const LEFT_SIDEBAR_MAX: f32 = 480.0;
const RIGHT_SIDEBAR_MIN: f32 = 200.0;
const RIGHT_SIDEBAR_MAX: f32 = 600.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RestorePlanMode {
    Attach,
    Ensure,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RestoreTabCandidate {
    tab_id: Uuid,
    session_id: Uuid,
    workspace_id: Uuid,
    is_active_tab: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RestorePlanEntry {
    tab_id: Uuid,
    session_id: Uuid,
    workspace_id: Uuid,
    mode: RestorePlanMode,
}

fn restore_trace_enabled() -> bool {
    std::env::var_os("SEOUL_RESTORE_TRACE").is_some()
}

fn should_attach_pending_terminal_on_activation(
    is_pending: bool,
    attach_in_flight: bool,
    daemon_ready: bool,
) -> bool {
    is_pending && !attach_in_flight && daemon_ready
}

fn plan_restore_order(
    candidates: Vec<RestoreTabCandidate>,
    active_workspace_id: Option<Uuid>,
) -> Vec<RestorePlanEntry> {
    if candidates.is_empty() {
        return Vec::new();
    }

    let active_attach_idx = active_workspace_id
        .and_then(|active_ws| {
            candidates
                .iter()
                .position(|c| c.workspace_id == active_ws && c.is_active_tab)
                .or_else(|| candidates.iter().position(|c| c.workspace_id == active_ws))
        })
        .unwrap_or(0);

    let active = candidates[active_attach_idx].clone();
    let mut entries = vec![RestorePlanEntry {
        tab_id: active.tab_id,
        session_id: active.session_id,
        workspace_id: active.workspace_id,
        mode: RestorePlanMode::Attach,
    }];

    entries.extend(
        candidates
            .into_iter()
            .enumerate()
            .filter(|(idx, _)| *idx != active_attach_idx)
            .map(|(_, candidate)| RestorePlanEntry {
                tab_id: candidate.tab_id,
                session_id: candidate.session_id,
                workspace_id: candidate.workspace_id,
                mode: RestorePlanMode::Ensure,
            }),
    );

    entries.sort_by_key(|entry| {
        (
            entry.mode != RestorePlanMode::Attach,
            active_workspace_id != Some(entry.workspace_id),
        )
    });
    entries
}

#[derive(Clone, Copy, Debug)]
enum SidebarSide {
    Left,
    Right,
}

#[derive(Clone)]
struct ResizeSidebar(SidebarSide);

impl Render for ResizeSidebar {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

struct ResizeDragState {
    start_width: f32,
    start_pointer_x: Pixels,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum RightSidebarTab {
    Files,
    Changes,
}

actions!(
    app,
    [
        NewTab,
        CloseActiveTab,
        ReopenClosedTab,
        ToggleSidebar,
        ToggleFileTree,
        AddProject,
        OpenSettings,
        SplitRight,
        SplitDown,
        Quit
    ]
);

use crate::pane::{Pane, PaneEvent, TabMetadata};
use crate::pane_group::{Axis, PaneGroup};

// ---------------------------------------------------------------------------
// AppView
// ---------------------------------------------------------------------------

pub struct AppView {
    state: AppState,
    /// Per-workspace pane layout tree.
    pane_groups: HashMap<Uuid, PaneGroup>,
    /// Currently focused pane (across all workspaces).
    focused_pane: Option<Entity<Pane>>,
    #[allow(dead_code)]
    pane_subscriptions: Vec<Subscription>,
    focus_handle: FocusHandle,
    collapsed_projects: Vec<Uuid>,
    daemon_client: Option<DaemonClient>,
    /// Shared inner handle for background thread access (set when daemon connects)
    daemon_inner: Option<Arc<DaemonClientInner>>,
    /// Background daemon connection task
    #[allow(dead_code)]
    _daemon_connect_task: Option<Task<()>>,
    /// Tab ID → daemon session ID mapping for persistence
    tab_sessions: HashMap<Uuid, Uuid>,
    /// Tab ID → workspace ID mapping for terminal lifecycle and persistence.
    tab_workspace: HashMap<Uuid, Uuid>,
    /// Recently closed terminal tabs for undo-close (Cmd+Shift+T)
    closed_tabs: Vec<ClosedTabInfo>,
    #[allow(dead_code)]
    pending_serialize: Option<Task<()>>,
    /// Prevents double-close (on_app_quit + Drop)
    closed: bool,
    #[allow(dead_code)]
    _quit_subscription: Option<Subscription>,
    // Right sidebar — file tree
    file_tree: Option<Entity<FileTreeView>>,
    #[allow(dead_code)]
    _file_tree_subscription: Option<Subscription>,
    /// Subscriptions for settings view events (keyed by tab ID)
    #[allow(dead_code)]
    settings_subscriptions: HashMap<Uuid, Subscription>,
    // Git integration
    git_provider: Option<Entity<GitStateProvider>>,
    #[allow(dead_code)]
    _git_subscription: Option<Subscription>,
    git_panel: Option<Entity<crate::git_panel_view::GitPanelView>>,
    #[allow(dead_code)]
    _git_panel_subscription: Option<Subscription>,
    right_sidebar_tab: RightSidebarTab,
    // Resource monitor
    resource_indicator: Option<Entity<ResourceIndicator>>,
    // Toast notifications
    toast: Entity<ToastManager>,
    // Workspace deletion confirmation
    pending_delete_ws: Option<Uuid>,
    // Workspace creation branch name prompt
    new_ws_prompt: Option<NewWorkspacePrompt>,
    // Daemon health check background task
    #[allow(dead_code)]
    _daemon_health_task: Option<Task<()>>,
    /// Session handles from background reattach, ready for in-place attach
    pending_reattach_handles: Vec<(Uuid, DaemonSessionHandle)>,
    /// Terminal tabs with an in-flight full attach request.
    pending_attach_tabs: HashSet<Uuid>,
    /// Sessions with an in-flight background ensure request.
    pending_ensure_sessions: HashSet<Uuid>,
    /// All terminal tab entities keyed by tab ID.
    terminal_tabs: HashMap<Uuid, Entity<TerminalView>>,
    /// Whether daemon is currently connected (drives disconnect overlay + input blocking)
    daemon_connected: bool,
    /// Number of daemon session reattachments still in flight or waiting to be applied.
    pending_recoveries: usize,
    /// Active sidebar resize drag (None when not dragging).
    resize_drag: Option<ResizeDragState>,
    /// PR status per workspace, populated by the daemon's `PrPoller`.
    pr_status_by_workspace: HashMap<Uuid, PrInfo>,
    /// "Why is PR sync unavailable?" for the global state (gh missing,
    /// not authenticated, rate-limited). Per-workspace reason lives here too
    /// since the unavailable case is keyed by workspace_id from the daemon.
    pr_unavailable_by_workspace: HashMap<Uuid, PrUnavailableReason>,
    /// Background task that drains `DaemonClient::try_recv_pr_event`.
    #[allow(dead_code)]
    _pr_event_task: Option<Task<()>>,
}

struct NewWorkspacePrompt {
    project_id: Uuid,
    generated_name: String,
    branch_input: Entity<BranchInput>,
    #[allow(dead_code)]
    subscription: Subscription,
}

impl AppView {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        mut state: AppState,
        daemon_client: Option<DaemonClient>,
    ) -> Self {
        let focus_handle = cx.focus_handle();

        // Create file tree view
        let root = state
            .active_workspace()
            .and_then(|ws| state.workspace_working_dir(ws));
        let file_tree = cx.new(|cx| FileTreeView::new(cx, root));
        let file_tree_sub = cx.subscribe(&file_tree, Self::on_file_tree_event);

        state.migrate_legacy_tabs();
        let mut saved_tab_sessions = HashMap::new();
        let mut saved_closed_tabs = std::mem::take(&mut state.closed_tabs);
        // Build runtime terminal maps from the canonical persisted tabs.
        let mut saved_tab_workspace = HashMap::new();
        for (ws_id, workspace_tabs) in &state.workspace_tabs {
            for tab in &workspace_tabs.tabs {
                if let PersistedTabKind::Terminal { session_id } = &tab.kind {
                    saved_tab_sessions.insert(tab.id, *session_id);
                    saved_tab_workspace.insert(tab.id, *ws_id);
                }
            }
        }

        // Migration: remove orphaned terminal sessions (tabs with no workspace mapping)
        let before_count = saved_tab_sessions.len();
        saved_tab_sessions.retain(|tab_id, _| saved_tab_workspace.contains_key(tab_id));
        if saved_tab_sessions.len() < before_count {
            tracing::info!(
                removed = before_count - saved_tab_sessions.len(),
                "cleaned orphaned tab_sessions on startup"
            );
        }

        // Clean closed_tabs referencing workspaces that no longer exist
        let valid_ws_ids: std::collections::HashSet<Uuid> =
            state.workspaces.iter().map(|w| w.id).collect();
        saved_closed_tabs.retain(|ct| valid_ws_ids.contains(&ct.workspace_id));

        let has_daemon = daemon_client.is_some();
        let mut app = Self {
            state,
            pane_groups: HashMap::new(),
            focused_pane: None,
            pane_subscriptions: Vec::new(),
            focus_handle,
            collapsed_projects: Vec::new(),
            daemon_client,
            daemon_inner: None,
            _daemon_connect_task: None,
            tab_sessions: saved_tab_sessions,
            tab_workspace: saved_tab_workspace,
            closed_tabs: saved_closed_tabs,
            pending_serialize: None,
            closed: false,
            _quit_subscription: None,
            file_tree: Some(file_tree),
            _file_tree_subscription: Some(file_tree_sub),
            settings_subscriptions: HashMap::new(),
            git_provider: None,
            _git_subscription: None,
            git_panel: None,
            _git_panel_subscription: None,
            right_sidebar_tab: RightSidebarTab::Files,
            resource_indicator: None,
            toast: cx.new(|_cx| ToastManager::new()),
            pending_delete_ws: None,
            new_ws_prompt: None,
            _daemon_health_task: None,
            pending_reattach_handles: Vec::new(),
            pending_attach_tabs: HashSet::new(),
            pending_ensure_sessions: HashSet::new(),
            terminal_tabs: HashMap::new(),
            daemon_connected: has_daemon,
            pending_recoveries: 0,
            resize_drag: None,
            pr_status_by_workspace: HashMap::new(),
            pr_unavailable_by_workspace: HashMap::new(),
            _pr_event_task: None,
        };

        // If daemon client was already provided, still restore the layout as
        // pending terminals first. Full attach is scheduled active-first below.
        if app.daemon_client.is_some() {
            let daemon_client = app.daemon_client.take();
            for ws in app.state.workspaces.clone() {
                if !app.restore_workspace_tabs_offline(&ws, window, cx) {
                    app.ensure_workspace_has_tab(&ws, window, cx);
                }
            }
            app.daemon_client = daemon_client;
            app.setup_daemon_client(cx);
            app.start_background_reattach_all(cx);
        } else {
            // Restore saved tab layout (using saved tab IDs for daemon reattach),
            // or create a fresh tab if no saved sessions exist.
            for ws in app.state.workspaces.clone() {
                if !app.restore_workspace_tabs_offline(&ws, window, cx) {
                    app.ensure_workspace_has_tab(&ws, window, cx);
                }
            }
            // Start background daemon connection
            app._daemon_connect_task = Some(Self::start_background_connect(cx));
        }

        app.init_git_provider(window, cx);

        // Register quit handler
        app._quit_subscription = Some(cx.on_app_quit(|this, cx| {
            this.prepare_to_close(cx);
            async {}
        }));

        // Hot reload: poll settings files every 1 second
        cx.spawn(async |_this, cx: &mut AsyncApp| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                cx.update(|cx| {
                    SettingsStore::check_and_reload(cx);
                });
            }
        })
        .detach();

        // Daemon health check: detect daemon death and auto-reconnect
        app._daemon_health_task = Some(Self::start_daemon_health_check(cx));

        app
    }

    /// Schedule a throttled state save (Zed pattern: 200ms).
    fn schedule_serialize(&mut self, cx: &mut Context<Self>) {
        if self.pending_serialize.is_some() {
            return;
        }
        self.pending_serialize =
            Some(cx.spawn(async |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                cx.background_executor()
                    .timer(Duration::from_millis(SERIALIZATION_THROTTLE_MS))
                    .await;
                this.update(cx, |this, cx| {
                    this.save_state(cx);
                    this.pending_serialize = None;
                })
                .ok();
            }));
    }

    /// Flush serialization immediately — bypasses throttle (Zed: flush_serialization).
    fn flush_serialization(&mut self, cx: &App) {
        self.pending_serialize = None;
        self.save_state(cx);
    }

    /// Handle a sidebar resize drag move. Registered once on the app root —
    /// GPUI's drag system routes mouse-move events here while a `ResizeSidebar`
    /// drag is active, regardless of where the pointer goes (incl. outside the
    /// window).
    fn on_resize_drag_move(
        &mut self,
        e: &DragMoveEvent<ResizeSidebar>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.resize_drag.as_ref() else {
            return;
        };
        let start_width = state.start_width;
        let start_pointer_x = state.start_pointer_x;
        let side = e.drag(cx).0;
        let delta_px = f32::from(e.event.position.x - start_pointer_x);
        let raw = match side {
            SidebarSide::Left => start_width + delta_px,
            SidebarSide::Right => start_width - delta_px,
        };
        let (min, max) = match side {
            SidebarSide::Left => (LEFT_SIDEBAR_MIN, LEFT_SIDEBAR_MAX),
            SidebarSide::Right => (RIGHT_SIDEBAR_MIN, RIGHT_SIDEBAR_MAX),
        };
        let clamped = raw.clamp(min, max);
        match side {
            SidebarSide::Left => self.state.sidebar_width = clamped,
            SidebarSide::Right => self.state.right_sidebar_width = clamped,
        }
        self.schedule_serialize(cx);
        cx.notify();
    }

    /// Set up daemon client state after connection (resource indicator, inner handle).
    fn setup_daemon_client(&mut self, cx: &mut Context<Self>) {
        if let Some(ref client) = self.daemon_client {
            self.daemon_inner = Some(client.inner_handle());
            self.resource_indicator = Some(cx.new(|cx| ResourceIndicator::new(cx, client)));
            self.sync_workspace_names(cx);
            self.register_all_workspaces_with_daemon();
            self.send_active_workspace_focus();
            self._pr_event_task = Some(self.start_pr_event_poll(cx));
        }
    }

    /// Tell the daemon about every workspace the app currently knows of.
    /// Called once after daemon connect; further changes go through
    /// `register_workspace_with_daemon` / `unregister_workspace_with_daemon`.
    fn register_all_workspaces_with_daemon(&self) {
        let Some(ref client) = self.daemon_client else {
            return;
        };
        for ws in &self.state.workspaces {
            let project = self.state.projects.iter().find(|p| p.id == ws.project_id);
            let working_dir = match project {
                Some(p) => ws.working_dir(p).to_path_buf(),
                None => continue,
            };
            let _ = client.register_workspace(ws.id, working_dir, ws.branch.clone());
        }
    }

    fn register_workspace_with_daemon(&self, ws: &Workspace) {
        let Some(ref client) = self.daemon_client else {
            return;
        };
        let Some(project) = self.state.projects.iter().find(|p| p.id == ws.project_id) else {
            return;
        };
        let working_dir = ws.working_dir(project).to_path_buf();
        let _ = client.register_workspace(ws.id, working_dir, ws.branch.clone());
    }

    fn unregister_workspace_with_daemon(&self, workspace_id: Uuid) {
        if let Some(ref client) = self.daemon_client {
            let _ = client.unregister_workspace(workspace_id);
        }
        // Drop locally cached PR state too — the badge should disappear immediately.
    }

    /// Tell the daemon which workspace the user is currently looking at.
    /// Active workspaces poll every 10s, idle ones every 120s.
    fn send_active_workspace_focus(&self) {
        if let Some(ref client) = self.daemon_client {
            let _ = client.focus_workspace(self.state.active_workspace_id);
        }
    }

    /// Background task that drains PR events from `DaemonClient` into
    /// `pr_status_by_workspace` / `pr_unavailable_by_workspace`. Poll interval
    /// is short (50ms) so badge updates feel instant; the underlying GitHub
    /// poll is throttled by the daemon so this loop is just a queue drainer.
    fn start_pr_event_poll(&self, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                let Ok(events) = this.update(cx, |this, _cx| this.drain_pr_events()) else {
                    break; // entity dropped
                };
                if !events.is_empty() {
                    let _ = this.update(cx, |this, cx| {
                        for ev in events {
                            this.apply_pr_event(ev);
                        }
                        cx.notify();
                    });
                }
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
            }
        })
    }

    fn drain_pr_events(&self) -> Vec<PrEvent> {
        let Some(ref client) = self.daemon_client else {
            return Vec::new();
        };
        let mut events = Vec::new();
        while let Some(ev) = client.try_recv_pr_event() {
            events.push(ev);
        }
        events
    }

    fn apply_pr_event(&mut self, ev: PrEvent) {
        match ev {
            PrEvent::Updated {
                workspace_id,
                pr_info,
            } => {
                self.pr_unavailable_by_workspace.remove(&workspace_id);
                match pr_info {
                    Some(pr) => {
                        self.pr_status_by_workspace.insert(workspace_id, pr);
                    }
                    None => {
                        self.pr_status_by_workspace.remove(&workspace_id);
                    }
                }
            }
            PrEvent::Unavailable {
                workspace_id,
                reason,
            } => {
                self.pr_status_by_workspace.remove(&workspace_id);
                self.pr_unavailable_by_workspace
                    .insert(workspace_id, reason);
            }
        }
    }

    /// Push current workspace names to the resource indicator.
    fn sync_workspace_names(&self, cx: &mut Context<Self>) {
        if let Some(ri) = &self.resource_indicator {
            let names: HashMap<Uuid, String> = self
                .state
                .workspaces
                .iter()
                .map(|w| (w.id, w.name.clone()))
                .collect();
            ri.update(cx, |ri, _| ri.set_workspace_names(names));
        }
    }

    /// Background daemon connection — non-blocking, runs off UI thread.
    fn start_background_connect(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            // Move the blocking connect_or_spawn() off the UI thread
            let result = cx
                .background_executor()
                .spawn(async { DaemonClient::connect_or_spawn() })
                .await;

            match result {
                Ok(client) => {
                    tracing::info!("daemon connected in background");
                    let _ = this.update(cx, |app, cx| {
                        app.daemon_client = Some(client);
                        app.daemon_connected = true;
                        app.setup_daemon_client(cx);

                        // Re-attach existing tabs to daemon sessions in background
                        app.start_background_reattach_all(cx);

                        cx.notify();
                    });
                }
                Err(e) => {
                    tracing::warn!("background daemon connect failed: {e}");
                }
            }
        })
    }

    /// Re-attach all existing terminal tabs to daemon sessions in background.
    fn start_background_reattach_all(&mut self, cx: &mut Context<Self>) {
        if self.pending_recoveries > 0 {
            tracing::debug!("skipping reattach: recovery already in progress");
            return;
        }

        let Some(inner) = self.daemon_inner.clone() else {
            return;
        };

        // Remove orphaned tab_sessions (tabs with no workspace mapping)
        self.tab_sessions.retain(|tab_id, session_id| {
            if self.tab_workspace.contains_key(tab_id) {
                true
            } else {
                tracing::warn!(%tab_id, %session_id, "removing orphaned tab_sessions entry");
                false
            }
        });

        // Collect tabs that need daemon work. Only the active terminal gets a
        // full attach; hidden tabs are only ensured so they do not replay
        // scrollback on the UI thread during startup.
        let mut candidates = Vec::new();
        let mut cwd_by_workspace = HashMap::new();
        let active_workspace = self.active_workspace_id();
        let mut workspace_ids: Vec<Uuid> = self.pane_groups.keys().copied().collect();
        workspace_ids.sort_by_key(|ws_id| (active_workspace != Some(*ws_id), *ws_id));

        for ws_id in workspace_ids {
            let Some(group) = self.pane_groups.get(&ws_id) else {
                continue;
            };

            let cwd = self
                .state
                .workspaces
                .iter()
                .find(|w| w.id == ws_id)
                .and_then(|w| self.state.workspace_working_dir(w));
            cwd_by_workspace.insert(ws_id, cwd.clone());

            for pane in group.panes() {
                let pane = pane.read(cx);
                for tab_id in pane.tabs.iter().map(|tab| tab.id) {
                    let Some(session_id) = self.tab_sessions.get(&tab_id).copied() else {
                        continue;
                    };
                    candidates.push(RestoreTabCandidate {
                        tab_id,
                        session_id,
                        workspace_id: ws_id,
                        is_active_tab: active_workspace == Some(ws_id)
                            && pane.active_tab_id == Some(tab_id),
                    });
                }
            }
        }

        let restore_plan = plan_restore_order(candidates, active_workspace);
        if restore_plan.is_empty() {
            self.pending_recoveries = 0;
            return;
        }
        self.pending_recoveries = restore_plan.len();
        for entry in &restore_plan {
            match entry.mode {
                RestorePlanMode::Attach => {
                    self.pending_attach_tabs.insert(entry.tab_id);
                }
                RestorePlanMode::Ensure => {
                    self.pending_ensure_sessions.insert(entry.session_id);
                }
            }
        }

        if restore_trace_enabled() {
            tracing::info!(
                count = restore_plan.len(),
                active_workspace = ?active_workspace,
                "restore trace: queued startup restore plan"
            );
        }

        // Spawn one background task. Work remains serial so the daemon is not
        // flooded, but only the first entry is a full attach.
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            for entry in restore_plan {
                let inner = inner.clone();
                let cwd = cwd_by_workspace
                    .get(&entry.workspace_id)
                    .cloned()
                    .unwrap_or(None);
                let request_entry = entry.clone();
                let started_at = std::time::Instant::now();
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        match request_entry.mode {
                            RestorePlanMode::Attach => {
                                DaemonClientInner::create_or_attach(
                                    &inner,
                                    request_entry.session_id,
                                    request_entry.workspace_id,
                                    80,
                                    24,
                                    cwd,
                                )
                                .map(|handle| (request_entry, Some(handle)))
                            }
                            RestorePlanMode::Ensure => DaemonClientInner::ensure_session(
                                &inner,
                                request_entry.session_id,
                                request_entry.workspace_id,
                                80,
                                24,
                                cwd,
                            )
                            .map(|_| (request_entry, None)),
                        }
                    })
                    .await;

                match result {
                    Ok((entry, Some(session_handle))) => {
                        let _ = this.update(cx, |app, cx| {
                            if restore_trace_enabled() {
                                tracing::info!(
                                    tab_id = %entry.tab_id,
                                    session_id = %entry.session_id,
                                    elapsed_ms = started_at.elapsed().as_millis(),
                                    scrollback_bytes = session_handle.attached_msg.scrollback_data.len(),
                                    "restore trace: full attach response ready"
                                );
                            }
                            app.pending_reattach_handles
                                .push((entry.tab_id, session_handle));
                            cx.notify();
                        });
                    }
                    Ok((entry, None)) => {
                        let _ = this.update(cx, |app, cx| {
                            if restore_trace_enabled() {
                                tracing::info!(
                                    tab_id = %entry.tab_id,
                                    session_id = %entry.session_id,
                                    elapsed_ms = started_at.elapsed().as_millis(),
                                    "restore trace: background ensure complete"
                                );
                            }
                            app.pending_ensure_sessions.remove(&entry.session_id);
                            app.pending_recoveries = app.pending_recoveries.saturating_sub(1);
                            cx.notify();
                        });
                    }
                    Err(e) => {
                        let _ = this.update(cx, |app, cx| {
                            match entry.mode {
                                RestorePlanMode::Attach => {
                                    app.pending_attach_tabs.remove(&entry.tab_id);
                                    tracing::warn!(
                                        "background attach failed for {}: {e}",
                                        entry.session_id
                                    );
                                }
                                RestorePlanMode::Ensure => {
                                    app.pending_ensure_sessions.remove(&entry.session_id);
                                    tracing::warn!(
                                        "background ensure failed for {}: {e}",
                                        entry.session_id
                                    );
                                }
                            }
                            app.pending_recoveries = app.pending_recoveries.saturating_sub(1);
                            cx.notify();
                        });
                    }
                }
            }
        })
        .detach();
    }

    /// Process completed background reattach handles — attach sessions in-place
    /// to pending TerminalView entities (no Entity replacement needed).
    fn process_pending_reattach_handles(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_reattach_handles.is_empty() {
            return;
        }

        let (tab_id, session_handle) = self.pending_reattach_handles.remove(0);
        let Some(client) = &self.daemon_client else {
            self.pending_attach_tabs.remove(&tab_id);
            self.pending_recoveries = self.pending_recoveries.saturating_sub(1);
            return;
        };
        if let Some(terminal) = self.terminal_tabs.get(&tab_id).cloned() {
            let started_at = std::time::Instant::now();
            terminal.update(cx, |tv, cx| {
                tv.attach_session(client, session_handle, cx);
                cx.notify();
            });
            if restore_trace_enabled() {
                tracing::info!(
                    tab_id = %tab_id,
                    elapsed_ms = started_at.elapsed().as_millis(),
                    "restore trace: applied full attach to terminal view"
                );
            }
        }
        self.pending_attach_tabs.remove(&tab_id);
        self.pending_recoveries = self.pending_recoveries.saturating_sub(1);
        if !self.pending_reattach_handles.is_empty() {
            cx.notify();
        }
    }

    fn remember_terminal_tab(&mut self, tab_id: Uuid, terminal: &Entity<TerminalView>) {
        self.terminal_tabs.insert(tab_id, terminal.clone());
    }

    fn attach_terminal_tab_if_pending(&mut self, tab_id: Uuid, cx: &mut Context<Self>) {
        let attach_in_flight = self.pending_attach_tabs.contains(&tab_id);
        let Some(terminal) = self.terminal_tabs.get(&tab_id).cloned() else {
            return;
        };
        let is_pending = terminal.read(cx).is_pending_restore();
        let daemon_ready = self.daemon_inner.is_some();
        if !should_attach_pending_terminal_on_activation(is_pending, attach_in_flight, daemon_ready)
        {
            return;
        }
        let Some(session_id) = self.tab_sessions.get(&tab_id).copied() else {
            return;
        };
        let Some(workspace_id) = self.tab_workspace.get(&tab_id).copied() else {
            return;
        };
        let Some(inner) = self.daemon_inner.clone() else {
            return;
        };
        let cwd = self
            .state
            .workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .and_then(|w| self.state.workspace_working_dir(w));

        self.pending_attach_tabs.insert(tab_id);
        self.pending_recoveries += 1;

        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let started_at = std::time::Instant::now();
            let result = cx
                .background_executor()
                .spawn(async move {
                    DaemonClientInner::create_or_attach(
                        &inner,
                        session_id,
                        workspace_id,
                        80,
                        24,
                        cwd,
                    )
                })
                .await;

            let _ = this.update(cx, |app, cx| match result {
                Ok(session_handle) => {
                    if restore_trace_enabled() {
                        tracing::info!(
                            tab_id = %tab_id,
                            session_id = %session_id,
                            elapsed_ms = started_at.elapsed().as_millis(),
                            scrollback_bytes = session_handle.attached_msg.scrollback_data.len(),
                            "restore trace: activation attach response ready"
                        );
                    }
                    app.pending_reattach_handles.push((tab_id, session_handle));
                    cx.notify();
                }
                Err(e) => {
                    app.pending_attach_tabs.remove(&tab_id);
                    app.pending_recoveries = app.pending_recoveries.saturating_sub(1);
                    tracing::warn!("activation attach failed for {session_id}: {e}");
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn attach_active_terminal_if_pending(&mut self, cx: &mut Context<Self>) {
        let Some(pane) = self.active_pane_entity() else {
            return;
        };
        let Some(tab_id) = pane.read(cx).active_tab_id else {
            return;
        };
        self.attach_terminal_tab_if_pending(tab_id, cx);
    }

    fn mark_daemon_terminals_pending(&mut self, cx: &mut Context<Self>) {
        self.pending_reattach_handles.clear();
        self.pending_attach_tabs.clear();
        self.pending_ensure_sessions.clear();
        self.pending_recoveries = 0;
        for tab_id in self.tab_sessions.keys() {
            let Some(terminal) = self.terminal_tabs.get(tab_id).cloned() else {
                continue;
            };
            terminal.update(cx, |terminal, cx| {
                terminal.mark_pending_restore();
                cx.notify();
            });
        }
    }

    /// Graceful window close: detach all sessions (PTYs stay alive) + save state.
    /// Safe to call multiple times (idempotent).
    pub fn prepare_to_close(&mut self, cx: &App) {
        if self.closed {
            return;
        }
        self.closed = true;
        tracing::info!(
            "preparing to close: detaching {} session(s)",
            self.tab_sessions.len()
        );
        if let Some(client) = &self.daemon_client {
            for session_id in self.tab_sessions.values() {
                client.detach(*session_id).ok();
            }
        }
        // Keep terminal/session descriptors intact so they persist to state.json
        // for session restoration on next launch.
        self.flush_serialization(cx);
    }

    /// Start a background task that monitors daemon health and reconnects on failure.
    fn start_daemon_health_check(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let mut backoff_ms = DAEMON_RECONNECT_BACKOFF_INITIAL_MS;

            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(DAEMON_HEALTH_CHECK_SECS))
                    .await;

                let needs_reconnect = this
                    .update(cx, |this, _cx| {
                        match &this.daemon_client {
                            Some(client) => !client.is_alive(),
                            None => false, // Background connect handles initial connection
                        }
                    })
                    .unwrap_or(false);

                if !needs_reconnect {
                    backoff_ms = DAEMON_RECONNECT_BACKOFF_INITIAL_MS;
                    continue;
                }

                tracing::info!("daemon health check: connection lost, attempting reconnect");

                // Mark disconnected — overlay will show automatically
                let _ = this.update(cx, |this, cx| {
                    this.daemon_connected = false;
                    this.mark_daemon_terminals_pending(cx);
                    cx.notify();
                });

                // Reconnect OFF the UI thread
                let result = cx
                    .background_executor()
                    .spawn(async { DaemonClient::connect_or_spawn() })
                    .await;

                match result {
                    Ok(new_client) => {
                        tracing::info!("daemon reconnected successfully");
                        let _ = this.update(cx, |app, cx| {
                            app.daemon_client = Some(new_client);
                            app.daemon_connected = true;
                            app.setup_daemon_client(cx);
                            app.start_background_reattach_all(cx);
                            app.toast.update(cx, |t, cx| {
                                t.show("Daemon reconnected".into(), ToastKind::Success, cx);
                            });
                            cx.notify();
                        });
                        backoff_ms = DAEMON_RECONNECT_BACKOFF_INITIAL_MS;
                    }
                    Err(e) => {
                        tracing::warn!("daemon reconnect failed: {e}");
                        let _ = this.update(cx, |app, cx| {
                            app.toast.update(cx, |t, cx| {
                                t.show(
                                    "Daemon reconnect failed, retrying...".into(),
                                    ToastKind::Error,
                                    cx,
                                );
                            });
                        });
                        cx.background_executor()
                            .timer(Duration::from_millis(backoff_ms))
                            .await;
                        backoff_ms = (backoff_ms * 2).min(DAEMON_RECONNECT_BACKOFF_MAX_MS);
                    }
                }
            }
        })
    }

    /// Handle Cmd+,: open settings UI tab.
    fn open_settings(&mut self, _: &OpenSettings, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws_id) = self.active_workspace_id() else {
            return;
        };

        let pane = self.get_or_create_pane(ws_id, cx);

        // Check if Settings tab already exists — activate it
        if let Some(tab_id) = pane.read(cx).find_tab_by_kind("settings") {
            pane.update(cx, |p, cx| p.activate_item(tab_id, _window, cx));
            return;
        }

        // Create new Settings tab
        let tab_id = Uuid::new_v4();
        let projects: Vec<_> = self
            .state
            .projects
            .iter()
            .map(|p| (p.id, p.name.clone(), p.path.clone()))
            .collect();
        let settings_view = cx.new(|cx| SettingsView::new(cx, projects));

        let sub = cx.subscribe(
            &settings_view,
            |this, _, event: &SettingsEvent, cx| match event {
                SettingsEvent::OpenSettingsFile { path } => {
                    this.open_file_from_tree(path.clone(), cx);
                }
            },
        );
        self.settings_subscriptions.insert(tab_id, sub);

        pane.update(cx, |p, cx| {
            p.add_item(
                tab_id,
                Box::new(settings_view),
                TabMetadata::new("settings", None, Some(PersistedTabKind::Settings)),
                _window,
                cx,
            );
        });
    }

    /// Handle Cmd+Q: graceful quit via platform quit.
    fn quit(&mut self, _: &Quit, _window: &mut Window, cx: &mut Context<Self>) {
        cx.quit();
    }

    fn create_editor_view(&mut self, path: PathBuf, cx: &mut Context<Self>) -> Entity<EditorView> {
        let editor = cx.new(|cx| EditorView::new(cx, path));
        cx.subscribe(
            &editor,
            move |_this: &mut AppView, _editor, _event: &EditorEvent, cx| {
                cx.notify();
            },
        )
        .detach();
        editor
    }

    fn create_settings_view(
        &mut self,
        tab_id: Uuid,
        cx: &mut Context<Self>,
    ) -> Entity<SettingsView> {
        let projects: Vec<_> = self
            .state
            .projects
            .iter()
            .map(|p| (p.id, p.name.clone(), p.path.clone()))
            .collect();
        let settings_view = cx.new(|cx| SettingsView::new(cx, projects));

        let sub = cx.subscribe(
            &settings_view,
            |this, _, event: &SettingsEvent, cx| match event {
                SettingsEvent::OpenSettingsFile { path } => {
                    this.open_file_from_tree(path.clone(), cx);
                }
            },
        );
        self.settings_subscriptions.insert(tab_id, sub);
        settings_view
    }

    fn diff_text_for_workspace(
        &self,
        workspace: &Workspace,
        path: &str,
        category: seoul_workspace::git::types::ChangeCategory,
    ) -> String {
        let Some(project) = self.state.project_by_id(workspace.project_id) else {
            return String::new();
        };
        let runner = seoul_workspace::git::GitCommandRunner::new(workspace.working_dir(project));
        seoul_workspace::git::diff::get_unified_diff(
            &runner,
            path,
            category,
            &project.default_branch,
        )
        .unwrap_or_default()
    }

    fn restore_persisted_tab(
        &mut self,
        workspace: &Workspace,
        tab: PersistedTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ws_id = workspace.id;
        match tab.kind {
            PersistedTabKind::Terminal { session_id } => {
                let cwd = self.state.workspace_working_dir(workspace);
                let terminal =
                    self.create_terminal_with_session(tab.id, ws_id, session_id, cwd, window, cx);
                let pane = self.get_or_create_pane(ws_id, cx);
                pane.update(cx, |p, cx| {
                    p.add_item(
                        tab.id,
                        Box::new(terminal),
                        TabMetadata::new(
                            "terminal",
                            None,
                            Some(PersistedTabKind::Terminal { session_id }),
                        ),
                        window,
                        cx,
                    );
                });
            }
            PersistedTabKind::Editor { path } => {
                let editor = self.create_editor_view(path.clone(), cx);
                let pane = self.get_or_create_pane(ws_id, cx);
                pane.update(cx, |p, cx| {
                    p.add_item(
                        tab.id,
                        Box::new(editor),
                        TabMetadata::new(
                            "editor",
                            Some(path.clone()),
                            Some(PersistedTabKind::Editor { path }),
                        ),
                        window,
                        cx,
                    );
                });
            }
            PersistedTabKind::Settings => {
                let settings_view = self.create_settings_view(tab.id, cx);
                let pane = self.get_or_create_pane(ws_id, cx);
                pane.update(cx, |p, cx| {
                    p.add_item(
                        tab.id,
                        Box::new(settings_view),
                        TabMetadata::new("settings", None, Some(PersistedTabKind::Settings)),
                        window,
                        cx,
                    );
                });
            }
            PersistedTabKind::Diff { path, category } => {
                let diff_text = self.diff_text_for_workspace(workspace, &path, category);
                let view_path = path.clone();
                let diff_view = cx.new(move |cx| {
                    crate::diff_view::DiffView::new(cx, view_path, category, diff_text)
                });
                let pane = self.get_or_create_pane(ws_id, cx);
                pane.update(cx, |p, cx| {
                    p.add_item(
                        tab.id,
                        Box::new(diff_view),
                        TabMetadata::new(
                            "diff",
                            None,
                            Some(PersistedTabKind::Diff { path, category }),
                        ),
                        window,
                        cx,
                    );
                });
            }
        }
    }

    /// Try to restore previously saved tabs.
    /// Returns true if at least one tab was restored.
    fn restore_workspace_tabs(
        &mut self,
        workspace: &Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let ws_id = workspace.id;
        let saved_tabs = match self.state.workspace_tabs.get(&ws_id) {
            Some(tabs) if !tabs.tabs.is_empty() => tabs.clone(),
            _ => return false,
        };

        let restored_ids: Vec<Uuid> = saved_tabs.tabs.iter().map(|tab| tab.id).collect();
        for tab in saved_tabs.tabs {
            self.restore_persisted_tab(workspace, tab, window, cx);
        }

        // Restore the previously active tab
        if let Some(active_id) = saved_tabs
            .active_tab_id
            .filter(|active_id| restored_ids.contains(active_id))
        {
            let pane = self.get_or_create_pane(ws_id, cx);
            pane.update(cx, |p, cx| {
                p.activate_item(active_id, window, cx);
            });
        }

        true
    }

    /// Restore saved tab layout with pending terminals (no PTY, no shell process).
    /// Uses saved tab IDs so background reattach can attach in-place later.
    fn restore_workspace_tabs_offline(
        &mut self,
        workspace: &Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.restore_workspace_tabs(workspace, window, cx)
    }

    fn ensure_workspace_has_tab(
        &mut self,
        workspace: &Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ws_id = workspace.id;
        let pane = self.get_or_create_pane(ws_id, cx);
        let needs_tab = pane.read(cx).tabs.is_empty();

        if needs_tab {
            let tab_id = Uuid::new_v4();
            let cwd = self.state.workspace_working_dir(workspace);
            let (terminal, session_id) = self.create_terminal(tab_id, ws_id, cwd, window, cx);
            // Reuse pane from above (create_terminal doesn't invalidate it)
            pane.update(cx, |p, cx| {
                p.add_item(
                    tab_id,
                    Box::new(terminal),
                    TabMetadata::new(
                        "terminal",
                        None,
                        Some(PersistedTabKind::Terminal { session_id }),
                    ),
                    window,
                    cx,
                );
            });
        }
    }

    /// Create a terminal with a fresh daemon session id.
    fn create_terminal(
        &mut self,
        tab_id: Uuid,
        workspace_id: Uuid,
        cwd: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<TerminalView>, Uuid) {
        let session_id = Uuid::new_v4();
        (
            self.create_terminal_with_session(tab_id, workspace_id, session_id, cwd, window, cx),
            session_id,
        )
    }

    /// Create a terminal backed by a daemon session id. If the daemon is not
    /// connected yet, keep a pending terminal so reattach can wire it later.
    fn create_terminal_with_session(
        &mut self,
        tab_id: Uuid,
        workspace_id: Uuid,
        session_id: Uuid,
        cwd: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalView> {
        self.tab_sessions.insert(tab_id, session_id);
        self.tab_workspace.insert(tab_id, workspace_id);

        if let Some(client) = &self.daemon_client {
            match client.create_or_attach(session_id, workspace_id, 80, 24, cwd) {
                Ok(session_handle) => {
                    let client_ref = self.daemon_client.as_ref().unwrap();
                    let terminal = cx.new(|cx| {
                        TerminalView::new_attached(window, cx, client_ref, session_handle)
                    });
                    self.remember_terminal_tab(tab_id, &terminal);
                    return terminal;
                }
                Err(e) => {
                    tracing::warn!("daemon session failed, keeping terminal pending: {e}");
                }
            }
        }

        let terminal = cx.new(|cx| TerminalView::new_pending(window, cx, session_id));
        self.remember_terminal_tab(tab_id, &terminal);
        terminal
    }

    fn active_workspace_id(&self) -> Option<Uuid> {
        self.state.active_workspace_id
    }

    fn active_pane_entity(&self) -> Option<Entity<Pane>> {
        self.focused_pane.clone().or_else(|| {
            let ws_id = self.active_workspace_id()?;
            self.pane_groups.get(&ws_id)?.panes().into_iter().next()
        })
    }

    fn focus_active_pane_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.active_pane_entity() else {
            return;
        };
        let focused_item = pane.update(cx, |pane, cx| pane.focus_active_item(window, cx));
        self.focused_pane = Some(pane.clone());
        if focused_item.is_none() {
            pane.read(cx).focus_handle(cx).focus(window, cx);
        }
    }

    fn create_pane(&mut self, cx: &mut Context<Self>) -> Entity<Pane> {
        let pane = cx.new(Pane::new);
        let sub = cx.subscribe(&pane, Self::on_pane_event);
        self.pane_subscriptions.push(sub);
        self.focused_pane = Some(pane.clone());
        pane
    }

    fn get_or_create_pane(&mut self, ws_id: Uuid, cx: &mut Context<Self>) -> Entity<Pane> {
        if let Some(group) = self.pane_groups.get(&ws_id)
            && let Some(pane) = group.panes().first()
        {
            return pane.clone();
        }
        let pane = self.create_pane(cx);
        self.pane_groups
            .insert(ws_id, PaneGroup::single(pane.clone()));
        pane
    }

    fn split_active_pane(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws_id) = self.active_workspace_id() else {
            return;
        };
        let Some(ws) = self.state.active_workspace().cloned() else {
            return;
        };
        let Some(target) = self.focused_pane.clone() else {
            return;
        };
        if !self.pane_groups.contains_key(&ws_id) {
            return;
        }

        // Create a new pane with a terminal
        let new_pane = self.create_pane(cx);
        let tab_id = Uuid::new_v4();
        let cwd = self.state.workspace_working_dir(&ws);
        let (terminal, session_id) = self.create_terminal(tab_id, ws.id, cwd, window, cx);
        new_pane.update(cx, |p, cx| {
            p.add_item(
                tab_id,
                Box::new(terminal),
                TabMetadata::new(
                    "terminal",
                    None,
                    Some(PersistedTabKind::Terminal { session_id }),
                ),
                window,
                cx,
            );
        });

        // Split the group (re-borrow after create_pane/create_terminal)
        if let Some(group) = self.pane_groups.get_mut(&ws_id) {
            group.split(&target, axis, new_pane.clone());
        }
        self.focused_pane = Some(new_pane);
        cx.notify();
    }

    fn on_pane_event(&mut self, _pane: Entity<Pane>, event: &PaneEvent, cx: &mut Context<Self>) {
        match event {
            PaneEvent::CloseItem { tab_id, kind_id } => {
                // For terminal tabs: kill daemon session immediately
                if *kind_id == "terminal" {
                    self.terminal_tabs.remove(tab_id);
                    self.pending_attach_tabs.remove(tab_id);
                    if let Some(session_id) = self.tab_sessions.remove(tab_id) {
                        self.pending_ensure_sessions.remove(&session_id);
                        if let Some(client) = &self.daemon_client {
                            client.kill(session_id).ok();
                        }
                        if let Some(ws_id) = self
                            .tab_workspace
                            .remove(tab_id)
                            .or_else(|| self.active_workspace_id())
                        {
                            self.closed_tabs.push(ClosedTabInfo {
                                tab_id: *tab_id,
                                workspace_id: ws_id,
                                title: "Terminal".into(),
                            });
                            while self.closed_tabs.len() > MAX_CLOSED_TABS {
                                self.closed_tabs.remove(0);
                            }
                        } else {
                            tracing::warn!(tab_id = %tab_id, "closed tab has no workspace; skipping closed_tabs");
                        }
                    }
                }
                self.settings_subscriptions.remove(tab_id);
                self.schedule_serialize(cx);
            }
            PaneEvent::Empty => {
                // Remove empty pane from group
                if let Some(ws_id) = self.active_workspace_id()
                    && let Some(group) = self.pane_groups.get_mut(&ws_id)
                {
                    group.remove(&_pane);
                }
                if self.focused_pane.as_ref() == Some(&_pane) {
                    self.focused_pane = None;
                }
                self.schedule_serialize(cx);
            }
            PaneEvent::ActivateItem(tab_id) => {
                self.attach_terminal_tab_if_pending(*tab_id, cx);
                self.schedule_serialize(cx);
            }
            PaneEvent::NewTabRequested | PaneEvent::ItemAdded => {
                self.schedule_serialize(cx);
            }
        }
        cx.notify();
    }

    fn save_state(&mut self, cx: &App) {
        self.state.closed_tabs = self.closed_tabs.clone();
        // Build canonical tab persistence from PaneGroup → Pane.tabs
        // (preserves visual order, flattened across panes).
        let mut workspace_tabs: HashMap<Uuid, PersistedWorkspaceTabs> = HashMap::new();
        for (&ws_id, group) in &self.pane_groups {
            let mut tabs = Vec::new();
            let mut active_tab_id = None;
            for pane in group.panes() {
                let pane_ref = pane.read(cx);
                let pane_tabs = crate::pane::TabEntry::persisted_tabs(&pane_ref.tabs);
                if active_tab_id.is_none()
                    && let Some(active_id) = pane_ref.active_tab_id
                    && pane_tabs.iter().any(|tab| tab.id == active_id)
                {
                    active_tab_id = Some(active_id);
                }
                tabs.extend(pane_tabs);
            }
            if !tabs.is_empty() {
                workspace_tabs.insert(
                    ws_id,
                    PersistedWorkspaceTabs {
                        tabs,
                        active_tab_id,
                    },
                );
            }
        }
        self.state.workspace_tabs = workspace_tabs;
        if let Err(e) = persistence::save_state(&self.state) {
            tracing::error!("Failed to save state: {e}");
        }
    }

    fn cleanup_workspace_tabs(&mut self, ws_id: Uuid) {
        // Collect tab IDs belonging to this workspace
        let orphan_tab_ids: Vec<Uuid> = self
            .tab_workspace
            .iter()
            .filter(|&(_, &ws)| ws == ws_id)
            .map(|(tab_id, _)| *tab_id)
            .collect();

        // Kill daemon sessions for orphaned tabs
        for tab_id in &orphan_tab_ids {
            if let Some(session_id) = self.tab_sessions.remove(tab_id)
                && let Some(client) = &self.daemon_client
            {
                client.kill(session_id).ok();
            }
            self.terminal_tabs.remove(tab_id);
        }

        self.pane_groups.remove(&ws_id);
        self.tab_workspace.retain(|_, ws| *ws != ws_id);
        self.closed_tabs.retain(|ct| ct.workspace_id != ws_id);
    }

    fn new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.state.active_workspace().cloned() else {
            return;
        };

        let tab_id = Uuid::new_v4();
        let cwd = self.state.workspace_working_dir(&ws);
        let (terminal, session_id) = self.create_terminal(tab_id, ws.id, cwd, window, cx);
        let pane = self.get_or_create_pane(ws.id, cx);
        pane.update(cx, |p, cx| {
            p.add_item(
                tab_id,
                Box::new(terminal),
                TabMetadata::new(
                    "terminal",
                    None,
                    Some(PersistedTabKind::Terminal { session_id }),
                ),
                window,
                cx,
            );
        });
        cx.notify();
    }

    fn close_active_tab(
        &mut self,
        _: &CloseActiveTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws_id) = self.active_workspace_id() else {
            return;
        };
        let Some(pane) = self
            .focused_pane
            .clone()
            .or_else(|| self.pane_groups.get(&ws_id)?.panes().into_iter().next())
        else {
            return;
        };
        pane.update(cx, |p, cx| {
            p.close_active_item(window, cx);
        });
    }

    /// Reopen the most recently closed terminal tab in the active workspace (Cmd+Shift+T).
    fn reopen_closed_tab(
        &mut self,
        _: &ReopenClosedTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws_id) = self.active_workspace_id() else {
            return;
        };

        // Find most recent closed tab for this workspace
        let Some(idx) = self
            .closed_tabs
            .iter()
            .rposition(|e| e.workspace_id == ws_id)
        else {
            return;
        };
        let entry = self.closed_tabs.remove(idx);

        let cwd = self
            .state
            .workspaces
            .iter()
            .find(|w| w.id == ws_id)
            .and_then(|w| self.state.workspace_working_dir(w));

        // Create fresh terminal (session was already killed on close)
        let (terminal, session_id) = self.create_terminal(entry.tab_id, ws_id, cwd, window, cx);
        let pane = self.get_or_create_pane(ws_id, cx);
        pane.update(cx, |p, cx| {
            p.add_item(
                entry.tab_id,
                Box::new(terminal),
                TabMetadata::new(
                    "terminal",
                    None,
                    Some(PersistedTabKind::Terminal { session_id }),
                ),
                window,
                cx,
            );
        });

        self.schedule_serialize(cx);
        cx.notify();
    }

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, _window: &mut Window, cx: &mut Context<Self>) {
        self.state.sidebar_collapsed = !self.state.sidebar_collapsed;
        self.schedule_serialize(cx);
        cx.notify();
    }

    fn toggle_file_tree(
        &mut self,
        _: &ToggleFileTree,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.right_sidebar_collapsed = !self.state.right_sidebar_collapsed;
        self.schedule_serialize(cx);
        cx.notify();
    }

    fn split_right(&mut self, _: &SplitRight, window: &mut Window, cx: &mut Context<Self>) {
        self.split_active_pane(Axis::Horizontal, window, cx);
    }

    fn split_down(&mut self, _: &SplitDown, window: &mut Window, cx: &mut Context<Self>) {
        self.split_active_pane(Axis::Vertical, window, cx);
    }

    fn select_workspace(&mut self, ws_id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        self.state.active_workspace_id = Some(ws_id);
        self.focused_pane = None;
        self.send_active_workspace_focus();

        // Refresh MainBranch's `branch` field from live HEAD before activation —
        // user may have run `git checkout` in another tool since last touch.
        self.refresh_main_branch_label(ws_id);

        let ws = self
            .state
            .workspaces
            .iter()
            .find(|w| w.id == ws_id)
            .cloned();
        if let Some(ws) = &ws {
            if let Some(file_tree) = &self.file_tree
                && let Some(path) = self.state.workspace_working_dir(ws)
            {
                file_tree.update(cx, |ft, cx| {
                    ft.set_root_path(Some(path), cx);
                });
            }
            self.ensure_workspace_has_tab(ws, window, cx);
        }

        // Reinitialize git provider for the new workspace
        self.init_git_provider(window, cx);

        self.focus_active_pane_item(window, cx);
        self.attach_active_terminal_if_pending(cx);
        self.save_state(cx);
        cx.notify();
    }

    fn add_project_action(&mut self, _: &AddProject, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });

        cx.spawn_in(window, async |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };

            this.update(cx, |this, cx| {
                this.do_add_project(&path, cx);
            })
            .ok();
        })
        .detach();
    }

    fn do_add_project(&mut self, path: &PathBuf, cx: &mut Context<Self>) {
        match Project::register(path) {
            Ok(project) => {
                if self.state.projects.iter().any(|p| p.path == project.path) {
                    tracing::warn!("Project already registered: {}", project.path.display());
                    return;
                }
                let main_ws = Workspace::main_branch(&project);
                self.state.projects.push(project);
                self.state.workspaces.push(main_ws);
                self.save_state(cx);
            }
            Err(e) => {
                tracing::error!("Failed to register project: {e}");
            }
        }
        cx.notify();
    }

    /// Re-read HEAD for `ws_id` if it's a MainBranch workspace and update the
    /// stored `branch` so the sidebar label reflects the current checkout.
    /// No-op for Worktree workspaces (their branch is pinned at creation).
    fn refresh_main_branch_label(&mut self, ws_id: Uuid) {
        let Some(ws_idx) = self
            .state
            .workspaces
            .iter()
            .position(|w| w.id == ws_id && w.kind == WorkspaceKind::MainBranch)
        else {
            return;
        };
        let project_id = self.state.workspaces[ws_idx].project_id;
        let Some(project_path) = self.state.project_by_id(project_id).map(|p| p.path.clone())
        else {
            return;
        };
        let runner = seoul_workspace::git::GitCommandRunner::new(&project_path);
        if let Ok(Some(branch)) = seoul_workspace::git::branch::current_branch(&runner) {
            self.state.workspaces[ws_idx].branch = branch;
        }
    }

    fn start_create_workspace(
        &mut self,
        project_id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self
            .state
            .projects
            .iter()
            .find(|p| p.id == project_id)
            .cloned()
        else {
            return;
        };

        let existing_names: Vec<String> = self
            .state
            .workspaces
            .iter()
            .filter(|ws| ws.project_id == project_id)
            .map(|ws| ws.name.clone())
            .collect();

        let generated =
            seoul_workspace::workspace::generate_workspace_name(&project.path, &existing_names);

        let branch_input = cx.new(|cx| BranchInput::new(generated.clone(), "branch name", cx));
        let subscription = cx.subscribe_in(
            &branch_input,
            window,
            move |this, _input, ev: &BranchInputEvent, window, cx| match ev {
                BranchInputEvent::Submitted => {
                    this.confirm_create_workspace(window, cx);
                }
                BranchInputEvent::Cancelled => {
                    this.new_ws_prompt = None;
                    cx.notify();
                }
            },
        );

        // Auto-focus the input so the user can start typing immediately.
        let focus_handle = branch_input.read(cx).focus_handle(cx);
        focus_handle.focus(window, cx);

        self.new_ws_prompt = Some(NewWorkspacePrompt {
            project_id,
            generated_name: generated,
            branch_input,
            subscription,
        });
        cx.notify();
    }

    fn confirm_create_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.new_ws_prompt.take() else {
            return;
        };
        let Some(project) = self
            .state
            .projects
            .iter()
            .find(|p| p.id == prompt.project_id)
            .cloned()
        else {
            return;
        };

        let entered = prompt.branch_input.read(cx).text().trim().to_string();
        let branch = if entered.is_empty() {
            prompt.generated_name.clone()
        } else {
            entered
        };
        let name = prompt.generated_name;

        match Workspace::create(&project, &name, &branch) {
            Ok(ws) => {
                let ws_id = ws.id;
                self.register_workspace_with_daemon(&ws);
                self.state.workspaces.push(ws);
                self.state.active_workspace_id = Some(ws_id);
                self.send_active_workspace_focus();
                self.save_state(cx);

                let ws = self
                    .state
                    .workspaces
                    .iter()
                    .find(|w| w.id == ws_id)
                    .cloned();
                if let Some(ws) = &ws {
                    if let Some(file_tree) = &self.file_tree
                        && let Some(path) = self.state.workspace_working_dir(ws)
                    {
                        file_tree.update(cx, |ft, cx| {
                            ft.set_root_path(Some(path), cx);
                        });
                    }
                    self.ensure_workspace_has_tab(ws, window, cx);
                }
                self.focus_active_pane_item(window, cx);
                self.attach_active_terminal_if_pending(cx);
                self.sync_workspace_names(cx);
                self.toast.update(cx, |t, cx| {
                    t.show(
                        format!("Workspace {name} 생성 완료"),
                        ToastKind::Success,
                        cx,
                    );
                });
            }
            Err(e) => {
                tracing::error!("Failed to create workspace: {e}");
                self.toast.update(cx, |t, cx| {
                    t.show(format!("Workspace 생성 실패: {e}"), ToastKind::Error, cx);
                });
            }
        }
        cx.notify();
    }

    fn start_delete_workspace(&mut self, ws_id: Uuid, cx: &mut Context<Self>) {
        self.pending_delete_ws = Some(ws_id);
        cx.notify();
    }

    fn confirm_delete_workspace(
        &mut self,
        ws_id: Uuid,
        remove_worktree: bool,
        cx: &mut Context<Self>,
    ) {
        self.pending_delete_ws = None;

        let ws = self
            .state
            .workspaces
            .iter()
            .find(|w| w.id == ws_id)
            .cloned();
        let project = ws.as_ref().and_then(|ws| {
            self.state
                .projects
                .iter()
                .find(|p| p.id == ws.project_id)
                .cloned()
        });

        if remove_worktree
            && let (Some(ws), Some(project)) = (&ws, &project)
            && let Err(e) = ws.remove_with_branch(project)
        {
            tracing::error!("Failed to remove worktree: {e}");
        }

        let ws_name = ws.as_ref().map(|w| w.name.clone()).unwrap_or_default();
        self.cleanup_workspace_tabs(ws_id);
        self.unregister_workspace_with_daemon(ws_id);
        self.pr_status_by_workspace.remove(&ws_id);
        self.pr_unavailable_by_workspace.remove(&ws_id);
        self.state.workspaces.retain(|w| w.id != ws_id);
        if self.state.active_workspace_id == Some(ws_id) {
            self.state.active_workspace_id = None;
            self.send_active_workspace_focus();
            if let Some(file_tree) = &self.file_tree {
                file_tree.update(cx, |ft, cx| {
                    ft.set_root_path(None, cx);
                });
            }
        }

        self.save_state(cx);
        self.sync_workspace_names(cx);
        let msg = if remove_worktree {
            format!("{ws_name} 삭제 완료 (worktree 제거)")
        } else {
            format!("{ws_name} 삭제 완료 (worktree 유지)")
        };
        self.toast
            .update(cx, |t, cx| t.show(msg, ToastKind::Info, cx));
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // File tree event handling
    // -----------------------------------------------------------------------

    fn on_file_tree_event(
        &mut self,
        _file_tree: Entity<FileTreeView>,
        event: &FileTreeEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            FileTreeEvent::FileSelected(path) => {
                self.open_file_from_tree(path.clone(), cx);
            }
        }
    }

    /// Open a file from the file tree (no Window access — subscribe callback).
    fn open_file_from_tree(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(ws_id) = self.active_workspace_id() else {
            return;
        };

        let pane = self.get_or_create_pane(ws_id, cx);

        // Check if already open — activate existing tab
        if let Some(tab_id) = pane.read(cx).find_tab_by_path("editor", &path) {
            // open_file_from_tree has no Window — just set active, let next render pick it up
            pane.update(cx, |p, _cx| {
                p.active_tab_id = Some(tab_id);
            });
            self.schedule_serialize(cx);
            cx.notify();
            return;
        }

        // Create new editor tab
        let tab_id = Uuid::new_v4();
        let editor = self.create_editor_view(path.clone(), cx);

        pane.update(cx, |p, cx| {
            p.add_item_without_focus(
                tab_id,
                Box::new(editor),
                TabMetadata::new(
                    "editor",
                    Some(path.clone()),
                    Some(PersistedTabKind::Editor { path }),
                ),
                cx,
            );
        });
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Git integration
    // -----------------------------------------------------------------------

    fn init_git_provider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.state.active_workspace() else {
            return;
        };
        let Some(project) = self.state.projects.iter().find(|p| p.id == ws.project_id) else {
            return;
        };

        let worktree_path = ws.working_dir(project).to_path_buf();
        let default_branch = project.default_branch.clone();

        let provider = cx.new(|cx| GitStateProvider::new(worktree_path, default_branch, cx));
        let sub = cx.subscribe(&provider, Self::on_git_state_changed);
        self.git_provider = Some(provider);
        self._git_subscription = Some(sub);

        let panel = cx.new(|cx| crate::git_panel_view::GitPanelView::new(window, cx));
        let panel_sub = cx.subscribe(&panel, Self::on_git_panel_event);
        self.git_panel = Some(panel);
        self._git_panel_subscription = Some(panel_sub);
    }

    fn on_git_state_changed(
        &mut self,
        provider: Entity<GitStateProvider>,
        event: &GitStateEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            GitStateEvent::StatusChanged => {
                let status = provider.read(cx).status().clone();
                let is_busy = provider.read(cx).is_busy();

                if let Some(file_tree) = &self.file_tree {
                    let status_map = provider.read(cx).file_status_map();
                    file_tree.update(cx, |ft, cx| ft.set_git_status(status_map, cx));
                }
                if let Some(panel) = &self.git_panel {
                    panel.update(cx, |p, cx| {
                        p.set_status(status, cx);
                        p.set_busy(is_busy, cx);
                    });
                }
            }
            GitStateEvent::OperationSuccess { message } => {
                self.toast.update(cx, |t, cx| {
                    t.show(message.clone(), ToastKind::Success, cx);
                });
            }
            GitStateEvent::OperationError { message } => {
                self.toast.update(cx, |t, cx| {
                    t.show(message.clone(), ToastKind::Error, cx);
                });
            }
        }
        cx.notify();
    }

    fn on_git_panel_event(
        &mut self,
        _panel: Entity<crate::git_panel_view::GitPanelView>,
        event: &crate::git_panel_view::GitPanelEvent,
        cx: &mut Context<Self>,
    ) {
        use crate::git_panel_view::GitPanelEvent;

        let Some(provider) = &self.git_provider else {
            return;
        };

        match event {
            GitPanelEvent::StageFile(path) => {
                provider.update(cx, |p, cx| p.stage_file(path, cx));
            }
            GitPanelEvent::UnstageFile(path) => {
                provider.update(cx, |p, cx| p.unstage_file(path, cx));
            }
            GitPanelEvent::DiscardFile(path) => {
                provider.update(cx, |p, cx| p.discard_file(path, cx));
            }
            GitPanelEvent::StageAll => {
                provider.update(cx, |p, cx| p.stage_all(cx));
            }
            GitPanelEvent::UnstageAll => {
                provider.update(cx, |p, cx| p.unstage_all(cx));
            }
            GitPanelEvent::Commit(msg) => {
                provider.update(cx, |p, cx| p.commit(msg, cx));
            }
            GitPanelEvent::Push => {
                provider.update(cx, |p, cx| p.push(cx));
            }
            GitPanelEvent::Pull => {
                provider.update(cx, |p, cx| p.pull(cx));
            }
            GitPanelEvent::Sync => {
                provider.update(cx, |p, cx| p.sync(cx));
            }
            GitPanelEvent::Fetch => {
                provider.update(cx, |p, cx| p.fetch(cx));
            }
            GitPanelEvent::OpenDiff { path, category } => {
                self.open_diff_tab(path.clone(), *category, cx);
            }
        }
    }

    fn open_diff_tab(
        &mut self,
        path: String,
        category: seoul_workspace::git::types::ChangeCategory,
        cx: &mut Context<Self>,
    ) {
        let Some(ws_id) = self.active_workspace_id() else {
            return;
        };
        let Some(workspace) = self.state.active_workspace().cloned() else {
            return;
        };

        let diff_text = self.diff_text_for_workspace(&workspace, &path, category);

        let tab_id = Uuid::new_v4();
        let view_path = path.clone();
        let diff_view =
            cx.new(move |cx| crate::diff_view::DiffView::new(cx, view_path, category, diff_text));

        let pane = self.get_or_create_pane(ws_id, cx);
        pane.update(cx, |p, cx| {
            p.add_item_without_focus(
                tab_id,
                Box::new(diff_view),
                TabMetadata::new(
                    "diff",
                    None,
                    Some(PersistedTabKind::Diff { path, category }),
                ),
                cx,
            );
        });
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Rendering
    // -----------------------------------------------------------------------

    fn render_resize_handle(&self, side: SidebarSide, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::theme(cx);
        let surface1 = t.surface1;
        let id_str = match side {
            SidebarSide::Left => "left-sidebar-resize-handle",
            SidebarSide::Right => "right-sidebar-resize-handle",
        };

        let handle = div()
            .id(id_str)
            .absolute()
            .top(px(0.))
            .h_full()
            .w(px(RESIZE_HANDLE_SIZE))
            .cursor_col_resize()
            .occlude()
            .hover(|s| s.bg(rgb(surface1)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                    let start_width = match side {
                        SidebarSide::Left => this.state.sidebar_width,
                        SidebarSide::Right => this.state.right_sidebar_width,
                    };
                    this.resize_drag = Some(ResizeDragState {
                        start_width,
                        start_pointer_x: ev.position.x,
                    });
                    cx.stop_propagation();
                }),
            )
            .on_drag(ResizeSidebar(side), |payload, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| payload.clone())
            });

        let positioned = match side {
            SidebarSide::Left => handle.right(px(-RESIZE_HANDLE_SIZE / 2.)),
            SidebarSide::Right => handle.left(px(-RESIZE_HANDLE_SIZE / 2.)),
        };
        deferred(positioned)
    }

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = theme::theme(cx);

        if self.state.sidebar_collapsed {
            return div()
                .id("sidebar-collapsed")
                .w(px(0.))
                .h_full()
                .into_any_element();
        }

        let width = self.state.sidebar_width;

        let mut sidebar = div()
            .id("sidebar")
            .w(px(width))
            .min_w(px(width))
            .h_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(t.mantle))
            .border_r_1()
            .border_color(rgb(t.surface0))
            .overflow_y_scroll()
            // Header
            .child(
                div()
                    .flex_none()
                    .px(px(12.))
                    .py(px(10.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(t.overlay0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("PROJECTS"),
                    )
                    .child(
                        div()
                            .id("add-project-btn")
                            .cursor_pointer()
                            .px(px(6.))
                            .py(px(2.))
                            .rounded(px(3.))
                            .hover(|s| s.bg(rgb(t.surface0)))
                            .child(Icon::new(IconName::Plus, rgb(t.overlay0)).size(px(14.)))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.add_project_action(&AddProject, window, cx);
                            })),
                    ),
            );

        // Projects list — clone needed because render_project_section borrows cx mutably
        let projects: Vec<_> = self.state.projects.clone();
        for project in &projects {
            sidebar = sidebar.child(self.render_project_section(project, cx));
        }

        // Empty state
        if self.state.projects.is_empty() {
            sidebar = sidebar.child(
                div()
                    .px(px(12.))
                    .py(px(20.))
                    .text_size(px(12.))
                    .text_color(rgb(t.surface2))
                    .child("No projects yet.")
                    .child(
                        div()
                            .pt(px(4.))
                            .text_color(rgb(t.overlay0))
                            .child("Click + to add one."),
                    ),
            );
        }

        sidebar
            .child(self.render_resize_handle(SidebarSide::Left, cx))
            .into_any_element()
    }

    fn render_project_section(&self, project: &Project, cx: &mut Context<Self>) -> AnyElement {
        let t = theme::theme(cx);
        let project_id = project.id;
        let is_collapsed = self.collapsed_projects.contains(&project_id);
        // Partition: MainBranch first (pinned), worktrees follow in their natural order.
        let (mains, worktrees): (Vec<&Workspace>, Vec<&Workspace>) = self
            .state
            .workspaces_for_project(project_id)
            .into_iter()
            .partition(|w| w.kind == WorkspaceKind::MainBranch);
        let workspaces: Vec<&Workspace> = mains.into_iter().chain(worktrees).collect();

        let mut section = div()
            .id(ElementId::Name(format!("project-{project_id}").into()))
            .flex()
            .flex_col()
            // Project header
            .child(
                div()
                    .id(ElementId::Name(
                        format!("project-header-{project_id}").into(),
                    ))
                    .px(px(8.))
                    .py(px(5.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(t.surface0)))
                    .rounded(px(3.))
                    .mx(px(4.))
                    .child(
                        div()
                            .w(px(12.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Icon::new(
                                    if is_collapsed {
                                        IconName::ChevronRight
                                    } else {
                                        IconName::ChevronDown
                                    },
                                    rgb(t.overlay0),
                                )
                                .size(px(12.)),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(t.text))
                            .font_weight(FontWeight::MEDIUM)
                            .flex_1()
                            .child(project.name.clone()),
                    )
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        if let Some(pos) = this
                            .collapsed_projects
                            .iter()
                            .position(|&id| id == project_id)
                        {
                            this.collapsed_projects.remove(pos);
                        } else {
                            this.collapsed_projects.push(project_id);
                        }
                        cx.notify();
                    })),
            );

        if !is_collapsed {
            // Workspace items
            for ws in &workspaces {
                let ws_id = ws.id;
                let is_active = self.state.active_workspace_id == Some(ws_id);
                let is_main = ws.kind == WorkspaceKind::MainBranch;
                // MainBranch shows the live HEAD branch; falls back to "(detached)"
                // when current_branch() returned None at activation time.
                let label = if is_main {
                    if ws.branch.is_empty() {
                        "(detached)".to_string()
                    } else {
                        ws.branch.clone()
                    }
                } else {
                    ws.name.clone()
                };

                let pr_badge = self.pr_status_by_workspace.get(&ws_id).cloned();
                let mut row = div()
                    .id(ElementId::Name(format!("ws-{ws_id}").into()))
                    .pl(px(28.))
                    .pr(px(8.))
                    .py(px(4.))
                    .mx(px(4.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .cursor_pointer()
                    .rounded(px(3.))
                    .when(is_active, |el: Stateful<Div>| el.bg(rgb(t.surface0)))
                    .hover(|s: StyleRefinement| s.bg(rgb(t.surface1)))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                Icon::new(
                                    if is_main {
                                        IconName::GitBranch
                                    } else {
                                        IconName::GitMerge
                                    },
                                    rgb(t.overlay0),
                                )
                                .size(px(13.)),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(if is_active {
                                        rgb(t.text)
                                    } else {
                                        rgb(t.subtext0)
                                    })
                                    .child(label),
                            )
                            .when_some(pr_badge, |d, pr| d.child(render_pr_badge(&pr, &t))),
                    );

                // Delete button — only for Worktree. MainBranch is pinned and not
                // user-deletable; ensure_main_workspaces would resurrect it anyway.
                if !is_main {
                    row = row.child(
                        div()
                            .id(ElementId::Name(format!("ws-del-{ws_id}").into()))
                            .cursor_pointer()
                            .text_size(px(10.))
                            .text_color(rgb(t.surface2))
                            .px(px(4.))
                            .rounded(px(2.))
                            .hover(|s| s.bg(rgb(t.surface2)).text_color(rgb(t.text)))
                            .child(Icon::new(IconName::X, rgb(t.surface2)).size(px(12.)))
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.start_delete_workspace(ws_id, cx);
                            })),
                    );
                }

                section = section.child(row.on_click(cx.listener(move |this, _, window, cx| {
                    this.select_workspace(ws_id, window, cx);
                })));

                // Delete confirmation inline
                if self.pending_delete_ws == Some(ws_id) {
                    section = section.child(
                        div()
                            .id(ElementId::Name(format!("ws-del-confirm-{ws_id}").into()))
                            .pl(px(28.))
                            .pr(px(8.))
                            .py(px(4.))
                            .mx(px(4.))
                            .bg(rgb(t.surface0))
                            .rounded(px(3.))
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(rgb(t.overlay2))
                                    .child("Worktree도 삭제할까요?"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap(px(4.))
                                    .child(
                                        div()
                                            .id(ElementId::Name(
                                                format!("ws-del-yes-{ws_id}").into(),
                                            ))
                                            .px(px(6.))
                                            .py(px(2.))
                                            .bg(rgb(t.red))
                                            .rounded(px(2.))
                                            .cursor_pointer()
                                            .text_size(px(10.))
                                            .text_color(rgb(t.mantle))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .hover(|s| s.opacity(0.8))
                                            .child("삭제")
                                            .on_click(cx.listener(move |this, _, _window, cx| {
                                                this.confirm_delete_workspace(ws_id, true, cx);
                                            })),
                                    )
                                    .child(
                                        div()
                                            .id(ElementId::Name(
                                                format!("ws-del-keep-{ws_id}").into(),
                                            ))
                                            .px(px(6.))
                                            .py(px(2.))
                                            .bg(rgb(t.surface1))
                                            .rounded(px(2.))
                                            .cursor_pointer()
                                            .text_size(px(10.))
                                            .text_color(rgb(t.subtext0))
                                            .hover(|s| s.bg(rgb(t.surface2)))
                                            .child("유지")
                                            .on_click(cx.listener(move |this, _, _window, cx| {
                                                this.confirm_delete_workspace(ws_id, false, cx);
                                            })),
                                    )
                                    .child(
                                        div()
                                            .id(ElementId::Name(
                                                format!("ws-del-cancel-{ws_id}").into(),
                                            ))
                                            .px(px(6.))
                                            .py(px(2.))
                                            .cursor_pointer()
                                            .text_size(px(10.))
                                            .text_color(rgb(t.surface2))
                                            .hover(|s| s.text_color(rgb(t.subtext0)))
                                            .child("취소")
                                            .on_click(cx.listener(move |this, _, _window, cx| {
                                                this.pending_delete_ws = None;
                                                cx.notify();
                                            })),
                                    ),
                            ),
                    );
                }
            }

            // New workspace prompt
            if let Some(prompt) = &self.new_ws_prompt
                && prompt.project_id == project_id
            {
                let branch_input = prompt.branch_input.clone();
                section = section.child(
                    div()
                        .id("new-ws-prompt")
                        .pl(px(28.))
                        .pr(px(8.))
                        .py(px(4.))
                        .mx(px(4.))
                        .bg(rgb(t.surface0))
                        .rounded(px(3.))
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(rgb(t.overlay2))
                                .child("Branch name:"),
                        )
                        .child(branch_input)
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap(px(4.))
                                .child(
                                    div()
                                        .id("new-ws-confirm")
                                        .px(px(6.))
                                        .py(px(2.))
                                        .bg(rgb(t.blue))
                                        .rounded(px(2.))
                                        .cursor_pointer()
                                        .text_size(px(10.))
                                        .text_color(rgb(t.mantle))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .hover(|s| s.opacity(0.8))
                                        .child("생성")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.confirm_create_workspace(window, cx);
                                        })),
                                )
                                .child(
                                    div()
                                        .id("new-ws-cancel")
                                        .px(px(6.))
                                        .py(px(2.))
                                        .cursor_pointer()
                                        .text_size(px(10.))
                                        .text_color(rgb(t.surface2))
                                        .hover(|s| s.text_color(rgb(t.subtext0)))
                                        .child("취소")
                                        .on_click(cx.listener(|this, _, _window, cx| {
                                            this.new_ws_prompt = None;
                                            cx.notify();
                                        })),
                                ),
                        ),
                );
            }

            // "New Workspace" button (hidden when prompt is active)
            if self
                .new_ws_prompt
                .as_ref()
                .is_none_or(|p| p.project_id != project_id)
            {
                section = section.child(
                    div()
                        .id(ElementId::Name(format!("new-ws-btn-{project_id}").into()))
                        .pl(px(28.))
                        .pr(px(8.))
                        .py(px(3.))
                        .mx(px(4.))
                        .cursor_pointer()
                        .rounded(px(3.))
                        .hover(|s| s.bg(rgb(t.surface0)))
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(t.surface2))
                                .child("+ New Workspace"),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.start_create_workspace(project_id, window, cx);
                        })),
                );
            }
        }

        section.into_any_element()
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = theme::theme(cx);

        let mut bar = div()
            .id("status-bar")
            .flex_none()
            .h(px(24.))
            .w_full()
            .px(px(8.))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .bg(rgb(t.mantle))
            .border_t_1()
            .border_color(rgb(t.surface0))
            .text_size(px(11.));

        // Left: git info
        if let Some(provider) = &self.git_provider {
            let status = provider.read(cx).status();
            let ws_branch;
            let branch = if status.branch == "HEAD" || status.branch == "(detached)" {
                ws_branch = self
                    .state
                    .active_workspace()
                    .map(|ws| ws.branch.clone())
                    .unwrap_or_else(|| status.branch.clone());
                &ws_branch
            } else {
                &status.branch
            };

            let mut git_info = div().flex().flex_row().items_center().gap(px(8.));

            // Branch name
            git_info = git_info.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(3.))
                    .child(Icon::new(IconName::GitBranch, rgb(t.blue)).size(px(12.)))
                    .child(div().text_color(rgb(t.text)).child(branch.clone())),
            );

            // Ahead/behind
            if status.ahead > 0 || status.behind > 0 {
                let mut counts = div().flex().flex_row().gap(px(4.));
                if status.ahead > 0 {
                    counts = counts.child(
                        div()
                            .text_color(rgb(t.yellow))
                            .child(format!("\u{2191}{}", status.ahead)),
                    );
                }
                if status.behind > 0 {
                    counts = counts.child(
                        div()
                            .text_color(rgb(t.yellow))
                            .child(format!("\u{2193}{}", status.behind)),
                    );
                }
                git_info = git_info.child(counts);
            }

            // Staged/unstaged summary
            let staged = status.staged.len();
            let unstaged = status.unstaged.len();
            if staged > 0 || unstaged > 0 {
                let mut summary = div().flex().flex_row().gap(px(4.));
                if staged > 0 {
                    summary =
                        summary.child(div().text_color(rgb(t.peach)).child(format!("+{staged}")));
                }
                if unstaged > 0 {
                    summary =
                        summary.child(div().text_color(rgb(t.peach)).child(format!("~{unstaged}")));
                }
                git_info = git_info.child(summary);
            }

            bar = bar.child(git_info);
        } else {
            bar = bar.child(div().text_color(rgb(t.surface2)).child("No git"));
        }

        let daemon_status = if !self.daemon_connected {
            Some(format!(
                "Daemon reconnecting · {} session(s) paused",
                self.tab_sessions.len()
            ))
        } else if self.pending_recoveries > 0 {
            Some(format!("Restoring {} session(s)…", self.pending_recoveries))
        } else {
            None
        };

        let mut right = div().flex().flex_row().items_center().gap(px(8.));

        if let Some(status) = daemon_status {
            right = right.child(
                div()
                    .px(px(6.))
                    .py(px(2.))
                    .rounded(px(4.))
                    .bg(rgb(t.surface0))
                    .text_color(rgb(t.yellow))
                    .child(status),
            );
        }

        if let Some(indicator) = &self.resource_indicator {
            right = right.child(indicator.clone());
        }

        bar = bar.child(right);

        bar.into_any_element()
    }

    fn render_pane_area(&self, _cx: &mut Context<Self>) -> AnyElement {
        if let Some(ws_id) = self.active_workspace_id()
            && let Some(group) = self.pane_groups.get(&ws_id)
        {
            return group.render(_cx);
        }

        // No active pane — empty state
        let t = theme::theme(_cx);
        div()
            .id("content-empty")
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(16.))
                            .text_color(rgb(t.surface2))
                            .child("Seoul"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(t.surface1))
                            .child("Add a project and create a workspace to start."),
                    ),
            )
            .into_any_element()
    }

    fn render_pr_card(
        &self,
        workspace_id: Option<Uuid>,
        pr: Option<PrInfo>,
        unavailable: Option<PrUnavailableReason>,
        t: &theme::ThemeColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let card = div()
            .id("pr-card")
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(4.))
            .px(px(8.))
            .py(px(8.))
            .border_b_1()
            .border_color(rgb(t.surface0));

        // Unavailable state takes priority — only render the explanation card.
        if let Some(reason) = unavailable {
            let (line1, line2) = match reason {
                PrUnavailableReason::GhNotInstalled => (
                    "GitHub CLI not installed".to_string(),
                    Some("Run: brew install gh && gh auth login".to_string()),
                ),
                PrUnavailableReason::NotAuthenticated => (
                    "GitHub not authenticated".to_string(),
                    Some("Run: gh auth login".to_string()),
                ),
                PrUnavailableReason::RateLimited { reset_unix } => (
                    "GitHub rate limit hit".to_string(),
                    Some(format!("Resets at unix {reset_unix}")),
                ),
                PrUnavailableReason::UnsupportedHost { host } => {
                    (format!("Hosting '{host}' not supported"), None)
                }
                PrUnavailableReason::Network => ("Network error fetching PR".to_string(), None),
                PrUnavailableReason::Other { message } => {
                    ("PR sync error".to_string(), Some(message))
                }
            };
            return card
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgb(t.subtext0))
                        .child(line1),
                )
                .when_some(line2, |d, l| {
                    d.child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(t.overlay2))
                            .child(l),
                    )
                })
                .into_any_element();
        }

        let Some(workspace_id) = workspace_id else {
            return card
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgb(t.overlay2))
                        .child("No active workspace"),
                )
                .into_any_element();
        };

        let Some(pr) = pr else {
            // No PR for this branch.
            return card
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(t.overlay2))
                                .child("No PR for this branch"),
                        )
                        .child(
                            div()
                                .id("pr-card-refresh")
                                .px(px(6.))
                                .py(px(2.))
                                .rounded(px(2.))
                                .cursor_pointer()
                                .text_size(px(10.))
                                .text_color(rgb(t.subtext0))
                                .hover(|s| s.bg(rgb(t.surface1)))
                                .child("Refresh")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(ref client) = this.daemon_client {
                                        let _ = client.refresh_pr(workspace_id);
                                    }
                                    cx.notify();
                                })),
                        ),
                )
                .into_any_element();
        };

        let state_color = match pr.state {
            PrState::Open => t.green,
            PrState::Draft => t.overlay2,
            PrState::Merged => t.mauve,
            PrState::Closed => t.red,
        };
        let state_icon = match pr.state {
            PrState::Open | PrState::Draft => IconName::GitPullRequest,
            PrState::Merged => IconName::GitMerge,
            PrState::Closed => IconName::XCircle,
        };
        let review_label = match pr.review_decision {
            ReviewDecision::Approved => Some((IconName::Check, "Approved", t.green)),
            ReviewDecision::ChangesRequested => Some((IconName::X, "Changes requested", t.red)),
            ReviewDecision::ReviewRequired => Some((IconName::Info, "Review required", t.yellow)),
            ReviewDecision::None => None,
        };
        let checks_label = match pr.checks_status {
            ChecksStatus::Success => Some((IconName::Check, "Checks passing", t.green)),
            ChecksStatus::Failure => Some((IconName::X, "Checks failing", t.red)),
            ChecksStatus::Pending => Some((IconName::RefreshCw, "Checks running", t.yellow)),
            ChecksStatus::None => None,
        };

        let pr_url = pr.url.clone();
        let title_line = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.))
            .child(Icon::new(state_icon, rgb(state_color)).size(px(13.)))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(state_color))
                    .child(format!("#{}", pr.number)),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(t.text))
                    .child(pr.title.clone()),
            );

        let stats_line = div()
            .text_size(px(10.))
            .text_color(rgb(t.subtext0))
            .child(format!(
                "+{} / -{}  ·  {}",
                pr.additions, pr.deletions, pr.head_ref_name
            ));

        let mut body = card.child(title_line).child(stats_line);

        if let Some((icon, label, color)) = review_label {
            body = body.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.))
                    .child(Icon::new(icon, rgb(color)).size(px(11.)))
                    .child(div().text_size(px(10.)).text_color(rgb(color)).child(label)),
            );
        }
        if let Some((icon, label, color)) = checks_label {
            body = body.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.))
                    .child(Icon::new(icon, rgb(color)).size(px(11.)))
                    .child(div().text_size(px(10.)).text_color(rgb(color)).child(label)),
            );
        }

        let buttons = div()
            .flex()
            .flex_row()
            .gap(px(6.))
            .child(
                div()
                    .id("pr-card-open")
                    .px(px(6.))
                    .py(px(2.))
                    .rounded(px(2.))
                    .cursor_pointer()
                    .text_size(px(10.))
                    .text_color(rgb(t.subtext0))
                    .hover(|s| s.bg(rgb(t.surface1)))
                    .child("Open in browser")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.open_url(&pr_url);
                    })),
            )
            .child(
                div()
                    .id("pr-card-refresh")
                    .px(px(6.))
                    .py(px(2.))
                    .rounded(px(2.))
                    .cursor_pointer()
                    .text_size(px(10.))
                    .text_color(rgb(t.subtext0))
                    .hover(|s| s.bg(rgb(t.surface1)))
                    .child("Refresh")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(ref client) = this.daemon_client {
                            let _ = client.refresh_pr(workspace_id);
                        }
                        cx.notify();
                    })),
            );

        body.child(buttons).into_any_element()
    }

    fn render_right_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = theme::theme(cx);
        if self.state.right_sidebar_collapsed {
            return div()
                .id("right-sidebar-collapsed")
                .w(px(0.))
                .h_full()
                .into_any_element();
        }

        let has_git = self.git_panel.is_some();
        let active_tab = self.right_sidebar_tab;

        let width = self.state.right_sidebar_width;

        let mut sidebar = div()
            .id("right-sidebar")
            .w(px(width))
            .min_w(px(width))
            .h_full()
            .relative()
            .border_l_1()
            .border_color(rgb(t.surface0))
            .flex()
            .flex_col();

        // Tab bar (only when git panel exists)
        if has_git {
            sidebar = sidebar.child(
                div()
                    .id("right-sidebar-tabs")
                    .flex_none()
                    .flex()
                    .flex_row()
                    .border_b_1()
                    .border_color(rgb(t.surface0))
                    .child(self.render_right_tab(
                        "FILES",
                        RightSidebarTab::Files,
                        active_tab,
                        &t,
                        cx,
                    ))
                    .child(self.render_right_tab(
                        "CHANGES",
                        RightSidebarTab::Changes,
                        active_tab,
                        &t,
                        cx,
                    )),
            );
        }

        // Content
        sidebar = sidebar.child(
            div()
                .id("right-sidebar-content")
                .flex_1()
                .overflow_hidden()
                .child(match active_tab {
                    RightSidebarTab::Files => {
                        if let Some(ft) = &self.file_tree {
                            ft.clone().into_any_element()
                        } else {
                            div().id("empty-files").into_any_element()
                        }
                    }
                    RightSidebarTab::Changes => {
                        let active_ws = self.state.active_workspace_id;
                        let pr =
                            active_ws.and_then(|id| self.pr_status_by_workspace.get(&id).cloned());
                        let unavailable = active_ws
                            .and_then(|id| self.pr_unavailable_by_workspace.get(&id).cloned());
                        let card = self.render_pr_card(active_ws, pr, unavailable, &t, cx);
                        let panel_el = if let Some(panel) = &self.git_panel {
                            panel.clone().into_any_element()
                        } else {
                            div().id("empty-changes").into_any_element()
                        };
                        div()
                            .flex()
                            .flex_col()
                            .h_full()
                            .child(card)
                            .child(div().flex_1().overflow_hidden().child(panel_el))
                            .into_any_element()
                    }
                }),
        );

        sidebar
            .child(self.render_resize_handle(SidebarSide::Right, cx))
            .into_any_element()
    }

    fn render_right_tab(
        &self,
        label: &'static str,
        tab: RightSidebarTab,
        active: RightSidebarTab,
        t: &theme::ThemeColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_active = tab == active;
        div()
            .id(ElementId::Name(format!("right-tab-{label}").into()))
            .flex_1()
            .py(px(6.))
            .flex()
            .justify_center()
            .cursor_pointer()
            .text_size(px(10.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(if is_active {
                rgb(t.blue)
            } else {
                rgb(t.overlay0)
            })
            .when(is_active, |el: Stateful<Div>| {
                el.border_b_2().border_color(rgb(t.blue))
            })
            .hover(|s: StyleRefinement| s.text_color(rgb(t.text)))
            .child(label)
            .on_click(cx.listener(move |this, _, _window, cx| {
                this.right_sidebar_tab = tab;
                cx.notify();
            }))
            .into_any_element()
    }
}

impl Focusable for AppView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AppView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Process completed background reattachments (non-blocking: just creates TerminalView entities)
        self.process_pending_reattach_handles(_window, cx);

        let t = theme::theme(cx);
        let sidebar = self.render_sidebar(cx);
        let pane_area = self.render_pane_area(cx);
        let status_bar = self.render_status_bar(cx);
        let right_sidebar = self.render_right_sidebar(cx);

        let resource_overlay = self.resource_indicator.as_ref().and_then(|ind| {
            ind.update(cx, |ri, cx| {
                if ri.is_expanded() {
                    Some(ri.render_panel_overlay(cx))
                } else {
                    None
                }
            })
        });

        div()
            .id("app-root")
            .key_context("app")
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .flex()
            .flex_row()
            .bg(rgb(t.base))
            .text_color(rgb(t.text))
            .on_action(cx.listener(Self::new_tab))
            .on_action(cx.listener(Self::close_active_tab))
            .on_action(cx.listener(Self::reopen_closed_tab))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::toggle_file_tree))
            .on_action(cx.listener(Self::add_project_action))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(Self::split_right))
            .on_action(cx.listener(Self::split_down))
            .on_action(cx.listener(Self::quit))
            .on_drag_move(cx.listener(Self::on_resize_drag_move))
            .child(sidebar)
            .child(
                div()
                    .id("main-content")
                    .flex_1()
                    .h_full()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(pane_area)
                    .child(status_bar),
            )
            .child(right_sidebar)
            .child(self.toast.clone())
            .children(resource_overlay)
    }
}

fn render_pr_badge(pr: &PrInfo, t: &theme::ThemeColors) -> AnyElement {
    let (icon, color_hex) = match pr.state {
        PrState::Open => (IconName::GitPullRequest, t.green),
        PrState::Draft => (IconName::GitPullRequest, t.overlay2),
        PrState::Merged => (IconName::GitMerge, t.mauve),
        PrState::Closed => (IconName::XCircle, t.red),
    };
    let dot_color: Option<u32> = match pr.checks_status {
        ChecksStatus::Success => Some(t.green),
        ChecksStatus::Failure => Some(t.red),
        ChecksStatus::Pending => Some(t.yellow),
        ChecksStatus::None => None,
    };
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(2.))
        .child(Icon::new(icon, rgb(color_hex)).size(px(11.)))
        .child(
            div()
                .text_size(px(10.))
                .text_color(rgb(color_hex))
                .child(format!("#{}", pr.number)),
        )
        .when_some(dot_color, |d, c| {
            d.child(div().ml(px(2.)).size(px(5.)).rounded_full().bg(rgb(c)))
        })
        .into_any_element()
}

impl Drop for AppView {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let Some(ref client) = self.daemon_client {
            for session_id in self.tab_sessions.values() {
                client.detach(*session_id).ok();
            }
        }
        if let Err(e) = persistence::save_state(&self.state) {
            tracing::error!("Failed to save state on drop: {e}");
        }
    }
}

#[cfg(test)]
mod restore_tests {
    use super::{RestorePlanEntry, RestorePlanMode, RestoreTabCandidate, plan_restore_order};
    use uuid::Uuid;

    #[test]
    fn restore_plan_attaches_active_tab_first_and_ensures_hidden_tabs() {
        let active_ws = Uuid::new_v4();
        let other_ws = Uuid::new_v4();
        let active_tab = Uuid::new_v4();
        let hidden_active_ws_tab = Uuid::new_v4();
        let other_tab = Uuid::new_v4();
        let active_session = Uuid::new_v4();
        let hidden_session = Uuid::new_v4();
        let other_session = Uuid::new_v4();

        let entries = plan_restore_order(
            vec![
                RestoreTabCandidate {
                    tab_id: hidden_active_ws_tab,
                    session_id: hidden_session,
                    workspace_id: active_ws,
                    is_active_tab: false,
                },
                RestoreTabCandidate {
                    tab_id: other_tab,
                    session_id: other_session,
                    workspace_id: other_ws,
                    is_active_tab: true,
                },
                RestoreTabCandidate {
                    tab_id: active_tab,
                    session_id: active_session,
                    workspace_id: active_ws,
                    is_active_tab: true,
                },
            ],
            Some(active_ws),
        );

        assert_eq!(
            entries,
            vec![
                RestorePlanEntry {
                    tab_id: active_tab,
                    session_id: active_session,
                    workspace_id: active_ws,
                    mode: RestorePlanMode::Attach,
                },
                RestorePlanEntry {
                    tab_id: hidden_active_ws_tab,
                    session_id: hidden_session,
                    workspace_id: active_ws,
                    mode: RestorePlanMode::Ensure,
                },
                RestorePlanEntry {
                    tab_id: other_tab,
                    session_id: other_session,
                    workspace_id: other_ws,
                    mode: RestorePlanMode::Ensure,
                },
            ]
        );
    }

    #[test]
    fn restore_plan_falls_back_to_first_tab_when_no_active_tab_is_marked() {
        let workspace_id = Uuid::new_v4();
        let first_tab = Uuid::new_v4();
        let second_tab = Uuid::new_v4();
        let first_session = Uuid::new_v4();
        let second_session = Uuid::new_v4();

        let entries = plan_restore_order(
            vec![
                RestoreTabCandidate {
                    tab_id: first_tab,
                    session_id: first_session,
                    workspace_id,
                    is_active_tab: false,
                },
                RestoreTabCandidate {
                    tab_id: second_tab,
                    session_id: second_session,
                    workspace_id,
                    is_active_tab: false,
                },
            ],
            Some(workspace_id),
        );

        assert_eq!(entries[0].tab_id, first_tab);
        assert_eq!(entries[0].mode, RestorePlanMode::Attach);
        assert_eq!(entries[1].tab_id, second_tab);
        assert_eq!(entries[1].mode, RestorePlanMode::Ensure);
    }

    #[test]
    fn pending_terminal_requires_attach_when_workspace_becomes_active() {
        assert!(super::should_attach_pending_terminal_on_activation(
            true, false, true
        ));
        assert!(!super::should_attach_pending_terminal_on_activation(
            true, true, true
        ));
        assert!(!super::should_attach_pending_terminal_on_activation(
            true, false, false
        ));
        assert!(!super::should_attach_pending_terminal_on_activation(
            false, false, true
        ));
    }
}
