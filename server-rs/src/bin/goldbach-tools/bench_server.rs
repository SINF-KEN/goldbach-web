//! Benchmarks the Rust coordinator (server-rs) pinned to one CPU core (the VPS has one
//! vCPU), on a copy of a database.
//!
//! usage: bench-server BIN DB [--cpu 2] [--seconds 10] [--conc 32] [--procs 8] [--pages 200]
//!                            [--workers 50] [--root .] [--json OUT] [--skip rebuild,get,post,mixed]
//!                            [--tmpdir DIR] [--wrap 'perf record -g -o rs.perf --']
//!
//!   rebuild  the dashboard snapshot rebuild (stats, boards), in process, N times
//!   get      read throughput and latency per endpoint (oha, keep-alive and a new
//!            connection per request)
//!   post     the device protocol under load: heartbeats; register + lease + result cycles
//!            (two networks per slice, so half of them verify)
//!   mixed    open pages polling /api/live every second while devices beat every second and
//!            send a result every 10 s: latency per endpoint
//!
//! DB: a database shaped like production, e.g. `goldbach-server seed-demo-db test/stats_fixture.json DB`.
//! Load comes from the other cores (oha and client threads). Never point it at production.
use crate::load::{merge, rand_ip, summary, synthetic, Client, Server, Timing};
use crate::util::{e, median, pin_thread, write_json, Args, Rng, R};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

type Samples = HashMap<String, Vec<(f64, Timing)>>;

struct Opt {
    bin: String,
    db: String,
    cpu: usize,
    seconds: f64,
    conc: usize,
    procs: usize,
    pages: usize,
    workers: usize,
    root: String,
    others: Vec<usize>,
    oha: String,
}

fn nap(s: f64) {
    std::thread::sleep(Duration::from_secs_f64(s));
}

/// One worker thread of the POST load: heartbeats, or register + lease + result cycles.
fn post_proc(port: u16, kind: &str, seconds: f64, seed: u64, others: &[usize]) -> (u64, Samples) {
    pin_thread(others);
    let mut rng = Rng::from_seed(&format!("post{seed}"));
    let mut cl = Client::new(port);
    let mut n = 0u64;
    let stop = Instant::now() + Duration::from_secs_f64(seconds);
    if kind == "heartbeat" {
        let (_, d) = cl.json("POST", "/api/register", Some(&json!({ "name": "hb", "gpu": "g" })), &rand_ip(&mut rng), None);
        let cid = d["client"].clone();
        let (_, d) = cl.json("POST", "/api/lease", Some(&json!({ "client": cid })), &rand_ip(&mut rng), None);
        let lease = d["lease"].clone();
        cl.lat.clear();
        while Instant::now() < stop {
            cl.call("POST", "/api/heartbeat", Some(&json!({ "client": cid, "lease": lease, "done": n.to_string() })), &rand_ip(&mut rng), None);
            n += 1;
        }
    } else {
        // register two devices on different networks, both lease (the same slice), both report: verified
        while Instant::now() < stop {
            let mut devs = vec![];
            for _ in 0..2 {
                let ip = rand_ip(&mut rng);
                let (s, d) = cl.json("POST", "/api/register", Some(&json!({ "name": "cy", "gpu": "g" })), &ip, Some("/api/register"));
                if s == 200 {
                    devs.push((d["client"].clone(), ip));
                }
            }
            let mut leases = vec![];
            for (cid, ip) in &devs {
                let (s, d) = cl.json("POST", "/api/lease", Some(&json!({ "client": cid })), ip, None);
                if s == 200 && d.get("lease").is_some() {
                    leases.push((cid.clone(), ip.clone(), d));
                }
            }
            for (cid, ip, l) in leases {
                cl.call("POST", "/api/result", Some(&json!({ "client": cid, "lease": l["lease"], "elapsed": 10,
                                                            "result": synthetic(&l["start"], &l["limit"]) })), &ip, None);
                n += 1;
            }
        }
    }
    (n, cl.lat)
}

