//! Benchmark CI report: reads bench-out/{full,lite,cpu}.json (test/bench_ab.mjs) and prints a
//! markdown report; the last line is <!-- decision: merge|keep -->.
//!
//! Statistics per mode: repetitions run in ABBA order; each AB + BA pair gives one speedup
//! s_i = sqrt(r_AB r_BA) - 1 with r = A / B (A = main, B = candidate, GPU ms per 233.75M
//! evens); mean and a 95% confidence interval from Student's t over the pairs. Two metrics
//! per mode: GPU pass time (timestamp sums) and slice wall time (setup + pipelined
//! segments, as the worker runs a slice). Decision: exact (golden hashes, modq, every
//! repetition's results identical) AND the lower bound > MIN_GAIN in at least one gating
//! metric AND the upper bound > -MAX_LOSS in all of them (no significant regression).
//! Gating: slice wall time per mode and the CPU verifier; the GPU pass sums (sequential
//! runs with timestamp queries: the GPU idles between segments and its clocks wander,
//! phone mode sd ~10 %) are reported per pass as diagnostics.
//!   goldbach-tools bench-report OUT_DIR [--branch B] [--sha S] [--base B] [--cand C]
//!                               [--exact E] [--golden F] [--modq F] [--crosscheck F]
use crate::util::{f, mean, read_json, stdev, Args, R};
use serde_json::Value;
use std::collections::BTreeSet;

const MIN_GAIN: f64 = 0.010; // +1.0 %: the smallest speedup worth a merge (A/A tests: pairs within ~0.5 %)
const MAX_LOSS: f64 = 0.005; // a mode may not get significantly slower than -0.5 %
const T95: [f64; 10] = [12.706, 4.303, 3.182, 2.776, 2.571, 2.447, 2.365, 2.306, 2.262, 2.228];

/// (mean, sd, lo, hi) of the paired speedups. Repetitions run in ABBA order; the side that
/// runs second is ~2-3 % slower (clock/thermal), so a consecutive AB + BA pair is one
/// measurement: the geometric mean of its two ratios cancels the order effect.
fn stats(runs: &[Value], ka: &str, kb: &str) -> (f64, f64, f64, f64) {
    let ratios: Vec<f64> = runs.iter().map(|r| f(r, ka) / f(r, kb)).collect();
    let mut s: Vec<f64> = (0..ratios.len().saturating_sub(1)).step_by(2).map(|i| (ratios[i] * ratios[i + 1]).sqrt() - 1.0).collect();
    if s.is_empty() {
        s = vec![ratios[0] - 1.0];
    }
    let m = mean(&s);
    let sd = if s.len() > 1 { stdev(&s) } else { 0.0 };
    let half = if s.len() > 1 {
        T95.get(s.len() - 2).copied().unwrap_or(2.0) * sd / (s.len() as f64).sqrt()
    } else {
        f64::INFINITY
    };
    (m, sd, m - half, m + half)
}

fn lines_of(path: &str) -> Vec<String> {
    if path.is_empty() || !std::path::Path::new(path).exists() {
        return vec![];
    }
    std::fs::read_to_string(path).unwrap_or_default().trim().lines().map(str::to_string).collect()
}

fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn avg(runs: &[Value], k: &str) -> f64 {
    mean(&runs.iter().map(|r| f(r, k)).collect::<Vec<_>>())
}

