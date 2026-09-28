//! Where a shard's rustdoc JSON comes from.
//!
//! * **The toolchain** ships it: rustup's `rust-docs-json` component puts
//!   `std.json`, `core.json`, `alloc.json` … in `<sysroot>/share/doc/rust/json/`
//!   of the same toolchain directory as the `rust-src` sources the shard is
//!   built from. It is used only when its `crate_version` names the shard's
//!   release and commit (a JSON of another nightly describes other code).
//! * **A dependency** needs `cargo rustdoc --output-format json`, which
//!   compiles the crate's dependencies: opt-in (`CODEGRAPH_DEPS_RUSTDOC=1`),
//!   only from the CLI's shard builds, offline, bounded by a deadline (the
//!   whole process group is killed), in a throwaway probe package that
//!   depends on the crate by path — the crate's source tree is only read.
//!   A crate that does not build keeps today's tree-sitter shard.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::analyze::fuzz::run::run_bounded_capped;

/// The toolchain crates whose JSON the toolchain shard indexes.
pub const TOOLCHAIN_CRATES: &[&str] = &["std", "core", "alloc"];

/// The cargo target directory (under the deps home) every crate's
/// `cargo rustdoc` shares — a cache `deps gc` removes.
pub const RUSTDOC_TARGET_DIR: &str = "rustdoc-target";
/// Default deadline of one crate's `cargo rustdoc`.
const DEFAULT_RUSTDOC_MS: u64 = 180_000;
/// Most output a `cargo rustdoc` run may log.
const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;

/// The toolchain's rustdoc JSON directory for its `library` source
/// directory (`<sysroot>/lib/rustlib/src/rust/library`), or
/// `CODEGRAPH_RUSTDOC_JSON_DIR`.
pub fn toolchain_json_dir(library: &Path) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CODEGRAPH_RUSTDOC_JSON_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    let sysroot = library.ancestors().nth(5)?;
    let dir = sysroot.join("share/doc/rust/json");
    dir.is_dir().then_some(dir)
}

/// Where to look for the toolchain's JSON, in order: its own sysroot (or
/// `CODEGRAPH_RUSTDOC_JSON_DIR`, which is then the only place), then every
/// other rustup toolchain's `rust-docs-json` directory. The same compiler
/// build can be installed twice — a `rustup-toolchain-install-master`
/// toolchain without the component, and another of the same commit with it
/// — and [`toolchain_json`] only accepts files of the shard's exact release
/// and commit, so a sibling's JSON is used only when it describes this code.
fn toolchain_json_dirs(library: &Path) -> Vec<PathBuf> {
    if std::env::var_os("CODEGRAPH_RUSTDOC_JSON_DIR").is_some_and(|v| !v.is_empty()) {
        return toolchain_json_dir(library).into_iter().collect();
    }
    let rustup_home = std::env::var_os("RUSTUP_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".rustup")));
    json_dirs_with(toolchain_json_dir(library), rustup_home.as_deref())
}

/// `own` first, then every `<rustup_home>/toolchains/*/share/doc/rust/json`
/// (sorted, without repeating `own`).
fn json_dirs_with(own: Option<PathBuf>, rustup_home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = own.into_iter().collect();
    if let Some(Ok(entries)) = rustup_home.map(|home| std::fs::read_dir(home.join("toolchains"))) {
        let mut others: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path().join("share/doc/rust/json"))
            .filter(|dir| dir.is_dir() && !dirs.contains(dir))
            .collect();
        others.sort();
        dirs.extend(others);
    }
    dirs
}

/// The toolchain JSON files (`std`, `core`, `alloc`) for the toolchain whose
/// library is `library` and whose shard version is `version`
/// (`1.100.0-nightly+1303417c416e`): from the first place
/// ([`toolchain_json_dirs`]) that has all three, of that release and commit;
/// else why none did.
pub fn toolchain_json(library: &Path, version: &str) -> Result<Vec<(String, PathBuf)>, String> {
    first_matching(&toolchain_json_dirs(library), version)
}

/// The JSON of the first of `dirs` that describes `version`.
fn first_matching(dirs: &[PathBuf], version: &str) -> Result<Vec<(String, PathBuf)>, String> {
    if dirs.is_empty() {
        return Err("no rust-docs-json component".to_string());
    }
    let mut reasons = Vec::new();
    for dir in dirs {
        match json_in(dir, version) {
            Ok(files) => return Ok(files),
            Err(reason) => reasons.push(reason),
        }
    }
    Err(reasons.join("; "))
}

