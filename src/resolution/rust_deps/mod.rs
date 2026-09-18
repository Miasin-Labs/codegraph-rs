//! Public method names of a Rust project's dependencies.
//!
//! A method call on a receiver of unknown type (`node.walk()` where `node`
//! came from a closure, `a.b().as_object()`) whose method a dependency
//! defines is not resolved by name alone: a same-named project method is a
//! guess, and usually a wrong one (see `name_matcher::rust_call`). This
//! module supplies those names, like the generated std list does for std.
//!
//! - `Cargo.lock` names the crates the workspace members depend on directly
//!   ([`lockfile`]), and the crates those re-export (`clap` →
//!   `clap_builder`) follow.
//! - Each crate's source is found vendored or in the local cargo registry
//!   and git checkouts ([`sources`]); nothing is downloaded.
//! - Its public method names are read with the tree-sitter-rust grammar
//!   ([`scan`]), bounded in files, bytes, and time.
//! - Each crate version gets one small artifact ([`store`]), shared by every
//!   project under `~/.codegraph/deps/crates/` (`CODEGRAPH_DEPS_DIR`
//!   overrides the root), or kept in the project's `.codegraph/deps/` when
//!   the source is vendored.
//!
//! [`prepare`] builds missing artifacts and runs only when indexing (`codegraph
//! init|index|sync`), never on the MCP or hook request path; resolution only
//! reads them ([`load`]). A missing lockfile, source, or artifact leaves that
//! crate's names out, which is the behaviour without this module.
//! `CODEGRAPH_RUST_DEPS=0` turns both off.

mod build;
mod lockfile;
mod scan;
mod sources;
mod store;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use build::Job;
use lockfile::{Lock, LockedCrate};
use sources::{Origin, locate, vendored};
use store::CrateApi;

use crate::directory::get_codegraph_dir;

/// How long one [`prepare`] may spend scanning sources, unless
/// `CODEGRAPH_RUST_DEPS_BUDGET_MS` says otherwise.
const DEFAULT_BUDGET: Duration = Duration::from_secs(20);
/// How many re-export hops (`clap` → `clap_builder`) are followed.
const MAX_REEXPORT_HOPS: usize = 2;

/// The method names a project's direct dependencies define publicly.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RustDependencyApi {
    methods: HashSet<String>,
    crates: usize,
}

impl RustDependencyApi {
    /// Some dependency defines a public method `name`.
    pub fn has_method(&self, name: &str) -> bool {
        self.methods.contains(name)
    }

    /// How many distinct method names the dependencies define.
    pub fn method_count(&self) -> usize {
        self.methods.len()
    }

    /// How many dependencies contributed names.
    pub fn crate_count(&self) -> usize {
        self.crates
    }
}

/// Where dependency sources and artifacts are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepsEnv {
    /// `$CARGO_HOME` (`~/.cargo`): its `registry/src/` and `git/checkouts/`.
    pub cargo_home: PathBuf,
    /// The shared store (`~/.codegraph/deps`); artifacts go in `crates/`.
    pub store: PathBuf,
}

impl DepsEnv {
    /// From `CARGO_HOME`, `CODEGRAPH_DEPS_DIR`, and `HOME`; `None` when
    /// `CODEGRAPH_RUST_DEPS=0` or no home directory is known.
    pub fn from_env() -> Option<DepsEnv> {
        if std::env::var("CODEGRAPH_RUST_DEPS").is_ok_and(|value| value.trim() == "0") {
            return None;
        }
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from);
        let from = |name: &str| {
            std::env::var_os(name)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        Some(DepsEnv {
            cargo_home: from("CARGO_HOME").or_else(|| home.as_ref().map(|h| h.join(".cargo")))?,
            store: from("CODEGRAPH_DEPS_DIR")
                .or_else(|| home.as_ref().map(|h| h.join(".codegraph").join("deps")))?,
        })
    }
}

/// What one [`prepare`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PrepareReport {
    /// Crates looked at: direct dependencies and the crates they re-export.
    pub crates: usize,
    /// Artifacts already current.
    pub cached: usize,
    /// Artifacts built now.
    pub built: usize,
    /// Crates whose source is not on this machine.
    pub missing: usize,
    /// Crates left for a later run: the time budget ran out, or the
    /// artifact could not be written.
    pub deferred: usize,
}