fn summaries(lat: &Samples, el: f64) -> Value {
    let mut keys: Vec<&String> = lat.keys().collect();
    keys.sort();
    keys.into_iter().map(|k| (k.clone(), summary(&lat[k], el))).collect::<Map<_, _>>().into()
}

fn post_load(o: &Opt, port: u16, kind: &str) -> (f64, Samples) {
    let t = Instant::now();
    let got: Vec<(u64, Samples)> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..o.procs).map(|i| s.spawn(move || post_proc(port, kind, o.seconds, i as u64 * 7919 + 1, &o.others))).collect();
        hs.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    let el = t.elapsed().as_secs_f64();
    let mut lat = Samples::new();
    let mut n = 0;
    for (k, l) in got {
        n += k;
        merge(&mut lat, l);
    }
    (n as f64 / el, lat)
}

fn mixed_load(o: &Opt, srv: &Server, seconds: f64) -> Value {
    let port = srv.port;
    let c0 = srv.cpu_s();
    let stop = Instant::now() + Duration::from_secs_f64(seconds);
    let mut lat = Samples::new();
    std::thread::scope(|s| {
        let pages = (0..o.pages).map(|i| {
            s.spawn(move || {
                pin_thread(&o.others);
                let mut cl = Client::new(port);
                let (mut since, mut bh) = (Value::from(0), String::new());
                nap(Rng::from_seed(&format!("mp{i}")).unit());
                while Instant::now() < stop {
                    let q = format!("/api/live?narrow=0&since={}&bh={bh}&tiers=300", crate::util::text(&since));
                    let (st, live) = cl.json("GET", &q, None, &format!("10.1.{}.{}", i / 250, i % 250), None);
                    if st == 200 {
                        since = live["t"].clone();
                        bh = crate::util::text(&live["bh"]);
                    }
                    nap(1.0);
                }
                cl.lat
            })
        });
        let workers = (0..o.workers).map(|i| {
            s.spawn(move || {
                pin_thread(&o.others);
                let mut rng = Rng::from_seed(&format!("mw{i}"));
                let mut cl = Client::new(port);
                let ip = rand_ip(&mut rng);
                let (_, d) = cl.json("POST", "/api/register", Some(&json!({ "name": "w", "gpu": "g" })), &ip, None);
                let cid = d["client"].clone();
                nap(rng.unit());
                while Instant::now() < stop {
                    let (st, lease) = cl.json("POST", "/api/lease", Some(&json!({ "client": cid })), &ip, None);
                    if st != 200 || lease.get("lease").is_none() {
                        nap(1.0);
                        continue;
                    }
                    let t_end = Instant::now() + Duration::from_secs(10);
                    while Instant::now() < t_end.min(stop) {
                        nap(1.0);
                        cl.call("POST", "/api/heartbeat", Some(&json!({ "client": cid, "lease": lease["lease"], "done": "1000" })), &ip, None);
                    }
                    if Instant::now() < stop {
                        cl.call("POST", "/api/result", Some(&json!({ "client": cid, "lease": lease["lease"], "elapsed": 10,
                                                                    "result": synthetic(&lease["start"], &lease["limit"]) })), &ip, None);
                    }
                }
                cl.lat
            })
        });
        let hs: Vec<_> = pages.chain(workers).collect();
        for h in hs {
            if let Ok(l) = h.join() {
                merge(&mut lat, l);
            }
        }
    });
    let mut out = summaries(&lat, seconds);
    out["server_cpu"] = ((srv.cpu_s() - c0) / seconds).into();
    out
}

