//! JS packages live in the project, not a shared cache: at the install path
//! the lockfile names, in pnpm's `node_modules/.pnpm/<name>@<version>/…`
//! store, or hoisted to `node_modules/<name>`. A directory counts only when
//! its package.json has the locked version — a stale `node_modules` is not
//! the locked code.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::deps::model::ResolvedDep;

pub(super) fn locate(dep: &ResolvedDep, project_root: &Path) -> Option<PathBuf> {
    let name = &dep.key.name;
    let version = &dep.lock_version;
    if let Some(install) = &dep.install_path {
        let dir = project_root.join(install);
        return installed_version_matches(&dir, name, version).then_some(dir);
    }
    let lock_dir = dep
        .lockfile
        .rsplit_once('/')
        .map_or(project_root.to_path_buf(), |(dir, _)| {
            project_root.join(dir)
        });
    let node_modules = lock_dir.join("node_modules");
    pnpm_store(&node_modules, name, version).or_else(|| {
        let hoisted = node_modules.join(name);
        installed_version_matches(&hoisted, name, version).then_some(hoisted)
    })
}

/// `node_modules/.pnpm/<name with / as +>@<version>[_peers|(peers)]/node_modules/<name>`.
fn pnpm_store(node_modules: &Path, name: &str, version: &str) -> Option<PathBuf> {
    let store = node_modules.join(".pnpm");
    let prefix = format!("{}@{version}", name.replace('/', "+"));
    let exact = store.join(&prefix).join("node_modules").join(name);
    if installed_version_matches(&exact, name, version) {
        return Some(exact);
    }
    let mut candidates: Vec<PathBuf> = fs::read_dir(&store)
        .ok()?
        .flatten()
        .filter(|e| {
            e.file_name().to_str().is_some_and(|n| {
                n.strip_prefix(&prefix)
                    .is_some_and(|rest| rest.starts_with('_') || rest.starts_with('('))
            })
        })
        .map(|e| e.path().join("node_modules").join(name))
        .collect();
    candidates.sort();
    candidates
        .into_iter()
        .find(|dir| installed_version_matches(dir, name, version))
}

fn installed_version_matches(dir: &Path, name: &str, version: &str) -> bool {
    let Some(manifest) = fs::read_to_string(dir.join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    else {
        return false;
    };
    let field = |k: &str| manifest.get(k).and_then(Value::as_str);
    field("version") == Some(version) && field("name").is_none_or(|n| n == name)
}
