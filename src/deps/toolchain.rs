//! The Rust toolchain's own library as a dependency: `std`, `core` and
//! `alloc` from the `rust-src` component, built into one read-only shard
//! per toolchain (`deps/rust/std-<release>+<commit>/`) — the one
//! representation of the toolchain every feature uses:
//!
//! * recorded for every project with a `Cargo.lock`, like a crate it
//!   depends on (built by `deps build`, gc'd with the other shards), so the
//!   external pass resolves the project's `std` calls and taint reads it;
//! * the ecosystem beliefs build ([`super::beliefs`]) builds or reuses the
//!   same shard for its population's read-only resolution.
//!
//! Resolution sees it as one graph per crate ([`graphs`]): `std`, `core`,
//! `alloc` (and `alloc_crate`, the name `std` re-exports `alloc` under),
//! each scoped to its own directory of the shard, so a path resolves in
//! the crate it names and re-exports hop between them as between crates.
//!
//! The toolchain is the one `rustc` resolves in the project's directory
//! (so a `rust-toolchain.toml` pin is honoured), asked with a deadline;
//! its library sources are `$(rustc --print sysroot)/lib/rustlib/src/rust/
//! library`, present only when `rust-src` is installed — without it there
//! is nothing to record, never an error. `CODEGRAPH_STD=0` turns this off;
//! `CODEGRAPH_RUST_SRC` (a `library` directory) with
//! `CODEGRAPH_RUST_VERSION` names one explicitly instead of asking rustc.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::model::{DepKey, DepSource, Ecosystem, ResolvedDep};
use super::scope::ShardLimits;
use super::shard::{BuildOutcome, BuildRequest, ShardMeta, build_shard};
use super::store::DepsHome;
use crate::db::ExternalGraphKind;
use crate::resolution::external::{GraphLocation, ReachableGraph};

/// The shard name of the toolchain's library.
pub const STD_NAME: &str = "std";
/// The library crates the shard holds, as code names them.
pub const STD_CRATES: &[&str] = &["std", "core", "alloc"];
/// The name `std` re-exports `alloc` under (`pub use alloc_crate::vec`).
pub const ALLOC_ALIAS: &str = "alloc_crate";
/// What the registry records as the dependency's lockfile.
pub const TOOLCHAIN_LOCKFILE: &str = "rust-toolchain";
/// Longest wait for `rustc`.
const RUSTC_DEADLINE: Duration = Duration::from_secs(5);

/// One toolchain's library sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toolchain {
    /// `1.101.0-nightly+d080e7dff1b0`: the release and its commit, so two
    /// nightlies of one release never share a shard.
    pub version: String,
    /// The `library` directory (`std/src/lib.rs` inside it).
    pub library: PathBuf,
}

impl Toolchain {
    pub fn key(&self) -> DepKey {
        DepKey::new(Ecosystem::Rust, STD_NAME, self.version.clone())
    }

    /// The toolchain as a dependency the registry records.
    pub fn as_dependency(&self) -> ResolvedDep {
        ResolvedDep {
            key: self.key(),
            lock_version: self.version.clone(),
            source: DepSource::Registry,
            direct: Some(true),
            lockfile: TOOLCHAIN_LOCKFILE.to_string(),
            install_path: None,
        }
    }
}

/// How [`super::locate::SourceRoots`] finds the toolchain.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ToolchainSource {
    /// Not recorded (tests, explicit roots).
    #[default]
    None,
    /// Ask `rustc` in the project's directory.
    Detect,
    /// This one.
    Fixed(Toolchain),
}

impl ToolchainSource {
    /// The toolchain for the project at `project_root`, if its library
    /// sources are here.
    pub fn resolve(&self, project_root: &Path) -> Option<Toolchain> {
        match self {
            Self::None => None,
            Self::Fixed(toolchain) => Some(toolchain.clone()),
            Self::Detect => detect(project_root),
        }
    }
}

/// `CODEGRAPH_STD=0` (or `false`/`off`) turns the toolchain shard off.
pub fn std_enabled() -> bool {
    !std::env::var("CODEGRAPH_STD").is_ok_and(|v| {
        let v = v.trim();
        v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off")
    })
}

