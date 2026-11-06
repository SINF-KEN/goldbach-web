//! Cross-check: the WebGPU engine against the CPU verifier (an independent implementation)
//! on random windows, both modes, random segment sizes, edge cases (single evens, windows at
//! 4e18 and at 5e18, many segments). The windows come from a seed (the benchmark CI uses the
//! commit), so every run checks other ones. Result hashes are the canonical-JSON sha256
//! prefixes of test/golden.mjs / golden.sh.
//!
//!   goldbach-tools crosscheck cpu SEED N OUT.json [CPU_BIN] [--fuzz]   # windows + CPU hashes (parallel, no GPU)
//!       --fuzz: every window also gets a random engine configuration (split, K, submit batch,
//!       depth, work-group sizes, primes per invocation, hit-list factor, checker phase A):
//!       results must not depend on any of them
//!   goldbach-tools crosscheck gpu OUT.json                    # WebGPU hashes of the same windows, compare
//!                                                             # (run from the checkout to test; uses the GPU)
//! Exit 1 on any difference. A window the engine refuses (fail closed) is reported, not failed.
use crate::util::{e, ncpu, read_json, sha_prefix, write_json, Rng, R};
use serde_json::{json, Map, Value};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::Mutex;

const LO: u64 = 4_000_000_000_000_000_000;
const HI: u64 = 5_000_000_000_000_000_000;
const MAX_LEN: u64 = 4_000_000_000; // integers per window: <= 2e9 evens (~10 s on one CPU core)

fn fuzz_cfg(rng: &mut Rng, mode: &str) -> Value {
    let mut c = Map::new();
    c.insert("split".into(), rng.range(8000, 60001).into());
    c.insert("K".into(), rng.choice(&[1u64, 2, 3, 5, 8, 16, 24, 32, 48]).into());
    c.insert("batch".into(), rng.range(1, 5).into());
    c.insert("depth".into(), rng.range(1, 13).into());
    c.insert("KA".into(), (4 * rng.range(1, 25)).into());
    if mode == "full" {
        c.insert("wgLarge".into(), rng.choice(&[64u64, 128, 256]).into());
        c.insert("wgHarvest".into(), rng.choice(&[64u64, 128, 256]).into());
        c.insert("ppi".into(), rng.choice(&[1u64, 2, 4]).into());
        c.insert("hlFactor".into(), [json!(0.5), json!(1), json!(2), json!(3)][rng.range(0, 4) as usize].clone());
    }
    c.into()
}

pub fn windows(seed: &str, n: usize, fuzz: bool) -> Vec<Value> {
    let mut rng = Rng::from_seed(seed);
    let mut out = vec![];
    for i in 0..n {
        let mode = rng.choice(&["full", "lite"]);
        let segs: [u64; 4] = if mode == "lite" { [500_000_000, 256_000_000, 64_000_000, 20_000_000] } else { [467_500_000, 233_750_000, 128_000_000, 20_000_000] };
        let odd = 2 * rng.range(10_000_000, 240_000_000);
        let seg = rng.choice(&[segs[0], segs[1], segs[2], segs[3], odd, odd]);
        let length = match i % 6 {
            0 => 0,                                                  // a single even
            1 => 2 * rng.range(1, 500_000),                          // inside one segment
            2 => 2 * rng.range(seg / 2, 2 * seg),                    // around a segment boundary
            _ => 2 * rng.range(MAX_LEN / 8, MAX_LEN / 2),            // many segments
        }
        .min(MAX_LEN);
        let start = match i {
            2 => LO,                                                 // the lowest even of the range
            3 => HI - length,                                        // ends at 5e18 (p^2 path)
            _ => LO + 2 * rng.range(0, (HI - LO - length) / 2),
        };
        let mut w = json!({ "start": start, "limit": start + length, "seg": seg, "mode": mode });
        if fuzz {
            w["cfg"] = fuzz_cfg(&mut rng, mode);
        }
        out.push(w);
    }
    out
}

fn cpu_hash(binary: &str, w: &Value) -> String {
    let o = Command::new("nice").args(["-n", "19", binary, &w["start"].to_string(), &w["limit"].to_string()]).output();
    match o {
        Ok(o) if o.status.success() => sha_prefix(String::from_utf8_lossy(&o.stdout).replace('\n', "").as_bytes(), 16),
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            let t = err.trim();
            format!("error: {}", &t[t.char_indices().rev().nth(199).map_or(0, |x| x.0)..])
        }
        Err(x) => format!("error: {x}"),
    }
}

/// (hash, line) of the engine's answer, or (None, the refusal).
fn gpu_hash(w: &Value) -> (Option<String>, String) {
    let lite = w["mode"] == "lite";
    let mut args = vec!["test/golden.mjs".to_string(), w["start"].to_string(), w["limit"].to_string(), w["seg"].to_string()];
    if lite {
        args.push("lite".into());
    }
    let cfg = w.get("cfg").cloned().unwrap_or(json!({}));
    let o = Command::new("node")
        .args(&args)
        .env("GB_LIST_MB", if lite { "192" } else { "768" }) // the engine's list budgets
        .env("GB_CFG", cfg.to_string())
        .output();
    let Ok(o) = o else { return (None, "cannot run node".into()) };
    let stdout = String::from_utf8_lossy(&o.stdout);
    let line = stdout.trim().lines().last().unwrap_or("").to_string();
    if !o.status.success() {
        let err = String::from_utf8_lossy(&o.stderr);
        let lines: Vec<&str> = err.trim().lines().collect();
        let msg = lines.iter().find(|l| l.contains("Error")).or(lines.last()).copied().unwrap_or("?");
        let tail = &msg[msg.char_indices().rev().nth(199).map_or(0, |x| x.0)..];
        return (None, tail.to_string());
    }
    (line.split_whitespace().next().map(str::to_string), line)
}

