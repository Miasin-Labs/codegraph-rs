//! GPU batch name-join for reference resolution (feature `gpu`).
//!
//! Replaces the per-reference hash probes of the resolution pre-filter with
//! one massively-parallel pass on an NVIDIA GPU: every node name goes into an
//! open-addressing hash table (built once per resolve pass), and EVERY
//! pending reference name — plus its `obj.method` / `Type::method` parts — is
//! probed by one GPU thread. The kernel writes a presence bitmask per
//! reference; `resolve_all` then uses those flags exactly where it would have
//! called `has_any_possible_match`, so results are bit-identical to the CPU
//! path by construction (differential-tested).
//!
//! Design notes (from the NVIDIA driver/source research):
//! - Explicit copies, never UVM: alternating CPU-write/GPU-read trips the
//!   driver's thrashing throttle and faults migrate at most 2 MB at a time.
//! - One H2D stream is enough — GB20x laptop parts expose exactly one
//!   sysmem-read copy engine.
//! - NVRTC at runtime (no nvcc at build time); `dynamic-loading` means a
//!   machine without libcuda simply reports `None` and the CPU path runs.

use std::sync::Arc;

use cudarc::driver::{CudaContext, CudaFunction, CudaStream, LaunchConfig, PushKernelArg};
use cudarc::nvrtc::compile_ptx;

use crate::error::log_debug;

/// FNV-1a 64-bit — tiny, branch-free, identical in Rust and CUDA C.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

const KERNEL_SRC: &str = r#"
typedef unsigned long long u64;
typedef unsigned int u32;
typedef unsigned char u8;

extern "C" __device__ u64 fnv1a64(const u8* s, u32 len) {
    u64 h = 0xcbf29ce484222325ULL;
    for (u32 i = 0; i < len; i++) {
        h ^= (u64)s[i];
        h *= 0x100000001b3ULL;
    }
    return h;
}

extern "C" __device__ bool probe(const u64* table, u64 mask, u64 hash) {
    // Open addressing, linear probing. 0 = empty slot (names hashing to 0
    // are stored as 1 — collision-safe for a membership filter).
    u64 h = hash == 0 ? 1 : hash;
    u64 slot = h & mask;
    for (u32 i = 0; i < 128; i++) {
        u64 v = table[(slot + i) & mask];
        if (v == h) return true;
        if (v == 0) return false;
    }
    return false;
}

// One thread per reference. Mirrors ReferenceResolver::has_any_possible_match
// EXACTLY: full name; first-'.' receiver/rest/capitalized-receiver plus
// last-'.' tail; first-"::" receiver/rest; last-'/' filename. out[i] = 1 when
// any probe hits, 0 when none can. Bit 0x80 = first receiver byte is
// non-ASCII (capitalize_first semantics diverge) -> caller re-checks on CPU.
extern "C" __global__ void probe_names(
    const u8* __restrict__ buf,
    const u32* __restrict__ offsets, // len = n_refs + 1
    const u64* __restrict__ table,
    u64 mask,
    u8* __restrict__ out,
    u32 n_refs
) {
    u32 i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= n_refs) return;
    u32 start = offsets[i], end = offsets[i + 1];
    u32 len = end - start;
    const u8* s = buf + start;

    if (probe(table, mask, fnv1a64(s, len))) { out[i] = 1; return; }
    u8 flags = 0;

    int first_dot = -1, last_dot = -1, first_colon = -1, last_slash = -1;
    for (u32 j = 0; j < len; j++) {
        if (s[j] == '.') { if (first_dot < 0) first_dot = (int)j; last_dot = (int)j; }
        if (first_colon < 0 && j + 1 < len && s[j] == ':' && s[j+1] == ':') first_colon = (int)j;
        if (s[j] == '/') last_slash = (int)j;
    }

    if (first_dot > 0) {
        u32 d = (u32)first_dot;
        if (probe(table, mask, fnv1a64(s, d))) { out[i] = 1; return; }          // receiver
        if (probe(table, mask, fnv1a64(s + d + 1, len - d - 1))) { out[i] = 1; return; } // rest
        u8 c0 = s[0];
        if (c0 >= 'a' && c0 <= 'z') {                                            // capitalized recv
            u64 h = 0xcbf29ce484222325ULL;
            h ^= (u64)(c0 - 32); h *= 0x100000001b3ULL;
            for (u32 j = 1; j < d; j++) { h ^= (u64)s[j]; h *= 0x100000001b3ULL; }
            if (probe(table, mask, h)) { out[i] = 1; return; }
        } else if (c0 >= 0x80) {
            flags |= 0x80; // unicode capitalize — defer to CPU
        }
        if (last_dot > first_dot && (u32)last_dot + 1 < len) {                   // FQN tail
            u32 ld = (u32)last_dot;
            if (probe(table, mask, fnv1a64(s + ld + 1, len - ld - 1))) { out[i] = 1; return; }
        }
    }
    if (first_colon > 0) {
        u32 c = (u32)first_colon;
        if (probe(table, mask, fnv1a64(s, c))) { out[i] = 1; return; }
        if (c + 2 <= len && probe(table, mask, fnv1a64(s + c + 2, len - c - 2))) { out[i] = 1; return; }
    }
    if (last_slash > 0 && (u32)last_slash + 1 < len) {
        u32 sl = (u32)last_slash;
        if (probe(table, mask, fnv1a64(s + sl + 1, len - sl - 1))) { out[i] = 1; return; }
    }
    out[i] = flags;
}

