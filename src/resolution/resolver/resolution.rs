#[cfg(feature = "gpu")]
use std::collections::HashMap;

#[cfg(feature = "gpu")]
use super::gpu_batch;
use super::{ReferenceResolver, engine};
use crate::error::Result;
#[cfg(feature = "gpu")]
use crate::resolution::types::ResolutionStats;
use crate::resolution::types::{ResolutionResult, ResolvedRef, UnresolvedRef};
use crate::types::UnresolvedReference;

impl ReferenceResolver {
    #[cfg(not(feature = "gpu"))]
    pub async fn resolve_all(
        &self,
        unresolved_refs: &[UnresolvedReference],
        on_progress: Option<&mut dyn FnMut(usize, usize)>,
    ) -> Result<ResolutionResult> {
        self.resolve_all_parallel(unresolved_refs, on_progress)
            .await
    }

    /// Resolve all unresolved references
    #[cfg(feature = "gpu")]
    pub async fn resolve_all(
        &self,
        unresolved_refs: &[UnresolvedReference],
        mut on_progress: Option<&mut dyn FnMut(usize, usize)>,
    ) -> Result<ResolutionResult> {
        self.warm_caches();

        let mut resolved: Vec<ResolvedRef> = Vec::new();
        let mut unresolved: Vec<UnresolvedRef> = Vec::new();
        let mut by_method: HashMap<String, usize> = HashMap::new();

        let refs: Vec<UnresolvedRef> = unresolved_refs
            .iter()
            .map(|r| UnresolvedRef {
                from_node_id: r.from_node_id.clone(),
                reference_name: r.reference_name.clone(),
                reference_kind: r.reference_kind,
                line: r.line,
                column: r.column,
                file_path: match &r.file_path {
                    Some(path) if !path.is_empty() => path.clone(),
                    _ => self.get_file_path_from_node_id(&r.from_node_id),
                },
                language: match r.language {
                    Some(language) => language,
                    None => self.get_language_from_node_id(&r.from_node_id),
                },
                candidates: r.candidates.clone(),
                metadata: r.metadata.clone(),
            })
            .collect();

        let total = refs.len();
        let mut last_reported_percent: i64 = -1;
        let (gpu_hints, gpu_ranked, gpu_s12, gpu_fuzzy) = gpu_batch::precompute(self, &refs);

        for (i, r) in refs.iter().enumerate() {
            let hint = gpu_hints.as_ref().map(|h| h[i]).and_then(|f| match f {
                1 => Some(true),
                0 => Some(false),
                _ => None,
            });
            let ranked_hint = gpu_ranked
                .as_ref()
                .and_then(|m| m.get(&i))
                .map(|winner| winner.as_ref());
            let s12_hint = gpu_s12
                .as_ref()
                .and_then(|m| m.get(&i))
                .map(|w| w.as_ref().map(|(n, s1)| (n, *s1)));
            let fuzzy_hint = gpu_fuzzy
                .as_ref()
                .and_then(|m| m.get(&i))
                .map(|w| w.as_ref().map(|(n, x)| (n, *x)));
            let result = self.resolve_one_hinted(r, hint, ranked_hint, s12_hint, fuzzy_hint);

            if let Some(result) = result {
                *by_method
                    .entry(result.resolved_by.as_str().to_string())
                    .or_insert(0) += 1;
                resolved.push(result);
            } else {
                unresolved.push(r.clone());
            }

            if let Some(cb) = on_progress.as_deref_mut() {
                let current_percent = ((i as f64 / total as f64) * 100.0).floor() as i64;
                if current_percent > last_reported_percent {
                    last_reported_percent = current_percent;
                    cb(i + 1, total);
                }
            }
        }

        if total > 0 {
            if let Some(cb) = on_progress {
                cb(total, total);
            }
        }

        Ok(ResolutionResult {
            stats: ResolutionStats {
                total,
                resolved: resolved.len(),
                unresolved: unresolved.len(),
                by_method,
            },
            resolved,
            unresolved,
        })
    }

    pub fn resolve_one(&self, r: &UnresolvedRef) -> Option<ResolvedRef> {
        self.resolve_one_hinted(r, None, None, None, None)
    }

    /// `resolve_one` with an optional precomputed `has_any_possible_match`
    /// verdict. The GPU batch pre-filter (feature `gpu`) probes every
    /// reference name in one kernel launch and feeds the verdicts through
    /// here; `None` falls back to the CPU check (also used for the rare
    /// names whose capitalization semantics the kernel defers).
    pub fn resolve_one_hinted(
        &self,
        r: &UnresolvedRef,
        known_hint: Option<bool>,
        ranked: Option<Option<&crate::types::Node>>,
        s12: Option<Option<(&crate::types::Node, bool)>>,
        fuzzy: Option<Option<(&crate::types::Node, bool)>>,
    ) -> Option<ResolvedRef> {
        let frameworks = self.frameworks.borrow();
        engine::resolve_one(
            r,
            &self.context,
            frameworks.as_slice(),
            self,
            known_hint,
            ranked,
            s12,
            fuzzy,
        )
    }
}
