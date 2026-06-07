//! GPU strongly-connected components via the coloring algorithm (feature
//! `gpu`). The SCC PARTITION of a digraph is canonical — independent of the
//! algorithm — so the GPU labeling is checked for exact partition equality
//! against petgraph's `tarjan_scc` (the acceptance gate).
//!
//! Coloring SCC (Orzan / Hong et al.), correct and embarrassingly parallel:
//! repeat over the still-active subgraph until empty —
//!   1. color[v] = v.
//!   2. forward-propagate color = MAX over active edges to a fixpoint, so
//!      color[v] = the largest id that can reach v within the active set.
//!   3. roots are vertices with color[v] == v. Mark them `found`, then
//!      backward-propagate `found` along active edges within the same color:
//!      found[u] |= found[v] for an edge u→v with color[u]==color[v]. At the
//!      fixpoint, {v : found[v]} are exactly the vertices that both reach the
//!      root (backward) and are reachable from it (color == root), i.e. one
//!      SCC per colour.
//!   4. label those vertices with their colour (the SCC's max id), deactivate
//!      them, and repeat on the remainder.
//! Trivial (singleton) SCCs — the vast majority of a call graph — are peeled
//! in the first rounds, so it converges quickly on real inputs.

use std::sync::Arc;

use cudarc::driver::{
    CudaContext,
    CudaFunction,
    CudaSlice,
    CudaStream,
    LaunchConfig,
    PushKernelArg,
};
use cudarc::nvrtc::compile_ptx;

const KERNEL_SRC: &str = r#"
typedef unsigned int u32;

extern "C" __global__ void init_round(
    const u32* __restrict__ active, u32* __restrict__ color, u32* __restrict__ found, u32 n
) {
    u32 v = blockIdx.x * blockDim.x + threadIdx.x;
    if (v >= n) return;
    if (active[v]) { color[v] = v; found[v] = 0; }
}

// Forward: color[v] = max(color[v], color[u]) over active edge u->v.
extern "C" __global__ void fwd_max(
    const u32* __restrict__ src, const u32* __restrict__ dst,
    const u32* __restrict__ active, u32* __restrict__ color,
    int* __restrict__ changed, u32 m
) {
    u32 e = blockIdx.x * blockDim.x + threadIdx.x;
    if (e >= m) return;
    u32 u = src[e], v = dst[e];
    if (!active[u] || !active[v]) return;
    u32 cu = color[u];
    if (cu > color[v]) { atomicMax(&color[v], cu); atomicExch(changed, 1); }
}

// Mark roots: active vertices whose colour is their own id.
extern "C" __global__ void mark_roots(
    const u32* __restrict__ active, const u32* __restrict__ color,
    u32* __restrict__ found, u32 n
) {
    u32 v = blockIdx.x * blockDim.x + threadIdx.x;
    if (v >= n) return;
    if (active[v] && color[v] == v) found[v] = 1;
}

// Backward: found[u] |= found[v] over active edge u->v with equal colour.
extern "C" __global__ void bwd_found(
    const u32* __restrict__ src, const u32* __restrict__ dst,
    const u32* __restrict__ active, const u32* __restrict__ color,
    u32* __restrict__ found, int* __restrict__ changed, u32 m
) {
    u32 e = blockIdx.x * blockDim.x + threadIdx.x;
    if (e >= m) return;
    u32 u = src[e], v = dst[e];
    if (!active[u] || !active[v]) return;
    if (color[u] != color[v]) return;
    if (found[v] && !found[u]) { found[u] = 1; atomicExch(changed, 1); }
}

// Assign labels to found vertices and deactivate them. `remaining` counts the
// still-active vertices so the host knows when to stop.
extern "C" __global__ void finalize_round(
    u32* __restrict__ active, const u32* __restrict__ color,
    const u32* __restrict__ found, u32* __restrict__ label,
    int* __restrict__ remaining, u32 n
) {
    u32 v = blockIdx.x * blockDim.x + threadIdx.x;
    if (v >= n) return;
    if (!active[v]) return;
    if (found[v]) { label[v] = color[v]; active[v] = 0; }
    else { atomicAdd(remaining, 1); }
}
"#;