/// Build the artifacts `project_root`'s dependencies lack, within the time
/// budget. Indexing only: this reads crate sources.
pub fn prepare(project_root: &Path) -> Option<PrepareReport> {
    let env = DepsEnv::from_env()?;
    let budget = std::env::var("CODEGRAPH_RUST_DEPS_BUDGET_MS")
        .ok()
        .and_then(|ms| ms.trim().parse().ok())
        .map_or(DEFAULT_BUDGET, Duration::from_millis);
    prepare_with(project_root, &env, budget)
}

/// [`prepare`] against explicit locations.
pub fn prepare_with(project_root: &Path, env: &DepsEnv, budget: Duration) -> Option<PrepareReport> {
    let lock = read_lock(project_root)?;
    let deadline = Instant::now() + budget;
    let stores = Stores::new(project_root, env);
    let mut report = PrepareReport::default();
    let mut walk = Walk::new(&lock);
    while let Some(round) = walk.next_round() {
        report.crates += round.len();
        let mut known = Vec::new();
        let mut jobs = Vec::new();
        for krate in round {
            let Some(source) = locate(&krate, project_root, &env.cargo_home) else {
                report.missing += 1;
                continue;
            };
            let path = stores.path(&krate, &source.origin);
            match store::read(&path, &krate) {
                Some(api) => {
                    report.cached += 1;
                    known.push((krate, api));
                }
                None => jobs.push(Job {
                    krate,
                    dir: source.dir,
                    path,
                }),
            }
        }
        for (job, api) in build::build_all(jobs, deadline) {
            match api {
                Some(api) => {
                    report.built += 1;
                    known.push((job.krate, api));
                }
                None => report.deferred += 1,
            }
        }
        walk.follow(&known);
    }
    Some(report)
}

/// The dependency method names `project_root`'s artifacts record: read
/// only, cheap, and safe on any path. `None` without a `Cargo.lock` or
/// any artifact.
pub fn load(project_root: &Path) -> Option<RustDependencyApi> {
    load_with(project_root, &DepsEnv::from_env()?)
}

/// [`load`] against explicit locations.
pub fn load_with(project_root: &Path, env: &DepsEnv) -> Option<RustDependencyApi> {
    let lock = read_lock(project_root)?;
    let stores = Stores::new(project_root, env);
    let mut api = RustDependencyApi::default();
    let mut walk = Walk::new(&lock);
    while let Some(round) = walk.next_round() {
        let mut known = Vec::new();
        for krate in round {
            let origin = artifact_origin(&krate, project_root);
            if let Some(found) = store::read(&stores.path(&krate, &origin), &krate) {
                api.methods.extend(found.methods.iter().cloned());
                api.crates += 1;
                known.push((krate, found));
            }
        }
        walk.follow(&known);
    }
    (api.crates > 0).then_some(api)
}

/// The crates to read, round by round: the direct dependencies, then the
/// crates the previous round re-exports, [`MAX_REEXPORT_HOPS`] deep.
struct Walk<'a> {
    lock: &'a Lock,
    next: Vec<LockedCrate>,
    seen: HashSet<LockedCrate>,
    hops: usize,
}

impl<'a> Walk<'a> {
    fn new(lock: &'a Lock) -> Self {
        Walk {
            lock,
            next: lock.direct_dependencies(),
            seen: HashSet::new(),
            hops: 0,
        }
    }

    fn next_round(&mut self) -> Option<Vec<LockedCrate>> {
        let mut round = std::mem::take(&mut self.next);
        round.retain(|krate| self.seen.insert(krate.clone()));
        (!round.is_empty()).then_some(round)
    }

    /// Queue what the crates of the round just read re-export.
    fn follow(&mut self, read: &[(LockedCrate, CrateApi)]) {
        if self.hops >= MAX_REEXPORT_HOPS {
            return;
        }
        self.hops += 1;
        self.next = read
            .iter()
            .flat_map(|(krate, api)| {
                api.reexports
                    .iter()
                    .filter_map(|ident| self.lock.dependency_named(krate, ident))
            })
            .collect();
    }
}

