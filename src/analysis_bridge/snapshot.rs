use std::collections::HashMap;
use std::path::Path;

use codegraph_analysis::graph::CodeGraph as AnalysisGraph;
use codegraph_analysis::nodes::NodeId as ANodeId;
use codegraph_analysis::overlay::load_snapshot_bincode;

use crate::analysis_bridge::cache::{
    CACHE_META_FILE,
    COMPLEXITY_SIDECAR_FILE,
    GRAPH_SNAPSHOT_FILE,
    PREV_SUFFIX,
    analysis_cache_dir,
    read_cache_meta,
};
use crate::analysis_bridge::sidecar::{StoredComplexity, load_complexity_sidecar};
use crate::error::Result;

/// Which snapshot generation served as the diff base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseGeneration {
    /// The current cache generation.
    Current,
    /// The rotated `.prev` generation.
    Previous,
    /// An explicit snapshot path supplied via `--base <path>`.
    Explicit,
}

impl BaseGeneration {
    pub fn as_str(self) -> &'static str {
        match self {
            BaseGeneration::Current => "cache",
            BaseGeneration::Previous => "cache-prev",
            BaseGeneration::Explicit => "file",
        }
    }
}

/// A base snapshot resolved for `analyze diff`.
pub struct BaseSnapshot {
    pub graph: AnalysisGraph,
    /// Fingerprint of the index state the base was bridged from, when known
    /// (`None` for an explicit bare `graph.bin` without a meta envelope).
    pub index_fingerprint: Option<u64>,
    pub generation: BaseGeneration,
    /// Per-function complexity captured for the base generation by a prior
    /// `analyze diff` run. Empty when no valid sidecar exists.
    pub complexity: HashMap<ANodeId, StoredComplexity>,
}

fn load_base_generation(
    cache_dir: &Path,
    suffix: &str,
    generation: BaseGeneration,
    current_fingerprint: u64,
) -> Option<BaseSnapshot> {
    let meta = read_cache_meta(&cache_dir.join(format!("{CACHE_META_FILE}{suffix}")))?;
    if meta.index_fingerprint == current_fingerprint {
        return None;
    }
    let loaded =
        load_snapshot_bincode(&cache_dir.join(format!("{GRAPH_SNAPSHOT_FILE}{suffix}"))).ok()?;
    let complexity = load_complexity_sidecar(
        &cache_dir.join(format!("{COMPLEXITY_SIDECAR_FILE}{suffix}")),
        meta.index_fingerprint,
    )
    .unwrap_or_default();
    Some(BaseSnapshot {
        graph: loaded.graph,
        index_fingerprint: Some(meta.index_fingerprint),
        generation,
        complexity,
    })
}

pub fn load_auto_base_snapshot(
    project_root: &Path,
    current_fingerprint: u64,
) -> Option<BaseSnapshot> {
    let cache_dir = analysis_cache_dir(project_root);
    load_base_generation(&cache_dir, "", BaseGeneration::Current, current_fingerprint).or_else(
        || {
            load_base_generation(
                &cache_dir,
                PREV_SUFFIX,
                BaseGeneration::Previous,
                current_fingerprint,
            )
        },
    )
}

pub fn load_explicit_base_snapshot(path: &Path) -> Result<BaseSnapshot> {
    let (graph_path, dir) = if path.is_dir() {
        (path.join(GRAPH_SNAPSHOT_FILE), Some(path))
    } else {
        (path.to_path_buf(), None)
    };
    let loaded = load_snapshot_bincode(&graph_path)
        .map_err(|e| crate::error::CodeGraphError::other(e.to_string()))?;
    let meta = dir.and_then(|d| read_cache_meta(&d.join(CACHE_META_FILE)));
    let index_fingerprint = meta.as_ref().map(|m| m.index_fingerprint);
    let complexity = match (dir, index_fingerprint) {
        (Some(d), Some(fp)) => {
            load_complexity_sidecar(&d.join(COMPLEXITY_SIDECAR_FILE), fp).unwrap_or_default()
        }
        _ => HashMap::new(),
    };
    Ok(BaseSnapshot {
        graph: loaded.graph,
        index_fingerprint,
        generation: BaseGeneration::Explicit,
        complexity,
    })
}
