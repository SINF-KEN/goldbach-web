//! Pushes the native B580 campaign's state to the goldbach-web coordinator.
//!
//! Reads (read-only) the production journal, ridge store and statistics log in the
//! native campaign directory (GOLDBACH_CAMPAIGN_DIR, default ~/coding/goldbach-l0)
//! and POSTs a summary to /api/native: progress (resume point, banked slices, recent
//! speed), the top ridges, the p(n) tail histogram, and the floor below which browsers
//! get no slices (the B580's resume point plus FLOOR_AHEAD_DAYS of its progress, so the
//! two never work on the same evens).
//!
//!   goldbach-sync-native [--url https://goldbach.net01.io] [--token-file ~/.config/goldbach/token]
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const JOURNAL: &str = "range-4000000000000000000-5000000000000000000.journal";
const RANGE_LO: u64 = 4_000_000_000_000_000_000;
const RANGE_HI: u64 = 5_000_000_000_000_000_000;
const FLOOR_AHEAD_DAYS: u64 = 7;
/// the live paper's Figure 4: one snapshot of the sampled p(n) counts (p < 250)
/// every HISTORY_STEP of native progress, kept on this machine
const HISTORY_STEP: i128 = 5_000_000_000_000_000;
const HISTORY_SEND: usize = 100; // snapshots uploaded of each kind (all kept here): bounds the server's payload
const SERIES_POINTS: usize = 400;
/// a copy of the statistics state every ARCHIVE_STEP of either range (p(n), gaps), so a
/// figure added later can still be drawn over the whole campaign: the state is
/// cumulative and the per-slice log does not hold the small-p histogram
const ARCHIVE_STEP: u64 = 1_000_000_000_000_000;

type R<T> = Result<T, String>;
type Kv = HashMap<String, String>;
type Row = (u64, u64, Kv);

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
}

struct Paths {
    root: PathBuf,
    history: PathBuf,
    archive: PathBuf,
}

fn paths() -> Paths {
    let env = |k: &str, d: PathBuf| std::env::var(k).map(PathBuf::from).unwrap_or(d);
    Paths {
        root: env("GOLDBACH_CAMPAIGN_DIR", home().join("coding/goldbach-l0")),
        history: env("GOLDBACH_FIG4_HISTORY", home().join(".local/state/goldbach-sync/fig4.json")),
        archive: env("GOLDBACH_STATE_ARCHIVE", home().join(".local/state/goldbach-sync/archive")),
    }
}

fn e<T: std::fmt::Display>(x: T) -> String {
    x.to_string()
}

fn num<T: std::str::FromStr>(s: &str) -> R<T> {
    s.parse().map_err(|_| format!("not a number: {s:?}"))
}

fn round_to(x: f64, d: i32) -> f64 {
    let m = 10f64.powi(d);
    (x * m).round() / m
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn odd_primes(n: usize) -> Vec<u64> {
    let mut s = vec![true; n + 1];
    s[0] = false;
    s[1] = false;
    let mut i = 2;
    while i * i <= n {
        if s[i] {
            (i * i..=n).step_by(i).for_each(|j| s[j] = false);
        }
        i += 1;
    }
    (3..=n).filter(|&p| s[p]).map(|p| p as u64).collect()
}

fn mulmod(a: u64, b: u64, m: u64) -> u64 {
    (a as u128 * b as u128 % m as u128) as u64
}

fn powmod(mut b: u64, mut x: u64, m: u64) -> u64 {
    let mut r = 1;
    b %= m;
    while x > 0 {
        if x & 1 == 1 {
            r = mulmod(r, b, m);
        }
        b = mulmod(b, b, m);
        x >>= 1;
    }
    r
}

/// Deterministic Miller-Rabin, exact for every n < 2^64.
fn is_prime(n: u64) -> bool {
    const SMALL: [u64; 12] = [2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37];
    if n < 2 {
        return false;
    }
    for p in SMALL {
        if n % p == 0 {
            return n == p;
        }
    }
    let (mut d, mut r) = (n - 1, 0);
    while d % 2 == 0 {
        d /= 2;
        r += 1;
    }
    'a: for a in SMALL {
        let mut x = powmod(a, d, n);
        if x == 1 || x == n - 1 {
            continue;
        }
        for _ in 0..r - 1 {
            x = mulmod(x, x, n);
            if x == n - 1 {
                continue 'a;
            }
        }
        return false;
    }
    true
}

/// (primes, twin pairs) at limit + 1, which no slice counts (server.py boundary_counts).
fn boundary_counts(limit: u64) -> (u64, u64) {
    if limit + 2 > RANGE_HI || !is_prime(limit + 1) {
        return (0, 0);
    }
    (1, is_prime(limit - 1) as u64 + is_prime(limit + 3) as u64)
}

/// At most n items of seq, evenly spaced, the last one always kept.
fn thin(seq: &[Value], n: usize) -> Vec<Value> {
    if seq.len() <= n {
        return seq.to_vec();
    }
    let step = seq.len() as f64 / n as f64;
    let mut v: Vec<Value> = (0..n - 1).map(|i| seq[(i as f64 * step) as usize].clone()).collect();
    v.push(seq.last().unwrap().clone());
    v
}

fn xnum(v: &Value) -> i128 {
    match v {
        Value::String(s) => s.parse().unwrap_or(0),
        Value::Number(n) => n.as_i64().map(i128::from).or_else(|| n.as_u64().map(i128::from)).unwrap_or(0),
        _ => 0,
    }
}

