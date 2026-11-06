//! Benchmark CI, CPU verifier: base and candidate server/cpu_worker built for the VPS's
//! CPU (EPYC Milan: -march=znver3, which this Zen 4 runs; the VPS runs its -march=native
//! build) with the VPS's segment (2e9) on a 2-segment window (sd ~0.3 %), one core
//! (taskset), ABBA order; the canonical result of every run must be identical.
//! Writes {mode: "cpu", runs: [...]} in bench_ab.mjs's format: aTotal / bTotal are
//! seconds per 1e9 evens.
//!   goldbach-tools bench-cpu BASE_ROOT CAND_ROOT REPS OUT.json [START LIMIT]
use crate::util::{e, sha_prefix, write_json, R};
use serde_json::json;
use std::process::Command;
use std::time::Instant;

fn build(root: &str, march: &str) -> R<String> {
    std::fs::create_dir_all(format!("{root}/build")).map_err(e)?;
    let st = Command::new("g++")
        .args(["-std=c++20", "-O3", &format!("-march={march}"), "-Ishared/include", "server/cpu_worker.cpp", "core/core.cpp",
               "shared/src/primes.cpp", "-o", "build/bench_cpu"])
        .current_dir(root)
        .status()
        .map_err(|x| format!("g++: {x}"))?;
    if !st.success() {
        return Err(format!("g++ failed in {root}"));
    }
    Ok(format!("{root}/build/bench_cpu"))
}

pub fn run(args: &[String]) -> R<i32> {
    if args.len() < 4 {
        return Err("usage: bench-cpu BASE_ROOT CAND_ROOT REPS OUT.json [START LIMIT]".into());
    }
    let reps: usize = args[2].parse().map_err(|_| "REPS is not a number")?;
    let start: u64 = args.get(4).map_or(Ok(4_000_000_000_000_000_000), |s| s.parse()).map_err(|_| "START")?;
    let limit: u64 = args.get(5).map_or(Ok(4_000_000_004_000_000_000), |s| s.parse()).map_err(|_| "LIMIT")?;
    let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    let (segment, march, core) = (env("GB_CPU_SEGMENT", "2000000000"), env("GB_CPU_MARCH", "znver3"), env("GB_CPU_CORE", "2"));
    let evens = (limit - start) / 2 + 1;
    let bins = [build(&args[0], &march)?, build(&args[1], &march)?];
    let cache = std::env::temp_dir().join(format!("gb-bench-cpu-{}", std::process::id()));
    std::fs::create_dir_all(&cache).map_err(e)?;
    let primes = cache.join("primes.bin").to_string_lossy().into_owned();
    let run1 = |side: usize| -> R<(f64, String)> {
        let t0 = Instant::now();
        let o = Command::new("taskset")
            .args(["-c", &core, &bins[side], &start.to_string(), &limit.to_string(), "--segment", &segment, "--primes", &primes])
            .output()
            .map_err(|x| format!("taskset: {x}"))?;
        if !o.status.success() {
            return Err(format!("verifier failed: {}", String::from_utf8_lossy(&o.stderr)));
        }
        let dt = t0.elapsed().as_secs_f64();
        Ok((dt / (evens as f64 / 1e9), sha_prefix(String::from_utf8_lossy(&o.stdout).trim().as_bytes(), 16)))
    };
    run1(0)?; // warm-up: writes the base-prime cache, caches warm
    let mut runs = vec![];
    for rep in 0..reps {
        let ab = matches!(rep % 4, 0 | 3);
        let (first, second) = if ab { (0, 1) } else { (1, 0) };
        let (r1, r2) = (run1(first)?, run1(second)?);
        let (ra, rb) = if ab { (r1, r2) } else { (r2, r1) };
        let same = ra.1 == rb.1;
        let order = if ab { "AB" } else { "BA" };
        runs.push(json!({ "rep": rep, "identical": same, "a": { "cpu": ra.0 }, "b": { "cpu": rb.0 },
                          "aTotal": ra.0, "bTotal": rb.0, "evensPerSeg": 1e9, "order": order }));
        println!("cpu rep {rep} ({order}): A {:.3}, B {:.3} s per 1e9 evens, {}", ra.0, rb.0, if same { "identical" } else { "RESULTS DIFFER" });
    }
    let _ = std::fs::remove_dir_all(&cache);
    write_json(&args[3], &json!({ "mode": "cpu", "start": start.to_string(), "limit": limit.to_string(),
                                   "gpu": format!("one core, -march={march}, segment {segment}"), "runs": runs }))?;
    Ok(0)
}
