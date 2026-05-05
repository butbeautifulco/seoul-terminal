mod host;
mod mode_tracker;
mod pr_poller;
mod process_info;
mod resource_monitor;
mod scrollback;
mod server;
mod session;
mod shell_readiness;
mod transient_errors;

use std::fs;
use std::process;
use std::sync;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::net::UnixListener;
use tokio::signal;
use tracing::{info, warn};

use seoul_daemon::lock::{self, LockHandle};
use seoul_terminal_proto::paths;
use seoul_workspace::git::github_auth;
use seoul_workspace::git::hosting::HostingRegistry;
use seoul_workspace::git::providers::GitHubProvider;

#[tokio::main]
async fn main() -> Result<()> {
    let seoul_dir = paths::seoul_dir();
    fs::create_dir_all(&seoul_dir).context("failed to create ~/.seoul")?;

    // Log to file (~/.seoul/daemon.log) since the process runs with null stdio.
    let log_path = paths::daemon_log_path();
    if let Ok(meta) = fs::metadata(&log_path)
        && meta.len() > 10 * 1024 * 1024
    {
        fs::write(&log_path, b"").ok();
    }
    let log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .context("failed to open daemon log")?;
    tracing_subscriber::fmt()
        .with_writer(sync::Mutex::new(log_file))
        .with_ansi(false)
        .init();

    let daemon_lock = acquire_daemon_lock()?;

    // Clean up stale runtime files. The daemon lock above proved there is
    // no live owner, so any existing socket/token is stale.
    let socket_path = paths::socket_path();
    if socket_path.exists() {
        fs::remove_file(&socket_path).ok();
    }
    let token_path = paths::token_path();
    if token_path.exists() {
        fs::remove_file(&token_path).ok();
    }

    // Generate auth token. Write atomically with mode 0o600 so the file is
    // never world-readable even for the brief window between create and
    // chmod. `create_new` errors if the file already exists, which would
    // indicate a race with another daemon — but the lock above already
    // serializes startup, so a present file means cleanup above failed.
    let token = generate_token();
    write_token_atomic(&token_path, &token)?;

    // Bind socket
    let listener = UnixListener::bind(&socket_path).context("failed to bind Unix socket")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600)).ok();
    }

    info!(pid = process::id(), "seoul-daemon started");

    // Build the GitHub-authenticated octocrab client and the hosting registry.
    // If no token is available we still build an unauthenticated client so the
    // daemon can boot; the first GraphQL call will return 401 → NotAuthenticated,
    // which the UI surfaces as "Connect GitHub".
    let token_opt = github_auth::load_github_token().ok();
    if token_opt.is_none() {
        warn!(
            "no GitHub token available — PR sync will surface NotAuthenticated until `gh auth login` is run"
        );
    }
    let octo = {
        let mut b = octocrab::Octocrab::builder();
        if let Some(t) = token_opt {
            b = b.personal_token(t);
        }
        Arc::new(b.build().context("failed to build octocrab client")?)
    };
    let registry =
        Arc::new(HostingRegistry::new().with_provider(Arc::new(GitHubProvider::new(octo.clone()))));

    // Run server
    let host = host::TerminalHost::new();
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let shared_host = server::run(listener, host, token, shutdown_tx, octo, registry).await;

    // Wait for shutdown signal (SIGTERM, Ctrl-C, or client Shutdown message)
    let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())
        .expect("failed to register SIGTERM handler");

    tokio::select! {
        _ = signal::ctrl_c() => {
            info!("received SIGINT");
        }
        _ = sigterm.recv() => {
            info!("received SIGTERM");
        }
        Ok(_) = shutdown_rx.wait_for(|v| *v) => {
            info!("received shutdown request from client");
        }
    }

    // Remove runtime files FIRST so a new daemon can start immediately,
    // then do the slow cleanup (scrollback flush, session kill).
    fs::remove_file(&socket_path).ok();
    fs::remove_file(paths::pid_path()).ok();
    fs::remove_file(paths::lock_path()).ok();
    drop(daemon_lock);
    info!("runtime files removed, flushing scrollback");

    {
        let mut h = shared_host.lock().await;
        h.graceful_shutdown();
    }

    info!("seoul-daemon exiting");
    Ok(())
}

/// Acquire the daemon singleton lock and write a PID file for human inspection.
///
/// The lock is held by `flock(2)` on the lock file's open fd: the kernel
/// releases it on close (i.e. on process exit, even via SIGKILL), so a
/// crashed daemon never wedges the next launch. The PID file is written for
/// debuggability only — it is no longer the source of truth for "is a
/// daemon running?".
fn acquire_daemon_lock() -> Result<LockHandle> {
    let lock_path = paths::lock_path();
    let handle = lock::acquire(&lock_path).context("failed to acquire daemon lock")?;

    // PID file is informational; ignore write failures.
    if let Err(e) = fs::write(paths::pid_path(), process::id().to_string()) {
        warn!("failed to write PID file: {e}");
    }

    Ok(handle)
}

fn generate_token() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..32)
        .map(|_| format!("{:02x}", rng.r#gen::<u8>()))
        .collect()
}

/// Write `token` to `path` atomically with mode 0o600, refusing to clobber.
///
/// Uses `O_CREAT | O_EXCL` (`create_new`) so the file is created with the
/// requested mode in a single syscall — no window where it exists with the
/// process umask before chmod tightens it. If the file already exists this
/// returns an error rather than silently truncating, since that indicates
/// stale runtime state the caller should investigate.
#[cfg(unix)]
fn write_token_atomic(path: &std::path::Path, token: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut f = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create token at {}", path.display()))?;
    f.write_all(token.as_bytes())
        .with_context(|| format!("write token at {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_token_atomic(path: &std::path::Path, token: &str) -> Result<()> {
    fs::write(path, token).with_context(|| format!("write token at {}", path.display()))
}
