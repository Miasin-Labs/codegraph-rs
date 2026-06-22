use crate::resolution::gpu::GpuNameJoiner;
use crate::resolution::gpu::hash::fnv1a64;

#[test]
#[allow(clippy::needless_range_loop)] // absolute indices ARE the kernel's output contract
fn gpu_method_match_matches_cpu_find_semantics() {
    let Some(joiner) = GpuNameJoiner::new(&["x"]) else {
        eprintln!("no CUDA device — skipping");
        return;
    };
    let mut state = 0xdeadbeefu64;
    let mut rnd = move |m: u64| {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) % m
    };
    let n_files = 10u32;
    let vocab = ["run", "init", "draw", "save"];
    let classes = ["Renderer", "Engine", "Store", "Widget"];
    let mut file_starts = vec![0u32];
    let (mut m_hash, mut m_qn_off, mut m_qn_len) = (Vec::new(), Vec::new(), Vec::new());
    let mut qn_buf: Vec<u8> = Vec::new();
    let mut m_meta: Vec<(u32, String, String)> = Vec::new();
    for f in 0..n_files {
        for _ in 0..rnd(7) {
            let name = vocab[rnd(4) as usize];
            let cls = classes[rnd(4) as usize];
            let qn = format!("{cls}.{name}");
            m_hash.push(fnv1a64(name.as_bytes()));
            m_qn_off.push(qn_buf.len() as u32);
            m_qn_len.push(qn.len() as u32);
            qn_buf.extend_from_slice(qn.as_bytes());
            m_meta.push((f, name.to_string(), qn));
        }
        file_starts.push(m_hash.len() as u32);
    }
    let n_refs = 300usize;
    let mut ref_cand_starts = vec![0u32];
    let (mut cls_file, mut cls_name_off, mut cls_name_len) = (Vec::new(), Vec::new(), Vec::new());
    let mut name_buf: Vec<u8> = Vec::new();
    let (mut ref_mh, mut ref_meta) = (Vec::new(), Vec::new());
    let mut cls_meta: Vec<(u32, String)> = Vec::new();
    for _ in 0..n_refs {
        let mname = vocab[rnd(4) as usize];
        ref_mh.push(fnv1a64(mname.as_bytes()));
        for _ in 0..rnd(4) {
            let cls = classes[rnd(4) as usize];
            let f = rnd(n_files as u64) as u32;
            cls_file.push(f);
            cls_name_off.push(name_buf.len() as u32);
            cls_name_len.push(cls.len() as u32);
            name_buf.extend_from_slice(cls.as_bytes());
            cls_meta.push((f, cls.to_string()));
        }
        ref_cand_starts.push(cls_file.len() as u32);
        ref_meta.push(mname.to_string());
    }
    let (gm, gc) = joiner
        .match_class_methods(
            &ref_cand_starts,
            &ref_mh,
            &cls_file,
            &cls_name_off,
            &cls_name_len,
            &name_buf,
            &file_starts,
            &m_hash,
            &m_qn_off,
            &m_qn_len,
            &qn_buf,
        )
        .expect("kernel");
    for i in 0..n_refs {
        let mut em = -1i32;
        let mut ec = -1i32;
        'outer: for c in ref_cand_starts[i] as usize..ref_cand_starts[i + 1] as usize {
            let (f, cls) = &cls_meta[c];
            for m in file_starts[*f as usize] as usize..file_starts[*f as usize + 1] as usize {
                if m_meta[m].1 == ref_meta[i] && m_meta[m].2.contains(cls.as_str()) {
                    em = m as i32;
                    ec = c as i32;
                    break 'outer;
                }
            }
        }
        assert_eq!((gm[i], gc[i]), (em, ec), "ref {i} method={}", ref_meta[i]);
    }
    eprintln!("GPU method-match parity: {n_refs} refs OK");
}
