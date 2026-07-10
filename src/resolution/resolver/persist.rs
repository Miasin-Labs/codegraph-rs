use super::super::callback_synthesizer::synthesize_callback_edges;
use super::ReferenceResolver;
#[cfg(not(feature = "gpu"))]
use super::snapshot::ResolverSnapshot;
use crate::db::ResolvedRefKey;
use crate::error::Result;
use crate::resolution::types::{ResolutionResult, ResolutionStats};

impl ReferenceResolver {
    /// Resolve and persist edges to database
    pub async fn resolve_and_persist(
        &self,
        unresolved_refs: &[crate::types::UnresolvedReference],
        on_progress: Option<&mut dyn FnMut(usize, usize)>,
    ) -> Result<ResolutionResult> {
        let result = self.resolve_all(unresolved_refs, on_progress).await?;
        let edges = self.create_edges(&result.resolved);
        if !edges.is_empty() {
            self.context.queries.insert_edges(&edges)?;
        }
        if !result.resolved.is_empty() {
            let keys: Vec<ResolvedRefKey> = result
                .resolved
                .iter()
                .map(|resolved_ref| ResolvedRefKey {
                    from_node_id: resolved_ref.original.from_node_id.clone(),
                    reference_name: resolved_ref.original.reference_name.clone(),
                    reference_kind: resolved_ref.original.reference_kind.as_str().to_string(),
                })
                .collect();
            self.context
                .queries
                .delete_specific_resolved_references(&keys)?;
        }

        Ok(result)
    }

    /// Resolve and persist in batches to keep memory bounded.
    /// Processes unresolved references in chunks, persisting edges and cleaning
    /// up resolved refs after each batch to avoid accumulating large arrays.
    ///
    /// `batch_size: None` uses the TS default of 5000.
    pub async fn resolve_and_persist_batched(
        &self,
        mut on_progress: Option<&mut dyn FnMut(usize, usize)>,
        batch_size: Option<usize>,
    ) -> Result<ResolutionResult> {
        let batch_size = batch_size.unwrap_or(5000);
        let total = self.context.queries.get_unresolved_references_count()? as usize;
        #[cfg(not(feature = "gpu"))]
        let snapshot = if total > 0 {
            Some(ResolverSnapshot::build(
                &self.context.project_root,
                &self.context.queries,
            )?)
        } else {
            None
        };
        let mut processed = 0usize;
        let mut aggregate_stats = ResolutionStats::default();
        let mut last_seen_ref_id: i64 = 0;

        loop {
            let batch_page = self
                .context
                .queries
                .get_unresolved_references_batch_after_id(last_seen_ref_id, batch_size)?;
            let batch = batch_page.refs;
            if batch.is_empty() {
                break;
            }
            last_seen_ref_id = batch_page.last_id;

            #[cfg(feature = "gpu")]
            let result = self.resolve_all(&batch, None).await?;
            #[cfg(not(feature = "gpu"))]
            let result = match snapshot.as_ref() {
                Some(snapshot) => self.resolve_snapshot_batch(&batch, snapshot, None).await?,
                None => self.resolve_all(&batch, None).await?,
            };

            let edges = self.create_edges(&result.resolved);
            if !edges.is_empty() {
                self.context.queries.insert_edges(&edges)?;
            }
            if !result.resolved.is_empty() {
                let keys: Vec<ResolvedRefKey> = result
                    .resolved
                    .iter()
                    .map(|resolved_ref| ResolvedRefKey {
                        from_node_id: resolved_ref.original.from_node_id.clone(),
                        reference_name: resolved_ref.original.reference_name.clone(),
                        reference_kind: resolved_ref.original.reference_kind.as_str().to_string(),
                    })
                    .collect();
                self.context
                    .queries
                    .delete_specific_resolved_references(&keys)?;
            }
            aggregate_stats.total += result.stats.total;
            aggregate_stats.resolved += result.stats.resolved;
            aggregate_stats.unresolved += result.stats.unresolved;
            for (method, count) in result.stats.by_method {
                *aggregate_stats.by_method.entry(method).or_insert(0) += count;
            }
            processed += batch.len();
            if let Some(cb) = on_progress.as_deref_mut() {
                cb(processed, total);
            }
        }

        if let Ok(count) = synthesize_callback_edges(&self.context.queries, &self.context) {
            aggregate_stats
                .by_method
                .insert("callback-synthesis".to_string(), count);
        }

        Ok(ResolutionResult {
            resolved: Vec::new(),
            unresolved: Vec::new(),
            stats: aggregate_stats,
        })
    }
}
