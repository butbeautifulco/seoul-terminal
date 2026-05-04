# Ghostty P0-P2 Integration Spec

## Summary

Seoul already uses `libghostty-vt` in the right high-level shape: the app owns Ghostty state on the GPUI thread, and the daemon owns PTYs/sessions. The gaps are at the integration boundary. This spec defines one coordinated implementation track for P0, P1, and P2 Ghostty work so later implementation can run with `superpowers:subagent-driven-development`.

The implementation must preserve the existing daemon/app architecture. It should make Seoul use Ghostty APIs for input and effects, fix visible cursor correctness, improve render hot paths, harden session restore/backpressure, and stabilize the Ghostty build/runtime linkage.

## Goals

- Use Ghostty paste encoding instead of Seoul's hand-written paste sanitizer.
- Report focus events when DEC focus reporting mode 1004 is enabled.
- Register Ghostty effect callbacks for terminal responses and user-visible state.
- Fix cursor rendering on wide-character tail cells and block cursor glyph visibility.
- Wire mouse press/release/motion and encoder size into `TerminalView`.
- Improve key event fidelity without changing existing IME behavior.
- Reduce terminal render work by caching row/run data and using dirty rows.
- Make daemon restore less lossy for tracked terminal modes and bound app-side data queues.
- Make Ghostty dependency/build behavior reproducible.

## Non-Goals

- Do not move Ghostty into `seoul-daemon`.
- Do not implement full Ghostty state snapshot/restore in the daemon.
- Do not add a product UI for unsafe paste confirmation.
- Do not add visible/audio bell UI beyond exposing a bell counter.
- Do not rewrite the entire terminal renderer.
- Do not replace the existing resize debounce design.

## Architecture

`seoul-vt::Terminal` remains the single owner of `libghostty_vt::Terminal`, render state, key encoder, and mouse encoder. The new `effects` module stores callback-owned side-effect state behind an `Arc<Mutex<_>>` so synchronous Ghostty callbacks can update title, bell, and size data without violating the single-threaded Ghostty object ownership model.

`TerminalView` remains the GPUI integration layer. It translates GPUI focus, mouse, key, resize, and IME events into `seoul-vt::Terminal` calls. It also owns render caches because GPUI text shaping is view-specific.

The daemon remains PTY/session owner. It should track only the minimal VT modes needed for warm/cold rehydrate and should bound client-side data queues so a stalled UI does not grow memory without limit.

## Reference Implementation

Use `Uzaaft/libghostty-rs` `example/ghostling_rs/src/main.rs` at commit `811cbdd85a99b40a63424a9d247c4f3653c11924` as a reference for idiomatic `libghostty-vt` usage: `https://github.com/Uzaaft/libghostty-rs/blob/811cbdd85a99b40a63424a9d247c4f3653c11924/example/ghostling_rs/src/main.rs`. It is a macroquad terminal, so Seoul must not copy the renderer, event polling, direct PTY ownership, or frame loop. The reusable parts are the `libghostty-vt` integration patterns:

- Register terminal effects early: `on_pty_write`, `on_size`, `on_device_attributes`, `on_xtversion`, and `on_color_scheme`.
- Keep `RenderState`, `RowIterator`, `CellIterator`, `key::Encoder`, `key::Event`, `mouse::Encoder`, and `mouse::Event` reusable instead of recreating them per frame.
- Update encoder options from terminal state before each key or mouse encode.
- For keys, set action, key, mods, consumed mods, unshifted codepoint, and optional UTF-8 text on the reusable `key::Event`.
- For mouse, set encoder size, any-button-pressed state, and track-last-cell state before encoding press/release/motion events.
- Consume render dirty state explicitly after row extraction/rendering.

Seoul-specific additions remain required: GPUI IME preservation, GPUI text shaping, daemon-backed session restore, title/bell/enquiry effects, wide-tail cursor correction, and app-side daemon queue bounds.

## Detailed Requirements

### P0: Correctness And Compatibility

- Paste:
  - `Terminal::paste` must call `libghostty_vt::paste::encode`.
  - It must preserve Ghostty/xterm behavior:
    - bracketed paste wraps with `ESC[200~` and `ESC[201~`;
    - unsafe control bytes are replaced with spaces;
    - non-bracketed `\n` becomes `\r`;
    - `\r\n` becomes `\r\r`.
  - `Terminal::paste_is_safe(text)` must expose `libghostty_vt::paste::is_safe`.