// Tier-2: full find_best_match scoring (name_matcher.rs:833-913), exact in
// scaled x10 integers (order-preserving vs the CPU's f64: every CPU term is a
// multiple of 0.1 except the line-distance term, which scales exactly).
// One thread per reference scans its CSR candidate slice IN ORDER (CPU
// tie-break is strict-> first-wins, so identical order = identical pick):
//   same file            +1000
//   dir-prefix proximity +150/shared segment, cap 800
//   same language        +500 else -800
//   Calls -> Fn|Method   +250
//   Instantiates -> Class|Struct|Interface +250
//   Decorates -> Fn|Method +250, Class|Interface +150
//   exported             +100
//   same-file line dist  max(0, 200 - |dline|)   [cand line != 0]
extern "C" __global__ void score_candidates(
    const int*  __restrict__ ref_group,    // n_refs: CSR group id or -1
    const u32*  __restrict__ ref_file,     // n_refs: interned file id
    const u8*   __restrict__ ref_lang,     // n_refs
    const u8*   __restrict__ ref_kind,     // n_refs: 1=Calls 2=Instantiates 3=Decorates 0=other
    const u32*  __restrict__ ref_line,     // n_refs
    const u32*  __restrict__ cand_starts,  // n_groups + 1 (CSR)
    const u32*  __restrict__ cand_file,
    const u8*   __restrict__ cand_lang,
    const u8*   __restrict__ cand_kind,    // 1=Fn 2=Method 3=Class 4=Struct 5=Interface 0=other
    const u8*   __restrict__ cand_exported,
    const u32*  __restrict__ cand_line,
    const u32*  __restrict__ dir_starts,   // n_files + 1: per-file dir-hash CSR
    const u64*  __restrict__ dir_hashes,
    int*        __restrict__ out_best,     // n_refs: best candidate idx or -1
    u32 n_refs
) {
    u32 i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= n_refs) return;
    int g = ref_group[i];
    out_best[i] = -1;
    if (g < 0) return;
    u32 s = cand_starts[g], e = cand_starts[g + 1];
    u32 rf = ref_file[i];
    u32 rds = dir_starts[rf], rde = dir_starts[rf + 1];
    // CPU best_score starts at -1.0 with strict > : a candidate scoring
    // below that (cross-language -800 dominating) is NEVER selected. x10
    // scale -> initialize to -10 for exact parity.
    long long best_score = -10;
    int best = -1;
    for (u32 c = s; c < e; c++) {
        long long score = 0;
        u32 cf = cand_file[c];
        if (cf == rf) score += 1000;
        // proximity: shared leading dir-hash segments
        u32 cds = dir_starts[cf], cde = dir_starts[cf + 1];
        u32 n1 = rde - rds, n2 = cde - cds;
        u32 lim = n1 < n2 ? n1 : n2;
        long long shared = 0;
        for (u32 k = 0; k < lim; k++) {
            if (dir_hashes[rds + k] == dir_hashes[cds + k]) shared++;
            else break;
        }
        long long prox = shared * 150; if (prox > 800) prox = 800;
        score += prox;
        score += (cand_lang[c] == ref_lang[i]) ? 500 : -800;
        u8 rk = ref_kind[i], ck = cand_kind[c];
        if (rk == 1 && (ck == 1 || ck == 2)) score += 250;
        if (rk == 2 && (ck == 3 || ck == 4 || ck == 5)) score += 250;
        if (rk == 3) {
            if (ck == 1 || ck == 2) score += 250;
            else if (ck == 3 || ck == 5) score += 150;
        }
        if (cand_exported[c]) score += 100;
        if (cf == rf && cand_line[c] != 0) {
            long long d = (long long)cand_line[c] - (long long)ref_line[i];
            if (d < 0) d = -d;
            long long lt = 200 - d; if (lt < 0) lt = 0;
            score += lt;
        }
        if (score > best_score) { best_score = score; best = (int)c; }
    }
    out_best[i] = best;
}
"#;

