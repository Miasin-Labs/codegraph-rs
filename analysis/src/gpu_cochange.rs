//! GPU co-occurrence counting for temporal coupling (feature `gpu`).
//!
//! `compute_co_changes` spends O(Σ commit_size²) building a map
//! `(node_a, node_b) → times_changed_together`. The counts are pure integer
//! accumulation — order-independent — so the GPU result is exactly equal to
//! the CPU map (the differential test asserts bit-equality). One thread per
//! (commit, lower-node) emits that node's higher-node pairs into a device
//! open-addressing hash table keyed by the packed `(a<<32)|b`, with the count
//! accumulated by `atomicAdd` (integer addition is associative → deterministic).

use cudarc::driver::{CudaContext, CudaFunction, LaunchConfig, PushKernelArg};
use cudarc::nvrtc::compile_ptx;

const EMPTY: u64 = u64::MAX;

const KERNEL_SRC: &str = r#"
typedef unsigned int u32;
typedef unsigned long long u64;

// splitmix64 finalizer — good avalanche for the packed pair key.
extern "C" __device__ u64 mix(u64 x) {
    x ^= x >> 30; x *= 0xbf58476d1ce4e5b9ULL;
    x ^= x >> 27; x *= 0x94d049bb133111ebULL;
    x ^= x >> 31;
    return x;
}

// One thread per (commit, position i). The commit's nodes are sorted+deduped
// in [coff[c], coff[c+1]); emit pairs (cn[i], cn[j]) for all j>i in the same
// commit, inserting each into the open-addressing table and bumping its count.
extern "C" __global__ void cooccur(
    const u32* __restrict__ coff,   // n_commits + 1
    const u32* __restrict__ cn,     // commit nodes (interned ids), per-commit sorted
    const u32* __restrict__ owner,  // for flat index k: which commit it belongs to
    u64*       __restrict__ keys,
    u32*       __restrict__ counts,
    u64 mask,
    u32 total   // length of cn
) {
    u32 k = blockIdx.x * blockDim.x + threadIdx.x;
    if (k >= total) return;
    u32 c = owner[k];
    u32 end = coff[c + 1];
    u32 a = cn[k];
    for (u32 j = k + 1; j < end; j++) {
        u32 b = cn[j];                       // a < b since the commit slice is sorted
        u64 key = ((u64)a << 32) | (u64)b;
        u64 slot = mix(key) & mask;
        for (;;) {
            u64 cur = atomicCAS((unsigned long long*)&keys[slot], (unsigned long long)0xffffffffffffffffULL, (unsigned long long)key);
            if (cur == 0xffffffffffffffffULL || cur == key) {
                atomicAdd(&counts[slot], 1u);
                break;
            }
            slot = (slot + 1) & mask;        // linear probe
        }
    }
}
"#;

/// Co-occurrence counts on the GPU. `commit_off`/`commit_nodes` is a CSR of
/// per-commit interned node ids (each commit's slice MUST be sorted ascending
/// and deduped, matching the CPU). Returns `(a, b, count)` triples (a < b).
/// `None` when no CUDA device, or when the table would exceed `max_slots`
/// (caller falls back to CPU).
pub fn cooccurrence_gpu(
    commit_off: &[u32],
    commit_nodes: &[u32],
    max_slots: usize,
) -> Option<Vec<(u32, u32, u32)>> {
    if commit_nodes.is_empty() {
        return Some(Vec::new());
    }
    crate::gpu_probe(|| cooccurrence_gpu_inner(commit_off, commit_nodes, max_slots))
}

