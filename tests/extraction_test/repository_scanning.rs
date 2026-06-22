use crate::extraction_test::fixture::*;

// =============================================================================
// describe('Git Submodules')
// =============================================================================

#[test]
fn git_submodules_indexes_files_inside_git_submodules_issue_147() {
    let temp_dir = tempfile::tempdir().unwrap();

    // Build a separate "library" repo to use as a submodule source.
    let lib_dir = temp_dir.path().join("_lib");
    fs::create_dir_all(&lib_dir).unwrap();
    git(&lib_dir, &["init", "-q"]);
    git(&lib_dir, &["config", "user.email", "test@test.com"]);
    git(&lib_dir, &["config", "user.name", "Test"]);
    fs::write(lib_dir.join("lib.ts"), "export const fromSubmodule = 1;").unwrap();
    git(&lib_dir, &["add", "-A"]);
    git(&lib_dir, &["commit", "-q", "-m", "lib init"]);

    // Build the main repo and add the lib repo as a submodule.
    let main_dir = temp_dir.path().join("main");
    fs::create_dir_all(&main_dir).unwrap();
    git(&main_dir, &["init", "-q"]);
    git(&main_dir, &["config", "user.email", "test@test.com"]);
    git(&main_dir, &["config", "user.name", "Test"]);
    fs::write(main_dir.join("app.ts"), "export const app = 1;").unwrap();
    git(&main_dir, &["add", "-A"]);
    git(&main_dir, &["commit", "-q", "-m", "app init"]);
    // protocol.file.allow=always is required to add a local-path submodule on
    // recent git versions (CVE-2022-39253 mitigation).
    git(
        &main_dir,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            lib_dir.to_str().unwrap(),
            "libs/lib",
        ],
    );
    git(&main_dir, &["commit", "-q", "-m", "add submodule"]);

    let files = scan_directory(&main_dir, None);

    assert!(files.contains(&"app.ts".to_string()));
    assert!(files.contains(&"libs/lib/lib.ts".to_string()));
}

// =============================================================================
// describe('Nested non-submodule git repos')
// =============================================================================

#[test]
fn nested_repos_indexes_files_in_embedded_git_repos_run_from_a_git_super_repo_issue_193() {
    let temp_dir = tempfile::tempdir().unwrap();

    // Top-level workspace is itself a git repo, holding no source directly —
    // the CMake "super-repo" layout from the issue.
    let root = temp_dir.path().join("root");
    fs::create_dir_all(root.join("coding")).unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.email", "test@test.com"]);
    git(&root, &["config", "user.name", "Test"]);
    fs::write(
        root.join("CMakeLists.txt"),
        "cmake_minimum_required(VERSION 3.10)\n",
    )
    .unwrap();

    // Two independent clones living inside the workspace (NOT submodules):
    // one with committed source, one with only untracked source.
    let sub1 = root.join("sub_repo1").join("src");
    fs::create_dir_all(&sub1).unwrap();
    git(&root.join("sub_repo1"), &["init", "-q"]);
    git(
        &root.join("sub_repo1"),
        &["config", "user.email", "test@test.com"],
    );
    git(&root.join("sub_repo1"), &["config", "user.name", "Test"]);
    fs::write(sub1.join("one.ts"), "export const one = 1;").unwrap();
    git(&root.join("sub_repo1"), &["add", "-A"]);
    git(
        &root.join("sub_repo1"),
        &["commit", "-q", "-m", "sub1 init"],
    );

    let sub2 = root.join("sub_repo2").join("src");
    fs::create_dir_all(&sub2).unwrap();
    git(&root.join("sub_repo2"), &["init", "-q"]);
    fs::write(sub2.join("two.ts"), "export const two = 2;").unwrap();

    let files = scan_directory(&root, None);

    // Both committed and untracked source from the nested repos must be found.
    assert!(files.contains(&"sub_repo1/src/one.ts".to_string()));
    assert!(files.contains(&"sub_repo2/src/two.ts".to_string()));
}

#[test]
fn nested_repos_respects_each_embedded_repos_own_gitignore() {
    let temp_dir = tempfile::tempdir().unwrap();

    let root = temp_dir.path().join("root");
    fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q"]);

    let sub = root.join("sub_repo").join("src");
    fs::create_dir_all(&sub).unwrap();
    git(&root.join("sub_repo"), &["init", "-q"]);
    fs::write(
        root.join("sub_repo").join(".gitignore"),
        "src/generated.ts\n",
    )
    .unwrap();
    fs::write(sub.join("real.ts"), "export const real = 1;").unwrap();
    fs::write(sub.join("generated.ts"), "export const generated = 1;").unwrap();

    let files = scan_directory(&root, None);

    assert!(files.contains(&"sub_repo/src/real.ts".to_string()));
    assert!(!files.contains(&"sub_repo/src/generated.ts".to_string()));
}

// =============================================================================
