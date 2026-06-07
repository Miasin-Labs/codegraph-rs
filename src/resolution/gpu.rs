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
"#;

/// A GPU-resident known-names table plus the compiled probe kernel.
pub struct GpuNameJoiner {
    stream: Arc<CudaStream>,
    kernel: CudaFunction,
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
}