/// The toolchain `rustc` resolves in `project_root`, with its library
/// sources, or `None` (no rustc, no `rust-src`, or too slow to answer).
pub fn detect(project_root: &Path) -> Option<Toolchain> {
    if !std_enabled() {
        return None;
    }
    if let Some(library) = std::env::var_os("CODEGRAPH_RUST_SRC").filter(|v| !v.is_empty()) {
        let library = PathBuf::from(library);
        let version = std::env::var("CODEGRAPH_RUST_VERSION").ok()?;
        return is_library(&library).then(|| Toolchain {
            version: sanitize(&version),
            library,
        });
    }
    let verbose = rustc(project_root, &["-vV"])?;
    let field = |name: &str| {
        verbose
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .map(|v| v.trim().to_string())
    };
    let release = field("release:")?;
    let commit: String = field("commit-hash:")
        .unwrap_or_default()
        .chars()
        .take(12)
        .collect();
    let sysroot = rustc(project_root, &["--print", "sysroot"])?;
    let library = Path::new(sysroot.trim()).join("lib/rustlib/src/rust/library");
    if !is_library(&library) {
        return None;
    }
    let version = if commit.is_empty() || commit == "unknown" {
        release
    } else {
        format!("{release}+{commit}")
    };
    Some(Toolchain {
        version: sanitize(&version),
        library: std::fs::canonicalize(&library).unwrap_or(library),
    })
}

fn is_library(dir: &Path) -> bool {
    dir.join("std/src/lib.rs").is_file() && dir.join("core/src/lib.rs").is_file()
}

/// `rustc <args>` in `dir`, its stdout, within [`RUSTC_DEADLINE`].
fn rustc(dir: &Path, args: &[&str]) -> Option<String> {
    let mut child = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if started.elapsed() >= RUSTC_DEADLINE => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    let mut out = String::new();
    use std::io::Read;
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    Some(out)
}