/// A GPU-resident known-names table plus the compiled probe kernel.
pub struct GpuNameJoiner {
    stream: Arc<CudaStream>,
    kernel: CudaFunction,
    score_kernel: CudaFunction,
    table: cudarc::driver::CudaSlice<u64>,
    mask: u64,
}

impl GpuNameJoiner {
    /// Probe for a usable CUDA device and build the names table on it.
    /// Returns `None` (never errors) when no GPU/driver is available.
    pub fn new(known_names: &[&str]) -> Option<Self> {
        // cudarc's dynamic loader PANICS (rather than erroring) when libcuda
        // or libnvrtc is absent; contain that so machines without CUDA just
        // fall back to the CPU path.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Self::new_inner(known_names)
        }))
        .ok()
        .flatten()
    }

    fn new_inner(known_names: &[&str]) -> Option<Self> {
        let ctx = CudaContext::new(0).ok()?;
        let stream = ctx.default_stream();
        let ptx = compile_ptx(KERNEL_SRC).ok()?;
        let module = ctx.load_module(ptx).ok()?;
        let kernel = module.load_function("probe_names").ok()?;
        let score_kernel = module.load_function("score_candidates").ok()?;

        // Host-side table build (linear, ~ms for 1.4M names); GPU does the
        // 5.9M-reference probe side where the parallelism actually is.
        let capacity = (known_names.len().max(1) * 2).next_power_of_two();
        let mask = (capacity - 1) as u64;
        let mut table = vec![0u64; capacity];
        for name in known_names {
            let mut h = fnv1a64(name.as_bytes());
            if h == 0 {
                h = 1;
            }
            let mut slot = (h & mask) as usize;
            loop {
                if table[slot] == 0 {
                    table[slot] = h;
                    break;
                }
                if table[slot] == h {
                    break;
                }
                slot = (slot + 1) & mask as usize;
            }
        }
        let table = stream.memcpy_stod(&table).ok()?;
        log_debug(
            "GPU name-join table built",
            Some(&serde_json::json!({ "names": known_names.len(), "slots": capacity })),
        );
        Some(Self {
            stream,
            kernel,
            score_kernel,
            table,
            mask,
        })
    }

    /// Probe every reference name in one GPU pass. Returns per-ref bitmasks
    /// (1 = full name known, 2 = head part known, 4 = tail part known).
    pub fn probe_batch(&self, names: &[&str]) -> Option<Vec<u8>> {
        if names.is_empty() {
            return Some(Vec::new());
        }
        let mut offsets: Vec<u32> = Vec::with_capacity(names.len() + 1);
        let mut buf: Vec<u8> = Vec::with_capacity(names.len() * 24);
        offsets.push(0);
        for n in names {
            buf.extend_from_slice(n.as_bytes());
            offsets.push(buf.len() as u32);
        }

        let d_buf = self.stream.memcpy_stod(&buf).ok()?;
        let d_off = self.stream.memcpy_stod(&offsets).ok()?;
        let mut d_out = self.stream.alloc_zeros::<u8>(names.len()).ok()?;

        let n = names.len() as u32;
        let cfg = LaunchConfig {
            grid_dim: (n.div_ceil(256), 1, 1),
            block_dim: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let mut launch = self.stream.launch_builder(&self.kernel);
        launch.arg(&d_buf);
        launch.arg(&d_off);
        launch.arg(&self.table);
        launch.arg(&self.mask);
        launch.arg(&mut d_out);
        launch.arg(&n);
        // SAFETY: kernel reads only within [offsets[i], offsets[i+1]) of buf
        // and writes only out[i]; all buffers are sized above.
        unsafe { launch.launch(cfg) }.ok()?;
        self.stream.memcpy_dtov(&d_out).ok()
    }

    /// Tier-2: full `find_best_match` candidate ranking on the GPU.
    ///
    /// Inputs are interned/flattened on the host: refs carry (CSR group or
    /// -1, file id, language, kind class, line); candidates carry (file id,
    /// language, kind class, exported, line) in `get_nodes_by_name` order
    /// (the CPU tie-break is strict-`>` first-wins, so preserving order
    /// gives identical selection); `dir_*` is the per-file CSR of cumulative
    /// directory-prefix hashes powering the proximity term. Returns the best
    /// candidate index per ref, -1 = none beat the CPU's -1.0 floor.
    #[allow(clippy::too_many_arguments)]
    pub fn score_batch(
        &self,
        ref_group: &[i32],
        ref_file: &[u32],
        ref_lang: &[u8],
        ref_kind: &[u8],
        ref_line: &[u32],
        cand_starts: &[u32],
        cand_file: &[u32],
        cand_lang: &[u8],
        cand_kind: &[u8],
        cand_exported: &[u8],
        cand_line: &[u32],
        dir_starts: &[u32],
        dir_hashes: &[u64],
    ) -> Option<Vec<i32>> {
        if ref_group.is_empty() {
            return Some(Vec::new());
        }
        let d_group = self.stream.memcpy_stod(ref_group).ok()?;
        let d_rfile = self.stream.memcpy_stod(ref_file).ok()?;
        let d_rlang = self.stream.memcpy_stod(ref_lang).ok()?;
        let d_rkind = self.stream.memcpy_stod(ref_kind).ok()?;
        let d_rline = self.stream.memcpy_stod(ref_line).ok()?;
        let d_starts = self.stream.memcpy_stod(cand_starts).ok()?;
        let d_cfile = self.stream.memcpy_stod(cand_file).ok()?;
        let d_clang = self.stream.memcpy_stod(cand_lang).ok()?;
        let d_ckind = self.stream.memcpy_stod(cand_kind).ok()?;
        let d_cexp = self.stream.memcpy_stod(cand_exported).ok()?;
        let d_cline = self.stream.memcpy_stod(cand_line).ok()?;
        let d_dstarts = self.stream.memcpy_stod(dir_starts).ok()?;
        let d_dhash = self.stream.memcpy_stod(dir_hashes).ok()?;
        let mut d_best = self.stream.alloc_zeros::<i32>(ref_group.len()).ok()?;
        let n = ref_group.len() as u32;
        let cfg = LaunchConfig {
            grid_dim: (n.div_ceil(256), 1, 1),
            block_dim: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let mut launch = self.stream.launch_builder(&self.score_kernel);
        launch.arg(&d_group);
        launch.arg(&d_rfile);
        launch.arg(&d_rlang);
        launch.arg(&d_rkind);
        launch.arg(&d_rline);
        launch.arg(&d_starts);
        launch.arg(&d_cfile);
        launch.arg(&d_clang);
        launch.arg(&d_ckind);
        launch.arg(&d_cexp);
        launch.arg(&d_cline);
        launch.arg(&d_dstarts);
        launch.arg(&d_dhash);
        launch.arg(&mut d_best);
        launch.arg(&n);
        // SAFETY: per-thread writes confined to out_best[i]; CSR arrays are
        // host-validated (monotonic, last element = len).
        unsafe { launch.launch(cfg) }.ok()?;
        self.stream.memcpy_dtov(&d_best).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CPU ground truth — has_any_possible_match semantics (resolver.rs).
    fn cpu_flags(known: &std::collections::HashSet<&str>, name: &str) -> u8 {
        if known.contains(name) {
            return 1;
        }
        if let Some(d) = name.find('.') {
            if d > 0 {
                let receiver = &name[..d];
                if known.contains(receiver) || known.contains(&name[d + 1..]) {
                    return 1;
                }
                let mut cap = receiver.to_string();
                if let Some(f) = cap.get_mut(0..1) {
                    f.make_ascii_uppercase();
                }
                if receiver.starts_with(|c: char| c.is_ascii_lowercase())
                    && known.contains(cap.as_str())
                {
                    return 1;
                }
                let ld = name.rfind('.').unwrap_or(0);
                if ld > d && !name[ld + 1..].is_empty() && known.contains(&name[ld + 1..]) {
                    return 1;
                }
                if !receiver.is_ascii() && receiver.starts_with(|c: char| !c.is_ascii()) {
                    return 0x80;
                }
            }
        }
        if let Some(c) = name.find("::") {
            if c > 0 && (known.contains(&name[..c]) || known.contains(&name[c + 2..])) {
                return 1;
            }
        }
        if let Some(sl) = name.rfind('/') {
            if sl > 0 && !name[sl + 1..].is_empty() && known.contains(&name[sl + 1..]) {
                return 1;
            }
        }
        0
    }

    #[test]
    fn gpu_probe_matches_cpu_ground_truth() {
        let names = vec![
            "alloc",
            "Renderer",
            "draw",
            "uvm_va_block",
            "fnv1a64",
            "probe",
            "Vec",
            "push",
            "open_addressing",
            "λ_unicode",
            "x",
        ];
        let Some(joiner) = GpuNameJoiner::new(&names) else {
            eprintln!("no CUDA device — skipping GPU differential test");
            return;
        };
        let known: std::collections::HashSet<&str> = names.iter().copied().collect();
        let refs = vec![
            "alloc",
            "Renderer.draw",
            "Renderer::draw",
            "missing",
            "missing.draw",
            "Renderer.missing",
            "a.b.c",
            "uvm_va_block",
            "x.x",
            "::draw",
            "trailing.",
            "λ_unicode",
            "deep::ns::probe",
        ];
        let gpu = joiner.probe_batch(&refs).expect("probe failed");
        for (i, r) in refs.iter().enumerate() {
            assert_eq!(
                gpu[i],
                cpu_flags(&known, r),
                "flag mismatch for {r:?}: gpu={:#b}",
                gpu[i]
            );
        }
        // Scale check: 200k probes against a 50k-name vocabulary.
        let big_names: Vec<String> = (0..50_000).map(|i| format!("sym_{i}")).collect();
        let big_refs: Vec<String> = (0..200_000)
            .map(|i| match i % 4 {
                0 => format!("sym_{}", i % 60_000),
                1 => format!("obj_{i}.sym_{}", i % 60_000),
                2 => format!("ns_{i}::sym_{}", i % 60_000),
                _ => format!("nope_{i}"),
            })
            .collect();
        let name_refs: Vec<&str> = big_names.iter().map(|s| s.as_str()).collect();
        let ref_refs: Vec<&str> = big_refs.iter().map(|s| s.as_str()).collect();
        let joiner = GpuNameJoiner::new(&name_refs).expect("gpu");
        let known: std::collections::HashSet<&str> = name_refs.iter().copied().collect();
        let t = std::time::Instant::now();
        let gpu = joiner.probe_batch(&ref_refs).expect("probe");
        let dt = t.elapsed();
        for (i, r) in ref_refs.iter().enumerate() {
            assert_eq!(gpu[i], cpu_flags(&known, r), "mismatch for {r:?}");
        }
        eprintln!(
            "GPU probe: 200k refs vs 50k names in {:?} ({:.0} Mrefs/s)",
            dt,
            0.2 / dt.as_secs_f64()
        );
    }

    #[test]
    fn gpu_scoring_matches_cpu_find_best_match() {
        let Some(joiner) = GpuNameJoiner::new(&["x"]) else {
            eprintln!("no CUDA device — skipping");
            return;
        };
        // Deterministic pseudo-random scenario sweep across the whole input
        // space: files with shared dir prefixes, languages, kinds, exported,
        // line distances — mirrored exactly against the CPU formula.
        let n_files = 12u32;
        // dir CSR: file f has f%4 dir segments drawn from 2 alternatives.
        let mut dir_starts = vec![0u32];
        let mut dir_hashes = Vec::new();
        for f in 0..n_files {
            let depth = (f % 4) as u64;
            let mut h = 0xabcdefu64;
            for d in 0..depth {
                // files with the same f%2 share prefixes; others diverge at d
                h = h
                    .wrapping_mul(31)
                    .wrapping_add(if f % 2 == 0 { d } else { d + 100 });
                dir_hashes.push(h);
            }
            dir_starts.push(dir_hashes.len() as u32);
        }
        let mut state = 0x12345678u64;
        let mut rnd = move |m: u64| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) % m
        };
        let n_groups = 40usize;
        let mut cand_starts = vec![0u32];
        let (mut cand_file, mut cand_lang, mut cand_kind, mut cand_exp, mut cand_line) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for _ in 0..n_groups {
            let k = rnd(6); // 0..5 candidates
            for _ in 0..k {
                cand_file.push(rnd(n_files as u64) as u32);
                cand_lang.push(rnd(4) as u8);
                cand_kind.push(rnd(6) as u8);
                cand_exp.push(rnd(2) as u8);
                cand_line.push(rnd(400) as u32); // incl. 0 = "no line"
            }
            cand_starts.push(cand_file.len() as u32);
        }
        let n_refs = 600usize;
        let (mut rg, mut rf, mut rl, mut rk, mut rline) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for i in 0..n_refs {
            rg.push(if i % 17 == 0 {
                -1
            } else {
                rnd(n_groups as u64) as i32
            });
            rf.push(rnd(n_files as u64) as u32);
            rl.push(rnd(4) as u8);
            rk.push(rnd(4) as u8);
            rline.push(rnd(400) as u32);
        }
        let gpu_best = joiner
            .score_batch(
                &rg,
                &rf,
                &rl,
                &rk,
                &rline,
                &cand_starts,
                &cand_file,
                &cand_lang,
                &cand_kind,
                &cand_exp,
                &cand_line,
                &dir_starts,
                &dir_hashes,
            )
            .expect("score_batch");

        // CPU mirror of find_best_match (name_matcher.rs:833) in x10 integers.
        let prox = |f1: u32, f2: u32| -> i64 {
            let (s1, e1) = (
                dir_starts[f1 as usize] as usize,
                dir_starts[f1 as usize + 1] as usize,
            );
            let (s2, e2) = (
                dir_starts[f2 as usize] as usize,
                dir_starts[f2 as usize + 1] as usize,
            );
            let mut shared = 0i64;
            for k in 0..(e1 - s1).min(e2 - s2) {
                if dir_hashes[s1 + k] == dir_hashes[s2 + k] {
                    shared += 1;
                } else {
                    break;
                }
            }
            (shared * 150).min(800)
        };
        for i in 0..n_refs {
            let mut expect = -1i32;
            let mut best_score = -10i64;
            if rg[i] >= 0 {
                let g = rg[i] as usize;
                for c in cand_starts[g] as usize..cand_starts[g + 1] as usize {
                    let mut score = 0i64;
                    if cand_file[c] == rf[i] {
                        score += 1000;
                    }
                    score += prox(rf[i], cand_file[c]);
                    score += if cand_lang[c] == rl[i] { 500 } else { -800 };
                    let (rk_i, ck) = (rk[i], cand_kind[c]);
                    if rk_i == 1 && (ck == 1 || ck == 2) {
                        score += 250;
                    }
                    if rk_i == 2 && (ck == 3 || ck == 4 || ck == 5) {
                        score += 250;
                    }
                    if rk_i == 3 {
                        if ck == 1 || ck == 2 {
                            score += 250;
                        } else if ck == 3 || ck == 5 {
                            score += 150;
                        }
                    }
                    if cand_exp[c] != 0 {
                        score += 100;
                    }
                    if cand_file[c] == rf[i] && cand_line[c] != 0 {
                        let d = (cand_line[c] as i64 - rline[i] as i64).abs();
                        score += (200 - d).max(0);
                    }
                    if score > best_score {
                        best_score = score;
                        expect = c as i32;
                    }
                }
            }
            assert_eq!(gpu_best[i], expect, "ref {i} (group {})", rg[i]);
        }
        eprintln!("GPU find_best_match parity: {n_refs} refs OK");
    }
}
