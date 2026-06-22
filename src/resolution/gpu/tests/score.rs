use crate::resolution::gpu::GpuNameJoiner;

#[test]
fn gpu_scoring_matches_cpu_find_best_match() {
    let Some(joiner) = GpuNameJoiner::new(&["x"]) else {
        eprintln!("no CUDA device — skipping");
        return;
    };
    let n_files = 12u32;
    let mut dir_starts = vec![0u32];
    let mut dir_hashes = Vec::new();
    for f in 0..n_files {
        let depth = (f % 4) as u64;
        let mut h = 0xabcdefu64;
        for d in 0..depth {
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
        let k = rnd(6);
        for _ in 0..k {
            cand_file.push(rnd(n_files as u64) as u32);
            cand_lang.push(rnd(4) as u8);
            cand_kind.push(rnd(6) as u8);
            cand_exp.push(rnd(2) as u8);
            cand_line.push(rnd(400) as u32);
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