/// A version as one path segment's worth of text.
fn sanitize(version: &str) -> String {
    version
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `base` with the toolchain's size budgets (~1k files, 14 MB of source),
/// whoever builds it: the shard is the same whether `deps build` or the
/// beliefs build made it. The time budget stays the caller's.
pub fn shard_limits(base: ShardLimits) -> ShardLimits {
    ShardLimits {
        max_files: base.max_files.max(8_000),
        max_bytes: base.max_bytes.max(96 * 1024 * 1024),
        max_file_bytes: base.max_file_bytes.max(2 * 1024 * 1024),
        time_ms: base.time_ms,
        max_db_bytes: base.max_db_bytes.max(256 * 1024 * 1024),
    }
}

/// Build (or confirm) the shard of `toolchain` in `home` within `time_ms`,
/// waiting out another builder holding its lock: its meta, or `None` when
/// there is nothing to index or the build failed.
pub async fn ensure(home: &DepsHome, toolchain: &Toolchain, time_ms: u64) -> Option<ShardMeta> {
    let key = toolchain.key();
    let limits = shard_limits(ShardLimits {
        time_ms,
        ..ShardLimits::default()
    });
    for _ in 0..600 {
        let outcome = build_shard(
            home,
            &BuildRequest {
                key: &key,
                source: &DepSource::Registry,
                source_dir: &toolchain.library,
                limits,
                force: false,
            },
        )
        .await;
        match outcome {
            BuildOutcome::Built(meta) | BuildOutcome::UpToDate(meta) => return Some(meta),
            BuildOutcome::NoSources | BuildOutcome::Failed(_) => return None,
            BuildOutcome::Locked => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    None
}

/// The graphs of the toolchain shard `key` in `home`, read-only (nothing
/// is created): empty when it has no shard this build reads.
pub fn graphs(home: &DepsHome, key: &DepKey) -> Vec<ReachableGraph> {
    let dir = home.shard_dir(key);
    ShardMeta::read(&dir)
        .filter(|meta| meta.is_readable() && meta.state.has_shard() && meta.key() == *key)
        .map(|meta| graphs_of(key, &meta, dir))
        .unwrap_or_default()
}

/// The toolchain shard `key` (described by `meta`, in `dir`) as the graphs
/// of `std`, `core` and `alloc` — one database, each crate its own library
/// root — plus `alloc` again as `alloc_crate`. Each crate's items are its
/// own directory's: a path resolves in the crate it names, and re-exports
/// hop between them as between any crates. `std` is also the facade of
/// `core` and `alloc` for types' members (it re-exports nearly all their
/// types): `RefCell::borrow`, `str::len`, `Poll::Ready` resolve through
/// `std` even where the re-export is out of the path rules' reach (`pub mod
/// task { pub use core::task::*; }` inline, a type node the grammar drops
/// like nightly's `RefCell`); a bare item never does (`std::str::eq` is not
/// `core::ptr::eq`).
pub fn graphs_of(key: &DepKey, meta: &ShardMeta, dir: PathBuf) -> Vec<ReachableGraph> {
    let source_dir = PathBuf::from(&meta.source_dir);
    let graph = |name: &str, krate: &str| ReachableGraph {
        kind: ExternalGraphKind::Dependency,
        key: format!("{}/{}", key.ecosystem.as_str(), key.dir_name()),
        krate: name.to_string(),
        lib_root: format!("{krate}/src/lib.rs"),
        fingerprint: format!(
            "{}:{}:{}",
            meta.built_at_ms, meta.extractor_version, meta.source_fingerprint
        ),
        location: GraphLocation::Shard {
            dir: dir.clone(),
            source_dir: source_dir.clone(),
            crate_dir: krate.to_string(),
            facade_of: if krate == STD_NAME {
                STD_CRATES
                    .iter()
                    .filter(|other| **other != STD_NAME)
                    .map(|other| (*other).to_string())
                    .collect()
            } else {
                Vec::new()
            },
        },
    };
    let mut graphs: Vec<ReachableGraph> =
        STD_CRATES.iter().map(|krate| graph(krate, krate)).collect();
    graphs.push(graph(ALLOC_ALIAS, "alloc"));
    graphs
}

/// The toolchain version a graph key names
/// (`rust/std-1.101.0-nightly+d080e7dff1b0` → `1.101.0-nightly+d080e7dff1b0`);
/// `None` for any other graph.
pub fn graph_key_version(key: &str) -> Option<&str> {
    key.strip_prefix(Ecosystem::Rust.as_str())?
        .strip_prefix('/')?
        .strip_prefix(STD_NAME)?
        .strip_prefix('-')
        .filter(|version| !version.is_empty())
}

/// The toolchain crate a file of the shard belongs to
/// (`alloc/src/vec/mod.rs` → `alloc`).
pub fn crate_of_file(file: &str) -> Option<&'static str> {
    let first = file.split('/').next()?;
    STD_CRATES.iter().copied().find(|krate| *krate == first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_keys_and_files_name_the_toolchain_crate() {
        assert_eq!(
            graph_key_version("rust/std-1.101.0-nightly+d080e7dff1b0"),
            Some("1.101.0-nightly+d080e7dff1b0")
        );
        assert_eq!(graph_key_version("crates/std-1.0.0"), None);
        assert_eq!(graph_key_version("rust/std-"), None);
        assert_eq!(crate_of_file("alloc/src/vec/mod.rs"), Some("alloc"));
        assert_eq!(crate_of_file("stdarch/crates/x.rs"), None);
        let limits = shard_limits(ShardLimits::default());
        assert!(limits.covers(&ShardLimits::default()));
        assert_eq!(limits.time_ms, ShardLimits::default().time_ms);
    }

    #[test]
    fn a_fixed_toolchain_is_a_direct_rust_dependency() {
        let toolchain = Toolchain {
            version: "1.99.0+abc".into(),
            library: PathBuf::from("/sysroot/lib/rustlib/src/rust/library"),
        };
        let dep = toolchain.as_dependency();
        assert_eq!(dep.key.ecosystem, Ecosystem::Rust);
        assert_eq!(dep.key.dir_name(), "std-1.99.0+abc");
        assert_eq!(dep.direct, Some(true));
        assert_eq!(
            ToolchainSource::Fixed(toolchain.clone()).resolve(Path::new("/p")),
            Some(toolchain)
        );
        assert_eq!(ToolchainSource::None.resolve(Path::new("/p")), None);
    }

    #[test]
    fn versions_stay_one_segment() {
        assert_eq!(
            sanitize("1.101.0-nightly+d080e7dff1b0"),
            "1.101.0-nightly+d080e7dff1b0"
        );
        assert_eq!(sanitize("1.0 (x/y)"), "1.0__x_y_");
    }
}
