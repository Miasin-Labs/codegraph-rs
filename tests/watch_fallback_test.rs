use std::fs;
use std::process::Command;

use codegraph::installer::offer_watch_fallback;

#[test]
fn disabled_watching_with_yes_installs_git_sync_hooks() {
    // Given: a Git project whose live watcher is explicitly disabled.
    let dir = tempfile::tempdir().unwrap();
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["config", "core.hooksPath", ".git/hooks"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success()
    );
    unsafe { std::env::set_var("CODEGRAPH_NO_WATCH", "1") };

    // When: non-interactive initialization accepts the recommended fallback.
    offer_watch_fallback(dir.path(), true);
    unsafe { std::env::remove_var("CODEGRAPH_NO_WATCH") };

    // Then: every default Git lifecycle hook contains the CodeGraph sync block.
    for hook in ["post-commit", "post-merge", "post-checkout"] {
        let content = fs::read_to_string(dir.path().join(".git/hooks").join(hook)).unwrap();
        assert!(content.contains("codegraph sync"), "{hook}: {content}");
    }
}
