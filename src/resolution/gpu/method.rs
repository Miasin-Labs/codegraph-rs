use cudarc::driver::{LaunchConfig, PushKernelArg};

use super::GpuNameJoiner;

impl GpuNameJoiner {
    /// Tier-3: `match_method_call` strategies 1+2 on the GPU.
    #[allow(clippy::too_many_arguments)]
    pub fn match_class_methods(
        &self,
        ref_cand_starts: &[u32],
        ref_method_hash: &[u64],
        cls_file: &[u32],
        cls_name_off: &[u32],
        cls_name_len: &[u32],
        name_buf: &[u8],
        file_starts: &[u32],
        m_hash: &[u64],
        m_qn_off: &[u32],
        m_qn_len: &[u32],
        qn_buf: &[u8],
    ) -> Option<(Vec<i32>, Vec<i32>)> {
        let n = ref_method_hash.len();
        if n == 0 {
            return Some((Vec::new(), Vec::new()));
        }
        let d_rcs = self.stream.memcpy_stod(ref_cand_starts).ok()?;
        let d_rmh = self.stream.memcpy_stod(ref_method_hash).ok()?;
        let d_cf = self.stream.memcpy_stod(cls_file).ok()?;
        let d_cno = self.stream.memcpy_stod(cls_name_off).ok()?;
        let d_cnl = self.stream.memcpy_stod(cls_name_len).ok()?;
        let d_nb = self.stream.memcpy_stod(name_buf).ok()?;
        let d_fs = self.stream.memcpy_stod(file_starts).ok()?;
        let d_mh = self.stream.memcpy_stod(m_hash).ok()?;
        let d_mqo = self.stream.memcpy_stod(m_qn_off).ok()?;
        let d_mql = self.stream.memcpy_stod(m_qn_len).ok()?;
        let d_qb = self.stream.memcpy_stod(qn_buf).ok()?;
        let mut d_om = self.stream.alloc_zeros::<i32>(n).ok()?;
        let mut d_oc = self.stream.alloc_zeros::<i32>(n).ok()?;
        let n32 = n as u32;
        let cfg = LaunchConfig {
            grid_dim: (n32.div_ceil(256), 1, 1),
            block_dim: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let mut launch = self.stream.launch_builder(&self.method_kernel);
        launch.arg(&d_rcs);
        launch.arg(&d_rmh);
        launch.arg(&d_cf);
        launch.arg(&d_cno);
        launch.arg(&d_cnl);
        launch.arg(&d_nb);
        launch.arg(&d_fs);
        launch.arg(&d_mh);
        launch.arg(&d_mqo);
        launch.arg(&d_mql);
        launch.arg(&d_qb);
        launch.arg(&mut d_om);
        launch.arg(&mut d_oc);
        launch.arg(&n32);
        // SAFETY: per-thread writes are confined to out[i]; CSR arrays are
        // host-validated by the resolver before this launch path runs.
        unsafe { launch.launch(cfg) }.ok()?;
        Some((
            self.stream.memcpy_dtov(&d_om).ok()?,
            self.stream.memcpy_dtov(&d_oc).ok()?,
        ))
    }
}
