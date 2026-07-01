use crate::extraction_test::fixture::*;

// =============================================================================
// describe('Directory Exclusion')
// =============================================================================

#[test]
fn directory_exclusion_excludes_directories_listed_in_gitignore() {
    let temp_dir = tempfile::tempdir().unwrap();
    // Create structure: src/index.ts + node_modules/pkg/index.js, gitignore node_modules
    let src_dir = temp_dir.path().join("src");
    let nm_dir = temp_dir.path().join("node_modules").join("pkg");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&nm_dir).unwrap();
    fs::write(src_dir.join("index.ts"), "export const x = 1;").unwrap();
    fs::write(nm_dir.join("index.js"), "module.exports = {};").unwrap();
    fs::write(temp_dir.path().join(".gitignore"), "node_modules/\n").unwrap();

    let files = scan_directory(temp_dir.path(), None);

    assert!(files.contains(&"src/index.ts".to_string()));
    assert!(files.iter().all(|f| !f.contains("node_modules")));
}

#[test]
fn directory_exclusion_excludes_nested_node_modules_via_a_root_gitignore() {
    let temp_dir = tempfile::tempdir().unwrap();
    // A trailing-slash pattern with no leading slash matches at any depth.
    let src_dir = temp_dir.path().join("packages").join("app").join("src");
    let nm_dir = temp_dir
        .path()
        .join("packages")
        .join("app")
        .join("node_modules")
        .join("pkg");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&nm_dir).unwrap();
    fs::write(src_dir.join("index.ts"), "export const x = 1;").unwrap();
    fs::write(nm_dir.join("index.js"), "module.exports = {};").unwrap();
    fs::write(temp_dir.path().join(".gitignore"), "node_modules/\n").unwrap();

    let files = scan_directory(temp_dir.path(), None);

    assert!(files.contains(&"packages/app/src/index.ts".to_string()));
    assert!(files.iter().all(|f| !f.contains("node_modules")));
}

#[test]
fn directory_exclusion_excludes_tracked_files_listed_in_codegraphignore() {
    let temp_dir = tempfile::tempdir().unwrap();
    let root = temp_dir.path();

    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("research/decompiled-references/all")).unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
    fs::write(
        root.join("research/decompiled-references/all/generated.c"),
        "int generated(void) { return 1; }",
    )
    .unwrap();
    fs::write(
        root.join(".codegraphignore"),
        "research/decompiled-references/\n",
    )
    .unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "tracked corpus"]);

    let files = scan_directory(root, None);

    assert!(files.contains(&"src/main.rs".to_string()));
    assert!(!files.contains(&"research/decompiled-references/all/generated.c".to_string()));
}

#[test]
fn directory_exclusion_applies_a_nested_gitignore_only_to_its_own_subtree() {
    let temp_dir = tempfile::tempdir().unwrap();
    let app_src = temp_dir.path().join("app").join("src");
    fs::create_dir_all(&app_src).unwrap();
    fs::write(app_src.join("keep.ts"), "export const a = 1;").unwrap();
    fs::write(app_src.join("skip.ts"), "export const b = 2;").unwrap();
    fs::write(
        temp_dir.path().join("app").join(".gitignore"),
        "src/skip.ts\n",
    )
    .unwrap();
    // A sibling with the same name outside app/ must NOT be ignored.
    let other_dir = temp_dir.path().join("other").join("src");
    fs::create_dir_all(&other_dir).unwrap();
    fs::write(other_dir.join("skip.ts"), "export const c = 3;").unwrap();

    let files = scan_directory(temp_dir.path(), None);

    assert!(files.contains(&"app/src/keep.ts".to_string()));
    assert!(!files.contains(&"app/src/skip.ts".to_string()));
    assert!(files.contains(&"other/src/skip.ts".to_string()));
}

