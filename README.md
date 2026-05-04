# Seoul

A terminal/IDE hybrid for macOS, written in Rust on top of [GPUI](https://www.gpui.rs/) (the UI framework from Zed). A long-running daemon owns PTYs and sessions; the desktop app connects over a Unix socket using MessagePack RPC, so the UI process can be restarted without losing terminal state.

## Status

Early development. Apple Silicon only for now. Expect rough edges and breaking changes.

## Building

You will need a recent Rust toolchain (the workspace pins a `rust-toolchain.toml`) and [`just`](https://github.com/casey/just). For the watch loop, install `watchexec`:

```
cargo install watchexec-cli
```

Then:

```
just dev   # rebuild on file change; kills the daemon so the app reconnects to the fresh binary
just app   # launch the desktop app (sets DYLD_LIBRARY_PATH for libghostty-vt)
```

`just app` is required on macOS because running the binary directly will fail to load the bundled `libghostty-vt` dylib. If you ever build without `just dev` running, use `just kill-daemon` before relaunching so a stale daemon does not serve the new app.

```
just test   # cargo test --workspace
just lint   # cargo clippy --workspace -- -D warnings
```

`dbg!`, `todo!`, and `redundant_clone` are denied at the workspace level, so `just lint` is the real gate.

## Workspace layout

```
crates/
  seoul-terminal         GPUI app binary (`seoul`); terminal + editor + workspace UI
  seoul-daemon           tokio async daemon; owns PTYs and sessions; ~/.seoul/terminal-host.sock
  seoul-vt               VT rendering layer wrapping libghostty-vt
  seoul-terminal-proto   RPC message types (frames, sessions, resources) and socket paths
  seoul-workspace        project persistence, git integration, settings store
```

The app and the daemon talk to each other over a Unix domain socket at `~/.seoul/terminal-host.sock`. The daemon writes a PID file and an auth token into the same directory.

## License

Apache License 2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
