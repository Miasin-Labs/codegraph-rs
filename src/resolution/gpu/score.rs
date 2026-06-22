use cudarc::driver::{LaunchConfig, PushKernelArg};

use super::GpuNameJoiner;

impl GpuNameJoiner {
    /// Tier-2: full `find_best_match` candidate ranking on the GPU.
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
        // SAFETY: per-thread writes are confined to out_best[i]; CSR arrays
        // are host-validated by the resolver before this launch path runs.
        unsafe { launch.launch(cfg) }.ok()?;
        self.stream.memcpy_dtov(&d_best).ok()
    }
}
