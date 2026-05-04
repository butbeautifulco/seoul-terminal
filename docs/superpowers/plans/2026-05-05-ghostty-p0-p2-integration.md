# Ghostty P0-P2 Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix Seoul's Ghostty integration gaps across paste/focus/effects, cursor rendering, mouse/key fidelity, render performance, daemon restore/backpressure, and build reproducibility.

**Architecture:** Keep `libghostty-vt` owned by `seoul-vt::Terminal` on the GPUI/UI thread. Add focused helper state for Ghostty callbacks and render caching, while preserving the daemon-owned PTY/session model and the existing `TerminalView` GPUI boundary.

**Tech Stack:** Rust 2024, GPUI, `libghostty-vt`, `portable-pty`, MessagePack RPC, `just`.

---

## Pre-Flight Context

- Repository root: `/Users/seongminpark/Projects/superset-rust`
- Development workflow uses `just`, not raw `cargo`, for final gates.
- Final verification gate is `just lint && just test`.
- `libghostty-vt` objects are `!Send + !Sync`; keep Ghostty objects on one thread.
- Reference `Uzaaft/libghostty-rs` `example/ghostling_rs/src/main.rs` at commit `811cbdd85a99b40a63424a9d247c4f3653c11924` for idiomatic callback, encoder, and render-state lifecycle patterns: `https://github.com/Uzaaft/libghostty-rs/blob/811cbdd85a99b40a63424a9d247c4f3653c11924/example/ghostling_rs/src/main.rs`. It uses macroquad and direct PTY ownership, so copy only the `libghostty-vt` integration patterns, not its UI/event loop.
- Do not start implementation on `main` or `master`.
- Do not revert unrelated user changes.
- This plan is designed for `superpowers:subagent-driven-development`: dispatch one implementer subagent per task, then a spec compliance reviewer, then a code quality reviewer.

## Ghostling Reference Patterns To Preserve

- Effects: register `on_pty_write`, `on_size`, `on_device_attributes`, `on_xtversion`, and `on_color_scheme` when constructing the terminal. Seoul additionally registers `on_bell`, `on_title_changed`, and `on_enquiry`.
- Helpers: keep `RenderState`, row/cell iterators, key encoder/event, and mouse encoder/event reusable for the lifetime of `Terminal`.
- Keys: before encoding, set action/key/mods/consumed mods/unshifted codepoint/UTF-8 text, then call `key_encoder.set_options_from_terminal(&terminal)`.
- Mouse: before encoding, call `set_options_from_terminal`, `set_size`, `set_any_button_pressed`, and `set_track_last_cell(true)`. Encode explicit press/release and motion events.
- Rendering: update the render snapshot, extract changed rows, clear per-row dirty flags, then clear the global dirty state.
- Differences: Seoul keeps the daemon as PTY/session owner, uses GPUI for event delivery and text shaping, and must preserve its existing IME path.

## File Structure

- Create: `crates/seoul-vt/src/effects.rs`
  - Ghostty callback state and fixed callback responses.
- Create: `crates/seoul-vt/tests/input_effects.rs`
  - Regression tests for paste, focus, effects, wide cursor, and mouse encoder state.
- Modify: `crates/seoul-vt/src/lib.rs`
  - Add private `effects` module.
- Modify: `crates/seoul-vt/src/terminal.rs`
  - Implement Ghostty paste/focus/effects, wide-tail cursor, dirty row generation, mouse button state, pixel clamps.
- Create: `crates/seoul-terminal/src/terminal_render_cache.rs`
  - Per-row terminal run cache owned by `TerminalView`.
- Modify: `crates/seoul-terminal/src/main.rs`
  - Add `mod terminal_render_cache;`.
- Modify: `crates/seoul-terminal/src/terminal_element.rs`
  - Use cached run rows and redraw block cursor glyph.
- Modify: `crates/seoul-terminal/src/terminal_view.rs`
  - Wire focus subscriptions, mouse events, mouse encoder size, richer key mapping, and render cache ownership.
- Modify: `crates/seoul-terminal/src/daemon_client.rs`
  - Bound app-side per-session daemon data queue.
- Modify: `crates/seoul-daemon/src/mode_tracker.rs`
  - Track additional rehydratable terminal modes.
- Modify: `crates/seoul-daemon/src/session.rs`
  - Clamp PTY pixel size fields.
- Modify: `Cargo.toml`
  - Pin `libghostty-vt` rev.
- Modify: `justfile`
  - Make Ghostty dylib lookup deterministic.

## Public Interface Changes

- `seoul_vt::terminal::TerminalContent`
  - Add `pub bell_count: u64`.
  - Add `pub dirty_rows: Vec<u16>`.
  - Add `pub content_generation: u64`.
- `seoul_vt::Terminal`
  - Add `pub fn paste_is_safe(text: &str) -> bool`.
  - Add `pub fn set_mouse_any_button_pressed(&mut self, pressed: bool)`.
  - Add `pub fn set_mouse_track_last_cell(&mut self, enabled: bool)`.
  - Change `try_keystroke` signature to include `unshifted: Option<char>`.