#[test]
fn directory_exclusion_nested_gitignore_negation_reincludes_file() {
    let temp_dir = tempfile::tempdir().unwrap();
    let app_dir = temp_dir.path().join("app");
    fs::create_dir_all(&app_dir).unwrap();
    fs::write(temp_dir.path().join(".gitignore"), "*.ts\n").unwrap();
    fs::write(app_dir.join(".gitignore"), "!keep.ts\n").unwrap();
    fs::write(app_dir.join("keep.ts"), "export const keep = 1;").unwrap();
    fs::write(app_dir.join("drop.ts"), "export const drop = 1;").unwrap();

    let files = scan_directory(temp_dir.path(), None);

    assert!(files.contains(&"app/keep.ts".to_string()), "got {files:?}");
    assert!(!files.contains(&"app/drop.ts".to_string()), "got {files:?}");
}

#[test]
fn directory_exclusion_always_skips_git_directories() {
    let temp_dir = tempfile::tempdir().unwrap();
    let src_dir = temp_dir.path().join("src");
    let git_dir = temp_dir.path().join(".git").join("objects");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&git_dir).unwrap();
    fs::write(src_dir.join("index.ts"), "export const x = 1;").unwrap();
    fs::write(git_dir.join("pack.ts"), "export const y = 2;").unwrap();

    let files = scan_directory(temp_dir.path(), None);

    assert!(files.contains(&"src/index.ts".to_string()));
    assert!(files.iter().all(|f| !f.contains(".git")));
}

#[test]
fn directory_exclusion_returns_forward_slash_paths_on_all_platforms() {
    let temp_dir = tempfile::tempdir().unwrap();
    let src_dir = temp_dir.path().join("src").join("components");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(src_dir.join("Button.tsx"), "export function Button() {}").unwrap();

    let files = scan_directory(temp_dir.path(), None);

    assert_eq!(files.len(), 1);
    assert_eq!(files[0], "src/components/Button.tsx");
    assert!(!files[0].contains('\\'));
}

#[cfg(unix)]
#[test]
fn directory_scan_skips_symlinked_source_files_outside_root() {
    let temp_dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp_dir.path().join("src")).unwrap();
    fs::write(outside.path().join("secret.ts"), "export const secret = 1;").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.ts"),
        temp_dir.path().join("src").join("secret.ts"),
    )
    .unwrap();

    let files = scan_directory(temp_dir.path(), None);

    assert!(!files.contains(&"src/secret.ts".to_string()));
}

#[cfg(unix)]
#[test]
fn git_directory_scan_skips_tracked_symlinked_source_files_outside_root() {
    let temp_dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = temp_dir.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);
    fs::write(root.join("real.ts"), "export const real = 1;").unwrap();
    fs::write(outside.path().join("secret.ts"), "export const secret = 1;").unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret.ts"), root.join("secret.ts")).unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "tracked symlink"]);

    let files = scan_directory(root, None);

    assert!(files.contains(&"real.ts".to_string()));
    assert!(!files.contains(&"secret.ts".to_string()));
}

#[cfg(unix)]
#[test]
fn index_file_blocks_symlinked_source_files_outside_root() {
    let temp_dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(
        outside.path().join("secret.ts"),
        "export function leakedSecret() { return 1; }",
    )
    .unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.ts"),
        temp_dir.path().join("link.ts"),
    )
    .unwrap();

    let (_conn, queries) = open_graph(temp_dir.path());
    let orch = ExtractionOrchestrator::new(temp_dir.path(), &queries);
    let result = orch.index_file("link.ts").unwrap();

    assert!(result.errors.iter().any(|e| {
        e.code.as_deref() == Some("path_traversal") && e.message.contains("Path traversal blocked")
    }));
    assert!(queries.get_file_by_path("link.ts").unwrap().is_none());
}

#[test]
fn index_file_reports_read_error_for_missing_files_inside_root() {
    let temp_dir = tempfile::tempdir().unwrap();
    let (_conn, queries) = open_graph(temp_dir.path());
    let orch = ExtractionOrchestrator::new(temp_dir.path(), &queries);
    let result = orch.index_file("missing.ts").unwrap();

    assert!(result.errors.iter().any(|e| {
        e.code.as_deref() == Some("read_error") && e.message.contains("Failed to read file")
    }));
    assert!(queries.get_file_by_path("missing.ts").unwrap().is_none());
}
