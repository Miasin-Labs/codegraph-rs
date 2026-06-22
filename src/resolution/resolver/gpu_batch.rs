use std::collections::HashMap;

use super::ReferenceResolver;
use crate::resolution::types::UnresolvedRef;
use crate::types::Node;

pub(super) type GpuHints = (
    Option<Vec<u8>>,
    Option<HashMap<usize, Option<Node>>>,
    Option<HashMap<usize, Option<(Node, bool)>>>,
    Option<HashMap<usize, Option<(Node, bool)>>>,
);

#[cfg(feature = "gpu")]
pub(super) fn precompute(resolver: &ReferenceResolver, refs: &[UnresolvedRef]) -> GpuHints {
    let joiner = {
        let guard = resolver.context.known_names.borrow();
        guard.as_ref().and_then(|known| {
            let names: Vec<&str> = known.iter().map(|s| s.as_str()).collect();
            super::super::gpu::GpuNameJoiner::new(&names)
        })
    };
    match joiner {
        None => (None, None, None, None),
        Some(joiner) => {
            let ref_names: Vec<&str> = refs.iter().map(|r| r.reference_name.as_str()).collect();
            let hints = joiner.probe_batch(&ref_names);
            let ranked = resolver.gpu_rank_exact_name(&joiner, refs, hints.as_deref());
            let s12 = resolver.gpu_match_s12(&joiner, refs, hints.as_deref());
            let fuzzy = resolver.gpu_fuzzy(&joiner, refs, hints.as_deref());
            (hints, ranked, s12, fuzzy)
        }
    }
}

#[cfg(not(feature = "gpu"))]
pub(super) fn precompute(_resolver: &ReferenceResolver, _refs: &[UnresolvedRef]) -> GpuHints {
    (None, None, None, None)
}
