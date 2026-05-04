#![allow(dead_code)]
use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use uuid::Uuid;

use crate::daemon_client::DaemonClient;
use crate::icons::{Icon, IconName};
use crate::process_metrics::{self, SelfMetrics};
use crate::theme;

use seoul_terminal_proto::resources::{ResourceSnapshot, SessionResourceMetrics};

const MAX_HISTORY: usize = 12; // 12 samples × 2.5s = 30 seconds of sparkline

// ── Actions ─────────────────────────────────────────

actions!(resource_indicator, [ToggleResourceMonitor]);

// ── Severity ────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
enum Severity {
    Normal,
    Elevated,
    High,
}

impl Severity {
    fn color(&self, t: &theme::ThemeColors) -> u32 {
        match self {
            Severity::Normal => t.green,
            Severity::Elevated => t.yellow,
            Severity::High => t.red,
        }
    }
}

fn cpu_severity(percent: f64) -> Severity {
    if percent >= 120.0 {
        Severity::High
    } else if percent >= 70.0 {
        Severity::Elevated
    } else {
        Severity::Normal
    }
}

fn memory_severity(bytes: u64) -> Severity {
    let gb = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    if gb >= 3.0 {
        Severity::High
    } else if gb >= 1.5 {
        Severity::Elevated
    } else {
        Severity::Normal
    }
}

fn overall_severity(cpu: f64, mem: u64) -> Severity {
    let cpu_s = cpu_severity(cpu);
    let mem_s = memory_severity(mem);
    match (cpu_s, mem_s) {
        (Severity::High, _) | (_, Severity::High) => Severity::High,
        (Severity::Elevated, _) | (_, Severity::Elevated) => Severity::Elevated,
        _ => Severity::Normal,
    }
}

// ── Formatting ──────────────────────────────────────

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * KB;
    const GB: f64 = 1024.0 * MB;

    let b = bytes as f64;
    if b >= GB {
        format!("{:.1}G", b / GB)
    } else if b >= MB {
        format!("{:.0}M", b / MB)
    } else if b >= KB {
        format!("{:.0}K", b / KB)
    } else {
        format!("{}B", bytes)
    }
}

fn format_percent(p: f64) -> String {
    if p >= 100.0 {
        format!("{}%", p as u32)
    } else if p >= 10.0 {
        format!("{:.0}%", p)
    } else {
        format!("{:.1}%", p)
    }
}

// ── Sparkline ───────────────────────────────────────

fn sparkline(values: &[f64], width: usize) -> String {
    if values.is_empty() {
        return " ".repeat(width);
    }

    let blocks = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = values.iter().copied().fold(1.0f64, f64::max);

    // Compress values to `width` slots
    let chunk_size = values.len().div_ceil(width);
    let mut result = String::with_capacity(width * 4);

    for i in 0..width {
        let start = i * chunk_size;
        let end = ((i + 1) * chunk_size).min(values.len());
        if start >= values.len() {
            result.push(' ');
            continue;
        }
        let avg: f64 = values[start..end].iter().sum::<f64>() / (end - start) as f64;
        let idx = ((avg / max) * (blocks.len() - 1) as f64).round() as usize;
        result.push(blocks[idx.min(blocks.len() - 1)]);
    }

    result
}

// ── Sort ────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum SortMode {
    Cpu,
    Memory,
    Name,
}

// ── ResourceIndicator Entity ────────────────────────

pub struct ResourceIndicator {
    focus_handle: FocusHandle,
    snapshot: Option<ResourceSnapshot>,
    history: VecDeque<ResourceSnapshot>,
    expanded: bool,
    sort_by: SortMode,
    collapsed_workspaces: HashSet<Uuid>,
    #[allow(dead_code)]
    _poll_task: Option<Task<()>>,
    /// Shared daemon client for kill actions
    daemon_writer: Option<crate::daemon_client::DaemonClientWriter>,
    /// Workspace ID → human-readable name mapping
    workspace_names: HashMap<Uuid, String>,
    /// Window-coordinate bounds of the badge, captured each frame so the popover
    /// panel can anchor itself directly above the trigger.
    badge_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Latest CPU% + memory for the seoul-terminal (client) process itself,
    /// computed locally each poll tick.
    self_metrics: Option<(f64, u64)>,
    /// Previous self-measurement for delta-based CPU% calculation.
    self_cpu_sample: Option<SelfCpuSample>,
}

