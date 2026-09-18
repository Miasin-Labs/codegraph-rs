//! Dependency graphs: one shared, read-only codegraph *shard* per dependency
//! version, built once on this machine and reused by every project that
//! pins that version.
//!
//! The pieces, in pipeline order:
//!
//! * [`lockfile`] — a project's lockfiles (Cargo.lock, package-lock.json,
//!   pnpm-lock.yaml, bun.lock, yarn.lock, go.mod) → [`ResolvedDep`]s.
//! * [`locate`] — where each one's source already is on this machine
//!   (`$CARGO_HOME`, `$GOMODCACHE`, the project's `node_modules`/`vendor`);
//!   never the network. Not found → *unavailable*, not an error.
//! * [`scope`] — the library files worth indexing, within per-shard budgets.
//! * [`shard`] — building a shard atomically under a per-shard lock into
//!   `codegraph_home()/deps/<ecosystem>/<name>-<version>/`, and opening one
//!   read-only.
//! * [`registry`] — `deps/registry.db`: versions, shard states, and which
//!   projects (by canonical checkout root, the atlas's key) use them.
//! * [`builder`], [`gc`], [`trigger`] — building what's pending within a
//!   budget, collecting unused shards, and the post-index hook that records
//!   a project and starts one detached background build.
//!
//! Local path dependencies are recorded (source `path`) but never built:
//! linking a project to its sibling checkouts is the atlas's job.

pub mod builder;
mod error;
pub mod gc;
pub mod locate;
pub mod lockfile;
mod model;
pub mod project;
pub mod registry;
pub mod scope;
pub mod shard;
pub mod store;
pub mod trigger;

use std::path::Path;

pub use self::error::{DepsError, DepsResult};
pub use self::model::{DepKey, DepSource, Ecosystem, ResolvedDep, ShardState, SourceKind};
pub use self::registry::{ProjectDependency, Registry};
pub use self::shard::{ShardHandle, ShardMeta};
pub use self::store::DepsHome;

/// The shard of `name`@`version`, opened read-only — `None` when it hasn't
/// been built (or was built for an incompatible schema). Never creates a
/// file or directory. For git dependencies `version` carries the commit
/// (see [`DepKey`]); [`dependencies_of`] returns keys in that form.
pub fn shard_for(ecosystem: Ecosystem, name: &str, version: &str) -> Option<ShardHandle> {
    ShardHandle::open(
        &DepsHome::from_env(),
        &DepKey::new(ecosystem, name, version),
    )
}

/// The dependencies recorded for the project at `project_root` (any path
/// inside is not enough — its canonical checkout root), read-only. Empty
/// when nothing was recorded.
pub fn dependencies_of(project_root: &Path) -> DepsResult<Vec<ProjectDependency>> {
    dependencies_of_in(&DepsHome::from_env(), project_root)
}

/// [`dependencies_of`] against an explicit store.
pub fn dependencies_of_in(
    home: &DepsHome,
    project_root: &Path,
) -> DepsResult<Vec<ProjectDependency>> {
    match Registry::open_read_only(&home.registry_path())? {
        Some(registry) => registry.dependencies_of(&project::canonical_root(project_root)),
        None => Ok(Vec::new()),
    }
}
