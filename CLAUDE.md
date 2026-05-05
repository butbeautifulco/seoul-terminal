# CLAUDE.md

This file provides guidance to coding agents (Claude Code, Codex, etc.) when working with code in this repository.

## Project

Seoul — a terminal/IDE hybrid built in Rust on GPUI (Zed's UI framework). A long-running `seoul-daemon` owns PTYs and sessions; the `seoul` app connects over a Unix socket using MessagePack RPC.

Workspace crates:
- `seoul-terminal` — GPUI application binary (`seoul`), terminal + editor + workspace UI
- `seoul-daemon` — tokio async daemon; PTY/session management; listens on `~/.seoul/terminal-host.sock`
- `seoul-vt` — VT rendering layer wrapping `libghostty-vt`
- `seoul-terminal-proto` — RPC message definitions (frame, session, resources) and socket paths
- `seoul-workspace` — project persistence, git integration, settings store

## Development workflow (use justfile, not raw cargo)

- `just dev` — watchexec rebuild loop; on every rebuild it kills the daemon and clears the socket/PID so the next app launch reconnects to a fresh binary. Leave this running during development.
- `just app` — launch `target/debug/seoul`. **Required** because it sets `DYLD_LIBRARY_PATH` to the built `libghostty-vt` directory; running `cargo run` or the binary directly will fail to load the dylib on macOS.
- `just kill-daemon` — manual daemon kill + runtime file cleanup. Only needed when `just dev` is **not** running (e.g., daemon wedged, or you built once with `just build`). `just dev` already handles this on every rebuild.
- `just clean-runtime` — remove `~/.seoul/terminal-host.{sock,pid,token}` without killing.
- `just test` → `cargo test --workspace --locked`
- `just lint` → `cargo clippy --workspace --all-targets --locked -- -D warnings`

Rust is pinned in `rust-toolchain.toml` and CI installs that pinned toolchain, not floating `stable`. Keep the file pinned so local and CI Clippy use the same lint set. Zig is pinned in `.zigversion`; CI installs that version for `libghostty-vt`.

## Lint rules (workspace-wide, hard errors)

`dbg_macro`, `todo`, and `redundant_clone` are denied at the workspace level. Never leave `dbg!(...)`, `todo!()`, or cloned-then-moved values in committed code — not even in scratch paths. Use `tracing::debug!` or a temporary local variable instead.

## Daemon/proto changes

When editing `seoul-daemon` or `seoul-terminal-proto`, the running daemon must be replaced before the app sees the change. If `just dev` is running, the watchexec recipe already kills it on rebuild. Otherwise, run `just kill-daemon` manually before the next `just app`.

## Working with coding agents

- **Verify before claiming done.** Run `just lint && just test` before committing or saying work is finished. `just lint` mirrors the CI clippy command and is the real gate, not `cargo check` — the workspace denies `dbg_macro`/`todo`/`redundant_clone` so clippy catches things `check` doesn't.
- **Plan before large changes.** For multi-file edits, refactors, or new subsystems, outline the approach before touching code.
- **Explain tradeoffs when refactoring.** When multiple valid approaches exist, surface what's being traded off (perf vs. clarity, flexibility vs. simplicity, etc.) rather than silently picking one.
