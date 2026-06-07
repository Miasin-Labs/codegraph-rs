//! GPU PageRank for the analysis engine's centrality metric (feature `gpu`).
//!
//! Faithful port of petgraph's `page_rank` (damping 0.85, 50 iters). That
//! algorithm's per-target update,
//!   pi[v] = Σ_w  { d·r[w]/outdeg[w]           if w→v
//!               { d·r[w]/nb                  if outdeg[w]==0 (dangling)
//!               { (1-d)·r[w]/nb              otherwise (random jump)
//! decomposes — since every predecessor of v has outdeg ≥ 1 — into a global
//! constant plus a sum over v's INCOMING edges:
//!   base  = d/nb·Σ_{dangling w} r[w]  +  (1-d)/nb·Σ_{outdeg>0 w} r[w]
//!   pi[v] = base + Σ_{w→v} ( d·r[w]/outdeg[w] − (1-d)·r[w]/nb )
//! then ranks = pi / Σ pi. One thread per v iterates a reverse-CSR
//! (predecessor) adjacency — exactly the shape a GPU wants.
//!
//! Float arithmetic is not associative, so f32 results differ from petgraph
//! in the last ULPs (both are explicitly "approximate" PageRank). The
//! acceptance gate is therefore identical TOP-N RANKING plus scores within a
//! tight tolerance — verified against petgraph on random graphs.

use cudarc::driver::{CudaContext, CudaFunction, LaunchConfig, PushKernelArg};
use cudarc::nvrtc::compile_ptx;

const KERNEL_SRC: &str = r#"
typedef unsigned int u32;

// pi[v] = base + sum over predecessors w of (d*r[w]/outdeg[w] - (1-d)*r[w]/nb)
extern "C" __global__ void pagerank_step(
    const u32*   __restrict__ in_off,   // n+1 reverse-CSR offsets
    const u32*   __restrict__ in_pred,  // predecessor node ids
    const float* __restrict__ r,        // current ranks
    const float* __restrict__ outdeg,   // out-degree per node
    float        base,
    float        d,
    float        nb,
    float*       __restrict__ pi,
    u32 n
) {
    u32 v = blockIdx.x * blockDim.x + threadIdx.x;
    if (v >= n) return;
    float acc = base;
    float jump = (1.0f - d) / nb;
    u32 s = in_off[v], e = in_off[v + 1];
    for (u32 k = s; k < e; k++) {
        u32 w = in_pred[k];
        acc += d * r[w] / outdeg[w] - jump * r[w];
    }
    pi[v] = acc;
}
"#;

/// Run petgraph-equivalent PageRank on the GPU.
///
/// `in_off`/`in_pred` are the reverse-CSR (incoming) adjacency; `out_degrees`
/// is per-node out-degree. Returns `None` (never errors) when no CUDA device
/// is available, so callers fall back to the CPU path.
pub fn pagerank_gpu(
    n: usize,
    in_off: &[u32],
    in_pred: &[u32],
    out_degrees: &[f32],
    damping: f32,
    iters: usize,
) -> Option<Vec<f32>> {
    if n == 0 {
        return Some(Vec::new());
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pagerank_gpu_inner(n, in_off, in_pred, out_degrees, damping, iters)
    }))
    .ok()
    .flatten()
}

fn pagerank_gpu_inner(
    n: usize,
    in_off: &[u32],
    in_pred: &[u32],
    out_degrees: &[f32],
    damping: f32,
    iters: usize,
) -> Option<Vec<f32>> {
    let ctx = CudaContext::new(0).ok()?;
    let stream = ctx.default_stream();
    let ptx = compile_ptx(KERNEL_SRC).ok()?;
    let module = ctx.load_module(ptx).ok()?;
    let kernel: CudaFunction = module.load_function("pagerank_step").ok()?;

    let nb = n as f32;
    let mut ranks = vec![1.0f32 / nb; n];

    let d_off = stream.memcpy_stod(in_off).ok()?;
    let d_pred = stream.memcpy_stod(in_pred).ok()?;
    let d_outdeg = stream.memcpy_stod(out_degrees).ok()?;
    let mut d_r = stream.memcpy_stod(&ranks).ok()?;
    let mut d_pi = stream.alloc_zeros::<f32>(n).ok()?;

    let cfg = LaunchConfig {
        grid_dim: ((n as u32).div_ceil(256), 1, 1),
        block_dim: (256, 1, 1),
        shared_mem_bytes: 0,
    };

    for _ in 0..iters {
        // Global mass terms — small reductions done on the host to match the
        // decomposition exactly (n-sized copy back is cheap vs the kernel).
        let mut dangling_sum = 0.0f32;
        let mut live_sum = 0.0f32;
        for i in 0..n {
            if out_degrees[i] == 0.0 {
                dangling_sum += ranks[i];
            } else {
                live_sum += ranks[i];
            }
        }
        let base = damping / nb * dangling_sum + (1.0 - damping) / nb * live_sum;

        let mut launch = stream.launch_builder(&kernel);
        let nu = n as u32;
        launch.arg(&d_off);
        launch.arg(&d_pred);
        launch.arg(&d_r);
        launch.arg(&d_outdeg);
        launch.arg(&base);
        launch.arg(&damping);
        launch.arg(&nb);
        launch.arg(&mut d_pi);
        launch.arg(&nu);
        // SAFETY: one write per thread to pi[v]; all device buffers sized n.
        unsafe { launch.launch(cfg) }.ok()?;

        let pi = stream.memcpy_dtov(&d_pi).ok()?;
        let sum: f32 = pi.iter().sum();
        for (rk, p) in ranks.iter_mut().zip(pi.iter()) {
            *rk = *p / sum;
        }
        d_r = stream.memcpy_stod(&ranks).ok()?;
    }
    Some(ranks)
}