struct SccCtx {
    stream: Arc<CudaStream>,
    init_round: CudaFunction,
    fwd_max: CudaFunction,
    mark_roots: CudaFunction,
    bwd_found: CudaFunction,
    finalize: CudaFunction,
}

impl SccCtx {
    fn new() -> Option<Self> {
        let ctx = CudaContext::new(0).ok()?;
        let stream = ctx.default_stream();
        let module = ctx.load_module(compile_ptx(KERNEL_SRC).ok()?).ok()?;
        Some(Self {
            init_round: module.load_function("init_round").ok()?,
            fwd_max: module.load_function("fwd_max").ok()?,
            mark_roots: module.load_function("mark_roots").ok()?,
            bwd_found: module.load_function("bwd_found").ok()?,
            finalize: module.load_function("finalize_round").ok()?,
            stream,
        })
    }
}

/// GPU SCC. Returns a per-node component label (the max node id in the
/// component) in node order. `None` when no CUDA device is present.
pub fn scc_gpu(n: usize, src: &[u32], dst: &[u32]) -> Option<Vec<u32>> {
    if n == 0 {
        return Some(Vec::new());
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| scc_gpu_inner(n, src, dst)))
        .ok()
        .flatten()
}

fn scc_gpu_inner(n: usize, src: &[u32], dst: &[u32]) -> Option<Vec<u32>> {
    let ctx = SccCtx::new()?;
    let m = src.len();
    let nu = n as u32;
    let mu = m as u32;
    let vgrid = nu.div_ceil(256);
    let egrid = mu.div_ceil(256);

    let d_src = ctx.stream.memcpy_stod(src).ok()?;
    let d_dst = ctx.stream.memcpy_stod(dst).ok()?;
    let mut d_active: CudaSlice<u32> = ctx.stream.memcpy_stod(&vec![1u32; n]).ok()?;
    let mut d_color = ctx.stream.alloc_zeros::<u32>(n).ok()?;
    let mut d_found = ctx.stream.alloc_zeros::<u32>(n).ok()?;
    let mut d_label: CudaSlice<u32> = ctx.stream.memcpy_stod(&(0..nu).collect::<Vec<_>>()).ok()?;

    let vcfg = LaunchConfig {
        grid_dim: (vgrid.max(1), 1, 1),
        block_dim: (256, 1, 1),
        shared_mem_bytes: 0,
    };

    // Bound rounds defensively (each removes ≥1 SCC); n is the hard ceiling.
    for _ in 0..=n {
        // 1+2: reset colour/found for active, forward-max to fixpoint.
        let mut l = ctx.stream.launch_builder(&ctx.init_round);
        l.arg(&d_active);
        l.arg(&mut d_color);
        l.arg(&mut d_found);
        l.arg(&nu);
        unsafe { l.launch(vcfg) }.ok()?;

        let ecfg = LaunchConfig {
            grid_dim: (egrid.max(1), 1, 1),
            block_dim: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        loop {
            let mut d_changed = ctx.stream.memcpy_stod(&[0i32]).ok()?;
            let mut b = ctx.stream.launch_builder(&ctx.fwd_max);
            b.arg(&d_src);
            b.arg(&d_dst);
            b.arg(&d_active);
            b.arg(&mut d_color);
            b.arg(&mut d_changed);
            b.arg(&mu);
            // SAFETY: edge-indexed reads, atomicMax write; buffers sized.
            unsafe { b.launch(ecfg) }.ok()?;
            if ctx.stream.memcpy_dtov(&d_changed).ok()?[0] == 0 {
                break;
            }
        }

        // 3: mark roots, backward-found to fixpoint.
        let mut l = ctx.stream.launch_builder(&ctx.mark_roots);
        l.arg(&d_active);
        l.arg(&d_color);
        l.arg(&mut d_found);
        l.arg(&nu);
        unsafe { l.launch(vcfg) }.ok()?;

        loop {
            let mut d_changed = ctx.stream.memcpy_stod(&[0i32]).ok()?;
            let mut b = ctx.stream.launch_builder(&ctx.bwd_found);
            b.arg(&d_src);
            b.arg(&d_dst);
            b.arg(&d_active);
            b.arg(&d_color);
            b.arg(&mut d_found);
            b.arg(&mut d_changed);
            b.arg(&mu);
            // SAFETY: edge-indexed reads, found write; buffers sized.
            unsafe { b.launch(ecfg) }.ok()?;
            if ctx.stream.memcpy_dtov(&d_changed).ok()?[0] == 0 {
                break;
            }
        }

        // 4: label found, deactivate, count remaining.
        let mut d_rem = ctx.stream.memcpy_stod(&[0i32]).ok()?;
        let mut l = ctx.stream.launch_builder(&ctx.finalize);
        l.arg(&mut d_active);
        l.arg(&d_color);
        l.arg(&d_found);
        l.arg(&mut d_label);
        l.arg(&mut d_rem);
        l.arg(&nu);
        unsafe { l.launch(vcfg) }.ok()?;

        if ctx.stream.memcpy_dtov(&d_rem).ok()?[0] == 0 {
            break;
        }
    }
    ctx.stream.memcpy_dtov(&d_label).ok()
}

/// Group canonical labels into the set of components (each a sorted node list),
/// itself sorted — a canonical form for comparing partitions across algorithms.
pub fn partition_of(labels: &[u32]) -> Vec<Vec<u32>> {
    use std::collections::HashMap;
    let mut groups: HashMap<u32, Vec<u32>> = HashMap::new();
    for (i, &l) in labels.iter().enumerate() {
        groups.entry(l).or_default().push(i as u32);
    }
    let mut parts: Vec<Vec<u32>> = groups.into_values().collect();
    for p in &mut parts {
        p.sort_unstable();
    }
    parts.sort_unstable();
    parts
}

#[cfg(test)]
mod tests {
    use petgraph::Graph;
    use petgraph::algo::scc::tarjan_scc::tarjan_scc;

    use super::*;

    #[test]
    fn gpu_scc_partition_matches_tarjan() {
        let n = 1500usize;
        let mut g = Graph::<(), ()>::new();
        let idx: Vec<_> = (0..n).map(|_| g.add_node(())).collect();
        let mut state = 0xcafef00du64;
        let mut rnd = |m: u64| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) % m
        };
        let (mut src, mut dst) = (Vec::new(), Vec::new());
        for _ in 0..(n * 3) {
            let a = rnd(n as u64) as u32;
            let b = rnd(n as u64) as u32;
            g.add_edge(idx[a as usize], idx[b as usize], ());
            src.push(a);
            dst.push(b);
        }
        // guaranteed cycles (4-node rings) so non-trivial SCCs exist
        for c in 0..30 {
            let base = (c * 7) % n;
            for k in 0..4u64 {
                let a = ((base as u64 + k) % n as u64) as u32;
                let b = ((base as u64 + (k + 1) % 4) % n as u64) as u32;
                g.add_edge(idx[a as usize], idx[b as usize], ());
                src.push(a);
                dst.push(b);
            }
        }

        let Some(labels) = scc_gpu(n, &src, &dst) else {
            eprintln!("no CUDA device — skipping GPU SCC test");
            return;
        };
        let gpu_parts = partition_of(&labels);

        let mut cpu_parts: Vec<Vec<u32>> = tarjan_scc(&g)
            .into_iter()
            .map(|comp| {
                let mut v: Vec<u32> = comp.iter().map(|ix| ix.index() as u32).collect();
                v.sort_unstable();
                v
            })
            .collect();
        cpu_parts.sort_unstable();

        assert_eq!(
            gpu_parts, cpu_parts,
            "GPU SCC partition differs from tarjan"
        );
        let nt = gpu_parts.iter().filter(|p| p.len() > 1).count();
        eprintln!(
            "GPU SCC: {} components ({nt} non-trivial) == tarjan",
            gpu_parts.len()
        );
    }
}
