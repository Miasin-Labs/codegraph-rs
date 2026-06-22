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

mod fuzzy;
mod hash;
mod joiner;
mod kernel;
mod method;
mod score;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use cudarc::driver::{CudaFunction, CudaStream};

/// A GPU-resident known-names table plus the compiled probe kernel.
pub struct GpuNameJoiner {
    pub(in crate::resolution::gpu) stream: Arc<CudaStream>,
    pub(in crate::resolution::gpu) kernel: CudaFunction,
    pub(in crate::resolution::gpu) score_kernel: CudaFunction,
    pub(in crate::resolution::gpu) method_kernel: CudaFunction,
    pub(in crate::resolution::gpu) fuzzy_kernel: CudaFunction,
    pub(in crate::resolution::gpu) table: cudarc::driver::CudaSlice<u64>,
    pub(in crate::resolution::gpu) mask: u64,
}
