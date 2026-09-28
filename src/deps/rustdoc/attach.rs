//! A shard build's API step: find the crate's rustdoc JSON, index it,
//! reconcile the shard's nodes with it, and write the indexes into the
//! unpublished shard directory — or record why not (the shard then serves
//! as before, from tree-sitter alone).

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::from_raw::build_index;
use super::model::ApiIndex;
use super::reconcile::{self, Reconciled};
use super::source::{self, TOOLCHAIN_CRATES};
use super::store::{self, ApiMeta};
use super::{API_FORMAT, read_crate};
use crate::db::{Db, QueryBuilder};
use crate::deps::model::{DepKey, Ecosystem};
use crate::deps::store::{DepsHome, TMP_PREFIX};
use crate::resolution::external::manifest::LibTarget;

/// `ApiMeta::origin` of indexes read from the toolchain's component.
pub const ORIGIN_TOOLCHAIN: &str = "toolchain";
/// `ApiMeta::origin` of indexes from `cargo rustdoc`.
pub const ORIGIN_CARGO: &str = "cargo-rustdoc";

/// `ApiMeta::origin` of indexes read from `CODEGRAPH_DEPS_RUSTDOC_JSON_DIR`.
pub const ORIGIN_PREBUILT: &str = "prebuilt";

/// Where this shard's JSON would come from now (`None`: nowhere).
pub fn available(key: &DepKey, source_dir: &Path) -> Option<&'static str> {
    match key.ecosystem {
        Ecosystem::Rust => source::toolchain_json(source_dir, &key.version)
            .is_ok()
            .then_some(ORIGIN_TOOLCHAIN),
        Ecosystem::Crates if source::prebuilt_json_dir().is_some() => Some(ORIGIN_PREBUILT),
        Ecosystem::Crates if source::crate_rustdoc_enabled() => Some(ORIGIN_CARGO),
        _ => None,
    }
}

/// The shard's API indexes need no rebuild: none can be had now, or they
/// were built (or tried) from that source with this format.
pub fn up_to_date(existing: &ApiMeta, key: &DepKey, source_dir: &Path) -> bool {
    match available(key, source_dir) {
        None => true,
        Some(origin) => existing.format == API_FORMAT && existing.origin == origin,
    }
}

/// Index the rustdoc JSON of the shard being built in `tmp` (database
/// `db_path`, sources `source_dir`) within `time_left`.
pub fn attach(
    home: &DepsHome,
    key: &DepKey,
    source_dir: &Path,
    tmp: &Path,
    db_path: &Path,
    time_left: Duration,
) -> ApiMeta {
    let Some(origin) = available(key, source_dir) else {
        return ApiMeta::default();
    };
    let mut meta = ApiMeta {
        format: API_FORMAT,
        origin: origin.to_string(),
        ..ApiMeta::default()
    };
    let indexes = match origin {
        ORIGIN_TOOLCHAIN => toolchain_indexes(key, source_dir),
        _ => crate_index(home, key, source_dir, time_left).map(|index| vec![index]),
    };
    let indexes = match indexes {
        Ok(indexes) => indexes,
        Err(error) => {
            meta.error = Some(error);
            return meta;
        }
    };
    let toolchain = origin == ORIGIN_TOOLCHAIN;
    // The index's files that are this shard's (as its nodes record them).
    let own_file = move |file: &str| -> Option<String> {
        if file.is_empty() || file.starts_with('@') {
            return None;
        }
        let in_crate = !toolchain
            || TOOLCHAIN_CRATES.iter().any(|krate| {
                file.strip_prefix(krate)
                    .is_some_and(|rest| rest.starts_with("/src/"))
            });
        in_crate.then(|| file.to_string())
    };
    match reconcile_db(db_path, source_dir, &indexes, &own_file) {
        Ok(done) => {
            meta.renamed = done.renamed;
            meta.added = done.added;
        }
        Err(error) => {
            meta.error = Some(format!("reconcile: {error}"));
            return meta;
        }
    }
    for index in &indexes {
        if let Err(error) = store::write(tmp, index) {
            meta.error = Some(format!("write index: {error}"));
            return meta;
        }
        meta.rustdoc_format = index.rustdoc_format;
        meta.crates.push(index.krate.clone());
    }
    meta
}

fn reconcile_db(
    db_path: &Path,
    source_dir: &Path,
    indexes: &[ApiIndex],
    own_file: &dyn Fn(&str) -> Option<String>,
) -> crate::error::Result<Reconciled> {
    let conn = rusqlite::Connection::open(db_path)?;
    let queries = QueryBuilder::new(Db::new(conn));
    let refs: Vec<&ApiIndex> = indexes.iter().collect();
    reconcile::reconcile(&queries, source_dir, &refs, own_file)
}

/// `std`, `core` and `alloc` from the toolchain's own JSON, each keeping
/// the items the shard holds files for (and every type and trait).
fn toolchain_indexes(key: &DepKey, library: &Path) -> Result<Vec<ApiIndex>, String> {
    let files = source::toolchain_json(library, &key.version)?;
    let mut indexes = Vec::new();
    for (krate, path) in files {
        let raw = read_crate(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut index = build_index(&raw, &krate, &|file| source::toolchain_span_file(file));
        drop(raw);
        index.retain_items(|item| {
            TOOLCHAIN_CRATES.iter().any(|krate| {
                item.file
                    .strip_prefix(krate)
                    .is_some_and(|rest| rest.starts_with("/src/"))
            })
        });
        indexes.push(index);
    }
    Ok(indexes)
}

/// The crate's index from `cargo rustdoc` (see [`source::crate_json`]).
fn crate_index(
    home: &DepsHome,
    key: &DepKey,
    source_dir: &Path,
    time_left: Duration,
) -> Result<ApiIndex, String> {
    let lib = LibTarget::read(source_dir, &key.name);
    let index_of = |json: &Path| -> Result<ApiIndex, String> {
        let raw = read_crate(json).map_err(|e| e.to_string())?;
        Ok(build_index(&raw, &lib.name, &|file| {
            source::span_file(source_dir, file)
        }))
    };
    if let Some(dir) = source::prebuilt_json_dir() {
        let json = dir.join(format!("{}.json", lib.name));
        if !json.is_file() {
            return Err(format!("{} missing", json.display()));
        }
        return index_of(&json);
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let work = home.ecosystem_dir(Ecosystem::Crates).join(format!(
        "{TMP_PREFIX}rustdoc-{}-{}-{nanos}",
        key.dir_name(),
        std::process::id()
    ));
    let target = home.root().join(source::RUSTDOC_TARGET_DIR);
    let result = (|| {
        std::fs::create_dir_all(&work).map_err(|e| format!("work dir: {e}"))?;
        let json = source::crate_json(&source::CrateRequest {
            package: &key.name,
            version: key.version.split('+').next().unwrap_or(&key.version),
            lib_name: &lib.name,
            source_dir,
            work_dir: &work,
            target_dir: &target,
            deadline: source::crate_rustdoc_deadline().min(time_left),
        })?;
        index_of(&json)
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}