- `CursorInfo`
  - Keep existing fields.
  - Semantic change: when Ghostty reports wide-tail cursor, `col` points at the wide glyph head and `is_wide` is true.

## Task 0: Create Dedicated Implementation Worktree

**Files:**
- No source files.

- [ ] **Step 1: Locate and skim the pinned ghostling-rs reference**

```bash
GHOSTLING_RS=$(find ~/.cargo/git/checkouts -path '*/example/ghostling_rs/src/main.rs' | sort | tail -n 1)
test -n "$GHOSTLING_RS"
git -C "$(dirname "$GHOSTLING_RS")/../../.." rev-parse HEAD
rg -n "on_pty_write|on_size|on_device_attributes|on_xtversion|set_unshifted_codepoint|set_any_button_pressed|set_track_last_cell|snapshot.set_dirty" "$GHOSTLING_RS"
```

Expected: the git SHA is `811cbdd85a99b40a63424a9d247c4f3653c11924`, and `rg` prints matches for callback registration, key encoder setup, mouse encoder setup, and dirty-state lifecycle.

- [ ] **Step 2: Verify current branch and status**

```bash
git branch --show-current
git status --short
```

Expected: not `main`/`master` for implementation, or stop and create a worktree in Step 3. Status may include this spec/plan commit only before implementation starts.

- [ ] **Step 3: Create worktree if currently on main/master**

```bash
mkdir -p ~/.codex/worktrees
git worktree add ~/.codex/worktrees/seoul-ghostty-p0-p2 -b fix/ghostty-p0-p2-integration
cd ~/.codex/worktrees/seoul-ghostty-p0-p2
```

Expected: worktree exists at `~/.codex/worktrees/seoul-ghostty-p0-p2` on branch `fix/ghostty-p0-p2-integration`.

- [ ] **Step 4: Confirm baseline gate**

```bash
just test
```

Expected: PASS before implementation. If baseline fails, stop and report the exact failing tests.

## Task 1: Add Regression Tests For Ghostty Input, Effects, And Cursor

**Files:**
- Create: `crates/seoul-vt/tests/input_effects.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/seoul-vt/tests/input_effects.rs` with:

```rust
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use libghostty_vt::{key, mouse};
use seoul_vt::config::TerminalConfig;
use seoul_vt::terminal::TerminalBounds;
use seoul_vt::{Terminal, TerminalBuilder};

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

fn terminal() -> (Terminal, Arc<Mutex<Vec<u8>>>) {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let writer = CaptureWriter(captured.clone());
    let terminal = TerminalBuilder::new(TerminalConfig {
        cols: 80,
        rows: 24,
        ..Default::default()
    })
    .build_attached(Box::new(writer))
    .unwrap();
    (terminal, captured)
}

fn take(captured: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
    std::mem::take(&mut *captured.lock().unwrap())
}

#[test]
fn paste_uses_ghostty_encoder() {
    let (mut term, captured) = terminal();

    term.feed_pty_data(b"\x1b[?2004h");
    term.paste("a\x1bb\x7fc\n");
    assert_eq!(take(&captured), b"\x1b[200~a b c\n\x1b[201~");

    term.feed_pty_data(b"\x1b[?2004l");
    term.paste("a\nb\r\nc\x03");
    assert_eq!(take(&captured), b"a\rb\r\rc ");
}

#[test]
fn focus_reporting_is_mode_gated() {
    let (mut term, captured) = terminal();

    term.focus_in();
    term.focus_out();
    assert!(take(&captured).is_empty());

    term.feed_pty_data(b"\x1b[?1004h");
    term.focus_in();
    term.focus_out();
    assert_eq!(take(&captured), b"\x1b[I\x1b[O");
}

#[test]
fn title_bell_queries_and_size_effects_work() {
    let (mut term, captured) = terminal();

    term.feed_pty_data(b"\x1b]2;Build\x07\x07");
    term.sync();
    assert_eq!(term.breadcrumb_text(), "Build");
    assert_eq!(term.last_content.bell_count, 1);

    term.feed_pty_data(b"\x05");
    assert_eq!(take(&captured), b"seoul");

    term.feed_pty_data(b"\x1b[>0q");
    assert_eq!(take(&captured), b"\x1bP>|seoul 0.1.0\x1b\\");

    term.resize(TerminalBounds {
        cols: 80,
        rows: 24,
        cell_width: 9.0,
        line_height: 18.0,
    })
    .unwrap();
    term.feed_pty_data(b"\x1b[18t");
    assert_eq!(take(&captured), b"\x1b[8;24;80t");
}

#[test]
fn silent_replay_does_not_mutate_user_visible_effects() {
    let (mut term, _captured) = terminal();

    term.feed_pty_data(b"\x1b]2;Live\x07\x07");
    term.sync();
    assert_eq!(term.breadcrumb_text(), "Live");
    assert_eq!(term.last_content.bell_count, 1);

    term.feed_pty_data_silently(b"\x1b]2;Replay\x07\x07");
    term.sync();
    assert_eq!(term.breadcrumb_text(), "Live");
    assert_eq!(term.last_content.bell_count, 1);
}

#[test]
fn wide_tail_cursor_is_rendered_at_wide_head() {
    let (mut term, _captured) = terminal();

    term.feed_pty_data("界".as_bytes());
    term.feed_pty_data(b"\x1b[1;2H");
    term.sync();

    assert_eq!(term.last_content.cursor.col, 0);
    assert!(term.last_content.cursor.is_wide);
}

#[test]
fn mouse_encoder_tracks_size_and_button_state() {
    let (mut term, captured) = terminal();

    term.feed_pty_data(b"\x1b[?1000h\x1b[?1006h");
    term.set_mouse_size(mouse::EncoderSize {
        screen_width: 800,
        screen_height: 400,
        cell_width: 10,
        cell_height: 20,
        padding_top: 0,
        padding_bottom: 0,
        padding_right: 0,
        padding_left: 0,
    });
    term.set_mouse_any_button_pressed(true);
    term.send_mouse_event(
        mouse::Action::Press,
        Some(mouse::Button::Left),
        key::Mods::empty(),
        15.0,
        25.0,
    );

    assert!(!take(&captured).is_empty());
}
```

