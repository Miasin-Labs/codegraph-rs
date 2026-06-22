use crate::resolution::gpu::GpuNameJoiner;

#[test]
fn gpu_fuzzy_matches_cpu_uniqueness() {
    let Some(joiner) = GpuNameJoiner::new(&["x"]) else {
        eprintln!("no CUDA device — skipping");
        return;
    };
    let mut state = 0xfeedfaceu64;
    let mut rnd = move |m: u64| {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) % m
    };
    let n_groups = 60usize;
    let mut cand_starts = vec![0u32];
    let (mut cl, mut ck) = (Vec::new(), Vec::new());
    for _ in 0..n_groups {
        for _ in 0..rnd(5) {
            cl.push(rnd(3) as u8);
            ck.push(rnd(7) as u8);
        }
        cand_starts.push(cl.len() as u32);
    }
    let n_refs = 400usize;
    let (mut rg, mut rl) = (Vec::new(), Vec::new());
    for i in 0..n_refs {
        rg.push(if i % 13 == 0 {
            -1
        } else {
            rnd(n_groups as u64) as i32
        });
        rl.push(rnd(3) as u8);
    }
    let (gi, gc) = joiner
        .fuzzy_unique(&rg, &rl, &cand_starts, &cl, &ck)
        .expect("fuzzy");
    for i in 0..n_refs {
        let (mut ei, mut ec) = (-1i32, 0u8);
        if rg[i] >= 0 {
            let g = rg[i] as usize;
            let rng = cand_starts[g] as usize..cand_starts[g + 1] as usize;
            let callable: Vec<usize> = rng.filter(|&c| matches!(ck[c], 1..=3)).collect();
            let same: Vec<usize> = callable
                .iter()
                .copied()
                .filter(|&c| cl[c] == rl[i])
                .collect();
            if same.len() == 1 {
                ei = same[0] as i32;
            } else if same.is_empty() && callable.len() == 1 {
                ei = callable[0] as i32;
                ec = 1;
            }
        }
        assert_eq!((gi[i], gc[i]), (ei, ec), "ref {i}");
    }
    eprintln!("GPU fuzzy parity: {n_refs} refs OK");
}
