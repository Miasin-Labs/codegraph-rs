use std::sync::Arc;

use tokio::task::JoinSet;

use super::engine;
use super::snapshot::SnapshotContext;
use crate::error::{CodeGraphError, Result};
use crate::resolution::types::{FrameworkResolver, ResolutionResult, ResolvedRef, UnresolvedRef};

fn worker_count_for(work_items: usize) -> usize {
    let available = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    work_items.clamp(1, available)
}

fn chunk_size_for(work_items: usize, workers: usize) -> usize {
    work_items.div_ceil(workers.saturating_mul(4).max(1)).max(1)
}

pub(super) async fn resolve_all(
    refs: Vec<UnresolvedRef>,
    context: Arc<SnapshotContext>,
    frameworks: Arc<Vec<Box<dyn FrameworkResolver>>>,
    mut on_progress: Option<&mut dyn FnMut(usize, usize)>,
) -> Result<ResolutionResult> {
    let total = refs.len();
    if total == 0 {
        return Ok(engine::result_from_parts(0, Vec::new(), Vec::new()));
    }

    let refs = Arc::new(refs);
    let worker_limit = worker_count_for(total);
    let chunk_size = chunk_size_for(total, worker_limit);
    let mut tasks = JoinSet::new();
    let mut slots: Vec<Option<Option<ResolvedRef>>> = (0..total).map(|_| None).collect();
    let mut next = 0usize;
    let mut processed = 0usize;

    while next < total || !tasks.is_empty() {
        while next < total && tasks.len() < worker_limit {
            let start = next;
            let end = (start + chunk_size).min(total);
            let refs = Arc::clone(&refs);
            let context = Arc::clone(&context);
            let frameworks = Arc::clone(&frameworks);
            tasks.spawn_blocking(move || {
                let mut results = Vec::with_capacity(end - start);
                for index in start..end {
                    let resolved = engine::resolve_one(
                        &refs[index],
                        context.as_ref(),
                        frameworks.as_slice(),
                        context.as_ref(),
                        None,
                        None,
                        None,
                        None,
                    );
                    results.push((index, resolved));
                }
                results
            });
            next = end;
        }

        match tasks.join_next().await {
            Some(Ok(results)) => {
                processed += results.len();
                for (index, resolved) in results {
                    slots[index] = Some(resolved);
                }
                if let Some(callback) = on_progress.as_deref_mut() {
                    callback(processed, total);
                }
            }
            Some(Err(error)) => {
                while tasks.join_next().await.is_some() {}
                return Err(CodeGraphError::other(format!(
                    "Tokio resolver worker failed: {error}"
                )));
            }
            None => break,
        }
    }

    let mut resolved = Vec::new();
    let mut unresolved = Vec::new();
    for (index, result) in slots.into_iter().enumerate() {
        match result.ok_or_else(|| {
            CodeGraphError::other(format!("resolver worker did not return result {index}"))
        })? {
            Some(result) => resolved.push(result),
            None => unresolved.push(refs[index].clone()),
        }
    }

    Ok(engine::result_from_parts(total, resolved, unresolved))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EdgeKind, Language, Node, NodeKind};

    fn unresolved(from: &str, name: &str, line: u32) -> UnresolvedRef {
        UnresolvedRef {
            from_node_id: from.to_string(),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::References,
            line,
            column: 0,
            file_path: "src/caller.rs".to_string(),
            language: Language::Rust,
            candidates: None,
            metadata: None,
        }
    }

    #[test]
    fn chunking_is_bounded_and_non_empty() {
        assert_eq!(chunk_size_for(1, 1), 1);
        assert_eq!(chunk_size_for(16, 4), 1);
        assert_eq!(chunk_size_for(100, 4), 7);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn preserves_input_order_and_duplicate_references() {
        let nodes = vec![
            Node::new(
                "caller",
                NodeKind::Function,
                "caller",
                "crate::caller",
                "src/caller.rs",
                Language::Rust,
                1,
                10,
            ),
            Node::new(
                "alpha",
                NodeKind::Struct,
                "Alpha",
                "crate::Alpha",
                "src/types.rs",
                Language::Rust,
                1,
                2,
            ),
            Node::new(
                "beta",
                NodeKind::Struct,
                "Beta",
                "crate::Beta",
                "src/types.rs",
                Language::Rust,
                3,
                4,
            ),
        ];
        let context = SnapshotContext::from_nodes(
            "/missing",
            nodes,
            vec!["src/caller.rs".into(), "src/types.rs".into()],
        )
        .unwrap();
        let refs = vec![
            unresolved("caller", "Alpha", 3),
            unresolved("caller", "Beta", 4),
            unresolved("caller", "Alpha", 5),
        ];
        let frameworks: Arc<Vec<Box<dyn FrameworkResolver>>> = Arc::new(Vec::new());

        let result = resolve_all(refs, Arc::new(context), frameworks, None)
            .await
            .unwrap();

        assert_eq!(result.stats.resolved, 3);
        assert_eq!(
            result
                .resolved
                .iter()
                .map(|resolved| (resolved.target_node_id.as_str(), resolved.original.line))
                .collect::<Vec<_>>(),
            vec![("alpha", 3), ("beta", 4), ("alpha", 5)]
        );
    }
}
