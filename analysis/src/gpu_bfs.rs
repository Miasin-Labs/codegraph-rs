//! GPU level-synchronous BFS over a CSR graph (feature `gpu`).
//!
//! Powers the transitive-`closure` traversal (callers/callees/impact) for
//! large multi-seed queries: every node reachable from any seed via the
//! pre-filtered adjacency, returned as a presence bitmap. Identical set to
//! the CPU work-stack BFS by construction (BFS and DFS visit the same
//! reachable set); the differential test asserts exact equality.
//!
//! Gating is the caller's job — a single small-frontier query is faster on
//! the CPU than a kernel launch + CSR upload; this engages for big seed sets.

use cudarc::driver::{CudaContext, CudaFunction, LaunchConfig, PushKernelArg};
use cudarc::nvrtc::compile_ptx;

const KERNEL_SRC: &str = r#"
typedef unsigned int u32;

// One thread per node. If it's in the current frontier, mark every neighbour
// as reached; newly-visited neighbours seed the next frontier. `changed` flags
// whether any new node was visited this level (host loops until it stays 0).
extern "C" __global__ void bfs_step(
    const u32* __restrict__ offsets,   // n+1
    const u32* __restrict__ neighbors,
    const u32* __restrict__ frontier,  // n: 1 if in current frontier
    u32*       __restrict__ next,      // n: next frontier (zeroed each level)
    u32*       __restrict__ visited,   // n: 0/1
    u32*       __restrict__ reached,    // n: 0/1 — arrived at via an edge
    int*       __restrict__ changed,
    u32 n
) {
    u32 u = blockIdx.x * blockDim.x + threadIdx.x;
    if (u >= n) return;
    if (frontier[u] == 0) return;
    u32 s = offsets[u], e = offsets[u + 1];
    for (u32 k = s; k < e; k++) {
        u32 v = neighbors[k];
        reached[v] = 1;
        // atomicExch returns the prior value; first thread to visit v wins and
        // adds it to the next frontier.
        if (atomicExch(&visited[v], 1u) == 0u) {
            next[v] = 1;
            atomicExch(changed, 1);
        }
    }
}
"#;

/// Reachable-set BFS on the GPU. `offsets`/`neighbors` are the CSR adjacency
/// (already filtered to the matching edge kind + direction); `seeds` are node
/// indices. Returns the per-node `reached` bitmap (node entered via an edge),
/// matching the CPU `closure`'s set. `None` (never errors) when no CUDA
/// device is present.
pub fn reachable_gpu(
    n: usize,
    offsets: &[u32],
    neighbors: &[u32],
    seeds: &[u32],
) -> Option<Vec<u8>> {
    if n == 0 {
        return Some(Vec::new());
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        reachable_gpu_inner(n, offsets, neighbors, seeds)
    }))
    .ok()
    .flatten()
}

fn reachable_gpu_inner(
    n: usize,
    offsets: &[u32],
    neighbors: &[u32],
    seeds: &[u32],
) -> Option<Vec<u8>> {
    let ctx = CudaContext::new(0).ok()?;
    let stream = ctx.default_stream();
    let ptx = compile_ptx(KERNEL_SRC).ok()?;
    let module = ctx.load_module(ptx).ok()?;
    let kernel: CudaFunction = module.load_function("bfs_step").ok()?;

    let mut visited = vec![0u32; n];
    let mut frontier = vec![0u32; n];
    for &s in seeds {
        if (s as usize) < n {
            visited[s as usize] = 1;
            frontier[s as usize] = 1;
        }
    }

    let d_off = stream.memcpy_stod(offsets).ok()?;
    let d_nb = stream.memcpy_stod(neighbors).ok()?;
    let mut d_frontier = stream.memcpy_stod(&frontier).ok()?;
    let mut d_visited = stream.memcpy_stod(&visited).ok()?;
    let mut d_reached = stream.alloc_zeros::<u32>(n).ok()?;

    let cfg = LaunchConfig {
        grid_dim: ((n as u32).div_ceil(256), 1, 1),
        block_dim: (256, 1, 1),
        shared_mem_bytes: 0,
    };

    loop {
        let mut d_next = stream.alloc_zeros::<u32>(n).ok()?;
        let mut d_changed = stream.memcpy_stod(&[0i32]).ok()?;
        let nu = n as u32;
        let mut launch = stream.launch_builder(&kernel);
        launch.arg(&d_off);
        launch.arg(&d_nb);
        launch.arg(&d_frontier);
        launch.arg(&mut d_next);
        launch.arg(&mut d_visited);
        launch.arg(&mut d_reached);
        launch.arg(&mut d_changed);
        launch.arg(&nu);
        // SAFETY: all device buffers sized n / n+1; atomics guard the writes.
        unsafe { launch.launch(cfg) }.ok()?;

        let changed = stream.memcpy_dtov(&d_changed).ok()?;
        if changed[0] == 0 {
            break;
        }
        d_frontier = d_next;
    }

    let reached = stream.memcpy_dtov(&d_reached).ok()?;
    Some(reached.into_iter().map(|x| x as u8).collect())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn gpu_bfs_matches_cpu_reachable_set() {
        let n = 2000usize;
        let mut state = 0x1234_5678u64;
        let mut rnd = |m: u64| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) % m
        };
        // random adjacency as CSR
        let mut adj: Vec<Vec<u32>> = vec![Vec::new(); n];
        for u in 0..n {
            for _ in 0..rnd(5) {
                adj[u].push(rnd(n as u64) as u32);
            }
        }
        let mut offsets = vec![0u32];
        let mut neighbors = Vec::new();
        for u in 0..n {
            neighbors.extend_from_slice(&adj[u]);
            offsets.push(neighbors.len() as u32);
        }
        let seeds: Vec<u32> = (0..50).map(|_| rnd(n as u64) as u32).collect();

        // CPU mirror of `closure`'s reached set.
        let mut visited: HashSet<u32> = seeds.iter().copied().collect();
        let mut reached: HashSet<u32> = HashSet::new();
        let mut stack: Vec<u32> = seeds.clone();
        while let Some(u) = stack.pop() {
            for &v in &adj[u as usize] {
                reached.insert(v);
                if visited.insert(v) {
                    stack.push(v);
                }
            }
        }

        let Some(bitmap) = reachable_gpu(n, &offsets, &neighbors, &seeds) else {
            eprintln!("no CUDA device — skipping GPU BFS test");
            return;
        };
        let gpu_set: HashSet<u32> = (0..n)
            .filter(|&i| bitmap[i] == 1)
            .map(|i| i as u32)
            .collect();
        assert_eq!(gpu_set, reached, "GPU reachable set differs from CPU");
        eprintln!("GPU BFS: {} reached, identical to CPU", gpu_set.len());
    }
}
