//! Running `rust-analyzer scip` and caching its index.
//!
//! The run is bounded by a deadline (`CODEGRAPH_COMPILER_BUDGET_MS`): the
//! whole process group is killed when it passes. Its output goes to
//! `.codegraph/compiler/` — `index.scip`, `state.json` (the fingerprint it
//! was made for and each file's content hash at run time) and
//! `rust-analyzer.log` — replaced only when a run succeeds, so a failed or
//! timed-out run leaves the previous index in place.
//!
//! The fingerprint covers what rust-analyzer's answer depends on: the
//! project's Rust sources and manifests (`Cargo.toml`s, `Cargo.lock`) and
//! the rust-analyzer version. A file's own entry in `state.json` lets a
//! document stay usable after *other* files changed: the layer applies a
//! document only while its file's hash still matches.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::utils::sha256_hex;

/// Bump when what a cached index means changes (a new run is due).
pub const COMPILER_STATE_VERSION: u32 = 1;
/// Default budget of one rust-analyzer run.
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(600);

/// `.codegraph/compiler/` of a project.
pub fn compiler_dir(project_root: &Path) -> PathBuf {
    crate::directory::get_codegraph_dir(project_root).join("compiler")
}

pub(crate) fn index_path(project_root: &Path) -> PathBuf {
    compiler_dir(project_root).join("index.scip")
}

fn state_path(project_root: &Path) -> PathBuf {
    compiler_dir(project_root).join("state.json")
}

pub(crate) fn log_path(project_root: &Path) -> PathBuf {
    compiler_dir(project_root).join("rust-analyzer.log")
}

pub(crate) fn lock_path(project_root: &Path) -> PathBuf {
    compiler_dir(project_root).join("run.lock")
}

/// rust-analyzer settings for the run (`--config-path`): the file
/// `CODEGRAPH_COMPILER_CONFIG` names, else `.codegraph/compiler/
/// rust-analyzer.json` when present — e.g. `{"cargo": {"features":
/// "all"}}` for a crate whose code sits behind features.
pub fn config_path(project_root: &Path) -> Option<PathBuf> {
    std::env::var_os("CODEGRAPH_COMPILER_CONFIG")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| Some(compiler_dir(project_root).join("rust-analyzer.json")))
        .filter(|path| path.is_file())
}

/// What the cached index was made from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompilerState {
    pub version: u32,
    pub fingerprint: String,
    pub tool_version: String,
    pub created_at_ms: i64,
    pub duration_ms: u64,
    /// Content hash (as the index records it) of every Rust file that did
    /// not change while rust-analyzer ran.
    pub files: BTreeMap<String, String>,
}

impl CompilerState {
    pub fn read(project_root: &Path) -> Option<CompilerState> {
        let text = std::fs::read_to_string(state_path(project_root)).ok()?;
        serde_json::from_str::<CompilerState>(&text)
            .ok()
            .filter(|state| state.version == COMPILER_STATE_VERSION)
    }

    fn write(&self, project_root: &Path) -> std::io::Result<()> {
        let path = state_path(project_root);
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(temp, path)
    }
}

/// The inputs of a run, hashed.
pub(crate) struct Inputs {
    pub(crate) fingerprint: String,
    pub(crate) files: BTreeMap<String, String>,
}

/// Hash the project's Rust files (`rust_files`, relative paths from the
/// index) and manifests, plus the tool version.
pub(crate) fn hash_inputs(
    project_root: &Path,
    rust_files: &[String],
    tool_version: &str,
) -> Inputs {
    let files: BTreeMap<String, String> = rust_files
        .iter()
        .filter_map(|path| Some((path.clone(), hash_file(&project_root.join(path))?)))
        .collect();
    let mut manifests: Vec<String> = vec!["Cargo.toml".into(), "Cargo.lock".into()];
    for path in rust_files {
        // Each crate's manifest sits above its `src/`.
        if let Some((crate_dir, _)) = path.split_once("/src/") {
            manifests.push(format!("{crate_dir}/Cargo.toml"));
        }
    }
    manifests.sort();
    manifests.dedup();
    let mut summary = format!("v{COMPILER_STATE_VERSION}\n{tool_version}\n");
    for (path, hash) in &files {
        summary.push_str(&format!("{path}\0{hash}\n"));
    }
    for manifest in &manifests {
        if let Some(hash) = hash_file(&project_root.join(manifest)) {
            summary.push_str(&format!("{manifest}\0{hash}\n"));
        }
    }
    if let Some(hash) = config_path(project_root).and_then(|path| hash_file(&path)) {
        summary.push_str(&format!("config\0{hash}\n"));
    }
    Inputs {
        fingerprint: sha256_hex(summary.as_bytes()),
        files,
    }
}

/// A file's content hash as the index computes it (lossy UTF-8, SHA-256).
pub(crate) fn hash_file(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(sha256_hex(String::from_utf8_lossy(&bytes).as_bytes()))
}