- [ ] **Step 2: Verify RED**

```bash
just test
```

Expected: FAIL because `bell_count` and `set_mouse_any_button_pressed` do not exist, and paste/focus/wide cursor behavior is incomplete.

- [ ] **Step 3: Commit failing tests**

```bash
git add crates/seoul-vt/tests/input_effects.rs
git commit -m "test: cover ghostty input effects and cursor"
```

## Task 2: Implement P0 Ghostty Paste, Focus, Effects, Wide Cursor, And Dirty Rows

**Files:**
- Create: `crates/seoul-vt/src/effects.rs`
- Modify: `crates/seoul-vt/src/lib.rs`
- Modify: `crates/seoul-vt/src/terminal.rs`
- Test: `crates/seoul-vt/tests/input_effects.rs`

- [ ] **Step 1: Add effect state module**

Create `crates/seoul-vt/src/effects.rs`:

```rust
use libghostty_vt::terminal::{
    ColorScheme, ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType,
    PrimaryDeviceAttributes, SecondaryDeviceAttributes, SizeReportSize, TertiaryDeviceAttributes,
};

use crate::terminal::TerminalBounds;

pub(crate) const ENQUIRY_RESPONSE: &str = "seoul";
pub(crate) const XTVERSION_RESPONSE: &str = concat!("seoul ", env!("CARGO_PKG_VERSION"));

#[derive(Debug)]
pub(crate) struct TerminalEffectState {
    pub title: String,
    pub bell_count: u64,
    pub suppress_side_effects: bool,
    pub size: SizeReportSize,
}

impl TerminalEffectState {
    pub fn new(cols: u16, rows: u16, cell_width: f32, line_height: f32) -> Self {
        let mut state = Self {
            title: String::new(),
            bell_count: 0,
            suppress_side_effects: false,
            size: SizeReportSize::default(),
        };
        state.set_size(TerminalBounds {
            cols,
            rows,
            cell_width,
            line_height,
        });
        state
    }

    pub fn set_size(&mut self, bounds: TerminalBounds) {
        self.size = SizeReportSize {
            rows: bounds.rows,
            columns: bounds.cols,
            cell_width: bounds.cell_width.round().max(1.0) as u32,
            cell_height: bounds.line_height.round().max(1.0) as u32,
        };
    }
}

pub(crate) fn device_attributes() -> DeviceAttributes {
    DeviceAttributes {
        primary: PrimaryDeviceAttributes::new(
            ConformanceLevel::VT420,
            [
                DeviceAttributeFeature::SELECTIVE_ERASE,
                DeviceAttributeFeature::ANSI_COLOR,
            ],
        ),
        secondary: SecondaryDeviceAttributes {
            device_type: DeviceType::VT420,
            firmware_version: 1,
            rom_cartridge: 0,
        },
        tertiary: TertiaryDeviceAttributes { unit_id: 0 },
    }
}

pub(crate) fn color_scheme() -> ColorScheme {
    ColorScheme::Dark
}
```

- [ ] **Step 2: Add module declaration**

In `crates/seoul-vt/src/lib.rs`, add:

```rust
mod effects;
```

- [ ] **Step 3: Update terminal model fields**

In `crates/seoul-vt/src/terminal.rs`:

```rust
use libghostty_vt::terminal::{Mode, ScrollViewport};
use libghostty_vt::{Terminal as GhosttyTerminal, TerminalOptions, focus, key, mouse, paste};

use crate::effects;
```

Add to `TerminalContent`:

```rust
pub bell_count: u64,
pub dirty_rows: Vec<u16>,
pub content_generation: u64,
```

Add to `Terminal`:

```rust
effect_state: Arc<Mutex<effects::TerminalEffectState>>,
```

- [ ] **Step 4: Register callbacks in `setup_ghostty`**

In `TerminalBuilder::setup_ghostty`, create:

```rust
let effect_state = Arc::new(Mutex::new(effects::TerminalEffectState::new(
    config.cols,
    config.rows,
    8.0,
    16.0,
)));
```

After `on_pty_write`, register:

