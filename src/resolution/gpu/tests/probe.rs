use std::collections::HashSet;
use std::time::Instant;

use super::cpu_flags;
use crate::resolution::gpu::GpuNameJoiner;

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
    let known: HashSet<&str> = names.iter().copied().collect();
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
    let known: HashSet<&str> = name_refs.iter().copied().collect();
    let t = Instant::now();
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