fn cpu(args: &[String]) -> R<i32> {
    let (seed, n, out) = (&args[0], args.get(1).and_then(|s| s.parse::<usize>().ok()).ok_or("N")?, args.get(2).ok_or("OUT.json")?);
    let fuzz = args.iter().any(|a| a == "--fuzz");
    let rest: Vec<&String> = args[3..].iter().filter(|a| *a != "--fuzz").collect();
    let binary = rest.first().map_or("build/cpu_worker-native", |s| s.as_str());
    let mut ws = windows(seed, n, fuzz);
    // the base-prime cache of the CPU verifier: one run first, then parallel
    let outdir = std::fs::canonicalize(std::path::Path::new(out).parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(".".as_ref())).map_err(e)?;
    let cache = outdir.join("cpu-primes.bin");
    let wrap = outdir.join("cpu_worker.sh");
    let bin = std::fs::canonicalize(binary).map_err(|x| format!("{binary}: {x}"))?;
    std::fs::write(&wrap, format!("#!/bin/sh\nexec \"{}\" \"$@\" --primes \"{}\"\n", bin.display(), cache.display())).map_err(e)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrap, std::fs::Permissions::from_mode(0o755)).map_err(e)?;
    }
    let wrap_s = wrap.to_string_lossy().into_owned();
    cpu_hash(&wrap_s, &json!({ "start": LO, "limit": LO }));
    let hashes = Mutex::new(vec![String::new(); ws.len()]);
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..ncpu().saturating_sub(2).max(1) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, SeqCst);
                let Some(w) = ws.get(i) else { break };
                let h = cpu_hash(&wrap_s, w);
                hashes.lock().unwrap()[i] = h;
            });
        }
    });
    let mut bad = 0;
    for (w, h) in ws.iter_mut().zip(hashes.into_inner().unwrap()) {
        bad += h.starts_with("error") as usize;
        w["cpu"] = h.into();
    }
    write_json(out, &json!({ "seed": seed, "windows": ws }))?;
    println!("crosscheck: {n} windows (seed {seed}), CPU hashes done{}", if bad > 0 { format!("; CPU ERRORS: {bad}") } else { String::new() });
    Ok((bad > 0) as i32)
}

fn gpu(args: &[String]) -> R<i32> {
    let d = read_json(args.first().ok_or("OUT.json")?)?;
    let (mut diff, mut refused, mut ok) = (0, 0, 0);
    for w in d["windows"].as_array().ok_or("windows")? {
        let (h, line) = gpu_hash(w);
        let tag = format!("[{}, {}] {} seg {}{}", w["start"], w["limit"], w["mode"].as_str().unwrap_or(""), w["seg"],
                          w.get("cfg").map_or(String::new(), |c| format!(" cfg {c}")));
        match h {
            None => {
                refused += 1;
                println!("refused  {tag}: {line}");
            }
            Some(h) if Some(h.as_str()) == w["cpu"].as_str() => {
                ok += 1;
                println!("ok       {tag}  {h}");
            }
            Some(h) => {
                diff += 1;
                println!("DIFFERS  {tag}  gpu {h}  cpu {}", w["cpu"].as_str().unwrap_or(""));
            }
        }
    }
    println!("CROSSCHECK {}: {ok} identical, {diff} different, {refused} refused (seed {})", if diff == 0 { "OK" } else { "FAIL" }, d["seed"].as_str().unwrap_or(""));
    Ok((diff > 0) as i32)
}

pub fn run(args: &[String]) -> R<i32> {
    match args.first().map(String::as_str) {
        Some("cpu") if args.len() >= 4 => cpu(&args[1..]),
        Some("gpu") if args.len() >= 2 => gpu(&args[1..]),
        _ => Err("usage: crosscheck cpu SEED N OUT.json [CPU_BIN] [--fuzz] | crosscheck gpu OUT.json".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_are_deterministic_and_inside_the_range() {
        let a = windows("abc123", 24, true);
        assert_eq!(a, windows("abc123", 24, true));
        assert_ne!(a, windows("abc124", 24, true));
        for (i, w) in a.iter().enumerate() {
            let (s, l) = (w["start"].as_u64().unwrap(), w["limit"].as_u64().unwrap());
            assert!(s >= LO && l <= HI && l >= s && l - s <= MAX_LEN && s % 2 == 0 && l % 2 == 0, "{w}");
            assert!(w["cfg"]["split"].as_u64().unwrap() >= 8000);
            assert_eq!(w["cfg"].get("ppi").is_some(), w["mode"] == "full");
            if i == 2 {
                assert_eq!(s, LO);
            }
            if i == 3 {
                assert_eq!(l, HI);
            }
            if i % 6 == 0 && i != 2 && i != 3 {
                assert_eq!(s, l);
            }
        }
        assert!(windows("x", 3, false)[0].get("cfg").is_none());
    }
}
