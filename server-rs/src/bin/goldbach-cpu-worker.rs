//! goldbach-web CPU worker service: verifies web slices with this machine's
//! spare CPU (the native verifier server/cpu_worker.cpp, one core), so the VPS
//! contributes when it has nothing else to do. Runs under goldbach-cpu.service at
//! idle CPU / IO priority with a memory cap. A deploy never interrupts a slice:
//! between slices the worker sees the new code on disk and exits; systemd starts
//! it again on the new code.
//!
//! Every result is identical to the browsers' (same canonical JSON); before the
//! first slice and once a day it verifies a known window and exits with code 2
//! (no restart) if the answer is ever wrong.
//!
//! env: GOLDBACH_URL     coordinator (default http://172.18.0.1:8090, the VPS bridge)
//!      GB_CPU_BIN       verifier binary (default: cpu_worker next to this file, then ./cpu_worker)
//!      GB_STATE_DIR     client id + base-prime cache (default: current directory)
//!      GB_NAME          device name on the board (default "server (idle CPU)")
//!      GB_SEGMENTS      candidate segment sizes (default 1000000000,2000000000,3000000000)
//!
//! Best engine: at start and once a day every candidate (verifier build x segment
//! size: the portable build and the one tuned to this CPU) verifies the known
//! window; candidates with a wrong answer are dropped. The fastest build runs the
//! slices with its largest exact segment that fits the memory budget. If none is
//! exact the worker stops (exit 2).
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::MetadataExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering::SeqCst};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const HEARTBEAT_S: u64 = 60; // the server expires a lease after 240 s without one
const SELFTEST_EVERY_S: u64 = 86400;
/// client/selftest.js: [start, limit, first 16 hex of the sha256 of the result JSON]
const KNOWN: (&str, &str, &str) = ("4999999998000000000", "5000000000000000000", "3672004b8c97290d");

static STOPPING: AtomicBool = AtomicBool::new(false);
static CHILD: AtomicI32 = AtomicI32::new(0); // pid of the running verifier, 0 = none
static LAST_PEAK_KB: AtomicU64 = AtomicU64::new(0);
static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);

#[derive(Clone, PartialEq, Debug)]
struct Engine {
    bin: String,
    seg: String,
}

impl Engine {
    fn name(&self) -> String {
        let base = std::path::Path::new(&self.bin).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        format!("{base} segment {}e9", self.seg.parse::<f64>().unwrap_or(0.0) / 1e9)
    }
}

struct Cfg {
    url: String,
    bin: String,
    state: String,
    name: String,
    segments: Vec<String>,
    version_file: String,
    exe: String,
}

fn env_or(k: &str, d: &str) -> String {
    std::env::var(k).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| d.to_string())
}

fn cfg() -> Cfg {
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let here = std::path::Path::new(&exe).parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let beside = here.join("cpu_worker");
    let bin = std::env::var("GB_CPU_BIN").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| {
        if beside.exists() {
            beside.to_string_lossy().into_owned()
        } else if std::path::Path::new("./cpu_worker").exists() {
            "./cpu_worker".into()
        } else {
            "cpu_worker".into()
        }
    });
    let state = env_or("GB_STATE_DIR", ".");
    Cfg {
        url: env_or("GOLDBACH_URL", "http://172.18.0.1:8090"),
        bin,
        name: env_or("GB_NAME", "server (idle CPU)"),
        segments: env_or("GB_SEGMENTS", "1000000000,2000000000,3000000000").split(',').map(str::to_string).collect(),
        version_file: here.join("VERSION.json").to_string_lossy().into_owned(),
        exe,
        state,
    }
}

fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

fn log(msg: &str) {
    let s = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (y, mo, d) = civil(s.div_euclid(86400));
    let t = s.rem_euclid(86400);
    println!("{y:04}-{mo:02}-{d:02} {:02}:{:02}:{:02} {msg}", t / 3600, t / 60 % 60, t % 60);
}