```rust
let state_for_bell = effect_state.clone();
ghostty.on_bell(move |_terminal| {
    if let Ok(mut state) = state_for_bell.lock()
        && !state.suppress_side_effects
    {
        state.bell_count = state.bell_count.saturating_add(1);
    }
})?;

let state_for_title = effect_state.clone();
ghostty.on_title_changed(move |terminal| {
    if let Ok(mut state) = state_for_title.lock()
        && !state.suppress_side_effects
        && let Ok(title) = terminal.title()
    {
        state.title.clear();
        state.title.push_str(title);
    }
})?;

let state_for_size = effect_state.clone();
ghostty.on_size(move |_terminal| state_for_size.lock().ok().map(|state| state.size))?;
ghostty.on_enquiry(|_terminal| Some(effects::ENQUIRY_RESPONSE))?;
ghostty.on_xtversion(|_terminal| Some(effects::XTVERSION_RESPONSE))?;
ghostty.on_color_scheme(|_terminal| Some(effects::color_scheme()))?;
ghostty.on_device_attributes(|_terminal| Some(effects::device_attributes()))?;
```

Return and store `effect_state` from `setup_ghostty` in both `build` and `build_attached`.

- [ ] **Step 5: Replace paste implementation**

Replace `Terminal::paste` with:

```rust
pub fn paste_is_safe(text: &str) -> bool {
    paste::is_safe(text)
}

pub fn paste(&mut self, text: &str) {
    if !Self::paste_is_safe(text) {
        tracing::warn!("pasting text that Ghostty marks unsafe; sanitizer will be applied");
    }

    let bracketed = self.ghostty.mode(Mode::BRACKETED_PASTE).unwrap_or(false);
    let mut input = text.as_bytes().to_vec();
    let mut out = vec![0; input.len() + 32];

    let written = match paste::encode(&mut input, bracketed, &mut out) {
        Ok(written) => written,
        Err(libghostty_vt::Error::OutOfSpace { required }) => {
            out.resize(required, 0);
            match paste::encode(&mut input, bracketed, &mut out) {
                Ok(written) => written,
                Err(e) => {
                    tracing::warn!("ghostty paste encode failed after resize: {e}");
                    return;
                }
            }
        }
        Err(e) => {
            tracing::warn!("ghostty paste encode failed: {e}");
            return;
        }
    };

    self.input(&out[..written]);
}
```

- [ ] **Step 6: Implement focus events**

Replace `focus_in` and `focus_out`:

```rust
pub fn focus_in(&mut self) {
    if self.ghostty.mode(Mode::FOCUS_EVENT).unwrap_or(false) {
        let mut buf = [0u8; 8];
        if let Ok(written) = focus::Event::Gained.encode(&mut buf) {
            self.input_raw(&buf[..written]);
        }
    }
}

pub fn focus_out(&mut self) {
    if self.ghostty.mode(Mode::FOCUS_EVENT).unwrap_or(false) {
        let mut buf = [0u8; 8];
        if let Ok(written) = focus::Event::Lost.encode(&mut buf) {
            self.input_raw(&buf[..written]);
        }
    }
}
```

- [ ] **Step 7: Suppress replay side effects**

In `feed_pty_data_silently`, set `suppress_side_effects = true` before `vt_write`, restore the previous value after `vt_write`, and keep the existing sink writer swap.

- [ ] **Step 8: Sync effect state and size**

In `resize`, after setting `last_content.terminal_bounds`, call:

```rust
if let Ok(mut state) = self.effect_state.lock() {
    state.set_size(bounds);
}
```

In `sync`, after cursor/colors are read:

```rust
if let Ok(state) = self.effect_state.lock() {
    self.breadcrumb_text = state.title.clone();
    self.last_content.bell_count = state.bell_count;
}
```

- [ ] **Step 9: Fix wide-tail cursor**

In `sync`, compute cursor from Ghostty viewport:

```rust
let cursor_vp = snapshot.cursor_viewport().ok().flatten();
let (cursor_col, cursor_wide_tail) = cursor_vp
    .map(|c| {
        if c.at_wide_tail && c.x > 0 {
            (c.x - 1, true)
        } else {
            (c.x, false)
        }
    })
    .unwrap_or((0, false));
```

Use `cursor_col` for `CursorInfo.col`. After cell extraction, set `is_wide` to `cursor_wide_tail || scanned_wide_cell`.

- [ ] **Step 10: Track dirty rows and generation**

At the start of row extraction:

```rust
self.last_content.dirty_rows.clear();
let mut touched_rows = false;
```

Whenever a row is dirty and extracted:

```rust
self.last_content.dirty_rows.push(row_idx as u16);
touched_rows = true;
```

After extraction:

```rust
if touched_rows || geometry_changed {
    self.last_content.content_generation =
        self.last_content.content_generation.saturating_add(1);
}
```

- [ ] **Step 11: Add mouse button API**

Add:

```rust
pub fn set_mouse_any_button_pressed(&mut self, pressed: bool) {
    self.mouse_encoder.set_any_button_pressed(pressed);
}

pub fn set_mouse_track_last_cell(&mut self, enabled: bool) {
    self.mouse_encoder.set_track_last_cell(enabled);
}
```