/// The three toolchain JSON files in `dir`, when all describe `version`.
fn json_in(dir: &Path, version: &str) -> Result<Vec<(String, PathBuf)>, String> {
    let mut files = Vec::new();
    for krate in TOOLCHAIN_CRATES {
        let path = dir.join(format!("{krate}.json"));
        if !path.is_file() {
            return Err(format!("{} missing", path.display()));
        }
        let described =
            crate_version(&path).ok_or_else(|| format!("{}: no crate_version", path.display()))?;
        if !same_toolchain(&described, version) {
            return Err(format!(
                "{} describes {described:?}, the shard is {version}",
                path.display()
            ));
        }
        files.push(((*krate).to_string(), path));
    }
    Ok(files)
}

/// The `crate_version` rustdoc writes near the start of the file.
fn crate_version(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut head = vec![0u8; 1024];
    let read = std::fs::File::open(path).ok()?.read(&mut head).ok()?;
    let text = String::from_utf8_lossy(&head[..read]);
    let at = text.find("\"crate_version\"")?;
    let rest = text[at + "\"crate_version\"".len()..]
        .trim_start()
        .strip_prefix(':')?;
    let rest = rest.trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].replace("\\t", "\t"))
}

/// rustdoc's `1.100.0-nightly\t(1303417c4\t2026-09-21)` against a shard
/// version `1.100.0-nightly+1303417c416e`: the same release, and commits
/// that agree as far as both are written.
pub fn same_toolchain(described: &str, version: &str) -> bool {
    let mut parts = described.split(['\t', ' ']);
    let release = parts.next().unwrap_or_default();
    let commit = parts
        .find(|part| !part.is_empty())
        .map(|part| part.trim_start_matches('(').trim_end_matches(')'))
        .unwrap_or_default();
    let (shard_release, shard_commit) = version.split_once('+').unwrap_or((version, ""));
    if release != shard_release {
        return false;
    }
    if commit.is_empty() || shard_commit.is_empty() {
        // A stable release names one toolchain; a nightly or beta release
        // only with its commit.
        return !release.contains('-');
    }
    let len = commit.len().min(shard_commit.len());
    len >= 7 && commit[..len] == shard_commit[..len]
}

/// `CODEGRAPH_DEPS_RUSTDOC=1` turns rustdoc JSON on for dependency shards.
pub fn crate_rustdoc_enabled() -> bool {
    std::env::var("CODEGRAPH_DEPS_RUSTDOC").is_ok_and(|v| {
        let v = v.trim();
        v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on")
    })
}

