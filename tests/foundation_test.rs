//! Foundation Tests
//!
//! Port of `__tests__/foundation.test.ts` — the directory-management layer.
//!
//! The TS suite drives most cases through `CodeGraph.initSync` /
//! `DatabaseConnection` / `QueryBuilder`. Those types are still being ported
//! by the db / public-API waves, so this file covers every case that targets
//! the already-ported `crate::directory` layer directly, simulating
//! "initialized" the way the directory layer defines it (`.codegraph/`
//! dir + `codegraph.db` present). The deferred cases are listed in
//! `rust/notes/ui.md` for the db / public-API waves.

use std::fs;

use codegraph::directory::{
    create_directory,
    get_codegraph_dir,
    is_initialized,
    remove_directory,
    validate_directory,
};

/// `CodeGraph.initSync`-equivalent at the directory layer: create
/// `.codegraph/` and the database file that marks a project initialized.
fn init_project(root: &std::path::Path) {
    create_directory(root).unwrap();
    fs::write(get_codegraph_dir(root).join("codegraph.db"), b"").unwrap();
}

// describe('Initialization')

// "should initialize a new project"
#[test]
fn should_initialize_a_new_project() {
    let tmp = tempfile::tempdir().unwrap();
    init_project(tmp.path());

    assert!(is_initialized(tmp.path()));
    assert!(get_codegraph_dir(tmp.path()).exists());
    assert!(get_codegraph_dir(tmp.path()).join("codegraph.db").exists());
}

// "should create .gitignore in .CodeGraph directory"
#[test]
fn should_create_gitignore_in_codegraph_directory() {
    let tmp = tempfile::tempdir().unwrap();
    init_project(tmp.path());

    let gitignore_path = get_codegraph_dir(tmp.path()).join(".gitignore");
    assert!(gitignore_path.exists());

    let content = fs::read_to_string(&gitignore_path).unwrap();
    // Ignore everything in .codegraph/ except this file itself, so transient
    // files (db, daemon.pid, sockets, logs) never show up in git. (#492, #484)
    assert!(content.contains('*'));
    assert!(content.contains("!.gitignore"));
}

// "should throw if already initialized"
#[test]
fn should_throw_if_already_initialized() {
    let tmp = tempfile::tempdir().unwrap();
    init_project(tmp.path());

    let err = create_directory(tmp.path()).unwrap_err();
    assert!(
        err.to_string()
            .to_lowercase()
            .contains("already initialized"),
        "expected /already initialized/i, got: {err}"
    );
}

// describe('Static Methods')

// "isInitialized should return false for new directory"
#[test]
fn is_initialized_returns_false_for_new_directory() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(!is_initialized(tmp.path()));
}

// "isInitialized should return true after init"
#[test]
fn is_initialized_returns_true_after_init() {
    let tmp = tempfile::tempdir().unwrap();
    init_project(tmp.path());
    assert!(is_initialized(tmp.path()));
}

// describe('Directory Management')

// "should validate directory structure"
#[test]
fn should_validate_directory_structure() {
    let tmp = tempfile::tempdir().unwrap();
    init_project(tmp.path());

    let validation = validate_directory(tmp.path());
    assert!(validation.valid);
    assert!(validation.errors.is_empty());
}

// "should detect invalid directory"
#[test]
fn should_detect_invalid_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let validation = validate_directory(tmp.path());
    assert!(!validation.valid);
    assert!(!validation.errors.is_empty());
}

// describe('Uninitialize')

// "should remove .CodeGraph directory"
#[test]
fn uninitialize_removes_codegraph_directory() {
    let tmp = tempfile::tempdir().unwrap();
    init_project(tmp.path());

    remove_directory(tmp.path()).unwrap();

    assert!(!get_codegraph_dir(tmp.path()).exists());
    assert!(!is_initialized(tmp.path()));
}