/// The program looked up on PATH when none is configured.
const DEFAULT_RUST_ANALYZER: &str = "rust-analyzer";

/// The rust-analyzer executable: `CODEGRAPH_RUST_ANALYZER`, else on PATH.
pub fn rust_analyzer_program() -> PathBuf {
    std::env::var_os("CODEGRAPH_RUST_ANALYZER")
        .filter(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from(DEFAULT_RUST_ANALYZER), PathBuf::from)
}

/// A rust-analyzer that runs for `project_root`, and its version: the
/// configured program (`CODEGRAPH_RUST_ANALYZER`, else `rust-analyzer` on
/// PATH) asked *in the project* — PATH's is usually rustup's proxy, which
/// picks the toolchain a `rust-toolchain.toml` pins, and a pinned toolchain
/// often lacks the component ("not installed for toolchain 1.98.1"). Then,
/// for the default lookup only, the newest `rust-analyzer` binary of
/// any installed rustup toolchain: rust-analyzer loads the project through
/// the project's own cargo and sysroot, so another toolchain's reads it.
pub fn usable_rust_analyzer(configured: &Path, project_root: &Path) -> Option<(PathBuf, String)> {
    if let Some(version) = rust_analyzer_version(configured, project_root) {
        return Some((configured.to_path_buf(), version));
    }
    // Only the default lookup falls back: a program someone chose
    // (`CODEGRAPH_RUST_ANALYZER`, `CompilerOptions`) is the one to run.
    if configured != Path::new(DEFAULT_RUST_ANALYZER) {
        return None;
    }
    let rustup_home = std::env::var_os("RUSTUP_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".rustup")))?;
    toolchain_rust_analyzers(&rustup_home)
        .into_iter()
        .find_map(|program| rust_analyzer_version(&program, project_root).map(|v| (program, v)))
}

/// `<rustup_home>/toolchains/*/bin/rust-analyzer`, newest binary first.
fn toolchain_rust_analyzers(rustup_home: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(rustup_home.join("toolchains")) else {
        return Vec::new();
    };
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().join("bin").join("rust-analyzer"))
        .filter_map(|program| {
            let modified = std::fs::metadata(&program).ok()?.modified().ok()?;
            Some((modified, program))
        })
        .collect();
    found.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    found.into_iter().map(|(_, program)| program).collect()
}

/// `<program> --version` run in `dir`, or `None` when it cannot run.
pub fn rust_analyzer_version(program: &Path, dir: &Path) -> Option<String> {
    let output = Command::new(program)
        .current_dir(dir)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!version.is_empty()).then_some(version)
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "status", content = "detail")]
pub enum RunStatus {
    /// A fresh index was written.
    Ran,
    /// The cached index was made for these very inputs.
    Cached,
    /// `rust-analyzer` is not installed (or does not run).
    Missing,
    /// The budget ran out; the process group was killed.
    TimedOut,
    /// It exited unsuccessfully (the log's last lines).
    Failed(String),
    /// Another run holds the project's compiler lock.
    Busy,
}

impl RunStatus {
    /// An index for the current inputs is on disk.
    pub fn has_index(&self) -> bool {
        matches!(self, RunStatus::Ran | RunStatus::Cached)
    }
}

/// Produce (or reuse) the SCIP index for `inputs`.
pub(crate) fn ensure_index(
    program: &Path,
    project_root: &Path,
    inputs: &Inputs,
    tool_version: &str,
    rust_files: &[String],
    budget: Duration,
    force: bool,
) -> RunStatus {
    let dir = compiler_dir(project_root);
    if std::fs::create_dir_all(&dir).is_err() {
        return RunStatus::Failed(format!("cannot create {}", dir.display()));
    }
    if !force
        && index_path(project_root).is_file()
        && CompilerState::read(project_root)
            .is_some_and(|state| state.fingerprint == inputs.fingerprint)
    {
        return RunStatus::Cached;
    }
    let Ok(Some(_lock)) = crate::deps::store::StoreLock::try_acquire(&lock_path(project_root))
    else {
        return RunStatus::Busy;
    };
    let started = Instant::now();
    let output = dir.join("index.scip.tmp");
    let _ = std::fs::remove_file(&output);
    let status = run_bounded(program, project_root, &output, budget);
    if status != RunStatus::Ran {
        let _ = std::fs::remove_file(&output);
        return status;
    }
    if std::fs::rename(&output, index_path(project_root)).is_err() {
        return RunStatus::Failed("cannot move the index into place".into());
    }
    // Files edited while rust-analyzer ran are not described by its index.
    let after = hash_inputs(project_root, rust_files, tool_version);
    let files = inputs
        .files
        .iter()
        .filter(|(path, hash)| after.files.get(*path) == Some(*hash))
        .map(|(path, hash)| (path.clone(), hash.clone()))
        .collect();
    // The run may write manifests itself (`cargo metadata` creates a
    // missing `Cargo.lock`): with the sources unchanged, the index is for
    // the inputs as they are now. With a source edited meanwhile it is not,
    // and the next sync runs again.
    let fingerprint = if after.files == inputs.files {
        after.fingerprint
    } else {
        inputs.fingerprint.clone()
    };
    let state = CompilerState {
        version: COMPILER_STATE_VERSION,
        fingerprint,
        tool_version: tool_version.to_string(),
        created_at_ms: crate::db::connection::now_ms(),
        duration_ms: started.elapsed().as_millis() as u64,
        files,
    };
    match state.write(project_root) {
        Ok(()) => RunStatus::Ran,
        Err(error) => RunStatus::Failed(format!("cannot write the compiler state: {error}")),
    }
}

