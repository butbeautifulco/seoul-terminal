use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::*;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use seoul_workspace::git::cache::GitStatusCache;
use seoul_workspace::git::runner::GitCommandRunner;
use seoul_workspace::git::types::{FileStatus, GitChangesStatus};
use seoul_workspace::git::{branch, operations, status};

const POLL_INTERVAL: Duration = Duration::from_secs(30);
const DEBOUNCE_MS: u64 = 150;

pub enum GitStateEvent {
    StatusChanged,
    OperationSuccess { message: String },
    OperationError { message: String },
}

pub struct GitStateProvider {
    worktree_path: PathBuf,
    default_branch: String,
    runner: GitCommandRunner,
    cache: Arc<GitStatusCache>,
    current_status: GitChangesStatus,
    #[allow(dead_code)]
    poll_task: Option<Task<()>>,
    #[allow(dead_code)]
    watcher_task: Option<Task<()>>,
    #[allow(dead_code)]
    _watcher: Option<RecommendedWatcher>,
    is_busy: bool,
}

impl EventEmitter<GitStateEvent> for GitStateProvider {}

#[allow(dead_code)]
impl GitStateProvider {
    pub fn new(worktree_path: PathBuf, default_branch: String, cx: &mut Context<Self>) -> Self {
        let runner = GitCommandRunner::new(worktree_path.clone());
        let cache = Arc::new(GitStatusCache::new());

        let mut provider = Self {
            worktree_path,
            default_branch,
            runner,
            cache,
            current_status: GitChangesStatus::default(),
            poll_task: None,
            watcher_task: None,
            _watcher: None,
            is_busy: false,
        };

        // Initial refresh
        provider.refresh(cx);

        // Start fallback polling loop (30s)
        let poll_task = cx.spawn(async |this, cx: &mut AsyncApp| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let should_continue = this
                    .update(cx, |this, cx| {
                        this.refresh(cx);
                    })
                    .is_ok();
                if !should_continue {
                    break;
                }
            }
        });
        provider.poll_task = Some(poll_task);

        // Start filesystem watcher for real-time git status updates
        provider.start_fs_watcher(cx);

        provider
    }

    /// Get the current git status.
    pub fn status(&self) -> &GitChangesStatus {
        &self.current_status
    }

    /// Whether a git operation is currently in progress.
    pub fn is_busy(&self) -> bool {
        self.is_busy
    }

    pub fn set_default_branch(&mut self, default_branch: String, cx: &mut Context<Self>) {
        if self.default_branch == default_branch {
            return;
        }
        self.default_branch = default_branch;
        self.invalidate_and_refresh(cx);
    }

    /// Get a map of relative paths to their git status, for file tree decorations.
    pub fn file_status_map(&self) -> HashMap<String, FileStatus> {
        let mut map = HashMap::new();
        let s = &self.current_status;

        for file in &s.staged {
            map.insert(file.path.clone(), file.status);
        }
        for file in &s.unstaged {
            map.insert(file.path.clone(), file.status);
        }
        for file in &s.untracked {
            map.insert(file.path.clone(), file.status);
        }
        // against_base for files changed vs default branch
        for file in &s.against_base {
            map.entry(file.path.clone()).or_insert(file.status);
        }

        map
    }

    /// Trigger an async status refresh.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        // Check cache first
        if let Some(cached) = self.cache.get(&self.worktree_path) {
            if cached != self.current_status {
                self.current_status = cached;
                cx.emit(GitStateEvent::StatusChanged);
                cx.notify();
            }
            return;
        }

        let runner = self.runner.clone();
        let default_branch = self.default_branch.clone();
        let cache = self.cache.clone();
        let worktree_path = self.worktree_path.clone();

        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let result =
                smol::unblock(move || status::compute_status(&runner, &default_branch)).await;

            if let Ok(new_status) = result {
                cache.set(&worktree_path, new_status.clone());
                let _ = this.update(cx, |this, cx| {
                    if this.current_status != new_status {
                        this.current_status = new_status;
                        cx.emit(GitStateEvent::StatusChanged);
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    /// Invalidate cache and refresh immediately.
    fn invalidate_and_refresh(&mut self, cx: &mut Context<Self>) {
        self.cache.invalidate(&self.worktree_path);
        self.refresh(cx);
    }

    /// Start a filesystem watcher on the .git directory for real-time updates.
    fn start_fs_watcher(&mut self, cx: &mut Context<Self>) {
        let git_dir = resolve_git_dir(&self.worktree_path);
        if !git_dir.exists() {
            tracing::debug!("git dir not found at {:?}, skipping fs watcher", git_dir);
            return;
        }

        let (fs_tx, fs_rx) = smol::channel::unbounded::<()>();

        let watcher =
            notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res {
                    let is_git_state_change = event.paths.iter().any(|p| {
                        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        matches!(name, "HEAD" | "index" | "COMMIT_EDITMSG" | "MERGE_HEAD")
                            || p.components().any(|c| c.as_os_str() == "refs")
                    });
                    if is_git_state_change {
                        let _ = fs_tx.send_blocking(());
                    }
                }
            });

        match watcher {
            Ok(mut w) => {
                // Watch the git directory (HEAD, index, etc.)
                let _ = w.watch(&git_dir, RecursiveMode::NonRecursive);
                // Watch refs/ recursively for branch/tag changes
                let refs_dir = git_dir.join("refs");
                if refs_dir.exists() {
                    let _ = w.watch(&refs_dir, RecursiveMode::Recursive);
                }
                self._watcher = Some(w);

                let watcher_task = cx.spawn(async move |this, cx: &mut AsyncApp| {
                    loop {
                        if fs_rx.recv().await.is_err() {
                            break;
                        }

                        // Debounce: wait then drain queued events
                        cx.background_executor()
                            .timer(Duration::from_millis(DEBOUNCE_MS))
                            .await;
                        while fs_rx.try_recv().is_ok() {}

                        let should_continue = this
                            .update(cx, |this, cx| {
                                this.invalidate_and_refresh(cx);
                            })
                            .is_ok();
                        if !should_continue {
                            break;
                        }
                    }
                });
                self.watcher_task = Some(watcher_task);
            }
            Err(e) => {
                tracing::warn!("failed to start git fs watcher: {e}");
            }
        }
    }

    // -----------------------------------------------------------------------
    // Mutation operations — each invalidates cache and refreshes
    // -----------------------------------------------------------------------

    pub fn stage_file(&mut self, path: &str, cx: &mut Context<Self>) {
        let runner = self.runner.clone();
        let path = path.to_string();
        let worktree = self.worktree_path.clone();
        let cache = self.cache.clone();

        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let result = smol::unblock({
                let path = path.clone();
                move || operations::stage_file(&runner, &path)
            })
            .await;

            let _ = this.update(cx, |this, cx| {
                if let Err(e) = result {
                    cx.emit(GitStateEvent::OperationError {
                        message: format_error("stage", &e),
                    });
                } else {
                    cache.invalidate(&worktree);
                    this.refresh(cx);
                }
            });
        })
        .detach();
    }

    pub fn stage_all(&mut self, cx: &mut Context<Self>) {
        self.run_and_refresh(cx, "stage all", |runner| operations::stage_all(&runner));
    }

    pub fn unstage_file(&mut self, path: &str, cx: &mut Context<Self>) {
        let runner = self.runner.clone();
        let path = path.to_string();
        let worktree = self.worktree_path.clone();
        let cache = self.cache.clone();

        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let result = smol::unblock({
                let path = path.clone();
                move || operations::unstage_file(&runner, &path)
            })
            .await;

            let _ = this.update(cx, |this, cx| {
                if let Err(e) = result {
                    cx.emit(GitStateEvent::OperationError {
                        message: format_error("unstage", &e),
                    });
                } else {
                    cache.invalidate(&worktree);
                    this.refresh(cx);
                }
            });
        })
        .detach();
    }

    pub fn unstage_all(&mut self, cx: &mut Context<Self>) {
        self.run_and_refresh(cx, "unstage all", |runner| operations::unstage_all(&runner));
    }

    pub fn discard_file(&mut self, path: &str, cx: &mut Context<Self>) {
        let runner = self.runner.clone();
        let path = path.to_string();
        let worktree = self.worktree_path.clone();
        let cache = self.cache.clone();

        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let result = smol::unblock({
                let path = path.clone();
                move || operations::discard_file(&runner, &path)
            })
            .await;

            let _ = this.update(cx, |this, cx| {
                if let Err(e) = result {
                    cx.emit(GitStateEvent::OperationError {
                        message: format_error("discard", &e),
                    });
                } else {
                    cache.invalidate(&worktree);
                    this.refresh(cx);
                }
            });
        })
        .detach();
    }

    pub fn commit(&mut self, message: &str, cx: &mut Context<Self>) {
        if self.is_busy {
            return;
        }
        self.is_busy = true;
        cx.emit(GitStateEvent::StatusChanged);
        cx.notify();

        let runner = self.runner.clone();
        let message = message.to_string();
        let worktree = self.worktree_path.clone();
        let cache = self.cache.clone();

        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let result = smol::unblock({
                let message = message.clone();
                move || operations::commit(&runner, &message)
            })
            .await;

            let _ = this.update(cx, |this, cx| {
                this.is_busy = false;
                match result {
                    Ok(hash) => {
                        let short = if hash.len() > 7 { &hash[..7] } else { &hash };
                        cache.invalidate(&worktree);
                        this.refresh(cx);
                        cx.emit(GitStateEvent::OperationSuccess {
                            message: format!("Commit {short}"),
                        });
                    }
                    Err(e) => {
                        cx.emit(GitStateEvent::OperationError {
                            message: format_error("commit", &e),
                        });
                    }
                }
            });
        })
        .detach();
    }

    pub fn push(&mut self, cx: &mut Context<Self>) {
        let has_upstream = self.current_status.has_upstream;
        self.run_and_refresh(cx, "push", move |runner| {
            operations::push(&runner, !has_upstream)
        });
    }

    pub fn pull(&mut self, cx: &mut Context<Self>) {
        self.run_and_refresh(cx, "pull", |runner| operations::pull(&runner));
    }

    pub fn fetch(&mut self, cx: &mut Context<Self>) {
        self.run_and_refresh(cx, "fetch", |runner| operations::fetch(&runner));
    }

    pub fn sync(&mut self, cx: &mut Context<Self>) {
        self.run_and_refresh(cx, "sync", |runner| operations::sync(&runner));
    }

    pub fn stash_push(&mut self, include_untracked: bool, cx: &mut Context<Self>) {
        self.run_and_refresh(cx, "stash push", move |runner| {
            operations::stash_push(&runner, include_untracked)
        });
    }

    pub fn stash_pop(&mut self, cx: &mut Context<Self>) {
        self.run_and_refresh(cx, "stash pop", |runner| operations::stash_pop(&runner));
    }

    pub fn switch_branch(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.is_busy {
            return;
        }
        self.is_busy = true;
        cx.emit(GitStateEvent::StatusChanged);
        cx.notify();

        let runner = self.runner.clone();
        let name = name.to_string();
        let worktree = self.worktree_path.clone();
        let cache = self.cache.clone();

        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let result = smol::unblock({
                let name = name.clone();
                move || branch::switch_branch(&runner, &name)
            })
            .await;

            let _ = this.update(cx, |this, cx| {
                this.is_busy = false;
                match result {
                    Ok(()) => {
                        cache.invalidate(&worktree);
                        this.refresh(cx);
                        cx.emit(GitStateEvent::OperationSuccess {
                            message: format!("Switched to {name}"),
                        });
                    }
                    Err(e) => {
                        cx.emit(GitStateEvent::OperationError {
                            message: format_error("switch branch", &e),
                        });
                    }
                }
            });
        })
        .detach();
    }

    // -----------------------------------------------------------------------
    // Helper
    // -----------------------------------------------------------------------

    fn run_and_refresh<F>(&mut self, cx: &mut Context<Self>, op_name: &str, operation: F)
    where
        F: FnOnce(GitCommandRunner) -> anyhow::Result<()> + Send + 'static,
    {
        if self.is_busy {
            return;
        }
        self.is_busy = true;
        cx.emit(GitStateEvent::StatusChanged);
        cx.notify();

        let runner = self.runner.clone();
        let worktree = self.worktree_path.clone();
        let cache = self.cache.clone();
        let op_name = op_name.to_string();

        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let result = smol::unblock(move || operation(runner)).await;

            let _ = this.update(cx, |this, cx| {
                this.is_busy = false;
                match result {
                    Ok(()) => {
                        cache.invalidate(&worktree);
                        this.refresh(cx);
                        cx.emit(GitStateEvent::OperationSuccess {
                            message: format_success(&op_name),
                        });
                    }
                    Err(e) => {
                        cx.emit(GitStateEvent::OperationError {
                            message: format_error(&op_name, &e),
                        });
                    }
                }
            });
        })
        .detach();
    }
}