#[cfg(test)]
mod tests {
    use petgraph::Graph;
    use petgraph::algo::page_rank;

    use super::*;

    /// Build reverse-CSR (incoming) adjacency + out-degrees from a petgraph.
    fn reverse_csr(g: &Graph<(), ()>) -> (Vec<u32>, Vec<u32>, Vec<f32>) {
        use petgraph::Direction;
        let n = g.node_count();
        let mut in_off = Vec::with_capacity(n + 1);
        let mut in_pred = Vec::new();
        let mut outdeg = vec![0f32; n];
        in_off.push(0);
        for v in 0..n {
            let vi = petgraph::graph::NodeIndex::new(v);
            // Dedupe predecessors: petgraph's update tests `any(target==v)`,
            // so a multi-edge w⇉v contributes ONCE, while outdeg[w] below
            // still counts every out-edge.
            let mut preds: Vec<u32> = g
                .neighbors_directed(vi, Direction::Incoming)
                .map(|w| w.index() as u32)
                .collect();
            preds.sort_unstable();
            preds.dedup();
            in_pred.extend_from_slice(&preds);
            in_off.push(in_pred.len() as u32);
        }
        for w in 0..n {
            let wi = petgraph::graph::NodeIndex::new(w);
            outdeg[w] = g.neighbors_directed(wi, Direction::Outgoing).count() as f32;
        }
        (in_off, in_pred, outdeg)
    }

    #[test]
    fn gpu_pagerank_matches_petgraph_ranking() {
        // Deterministic random DAG-ish graph, incl. dangling nodes.
        let n = 500usize;
        let mut g = Graph::<(), ()>::new();
        let idx: Vec<_> = (0..n).map(|_| g.add_node(())).collect();
        let mut state = 0x9e3779b9u64;
        let mut rnd = |m: u64| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) % m
        };
        for _ in 0..(n * 4) {
            let a = rnd(n as u64) as usize;
            let b = rnd(n as u64) as usize;
            if a != b {
                g.add_edge(idx[a], idx[b], ());
            }
        }

        let cpu = page_rank(&g, 0.85f32, 50);
        let (in_off, in_pred, outdeg) = reverse_csr(&g);
        let Some(gpu) = pagerank_gpu(n, &in_off, &in_pred, &outdeg, 0.85, 50) else {
            eprintln!("no CUDA device — skipping GPU PageRank test");
            return;
        };

        // Scores close (approximate algorithm; f32, reordered summation).
        let mut max_rel = 0f32;
        for i in 0..n {
            let denom = cpu[i].abs().max(1e-12);
            max_rel = max_rel.max((gpu[i] - cpu[i]).abs() / denom);
        }
        assert!(
            max_rel < 1e-3,
            "max relative score diff {max_rel} too large"
        );

        // Top-N ranking identical (the actual consumer: hottest_functions).
        let topn = |v: &[f32], k: usize| -> Vec<usize> {
            let mut idx: Vec<usize> = (0..v.len()).collect();
            idx.sort_by(|&a, &b| v[b].partial_cmp(&v[a]).unwrap().then(a.cmp(&b)));
            idx.truncate(k);
            idx
        };
        assert_eq!(topn(&cpu, 25), topn(&gpu, 25), "top-25 ranking differs");
        eprintln!("GPU PageRank: top-25 identical, max rel score diff {max_rel:.2e}");
    }
}