/// `rust-analyzer scip <root> --output <output>`, killed at `budget`.
fn run_bounded(program: &Path, project_root: &Path, output: &Path, budget: Duration) -> RunStatus {
    let log = log_path(project_root);
    let Ok(file) = std::fs::File::create(&log) else {
        return RunStatus::Failed(format!("cannot create {}", log.display()));
    };
    let Ok(err_file) = file.try_clone() else {
        return RunStatus::Failed(format!("cannot open {}", log.display()));
    };
    let mut command = Command::new(program);
    command
        .arg("scip")
        .arg(project_root)
        .arg("--output")
        .arg(output);
    if let Some(config) = config_path(project_root) {
        command.arg("--config-path").arg(config);
    }
    command
        .current_dir(project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(err_file));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return RunStatus::Missing,
        Err(error) => return RunStatus::Failed(format!("cannot run rust-analyzer: {error}")),
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() && output.is_file() => return RunStatus::Ran,
            Ok(Some(_)) => return RunStatus::Failed(log_tail(&log)),
            Ok(None) if started.elapsed() >= budget => {
                kill_group(child.id());
                let _ = child.kill();
                let _ = child.wait();
                return RunStatus::TimedOut;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(error) => return RunStatus::Failed(format!("waiting for rust-analyzer: {error}")),
        }
    }
}

/// The last few lines of the run's log.
fn log_tail(log: &Path) -> String {
    let mut text = String::new();
    let _ = std::fs::File::open(log).and_then(|mut file| file.read_to_string(&mut text));
    let lines: Vec<&str> = text.lines().rev().take(6).collect();
    let tail: Vec<&str> = lines.into_iter().rev().collect();
    let tail = tail.join("\n");
    if tail.is_empty() {
        "rust-analyzer exited unsuccessfully".to_string()
    } else {
        tail
    }
}

#[cfg(unix)]
fn kill_group(pid: u32) {
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(unix))]
fn kill_group(_pid: u32) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_follows_sources_manifests_and_tool() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
        let files = vec!["src/lib.rs".to_string()];
        let first = hash_inputs(root, &files, "ra 1");
        assert_eq!(
            first.fingerprint,
            hash_inputs(root, &files, "ra 1").fingerprint
        );
        assert_ne!(
            first.fingerprint,
            hash_inputs(root, &files, "ra 2").fingerprint
        );
        std::fs::write(root.join("Cargo.lock"), "# lock\n").unwrap();
        let locked = hash_inputs(root, &files, "ra 1");
        assert_ne!(first.fingerprint, locked.fingerprint);
        std::fs::write(root.join("src/lib.rs"), "pub fn b() {}\n").unwrap();
        let edited = hash_inputs(root, &files, "ra 1");
        assert_ne!(locked.fingerprint, edited.fingerprint);
        assert_ne!(locked.files["src/lib.rs"], edited.files["src/lib.rs"]);
    }
}

#[cfg(test)]
mod rust_analyzer_lookup_tests {
    use super::*;

    /// Toolchains that hold a rust-analyzer binary are listed newest first;
    /// ones without the component are skipped.
    #[test]
    fn toolchain_binaries_are_found_newest_first() {
        let home = tempfile::tempdir().unwrap();
        let make = |name: &str, age_secs: u64| {
            let bin = home.path().join("toolchains").join(name).join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            let program = bin.join("rust-analyzer");
            std::fs::write(&program, "").unwrap();
            let when = std::time::SystemTime::now() - std::time::Duration::from_secs(age_secs);
            std::fs::File::options()
                .write(true)
                .open(&program)
                .unwrap()
                .set_modified(when)
                .unwrap();
            program
        };
        let old = make("1.85.0-x86_64-unknown-linux-gnu", 3_000);
        let new = make("rustc-master", 10);
        std::fs::create_dir_all(
            home.path()
                .join("toolchains/1.98.1-x86_64-unknown-linux-gnu/bin"),
        )
        .unwrap();
        assert_eq!(toolchain_rust_analyzers(home.path()), vec![new, old]);
        assert!(toolchain_rust_analyzers(&home.path().join("missing")).is_empty());
    }
}