- [ ] **Step 12: Verify GREEN**

```bash
just test
```

Expected: PASS for `crates/seoul-vt/tests/input_effects.rs`.

- [ ] **Step 13: Commit**

```bash
git add crates/seoul-vt/src/lib.rs crates/seoul-vt/src/effects.rs crates/seoul-vt/src/terminal.rs crates/seoul-vt/tests/input_effects.rs
git commit -m "fix: integrate ghostty input effects"
```

## Task 3: Wire GPUI Focus And Mouse Events

**Files:**
- Modify: `crates/seoul-terminal/src/terminal_view.rs`

- [ ] **Step 1: Add fields**

Add to `TerminalView`:

```rust
_focus_subscriptions: [Subscription; 2],
mouse_button_pressed: bool,
```

Change `element_bounds` to:

```rust
element_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
```

- [ ] **Step 2: Create focus subscriptions in constructors**

In `new` and `new_pending`, after `focus_handle` is created:

```rust
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
```

Store `_focus_subscriptions` and initialize `mouse_button_pressed: false`.

- [ ] **Step 3: Store full bounds**

In the `ime_canvas` paint callback, replace the stored size with the full bounds:

```rust
let prev = bounds_cell.replace(Some(bounds));
if prev.map(|b| b.size) != Some(bounds.size) {
    let weak = weak_view.clone();
    cx.spawn(async move |cx| {
        weak.update(cx, |this, cx| this.on_bounds_changed(cx)).ok();
    })
    .detach();
}
```

Update all callers that read `element_bounds.get()` to use `.map(|b| b.size)`.

- [ ] **Step 4: Add mouse size helper**

Add:

```rust
fn update_mouse_size_from_bounds(&mut self) {
    let Some(bounds) = self.element_bounds.get() else {
        return;
    };
    let pad = self.config.padding.round().max(0.0) as u32;
    self.terminal.set_mouse_size(mouse::EncoderSize {
        screen_width: f32::from(bounds.size.width).round().max(1.0) as u32,
        screen_height: f32::from(bounds.size.height).round().max(1.0) as u32,
        cell_width: self.cell_width.round().max(1.0) as u32,
        cell_height: self.cell_height.round().max(1.0) as u32,
        padding_top: pad,
        padding_bottom: pad,
        padding_right: pad,
        padding_left: pad,
    });
    self.terminal.set_mouse_track_last_cell(true);
}
```

Call it from `initialize_local_terminal`, `initialize_attached_terminal`, `on_bounds_changed`, `apply_resize_ack`, and `fire_resize_ack_timeout`.

- [ ] **Step 5: Add mouse modifier and button helpers**

```rust
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
```

- [ ] **Step 6: Add mouse event handlers**

Add methods:

```rust
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
    self.mouse_button_pressed = true;
    self.terminal.set_mouse_any_button_pressed(true);
    self.terminal.set_mouse_track_last_cell(true);
    if self.terminal.is_mouse_tracking() {
        self.terminal.send_mouse_event(
            mouse::Action::Press,
            Some(button),
            Self::mouse_mods(event.modifiers),
            f32::from(event.position.x),
            f32::from(event.position.y),
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
    self.mouse_button_pressed = false;
    self.terminal.set_mouse_any_button_pressed(false);
    self.terminal.set_mouse_track_last_cell(true);
    if self.terminal.is_mouse_tracking() {
        self.terminal.send_mouse_event(
            mouse::Action::Release,
            Some(button),
            Self::mouse_mods(event.modifiers),
            f32::from(event.position.x),
            f32::from(event.position.y),
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
    self.terminal
        .set_mouse_any_button_pressed(self.mouse_button_pressed);
    self.terminal.set_mouse_track_last_cell(true);
    self.terminal.send_mouse_event(
        mouse::Action::Motion,
        None,
        Self::mouse_mods(event.modifiers),
        f32::from(event.position.x),
        f32::from(event.position.y),
    );
    if self.mouse_button_pressed {
        self.show_cursor_now(cx);
        cx.notify();
    }
}
```

- [ ] **Step 7: Wire handlers in render chain**

Add to the terminal `div()` chain:

```rust
.on_mouse_down(MouseButton::Left, cx.listener(Self::on_terminal_mouse_down))
.on_mouse_down(MouseButton::Right, cx.listener(Self::on_terminal_mouse_down))
.on_mouse_down(MouseButton::Middle, cx.listener(Self::on_terminal_mouse_down))
.on_mouse_up(MouseButton::Left, cx.listener(Self::on_terminal_mouse_up))
.on_mouse_up(MouseButton::Right, cx.listener(Self::on_terminal_mouse_up))
.on_mouse_up(MouseButton::Middle, cx.listener(Self::on_terminal_mouse_up))
.on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_terminal_mouse_up))
.on_mouse_move(cx.listener(Self::on_terminal_mouse_move))
```

- [ ] **Step 8: Verify**

```bash
just test
just lint
```

