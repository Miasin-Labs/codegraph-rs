use std::sync::Mutex;

use ws_deque::scheduler;

use super::engine;
use super::snapshot::SnapshotContext;
use crate::resolution::types::{FrameworkResolver, ResolutionResult, ResolvedRef, UnresolvedRef};

fn worker_count_for(work_items: usize) -> usize {
    let available = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    work_items.clamp(1, available)
}

pub(super) fn resolve_all(
    refs: &[UnresolvedRef],
    context: &SnapshotContext,
    frameworks: &[Box<dyn FrameworkResolver>],
) -> ResolutionResult {
    let total = refs.len();
    if total == 0 {
        return engine::result_from_parts(0, Vec::new(), Vec::new());
    }
    if total == 1 {
        let resolved = engine::resolve_one(
            &refs[0], context, frameworks, context, None, None, None, None,
        );
        return match resolved {
            Some(result) => engine::result_from_parts(total, vec![result], Vec::new()),
            None => engine::result_from_parts(total, Vec::new(), vec![refs[0].clone()]),
        };
    }

    let slots: Vec<Mutex<Option<Option<ResolvedRef>>>> =
        (0..total).map(|_| Mutex::new(None)).collect();
    scheduler::run(worker_count_for(total), 0..total, |index, _| {
        let result = engine::resolve_one(
            &refs[index],
            context,
            frameworks,
            context,
            None,
            None,
            None,
            None,
        );
        let mut slot = slots[index]
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *slot = Some(result);
    });

    let mut resolved = Vec::new();
    let mut unresolved = Vec::new();
    for (index, slot) in slots.into_iter().enumerate() {
        let result = match slot.into_inner() {
            Ok(Some(result)) => result,
            Ok(None) => panic!("parallel resolver worker did not write a result"),
            Err(poisoned) => poisoned
                .into_inner()
                .unwrap_or_else(|| panic!("parallel resolver worker did not write a result")),
        };
        match result {
            Some(result) => resolved.push(result),
            None => unresolved.push(refs[index].clone()),
        }
    }

    engine::result_from_parts(total, resolved, unresolved)
}
