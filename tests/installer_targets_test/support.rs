pub(crate) use std::ffi::OsString;
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::sync::{Mutex, MutexGuard};
pub(crate) use std::{env, fs};

pub(crate) use codegraph::installer::UninstallStatus;
pub(crate) use codegraph::installer::config_writer::write_mcp_config;
pub(crate) use codegraph::installer::install::uninstall_targets;
pub(crate) use codegraph::installer::targets::claude::cleanup_legacy_hooks;
pub(crate) use codegraph::installer::targets::registry::{
    ALL_TARGETS,
    get_target,
    resolve_target_flag,
};
pub(crate) use codegraph::installer::targets::toml::{
    TomlRemoveAction,
    TomlUpsertAction,
    TomlValue,
    build_toml_table,
    remove_toml_table,
    upsert_toml_table,
};
pub(crate) use codegraph::installer::targets::types::{
    AgentTarget,
    FileAction,
    InstallOptions,
    Location,
};
pub(crate) use serde_json::{Value, json};

static ENV_MUTEX: Mutex<()> = Mutex::new(());

const SAVED_VARS: [&str; 5] = [
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "XDG_CONFIG_HOME",
    "HERMES_HOME",
];

/// RAII guard that redirects HOME + cwd into temp dirs and restores on drop.
pub(crate) struct TestEnv {
    _guard: MutexGuard<'static, ()>,
    home: PathBuf,
    cwd: PathBuf,
    orig_cwd: PathBuf,
    saved: Vec<(&'static str, Option<OsString>)>,
    _home_dir: tempfile::TempDir,
    _cwd_dir: tempfile::TempDir,
}

impl TestEnv {
    pub(crate) fn new() -> Self {
        let guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home_dir = tempfile::Builder::new()
            .prefix("cg-targets-home-")
            .tempdir()
            .unwrap();
        let cwd_dir = tempfile::Builder::new()
            .prefix("cg-targets-cwd-")
            .tempdir()
            .unwrap();
        // Canonicalize so paths derived from `env::current_dir()` (which
        // resolves symlinks like macOS's /var → /private/var) compare
        // equal to paths we build from these roots.
        let home = home_dir.path().canonicalize().unwrap();
        let cwd = cwd_dir.path().canonicalize().unwrap();
        let orig_cwd = env::current_dir().unwrap();

        let saved: Vec<(&'static str, Option<OsString>)> =
            SAVED_VARS.iter().map(|&k| (k, env::var_os(k))).collect();

        env::set_var("HOME", &home);
        env::set_var("USERPROFILE", &home);
        env::set_var("APPDATA", home.join(".config"));
        env::set_var("XDG_CONFIG_HOME", home.join(".config"));
        env::remove_var("HERMES_HOME");
        env::set_current_dir(&cwd).unwrap();

        TestEnv {
            _guard: guard,
            home,
            cwd,
            orig_cwd,
            saved,
            _home_dir: home_dir,
            _cwd_dir: cwd_dir,
        }
    }

    pub(crate) fn home(&self) -> &Path {
        &self.home
    }

    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = env::set_current_dir(&self.orig_cwd);
        for (k, v) in &self.saved {
            match v {
                Some(val) => env::set_var(k, val),
                None => env::remove_var(k),
            }
        }
    }
}

pub(crate) fn write(path: &Path, content: &str) {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(path, content).unwrap();
}

pub(crate) fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

pub(crate) fn read_json(path: &Path) -> Value {
    serde_json::from_str(&read(path)).unwrap()
}

pub(crate) fn pretty(value: &Value) -> String {
    format!("{}\n", serde_json::to_string_pretty(value).unwrap())
}

pub(crate) fn auto_allow() -> InstallOptions {
    InstallOptions {
        auto_allow: true,
        prompt_hook: None,
    }
}

pub(crate) fn no_allow() -> InstallOptions {
    InstallOptions {
        auto_allow: false,
        prompt_hook: None,
    }
}

pub(crate) fn prompt_hook(enabled: bool) -> InstallOptions {
    InstallOptions {
        auto_allow: false,
        prompt_hook: Some(enabled),
    }
}

/// A marker-delimited CodeGraph block exactly as a previous installer
/// wrote it. Install must replace this stale long block with the current
/// short guidance while preserving user-owned content around it.
pub(crate) const LEGACY_BLOCK: &str = "<!-- CODEGRAPH_START -->\n## CodeGraph\n\nPrefer `codegraph_search` / `codegraph_callers` over grep.\n<!-- CODEGRAPH_END -->";

pub(crate) fn list_all_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !dir.exists() {
        return out;
    }
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let full = entry.path();
        if entry.file_type().unwrap().is_dir() {
            out.extend(list_all_files(&full));
        } else {
            out.push(full);
        }
    }
    out
}

pub(crate) fn supported_locations(target: &dyn AgentTarget) -> Vec<Location> {
    [Location::Global, Location::Local]
        .into_iter()
        .filter(|l| target.supports_location(*l))
        .collect()
}