fn post(cfg: &Cfg, path: &str, body: &str) -> Result<Value, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(60))).build().into();
    let mut r = agent
        .post(format!("{}{path}", cfg.url))
        .content_type("application/json")
        .send(body.as_bytes())
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&r.body_mut().read_to_string().map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

fn text(v: &Value) -> String {
    v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())
}

fn client_id(cfg: &Cfg) -> Result<String, String> {
    let path = format!("{}/cpu-worker.id", cfg.state);
    if let Ok(s) = std::fs::read_to_string(&path) {
        return Ok(s.trim().to_string());
    }
    let body = serde_json::json!({ "name": cfg.name, "gpu": format!("CPU, 1 core ({})", std::env::consts::ARCH) });
    let cid = text(&post(cfg, "/api/register", &body.to_string())?["client"]);
    std::fs::write(&path, format!("{cid}\n")).map_err(|e| e.to_string())?;
    Ok(cid)
}

fn terminate_child() {
    let pid = CHILD.load(SeqCst);
    if pid > 0 {
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
}

/// Runs the verifier on [start, limit]; returns the result JSON text, or None if stopped.
fn run(cfg: &Cfg, e: &Engine, start: &str, limit: &str, on_progress: &(dyn Fn(u64, u64) + Sync)) -> Result<Option<String>, String> {
    let mut child = Command::new(&e.bin)
        .args([start, limit, "--segment", &e.seg, "--primes", &format!("{}/primes-5e18.v1.bin", cfg.state)])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|x| format!("{}: {x}", e.bin))?;
    let pid = child.id() as i32;
    CHILD.store(pid, SeqCst);
    let (mut stdout, stderr) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let mut out = String::new();
    let mut err_tail: Vec<String> = Vec::new();
    std::thread::scope(|s| {
        let t = s.spawn(|| {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut w = line.split_whitespace();
                if w.next() == Some("progress") {
                    if let (Some(Ok(d)), Some(Ok(t))) = (w.next().map(str::parse::<u64>), w.next().map(str::parse::<u64>)) {
                        on_progress(d, t);
                        continue;
                    }
                }
                err_tail.push(line.trim().to_string());
                if err_tail.len() > 20 {
                    err_tail.remove(0);
                }
            }
        });
        let _ = stdout.read_to_string(&mut out);
        let _ = t.join();
    });
    // wait4 instead of wait: also the child's own peak resident memory (KB on Linux)
    let mut status = 0;
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::wait4(pid, &mut status, 0, &mut usage) };
    CHILD.store(0, SeqCst);
    LAST_PEAK_KB.store(usage.ru_maxrss as u64, SeqCst);
    std::mem::forget(child); // already reaped
    if STOPPING.load(SeqCst) {
        return Ok(None);
    }
    let rc = if libc::WIFEXITED(status) { libc::WEXITSTATUS(status) } else { -libc::WTERMSIG(status) };
    if rc != 0 {
        let n = err_tail.len();
        return Err(format!("verifier exit {rc}: {}", err_tail[n.saturating_sub(3)..].join(" | ")));
    }
    Ok(Some(out.trim().to_string()))
}

/// This service's memory.high (systemd MemoryHigh=) in MB, or None (no limit).
fn memory_budget_mb() -> Option<f64> {
    let cg = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let path = cg.trim().split_once("::")?.1;
    let v = std::fs::read_to_string(format!("/sys/fs/cgroup{path}/memory.high")).ok()?;
    let v = v.trim();
    if v == "max" { None } else { Some(v.parse::<f64>().ok()? / 1048576.0) }
}

fn candidates(cfg: &Cfg) -> Vec<Engine> {
    let mut v = Vec::new();
    for b in [cfg.bin.clone(), format!("{}-native", cfg.bin)] {
        if std::path::Path::new(&b).exists() {
            for s in &cfg.segments {
                v.push(Engine { bin: b.clone(), seg: s.clone() });
            }
        }
    }
    v
}

