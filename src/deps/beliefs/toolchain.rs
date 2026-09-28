//! The toolchain's own crates — `std`, `core`, `alloc` — as shards, so a
//! crate's calls into them (`File::create`, `Mutex::lock`,
//! `CString::into_raw`) resolve like calls into any dependency.
//!
//! Built from the local `rust-src` component (`$(rustc --print
//! sysroot)/lib/rustlib/src/rust/library`, or `CODEGRAPH_RUST_SRC`), never
//! downloaded, into `deps/beliefs/toolchain/crates/<name>-<rustc
//! version>/` — apart from the dependency store, since no lockfile names
//! them and `deps gc` must not weigh them against projects' shards.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::deps::scope::ShardLimits;
use crate::deps::shard::{BuildOutcome, BuildRequest, ShardMeta, build_shard};
use crate::deps::{DepKey, DepSource, DepsHome, Ecosystem};
use crate::resolution::external::ReachableGraph;

/// The toolchain crates resolved into, in the order `std` re-exports them.
pub const TOOLCHAIN_CRATES: &[&str] = &["std", "core", "alloc"];

/// The local toolchain: its `rustc -V` and its library sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toolchain {
    /// `rustc 1.101.0-nightly (d080e7dff 2026-09-27)`.
    pub rustc: String,
    /// `1.101.0-nightly`: the version the shards are keyed by.
    pub version: String,
    pub library: PathBuf,
}

impl Toolchain {
    /// The toolchain `rustc` on `PATH` reports, when its `rust-src` is
    /// installed (`CODEGRAPH_RUST_SRC` names a `library/` dir instead).
    pub fn locate() -> Option<Toolchain> {
        let output = Command::new("rustc").arg("-V").output().ok()?;
        let rustc = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let version = rustc.split_whitespace().nth(1)?.to_string();
        let library = match std::env::var_os("CODEGRAPH_RUST_SRC") {
            Some(dir) => PathBuf::from(dir),
            None => {
                let sysroot = Command::new("rustc")
                    .args(["--print", "sysroot"])
                    .output()
                    .ok()?;
                let sysroot = String::from_utf8_lossy(&sysroot.stdout).trim().to_string();
                Path::new(&sysroot).join("lib/rustlib/src/rust/library")
            }
        };
        library
            .join("std/src/lib.rs")
            .is_file()
            .then_some(Toolchain {
                rustc,
                version,
                library,
            })
    }

    fn key(&self, name: &str) -> DepKey {
        DepKey::new(Ecosystem::Crates, name, self.version.clone())
    }
}

/// The store the toolchain shards live in.
pub fn toolchain_home(beliefs_dir: &Path) -> DepsHome {
    DepsHome::at(beliefs_dir.join("toolchain"))
}

/// Build (or confirm) the toolchain shards; the graphs for those that
/// exist. `alloc` is also reachable as `alloc_crate`, the name `std`
/// re-exports it under (`pub use alloc_crate::vec`).
pub async fn ensure(home: &DepsHome, toolchain: &Toolchain, time_ms: u64) -> Vec<ReachableGraph> {
    let limits = ShardLimits {
        max_files: 8_000,
        max_bytes: 96 * 1024 * 1024,
        max_file_bytes: 2 * 1024 * 1024,
        time_ms,
        max_db_bytes: 256 * 1024 * 1024,
    };
    let mut graphs = Vec::new();
    for name in TOOLCHAIN_CRATES {
        let key = toolchain.key(name);
        let source_dir = toolchain.library.join(name);
        let outcome = build_shard(
            home,
            &BuildRequest {
                key: &key,
                source: &DepSource::Registry,
                source_dir: &source_dir,
                limits,
                force: false,
            },
        )
        .await;
        let meta = match outcome {
            BuildOutcome::Built(meta) | BuildOutcome::UpToDate(meta) => Some(meta),
            BuildOutcome::Locked => ShardMeta::read(&home.shard_dir(&key)),
            BuildOutcome::NoSources | BuildOutcome::Failed(_) => None,
        };
        graphs.extend(meta.map(|meta| ReachableGraph::shard(&key, &meta, home.shard_dir(&key))));
    }
    with_aliases(graphs)
}

/// The toolchain shards already built (read-only; nothing is created).
pub fn existing(home: &DepsHome, toolchain_version: &str) -> Vec<ReachableGraph> {
    let graphs = TOOLCHAIN_CRATES
        .iter()
        .filter_map(|name| {
            let key = DepKey::new(Ecosystem::Crates, *name, toolchain_version);
            let dir = home.shard_dir(&key);
            let meta = ShardMeta::read(&dir)
                .filter(|meta| meta.is_readable() && meta.state.has_shard())?;
            Some(ReachableGraph::shard(&key, &meta, dir))
        })
        .collect();
    with_aliases(graphs)
}

fn with_aliases(mut graphs: Vec<ReachableGraph>) -> Vec<ReachableGraph> {
    if let Some(alloc) = graphs.iter().find(|graph| graph.krate == "alloc").cloned() {
        graphs.push(ReachableGraph {
            krate: "alloc_crate".to_string(),
            ..alloc
        });
    }
    graphs
}