fn kvs<'a>(words: impl Iterator<Item = &'a str>) -> Kv {
    words.filter_map(|x| x.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn kv_u64(kv: &Kv, k: &str) -> R<u64> {
    num(kv.get(k).map(String::as_str).unwrap_or("0"))
}

fn kv_f64(kv: &Kv, k: &str) -> R<f64> {
    num(kv.get(k).map(String::as_str).unwrap_or("0"))
}

/// Figure 8's gap snapshots: the per-slice gap histograms (gsamp) of the statistics log
/// summed over the slices contiguous from RANGE_LO, one snapshot each time the run
/// crosses a HISTORY_STEP boundary (x = that slice's limit). A slice logged twice counts
/// once, as in the state.
fn gap_history(root: &Path) -> Vec<Value> {
    fn rows(root: &Path) -> Option<HashMap<u64, (u64, u64, String)>> {
        let mut rows = HashMap::new();
        for line in std::fs::read_to_string(root.join("goldbach_stats.log")).ok()?.lines() {
            if !line.contains("gsamp=") {
                continue;
            }
            let kv = kvs(line.split_whitespace().skip(2));
            let start = kv.get("start")?.parse().ok()?;
            rows.entry(start).or_insert((kv.get("limit")?.parse().ok()?, kv.get("evens")?.parse().ok()?, kv.get("gsamp")?.clone()));
        }
        Some(rows)
    }
    let Some(mut rows) = rows(root) else { return vec![] };
    let (mut out, mut cur, mut evens, mut acc): (Vec<Value>, u64, u64, Vec<u64>) = (vec![], RANGE_LO, 0, vec![]);
    while let Some((lim, ev, gsamp)) = rows.remove(&cur) {
        evens += ev;
        for (i, c) in gsamp.split(',').enumerate() {
            if i == acc.len() {
                acc.push(0);
            }
            acc[i] += c.parse::<u64>().unwrap_or(0);
        }
        if (lim + 2 - RANGE_LO) / HISTORY_STEP as u64 > (cur - RANGE_LO) / HISTORY_STEP as u64 {
            out.push(json!({ "x": lim.to_string(), "evens": 0, "counts": [], "gevens": evens, "gcounts": &acc[..acc.len().min(107)] }));
        }
        cur = lim + 2;
    }
    out
}

/// Figure 4's p(n) snapshots of a range run again (the gap backfill, goldbach_l0 with
/// GBPSAMP rows): each row is one slice's sampled histogram (psev evens, psamp counts by
/// prime index, classes summed), one row per start. Upward from RANGE_LO the rows are
/// summed, as for the gaps. Above that, a boundary x is still exact while rows cover
/// (x, end] contiguously: the state (`counts`, `evens`, cumulative to `end`) minus those
/// rows, the backfill sampling the same segments as the run that built the state.
/// Snapshots sit at the HISTORY_STEP boundaries (x = the limit of the slice ending there).
fn psamp_history(root: &Path, end: u64, counts: &[u64], evens: u64) -> Vec<Value> {
    fn rows(root: &Path, n: usize) -> Option<HashMap<u64, (u64, u64, Vec<u64>)>> {
        let mut rows = HashMap::new();
        for line in std::fs::read_to_string(root.join("goldbach_stats.log")).ok()?.lines() {
            if !line.starts_with("GBPSAMP ") {
                continue;
            }
            let kv = kvs(line.split_whitespace().skip(2));
            let start = kv.get("start")?.parse().ok()?;
            let mut v = vec![0u64; n];
            for (i, c) in kv.get("psamp")?.split(',').take(n).enumerate() {
                v[i] = if c.is_empty() { 0 } else { c.parse().ok()? };
            }
            rows.entry(start).or_insert((kv.get("limit")?.parse().ok()?, kv.get("psev")?.parse().ok()?, v));
        }
        Some(rows)
    }
    let n = counts.len();
    let Some(rows) = rows(root, n) else { return vec![] };
    let step = HISTORY_STEP as u64;
    let boundary = |lim: u64| (lim + 2 - RANGE_LO) % step == 0;
    let snap = |x: u64, ev: u64, c: Vec<u64>| json!({ "x": x.to_string(), "evens": ev, "counts": c });
    let mut out = vec![];
    // upward: the rows alone, contiguous from RANGE_LO
    let (mut cur, mut ev, mut acc) = (RANGE_LO, 0u64, vec![0u64; n]);
    while let Some((lim, pe, c)) = rows.get(&cur) {
        ev += pe;
        acc.iter_mut().zip(c).for_each(|(a, b)| *a += b);
        if boundary(*lim) && *lim <= end {
            out.push(snap(*lim, ev, acc.clone()));
        }
        cur = lim + 2;
    }
    let top = cur; // rows cover [RANGE_LO, top - 2]
    // downward: the state at `end` minus the rows above x
    let by_limit: HashMap<u64, u64> = rows.iter().map(|(s, r)| (r.0, *s)).collect();
    let (mut hi, mut ev, mut acc) = (end, 0u64, vec![0u64; n]);
    let mut down = vec![];
    while let Some(&s) = by_limit.get(&hi) {
        let (_, pe, c) = &rows[&s];
        ev += pe;
        acc.iter_mut().zip(c).for_each(|(a, b)| *a += b);
        let x = s - 2;
        if s <= RANGE_LO || x < top {
            break; // the upward sums hold it (or nothing lies below)
        }
        if boundary(x) && ev <= evens && acc.iter().zip(counts).all(|(a, b)| a <= b) {
            down.push(snap(x, evens - ev, counts.iter().zip(&acc).map(|(b, a)| b - a).collect()));
        }
        hi = x;
    }
    down.reverse();
    out.extend(down);
    out
}

/// Keeps a gzipped copy of the statistics state (+ its gap file) each time the p(n) or
/// the gap range crosses an ARCHIVE_STEP boundary. Never fails the sync.
fn archive_state(p: &Paths, rng: &Option<[String; 2]>, gap_ranges: &[[String; 2]]) {
    let run = || -> R<()> {
        let mut gend = RANGE_LO - 2; // the gap run contiguous from RANGE_LO (a backfill grows it)
        let mut rs: Vec<(u64, u64)> = gap_ranges.iter().map(|r| Ok((num(&r[0])?, num(&r[1])?))).collect::<R<_>>()?;
        rs.sort();
        for (lo, hi) in rs {
            if lo <= gend + 2 {
                gend = gend.max(hi);
            }
        }
        let pend = match rng {
            Some(r) => num::<u64>(&r[1])?,
            None => RANGE_LO - 2,
        };
        let name = format!("state-{}-{}.tar.gz", (pend + 2 - RANGE_LO) / ARCHIVE_STEP, (gend + 2 - RANGE_LO) / ARCHIVE_STEP);
        let path = p.archive.join(name);
        if path.exists() {
            return Ok(());
        }
        std::fs::create_dir_all(&p.archive).map_err(e)?;
        let tmp = path.with_extension("gz.tmp");
        let gz = flate2::write::GzEncoder::new(std::fs::File::create(&tmp).map_err(e)?, flate2::Compression::default());
        let mut t = tar::Builder::new(gz);
        for fnm in ["goldbach_stats.state", "goldbach_stats.state.gaps"] {
            if p.root.join(fnm).exists() {
                t.append_path_with_name(p.root.join(fnm), fnm).map_err(e)?;
            }
        }
        t.into_inner().map_err(e)?.finish().map_err(e)?;
        std::fs::rename(&tmp, &path).map_err(e)
    };
    if let Err(x) = run() {
        eprintln!("state archive skipped: {x:?}");
    }
}

struct Gaps {
    range: Vec<[String; 2]>,
    evens: u64,
    sampled_evens: u64,
    n: u64,
    d1: String,
    walk_n: u64,
    walk_mom: Vec<String>,
    samp: Vec<u64>,
    max: Value,
    tail: Vec<Value>,
}

/// The live paper page's data (/paper): what the native statistics state holds (first
/// occurrences, the sampled p(n) histogram), per-slice speed and cumulative prime / twin
/// counts, and the Figure 4 snapshot history.
fn paper(p: &Paths, rows: &[Row], bnd: &[(u64, u64)], primes: &[u64]) -> R<Value> {
    let (mut first, mut sampled, mut sampled_evens, mut rng): (Vec<Value>, Vec<u64>, u64, Option<[String; 2]>) =
        (vec![], vec![0; primes.len()], 0, None);
    // prime gaps (goldbach_l0 >= the gaps30 release; absent before)
    let mut g = Gaps {
        range: vec![], evens: 0, sampled_evens: 0, n: 0, d1: "0".into(), walk_n: 0, walk_mom: vec![], samp: vec![],
        max: Value::Null, tail: vec![],
    };
    let state = p.root.join("goldbach_stats.state");
    let mut text = std::fs::read_to_string(&state).map_err(e)?;
    // gap records: goldbach_l0 >= 2.4 keeps them in their own file next to the state
    let gfile = p.root.join("goldbach_stats.state.gaps");
    if gfile.exists() {
        text.push('\n');
        text.push_str(&std::fs::read_to_string(gfile).map_err(e)?);
    }
    for line in text.lines() {
        let w: Vec<&str> = line.split_whitespace().collect();
        let Some(&key) = w.first() else { continue };
        let at = |i: usize| w.get(i).copied().ok_or_else(|| format!("short line: {line}"));
        match key {
            "first" => first.push(json!([num::<u64>(at(1)?)?, at(2)?])),
            "sampled" if w.len() > 2 => {
                for (i, c) in at(2)?.split(',').take(primes.len()).enumerate() {
                    if !c.is_empty() {
                        sampled[i] += num::<u64>(c)?;
                    }
                }
            }
            "evens" => sampled_evens = num(at(2)?)?,
            "range" => rng = Some([at(1)?.into(), at(2)?.into()]),
            "gaps_range" => g.range.push([at(1)?.into(), at(2)?.into()]),
            "gaps_evens" => {
                g.evens = num(at(1)?)?;
                g.sampled_evens = num(at(2)?)?;
            }
            "gaps" => {
                // exact: count, sum of gaps
                g.n = num(at(1)?)?;
                g.d1 = at(2)?.into();
            }
            "gaps_walk" => {
                // walked segments: count, sums of g^1..g^4
                g.walk_n = num(at(1)?)?;
                g.walk_mom = w[2..6.min(w.len())].iter().map(|s| s.to_string()).collect();
            }
            "gaps_samp" => {
                g.samp = if w.len() > 1 { w[1].split(',').map(|c| if c.is_empty() { Ok(0) } else { num(c) }).collect::<R<_>>()? } else { vec![] }
            }
            "gaps_max" => g.max = json!([num::<u64>(at(1)?)?, at(2)?, w.len() > 3 && w[3] == "1"]), // g, lower prime, 1 = exact
            "gaps_tail" => g.tail.push(json!([num::<u64>(at(1)?)?, num::<u64>(at(2)?)?, at(3)?])),
            _ => {}
        }
    }
    while sampled.last() == Some(&0) {
        sampled.pop();
    }
    let (mut series, mut cp, mut ct) = (vec![], 0u64, 0u64);
    for ((s, l, kv), b) in rows.iter().zip(bnd) {
        cp += kv_u64(kv, "primes")? + b.0;
        ct += kv_u64(kv, "twins")? + b.1;
        series.push((s.to_string(), l.to_string(), cp.to_string(), ct.to_string(), round_to(kv_f64(kv, "gpu_s")?, 3)));
    }
    // Figure 4 history: append a snapshot each HISTORY_STEP of progress
    let hist: Vec<Value> = std::fs::read_to_string(&p.history).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let old = hist.clone();
    // p(n) snapshots (gap-only ones have no counts)
    let mut pn: Vec<Value> = hist.into_iter().filter(|s| s["counts"].as_array().is_some_and(|c| !c.is_empty())).collect();
    if let (Some(r), Some(last)) = (&rng, pn.last()) {
        if xnum(&json!(r[1])) < xnum(&last["x"]) {
            pn.clear(); // the statistics restarted: a new accumulation
        }
    }
    if let Some(r) = &rng {
        if pn.last().is_none_or(|last| xnum(&json!(r[1])) - xnum(&last["x"]) >= HISTORY_STEP) {
            let counts: Vec<u64> = primes.iter().zip(&sampled).filter(|(p, _)| **p < 250).map(|(_, c)| *c).collect();
            pn.push(json!({ "x": r[1], "evens": sampled_evens, "counts": counts,
                            "gevens": g.sampled_evens, "gcounts": &g.samp[..g.samp.len().min(107)] }));
        }
    }
    // Figure 8's gap snapshots are rebuilt from the statistics log every time, so
    // whatever range banks gaps (the backfill too) and however late the sync runs, none
    // goes missing
    let xs: HashSet<String> = pn.iter().map(|s| text_of(&s["x"])).collect();
    // Figure 4's columns of a range run again come from its GBPSAMP rows the same way
    // (rebuilt every time, sent but not kept: the kept history is the live cadence's;
    // a gap snapshot at the same x stays a separate entry, each figure reads its own)
    let ps: Vec<Value> = match &rng {
        Some(r) => {
            let k = primes.iter().filter(|p| **p < 250).count();
            let small: Vec<u64> = (0..k).map(|i| sampled.get(i).copied().unwrap_or(0)).collect();
            psamp_history(&p.root, num(&r[1])?, &small, sampled_evens).into_iter().filter(|s| !xs.contains(&text_of(&s["x"]))).collect()
        }
        None => vec![],
    };
    let gs: Vec<Value> = gap_history(&p.root).into_iter().filter(|s| !xs.contains(&text_of(&s["x"]))).collect();
    let mut hist: Vec<Value> = pn.iter().chain(&gs).cloned().collect();
    hist.sort_by_key(|s| xnum(&s["x"]));
    if hist != old {
        if let Some(d) = p.history.parent() {
            std::fs::create_dir_all(d).map_err(e)?;
        }
        let tmp = PathBuf::from(format!("{}.tmp", p.history.display()));
        std::fs::write(&tmp, serde_json::to_string(&hist).map_err(e)?).map_err(e)?;
        std::fs::rename(&tmp, &p.history).map_err(e)?;
    }
    archive_state(p, &rng, &g.range);
    let slices: Vec<Value> = series.iter().map(|r| json!([r.0, r.1, r.4])).collect();
    let counts: Vec<Value> = series.iter().map(|r| json!([r.1, r.2, r.3])).collect();
    let mut fig4 = thin(&pn, HISTORY_SEND);
    fig4.extend(thin(&ps, HISTORY_SEND));
    fig4.extend(thin(&gs, HISTORY_SEND));
    fig4.sort_by_key(|s| xnum(&s["x"]));
    Ok(json!({
        "synced": now(), "range": rng, "first": first, "sampled": sampled, "sampledEvens": sampled_evens,
        "slices": thin(&slices, SERIES_POINTS * 2), "counts": thin(&counts, SERIES_POINTS), "fig4": fig4,
        "gaps": { "range": g.range, "evens": g.evens, "sampledEvens": g.sampled_evens, "n": g.n, "d1": g.d1,
                  "walk": { "n": g.walk_n, "mom": g.walk_mom }, "samp": g.samp, "max": g.max, "tail": g.tail },
    }))
}

fn text_of(v: &Value) -> String {
    v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())
}

/// {p: evens with p(n) = p} for p >= tail_t over the campaign, from the statistics state
/// (tailcls per class + tailagg: what the run log's tail= fields add up to, without
/// reading the ever-growing log).
fn state_tail(root: &Path, primes: &[u64]) -> R<BTreeMap<u64, u64>> {
    let (mut tot, mut tail_t) = (vec![0u64; primes.len()], 0u64);
    for line in std::fs::read_to_string(root.join("goldbach_stats.state")).map_err(e)?.lines() {
        let w: Vec<&str> = line.split_whitespace().collect();
        match w.first() {
            Some(&"tail_t") => tail_t = num(w.get(1).ok_or("tail_t")?)?,
            Some(&k @ ("tailcls" | "tailagg")) => {
                let head = if k == "tailagg" { 1 } else { 2 }; // "tailagg LIST" / "tailcls CLASS LIST"
                let lst = w.get(head).copied().unwrap_or("");
                if !lst.is_empty() {
                    for (i, c) in lst.split(',').take(primes.len()).enumerate() {
                        if !c.is_empty() {
                            tot[i] += num::<u64>(c)?;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(primes.iter().zip(tot).filter(|&(&p, c)| c > 0 && p >= tail_t).map(|(&p, c)| (p, c)).collect())
}

/// (PASS rows (start, limit, kv) sorted, {start: banked at}) of a journal; a RECHECK row
/// whose re-run matched primecount replaces the counts of its PASS row (a transient
/// fault that left the verification intact: 2026-10-03 slice).
fn journal_rows(path: &Path) -> R<(Vec<Row>, HashMap<u64, String>)> {
    let (mut rows, mut fixed, mut stamps): (Vec<Row>, HashMap<(u64, u64), Kv>, HashMap<u64, String>) = Default::default();
    for line in std::fs::read_to_string(path).map_err(e)?.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 4 || !matches!(parts[0], "PASS" | "RECHECK") {
            continue;
        }
        let kv = kvs(parts[4..].iter().copied());
        let (s, l): (u64, u64) = (num(parts[1])?, num(parts[2])?);
        if parts[0] == "PASS" {
            stamps.insert(s, parts[3].to_string());
            rows.push((s, l, kv));
        } else if kv.get("primecount_ok").is_some_and(|v| v == "1") || kv.get("outcome").is_some_and(|v| v == "CONFIRMED") {
            fixed.insert((s, l), ["primes", "twins"].iter().filter_map(|k| kv.get(*k).map(|v| (k.to_string(), v.clone()))).collect());
        }
    }
    for r in &mut rows {
        if let Some(f) = fixed.get(&(r.0, r.1)) {
            r.2.extend(f.clone());
        }
    }
    rows.sort_by_key(|r| (r.0, r.1));
    Ok((rows, stamps))
}

/// The resume point: the end of the banked slices contiguous from lo.
fn contiguous(rows: &[Row], lo: u64) -> u64 {
    let mut resume = lo;
    for (s, l, _) in rows {
        if *s <= resume && resume <= l + 2 {
            resume = resume.max(l + 2);
        }
    }
    resume
}

/// The board's latest banked slices, newest first.
fn recent_slices(rows: &[Row], stamps: &HashMap<u64, String>) -> R<Vec<Value>> {
    rows.iter()
        .rev()
        .take(8)
        .map(|(s, l, kv)| {
            let gpu = kv_f64(kv, "gpu_s")?;
            let div = if kv.contains_key("gpu_s") { gpu } else { 1.0 };
            Ok(json!({ "start": s.to_string(), "limit": l.to_string(), "at": stamps.get(s).cloned().unwrap_or_default(),
                       "gpu_s": gpu, "rate": ((l - s) / 2 + 1) as f64 / if div == 0.0 { 1.0 } else { div },
                       "pmax": kv_u64(kv, "stats_pmax")? }))
        })
        .collect()
}

/// Evens per second over the last 6 banked slices (0 without any).
fn speed(rows: &[Row]) -> R<f64> {
    let recent = &rows[rows.len().saturating_sub(6)..];
    let gs: f64 = recent.iter().map(|r| kv_f64(&r.2, "gpu_s")).sum::<R<f64>>()?;
    Ok(if gs > 0.0 { recent.iter().map(|r| ((r.1 - r.0) / 2 + 1) as f64).sum::<f64>() / gs } else { 0.0 })
}

fn json_u64(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.parse().ok())
}

/// While production runs another range than the main one below the main range's resume
/// point (the v2.4 gap statistics re-sieved from 4e18: ci-state/production.json names its
/// journal), that range's progress: the board shows it instead of a B580 that seems to
/// stall. None otherwise.
fn backfill_progress(root: &Path, main_resume: u64) -> Option<Value> {
    let pc: Value = serde_json::from_str(&std::fs::read_to_string(root.join("ci-state/production.json")).ok()?).ok()?;
    let (lo, hi, jn) = (json_u64(&pc["start"])?, json_u64(&pc["limit"])?, pc["journal"].as_str()?);
    if jn == JOURNAL || hi > main_resume {
        return None;
    }
    let (rows, stamps) = journal_rows(&root.join(jn)).ok()?;
    let resume = contiguous(&rows, lo);
    let rate = speed(&rows).ok()?;
    // what the range re-runs for (production.json "what"; the first backfill: the gaps)
    let what = pc["what"].as_str().unwrap_or("gaps");
    let mut out = json!({ "what": what, "start": lo.to_string(), "limit": hi.to_string(), "resume": resume.to_string(),
        "slices": rows.len(), "pct": round_to(100.0 * (resume - lo) as f64 / (hi + 2 - lo) as f64, 4), "rate": rate,
        "eta_days": if rate > 0.0 { json!(round_to((hi + 2 - resume) as f64 / 2.0 / rate / 86400.0, 2)) } else { Value::Null },
        "desired": pc.get("desired").cloned().unwrap_or(json!("")), "recent": recent_slices(&rows, &stamps).ok()? });
    // the slice in progress (goldbach_live.json, rewritten by goldbach_l0 every segment)
    let lv = std::fs::read_to_string(root.join("goldbach_live.json")).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok());
    if let Some(lv) = lv {
        let ok = || -> Option<Value> {
            let total = lv["evens_total"].as_f64().filter(|t| *t != 0.0)?;
            let t = lv["time"].as_f64().or_else(|| lv["time"].as_str()?.parse().ok())?;
            if json_u64(&lv["start"])? != resume || now() - t >= 600.0 {
                return None;
            }
            Some(json!({ "start": text_of(&lv["start"]), "done": round_to(lv["evens_done"].as_f64()? / total, 4),
                         "elapsed_s": round_to(lv["elapsed_s"].as_f64()?, 1), "at": lv["time"] }))
        };
        if let Some(n) = ok() {
            out["now"] = n;
        }
    }
    Some(out)
}

/// {"paper": ...}, or {} when the statistics files cannot be read: the live paper must
/// never stop the progress / floor sync.
fn paper_or_nothing(p: &Paths, rows: &[Row], bnd: &[(u64, u64)], primes: &[u64]) -> Map<String, Value> {
    let mut m = Map::new();
    match paper(p, rows, bnd, primes) {
        Ok(v) => {
            m.insert("paper".into(), v);
        }
        Err(x) => eprintln!("live paper data skipped: {x:?}"),
    }
    m
}

fn run() -> R<()> {
    let p = paths();
    let (mut url, mut token_file) = ("https://goldbach.net01.io".to_string(), home().join(".config/goldbach/token"));
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--url" => url = args.next().ok_or("--url needs a value")?,
            "--token-file" => token_file = PathBuf::from(args.next().ok_or("--token-file needs a value")?),
            _ => return Err(format!("usage: goldbach-sync-native [--url URL] [--token-file FILE] (got {a})")),
        }
    }
    let token = std::fs::read_to_string(&token_file).map_err(|x| format!("{}: {x}", token_file.display()))?.trim().to_string();

    // ---- progress from the journal (PASS rows; contiguous from RANGE_LO) ----
    let (rows, stamps) = journal_rows(&p.root.join(JOURNAL))?;
    let resume = contiguous(&rows, RANGE_LO);
    let rate = speed(&rows)?;
    // over each slice's count range (start, limit + 1]: totals of adjacent slices = pi(N) - pi(S)
    let bnd: Vec<(u64, u64)> = rows.iter().map(|r| boundary_counts(r.1)).collect();
    let mut primes_n = bnd.iter().map(|b| b.0).sum::<u64>();
    let mut twins_n = bnd.iter().map(|b| b.1).sum::<u64>();
    for r in &rows {
        primes_n += kv_u64(&r.2, "primes")?;
        twins_n += kv_u64(&r.2, "twins")?;
    }
    let mut progress = json!({ "resume": resume.to_string(), "slices": rows.len(), "rate": rate,
        "last": rows.last().and_then(|r| r.2.get("release").cloned()).unwrap_or_default(),
        "primes": primes_n.to_string(), "twins": twins_n.to_string(), "recent": recent_slices(&rows, &stamps)? });
    if let Some(bf) = backfill_progress(&p.root, resume) {
        progress["backfill"] = bf;
    }

    // ---- ridges (top 30 store) ----
    let mut ridges = vec![];
    for line in std::fs::read_to_string(p.root.join("goldbach_ridges.csv")).map_err(e)?.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.trim().split(',').collect();
        if f.len() < 6 {
            return Err(format!("ridges: short line {line:?}"));
        }
        ridges.push(json!({ "n": f[1], "p": num::<u64>(f[2])?, "found": if f[5].is_empty() { 0 } else { num::<u64>(f[5])? } }));
    }

    // ---- tail histogram (p >= tail_t): the state's exact campaign totals ----
    let primes = odd_primes(9781);
    let tail = state_tail(&p.root, &primes)?;

    let floor = RANGE_HI.min(resume + (rate * 2.0 * 86400.0 * FLOOR_AHEAD_DAYS as f64) as u64);
    let mut data = Map::new();
    data.insert("progress".into(), progress);
    data.insert("ridges".into(), ridges.into());
    data.insert("tail".into(), tail.iter().map(|(p, c)| json!([p, c])).collect::<Vec<_>>().into());
    data.extend(paper_or_nothing(&p, &rows, &bnd, &primes));
    let body = json!({ "token": token, "floor": floor.to_string(), "data": data }).to_string();
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).build().into();
    let mut r = agent
        .post(format!("{}/api/native", url.trim_end_matches('/')))
        .header("User-Agent", "goldbach-sync")
        .content_type("application/json")
        .send(body.as_bytes())
        .map_err(e)?;
    let status = r.status();
    let reply: String = r.body_mut().read_to_string().map_err(e)?.chars().take(200).collect();
    println!("{} {reply} resume={resume} floor={floor} rate={rate:.3e}", status.as_u16());
    Ok(())
}

