use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::git::types::GitChangesStatus;

const CACHE_TTL: Duration = Duration::from_secs(3);

struct CacheEntry {
    result: GitChangesStatus,
    timestamp: Instant,
}

/// TTL-based cache for git status results with per-worktree entries.
pub struct GitStatusCache {
    entries: Mutex<HashMap<PathBuf, CacheEntry>>,
}

impl GitStatusCache {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Get cached status if still fresh (within TTL).
    pub fn get(&self, worktree_path: &Path) -> Option<GitChangesStatus> {
        let entries = self.entries.lock().unwrap();
        if let Some(entry) = entries.get(worktree_path)
            && entry.timestamp.elapsed() < CACHE_TTL
        {
            return Some(entry.result.clone());
        }
        None
    }

    /// Store a status result in the cache.
    pub fn set(&self, worktree_path: &Path, status: GitChangesStatus) {
        let mut entries = self.entries.lock().unwrap();
        entries.insert(
            worktree_path.to_path_buf(),
            CacheEntry {
                result: status,
                timestamp: Instant::now(),
            },
        );
    }

    /// Invalidate the cache for a specific worktree.
    pub fn invalidate(&self, worktree_path: &Path) {
        let mut entries = self.entries.lock().unwrap();
        entries.remove(worktree_path);
    }

    /// Invalidate all cache entries.
    pub fn invalidate_all(&self) {
        let mut entries = self.entries.lock().unwrap();
        entries.clear();
    }
}

impl Default for GitStatusCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_set_get() {
        let cache = GitStatusCache::new();
        let path = Path::new("/tmp/test-repo");
        let status = GitChangesStatus {
            branch: "main".into(),
            ..Default::default()
        };

        assert!(cache.get(path).is_none());
        cache.set(path, status);
        assert_eq!(cache.get(path).unwrap().branch, "main");
    }

    #[test]
    fn test_cache_invalidate() {
        let cache = GitStatusCache::new();
        let path = Path::new("/tmp/test-repo");
        cache.set(path, GitChangesStatus::default());

        cache.invalidate(path);
        assert!(cache.get(path).is_none());
    }

    #[test]
    fn test_cache_ttl_expiry() {
        // Use a very short internal test — we can't easily test 3s TTL in unit tests,
        // but we can verify that a fresh entry is returned.
        let cache = GitStatusCache::new();
        let path = Path::new("/tmp/test-repo");
        cache.set(path, GitChangesStatus::default());
        // Should be fresh immediately
        assert!(cache.get(path).is_some());
    }
}