/// The build by time; the segment: the largest exact one that fits the memory
/// budget. timings: (seconds, engine, peak MB) of the exact candidates. One whose
/// run peaked at >= 90 % of MemoryHigh was being squeezed (pages reclaimed and read
/// back from swap all slice long: 2026-10-13 on the VPS, 3e9 at the 240 MB cap ran
/// at less than half its speed and kept the whole box swapping); when none fits,
/// the smallest exact one. The known window is one segment of 2e9 or less, so its
/// timing cannot show what larger segments save on real slices.
fn pick(timings: &[(f64, Engine, f64)], budget: Option<f64>) -> (Engine, f64) {
    let best = &timings.iter().min_by(|a, b| a.0.total_cmp(&b.0)).unwrap().1.bin;
    let seg = |e: &Engine| e.seg.parse::<u64>().unwrap_or(0);
    let same: Vec<_> = timings.iter().filter(|t| &t.1.bin == best).collect();
    let fits: Vec<_> = same.iter().filter(|t| budget.is_none_or(|b| t.2 < 0.9 * b)).collect();
    let chosen = if fits.is_empty() {
        same.iter().min_by_key(|t| seg(&t.1)).unwrap()
    } else {
        fits.iter().max_by_key(|t| seg(&t.1)).unwrap()
    };
    (chosen.1.clone(), chosen.2)
}

/// Benchmarks every candidate on the known window (also the self-test) and selects the
/// fastest one with the exact answer.
fn choose_engine(cfg: &Cfg) {
    let mut timings = Vec::new();
    for cand in candidates(cfg) {
        let t0 = Instant::now();
        let out = match run(cfg, &cand, KNOWN.0, KNOWN.1, &|_, _| {}) {
            Ok(Some(o)) => o,
            Ok(None) => return,
            Err(e) => {
                log(&format!("candidate {}: {e}: never used", cand.name()));
                continue;
            }
        };
        let dt = t0.elapsed().as_secs_f64();
        let h: String = Sha256::digest(out.as_bytes()).iter().take(8).map(|b| format!("{b:02x}")).collect();
        let peak = LAST_PEAK_KB.load(SeqCst) as f64 / 1024.0;
        if h != KNOWN.2 {
            log(&format!("candidate {}: WRONG ANSWER {h} (expected {}): never used", cand.name(), KNOWN.2));
            continue;
        }
        log(&format!("candidate {}: exact, {dt:.1} s, peak {peak:.0} MB", cand.name()));
        timings.push((dt, cand, peak));
    }
    if timings.is_empty() {
        log("SELF-TEST FAILED: no candidate computes the exact answer on this machine; stopping");
        std::process::exit(2);
    }
    let budget = memory_budget_mb();
    let dt = timings.iter().map(|t| t.0).fold(f64::INFINITY, f64::min);
    let (e, peak) = pick(&timings, budget);
    let lim = match budget {
        Some(b) => format!(" of {b:.0} MB"),
        None => ", no limit".into(),
    };
    let rate = (KNOWN.1.parse::<f64>().unwrap() - KNOWN.0.parse::<f64>().unwrap()) / 2.0 / dt / 1e9;
    log(&format!(
        "engine: {} (build chosen by time: {rate:.3} G evens/s on the known window; segment: the largest exact one that fits the memory budget: peak {peak:.0} MB{lim})",
        e.name()
    ));
    *ENGINE.lock().unwrap() = Some(e);
}

fn engine() -> Engine {
    ENGINE.lock().unwrap().clone().expect("engine chosen")
}