fn main() {
    if let Err(x) = run() {
        eprintln!("goldbach-sync-native: {x}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn miller_rabin() {
        assert!(is_prime(2) && is_prime(4_000_000_000_000_000_059) == is_prime(4_000_000_000_000_000_059));
        assert!(is_prime(1_000_000_007) && !is_prime(1_000_000_007 * 3));
        assert!(!is_prime(3_215_031_751)); // strong pseudoprime to bases 2, 3, 5, 7
        assert!(is_prime(18_446_744_073_709_551_557)); // the largest prime below 2^64
    }

    #[test]
    fn thinning_keeps_the_last() {
        let v: Vec<Value> = (0..1000).map(Value::from).collect();
        let t = thin(&v, 10);
        assert_eq!((t.len(), t[0].clone(), t[9].clone()), (10, 0.into(), 999.into()));
        assert_eq!(thin(&v[..5], 10).len(), 5);
    }

    #[test]
    fn contiguous_resume() {
        let r = |s, l| (s, l, Kv::new());
        let rows = vec![r(RANGE_LO, RANGE_LO + 98), r(RANGE_LO + 100, RANGE_LO + 198), r(RANGE_LO + 400, RANGE_LO + 498)];
        assert_eq!(contiguous(&rows, RANGE_LO), RANGE_LO + 200);
    }

    #[test]
    fn odd_primes_are_odd() {
        let p = odd_primes(9781);
        assert_eq!((p[0], *p.last().unwrap()), (3, 9781));
    }

    #[test]
    fn journal_recheck_replaces_counts() {
        let dir = std::env::temp_dir().join(format!("gbsync-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("j");
        std::fs::write(&f, "PASS 10 20 t1 primes=1 twins=2 gpu_s=3\nRECHECK 10 20 t2 primecount_ok=1 primes=5 twins=6\nnoise\n").unwrap();
        let (rows, stamps) = journal_rows(&f).unwrap();
        assert_eq!((rows[0].2["primes"].as_str(), rows[0].2["twins"].as_str(), stamps[&10].as_str()), ("5", "6", "t1"));
    }

    #[test]
    fn figure_4_columns_of_a_rerun_range_come_from_gbpsamp_rows() {
        let td = tmp("psamp");
        let (lo, step) = (RANGE_LO, HISTORY_STEP as u64);
        let end = lo + 4 * step - 2;
        // half-step slices: two from RANGE_LO (upward sums), a hole, four ending at `end`
        // (the state minus the rows above x); one logged twice, a gap row in between
        let mut log = String::new();
        for (k, c) in [(0, "1,2"), (1, "3,,1"), (4, "1,1"), (5, "2"), (6, ",4"), (6, "9,9"), (7, "1,1,1")] {
            let a = lo + k * step / 2;
            log += &format!("GBPSAMP v1 version=t start={a} limit={} evens={} psev={} psamp={c}\n", a + step / 2 - 2, step / 4, 10 + k);
            log += &format!("GBGAPS v1 start={a} limit={} evens=1 gsamp=,1\n", a + step / 2 - 2);
        }
        std::fs::write(td.join("goldbach_stats.log"), log).unwrap();
        let h = psamp_history(&td, end, &[100, 100, 100], 1000);
        let got: Vec<(String, u64, Vec<u64>)> = h
            .iter()
            .map(|s| (text_of(&s["x"]), s["evens"].as_u64().unwrap(), s["counts"].as_array().unwrap().iter().map(|c| c.as_u64().unwrap()).collect()))
            .collect();
        assert_eq!(
            got,
            [
                ((lo + step - 2).to_string(), 10 + 11, vec![4, 2, 1]),               // upward: rows 0 + 1
                ((lo + 2 * step - 2).to_string(), 1000 - (14 + 15 + 16 + 17), vec![96, 94, 99]), // state minus rows 4..7
                ((lo + 3 * step - 2).to_string(), 1000 - (16 + 17), vec![99, 95, 99]),          // state minus rows 6, 7
            ],
            "the slice logged twice counts once (its first row); nothing is invented across the hole"
        );
        assert!(psamp_history(&td.join("missing"), end, &[1], 1).is_empty());
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gbsync-{name}-{}-{}", std::process::id(), now()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn paths_in(td: &Path) -> Paths {
        Paths { root: td.to_path_buf(), history: td.join("fig4.json"), archive: td.join("archive") }
    }

    fn one_slice() -> (Vec<Row>, Vec<(u64, u64)>) {
        let kv: Kv = [("primes", "7"), ("twins", "1"), ("gpu_s", "2")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        (vec![(4_000_000_000_000_000_000, 4_000_000_000_099_999_998, kv)], vec![(0, 0)])
    }

    #[test]
    fn the_paper_reads_the_statistics_state_gap_records_included() {
        let td = tmp("paper");
        std::fs::write(
            td.join("goldbach_stats.state"),
            "tail_t 3000\nrange 4000000000000000000 4000000000099999998\nevens 50000000000 200000000\n\
             first 3001 4000000000093167212\nsampled 0 5,,7\nsampled 1 1\n",
        )
        .unwrap();
        std::fs::write(
            td.join("goldbach_stats.state.gaps"),
            "gaps_range 4000000000050000000 4000000000099999998\ngaps_evens 25000000000 100000000\n\
             gaps 1167 99999\ngaps_walk 81 3401 9000000 900000000 99000000000000000000000000001\ngaps_samp ,40,41\n\
             gaps_max 812 4000000000077777777 1\ngaps_tail 812 1 4000000000077777777\nend\n",
        )
        .unwrap();
        let p = paths_in(&td);
        let primes = odd_primes(9781);
        let (rows, bnd) = one_slice();
        let pd = paper(&p, &rows, &bnd, &primes).unwrap();
        let gp = &pd["gaps"];
        assert_eq!(pd["first"], json!([[3001, "4000000000093167212"]]));
        assert_eq!(pd["sampled"].as_array().unwrap()[..3], [json!(6), json!(0), json!(7)]);
        assert_eq!(gp["range"], json!([["4000000000050000000", "4000000000099999998"]]));
        assert_eq!((&gp["evens"], &gp["sampledEvens"]), (&json!(25000000000u64), &json!(100000000)));
        assert_eq!((&gp["n"], &gp["d1"]), (&json!(1167), &json!("99999")));
        assert_eq!(gp["walk"], json!({"n": 81, "mom": ["3401", "9000000", "900000000", "99000000000000000000000000001"]}));
        assert_eq!(gp["samp"], json!([0, 40, 41]));
        assert_eq!(gp["max"], json!([812, "4000000000077777777", true]));
        assert_eq!(gp["tail"], json!([[812, 1, "4000000000077777777"]]));
        let last = pd["fig4"].as_array().unwrap().last().unwrap();
        assert_eq!((&last["gcounts"], &last["gevens"]), (&json!([0, 40, 41]), &json!(100000000)));

        // Figure 8's gap snapshots come from the statistics log (contiguous from 4e18, a slice
        // logged twice counts once), whenever the sync runs
        let step = HISTORY_STEP as u64;
        let mut log = String::new();
        for (s, g) in [(0, ",1,2"), (step / 2, ",3"), (step / 2, ",9"), (step, ",,4"), (3 * step, ",5")] {
            let lo = 4 * 10u64.pow(18) + s;
            log += &format!("GBGAPS v1 start={lo} limit={} evens={} gsamp={g}\n", lo + step / 2 - 2, step / 4);
        }
        log += "GBSTATS v2 start=1 limit=3 evens=1\n";
        std::fs::write(td.join("goldbach_stats.log"), log).unwrap();
        let gh = gap_history(&td);
        let xs: Vec<&str> = gh.iter().map(|s| s["x"].as_str().unwrap()).collect();
        assert_eq!(xs, [(4 * 10u64.pow(18) + step - 2).to_string()], "one snapshot per HISTORY_STEP crossed, stops at the hole");
        assert_eq!((&gh[0]["gcounts"], &gh[0]["gevens"], &gh[0]["counts"]), (&json!([0, 4, 2]), &json!(step / 2), &json!([])));
        let pd = paper(&p, &rows, &bnd, &primes).unwrap();
        let xs: Vec<&str> = pd["fig4"].as_array().unwrap().iter().map(|s| s["x"].as_str().unwrap()).collect();
        assert_eq!(xs, ["4000000000099999998".to_string(), (4 * 10u64.pow(18) + step - 2).to_string()]);
        assert!(!pd["fig4"][0]["counts"].as_array().unwrap().is_empty(), "the p(n) cadence ignores gap snapshots");

        archive_state(
            &p,
            &Some(["4000000000000000000".into(), (4 * 10u64.pow(18) + 2 * 10u64.pow(15) - 2).to_string()]),
            &[["4000000000000000000".into(), (4 * 10u64.pow(18) + 10u64.pow(15) - 2).to_string()], ["4100000000000000000".into(), "4100000000000000098".into()]],
        );
        assert!(td.join("archive/state-2-1.tar.gz").exists(), "archive named by the p(n) / contiguous gap step reached");

        let mut state = std::fs::read_to_string(td.join("goldbach_stats.state")).unwrap();
        state += &format!("tailcls 0 \ntailcls 1 {}5,,2\ntailagg {}1\n", ",".repeat(429), ",".repeat(429));
        std::fs::write(td.join("goldbach_stats.state"), state).unwrap();
        let tail = state_tail(&td, &primes).unwrap();
        assert_eq!(tail, BTreeMap::from([(primes[429], 6), (primes[431], 2)]), "per-class tail + tailagg summed by prime (p >= tail_t)");

        let missing = paths_in(&td.join("missing"));
        assert!(paper_or_nothing(&missing, &[], &[], &primes).is_empty(), "an unreadable state leaves the paper out");
    }

    #[test]
    fn a_backfill_is_sent_as_progress() {
        // a backfill (production re-running a range below the main resume point) is sent as
        // progress.backfill; the board shows it instead of a B580 that seems to stall
        let bd = tmp("backfill");
        std::fs::create_dir_all(bd.join("ci-state")).unwrap();
        let bj = "range-4000000000000000000-4000199999999999998.journal";
        std::fs::write(
            bd.join("ci-state/production.json"),
            json!({"desired": "running", "start": "4000000000000000000", "limit": "4000199999999999998", "journal": bj,
                   "then": [{"journal": JOURNAL}]})
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            bd.join(bj),
            "PASS 4000000000000000000 4000049999999999998 2026-10-19T10:02:22+02:00 gpu_s=400 stats_pmax=6000\n\
             PASS 4000050000000000000 4000099999999999998 2026-10-19T10:09:22+02:00 gpu_s=500\n",
        )
        .unwrap();
        let main_resume = 4_116_600_000_000_000_000;
        let bf = backfill_progress(&bd, main_resume).unwrap();
        assert!(bf["what"] == "gaps" && bf["resume"] == "4000100000000000000" && bf["pct"] == 50.0 && bf["slices"] == 2);
        assert_eq!(bf["recent"][0]["start"], "4000050000000000000");
        assert!((bf["rate"].as_f64().unwrap() - 5e13 / 900.0).abs() < 1.0);
        let eta = (4_000_200_000_000_000_000u64 - 4_000_100_000_000_000_000u64) as f64 / 2.0 / (5e13 / 900.0) / 86400.0;
        assert!((bf["eta_days"].as_f64().unwrap() - eta).abs() < 0.01 && bf.get("now").is_none(), "days left; no live file, no slice in progress");
        let live = |age: f64| {
            std::fs::write(
                bd.join("goldbach_live.json"),
                json!({"time": now() - age, "start": 4_000_100_000_000_000_000u64, "evens_done": 5, "evens_total": 20, "elapsed_s": 99.0}).to_string(),
            )
            .unwrap();
        };
        live(0.0);
        let bfn = backfill_progress(&bd, main_resume).unwrap();
        assert!(bfn["now"]["done"] == 0.25 && bfn["now"]["start"] == "4000100000000000000", "the slice in progress");
        live(3600.0);
        assert!(backfill_progress(&bd, main_resume).unwrap().get("now").is_none(), "a stale live file is no slice in progress");
        assert!(backfill_progress(&bd, 4_000_100_000_000_000_000).is_none(), "a range above the main resume point is no backfill");
        std::fs::write(
            bd.join("ci-state/production.json"),
            json!({"start": "4000000000000000000", "limit": "5000000000000000000", "journal": JOURNAL}).to_string(),
        )
        .unwrap();
        assert!(backfill_progress(&bd, main_resume).is_none(), "the main range itself is no backfill");
    }
}
