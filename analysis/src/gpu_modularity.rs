//! GPU modularity objective for community detection (feature `gpu`).
//!
//! Louvain's `local_moves` optimisation is irreducibly sequential — each
//! node's best move depends on the communities left by the moves applied
//! before it (in a seeded order), so a faithful, deterministic Louvain stays
//! on the CPU. Its OBJECTIVE, modularity Q, is order-independent and
//! parallel: per community c, Q = Σ_c L_c/(2m) − γ·Σ_c (D_c/2m)², where L_c is
//! internal weight and D_c is total degree. This computes the per-community
//! degree and internal-weight sums on the GPU.
//!
//! Determinism: weights are accumulated in fixed-point i64 (integer atomicAdd
//! IS associative, unlike float), so the result is identical run-to-run and
//! bit-exact against the CPU for the integer edge weights real call graphs
//! carry (default 1.0). The differential test asserts that equality.

use cudarc::driver::{CudaContext, CudaFunction, LaunchConfig, PushKernelArg};
use cudarc::nvrtc::compile_ptx;

/// Fixed-point scale (2^20): exact for integer weights, ~1e-6 for fractional.
const SCALE: f64 = 1_048_576.0;

const KERNEL_SRC: &str = r#"
typedef unsigned int u32;
typedef long long i64;

// Per-node: add fixed-point degree into its community bucket.
extern "C" __global__ void accum_degree(
    const u32* __restrict__ community,
    const i64* __restrict__ degree_fx,
    i64*       __restrict__ comm_degree,
    u32 n
) {
    u32 i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= n) return;
    atomicAdd((unsigned long long*)&comm_degree[community[i]],
              (unsigned long long)degree_fx[i]);
}

// Per directed edge (i->j): if same community, add fixed-point weight to that
// community's internal bucket. Each undirected edge appears twice (i->j, j->i)
// so this yields internal_2x, halved on the host.
extern "C" __global__ void accum_internal(
    const u32* __restrict__ esrc,
    const u32* __restrict__ edst,
    const i64* __restrict__ eweight_fx,
    const u32* __restrict__ community,
    i64*       __restrict__ comm_internal_2x,
    u32 m
) {
    u32 e = blockIdx.x * blockDim.x + threadIdx.x;
    if (e >= m) return;
    u32 i = esrc[e], j = edst[e];
    if (community[i] == community[j]) {
        atomicAdd((unsigned long long*)&comm_internal_2x[community[i]],
                  (unsigned long long)eweight_fx[e]);
    }
}
"#;

/// Compute modularity Q on the GPU from a flattened weighted graph.
///
/// `community` is the per-node community id (0..k). `esrc`/`edst`/`eweight`
/// are directed edges — each undirected edge given in BOTH directions, exactly
/// as the CPU adjacency stores it. Returns `None` when no CUDA device.
pub fn modularity_gpu(
    n: usize,
    community: &[u32],
    esrc: &[u32],
    edst: &[u32],
    eweight: &[f64],
    resolution: f64,
) -> Option<f64> {
    if n == 0 {
        return Some(0.0);
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        modularity_gpu_inner(n, community, esrc, edst, eweight, resolution)
    }))
    .ok()
    .flatten()
}