/// `CODEGRAPH_DEPS_RUSTDOC_JSON_DIR`: rustdoc JSON already made for the
/// dependency crates (`<dir>/<lib name>.json`), read instead of running
/// cargo — docs built elsewhere, and tests.
pub fn prebuilt_json_dir() -> Option<PathBuf> {
    std::env::var_os("CODEGRAPH_DEPS_RUSTDOC_JSON_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// One dependency crate to document.
#[derive(Debug, Clone)]
pub struct CrateRequest<'a> {
    /// The package name (`serde_json`, `tokio-util`).
    pub package: &'a str,
    pub version: &'a str,
    /// Its library's name (the JSON file's name).
    pub lib_name: &'a str,
    /// Its source (read only).
    pub source_dir: &'a Path,
    /// Where the probe package and its output go (created, and removed by
    /// the caller).
    pub work_dir: &'a Path,
    /// A cargo target directory reused across crates (its compiled
    /// dependencies are what makes the next crate fast).
    pub target_dir: &'a Path,
    pub deadline: Duration,
}

/// The deadline of one crate's `cargo rustdoc` (`CODEGRAPH_DEPS_RUSTDOC_MS`).
pub fn crate_rustdoc_deadline() -> Duration {
    Duration::from_millis(
        std::env::var("CODEGRAPH_DEPS_RUSTDOC_MS")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(DEFAULT_RUSTDOC_MS),
    )
}

/// Run `cargo +<toolchain> rustdoc … --output-format json` for the crate:
/// the JSON file it wrote, or why there is none.
pub fn crate_json(request: &CrateRequest<'_>) -> Result<PathBuf, String> {
    let probe = request.work_dir.join("probe");
    std::fs::create_dir_all(probe.join("src")).map_err(|e| format!("probe: {e}"))?;
    let manifest = format!(
        "[package]\nname = \"codegraph-rustdoc-probe\"\nversion = \"0.0.0\"\nedition = \"2021\"\npublish = false\n\n[lib]\npath = \"src/lib.rs\"\n\n[dependencies]\n{} = {{ package = {:?}, path = {:?} }}\n\n[workspace]\n",
        toml_key(request.package),
        request.package,
        request.source_dir.to_string_lossy()
    );
    std::fs::write(probe.join("Cargo.toml"), manifest).map_err(|e| format!("probe: {e}"))?;
    std::fs::write(probe.join("src/lib.rs"), "").map_err(|e| format!("probe: {e}"))?;
    let toolchain =
        std::env::var("CODEGRAPH_RUSTDOC_TOOLCHAIN").unwrap_or_else(|_| "nightly".into());
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    if !toolchain.is_empty() {
        command.arg(format!("+{toolchain}"));
    }
    command
        .args(["rustdoc", "--offline", "--lib", "-p"])
        .arg(format!("{}@{}", request.package, request.version))
        .arg("--manifest-path")
        .arg(probe.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(request.target_dir)
        .args(["--", "-Z", "unstable-options", "--output-format", "json"])
        .current_dir(&probe)
        .env("CARGO_TERM_COLOR", "never")
        .env("CARGO_NET_OFFLINE", "true")
        .env_remove("RUSTDOCFLAGS")
        .env_remove("RUSTUP_TOOLCHAIN");
    let log = request.work_dir.join("rustdoc.log");
    let finished = run_bounded_capped(command, &log, request.deadline, MAX_LOG_BYTES)?;
    if finished.killed {
        return Err(format!("cargo rustdoc exceeded {:?}", request.deadline));
    }
    if !finished.success {
        let last = finished
            .output
            .lines()
            .rev()
            .find(|line| line.starts_with("error"))
            .unwrap_or("cargo rustdoc failed")
            .to_string();
        return Err(last);
    }
    let json = request
        .target_dir
        .join("doc")
        .join(format!("{}.json", request.lib_name.replace('-', "_")));
    if !json.is_file() {
        return Err(format!("{} not written", json.display()));
    }
    // Moved out of the shared target directory: the next crate's run must
    // not see (or overwrite) it.
    let kept = request.work_dir.join("crate.json");
    std::fs::rename(&json, &kept)
        .or_else(|_| std::fs::copy(&json, &kept).map(|_| ()))
        .map_err(|e| format!("keep JSON: {e}"))?;
    Ok(kept)
}

/// A dependency key in the probe's manifest (quoted when it isn't a bare key).
fn toml_key(name: &str) -> String {
    let bare = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if bare {
        format!("dep-{name}")
    } else {
        format!("{:?}", format!("dep-{name}"))
    }
}

/// How a rustdoc span's file is recorded in an index built for the graph
/// rooted at `root` (the shard's source directory): relative when inside
/// it; `@toolchain:<crate>/src/…` for the toolchain's library (under any
/// sysroot or `/rustc/<commit>/` remapping); `@abs:<path>` otherwise.
pub fn span_file(root: &Path, filename: &str) -> Option<String> {
    let normalized = normalize(filename);
    let path = Path::new(&normalized);
    if path.is_absolute() {
        if let Ok(inside) = path.strip_prefix(root) {
            return Some(inside.to_string_lossy().into_owned());
        }
        if let Some(library) = toolchain_relative(&normalized) {
            return Some(format!("@toolchain:{library}"));
        }
        return Some(format!("@abs:{normalized}"));
    }
    if let Some(library) = toolchain_relative(&normalized) {
        if !root.join(&normalized).exists() {
            return Some(format!("@toolchain:{library}"));
        }
    }
    Some(normalized)
}

/// A toolchain JSON's span file (`library/core/src/cell.rs`, relative to the
/// source checkout) as the toolchain shard records it (`core/src/cell.rs`).
pub fn toolchain_span_file(filename: &str) -> Option<String> {
    let normalized = normalize(filename);
    normalized
        .strip_prefix("library/")
        .map(str::to_string)
        .or_else(|| toolchain_relative(&normalized))
}

/// `…/library/<std|core|alloc>/src/…` → `<crate>/src/…`.
fn toolchain_relative(path: &str) -> Option<String> {
    let at = path.rfind("library/")?;
    let rest = &path[at + "library/".len()..];
    let krate = rest.split('/').next()?;
    (TOOLCHAIN_CRATES.contains(&krate) && rest[krate.len()..].starts_with("/src/"))
        .then(|| rest.to_string())
}

/// `a/b/../c` → `a/c`, `./x` → `x` (lexically).
fn normalize(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|last| *last != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolchain_versions_match_release_and_commit() {
        let described = "1.100.0-nightly\t(1303417c4\t2026-09-21)";
        assert!(same_toolchain(described, "1.100.0-nightly+1303417c416e"));
        assert!(!same_toolchain(described, "1.100.0-nightly+d080e7dff1b0"));
        assert!(!same_toolchain(described, "1.101.0-nightly+1303417c416e"));
        assert!(same_toolchain(
            "1.85.0\t(4d91de4e4\t2025-02-17)",
            "1.85.0+4d91de4e48"
        ));
        assert!(!same_toolchain(described, "1.100.0-nightly"));
    }

    #[test]
    fn span_files_are_recorded_per_graph() {
        let root = Path::new("/reg/serde_json-1.0.0");
        assert_eq!(
            span_file(root, "/reg/serde_json-1.0.0/src/de.rs").as_deref(),
            Some("src/de.rs")
        );
        assert_eq!(
            span_file(root, "/rustc/abc/library/alloc/src/string.rs").as_deref(),
            Some("@toolchain:alloc/src/string.rs")
        );
        assert_eq!(
            span_file(root, "/reg/serde-1.0.0/src/lib.rs").as_deref(),
            Some("@abs:/reg/serde-1.0.0/src/lib.rs")
        );
        assert_eq!(
            toolchain_span_file("library/std/src/../../core/src/primitive_docs.rs").as_deref(),
            Some("core/src/primitive_docs.rs")
        );
        assert_eq!(
            toolchain_span_file("library/std/src/../../backtrace/src/lib.rs").as_deref(),
            Some("backtrace/src/lib.rs")
        );
    }
}

#[cfg(test)]
mod toolchain_json_tests {
    use super::*;

    fn toolchain(home: &Path, name: &str, crate_version: Option<&str>) -> PathBuf {
        let dir = home
            .join("toolchains")
            .join(name)
            .join("share/doc/rust/json");
        if let Some(version) = crate_version {
            std::fs::create_dir_all(&dir).unwrap();
            for krate in TOOLCHAIN_CRATES {
                std::fs::write(
                    dir.join(format!("{krate}.json")),
                    format!("{{\"root\":0,\"crate_version\":\"{version}\",\"index\":{{}}}}"),
                )
                .unwrap();
            }
        }
        dir
    }

    /// A `rustup-toolchain-install-master` toolchain without the component
    /// takes the JSON of another toolchain of the same commit, never one of
    /// another nightly.
    #[test]
    fn a_sibling_toolchain_of_the_same_commit_supplies_the_json() {
        let home = tempfile::tempdir().unwrap();
        let own = toolchain(home.path(), "rustc-master", None);
        let other_nightly = toolchain(
            home.path(),
            "nightly-x86_64-unknown-linux-gnu",
            Some("1.100.0-nightly\t(1303417c4\t2026-09-21)"),
        );
        let same_commit = toolchain(
            home.path(),
            "rustc-master-json",
            Some("1.101.0-nightly\t(d080e7dff\t2026-09-27)"),
        );
        let dirs = json_dirs_with(None, Some(home.path()));
        assert!(!dirs.contains(&own), "no component there: {dirs:?}");
        assert_eq!(dirs, vec![other_nightly.clone(), same_commit.clone()]);

        let files = first_matching(&dirs, "1.101.0-nightly+d080e7dff1b0").unwrap();
        assert!(files.iter().all(|(_, path)| path.starts_with(&same_commit)));
        let err = first_matching(&dirs, "1.101.0-nightly+aaaaaaaaaaaa").unwrap_err();
        assert!(err.contains("describes"), "{err}");
        // The toolchain's own directory, when it has the component, comes first.
        let dirs = json_dirs_with(Some(same_commit.clone()), Some(home.path()));
        assert_eq!(dirs, vec![same_commit, other_nightly]);
    }
}