/// Resolve the actual .git directory, handling worktree indirection.
/// In a worktree, `.git` is a file containing `gitdir: <path>`.
fn resolve_git_dir(worktree_path: &Path) -> PathBuf {
    let dot_git = worktree_path.join(".git");
    if dot_git.is_file()
        && let Ok(contents) = std::fs::read_to_string(&dot_git)
        && let Some(gitdir) = contents.strip_prefix("gitdir: ")
    {
        let gitdir = gitdir.trim();
        let p = Path::new(gitdir);
        if p.is_absolute() {
            return p.to_path_buf();
        } else {
            return worktree_path.join(gitdir);
        }
    }
    dot_git
}

fn format_success(op: &str) -> String {
    match op {
        "push" => "Push 완료".to_string(),
        "pull" => "Pull 완료".to_string(),
        "fetch" => "Fetch 완료".to_string(),
        "sync" => "Sync 완료".to_string(),
        "stash push" => "Stash 저장 완료".to_string(),
        "stash pop" => "Stash 복원 완료".to_string(),
        "stage all" => "전체 Stage 완료".to_string(),
        "unstage all" => "전체 Unstage 완료".to_string(),
        _ => format!("{op} 완료"),
    }
}

fn format_error(op: &str, err: &anyhow::Error) -> String {
    let err_str = err.to_string();
    if err_str.contains("no tracking information") {
        return "이 브랜치에 upstream이 설정되어 있지 않습니다".to_string();
    }
    if err_str.contains("commit message cannot be empty") {
        return "커밋 메시지를 입력해주세요".to_string();
    }
    if err_str.contains("nothing to commit") {
        return "커밋할 변경사항이 없습니다".to_string();
    }
    if err_str.contains("CONFLICT") || err_str.contains("conflict") {
        return "Merge conflict이 발생했습니다".to_string();
    }
    format!("{op} 실패: {err_str}")
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use notify::{RecommendedWatcher, RecursiveMode, Watcher};

    #[test]
    fn recommended_git_watcher_uses_fsevents_on_macos() {
        assert_eq!(
            <RecommendedWatcher as Watcher>::kind(),
            notify::WatcherKind::Fsevent
        );
    }

    #[test]
    fn dropping_recommended_git_watcher_does_not_panic_on_macos() {
        let watch_dir =
            std::env::temp_dir().join(format!("seoul-notify-drop-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&watch_dir).expect("create temp watch directory");

        let result = std::panic::catch_unwind(|| {
            let mut watcher = notify::recommended_watcher(
                |_: std::result::Result<notify::Event, notify::Error>| {},
            )
            .expect("create watcher");
            watcher
                .watch(&watch_dir, RecursiveMode::NonRecursive)
                .expect("watch temp directory");
            drop(watcher);
        });

        let _ = std::fs::remove_dir(&watch_dir);
        assert!(result.is_ok(), "dropping watcher panicked");
    }
}