/// The shared store's `crates/` dir and the project's own `.codegraph/deps/`.
struct Stores {
    global: PathBuf,
    project: PathBuf,
}

impl Stores {
    fn new(project_root: &Path, env: &DepsEnv) -> Self {
        Stores {
            global: env.store.join("crates"),
            project: get_codegraph_dir(project_root).join("deps"),
        }
    }

    fn path(&self, krate: &LockedCrate, origin: &Origin) -> PathBuf {
        store::artifact_path(krate, origin, &self.global, &self.project)
    }
}

/// Where `krate`'s artifact would have come from, without looking for its
/// source beyond the project's own `vendor/`.
fn artifact_origin(krate: &LockedCrate, project_root: &Path) -> Origin {
    if vendored(krate, project_root).is_some() {
        return Origin::Vendored;
    }
    match krate.git_commit() {
        Some(commit) => Origin::Git {
            commit: commit.to_string(),
        },
        None => Origin::Registry,
    }
}

fn read_lock(project_root: &Path) -> Option<Lock> {
    let text = std::fs::read_to_string(project_root.join("Cargo.lock")).ok()?;
    let lock = Lock::parse(&text);
    (!lock.direct_dependencies().is_empty()).then_some(lock)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::time::Duration;

    use super::{DepsEnv, load_with, prepare_with};

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn package(name: &str, version: &str) -> String {
        format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\n")
    }

    /// A project pinning one registry crate and one vendored crate: the
    /// registry artifact goes to the shared store and is reused by a second
    /// project, the vendored one stays with its project.
    #[test]
    fn prepares_shared_and_vendored_artifacts_then_loads_them() {
        let cargo = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let env = DepsEnv {
            cargo_home: cargo.path().to_path_buf(),
            store: store.path().to_path_buf(),
        };
        let registry = cargo
            .path()
            .join("registry/src/index.crates.io-abc/treelike-0.2.0");
        write(&registry.join("Cargo.toml"), &package("treelike", "0.2.0"));
        write(
            &registry.join("src/lib.rs"),
            "pub struct Node;\nimpl Node { pub fn walk(&self) {} pub fn kind(&self) -> u8 { 0 } }\n",
        );
        let lock = "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\n \"treelike\",\n \"vend\",\n \"absent\",\n]\n\n[[package]]\nname = \"treelike\"\nversion = \"0.2.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"vend\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"absent\"\nversion = \"9.9.9\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
        let first = tempfile::tempdir().unwrap();
        write(&first.path().join("Cargo.lock"), lock);
        let vendor = first.path().join("vendor/vend");
        write(&vendor.join("Cargo.toml"), &package("vend", "1.0.0"));
        write(
            &vendor.join("src/lib.rs"),
            "pub trait Visit { fn visit_all(&self); }\n",
        );

        let report = prepare_with(first.path(), &env, Duration::from_secs(30)).unwrap();
        assert_eq!((report.crates, report.built, report.missing), (3, 2, 1));
        assert!(store.path().join("crates/treelike-0.2.0.api").is_file());
        assert!(
            first
                .path()
                .join(".codegraph/deps/vend-1.0.0.api")
                .is_file()
        );
        let api = load_with(first.path(), &env).unwrap();
        for name in ["walk", "kind", "visit_all"] {
            assert!(api.has_method(name), "{name}");
        }
        assert!(!api.has_method("absent_method"));

        // Another project pinning the same version reuses the artifact.
        let second = tempfile::tempdir().unwrap();
        write(&second.path().join("Cargo.lock"), lock);
        let report = prepare_with(second.path(), &env, Duration::from_secs(30)).unwrap();
        assert_eq!((report.cached, report.built), (1, 0));
        let api = load_with(second.path(), &env).unwrap();
        assert!(api.has_method("walk") && !api.has_method("visit_all"));
    }

    #[test]
    fn no_lockfile_means_no_names() {
        let project = tempfile::tempdir().unwrap();
        let env = DepsEnv {
            cargo_home: project.path().join("cargo"),
            store: project.path().join("store"),
        };
        assert_eq!(
            prepare_with(project.path(), &env, Duration::from_secs(1)),
            None
        );
        assert_eq!(load_with(project.path(), &env), None);
    }
}
