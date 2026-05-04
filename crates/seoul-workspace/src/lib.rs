pub mod git;
pub mod persistence;
pub mod project;
mod seoul_dong;
pub mod settings;
pub mod workspace;
pub mod worktree;

pub use persistence::AppState;
pub use project::Project;
pub use workspace::Workspace;

use std::path::PathBuf;

pub fn seoul_dir() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("No home directory"))?;
    Ok(home.join(".seoul"))
}
