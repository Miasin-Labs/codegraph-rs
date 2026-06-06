//! Sync Module
//!
//! Provides synchronization functionality for keeping the code graph
//! up-to-date with file system changes. (Port of `src/sync/index.ts`.)
//!
//! Components:
//! - [`FileWatcher`]: Debounced native file watching (notify) that
//!   auto-triggers sync on file changes
//! - Watch policy: decides when the watcher must be disabled (e.g. WSL2 /mnt)
//! - Git sync hooks: opt-in commit/merge/checkout hooks when watching is off
//! - Git worktree awareness: detect when a query borrows another tree's index
//! - Content hashing for change detection (in extraction module)
//! - Incremental reindexing (in extraction module)

pub mod git_hooks;
pub mod watch_policy;
pub mod watcher;
pub mod worktree;

pub use git_hooks::{
    DEFAULT_SYNC_HOOKS,
    GitHookName,
    GitHookResult,
    install_git_sync_hook,
    is_git_repo,
    is_sync_hook_installed,
    remove_git_sync_hook,
};
pub use watch_policy::{WatchProbe, detect_wsl, watch_disabled_reason};
pub use watcher::{
    DEFAULT_DEBOUNCE_MS,
    DEFAULT_READY_TIMEOUT_MS,
    FileWatcher,
    LockUnavailableError,
    PendingFile,
    SyncError,
    SyncFn,
    WatchOptions,
    WatchSyncResult,
    emit_watch_event_for_tests,
};
pub use worktree::{
    WorktreeIndexMismatch,
    detect_worktree_index_mismatch,
    git_worktree_root,
    worktree_mismatch_notice,
    worktree_mismatch_warning,
};