/// Verifies one leased slice, heartbeating; returns (result JSON text, seconds) or None.
fn verify(cfg: &Cfg, cid: &str, lease: &Value) -> Result<Option<(String, f64)>, String> {
    let done = AtomicU64::new(0);
    let lost = AtomicBool::new(false);
    let finished = AtomicBool::new(false); // this slice's run ended: its ticker stops
    let beat = Mutex::new(Instant::now());
    let heartbeat = |d: u64| {
        let body = serde_json::json!({ "client": cid, "lease": lease["lease"], "done": d.to_string() });
        match post(cfg, "/api/heartbeat", &body.to_string()) {
            Ok(v) if v["ok"].as_bool().unwrap_or(false) => {}
            Ok(_) => {
                lost.store(true, SeqCst);
                log("lease lost; dropping the slice");
                if !finished.load(SeqCst) {
                    terminate_child(); // only this slice's verifier, never the next one's
                }
            }
            Err(e) => log(&format!("heartbeat failed: {e}")), // a missed heartbeat is retried at the next segment
        }
    };
    let progress = |d: u64, _total: u64| {
        done.store(d, SeqCst);
        let mut b = beat.lock().unwrap();
        if b.elapsed().as_secs() >= HEARTBEAT_S {
            *b = Instant::now();
            drop(b);
            heartbeat(d);
        }
    };
    let t0 = Instant::now();
    let e = engine();
    let out = std::thread::scope(|s| {
        // heartbeats also between segments (a segment can take longer than a minute on a slow core)
        s.spawn(|| {
            let mut waited = 0;
            while !finished.load(SeqCst) && !STOPPING.load(SeqCst) && !lost.load(SeqCst) {
                std::thread::sleep(Duration::from_millis(500));
                waited += 1;
                if waited >= HEARTBEAT_S * 2 {
                    waited = 0;
                    heartbeat(done.load(SeqCst));
                }
            }
        });
        let r = run(cfg, &e, &text(&lease["start"]), &text(&lease["limit"]), &progress);
        finished.store(true, SeqCst);
        r
    })?;
    match out {
        Some(o) if !lost.load(SeqCst) => {
            let end = o.rfind('}').ok_or("verifier output is not JSON")?;
            Ok(Some((format!("{},\"pass\":true}}", &o[..end]), t0.elapsed().as_secs_f64())))
        }
        _ => Ok(None),
    }
}

/// The deployed commit and the engine in use: sent with every result (dashboard: speed per version).
fn version(cfg: &Cfg) -> Value {
    let mut v = std::fs::read_to_string(&cfg.version_file)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| serde_json::json!({ "commit": "unknown" }));
    v["engine"] = engine().name().into();
    v
}

/// Identity of the code on disk (verifier binary + this program): a deploy changes it.
fn code_id(cfg: &Cfg) -> Vec<Option<(u64, i64, i64)>> {
    [cfg.bin.clone(), format!("{}-native", cfg.bin), cfg.exe.clone(), cfg.version_file.clone()]
        .iter()
        .map(|f| std::fs::metadata(f).ok().map(|m| (m.ino(), m.mtime(), m.mtime_nsec())))
        .collect()
}