fn cooccurrence_gpu_inner(
    commit_off: &[u32],
    commit_nodes: &[u32],
    max_slots: usize,
) -> Option<Vec<(u32, u32, u32)>> {
    // Upper bound on distinct pairs = total pair count Σ C(size,2).
    let mut total_pairs: u64 = 0;
    for w in commit_off.windows(2) {
        let s = (w[1] - w[0]) as u64;
        total_pairs = total_pairs.checked_add(s.checked_mul(s.saturating_sub(1))? / 2)?;
    }
    if total_pairs == 0 {
        return Some(Vec::new());
    }
    let capacity_u64 = total_pairs.checked_mul(2)?.checked_next_power_of_two()?;
    let capacity = usize::try_from(capacity_u64).ok()?.max(1024);
    if capacity > max_slots {
        return None; // too big for VRAM budget — let the CPU handle it
    }
    let mask = (capacity - 1) as u64;

    // owner[k] = commit index of flat node position k.
    let mut owner = vec![0u32; commit_nodes.len()];
    for (c, w) in commit_off.windows(2).enumerate() {
        for k in w[0]..w[1] {
            owner[k as usize] = c as u32;
        }
    }

    let ctx = CudaContext::new(0).ok()?;
    let stream = ctx.default_stream();
    let module = ctx.load_module(compile_ptx(KERNEL_SRC).ok()?).ok()?;
    let kernel: CudaFunction = module.load_function("cooccur").ok()?;

    let d_off = stream.memcpy_stod(commit_off).ok()?;
    let d_cn = stream.memcpy_stod(commit_nodes).ok()?;
    let d_owner = stream.memcpy_stod(&owner).ok()?;
    let mut d_keys = stream.memcpy_stod(&vec![EMPTY; capacity]).ok()?;
    let mut d_counts = stream.alloc_zeros::<u32>(capacity).ok()?;

    let total = u32::try_from(commit_nodes.len()).ok()?; // kernel index is u32
    let cfg = LaunchConfig {
        grid_dim: (total.div_ceil(256).max(1), 1, 1),
        block_dim: (256, 1, 1),
        shared_mem_bytes: 0,
    };
    let mut l = stream.launch_builder(&kernel);
    l.arg(&d_off);
    l.arg(&d_cn);
    l.arg(&d_owner);
    l.arg(&mut d_keys);
    l.arg(&mut d_counts);
    l.arg(&mask);
    l.arg(&total);
    // SAFETY: open-addressing writes guarded by atomicCAS/atomicAdd; the table
    // is sized > 2× the distinct-pair upper bound so probing always finds a slot.
    unsafe { l.launch(cfg) }.ok()?;

    let keys = stream.memcpy_dtov(&d_keys).ok()?;
    let counts = stream.memcpy_dtov(&d_counts).ok()?;
    let mut out = Vec::new();
    for (slot, &key) in keys.iter().enumerate() {
        if key != EMPTY {
            out.push(((key >> 32) as u32, (key & 0xffff_ffff) as u32, counts[slot]));
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn gpu_cooccurrence_matches_cpu() {
        let n_commits = 4000usize;
        let n_nodes = 3000u32;
        let mut state = 0xc0c4_a9e5u64;
        let mut rnd = |m: u64| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) % m
        };
        let mut commit_off = vec![0u32];
        let mut commit_nodes = Vec::new();
        for _ in 0..n_commits {
            let sz = rnd(12) as usize; // 0..11 nodes per commit
            let mut nodes: Vec<u32> = (0..sz).map(|_| rnd(n_nodes as u64) as u32).collect();
            nodes.sort_unstable();
            nodes.dedup();
            commit_nodes.extend_from_slice(&nodes);
            commit_off.push(commit_nodes.len() as u32);
        }

        // CPU reference map.
        let mut cpu: HashMap<(u32, u32), u32> = HashMap::new();
        for w in commit_off.windows(2) {
            let slice = &commit_nodes[w[0] as usize..w[1] as usize];
            for i in 0..slice.len() {
                for j in (i + 1)..slice.len() {
                    *cpu.entry((slice[i], slice[j])).or_insert(0) += 1;
                }
            }
        }

        let Some(triples) = cooccurrence_gpu(&commit_off, &commit_nodes, 1 << 26) else {
            eprintln!("no CUDA device — skipping GPU co-change test");
            return;
        };
        let gpu: HashMap<(u32, u32), u32> =
            triples.into_iter().map(|(a, b, c)| ((a, b), c)).collect();
        assert_eq!(gpu, cpu, "GPU co-occurrence map differs from CPU");
        eprintln!(
            "GPU co-change: {} distinct pairs, identical to CPU",
            gpu.len()
        );
    }
}
