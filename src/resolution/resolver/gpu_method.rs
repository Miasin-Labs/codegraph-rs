use std::collections::HashMap;

use super::ReferenceResolver;
use crate::resolution::types::{ResolutionContext, UnresolvedRef};
use crate::types::{Node, NodeKind};

impl ReferenceResolver {
    pub(super) fn gpu_match_s12(
        &self,
        joiner: &super::super::gpu::GpuNameJoiner,
        refs: &[UnresolvedRef],
        prefilter: Option<&[u8]>,
    ) -> Option<HashMap<usize, Option<(Node, bool)>>> {
        use super::super::name_matcher::{capitalize_first_shared, split_method_call};

        fn fnv(name: &str) -> u64 {
            let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
            for &byte in name.as_bytes() {
                hash ^= byte as u64;
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
            hash
        }

        let mut file_ids: HashMap<String, u32> = HashMap::new();
        let mut file_starts: Vec<u32> = vec![0];
        let (mut method_hash, mut method_qn_off, mut method_qn_len) =
            (Vec::new(), Vec::new(), Vec::new());
        let mut qn_buf: Vec<u8> = Vec::new();
        let mut method_nodes: Vec<Node> = Vec::new();
        let mut ref_cand_starts: Vec<u32> = vec![0];
        let (mut cls_file, mut cls_name_off, mut cls_name_len) =
            (Vec::new(), Vec::new(), Vec::new());
        let mut name_buf: Vec<u8> = Vec::new();
        let mut ref_method_hash: Vec<u64> = Vec::new();
        let mut ref_idx_map: Vec<usize> = Vec::new();
        let mut s1_boundary: Vec<u32> = Vec::new();

        for (idx, reference) in refs.iter().enumerate() {
            if prefilter.is_some_and(|flags| flags[idx] == 0) {
                continue;
            }
            let Some((object, method)) = split_method_call(&reference.reference_name) else {
                continue;
            };
            let mut push_classes = |name: &str,
                                    cls_file: &mut Vec<u32>,
                                    cls_name_off: &mut Vec<u32>,
                                    cls_name_len: &mut Vec<u32>,
                                    name_buf: &mut Vec<u8>| {
                for candidate in self.context.get_nodes_by_name(name) {
                    if !(candidate.kind == NodeKind::Class
                        || candidate.kind == NodeKind::Struct
                        || candidate.kind == NodeKind::Interface)
                        || candidate.language != reference.language
                    {
                        continue;
                    }
                    let file_id = match file_ids.get(&candidate.file_path) {
                        Some(&id) => id,
                        None => {
                            let id = file_ids.len() as u32;
                            file_ids.insert(candidate.file_path.clone(), id);
                            for node in self.context.get_nodes_in_file(&candidate.file_path) {
                                if node.kind == NodeKind::Method {
                                    method_hash.push(fnv(&node.name));
                                    method_qn_off.push(qn_buf.len() as u32);
                                    method_qn_len.push(node.qualified_name.len() as u32);
                                    qn_buf.extend_from_slice(node.qualified_name.as_bytes());
                                    method_nodes.push(node);
                                }
                            }
                            file_starts.push(method_hash.len() as u32);
                            id
                        }
                    };
                    cls_file.push(file_id);
                    cls_name_off.push(name_buf.len() as u32);
                    cls_name_len.push(candidate.name.len() as u32);
                    name_buf.extend_from_slice(candidate.name.as_bytes());
                }
            };
            push_classes(
                object,
                &mut cls_file,
                &mut cls_name_off,
                &mut cls_name_len,
                &mut name_buf,
            );
            let boundary = cls_file.len() as u32;
            let capitalized = capitalize_first_shared(object);
            if capitalized != object {
                push_classes(
                    &capitalized,
                    &mut cls_file,
                    &mut cls_name_off,
                    &mut cls_name_len,
                    &mut name_buf,
                );
            }
            if cls_file.len() as u32 == *ref_cand_starts.last().unwrap_or(&0) {
                continue;
            }
            ref_cand_starts.push(cls_file.len() as u32);
            ref_method_hash.push(fnv(method));
            ref_idx_map.push(idx);
            s1_boundary.push(boundary);
        }
        if ref_method_hash.is_empty() {
            return Some(HashMap::new());
        }

        let (best_method, best_class) = joiner.match_class_methods(
            &ref_cand_starts,
            &ref_method_hash,
            &cls_file,
            &cls_name_off,
            &cls_name_len,
            &name_buf,
            &file_starts,
            &method_hash,
            &method_qn_off,
            &method_qn_len,
            &qn_buf,
        )?;

        let mut out: HashMap<usize, Option<(Node, bool)>> = HashMap::new();
        for (ranked_idx, &original_idx) in ref_idx_map.iter().enumerate() {
            let winner = if best_method[ranked_idx] < 0 {
                None
            } else {
                let via_s1 = (best_class[ranked_idx] as u32) < s1_boundary[ranked_idx];
                Some((
                    method_nodes[best_method[ranked_idx] as usize].clone(),
                    via_s1,
                ))
            };
            out.insert(original_idx, winner);
        }
        Some(out)
    }
}
