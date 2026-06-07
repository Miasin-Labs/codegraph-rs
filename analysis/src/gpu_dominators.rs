//! GPU dominator tree via parallel Cooper-Harvey-Kennedy (feature `gpu`).
//!
//! The immediate-dominator map of a rooted digraph is UNIQUE, so the GPU
//! result is checked for exact equality against petgraph's `simple_fast`
//! (the acceptance gate). Cooper-Harvey-Kennedy is an iterative dataflow:
//! `idom[b] = fold(intersect, defined-idom predecessors of b)`, iterated to a
//! fixpoint. Each node's update reads only the previous iteration's idoms, so
//! a whole sweep runs in parallel (one thread per node); the monotone
//! framework converges to the same unique tree regardless of sweep order.
//!
//! The reverse-postorder numbering the `intersect` finger-walk needs is
//! computed once on the host (cheap O(V+E) DFS); everything else is on-GPU.

use cudarc::driver::{CudaContext, CudaFunction, LaunchConfig, PushKernelArg};
use cudarc::nvrtc::compile_ptx;

const UNDEF: u32 = u32::MAX;

const KERNEL_SRC: &str = r#"
typedef unsigned int u32;
#define UNDEF 0xffffffffu

// intersect(a,b): walk up via idom comparing RPO numbers until they meet.
extern "C" __device__ u32 intersect(
    u32 a, u32 b, const u32* __restrict__ rpo, const u32* __restrict__ idom
) {
    while (a != b) {
        while (rpo[a] > rpo[b]) a = idom[a];
        while (rpo[b] > rpo[a]) b = idom[b];
    }
    return a;
}

// One thread per node (excluding the root). Recomputes idom[b] from its
// predecessors whose idom is already defined, reading `idom_in` and writing
// `idom_out`. Sets `changed` when a value moves.
extern "C" __global__ void dom_step(
    const u32* __restrict__ pred_off,  // n+1
    const u32* __restrict__ pred,      // predecessor node ids (RPO-reachable)
    const u32* __restrict__ rpo,       // RPO number per node (UNDEF if unreachable)
    u32 root,
    const u32* __restrict__ idom_in,
    u32*       __restrict__ idom_out,
    int*       __restrict__ changed,
    u32 n
) {
    u32 b = blockIdx.x * blockDim.x + threadIdx.x;
    if (b >= n) return;
    if (b == root || rpo[b] == UNDEF) { idom_out[b] = idom_in[b]; return; }
    u32 new_idom = UNDEF;
    u32 s = pred_off[b], e = pred_off[b + 1];
    for (u32 k = s; k < e; k++) {
        u32 p = pred[k];
        if (idom_in[p] == UNDEF) continue;   // predecessor not yet processed
        new_idom = (new_idom == UNDEF) ? p : intersect(p, new_idom, rpo, idom_in);
    }
    idom_out[b] = new_idom;
    if (new_idom != idom_in[b]) atomicExch(changed, 1);
}
"#;

/// GPU immediate-dominator map rooted at `root`.
///
/// `pred_off`/`pred` are the predecessor CSR; `rpo[v]` is v's reverse-postorder
/// number (`u32::MAX` if unreachable from root). Returns `idom[v]` per node
/// (`u32::MAX` = root or unreachable), matching petgraph's
/// `immediate_dominator`. `None` when no CUDA device is present.
pub fn dominators_gpu(
    n: usize,
    root: u32,
    pred_off: &[u32],
    pred: &[u32],
    rpo: &[u32],
) -> Option<Vec<u32>> {
    if n == 0 {
        return Some(Vec::new());
    }
    crate::gpu_probe(|| dominators_gpu_inner(n, root, pred_off, pred, rpo))
}