fn oha(o: &Opt, url: &str, keepalive: bool, headers: &[&str]) -> R<Value> {
    let mut cmd: Vec<String> = vec![];
    if cfg!(target_os = "linux") {
        cmd.extend(["taskset".into(), "-c".into(), o.others.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(",")]);
    }
    cmd.extend([o.oha.clone(), "-z".into(), format!("{}s", o.seconds), "-c".into(), o.conc.to_string(), "--no-tui".into(), "--output-format".into(), "json".into()]);
    for h in headers {
        cmd.extend(["-H".into(), h.to_string()]);
    }
    if !keepalive {
        cmd.push("--disable-keepalive".into());
    }
    cmd.push(url.into());
    let out = Command::new(&cmd[0]).args(&cmd[1..]).output().map_err(|x| format!("{}: {x}", cmd[0]))?;
    if !out.status.success() {
        return Err(format!("oha failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    let r: Value = serde_json::from_slice(&out.stdout).map_err(e)?;
    let (s, lp) = (&r["summary"], &r["latencyPercentiles"]);
    Ok(json!({ "rps": s["requestsPerSec"], "p50": lp["p50"].as_f64().unwrap_or(0.0) * 1000.0, "p99": lp["p99"].as_f64().unwrap_or(0.0) * 1000.0,
               "ok": s.get("successRate").cloned().unwrap_or(json!(1.0)), "codes": r.get("statusCodeDistribution").cloned().unwrap_or(json!({})) }))
}

fn get_json(url: &str) -> R<Value> {
    let mut r = ureq::get(url).call().map_err(e)?;
    serde_json::from_str(&r.body_mut().read_to_string().map_err(e)?).map_err(e)
}

pub fn run(args: &[String]) -> R<i32> {
    let a = Args::parse(args, &[])?;
    let cpu: usize = a.num("cpu", 2)?;
    let o = Opt {
        bin: a.positional(0, "BIN")?.into(),
        db: a.positional(1, "DB")?.into(),
        cpu,
        seconds: a.num("seconds", 10.0)?,
        conc: a.num("conc", 32)?,
        procs: a.num("procs", 8)?,
        pages: a.num("pages", 200)?,
        workers: a.num("workers", 50)?,
        root: a.str_or("root", "."),
        others: (0..crate::util::ncpu()).filter(|&c| c != cpu).collect(),
        oha: which("oha"),
    };
    let skip = a.str_or("skip", "");
    let skipped = |s: &str| skip.split(',').any(|x| x == s);
    let mut out = json!({ "cpu": o.cpu, "seconds": o.seconds, "conc": o.conc });
    if !skipped("rebuild") {
        let tmp = std::env::temp_dir().join(format!("gb-rebuild-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).map_err(e)?;
        let db = tmp.join("r.db");
        std::fs::copy(&o.db, &db).map_err(e)?;
        let mut cmd: Vec<String> = vec![];
        if cfg!(target_os = "linux") {
            cmd.extend(["taskset".into(), "-c".into(), o.cpu.to_string()]);
        }
        cmd.extend([o.bin.clone(), "dev".into(), "rebuild".into(), "11".into()]);
        let mut ch = Command::new(&cmd[0]).args(&cmd[1..]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().map_err(e)?;
        std::io::Write::write_all(&mut ch.stdin.take().unwrap(), db.to_string_lossy().as_bytes()).map_err(e)?;
        let r = ch.wait_with_output().map_err(e)?;
        let ms: Vec<f64> = String::from_utf8_lossy(&r.stdout).split_whitespace().filter_map(|x| x.parse().ok()).skip(1).collect();
        let _ = std::fs::remove_dir_all(&tmp);
        if ms.is_empty() {
            return Err("`dev rebuild` printed no timings".into());
        }
        out["rebuild"] = json!({ "rust": median(&ms) });
        println!("snapshot rebuild (median of {}): {:.1} ms", ms.len(), median(&ms));
    }
    let srv = Server::start(&o.bin, &o.db, &o.root, o.cpu, &a.str_or("wrap", "").split_whitespace().map(str::to_string).collect::<Vec<_>>(), a.get("tmpdir"))?;
    let mut res = Map::new();
    res.insert("rss_idle_mb".into(), srv.rss_mb().into());
    let r = (|| -> R<()> {
        if !skipped("get") {
            let live = get_json(&format!("{}/api/live?tiers=300", srv.url()))?;
            let ver = live["v"]["engine"].as_str().unwrap_or("").to_string();
            let paths = [
                ("/api/live (poll)".to_string(), format!("/api/live?narrow=0&since={}&bh={}&tiers=300", crate::util::text(&live["t"]), crate::util::text(&live["bh"]))),
                ("/api/live (first)".into(), "/api/live?narrow=0&tiers=300".into()),
                ("/ (page)".into(), "/".into()),
                ("/api/stats (gzip)".into(), "/api/stats".into()),
                ("/app.js (gzip)".into(), "/app.js".into()),
                ("/worker.js?v= (immutable)".into(), format!("/worker.js?v={ver}")),
                ("/api/health".into(), "/api/health".into()),
            ];
            for (name, path) in &paths {
                for ka in [true, false] {
                    let r = oha(&o, &format!("{}{path}", srv.url()), ka, &["Accept-Encoding: gzip"])?;
                    println!("  GET {name:28} {} {:9.0} req/s  p50 {:7.2} ms  p99 {:8.2} ms  ok {:.3}", if ka { "keep-alive" } else { "new conn  " },
                             r["rps"].as_f64().unwrap_or(0.0), r["p50"].as_f64().unwrap_or(0.0), r["p99"].as_f64().unwrap_or(0.0), r["ok"].as_f64().unwrap_or(0.0));
                    res.insert(format!("GET {name}{}", if ka { "" } else { " [new conn]" }), r);
                }
            }
        }
        if !skipped("post") {
            for kind in ["heartbeat", "cycle"] {
                let (ops, lat) = post_load(&o, srv.port, kind);
                let sm = summaries(&lat, o.seconds);
                for (k, v) in sm.as_object().unwrap() {
                    let g = |f: &str| v[f].as_f64().unwrap_or(0.0);
                    println!("  POST {kind:9} {k:14} {:8.0} req/s  p50 {:7.2}  p95 {:7.2}  p99 {:8.2}  max {:8.1} ms", g("rps"), g("p50"), g("p95"), g("p99"), g("max"));
                }
                res.insert(format!("POST {kind}"), json!({ "ops": ops, "lat": sm }));
            }
        }
        if !skipped("mixed") {
            let m = mixed_load(&o, &srv, o.seconds.max(30.0));
            println!("  mixed ({} pages, {} devices): server CPU {:.0} %", o.pages, o.workers, 100.0 * m["server_cpu"].as_f64().unwrap_or(0.0));
            for (k, v) in m.as_object().unwrap() {
                if k != "server_cpu" {
                    let g = |f: &str| v[f].as_f64().unwrap_or(0.0);
                    println!("    {k:16} n {:6}  p50 {:7.2}  p95 {:7.2}  p99 {:8.2}  max {:8.1} ms", v["n"], g("p50"), g("p95"), g("p99"), g("max"));
                }
            }
            res.insert("mixed".into(), m);
        }
        let end = srv.rss_mb();
        println!("  RSS idle {:.1} MB, after load {end:.1} MB", res["rss_idle_mb"].as_f64().unwrap_or(0.0));
        res.insert("rss_end_mb".into(), end.into());
        Ok(())
    })();
    srv.close();
    r?;
    out["servers"] = json!({ "rust": res });
    if let Some(p) = a.get("json") {
        write_json(p, &out)?;
    }
    Ok(0)
}

fn which(name: &str) -> String {
    std::env::var_os("PATH")
        .and_then(|p| std::env::split_paths(&p).map(|d| d.join(name)).find(|f| f.exists()))
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("{}/.cargo/bin/{name}", std::env::var("HOME").unwrap_or_default()))
}