Expected: both PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/seoul-terminal/src/terminal_view.rs
git commit -m "fix: wire terminal focus and mouse events"
```

## Task 4: Improve Key Event Fidelity

**Files:**
- Modify: `crates/seoul-terminal/src/terminal_view.rs`
- Modify: `crates/seoul-vt/src/terminal.rs`

- [ ] **Step 1: Change `try_keystroke` signature**

In `crates/seoul-vt/src/terminal.rs`, change:

```rust
pub fn try_keystroke(&mut self, key: key::Key, mods: key::Mods, utf8: Option<&str>) -> bool
```

to:

```rust
pub fn try_keystroke(
    &mut self,
    key: key::Key,
    mods: key::Mods,
    utf8: Option<&str>,
    unshifted: Option<char>,
) -> bool
```

Set:

```rust
self.key_event.set_composing(false);
if let Some(ch) = unshifted {
    self.key_event.set_unshifted_codepoint(ch);
}
```

- [ ] **Step 2: Extend key mapping return type**

In `TerminalView::map_keystroke`, return:

```rust
Option<(gkey::Key, key::Mods, Option<String>, Option<char>)>
```

For single-character keys, set `unshifted` to lowercase ASCII when `ch.is_ascii()`, otherwise `Some(ch)`.

- [ ] **Step 3: Add fixed key mappings**

Add mappings:

```rust
"insert" => gkey::Key::Insert,
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
```

- [ ] **Step 4: Update call site**

In keydown handler, call:

```rust
if let Some((key, mods, utf8, unshifted)) = Self::map_keystroke(&event.keystroke) {
    this.show_cursor_now(cx);
    if !this.ime_preedit.is_empty() {
        let text = std::mem::take(&mut this.ime_preedit);
        this.terminal.input(text.as_bytes());
    }
    this.terminal.try_keystroke(key, mods, utf8.as_deref(), unshifted);
}
```

- [ ] **Step 5: Verify**

```bash
just test
just lint
```

Expected: both PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/seoul-terminal/src/terminal_view.rs crates/seoul-vt/src/terminal.rs
git commit -m "fix: improve ghostty key event fidelity"
```

## Task 5: Add Terminal Render Cache And Block Cursor Glyph Redraw

**Files:**
- Create: `crates/seoul-terminal/src/terminal_render_cache.rs`
- Modify: `crates/seoul-terminal/src/main.rs`
- Modify: `crates/seoul-terminal/src/terminal_element.rs`
- Modify: `crates/seoul-terminal/src/terminal_view.rs`

- [ ] **Step 1: Add module declaration**

In `crates/seoul-terminal/src/main.rs`, add:

```rust
mod terminal_render_cache;
```

- [ ] **Step 2: Create cache module**

Create `crates/seoul-terminal/src/terminal_render_cache.rs` with public types:

```rust
use gpui::*;
use libghostty_vt::style::RgbColor;
use seoul_vt::config::TerminalConfig;
use seoul_vt::terminal::{CellWidthKind, TerminalContent};

#[derive(Clone, Default)]
pub struct CachedCellRun {
    pub text: SharedString,
    pub fg: Hsla,
    pub bg: Hsla,
    pub col_start: u16,
    pub cols: u16,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub faint: bool,
    pub has_wide: bool,
}

#[derive(Default)]
struct CachedRow {
    generation: u64,
    runs: Vec<CachedCellRun>,
}

#[derive(Default)]
pub struct TerminalRenderCache {
    rows: Vec<CachedRow>,
    last_generation: u64,
}

impl TerminalRenderCache {
    pub fn update(&mut self, content: &TerminalContent, config: &TerminalConfig) {
        if self.rows.len() != content.cells.len() {
            self.rows.clear();
            self.rows.resize_with(content.cells.len(), CachedRow::default);
        }

        let full_refresh = self.last_generation != content.content_generation
            && content.dirty_rows.is_empty();
        for row_idx in 0..content.cells.len() {
            let row_dirty = full_refresh
                || content.dirty_rows.contains(&(row_idx as u16))
                || self.rows[row_idx].generation == 0;
            if row_dirty {
                self.rows[row_idx].runs = build_runs_for_row(&content.cells[row_idx]);
                self.rows[row_idx].generation = content.content_generation;
            }
        }
        self.last_generation = content.content_generation;

        let _ = config;
    }

    pub fn rows(&self) -> impl ExactSizeIterator<Item = &[CachedCellRun]> {
        self.rows.iter().map(|row| row.runs.as_slice())
    }
}

fn build_runs_for_row(row_cells: &[seoul_vt::terminal::RenderedCell]) -> Vec<CachedCellRun> {
    let mut runs: Vec<CachedCellRun> = Vec::new();
    for cell in row_cells {
        if matches!(cell.wide, CellWidthKind::SpacerTail | CellWidthKind::SpacerHead) {
            continue;
        }
        let fg_raw = rgb_to_hsla(cell.fg);
        let bg = rgb_to_hsla(cell.bg);
        let fg = if cell.faint {
            let mut f = fg_raw;
            f.a *= 0.5;
            f
        } else {
            fg_raw
        };
        let is_wide = cell.wide == CellWidthKind::Wide;

        let text = if cell.graphemes.is_empty() || cell.graphemes.as_slice() == [' '] {
            " ".to_string()
        } else {
            cell.graphemes.iter().collect()
        };

        if !is_wide
            && let Some(last) = runs.last_mut()
            && !last.has_wide
            && last.fg == fg
            && last.bg == bg
            && last.bold == cell.bold
            && last.italic == cell.italic
            && last.underline == cell.underline
            && last.strikethrough == cell.strikethrough
            && last.faint == cell.faint
        {
            let mut merged = last.text.to_string();
            merged.push_str(&text);
            last.text = SharedString::from(merged);
            last.cols = last.cols.saturating_add(1);
            continue;
        }

        runs.push(CachedCellRun {
            text: SharedString::from(text),
            fg,
            bg,
            col_start: cell.col,
            cols: if is_wide { 2 } else { 1 },
            bold: cell.bold,
            italic: cell.italic,
            underline: cell.underline,
            strikethrough: cell.strikethrough,
            faint: cell.faint,
            has_wide: is_wide,
        });
    }
    runs
}

fn rgb_to_hsla(c: RgbColor) -> Hsla {
    hsla(
        c.r as f32 / 255.0,
        c.g as f32 / 255.0,
        c.b as f32 / 255.0,
        1.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use smallvec::smallvec;

    #[test]
    fn skips_wide_spacers() {
        let row = vec![
            seoul_vt::terminal::RenderedCell {
                col: 0,
                row: 0,
                graphemes: smallvec!['界'],
                fg: RgbColor { r: 255, g: 255, b: 255 },
                bg: RgbColor { r: 0, g: 0, b: 0 },
                bold: false,
                italic: false,
                underline: false,
                strikethrough: false,
                faint: false,
                wide: CellWidthKind::Wide,
            },
            seoul_vt::terminal::RenderedCell {
                col: 1,
                row: 0,
                graphemes: smallvec![],
                fg: RgbColor { r: 255, g: 255, b: 255 },
                bg: RgbColor { r: 0, g: 0, b: 0 },
                bold: false,
                italic: false,
                underline: false,
                strikethrough: false,
                faint: false,
                wide: CellWidthKind::SpacerTail,
            },
        ];
        let runs = build_runs_for_row(&row);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].cols, 2);
    }
}
```