fn modularity_gpu_inner(
    n: usize,
    community: &[u32],
    esrc: &[u32],
    edst: &[u32],
    eweight: &[f64],
    resolution: f64,
) -> Option<f64> {
    let ctx = CudaContext::new(0).ok()?;
    let stream = ctx.default_stream();
    let module = ctx.load_module(compile_ptx(KERNEL_SRC).ok()?).ok()?;
    let accum_degree: CudaFunction = module.load_function("accum_degree").ok()?;
    let accum_internal: CudaFunction = module.load_function("accum_internal").ok()?;

    // Fixed-point conversion; node degree = sum of incident edge weights.
    let eweight_fx: Vec<i64> = eweight.iter().map(|w| (w * SCALE).round() as i64).collect();
    let mut degree_fx = vec![0i64; n];
    for (e, &w) in eweight_fx.iter().enumerate() {
        degree_fx[esrc[e] as usize] += w;
    }
    // total edge weight m = (Σ directed weights) / 2.
    let total_fx: i64 = eweight_fx.iter().sum();
    let two_m = total_fx as f64 / SCALE; // = 2*m
    if two_m == 0.0 {
        return Some(0.0);
    }

    let d_comm = stream.memcpy_stod(community).ok()?;
    let d_degfx = stream.memcpy_stod(&degree_fx).ok()?;
    let d_esrc = stream.memcpy_stod(esrc).ok()?;
    let d_edst = stream.memcpy_stod(edst).ok()?;
    let d_ewfx = stream.memcpy_stod(&eweight_fx).ok()?;
    let mut d_cdeg = stream.alloc_zeros::<i64>(n).ok()?;
    let mut d_cint = stream.alloc_zeros::<i64>(n).ok()?;

    let nu = n as u32;
    let mu = esrc.len() as u32;
    let vcfg = LaunchConfig {
        grid_dim: (nu.div_ceil(256).max(1), 1, 1),
        block_dim: (256, 1, 1),
        shared_mem_bytes: 0,
    };
    let ecfg = LaunchConfig {
        grid_dim: (mu.div_ceil(256).max(1), 1, 1),
        block_dim: (256, 1, 1),
        shared_mem_bytes: 0,
    };

    let mut l = stream.launch_builder(&accum_degree);
    l.arg(&d_comm);
    l.arg(&d_degfx);
    l.arg(&mut d_cdeg);
    l.arg(&nu);
    // SAFETY: per-node integer atomicAdd into n-sized bucket array.
    unsafe { l.launch(vcfg) }.ok()?;

    let mut l = stream.launch_builder(&accum_internal);
    l.arg(&d_esrc);
    l.arg(&d_edst);
    l.arg(&d_ewfx);
    l.arg(&d_comm);
    l.arg(&mut d_cint);
    l.arg(&mu);
    // SAFETY: per-edge integer atomicAdd into n-sized bucket array.
    unsafe { l.launch(ecfg) }.ok()?;

    let cdeg = stream.memcpy_dtov(&d_cdeg).ok()?;
    let cint = stream.memcpy_dtov(&d_cint).ok()?;

    // Q = Σ_c L_c/(2m) − γ·Σ_c (D_c/2m)², L_c = internal_2x/2.
    let mut q = 0.0f64;
    for c in 0..n {
        if cint[c] != 0 {
            let l_c = (cint[c] as f64 / SCALE) / 2.0;
            q += l_c / two_m;
        }
    }
    for c in 0..n {
        if cdeg[c] != 0 {
            let d_c = cdeg[c] as f64 / SCALE;
            q -= resolution * (d_c / two_m).powi(2);
        }
    }
    Some(q)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// CPU modularity (mirror of communities::compute_modularity).
    fn cpu_modularity(
        n: usize,
        community: &[u32],
        esrc: &[u32],
        edst: &[u32],
        eweight: &[f64],
        resolution: f64,
    ) -> f64 {
        let total: f64 = eweight.iter().sum();
        let m = total / 2.0;
        if m == 0.0 {
            return 0.0;
        }
        let mut degree = vec![0.0f64; n];
        for (e, &w) in eweight.iter().enumerate() {
            degree[esrc[e] as usize] += w;
        }
        let mut ci: HashMap<u32, f64> = HashMap::new();
        let mut cd: HashMap<u32, f64> = HashMap::new();
        for i in 0..n {
            *cd.entry(community[i]).or_insert(0.0) += degree[i];
        }
        for e in 0..esrc.len() {
            if community[esrc[e] as usize] == community[edst[e] as usize] {
                *ci.entry(community[esrc[e] as usize]).or_insert(0.0) += eweight[e];
            }
        }
        let mut q = 0.0;
        for (_c, &i2) in &ci {
            q += (i2 / 2.0) / (2.0 * m);
        }
        for (_c, &d) in &cd {
            q -= resolution * (d / (2.0 * m)).powi(2);
        }
        q
    }

    #[test]
    fn gpu_modularity_matches_cpu() {
        let n = 1200usize;
        let mut state = 0x5eed_1234u64;
        let mut rnd = |m: u64| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) % m
        };
        // undirected edges stored both ways, unit weights (real call-graph shape)
        let (mut esrc, mut edst, mut ew) = (Vec::new(), Vec::new(), Vec::new());
        for _ in 0..(n * 3) {
            let a = rnd(n as u64) as u32;
            let b = rnd(n as u64) as u32;
            if a != b {
                esrc.push(a);
                edst.push(b);
                ew.push(1.0);
                esrc.push(b);
                edst.push(a);
                ew.push(1.0);
            }
        }
        // random community assignment in 0..40
        let community: Vec<u32> = (0..n).map(|_| rnd(40) as u32).collect();

        let cpu = cpu_modularity(n, &community, &esrc, &edst, &ew, 1.0);
        let Some(gpu) = modularity_gpu(n, &community, &esrc, &edst, &ew, 1.0) else {
            eprintln!("no CUDA device — skipping GPU modularity test");
            return;
        };
        // Unit weights → fixed-point is exact → bit-exact up to f64 final-sum
        // ordering (a few ULPs).
        assert!(
            (gpu - cpu).abs() < 1e-9,
            "GPU modularity {gpu} vs CPU {cpu}"
        );
        eprintln!("GPU modularity {gpu:.6} == CPU {cpu:.6}");
    }
}