#[derive(Debug, Clone, Copy)]
struct SelfCpuSample {
    user_us: u64,
    sys_us: u64,
    wall: Instant,
}

impl ResourceIndicator {
    pub fn new(cx: &mut Context<Self>, daemon_client: &DaemonClient) -> Self {
        let focus_handle = cx.focus_handle();
        let socket_writer = daemon_client.clone_writer();

        // Clone socket_writer for the polling task (it just sends GetResourceSnapshot)
        let poll_writer = Arc::new(Mutex::new(daemon_client.clone_resource_requester()));

        let poll_task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            // Initial delay to let daemon stabilize
            cx.background_executor().timer(Duration::from_secs(1)).await;

            loop {
                let Ok(interval) =
                    this.update(cx, |this, _| if this.expanded { 2500u64 } else { 5000 })
                else {
                    break; // Entity dropped
                };

                // Request snapshot on background thread
                let writer = poll_writer.clone();
                let snapshot = cx
                    .background_executor()
                    .spawn(async move { writer.lock().ok().and_then(|r| r.request_snapshot()) })
                    .await;

                // Locally measure the seoul-terminal process itself; the daemon
                // doesn't see us, so we report our own footprint.
                let self_sample = process_metrics::measure_self();

                if let Some(snapshot) = snapshot {
                    this.update(cx, |this, cx| {
                        // Append to history ring buffer
                        if this.history.len() >= MAX_HISTORY {
                            this.history.pop_front();
                        }
                        this.history.push_back(snapshot.clone());
                        this.snapshot = Some(snapshot);
                        this.update_self_metrics(self_sample);
                        cx.notify();
                    })
                    .ok();
                }

                cx.background_executor()
                    .timer(Duration::from_millis(interval))
                    .await;
            }
        });

        Self {
            focus_handle,
            snapshot: None,
            history: VecDeque::with_capacity(MAX_HISTORY),
            expanded: false,
            sort_by: SortMode::Cpu,
            collapsed_workspaces: HashSet::new(),
            _poll_task: Some(poll_task),
            daemon_writer: Some(socket_writer),
            workspace_names: HashMap::new(),
            badge_bounds: Rc::new(Cell::new(None)),
            self_metrics: None,
            self_cpu_sample: None,
        }
    }

    pub fn set_workspace_names(&mut self, names: HashMap<Uuid, String>) {
        self.workspace_names = names;
    }

    /// Fold a fresh `SelfMetrics` reading into `self_metrics` using the
    /// previous sample to compute CPU% via wall-clock delta. Same shape as
    /// `ResourceMonitor::compute_cpu_percent` in the daemon, just kept here
    /// to avoid pulling daemon code into the client.
    fn update_self_metrics(&mut self, sample: Option<SelfMetrics>) {
        let Some(sample) = sample else {
            return;
        };
        let now = Instant::now();
        let cpu_percent = if let Some(prev) = self.self_cpu_sample {
            let wall_delta = now.duration_since(prev.wall).as_micros() as f64;
            if wall_delta > 0.0 {
                let prev_total = prev.user_us + prev.sys_us;
                let curr_total = sample.user_time_us + sample.sys_time_us;
                let cpu_delta = curr_total.saturating_sub(prev_total) as f64;
                ((cpu_delta / wall_delta) * 100.0).clamp(0.0, 10000.0)
            } else {
                0.0
            }
        } else {
            // First sample: no baseline yet, report 0% until next tick.
            0.0
        };
        self.self_cpu_sample = Some(SelfCpuSample {
            user_us: sample.user_time_us,
            sys_us: sample.sys_time_us,
            wall: now,
        });
        self.self_metrics = Some((cpu_percent, sample.memory_bytes));
    }

    pub fn toggle(
        &mut self,
        _: &ToggleResourceMonitor,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.expanded = !self.expanded;
        cx.notify();
    }

    fn session_cpu_history(&self, session_id: Uuid) -> Vec<f64> {
        self.history
            .iter()
            .filter_map(|snap| {
                snap.sessions
                    .iter()
                    .find(|s| s.session_id == session_id)
                    .map(|s| s.cpu_percent)
            })
            .collect()
    }

    fn sorted_sessions(&self) -> Vec<SessionResourceMetrics> {
        let Some(snap) = &self.snapshot else {
            return Vec::new();
        };
        let mut sessions = snap.sessions.clone();
        match self.sort_by {
            SortMode::Cpu => sessions.sort_by(|a, b| {
                b.cpu_percent
                    .partial_cmp(&a.cpu_percent)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            SortMode::Memory => sessions.sort_by(|a, b| b.memory_bytes.cmp(&a.memory_bytes)),
            SortMode::Name => sessions.sort_by(|a, b| {
                let a_name = a.foreground_process.as_deref().unwrap_or("shell");
                let b_name = b.foreground_process.as_deref().unwrap_or("shell");
                a_name.cmp(b_name)
            }),
        }
        sessions
    }

    fn render_badge(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = theme::theme(cx);

        let (cpu_text, mem_text, severity) = match &self.snapshot {
            Some(snap) => {
                let sev = overall_severity(snap.total_cpu_percent, snap.total_memory_bytes);
                (
                    format_percent(snap.total_cpu_percent),
                    format_bytes(snap.total_memory_bytes),
                    sev,
                )
            }
            None => ("--".into(), "--".into(), Severity::Normal),
        };

        let sev_color = severity.color(&t);

        let bounds_handle = self.badge_bounds.clone();

        div()
            .id("resource-badge")
            .relative()
            .h_full()
            .px(px(8.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.))
            .cursor_pointer()
            .bg(rgb(t.surface0))
            .rounded(px(4.))
            .hover(|s| s.bg(rgb(t.surface1)))
            .on_click(cx.listener(|this, _, window, cx| {
                this.toggle(&ToggleResourceMonitor, window, cx);
            }))
            // Invisible bounds probe: records the badge's window-coords bounds each
            // frame so the popover panel can anchor itself directly above the trigger.
            .child(
                canvas(
                    move |bounds, _, _| {
                        bounds_handle.set(Some(bounds));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(
                // Severity dot
                div().w(px(6.)).h(px(6.)).rounded(px(3.)).bg(rgb(sev_color)),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(t.subtext0))
                    .child(cpu_text),
            )
            .child(div().w(px(1.)).h(px(12.)).bg(rgb(t.surface1)))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(t.subtext0))
                    .child(mem_text),
            )
    }

    fn render_panel(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = theme::theme(cx);

        let Some(snap) = &self.snapshot else {
            return div()
                .id("resource-panel")
                .w(px(320.))
                .p(px(12.))
                .bg(rgb(t.base))
                .border_1()
                .border_color(rgb(t.surface0))
                .rounded(px(6.))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(t.surface2))
                        .child("Waiting for data..."),
                )
                .into_any_element();
        };

        let mut panel = div()
            .id("resource-panel")
            .w(px(320.))
            .max_h(px(400.))
            .overflow_y_scroll()
            .bg(rgb(t.base))
            .border_1()
            .border_color(rgb(t.surface0))
            .rounded(px(6.))
            .shadow_lg();

        // Header with sort buttons
        let sort_by = self.sort_by;
        panel = panel.child(
            div()
                .px(px(12.))
                .py(px(8.))
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .border_b_1()
                .border_color(rgb(t.surface0))
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(rgb(t.overlay0))
                        .child("RESOURCE MONITOR"),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(2.))
                        .child(self.sort_button("CPU", SortMode::Cpu, sort_by, &t, cx))
                        .child(self.sort_button("Mem", SortMode::Memory, sort_by, &t, cx))
                        .child(self.sort_button("Name", SortMode::Name, sort_by, &t, cx)),
                ),
        );

        // System section
        let sys = &snap.system;
        let sys_mem_pct = if sys.total_memory_bytes > 0 {
            (sys.used_memory_bytes as f64 / sys.total_memory_bytes as f64) * 100.0
        } else {
            0.0
        };
        let cpu_bar_pct =
            (snap.system.load_average_1m / snap.system.cpu_count as f64 * 100.0).clamp(0.0, 100.0);

        panel =
            panel.child(
                div()
                    .px(px(12.))
                    .py(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .border_b_1()
                    .border_color(rgb(t.surface0))
                    .child(
                        div()
                            .text_size(px(10.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(t.overlay0))
                            .child("SYSTEM"),
                    )
                    // CPU bar
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgb(t.subtext0))
                                    .w(px(28.))
                                    .child("CPU"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .h(px(6.))
                                    .rounded(px(3.))
                                    .bg(rgb(t.surface0))
                                    .child(
                                        div()
                                            .h_full()
                                            .w(relative(cpu_bar_pct as f32 / 100.0))
                                            .rounded(px(3.))
                                            .bg(rgb(cpu_severity(cpu_bar_pct).color(&t))),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgb(t.text))
                                    .w(px(36.))
                                    .child(format_percent(cpu_bar_pct)),
                            ),
                    )
                    // RAM
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgb(t.subtext0))
                                    .w(px(28.))
                                    .child("RAM"),
                            )
                            .child(div().text_size(px(11.)).text_color(rgb(t.text)).child(
                                format!(
                                    "{} / {}  ({}%)",
                                    format_bytes(sys.used_memory_bytes),
                                    format_bytes(sys.total_memory_bytes),
                                    sys_mem_pct as u32
                                ),
                            )),
                    ),
            );

        // Process section — daemon and seoul-terminal (the GPUI client itself).
        // Same row format so the two are visually directly comparable.
        let process_row = |label: &'static str, cpu: f64, mem: u64, t: &theme::ThemeColors| {
            div()
                .px(px(12.))
                .py(px(4.))
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgb(t.subtext0))
                        .child(label),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(t.text))
                                .w(px(48.))
                                .text_align(TextAlign::Right)
                                .child(format_percent(cpu)),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(t.text))
                                .w(px(48.))
                                .text_align(TextAlign::Right)
                                .child(format_bytes(mem)),
                        ),
                )
        };

        panel = panel.child(
            div()
                .py(px(2.))
                .border_b_1()
                .border_color(rgb(t.surface0))
                .child(process_row(
                    "seoul-daemon",
                    snap.daemon.cpu_percent,
                    snap.daemon.memory_bytes,
                    &t,
                ))
                .child(match self.self_metrics {
                    Some((cpu, mem)) => process_row("seoul-terminal", cpu, mem, &t),
                    None => process_row("seoul-terminal", 0.0, 0, &t),
                }),
        );

        // Sessions section
        let sessions = self.sorted_sessions();
        let mut sessions_header = div()
            .px(px(12.))
            .pt(px(8.))
            .pb(px(4.))
            .text_size(px(10.))
            .font_weight(FontWeight::BOLD)
            .text_color(rgb(t.overlay0));

        if snap.inaccessible_pids > 0 {
            sessions_header = sessions_header.child(format!(
                "SESSIONS ({} inaccessible)",
                snap.inaccessible_pids
            ));
        } else {
            sessions_header = sessions_header.child("SESSIONS");
        }

        panel = panel.child(sessions_header);

        if sessions.is_empty() {
            panel = panel.child(
                div()
                    .px(px(12.))
                    .py(px(12.))
                    .text_size(px(11.))
                    .text_color(rgb(t.surface2))
                    .child("No active sessions"),
            );
        } else {
            // Group by workspace
            let mut workspace_groups: Vec<(Uuid, Vec<&SessionResourceMetrics>)> = Vec::new();
            for session in &sessions {
                if let Some(group) = workspace_groups
                    .iter_mut()
                    .find(|(ws, _)| *ws == session.workspace_id)
                {
                    group.1.push(session);
                } else {
                    workspace_groups.push((session.workspace_id, vec![session]));
                }
            }

            for (ws_id, ws_sessions) in &workspace_groups {
                let ws_id = *ws_id;
                let is_collapsed = self.collapsed_workspaces.contains(&ws_id);
                let ws_label = self
                    .workspace_names
                    .get(&ws_id)
                    .cloned()
                    .unwrap_or_else(|| format!("unknown-{}", &ws_id.to_string()[..6]));
                panel = panel.child(
                    div()
                        .id(ElementId::Name(format!("ws-{ws_id}").into()))
                        .px(px(12.))
                        .py(px(4.))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(4.))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(t.surface0)))
                        .text_size(px(11.))
                        .text_color(rgb(t.subtext0))
                        .child(
                            Icon::new(
                                if is_collapsed {
                                    IconName::ChevronRight
                                } else {
                                    IconName::ChevronDown
                                },
                                rgb(t.subtext0),
                            )
                            .size(px(12.)),
                        )
                        .child(ws_label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.collapsed_workspaces.contains(&ws_id) {
                                this.collapsed_workspaces.remove(&ws_id);
                            } else {
                                this.collapsed_workspaces.insert(ws_id);
                            }
                            cx.notify();
                        })),
                );

                if !is_collapsed {
                    for session in ws_sessions {
                        let name = session
                            .foreground_process
                            .as_deref()
                            .unwrap_or("shell")
                            .to_string();
                        let cpu_text = format_percent(session.cpu_percent);
                        let mem_text = format_bytes(session.memory_bytes);
                        let sev = overall_severity(session.cpu_percent, session.memory_bytes);
                        let sev_color = sev.color(&t);
                        let cpu_bar_color = cpu_severity(session.cpu_percent).color(&t);
                        // Visualize CPU as a bar fill capped at 100%; values above
                        // (multi-core) saturate the bar in the High severity color.
                        let cpu_bar_pct = (session.cpu_percent / 100.0).clamp(0.0, 1.0) as f32;

                        let session_id = session.session_id;

                        panel = panel.child(
                            div()
                                .id(ElementId::Name(format!("sess-{session_id}").into()))
                                .px(px(16.))
                                .py(px(3.))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.))
                                .hover(|s| s.bg(rgb(t.surface0)))
                                .rounded(px(3.))
                                // Process name (left-aligned, fixed width, truncates long names)
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(rgb(t.text))
                                        .w(px(64.))
                                        .overflow_x_hidden()
                                        .child(name),
                                )
                                // CPU load bar — mirrors the SYSTEM section's bar so the
                                // visualization is self-explanatory through parallel structure.
                                .child(
                                    div()
                                        .w(px(60.))
                                        .h(px(6.))
                                        .rounded(px(3.))
                                        .bg(rgb(t.surface0))
                                        .child(
                                            div()
                                                .h_full()
                                                .w(relative(cpu_bar_pct))
                                                .rounded(px(3.))
                                                .bg(rgb(cpu_bar_color)),
                                        ),
                                )
                                // CPU (right-aligned numeric column)
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(
                                            rgb(cpu_severity(session.cpu_percent).color(&t)),
                                        )
                                        .w(px(44.))
                                        .text_align(TextAlign::Right)
                                        .child(cpu_text),
                                )
                                // Memory (right-aligned numeric column)
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(rgb(
                                            memory_severity(session.memory_bytes).color(&t)
                                        ))
                                        .w(px(44.))
                                        .text_align(TextAlign::Right)
                                        .child(mem_text),
                                )
                                // Spacer pushes severity dot + kill button to the row's right edge
                                .child(div().flex_1())
                                // Severity dot
                                .child(div().w(px(6.)).h(px(6.)).rounded(px(3.)).bg(rgb(sev_color)))
                                // Kill button
                                .child(
                                    div()
                                        .id(ElementId::Name(format!("kill-{session_id}").into()))
                                        .cursor_pointer()
                                        .px(px(4.))
                                        .rounded(px(2.))
                                        .hover(|s| s.bg(rgb(t.red)).text_color(rgb(t.base)))
                                        .child(
                                            Icon::new(IconName::X, rgb(t.surface2)).size(px(12.)),
                                        )
                                        .on_click(cx.listener(move |this, _, _, _cx| {
                                            if let Some(writer) = &this.daemon_writer {
                                                writer.kill(session_id);
                                            }
                                        })),
                                ),
                        );
                    }
                }
            }
        }

        // Bottom padding
        panel = panel.child(div().h(px(4.)));

        panel.into_any_element()
    }

    fn sort_button(
        &self,
        label: &'static str,
        mode: SortMode,
        current: SortMode,
        t: &theme::ThemeColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_active = mode == current;
        div()
            .id(ElementId::Name(format!("sort-{label}").into()))
            .px(px(4.))
            .py(px(1.))
            .rounded(px(3.))
            .cursor_pointer()
            .text_size(px(10.))
            .when(is_active, |el| {
                el.text_color(rgb(t.blue)).bg(rgb(t.surface0))
            })
            .when(!is_active, |el| {
                el.text_color(rgb(t.surface2))
                    .hover(|s| s.text_color(rgb(t.subtext0)))
            })
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.sort_by = mode;
                cx.notify();
            }))
    }
}

