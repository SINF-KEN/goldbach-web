//! `goldbach-server seed-demo-db STATS_JSON DB`: a local coordinator database shaped
//! like production, for page / traffic tests and the benchmarks.
//!
//! STATS_JSON is a saved /api/stats (curl --compressed https://goldbach.net01.io/api/stats).
//! The aggregates, native progress, histograms and ridges are copied from it; the
//! speed history (7 days of browser slices, one every ~4 min) and the server CPU's
//! slices (one every ~2.7 h, three deployed versions) are synthesised relative to
//! now, so every chart tier and table is full. No result is real: never point a
//! worker at a server running on this database.
use crate::store::{Store, now, slice_bounds};
use crate::*;
use serde_json::{Value, json};

/// xorshift64*: the synthetic speeds only need to look random, and be repeatable.
struct Rng(u64);
impl Rng {
    fn uniform(&mut self, lo: f64, hi: f64) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let u = (self.0.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64;
        lo + (hi - lo) * u
    }
}

pub fn seed(src: &str, path: &str) -> Result<String, String> {
    let e = |x: &dyn std::fmt::Display| x.to_string();
    let s: Value = serde_json::from_str(&std::fs::read_to_string(src).map_err(|x| e(&x))?).map_err(|x| e(&x))?;
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{path}{ext}"));
    }
    let st = Store::open(path).map_err(|x| e(&format!("{x:?}")))?;
    let db = st.lock();
    let mut rng = Rng(0x9E3779B97F4A7C15);
    let t_now = now();
    let q = |r: rusqlite::Result<usize>| r.map(|_| ()).map_err(|x| e(&x));

    let mut agg = crate::store::Db::agg_empty();
    for k in ["evens", "primes", "twins", "pmax", "lowest"] {
        agg[k] = s["web"][k].clone();
    }
    agg["hist"] = Value::Object(s["hist"].as_array().into_iter().flatten().map(|h| (h[0].to_string(), h[1].clone())).collect());
    let web_ridge = |r: &&Value| r["src"] == "web";
    agg["ridges"] = Value::Array(s["ridges"].as_array().into_iter().flatten().filter(web_ridge).map(|r| json!({"n": r["n"], "p": r["p"]})).collect());
    db.set_meta(AGG_KEY, &agg.to_string()).map_err(|x| e(&format!("{x:?}")))?;
    let b580: Vec<Value> = s["ridges"].as_array().into_iter().flatten().filter(|r| r["src"] == "B580").map(|r| json!({"n": r["n"], "p": r["p"]})).collect();
    db.set_meta("native", &json!({"progress": s["native"], "tail": s["nativeHist"], "ridges": b580}).to_string())
        .map_err(|x| e(&format!("{x:?}")))?;

    let (verified, open) = (s["web"]["verified"].as_i64().unwrap_or(0), s["web"]["open"].as_i64().unwrap_or(0));
    db.batch("BEGIN").map_err(|x| e(&x))?;
    for i in 0..verified + open {
        let (a, b) = slice_bounds(i as i128);
        q(db.exec(
            "INSERT INTO slices(idx, start, lim, state, verified_at) VALUES(?,?,?,?,?)",
            &[&i, &a.to_string(), &b.to_string(), &(if i < verified { "verified" } else { "open" }), &(t_now - 600.0 * i as f64)],
        ))?;
    }
    let evens = *SLICE / 2;
    q(db.exec("INSERT INTO clients(id, name, gpu, ip, created, last_seen) VALUES('b','browser','gpu','1.2.3.4',?,?)", &[&(t_now - 8.0 * 86400.0), &t_now]))?;
    q(db.exec("INSERT INTO clients(id, name, gpu, ip, created, last_seen) VALUES('s','server (idle CPU)','CPU','172.18.0.1',?,?)", &[&(t_now - 8.0 * 86400.0), &t_now]))?;
    let mut t = t_now - 7.0 * 86400.0;
    while t < t_now {
        // browsers: ~7e9 evens/s, a slice every ~4 min
        let rate = rng.uniform(6e9, 8e9);
        let el = evens as f64 / rate;
        q(db.exec(
            "INSERT INTO leases(slice_idx, client, ip, leased, heartbeat, status, done_evens, elapsed, speed, finished)
             VALUES(0,'b','1.2.3.4',?,?,'done',?,?,?,?)",
            &[&(t - el), &t, &evens.to_string(), &el, &rate, &t],
        ))?;
        t += rng.uniform(120.0, 360.0);
    }
    let versions = [json!({"commit": "aaa1111", "subject": "older engine"}), json!({"commit": "f3959ff", "subject": "Merge pull request #123"}),
                    json!({"commit": "263ea29", "subject": "Merge pull request #124"})];
    let (mut t, mut k) = (t_now - 7.0 * 86400.0, 0usize);
    while t < t_now {
        // the server CPU: ~5e7 evens/s, a slice every ~2.7 h
        let mut v = versions[(3 * k / 62).min(2)].clone();
        v["engine"] = "cpu_worker segment 3e9".into();
        let rate = rng.uniform(4.5e7, 5.5e7);
        let el = evens as f64 / rate;
        q(db.exec(
            "INSERT INTO leases(slice_idx, client, ip, leased, heartbeat, status, done_evens, elapsed, speed, finished, version)
             VALUES(1,'s','172.18.0.1',?,?,'done',?,?,?,?,?)",
            &[&(t - el), &t, &evens.to_string(), &el, &rate, &t, &v.to_string()],
        ))?;
        t += el;
        k += 1;
    }
    q(db.exec(
        "INSERT INTO leases(slice_idx, client, ip, leased, heartbeat, status, done_evens) VALUES(2,'s','172.18.0.1',?,?,'active',?)",
        &[&(t_now - 3600.0), &t_now, &(evens / 3).to_string()],
    ))?;
    db.batch("COMMIT").map_err(|x| e(&x))?;
    Ok(format!("seeded {path}: {k} server slices"))
}