fn sleep_unless_stopped(secs: u64) {
    for _ in 0..secs * 4 {
        if STOPPING.load(SeqCst) {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

extern "C" fn on_signal(_: libc::c_int) {
    STOPPING.store(true, SeqCst);
    terminate_child();
}

fn main() {
    let cfg = cfg();
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as usize);
        libc::signal(libc::SIGINT, on_signal as *const () as usize);
    }
    let _ = std::fs::create_dir_all(&cfg.state);
    log(&format!("cpu worker: {} candidate engines, server {}", candidates(&cfg).len(), cfg.url));
    let started_code = code_id(&cfg);
    choose_engine(&cfg);
    let mut last_test = Instant::now();
    let cid = loop {
        match client_id(&cfg) {
            Ok(c) => break c,
            Err(e) => {
                log(&format!("error: {e}"));
                if STOPPING.load(SeqCst) {
                    return;
                }
                sleep_unless_stopped(10);
            }
        }
    };
    let mut backoff = 10;
    while !STOPPING.load(SeqCst) {
        if code_id(&cfg) != started_code {
            // deployed: restart on the new code (systemd)
            log("new code deployed: restarting between slices");
            return;
        }
        let mut lease_id: Option<Value> = None;
        let step = (|| -> Result<(), String> {
            if last_test.elapsed().as_secs() > SELFTEST_EVERY_S {
                choose_engine(&cfg);
                last_test = Instant::now();
            }
            let lease = post(&cfg, "/api/lease", &serde_json::json!({ "client": cid }).to_string())?;
            if let Some(w) = lease.get("wait").filter(|w| !w.is_null()) {
                log(&format!("no slice ({}); waiting {} s", text(&lease["why"]), text(w)));
                sleep_unless_stopped(300);
                return Ok(());
            }
            lease_id = Some(lease["lease"].clone());
            log(&format!("slice #{} [{}, {}]", text(&lease["slice"]), text(&lease["start"]), text(&lease["limit"])));
            let Some((res, elapsed)) = verify(&cfg, &cid, &lease)? else {
                post(&cfg, "/api/release", &serde_json::json!({ "client": cid, "lease": lease["lease"] }).to_string())?;
                return Ok(());
            };
            let body = format!(
                "{{\"client\":{},\"lease\":{},\"elapsed\":{elapsed},\"result\":{res},\"version\":{}}}",
                Value::from(cid.as_str()),
                lease["lease"],
                version(&cfg)
            );
            let r = post(&cfg, "/api/result", &body)?;
            let rv: Value = serde_json::from_str(&res).map_err(|e| e.to_string())?;
            let evens: f64 = text(&rv["evens"]).parse().unwrap_or(0.0);
            let why = r.get("why").filter(|w| w.as_str().is_some_and(|s| !s.is_empty())).map(|w| format!(" ({})", text(w))).unwrap_or_default();
            log(&format!(
                "slice #{}: {}{why}, {:.2} h, {:.3} G evens/s, p max {}",
                text(&lease["slice"]),
                text(&r["status"]),
                elapsed / 3600.0,
                evens / elapsed / 1e9,
                text(&rv["pmax"])
            ));
            backoff = 10;
            Ok(())
        })();
        if let Err(msg) = step {
            log(&format!("error: {msg}"));
            if let Some(l) = &lease_id {
                let _ = post(&cfg, "/api/release", &serde_json::json!({ "client": cid, "lease": l }).to_string());
            }
            let m: String = msg.chars().take(500).collect();
            let _ = post(&cfg, "/api/report", &serde_json::json!({ "client": cid, "lease": lease_id.unwrap_or(0.into()), "error": m }).to_string());
            sleep_unless_stopped(backoff);
            backoff = (backoff * 2).min(1800);
        }
    }
    log("stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake verifier that uses ~60 MB per 1e9 of segment, as the old cpu_select_test did:
    /// 1e9 ~ 60 MB, 2e9 ~ 120 MB, 3e9 ~ 180 MB, all equally fast.
    fn timings() -> Vec<(f64, Engine, f64)> {
        ["1000000000", "2000000000", "3000000000"]
            .iter()
            .map(|s| (1.0, Engine { bin: "v".into(), seg: s.to_string() }, s.parse::<f64>().unwrap() / 1e9 * 60.0))
            .collect()
    }

    #[test]
    fn no_limit_takes_the_largest_exact_segment() {
        assert_eq!(pick(&timings(), None).0.seg, "3000000000");
    }

    #[test]
    fn a_squeezed_segment_is_excluded() {
        // 90 % of 160 MB = 144 MB: 3e9 (180) does not fit, 2e9 (120) does
        assert_eq!(pick(&timings(), Some(160.0)).0.seg, "2000000000");
    }

    #[test]
    fn nothing_fits_takes_the_smallest() {
        assert_eq!(pick(&timings(), Some(50.0)).0.seg, "1000000000");
    }

    #[test]
    fn the_faster_build_wins_then_its_segment() {
        let mut t = timings();
        t.push((0.5, Engine { bin: "v-native".into(), seg: "1000000000".into() }, 60.0));
        t.push((0.6, Engine { bin: "v-native".into(), seg: "2000000000".into() }, 120.0));
        let e = pick(&t, None).0;
        assert_eq!((e.bin.as_str(), e.seg.as_str()), ("v-native", "2000000000"));
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_000), (2024, 10, 4));
    }
}
