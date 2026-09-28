//! Ecosystem beliefs: how the crates of the cargo cache call the library
//! APIs they depend on, mined into per-API protocols (Engler's deviance at
//! ecosystem scale) that `codegraph analyze bugs` checks a project against.
//!
//! Layout under `codegraph_home()/deps/beliefs/` (dirs 0700, files 0600,
//! written temp+rename):
//!
//! * `beliefs.json` — the artifact ([`model::BeliefSet`]): each strong
//!   belief with its support (crates and sites), z, lift, and example
//!   crates. The only file `analyze bugs` reads.
//! * `obs/<name>-<version>.json` — one crate's observations
//!   ([`model::CrateObservations`]), kept so a later build only observes
//!   crates it has not seen (or whose inputs changed) before mining again.
//! * `toolchain/crates/{std,core,alloc}-<rustc version>/` — shards of the
//!   local `rust-src` ([`toolchain`]).
//!
//! Built only by `codegraph deps beliefs build` (CLI; `--background` for a
//! detached, single-builder run) within a crate and a time budget, from
//! shards of cached sources — never from the network, never from MCP or
//! the prompt hook. Readers ([`load`]) open nothing for writing.

pub mod build;
pub mod model;
pub mod population;
pub mod toolchain;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use self::model::{BELIEFS_FORMAT, BeliefSet, CrateObservations, OBSERVE_VERSION};
use super::DepsHome;
use crate::resolution::external::ReachableGraph;

/// The artifact's file name.
pub const BELIEFS_FILE: &str = "beliefs.json";

/// `deps/beliefs/` of a dependency store.
pub fn beliefs_dir(home: &DepsHome) -> PathBuf {
    home.root().join("beliefs")
}

/// The beliefs, with the toolchain shards they were mined against (to
/// resolve a project's `std` calls the same way).
#[derive(Debug, Clone)]
pub struct LoadedBeliefs {
    pub set: BeliefSet,
    pub toolchain: Vec<ReachableGraph>,
    pub path: PathBuf,
}

/// The beliefs of `home`, read-only: `None` when none were built (or they
/// were built in another format).
pub fn load(home: &DepsHome) -> Option<LoadedBeliefs> {
    let dir = beliefs_dir(home);
    let path = dir.join(BELIEFS_FILE);
    let set: BeliefSet = serde_json::from_slice(&fs::read(&path).ok()?).ok()?;
    if set.format != BELIEFS_FORMAT {
        return None;
    }
    let toolchain = set
        .toolchain
        .as_deref()
        .and_then(|rustc| rustc.split_whitespace().nth(1))
        .map(|version| toolchain::existing(&toolchain::toolchain_home(&dir), version))
        .unwrap_or_default();
    Some(LoadedBeliefs {
        set,
        toolchain,
        path,
    })
}

pub use crate::analyze::bugs::ecosystem::mine::{ApiProfile, MineOptions};

/// What the cached observations say about `api`, strong belief or not.
pub fn profile(home: &DepsHome, api: &str) -> Option<ApiProfile> {
    crate::analyze::bugs::ecosystem::mine::profile(
        &cached_observations(home),
        api,
        &MineOptions::default(),
    )
}

/// Every current observation file under `obs/`, name-ordered.
pub fn cached_observations(home: &DepsHome) -> Vec<CrateObservations> {
    let dir = beliefs_dir(home).join("obs");
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| serde_json::from_slice::<CrateObservations>(&fs::read(path).ok()?).ok())
        .filter(|obs| obs.version_of_observer == OBSERVE_VERSION)
        .collect()
}

/// Start `codegraph deps beliefs build --background` with `args` detached
/// (one builder at a time: a running build keeps the lock and the new one
/// exits). Never from MCP or the prompt hook; `CODEGRAPH_NO_BACKGROUND_SYNC=1`
/// refuses.
pub fn spawn_background_build(args: &[String]) -> crate::sync::background::BackgroundSync {
    use std::process::{Command, Stdio};

    use crate::sync::background::{BackgroundSync, background_disabled, cli_binary};
    if background_disabled() {
        return BackgroundSync::Disabled;
    }
    let Some(exe) = cli_binary() else {
        return BackgroundSync::Unavailable;
    };
    let mut command = Command::new(exe);
    command
        .args(["deps", "beliefs", "build", "--background"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    if command.spawn().is_ok() {
        BackgroundSync::Started
    } else {
        BackgroundSync::Unavailable
    }
}

/// The cached observations of `name@version` when made by this observer
/// with inputs fingerprinted `fingerprint`.
pub fn read_observations(
    dir: &Path,
    name: &str,
    version: &str,
    fingerprint: Option<&str>,
) -> Option<CrateObservations> {
    let path = observations_path(dir, name, version);
    let obs: CrateObservations = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    (obs.version_of_observer == OBSERVE_VERSION
        && fingerprint.is_none_or(|fingerprint| obs.fingerprint == fingerprint))
    .then_some(obs)
}

pub fn observations_path(dir: &Path, name: &str, version: &str) -> PathBuf {
    dir.join("obs").join(format!("{name}-{version}.json"))
}

/// Write `value` as JSON to `path`: a temp file (0600) renamed into place,
/// parent directories created 0700.
pub fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    create_private_dir(parent)?;
    let tmp = parent.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        path.file_name().and_then(|n| n.to_str()).unwrap_or("out")
    ));
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(&serde_json::to_vec(value).map_err(std::io::Error::other)?)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)
}

/// `dir` and its missing parents, private to the user.
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}
