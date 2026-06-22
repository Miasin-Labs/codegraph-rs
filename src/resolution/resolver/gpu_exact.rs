use std::collections::HashMap;

use super::ReferenceResolver;
use crate::resolution::types::{ResolutionContext, UnresolvedRef};
use crate::types::{EdgeKind, Language, Node, NodeKind};

impl ReferenceResolver {
    pub(super) fn gpu_rank_exact_name(
        &self,
        joiner: &super::super::gpu::GpuNameJoiner,
        refs: &[UnresolvedRef],
        prefilter: Option<&[u8]>,
    ) -> Option<HashMap<usize, Option<Node>>> {
        fn kind_class(kind: NodeKind) -> u8 {
            match kind {
                NodeKind::Function => 1,
                NodeKind::Method => 2,
                NodeKind::Class => 3,
                NodeKind::Struct => 4,
                NodeKind::Interface => 5,
                _ => 0,
            }
        }
        fn ref_kind_class(kind: EdgeKind) -> u8 {
            match kind {
                EdgeKind::Calls => 1,
                EdgeKind::Instantiates => 2,
                EdgeKind::Decorates => 3,
                _ => 0,
            }
        }
        fn chain_hash(previous: u64, segment: &str) -> u64 {
            let mut hash = previous ^ 0xcbf2_9ce4_8422_2325;
            for &byte in segment.as_bytes() {
                hash ^= byte as u64;
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
            hash
        }

        let mut file_ids: HashMap<String, u32> = HashMap::new();
        let mut dir_starts: Vec<u32> = vec![0];
        let mut dir_hashes: Vec<u64> = Vec::new();
        let mut intern_file =
            |path: &str, dir_starts: &mut Vec<u32>, dir_hashes: &mut Vec<u64>| -> u32 {
                if let Some(&id) = file_ids.get(path) {
                    return id;
                }
                let id = file_ids.len() as u32;
                file_ids.insert(path.to_string(), id);
                let mut segments: Vec<&str> = path.split('/').collect();
                segments.pop();
                let mut hash = 0u64;
                for segment in segments {
                    hash = chain_hash(hash, segment);
                    dir_hashes.push(hash);
                }
                dir_starts.push(dir_hashes.len() as u32);
                id
            };
        let mut lang_ids: HashMap<Language, u8> = HashMap::new();
        let mut intern_lang = |language: Language| -> u8 {
            let next = lang_ids.len() as u8;
            *lang_ids.entry(language).or_insert(next)
        };
        let mut groups: HashMap<&str, i32> = HashMap::new();
        let mut cand_starts: Vec<u32> = vec![0];
        let (mut cand_file, mut cand_lang, mut cand_kind, mut cand_exp, mut cand_line) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut group_nodes: Vec<Vec<Node>> = Vec::new();
        let mut ref_group: Vec<i32> = Vec::with_capacity(refs.len());
        let (mut ref_file, mut ref_lang, mut ref_kind, mut ref_line) = (
            Vec::with_capacity(refs.len()),
            Vec::with_capacity(refs.len()),
            Vec::with_capacity(refs.len()),
            Vec::with_capacity(refs.len()),
        );
        for (idx, reference) in refs.iter().enumerate() {
            if prefilter.is_some_and(|flags| flags[idx] == 0) {
                ref_group.push(-1);
                ref_file.push(0);
                ref_lang.push(0);
                ref_kind.push(0);
                ref_line.push(0);
                continue;
            }
            let group = *groups
                .entry(reference.reference_name.as_str())
                .or_insert_with(|| {
                    let candidates = self.context.get_nodes_by_name(&reference.reference_name);
                    if candidates.len() < 2 {
                        -1
                    } else {
                        for candidate in &candidates {
                            cand_file.push(intern_file(
                                &candidate.file_path,
                                &mut dir_starts,
                                &mut dir_hashes,
                            ));
                            cand_lang.push(intern_lang(candidate.language));
                            cand_kind.push(kind_class(candidate.kind));
                            cand_exp.push(u8::from(candidate.is_exported == Some(true)));
                            cand_line.push(candidate.start_line);
                        }
                        cand_starts.push(cand_file.len() as u32);
                        group_nodes.push(candidates);
                        (group_nodes.len() - 1) as i32
                    }
                });
            ref_group.push(group);
            ref_file.push(intern_file(
                &reference.file_path,
                &mut dir_starts,
                &mut dir_hashes,
            ));
            ref_lang.push(intern_lang(reference.language));
            ref_kind.push(ref_kind_class(reference.reference_kind));
            ref_line.push(reference.line);
        }
        if group_nodes.is_empty() {
            return Some(HashMap::new());
        }

        let best = joiner.score_batch(
            &ref_group,
            &ref_file,
            &ref_lang,
            &ref_kind,
            &ref_line,
            &cand_starts,
            &cand_file,
            &cand_lang,
            &cand_kind,
            &cand_exp,
            &cand_line,
            &dir_starts,
            &dir_hashes,
        )?;
        let mut out: HashMap<usize, Option<Node>> = HashMap::new();
        for (idx, (&group, &best_idx)) in ref_group.iter().zip(best.iter()).enumerate() {
            if group < 0 {
                continue;
            }
            let winner = if best_idx < 0 {
                None
            } else {
                let local = (best_idx as u32 - cand_starts[group as usize]) as usize;
                Some(group_nodes[group as usize][local].clone())
            };
            out.insert(idx, winner);
        }
        Some(out)
    }
}
