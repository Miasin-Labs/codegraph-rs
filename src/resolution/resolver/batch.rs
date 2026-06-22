use super::ReferenceResolver;
#[cfg(not(feature = "gpu"))]
use super::cache::should_use_snapshot_resolution;
#[cfg(not(feature = "gpu"))]
use super::snapshot::{ResolverSnapshot, SnapshotContext};
use crate::error::Result;
use crate::resolution::types::ResolutionResult;
#[cfg(not(feature = "gpu"))]
use crate::resolution::types::UnresolvedRef;
#[cfg(not(feature = "gpu"))]
use crate::types::Language;
use crate::types::UnresolvedReference;

impl ReferenceResolver {
    #[cfg(not(feature = "gpu"))]
    fn materialize_refs_with_snapshot(
        &self,
        unresolved_refs: &[UnresolvedReference],
        snapshot: &SnapshotContext,
    ) -> Vec<UnresolvedRef> {
        unresolved_refs
            .iter()
            .map(|r| UnresolvedRef {
                from_node_id: r.from_node_id.clone(),
                reference_name: r.reference_name.clone(),
                reference_kind: r.reference_kind,
                line: r.line,
                column: r.column,
                file_path: match &r.file_path {
                    Some(path) if !path.is_empty() => path.clone(),
                    _ => snapshot
                        .get_node_by_id(&r.from_node_id)
                        .map(|node| node.file_path.clone())
                        .unwrap_or_default(),
                },
                language: match r.language {
                    Some(language) => language,
                    None => snapshot
                        .get_node_by_id(&r.from_node_id)
                        .map(|node| node.language)
                        .unwrap_or(Language::Unknown),
                },
                candidates: None,
            })
            .collect()
    }

    #[cfg(not(feature = "gpu"))]
    pub(super) fn resolve_snapshot_batch(
        &self,
        unresolved_refs: &[UnresolvedReference],
        snapshot: &ResolverSnapshot,
    ) -> ResolutionResult {
        let refs = self.materialize_refs_with_snapshot(unresolved_refs, snapshot.context());
        super::parallel::resolve_all(&refs, snapshot.context(), &self.frameworks)
    }

    /// Resolve over an immutable in-memory snapshot. This is the benchmarkable
    /// CPU path used by full-index batched resolution; persistence still happens
    /// serially on the SQLite connection.
    pub fn resolve_all_parallel(
        &self,
        unresolved_refs: &[UnresolvedReference],
        mut on_progress: Option<&mut dyn FnMut(usize, usize)>,
    ) -> Result<ResolutionResult> {
        #[cfg(feature = "gpu")]
        {
            let result = self.resolve_all(unresolved_refs, None);
            if let Some(cb) = on_progress.as_deref_mut() {
                cb(result.stats.total, result.stats.total);
            }
            return Ok(result);
        }

        #[cfg(not(feature = "gpu"))]
        {
            if !should_use_snapshot_resolution(unresolved_refs.len()) {
                return Ok(self.resolve_all(unresolved_refs, on_progress));
            }

            let snapshot =
                ResolverSnapshot::build(&self.context.project_root, &self.context.queries)?;
            let result = self.resolve_snapshot_batch(unresolved_refs, &snapshot);
            if let Some(cb) = on_progress.as_deref_mut() {
                cb(result.stats.total, result.stats.total);
            }
            Ok(result)
        }
    }
}