- [ ] **Step 3: Store cache in TerminalView**

In `TerminalView`, replace `row_runs_buf: RowRunsBuffer` with:

```rust
render_cache: Rc<RefCell<TerminalRenderCache>>,
```

Initialize it in both constructors:

```rust
render_cache: Rc::new(RefCell::new(TerminalRenderCache::default())),
```

- [ ] **Step 4: Update render_terminal signature**

In `terminal_element.rs`, change `render_terminal` to accept:

```rust
render_cache: &Rc<RefCell<TerminalRenderCache>>
```

At the start of `render_terminal`, call:

```rust
render_cache.borrow_mut().update(content, config);
```

Use cached rows in paint instead of rebuilding `CellRun` from `content.cells`.

- [ ] **Step 5: Redraw glyph under block cursor**

In `terminal_element.rs`, after painting a block cursor quad, locate the cursor run and shape the visible text again with foreground = background theme color and no background. Use the existing `shape_line` call shape, with text from the cached run.

- [ ] **Step 6: Verify**

```bash
just test
just lint
```

Expected: both PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/seoul-terminal/src/main.rs crates/seoul-terminal/src/terminal_render_cache.rs crates/seoul-terminal/src/terminal_element.rs crates/seoul-terminal/src/terminal_view.rs
git commit -m "perf: cache terminal render rows"
```

## Task 6: Harden Restore And App-Side Backpressure

**Files:**
- Modify: `crates/seoul-daemon/src/mode_tracker.rs`
- Modify: `crates/seoul-terminal/src/terminal_view.rs`
- Modify: `crates/seoul-terminal/src/daemon_client.rs`

- [ ] **Step 1: Extend tracked modes**

In `TerminalModes`, add:

```rust
pub mouse_urxvt: bool,              // DECSET 1015
pub mouse_sgr_pixels: bool,         // DECSET 1016
pub synchronized_output: bool,      // DECSET 2026
pub grapheme_cluster: bool,         // DECSET 2027
pub color_scheme_report: bool,      // DECSET 2031
pub in_band_resize_reports: bool,   // DECSET 2048
```

Initialize each to `false` in `Default`.

- [ ] **Step 2: Update mode parsing and rehydrate**

In `set_mode`, add:

```rust
1015 => self.modes.mouse_urxvt = enabled,
1016 => self.modes.mouse_sgr_pixels = enabled,
2026 => self.modes.synchronized_output = enabled,
2027 => self.modes.grapheme_cluster = enabled,
2031 => self.modes.color_scheme_report = enabled,
2048 => self.modes.in_band_resize_reports = enabled,
```

In `generate_rehydrate_sequences`, add:

```rust
emit(1015, m.mouse_urxvt, d.mouse_urxvt);
emit(1016, m.mouse_sgr_pixels, d.mouse_sgr_pixels);
emit(2026, m.synchronized_output, d.synchronized_output);
emit(2027, m.grapheme_cluster, d.grapheme_cluster);
emit(2031, m.color_scheme_report, d.color_scheme_report);
emit(2048, m.in_band_resize_reports, d.in_band_resize_reports);
```

- [ ] **Step 3: Update recovered-session cleanup**

In `TerminalView::replay_attached_state`, inside `if attached_msg.was_recovered`, add:

```rust
terminal.feed_pty_data_silently(b"\x1b[?1015l");
terminal.feed_pty_data_silently(b"\x1b[?1016l");
terminal.feed_pty_data_silently(b"\x1b[?2026l");
terminal.feed_pty_data_silently(b"\x1b[?2048l");
```

Keep the existing bracketed paste cleanup only in this recovered-session branch.

- [ ] **Step 4: Bound app-side channels**

In `DaemonClientInner::create_or_attach`, replace:

```rust
let (data_tx, data_rx) = async_channel::unbounded::<Vec<u8>>();
```

with:

```rust
const APP_SESSION_DATA_CHANNEL_CAPACITY: usize = 512;
let (data_tx, data_rx) =
    async_channel::bounded::<Vec<u8>>(APP_SESSION_DATA_CHANNEL_CAPACITY);