impl Focusable for ResourceIndicator {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ResourceIndicator {
    /// Whether the panel is currently expanded (used by AppView to render overlay).
    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    /// Render the panel overlay — called by AppView at the root level to avoid tab bar clipping.
    pub fn render_panel_overlay(&self, cx: &mut Context<Self>) -> AnyElement {
        // Bounds may not be captured on the very first frame after toggling; bail
        // and re-render once the badge's prepaint runs.
        let Some(badge_bounds) = self.badge_bounds.get() else {
            return div().into_any_element();
        };

        let panel = self.render_panel(cx);

        // Anchor the panel's bottom-right at the badge's top-right with a small
        // gap above the trigger.
        let gap = px(6.);
        let anchor_pos = point(badge_bounds.right(), badge_bounds.top() - gap);

        div()
            .id("resource-panel-overlay")
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .size_full()
            // Click-outside to dismiss. Inner clicks are absorbed by the
            // panel-container's `.occlude()` below, so this only fires for
            // genuine outside clicks.
            .on_click(cx.listener(|this, _, _, cx| {
                this.expanded = false;
                cx.notify();
            }))
            .child(
                anchored()
                    .anchor(Corner::BottomRight)
                    .position(anchor_pos)
                    .snap_to_window_with_margin(px(4.))
                    .child(div().id("resource-panel-container").occlude().child(panel)),
            )
            .into_any_element()
    }
}

impl Render for ResourceIndicator {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Only render the compact badge here — panel is rendered by AppView
        self.render_badge(cx)
    }
}

// ── ResourceRequester: thin handle for polling ──────

/// Thin wrapper for sending GetResourceSnapshot requests + reading responses.
/// Routes outgoing requests through the central daemon-writer thread so the
/// polling task never blocks on socket I/O.
pub struct ResourceRequester {
    pub(crate) writer_tx: std::sync::mpsc::Sender<crate::daemon_client::OutgoingMessage>,
    pub(crate) resource_rx:
        std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<ResourceSnapshot>>>,
}

impl ResourceRequester {
    pub fn request_snapshot(&self) -> Option<ResourceSnapshot> {
        self.writer_tx
            .send(crate::daemon_client::OutgoingMessage::GetResourceSnapshot)
            .ok()?;

        self.resource_rx
            .lock()
            .ok()?
            .recv_timeout(Duration::from_secs(2))
            .ok()
    }
}
