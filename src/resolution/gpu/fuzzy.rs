use cudarc::driver::{LaunchConfig, PushKernelArg};

use super::GpuNameJoiner;

impl GpuNameJoiner {
    /// Tier-4: `match_fuzzy` uniqueness selection on the GPU.
    pub fn fuzzy_unique(
        &self,
        ref_group: &[i32],
        ref_lang: &[u8],
        cand_starts: &[u32],
        cand_lang: &[u8],
        cand_kind: &[u8],
    ) -> Option<(Vec<i32>, Vec<u8>)> {
        let n = ref_group.len();
        if n == 0 {
            return Some((Vec::new(), Vec::new()));
        }
        let d_g = self.stream.memcpy_stod(ref_group).ok()?;
        let d_rl = self.stream.memcpy_stod(ref_lang).ok()?;
        let d_cs = self.stream.memcpy_stod(cand_starts).ok()?;
        let d_cl = self.stream.memcpy_stod(cand_lang).ok()?;
        let d_ck = self.stream.memcpy_stod(cand_kind).ok()?;
        let mut d_oi = self.stream.alloc_zeros::<i32>(n).ok()?;
        let mut d_oc = self.stream.alloc_zeros::<u8>(n).ok()?;
        let n32 = n as u32;
        let cfg = LaunchConfig {
            grid_dim: (n32.div_ceil(256), 1, 1),
            block_dim: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let mut launch = self.stream.launch_builder(&self.fuzzy_kernel);
        launch.arg(&d_g);
        launch.arg(&d_rl);
        launch.arg(&d_cs);
        launch.arg(&d_cl);
        launch.arg(&d_ck);
        launch.arg(&mut d_oi);
        launch.arg(&mut d_oc);
        launch.arg(&n32);
        // SAFETY: per-thread writes are confined to out[i]; CSR arrays are
        // host-validated by the resolver before this launch path runs.
        unsafe { launch.launch(cfg) }.ok()?;
        Some((
            self.stream.memcpy_dtov(&d_oi).ok()?,
            self.stream.memcpy_dtov(&d_oc).ok()?,
        ))
    }
}
