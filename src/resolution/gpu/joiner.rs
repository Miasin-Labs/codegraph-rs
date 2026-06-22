use cudarc::driver::{CudaContext, LaunchConfig, PushKernelArg};
use cudarc::nvrtc::compile_ptx;

use super::GpuNameJoiner;
use super::hash::fnv1a64;
use super::kernel::KERNEL_SRC;
use crate::error::log_debug;

impl GpuNameJoiner {
    /// Probe for a usable CUDA device and build the names table on it.
    /// Returns `None` (never errors) when no GPU/driver is available.
    pub fn new(known_names: &[&str]) -> Option<Self> {
        if !codegraph_analysis::cuda_runtime_available() {
            return None;
        }
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
        let method_kernel = module.load_function("match_class_methods").ok()?;
        let fuzzy_kernel = module.load_function("fuzzy_unique").ok()?;

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
            method_kernel,
            fuzzy_kernel,
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
