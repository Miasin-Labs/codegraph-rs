//! Git Sync Hooks Tests
//!
//! Port of `__tests__/git-hooks.test.ts`.
//!
//! Covers installing/removing the opt-in commit/merge/checkout hooks that
//! keep the index fresh when the live watcher is disabled (issue #199).
//! Exercises real git repos in temp dirs — no mocking.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use codegraph::sync::git_hooks::{
    DEFAULT_SYNC_HOOKS,
    install_git_sync_hook,
    is_git_repo,
    is_sync_hook_installed,
    remove_git_sync_hook,
};
use tempfile::TempDir;

fn git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git should be runnable");
    assert!(status.success(), "git {args:?} failed in {}", cwd.display());
}

fn git_stdout(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git should be runnable");
    assert!(
        out.status.success(),
        "git {args:?} failed in {}",
        cwd.display()
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn git_init(dir: &Path) {
    git(dir, &["init", "-q"]);
    // Neutralize any global core.hooksPath (e.g. husky) — same as the TS suite.
    git(dir, &["config", "core.hooksPath", ".git/hooks"]);
}

#[cfg(unix)]
fn is_executable(file: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(file).unwrap().permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_file: &Path) -> bool {
    true // mode bits not meaningful on Windows
}

fn resolved_hooks_dir(repo: &Path) -> PathBuf {
    let out = git_stdout(repo, &["rev-parse", "--git-path", "hooks"]);
    let p = Path::new(&out);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        repo.join(p)
    }
}

#[test]
fn installs_all_default_hooks_executable_invoking_codegraph_sync() {
    let repo = TempDir::new().unwrap();
    git_init(repo.path());

    let result = install_git_sync_hook(repo.path(), &DEFAULT_SYNC_HOOKS);

    let mut installed: Vec<&str> = result.installed.iter().map(|h| h.as_str()).collect();
    installed.sort();
    let mut expected: Vec<&str> = DEFAULT_SYNC_HOOKS.iter().map(|h| h.as_str()).collect();
    expected.sort();
    assert_eq!(installed, expected);
    assert!(result.skipped.is_none());
    let hooks_dir = result.hooks_dir.as_ref().expect("hooks_dir should be set");

    for hook in DEFAULT_SYNC_HOOKS {
        let file = hooks_dir.join(hook.as_str());
        assert!(file.exists(), "{} should exist", file.display());
        let body = fs::read_to_string(&file).unwrap();
        assert!(body.contains("codegraph sync"));
        assert!(body.contains("command -v codegraph")); // no-op when not on PATH
        assert!(is_executable(&file));
    }
    assert!(is_sync_hook_installed(repo.path(), &DEFAULT_SYNC_HOOKS));
}

#[test]
fn is_idempotent_reinstall_does_not_duplicate_the_block() {
    let repo = TempDir::new().unwrap();
    git_init(repo.path());

    let result = install_git_sync_hook(repo.path(), &DEFAULT_SYNC_HOOKS);
    install_git_sync_hook(repo.path(), &DEFAULT_SYNC_HOOKS);

    let body = fs::read_to_string(result.hooks_dir.unwrap().join("post-commit")).unwrap();
    let occurrences = body.matches("# >>> codegraph sync hook >>>").count();
    assert_eq!(occurrences, 1);
}

#[test]
fn preserves_a_preexisting_user_hook_and_appends_our_block() {
    let repo = TempDir::new().unwrap();
    git_init(repo.path());

    let hooks_dir = resolved_hooks_dir(repo.path());
    fs::create_dir_all(&hooks_dir).unwrap();
    let file = hooks_dir.join("post-commit");
    fs::write(&file, "#!/bin/sh\necho \"my custom hook\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
    }

    install_git_sync_hook(repo.path(), &[codegraph::sync::GitHookName::PostCommit]);

    let body = fs::read_to_string(&file).unwrap();
    assert!(body.contains("echo \"my custom hook\""));
    assert!(body.contains("codegraph sync"));
}

#[test]
fn remove_strips_our_block_and_deletes_a_hook_that_was_only_ours() {
    let repo = TempDir::new().unwrap();
    git_init(repo.path());

    let hooks = [codegraph::sync::GitHookName::PostCommit];
    let installed = install_git_sync_hook(repo.path(), &hooks);
    let file = installed.hooks_dir.as_ref().unwrap().join("post-commit");
    assert!(file.exists());

    let result = remove_git_sync_hook(repo.path(), &hooks);
    assert_eq!(
        result.installed,
        vec![codegraph::sync::GitHookName::PostCommit]
    );
    assert!(!file.exists()); // was ours-only → deleted
    assert!(!is_sync_hook_installed(repo.path(), &DEFAULT_SYNC_HOOKS));
}

#[test]
fn remove_keeps_user_content_when_the_hook_is_shared() {
    let repo = TempDir::new().unwrap();
    git_init(repo.path());

    let hooks_dir = resolved_hooks_dir(repo.path());
    fs::create_dir_all(&hooks_dir).unwrap();
    let file = hooks_dir.join("post-commit");
    fs::write(&file, "#!/bin/sh\necho \"keep me\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let hooks = [codegraph::sync::GitHookName::PostCommit];
    install_git_sync_hook(repo.path(), &hooks);

    remove_git_sync_hook(repo.path(), &hooks);

    assert!(file.exists());
    let body = fs::read_to_string(&file).unwrap();
    assert!(body.contains("echo \"keep me\""));
    assert!(!body.contains("codegraph sync"));
}

#[test]
fn honors_core_hooks_path() {
    let repo = TempDir::new().unwrap();
    git_init(repo.path());

    let custom_hooks = repo.path().join(".husky");
    fs::create_dir(&custom_hooks).unwrap();
    git(repo.path(), &["config", "core.hooksPath", ".husky"]);

    let hooks = [codegraph::sync::GitHookName::PostCommit];
    let result = install_git_sync_hook(repo.path(), &hooks);
    assert_eq!(result.hooks_dir.as_deref(), Some(custom_hooks.as_path()));
    assert!(custom_hooks.join("post-commit").exists());
    // The default .git/hooks dir should NOT have received the hook.
    assert!(
        !repo
            .path()
            .join(".git")
            .join("hooks")
            .join("post-commit")
            .exists()
    );
}

#[test]
fn skips_cleanly_when_not_a_git_repository() {
    let repo = TempDir::new().unwrap();

    assert!(!is_git_repo(repo.path()));
    let result = install_git_sync_hook(repo.path(), &DEFAULT_SYNC_HOOKS);
    assert!(result.installed.is_empty());
    assert!(result.hooks_dir.is_none());
    assert!(
        result
            .skipped
            .as_deref()
            .unwrap()
            .contains("not a git repository")
    );
    assert!(!is_sync_hook_installed(repo.path(), &DEFAULT_SYNC_HOOKS));
}
