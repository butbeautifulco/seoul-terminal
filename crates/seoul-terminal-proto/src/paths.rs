use std::path::PathBuf;

use uuid::Uuid;

pub fn seoul_dir() -> PathBuf {
    dirs::home_dir()
        .expect("home directory not found")
        .join(".seoul")
}

pub fn socket_path() -> PathBuf {
    seoul_dir().join("terminal-host.sock")
}

pub fn pid_path() -> PathBuf {
    seoul_dir().join("terminal-host.pid")
}

pub fn token_path() -> PathBuf {
    seoul_dir().join("terminal-host.token")
}

pub fn lock_path() -> PathBuf {
    seoul_dir().join("terminal-host.lock")
}

pub fn daemon_log_path() -> PathBuf {
    seoul_dir().join("daemon.log")
}

pub fn terminal_history_dir() -> PathBuf {
    seoul_dir().join("terminal-history")
}

pub fn session_history_dir(session_id: Uuid) -> PathBuf {
    terminal_history_dir().join(session_id.to_string())
}

pub fn scrollback_path(session_id: Uuid) -> PathBuf {
    session_history_dir(session_id).join("scrollback.bin")
}

pub fn meta_path(session_id: Uuid) -> PathBuf {
    session_history_dir(session_id).join("meta.json")
}