```

Move the constant near `INITIAL_ATTACH_SCROLLBACK_LIMIT_BYTES`.

- [ ] **Step 5: Avoid blocking reader loop on full queue**

In `reader_loop`, replace `tx.send_blocking(msg.data).ok();` with:

```rust
if tx.try_send(msg.data).is_err() {
    drop(senders);
    session_senders.lock().unwrap().remove(&msg.session_id);
    tracing::warn!(
        session_id = %msg.session_id,
        "dropping stalled app-side terminal data receiver"
    );
}
```

Use a scoped lock so `senders` is dropped before removing from the map.

- [ ] **Step 6: Verify**

```bash
just test
just lint
```

Expected: both PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/seoul-daemon/src/mode_tracker.rs crates/seoul-terminal/src/terminal_view.rs crates/seoul-terminal/src/daemon_client.rs
git commit -m "fix: harden terminal restore and data backpressure"
```

## Task 7: Stabilize Ghostty Build And PTY Pixel Sizing

**Files:**
- Modify: `Cargo.toml`
- Modify: `justfile`
- Modify: `crates/seoul-vt/src/terminal.rs`
- Modify: `crates/seoul-daemon/src/session.rs`

- [ ] **Step 1: Pin Ghostty dependency**

In root `Cargo.toml`, replace:

```toml
libghostty-vt = { git = "https://github.com/Uzaaft/libghostty-rs" }
```

with:

```toml
libghostty-vt = { git = "https://github.com/Uzaaft/libghostty-rs", rev = "811cbdd85a99b40a63424a9d247c4f3653c11924" }
```

- [ ] **Step 2: Make dylib lookup deterministic**

In `justfile`, replace:

```make
ghostty_lib_dir := `find target/debug/build -path "*ghostty-vt-sys*/out/ghostty-install/lib" 2>/dev/null | head -1`
```

with:

```make
ghostty_lib_dir := `find target/debug/build -path "*ghostty-vt-sys*/out/ghostty-install/lib" 2>/dev/null | sort | tail -n 1`
```

- [ ] **Step 3: Clamp local PTY pixel dimensions**

In `crates/seoul-vt/src/terminal.rs`, replace `pixel_width as u16` and `pixel_height as u16` in `PtyResizer::resize` with:

```rust
pixel_width: pixel_width.min(u16::MAX as u32) as u16,
pixel_height: pixel_height.min(u16::MAX as u32) as u16,
```

- [ ] **Step 4: Clamp daemon PTY pixel dimensions**

In `crates/seoul-daemon/src/session.rs`, replace `pixel_width as u16` and `pixel_height as u16` in `DaemonSession::resize` with:

```rust
pixel_width: pixel_width.min(u16::MAX as u32) as u16,
pixel_height: pixel_height.min(u16::MAX as u32) as u16,
```

- [ ] **Step 5: Verify**

```bash
just test
just lint
```

Expected: both PASS.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml justfile crates/seoul-vt/src/terminal.rs crates/seoul-daemon/src/session.rs
git commit -m "fix: stabilize ghostty build and pty sizing"
```

## Final Verification And Manual QA

- [ ] **Step 1: Run full gate**

```bash
just lint && just test
```

Expected: both PASS.

- [ ] **Step 2: Manual app check**

Run:

```bash
just dev
just app
```

Manual scenarios:
- Paste text containing ESC, DEL, Ctrl-C, LF, and CRLF; verify sanitized paste reaches the shell.
- Run `cat -v`, then `printf '\e[?1004h'`; switch terminal focus out/in and verify focus reports.
- Run `printf '\e]2;Build\a'`; verify title-derived terminal breadcrumb state updates.
- Use `vim` or `less` with mouse enabled; verify click, drag, and wheel behavior.
- Print a wide character, move cursor onto its tail column, and verify cursor covers the wide glyph head.
- Resize a terminal with large scrollback; verify no panic or stale dylib runtime error.

- [ ] **Step 3: Final code review**

Use `superpowers:requesting-code-review` or a final reviewer subagent over the whole branch. Fix all Important or Critical findings before completion.
