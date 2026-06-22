use std::collections::HashMap;

use super::ReferenceResolver;
use crate::resolution::types::{ResolutionContext, UnresolvedRef};
use crate::types::{Language, Node, NodeKind};

impl ReferenceResolver {
    pub(super) fn gpu_fuzzy(
        &self,
        joiner: &super::super::gpu::GpuNameJoiner,
        refs: &[UnresolvedRef],
        prefilter: Option<&[u8]>,
    ) -> Option<HashMap<usize, Option<(Node, bool)>>> {
        fn kind_class(kind: NodeKind) -> u8 {
            match kind {
                NodeKind::Function => 1,
                NodeKind::Method => 2,
                NodeKind::Class => 3,
                _ => 0,
            }
        }
        let mut lang_ids: HashMap<Language, u8> = HashMap::new();
        let mut intern_lang = |language: Language| -> u8 {
            let next = lang_ids.len() as u8;
            *lang_ids.entry(language).or_insert(next)
        };
        let mut groups: HashMap<String, i32> = HashMap::new();
        let mut cand_starts: Vec<u32> = vec![0];
        let (mut cand_lang, mut cand_kind) = (Vec::new(), Vec::new());
        let mut group_nodes: Vec<Vec<Node>> = Vec::new();
        let (mut ref_group, mut ref_lang) = (
            Vec::with_capacity(refs.len()),
            Vec::with_capacity(refs.len()),
        );
        for (idx, reference) in refs.iter().enumerate() {
            if prefilter.is_some_and(|flags| flags[idx] == 0) {
                ref_group.push(-1);
                ref_lang.push(0);
                continue;
            }
            let lower = reference.reference_name.to_lowercase();
            let group = *groups.entry(lower.clone()).or_insert_with(|| {
                let candidates = self.context.get_nodes_by_lower_name(&lower);
                if candidates.is_empty() {
                    -1
                } else {
                    for candidate in &candidates {
                        cand_lang.push(intern_lang(candidate.language));
                        cand_kind.push(kind_class(candidate.kind));
                    }
                    cand_starts.push(cand_lang.len() as u32);
                    group_nodes.push(candidates);
                    (group_nodes.len() - 1) as i32
                }
            });
            ref_group.push(group);
            ref_lang.push(intern_lang(reference.language));
        }
        if group_nodes.is_empty() {
            return Some(HashMap::new());
        }
        let (best, cross) =
            joiner.fuzzy_unique(&ref_group, &ref_lang, &cand_starts, &cand_lang, &cand_kind)?;
        let mut out = HashMap::new();
        for (idx, &group) in ref_group.iter().enumerate() {
            if group < 0 {
                continue;
            }
            let winner = if best[idx] < 0 {
                None
            } else {
                let local = (best[idx] as u32 - cand_starts[group as usize]) as usize;
                Some((group_nodes[group as usize][local].clone(), cross[idx] != 0))
            };
            out.insert(idx, winner);
        }
        Some(out)
    }
}