pub fn run(args: &[String]) -> R<i32> {
    let a = Args::parse(args, &[])?;
    let out = a.positional(0, "OUT_DIR")?;
    let g = |k: &str| a.str_or(k, "");
    let (branch, sha, base, cand) = (g("branch"), g("sha"), g("base"), g("cand"));
    let mut modes: Vec<(&str, Value)> = vec![];
    for m in ["full", "lite", "cpu"] {
        let p = format!("{out}/{m}.json");
        if std::path::Path::new(&p).exists() {
            modes.push((m, read_json(&p)?));
        }
    }
    let has = |m: &str| modes.iter().any(|x| x.0 == m);
    let have_gpu = has("full") && has("lite");
    let runs_of = |d: &Value| d["runs"].as_array().cloned().unwrap_or_default();
    let identical = have_gpu && modes.iter().all(|(_, d)| runs_of(d).iter().all(|r| r["identical"].as_bool().unwrap_or(false)));
    let exact = g("exact") == "ok" && identical;
    let mut l: Vec<String> = vec![];
    l.push(format!("## Benchmark: `{branch}` vs main"));
    let gpu = modes.first().map(|m| m.1["gpu"].as_str().unwrap_or("?").to_string()).unwrap_or("?".into());
    l.push(format!(
        "candidate `{}` (= `{}` merged with main `{}`), {gpu}, GPU time per segment from timestamp queries, base and candidate alternating segment by segment in one process",
        head(&cand, 9), head(&sha, 9), head(&base, 9)
    ));
    l.push(String::new());
    l.push("| check | result |".into());
    l.push("|---|---|".into());
    let (gl, ml) = (lines_of(&g("golden")), lines_of(&g("modq")));
    l.push(format!("| golden result hashes (6 windows, full + phone) | {} |", gl.last().map_or("?", |s| s)));
    l.push(format!("| modq vs BigInt | {} |", ml.last().map_or("?", |s| s)));
    let cc = g("crosscheck");
    if !cc.is_empty() && std::path::Path::new(&cc).exists() {
        let xl = lines_of(&cc);
        l.push(format!("| GPU engine vs CPU verifier, random windows (`goldbach-tools crosscheck`) | {} |", xl.last().map_or("?", |s| s)));
    }
    l.push(format!("| results identical to main in every repetition | {} |", if identical { "yes" } else { "**NO**" }));
    l.push(String::new());
    let mut verdicts: Vec<(String, f64, f64, f64)> = vec![]; // gating: slice wall time per mode, CPU verifier
    let mut diag: Vec<(String, f64, f64, f64)> = vec![]; // reported, not gating: GPU pass sums (sequential, profiled: noisy)
    for (m, d) in &modes {
        let runs = runs_of(d);
        let (mn, sd, lo, hi) = stats(&runs, "aTotal", "bTotal");
        if *m == "cpu" {
            verdicts.push((m.to_string(), mn, lo, hi)); // CPU verifier: gates
        } else {
            diag.push((format!("{m} GPU passes"), mn, lo, hi)); // diagnostic: per-pass breakdown, does not gate
        }
        let evens = f(&runs[0], "evensPerSeg");
        let (ga, gb) = (avg(&runs, "aTotal"), avg(&runs, "bTotal"));
        let (pm, ps, pl, ph) = (100.0 * mn, 100.0 * sd, 100.0 * lo, 100.0 * hi);
        let ds = d["start"].as_str().unwrap_or("");
        let dl = d["limit"].as_str().unwrap_or("");
        if *m == "cpu" {
            l.push(format!("### CPU verifier, VPS build ({} repetitions, window [{ds}, {dl}], {})", runs.len(), d["gpu"].as_str().unwrap_or("")));
            l.push(String::new());
            l.push(format!("**{pm:+.2} %** (95 % CI {pl:+.2} .. {ph:+.2} % over {} ABBA pairs, sd {ps:.2} %): {ga:.3} -> {gb:.3} s per 1e9 evens, {:.4} -> {:.4} G evens/s on one core",
                           runs.len() / 2, 1.0 / ga, 1.0 / gb));
            l.push(String::new());
            continue;
        }
        l.push(format!("### {} ({} repetitions, window [{ds}, {dl}], each side with its own engineDefaults)",
                       if *m == "full" { "Full engine" } else { "Phone mode" }, runs.len()));
        l.push(String::new());
        l.push(format!("**{pm:+.2} %** (95 % CI {pl:+.2} .. {ph:+.2} % over {} ABBA pairs, sd {ps:.2} %): {ga:.3} -> {gb:.3} GPU ms per 233.75M evens, {:.2} -> {:.2} G evens/s GPU-bound",
                       runs.len() / 2, evens / ga / 1e6, evens / gb / 1e6));
        l.push(String::new());
        l.push("| pass | main ms | candidate ms | change |".into());
        l.push("|---|---:|---:|---:|".into());
        let mut keys = BTreeSet::new();
        for r in &runs {
            for side in ["a", "b"] {
                keys.extend(r[side].as_object().into_iter().flatten().map(|(k, _)| k.clone()));
            }
        }
        for k in &keys {
            let side = |s: &str| mean(&runs.iter().map(|r| r[s][k.as_str()].as_f64().unwrap_or(0.0)).collect::<Vec<_>>());
            let (pa, pb) = (side("a"), side("b"));
            let ch = if pa > 0.0 { format!("{:+.1} %", 100.0 * (pb / pa - 1.0)) } else { "new".into() };
            l.push(format!("| {k} | {pa:.3} | {pb:.3} | {ch} |"));
        }
        l.push(format!("| **total** | **{ga:.3}** | **{gb:.3}** | **{:+.1} %** |", 100.0 * (gb / ga - 1.0)));
        l.push(String::new());
        let per: Vec<String> = runs.iter().map(|r| format!("{} {:.3}/{:.3}", r["order"].as_str().unwrap_or(""), f(r, "aTotal"), f(r, "bTotal"))).collect();
        l.push(format!("per repetition (order: main / candidate ms): {}", per.join(", ")));
        l.push(String::new());
        if runs.iter().all(|r| r.get("aWall").is_some()) {
            // slice wall time (test/bench_ab.mjs wallSide): setup + pipelined segments, as the worker runs a slice
            let (mn, sd, lo, hi) = stats(&runs, "aWall", "bWall");
            verdicts.push((format!("{m} wall"), mn, lo, hi));
            let (wa, wb) = (avg(&runs, "aWall"), avg(&runs, "bWall"));
            let (pa, pb) = (avg(&runs, "aPrep"), avg(&runs, "bPrep"));
            let (sa, sb) = (avg(&runs, "aSegWall"), avg(&runs, "bSegWall"));
            l.push(format!(
                "**Slice wall time** (setup + pipelined segments as the worker runs them, scaled to a 1e12 slice): **{:+.2} %** (95 % CI {:+.2} .. {:+.2} %, sd {:.2} %): {wa:.3} -> {wb:.3} ms per 233.75M evens, {:.2} -> {:.2} G evens/s; setup {pa:.0} -> {pb:.0} ms per slice, {sa:.3} -> {sb:.3} ms per segment",
                100.0 * mn, 100.0 * lo, 100.0 * hi, 100.0 * sd, evens / wa / 1e6, evens / wb / 1e6));
            l.push(String::new());
        }
    }
    if have_gpu && !["full", "lite"].iter().all(|m| verdicts.iter().any(|v| v.0 == format!("{m} wall"))) {
        verdicts.extend(diag); // a harness without the wall metric: gate on the pass sums as before
    }
    let gain = verdicts.iter().any(|v| v.2 > MIN_GAIN);
    let no_loss = verdicts.iter().all(|v| v.3 > -MAX_LOSS);
    // fix/* branches (correctness, fail-closed guards): exact and no significant regression
    // is enough; every other branch must also be significantly faster
    let fix = branch.starts_with("fix/");
    let merge = exact && (gain || fix) && no_loss && have_gpu;
    let why = if !exact {
        "not exact: never merged".to_string()
    } else if !no_loss {
        format!("a metric got significantly slower: {}", verdicts.iter().filter(|v| v.3 <= -MAX_LOSS).map(|v| v.0.as_str()).collect::<Vec<_>>().join(", "))
    } else if fix && !gain {
        "a fix: exact, no significant regression".into()
    } else if !gain {
        format!("no significant speedup (needs a 95 % lower bound above +{:.1} % in one metric)", 100.0 * MIN_GAIN)
    } else {
        "exact and significantly faster".into()
    };
    // borderline: a mode's mean clears the bar but its interval does not -> the CI extends
    // the measurement (more ABBA pairs) instead of deciding on noise
    let borderline = exact && !merge && verdicts.iter().any(|v| v.1 > MIN_GAIN && v.2 <= MIN_GAIN);
    let g: Vec<String> = verdicts.iter().map(|(m, mn, lo, hi)| format!("{m} {:+.2} % [{:+.2}, {:+.2}]", 100.0 * mn, 100.0 * lo, 100.0 * hi)).collect();
    l.push(format!("Gating metrics: {}. GPU pass sums are diagnostics (sequential, profiled runs; phone mode sd ~10 %): they do not gate.", g.join(", ")));
    l.push(String::new());
    l.push(format!("**Decision: {}** ({why}).", if merge { "MERGE" } else { "keep on the branch" }));
    if borderline {
        l.push("<!-- borderline -->".into());
    }
    l.push(format!("<!-- decision: {} -->", if merge { "merge" } else { "keep" }));
    println!("{}", l.join("\n"));
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn runs(ratios: &[f64]) -> Vec<Value> {
        ratios.iter().map(|r| json!({ "aTotal": *r, "bTotal": 1.0 })).collect()
    }

    #[test]
    fn pairs_cancel_the_order_effect() {
        // AB then BA: ratios 1.05 and 0.95 -> geometric mean ~ -0.125 %
        let (m, sd, ..) = stats(&runs(&[1.05, 0.95]), "aTotal", "bTotal");
        assert!((m - ((1.05f64 * 0.95).sqrt() - 1.0)).abs() < 1e-12 && sd == 0.0);
    }

    #[test]
    fn interval_from_student_t() {
        let (m, sd, lo, hi) = stats(&runs(&[1.02, 1.02, 1.04, 1.04, 1.03, 1.03]), "aTotal", "bTotal");
        let half = 4.303 * sd / 3f64.sqrt();
        assert!((lo - (m - half)).abs() < 1e-12 && (hi - (m + half)).abs() < 1e-12);
        assert!((m - 0.03).abs() < 1e-9);
    }

    #[test]
    fn one_repetition_has_no_interval() {
        let (m, _, lo, hi) = stats(&runs(&[1.1]), "aTotal", "bTotal");
        assert!((m - 0.1).abs() < 1e-12 && lo == f64::NEG_INFINITY && hi == f64::INFINITY);
    }
}