fn dominators_gpu_inner(
    n: usize,
    root: u32,
    pred_off: &[u32],
    pred: &[u32],
    rpo: &[u32],
) -> Option<Vec<u32>> {
    let ctx = CudaContext::new(0).ok()?;
    let stream = ctx.default_stream();
    let module = ctx.load_module(compile_ptx(KERNEL_SRC).ok()?).ok()?;
    let kernel: CudaFunction = module.load_function("dom_step").ok()?;

    let mut idom = vec![UNDEF; n];
    idom[root as usize] = root; // root dominates itself (sentinel for the walk)

    let d_off = stream.memcpy_stod(pred_off).ok()?;
    let d_pred = stream.memcpy_stod(pred).ok()?;
    let d_rpo = stream.memcpy_stod(rpo).ok()?;
    let mut d_in = stream.memcpy_stod(&idom).ok()?;
    let mut d_out = stream.memcpy_stod(&idom).ok()?;

    let nu = n as u32;
    let cfg = LaunchConfig {
        grid_dim: (nu.div_ceil(256).max(1), 1, 1),
        block_dim: (256, 1, 1),
        shared_mem_bytes: 0,
    };

    // Each sweep reads d_in, writes d_out, then swaps. Bounded by n (the tree
    // depth is the true bound; n is the hard ceiling).
    for _ in 0..=n {
        let mut d_changed = stream.memcpy_stod(&[0i32]).ok()?;
        let mut l = stream.launch_builder(&kernel);
        l.arg(&d_off);
        l.arg(&d_pred);
        l.arg(&d_rpo);
        l.arg(&root);
        l.arg(&d_in);
        l.arg(&mut d_out);
        l.arg(&mut d_changed);
        l.arg(&nu);
        // SAFETY: per-node write to idom_out[b]; CSR + rpo host-validated.
        unsafe { l.launch(cfg) }.ok()?;
        std::mem::swap(&mut d_in, &mut d_out);
        if stream.memcpy_dtov(&d_changed).ok()?[0] == 0 {
            break;
        }
    }

    let mut result = stream.memcpy_dtov(&d_in).ok()?;
    // Report the root and unreachable nodes as UNDEF (petgraph parity:
    // `immediate_dominator(root)` is None; unreachable nodes are None).
    result[root as usize] = UNDEF;
    for v in 0..n {
        if rpo[v] == UNDEF {
            result[v] = UNDEF;
        }
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use petgraph::Graph;
    use petgraph::algo::dominators::simple_fast;

    use super::*;

    /// Host reverse-postorder numbering + predecessor CSR (reachable nodes).
    fn rpo_and_pred(g: &Graph<(), ()>, root: usize) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
        use petgraph::Direction;
        let n = g.node_count();
        // iterative DFS postorder from root
        let mut visited = vec![false; n];
        let mut post = Vec::new();
        let mut stack = vec![(root, false)];
        while let Some((u, processed)) = stack.pop() {
            if processed {
                post.push(u as u32);
                continue;
            }
            if visited[u] {
                continue;
            }
            visited[u] = true;
            stack.push((u, true));
            for w in g.neighbors_directed(petgraph::graph::NodeIndex::new(u), Direction::Outgoing) {
                if !visited[w.index()] {
                    stack.push((w.index(), false));
                }
            }
        }
        // reverse postorder number: earlier-in-RPO (closer to root) = smaller
        let mut rpo = vec![UNDEF; n];
        for (i, &node) in post.iter().rev().enumerate() {
            rpo[node as usize] = i as u32;
        }
        // predecessor CSR over ALL nodes (kernel skips unreachable via rpo)
        let mut pred_off = vec![0u32];
        let mut pred = Vec::new();
        for v in 0..n {
            for w in g.neighbors_directed(petgraph::graph::NodeIndex::new(v), Direction::Incoming) {
                pred.push(w.index() as u32);
            }
            pred_off.push(pred.len() as u32);
        }
        (rpo, pred_off, pred)
    }

    #[test]
    fn gpu_dominators_match_petgraph() {
        let n = 800usize;
        let mut g = Graph::<(), ()>::new();
        let idx: Vec<_> = (0..n).map(|_| g.add_node(())).collect();
        let mut state = 0xd0d0_1234u64;
        let mut rnd = |m: u64| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) % m
        };
        // Mostly-forward edges (reducible-ish CFG shape) plus some back-edges.
        for a in 0..n {
            for _ in 0..(1 + rnd(3)) {
                let b = (a + 1 + rnd(40) as usize) % n;
                g.add_edge(idx[a], idx[b], ());
            }
            if rnd(5) == 0 && a > 0 {
                let b = rnd(a as u64 + 1) as usize;
                g.add_edge(idx[a], idx[b], ());
            }
        }
        let root = 0usize;

        let doms = simple_fast(&g, idx[root]);
        let (rpo, pred_off, pred) = rpo_and_pred(&g, root);
        let Some(gpu) = dominators_gpu(n, root as u32, &pred_off, &pred, &rpo) else {
            eprintln!("no CUDA device — skipping GPU dominators test");
            return;
        };

        let mut mismatches = 0;
        for v in 0..n {
            let cpu = doms
                .immediate_dominator(idx[v])
                .map(|x| x.index() as u32)
                .unwrap_or(UNDEF);
            if cpu != gpu[v] {
                mismatches += 1;
                if mismatches <= 5 {
                    eprintln!("node {v}: cpu idom {cpu} != gpu {}", gpu[v]);
                }
            }
        }
        assert_eq!(mismatches, 0, "GPU idom map differs from petgraph");
        let reachable = rpo.iter().filter(|&&r| r != UNDEF).count();
        eprintln!("GPU dominators: {reachable} reachable nodes, idom == petgraph");
    }
}
