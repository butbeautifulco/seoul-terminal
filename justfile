# Seoul — Development Workflow
# Requires: watchexec (`cargo install watchexec-cli`)

ghostty_lib_dir := `find target/debug/build -path "*ghostty-vt-sys*/out/ghostty-install/lib" 2>/dev/null | sort | tail -n 1`

# Build all crates
build:
    cargo build

# Run pre-built app binary (spawns daemon automatically)
app:
    DYLD_LIBRARY_PATH="{{ghostty_lib_dir}}" target/debug/seoul

# Dev mode: watch all crates, rebuild on change, kill daemon so app auto-reconnects with new binary
# -o queue: if files change during build, re-build after current build finishes
dev:
    watchexec -w crates -e rs,toml -o queue -- 'cargo build && kill $(cat ~/.seoul/terminal-host.pid 2>/dev/null) 2>/dev/null; rm -f ~/.seoul/terminal-host.sock ~/.seoul/terminal-host.pid ~/.seoul/terminal-host.lock; true'

# Release build
release:
    cargo build --release

# Build a zip containing Seoul.app for internal macOS Apple Silicon distribution
package-macos:
    bash scripts/package-macos.sh

# Run all tests
test:
    cargo test --workspace

# Lint
lint:
    cargo clippy --workspace --all-targets --locked -- -D warnings

# Kill running daemon (SIGKILL to avoid graceful shutdown delay, clean up all runtime files)
kill-daemon:
    kill -9 $(cat ~/.seoul/terminal-host.pid) 2>/dev/null || echo "no daemon running"
    pkill -9 -f "$(pwd)/target/debug/seoul-daemon" 2>/dev/null || true
    rm -f ~/.seoul/terminal-host.sock ~/.seoul/terminal-host.pid ~/.seoul/terminal-host.token ~/.seoul/terminal-host.lock

# Clean up runtime files (socket, PID, token)
clean-runtime:
    rm -f ~/.seoul/terminal-host.sock ~/.seoul/terminal-host.pid ~/.seoul/terminal-host.token ~/.seoul/terminal-host.lock
