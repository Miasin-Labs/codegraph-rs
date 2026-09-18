//! Which crates of the project a path's first segment can name.
//!
//! `use tree_sitter::Node` and `use codegraph_analysis::graph::CodeGraph`
//! look alike; only the manifests say that the second names a crate of the
//! project. Workspace members come from `[workspace] members`, the root
//! package from its `[package]` and `[lib]` names.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use crate::resolution::frameworks::cargo_workspace::{
    get_cargo_workspace_crate_map,
    get_root_package_crate_names,
};
use crate::resolution::types::ResolutionContext;

type CrateDirs = Arc<HashMap<String, String>>;

/// Crate source directories, per project root and root-manifest contents.
static CRATE_DIRS: LazyLock<Mutex<HashMap<(String, u64), CrateDirs>>> =
    LazyLock::new(Default::default);

/// The source directory (`analysis/src/`) of the project crate `name`, or
/// `None` when `name` is not a crate of the project.
pub(super) fn project_crate_dir(name: &str, context: &dyn ResolutionContext) -> Option<String> {
    crate_dirs(context).get(name).cloned()
}

fn crate_dirs(context: &dyn ResolutionContext) -> CrateDirs {
    let manifest = context.read_file("Cargo.toml").unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    manifest.hash(&mut hasher);
    let key = (context.get_project_root().to_string(), hasher.finish());
    if let Some(dirs) = CRATE_DIRS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
    {
        return Arc::clone(dirs);
    }
    let dirs = Arc::new(build(&manifest, context));
    CRATE_DIRS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key, Arc::clone(&dirs));
    dirs
}

fn build(manifest: &str, context: &dyn ResolutionContext) -> HashMap<String, String> {
    let mut dirs: HashMap<String, String> = get_cargo_workspace_crate_map(context)
        .into_iter()
        .map(|(name, member)| (name.replace('-', "_"), source_dir(&member)))
        .collect();
    for name in get_root_package_crate_names(manifest) {
        dirs.entry(name).or_insert_with(|| source_dir(""));
    }
    dirs
}

fn source_dir(member: &str) -> String {
    let member = member.trim_start_matches("./").trim_end_matches('/');
    if member.is_empty() || member == "." {
        "src/".to_string()
    } else {
        format!("{member}/src/")
    }
}