- Focus:
  - `Terminal::focus_in` writes `ESC[I` only when `Mode::FOCUS_EVENT` is active.
  - `Terminal::focus_out` writes `ESC[O` only when `Mode::FOCUS_EVENT` is active.

- Effects:
  - Register `on_pty_write`, `on_bell`, `on_title_changed`, `on_enquiry`, `on_xtversion`, `on_size`, `on_color_scheme`, and `on_device_attributes`.
  - `on_enquiry` returns `seoul`.
  - `on_xtversion` returns `seoul 0.1.0` via `env!("CARGO_PKG_VERSION")`.
  - `on_size` reports current rows, columns, cell width, and cell height.
  - `on_color_scheme` returns `Dark` for the current default terminal theme.
  - `on_device_attributes` returns a stable VT420-compatible baseline with ANSI color and selective erase.
  - `feed_pty_data_silently` must suppress title/bell side effects and VT responses during scrollback replay.

- Cursor:
  - If Ghostty reports `CursorViewport.at_wide_tail`, Seoul must render cursor at the wide glyph's head column and set `CursorInfo.is_wide = true`.
  - Block cursor must not hide the underlying glyph; redraw the cursor cell glyph over the block using inverse colors.

### P1: Input Fidelity And Performance

- Mouse:
  - `TerminalView` must set Ghostty mouse encoder size whenever terminal bounds or cell metrics change.
  - Left, right, middle press/release, release-out, and motion must be sent to Ghostty when mouse tracking is active.
  - Mouse motion must set `mouse::Action::Motion`.
  - `Terminal::set_mouse_any_button_pressed` must update Ghostty encoder button state.
  - `Terminal::set_mouse_track_last_cell(true)` must be enabled for terminal mouse reporting, matching the ghostling encoder pattern.
  - Wheel behavior keeps the current alt-screen/alt-scroll behavior.

- Keys:
  - Preserve current IME path for unmodified printable text.
  - Extend mapped keys to Insert, F13-F25, numpad digits, numpad operators, numpad enter/backspace, and numpad navigation keys.
  - `Terminal::try_keystroke` must set `set_composing(false)` and set unshifted codepoint for single-character key strings.
  - Keep current macOS Option behavior: GPUI Alt maps to `key::Mods::ALT`; do not add a setting in this spec.

- Rendering:
  - `TerminalContent` must expose `dirty_rows` and `content_generation`.
  - `TerminalView` must cache per-row render runs and avoid rebuilding unchanged row run strings.
  - The renderer must keep existing paint order: background, text, cursor, scrollbar.
  - Renderer cache must preserve wide-cell spacer skipping and faint/inverse/underline/strikethrough styling.

### P2: Restore, Backpressure, And Build Stability

- Restore:
  - `ModeTracker` must track modes 1015, 1016, 2026, 2027, 2031, and 2048 in addition to existing tracked modes.
  - Rehydrate sequences must include every tracked non-default mode.
  - Cold recovered sessions must clear stale mouse modes, synchronized output, bracketed paste, and cursor visibility state as needed to avoid broken new shells.

- Backpressure:
  - App-side per-session daemon data channels must be bounded.
  - If a session data queue is full, the reader loop must drop that session sender and log a warning rather than blocking the daemon socket reader indefinitely or growing memory without bound.

- Build and PTY sizing:
  - `Cargo.toml` must pin `libghostty-vt` to `811cbdd85a99b40a63424a9d247c4f3653c11924`.
  - `justfile` must choose Ghostty dylib path deterministically with `sort | tail -n 1`.
  - PTY pixel width/height must clamp to `u16::MAX` instead of truncating casts.

## Acceptance Criteria

- `just test` passes.
- `just lint` passes.
- New tests cover paste, focus, callbacks, wide-tail cursor, and mouse encoder behavior.
- Manual `just app` checks confirm paste sanitization, focus reports, title update, mouse tracking in terminal apps, and wide cursor behavior.
- No `dbg!`, `todo!`, or redundant clone warnings are introduced.
- No implementation subagent starts on `main` or `master`; implementation must run in a dedicated worktree.

## Subagent Execution Model

Use `superpowers:subagent-driven-development` after these documents are accepted. Dispatch one implementer subagent per task, then run spec compliance review and code quality review before moving to the next task. Do not dispatch multiple implementation subagents in parallel because the tasks touch overlapping terminal files.
