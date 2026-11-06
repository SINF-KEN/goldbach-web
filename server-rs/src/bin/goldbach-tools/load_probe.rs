//! Coordinator latency under a dashboard + verifier load, from Server-Timing.
//!
//! usage: load-probe BIN DB [--seconds 60] [--pages 30] [--poll 1.0] [--workers 10]
//!                          [--beat 1.0] [--result 10] [--cpu 3] [--root .] [--json OUT]
//!
//! Starts BIN on a copy of DB, pinned to one CPU core (the VPS has one vCPU), then for
//! --seconds:
//!   --pages   open pages polling /api/live every --poll s, as the page does (since, board
//!             hash, newest server slice)
//!   --workers devices: register, lease, heartbeat every --beat s, a result every --result s
//!             (each one invalidates the stats snapshot), lease again
//! and reports latency per endpoint (client side) with the server's own split: lock wait,
//! SQL, stats rebuild, total. The results are synthetic ones the coordinator accepts. Never
//! point it at production. DB: e.g. `goldbach-server seed-demo-db test/stats_fixture.json DB`.
use crate::load::{merge, synthetic, Client, Server, Timing};
use crate::util::{mean, pct, write_json, Args, Rng, R};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::time::{Duration, Instant};

type Samples = HashMap<String, Vec<(f64, Timing)>>;

fn nap(s: f64) {
    std::thread::sleep(Duration::from_secs_f64(s.max(0.0)));
}

fn page(port: u16, i: usize, poll: f64, stop: Instant) -> Samples {
    let mut cl = Client::new(port);
    let (mut since, mut bh, mut runs) = (Value::from(0), String::new(), None::<Value>);
    nap(Rng::from_seed(&format!("page{i}")).unit() * poll);
    while Instant::now() < stop {
        let q = format!("/api/live?narrow=0&since={}&bh={bh}&tiers=300{}", crate::util::text(&since), runs.as_ref().map_or(String::new(), |r| format!("&runs={}", crate::util::text(r))));
        let (s, live) = cl.json("GET", &q, None, &format!("10.1.{}.{}", i / 250, i % 250), None);
        if s == 200 {
            since = live["t"].clone();
            bh = crate::util::text(&live["bh"]);
            if let Some(t) = live["srv"]["last"].get("t").filter(|t| !t.is_null()) {
                runs = Some(t.clone());
            }
        }
        nap(poll);
    }
    cl.lat
}

fn worker(port: u16, i: usize, a: &Cfg, stop: Instant) -> Samples {
    let mut cl = Client::new(port);
    let ip = format!("10.2.{i}.1");
    let (_, d) = cl.json("POST", "/api/register", Some(&json!({ "name": format!("load{i}"), "gpu": "probe" })), &ip, None);
    let cid = d["client"].clone();
    nap(Rng::from_seed(&format!("worker{i}")).unit() * a.beat);
    while Instant::now() < stop {
        let (s, lease) = cl.json("POST", "/api/lease", Some(&json!({ "client": cid })), &ip, None);
        if s != 200 || lease.get("lease").is_none() {
            nap(a.beat);
            continue;
        }
        let t_end = Instant::now() + Duration::from_secs_f64(a.result);
        while Instant::now() < t_end.min(stop) {
            nap(a.beat);
            cl.call("POST", "/api/heartbeat", Some(&json!({ "client": cid, "lease": lease["lease"], "done": "1000" })), &ip, None);
        }
        if Instant::now() < stop {
            cl.call("POST", "/api/result", Some(&json!({ "client": cid, "lease": lease["lease"], "elapsed": a.result,
                                                          "result": synthetic(&lease["start"], &lease["limit"]) })), &ip, None);
        }
    }
    cl.lat
}

struct Cfg {
    seconds: f64,
    pages: usize,
    poll: f64,
    workers: usize,
    beat: f64,
    result: f64,
}

/// endpoint -> {n, p50, p95, max, mean, server p95s, rebuilds}
fn report(samples: &Samples) -> Value {
    let mut keys: Vec<&String> = samples.keys().collect();
    keys.sort();
    let mut out = Map::new();
    println!("{:16} {:>6} {:>8} {:>8} {:>8}   server p95: lock / sql / rebuild / total   rebuilds", "endpoint", "n", "p50 ms", "p95 ms", "max ms");
    for k in keys {
        let v = &samples[k];
        let cl: Vec<f64> = v.iter().map(|x| x.0).collect();
        let col = |f: &str| v.iter().map(|x| x.1.get(f).copied().unwrap_or(0.0)).collect::<Vec<_>>();
        let rebuilds = v.iter().filter(|x| x.1.contains_key("rebuild")).count();
        let o = json!({ "n": v.len(), "p50": pct(&cl, 50.0), "p95": pct(&cl, 95.0), "max": cl.iter().cloned().fold(f64::NAN, f64::max), "mean": mean(&cl),
                        "lock_p95": pct(&col("lock"), 95.0), "sql_p95": pct(&col("sql"), 95.0), "rebuild_p95": pct(&col("rebuild"), 95.0),
                        "total_p95": pct(&col("total"), 95.0), "rebuilds": rebuilds });
        let g = |f: &str| o[f].as_f64().unwrap_or(0.0);
        println!("{k:16} {:6} {:8.2} {:8.2} {:8.1}   {:6.2} / {:6.2} / {:6.2} / {:6.2}   {rebuilds}", v.len(), g("p50"), g("p95"), g("max"),
                 g("lock_p95"), g("sql_p95"), g("rebuild_p95"), g("total_p95"));
        out.insert(k.clone(), o);
    }
    out.into()
}

pub fn run(args: &[String]) -> R<i32> {
    let a = Args::parse(args, &[])?;
    let (bin, db) = (a.positional(0, "BIN")?, a.positional(1, "DB")?);
    let cfg = Cfg { seconds: a.num("seconds", 60.0)?, pages: a.num("pages", 30)?, poll: a.num("poll", 1.0)?, workers: a.num("workers", 10)?,
                    beat: a.num("beat", 1.0)?, result: a.num("result", 10.0)? };
    let srv = Server::start(bin, db, &a.str_or("root", "."), a.num("cpu", 3)?, &[], None)?;
    let stop = Instant::now() + Duration::from_secs_f64(cfg.seconds);
    let mut all = Samples::new();
    std::thread::scope(|s| {
        let hs: Vec<_> = (0..cfg.pages).map(|i| s.spawn(move || page(srv.port, i, cfg.poll, stop)))
            .chain((0..cfg.workers).map(|i| { let c = &cfg; s.spawn(move || worker(srv.port, i, c, stop)) }))
            .collect();
        for h in hs {
            if let Ok(l) = h.join() {
                merge(&mut all, l);
            }
        }
    });
    srv.close();
    let out = report(&all);
    if let Some(p) = a.get("json") {
        write_json(p, &out)?;
    }
    Ok(0)
}
