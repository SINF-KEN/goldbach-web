//! The coordinator's state in SQLite: slices, leases, results, trust, bans,
//! running totals, and the dashboard snapshot rebuilt off the request path.
//! The schema and every stored value are the same as server.py's, so either
//! server runs on the same database.
use crate::board::board;
use crate::py::{self, fnum, num};
use crate::timing::timed;
use crate::validate::{boundary_counts, validate, validate_gaps};
use crate::*;
use rusqlite::types::Value as SV;
use rusqlite::{Connection, ToSql};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub type R<T> = Result<T, rusqlite::Error>;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS clients (
  id TEXT PRIMARY KEY, name TEXT, gpu TEXT, ip TEXT, created REAL, last_seen REAL,
  slices INTEGER DEFAULT 0, evens TEXT DEFAULT '0');
CREATE TABLE IF NOT EXISTS slices (
  idx INTEGER PRIMARY KEY, start TEXT, lim TEXT, state TEXT DEFAULT 'open',
  hash TEXT, summary TEXT, verified_at REAL);
CREATE TABLE IF NOT EXISTS leases (
  id INTEGER PRIMARY KEY AUTOINCREMENT, slice_idx INTEGER, client TEXT, ip TEXT,
  leased REAL, heartbeat REAL, status TEXT DEFAULT 'active', done_evens TEXT DEFAULT '0',
  hash TEXT, elapsed REAL, speed REAL);
CREATE INDEX IF NOT EXISTS leases_slice ON leases(slice_idx);
CREATE INDEX IF NOT EXISTS leases_status ON leases(status);
CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT);
CREATE TABLE IF NOT EXISTS results (slice_idx INTEGER, hash TEXT, canon TEXT, PRIMARY KEY (slice_idx, hash));
CREATE TABLE IF NOT EXISTS errors (id INTEGER PRIMARY KEY AUTOINCREMENT, t REAL, client TEXT, lease INTEGER, msg TEXT);
CREATE TABLE IF NOT EXISTS bans (net TEXT PRIMARY KEY, t REAL, why TEXT);
CREATE TABLE IF NOT EXISTS speed_samples (
  bucket INTEGER PRIMARY KEY, evens TEXT NOT NULL, seconds REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS speed_history (
  resolution INTEGER NOT NULL, bucket INTEGER NOT NULL,
  evens TEXT NOT NULL, seconds REAL NOT NULL,
  PRIMARY KEY (resolution, bucket)
);
";

const MIGRATIONS: &[&str] = &[
    "ALTER TABLE clients ADD COLUMN agree INTEGER DEFAULT 0",
    "ALTER TABLE slices ADD COLUMN single TEXT",
    "ALTER TABLE slices ADD COLUMN spot INTEGER DEFAULT 0",
    "ALTER TABLE clients ADD COLUMN banned INTEGER DEFAULT 0",
    "ALTER TABLE leases ADD COLUMN net TEXT",
    "ALTER TABLE leases ADD COLUMN finished REAL",
    "ALTER TABLE slices ADD COLUMN audit TEXT",
    "ALTER TABLE slices ADD COLUMN audit_at REAL",
    "ALTER TABLE leases ADD COLUMN version TEXT",
    "ALTER TABLE leases ADD COLUMN gaps TEXT",
    "ALTER TABLE slices ADD COLUMN gaps TEXT",
    "ALTER TABLE slices ADD COLUMN gaps_state TEXT",
];

pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

// ---- dynamically typed SQLite values, read the way Python sees them ---------
pub fn vi(v: &SV) -> i128 {
    match v {
        SV::Integer(i) => *i as i128,
        SV::Real(f) => *f as i128,
        SV::Text(s) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

pub fn vf(v: &SV) -> f64 {
    match v {
        SV::Integer(i) => *i as f64,
        SV::Real(f) => *f,
        SV::Text(s) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

pub fn vs(v: &SV) -> Option<String> {
    match v {
        SV::Text(s) => Some(s.clone()),
        SV::Integer(i) => Some(i.to_string()),
        SV::Real(f) => Some(py::float_repr(*f)),
        _ => None,
    }
}

/// A SQLite value as the JSON Python's json.dumps would write for it.
pub fn vj(v: &SV) -> Value {
    match v {
        SV::Null => Value::Null,
        SV::Integer(i) => Value::from(*i),
        SV::Real(f) => fnum(*f),
        SV::Text(s) => Value::String(s.clone()),
        SV::Blob(_) => Value::Null,
    }
}

pub fn net_of(ip: &str) -> String {
    let ip = ip.trim_matches(&[' ', '\t', '\n', '\r', '\x0b', '\x0c'][..]);
    if ip.contains(':') {
        let head = ip.split('%').next().unwrap_or("");
        let mut parts: Vec<&str> = head.split(':').collect();
        parts.extend(std::iter::repeat_n("0", 8));
        let p: Vec<String> = parts[..3].iter().map(|p| if p.is_empty() { "0".into() } else { p.to_lowercase() }).collect();
        return format!("v6:{}", p.join(":"));
    }
    let p: Vec<&str> = ip.split('.').take(3).collect();
    format!("v4:{}", p.join("."))
}

pub fn slice_bounds(idx: i128) -> (i128, i128) {
    let lim = RANGE_HI - idx * *SLICE;
    (lim - *SLICE + 2, lim)
}

pub struct Db {
    conn: Connection,
    dirty: Arc<AtomicBool>,
}

type Row = Vec<SV>;

impl Db {
    pub fn exec(&self, sql: &str, p: &[&dyn ToSql]) -> R<usize> {
        let t = Instant::now();
        let r = self.conn.prepare_cached(sql).and_then(|mut s| s.execute(p));
        timed("sql", t.elapsed().as_secs_f64());
        r
    }

    pub fn insert(&self, sql: &str, p: &[&dyn ToSql]) -> R<i64> {
        self.exec(sql, p)?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn batch(&self, sql: &str) -> R<()> {
        let t = Instant::now();
        let r = self.conn.execute_batch(sql);
        timed("sql", t.elapsed().as_secs_f64());
        r
    }

    pub fn q(&self, sql: &str, p: &[&dyn ToSql]) -> R<Vec<Row>> {
        let t = Instant::now();
        let r = (|| {
            let mut st = self.conn.prepare_cached(sql)?;
            let n = st.column_count();
            let rows = st.query_map(p, |r| (0..n).map(|i| r.get::<_, SV>(i)).collect::<R<Row>>())?;
            rows.collect::<R<Vec<Row>>>()
        })();
        timed("sql", t.elapsed().as_secs_f64());
        r
    }

    fn q1(&self, sql: &str, p: &[&dyn ToSql]) -> R<Option<Row>> {
        Ok(self.q(sql, p)?.into_iter().next())
    }

    /// The first column of the first row (NULL when there is none).
    fn val(&self, sql: &str, p: &[&dyn ToSql]) -> R<SV> {
        Ok(self.q1(sql, p)?.map(|mut r| r.swap_remove(0)).unwrap_or(SV::Null))
    }

    pub fn meta(&self, k: &str) -> R<Option<String>> {
        Ok(vs(&self.val("SELECT v FROM meta WHERE k=?", &[&k])?))
    }

    pub fn meta_json(&self, k: &str, default: &str) -> R<Value> {
        let s = self.meta(k)?.unwrap_or_else(|| default.to_string());
        Ok(serde_json::from_str(&s).unwrap_or(Value::Null))
    }

    pub fn set_meta(&self, k: &str, v: &str) -> R<()> {
        self.exec("INSERT INTO meta(k, v) VALUES(?, ?) ON CONFLICT(k) DO UPDATE SET v=excluded.v", &[&k, &v])?;
        Ok(())
    }

    pub fn invalidate(&self) {
        self.dirty.store(true, Ordering::SeqCst);
    }

    /// f in one transaction (one commit, one fsync; all or nothing), or in the
    /// caller's when one is open.
    pub fn tx<T>(&self, f: impl FnOnce() -> R<T>) -> R<T> {
        if !self.conn.is_autocommit() {
            return f();
        }
        self.batch("BEGIN")?;
        match f() {
            Ok(v) => {
                self.batch("COMMIT")?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.batch("ROLLBACK");
                Err(e)
            }
        }
    }

    // ---- running totals of the verified slices ----------------------------
    pub(crate) fn agg_empty() -> Value {
        json!({"evens": "0", "primes": "0", "twins": "0", "pmax": 0, "hist": {}, "ridges": [], "lowest": null})
    }

    pub fn agg(&self) -> R<Value> {
        if self.meta(AGG_KEY)?.is_none() {
            self.set_meta(AGG_KEY, &Self::agg_empty().to_string())?;
            for r in self.q("SELECT summary FROM slices WHERE state='verified' ORDER BY idx", &[])? {
                let mut d: Value = serde_json::from_str(&vs(&r[0]).unwrap_or_default()).unwrap_or(Value::Null);
                if let Value::Object(o) = &mut d {
                    o.entry("histByP").or_insert(json!([]));
                }
                self.agg_add(&d)?;
            }
        }
        self.meta_json(AGG_KEY, "null")
    }

    fn agg_load(&self) -> R<Map<String, Value>> {
        Ok(match self.meta(AGG_KEY)?.and_then(|s| serde_json::from_str(&s).ok()) {
            Some(Value::Object(o)) => o,
            _ => Self::agg_empty().as_object().unwrap().clone(),
        })
    }

    fn agg_add(&self, d: &Value) -> R<()> {
        let mut a = self.agg_load()?;
        let g = |m: &Map<String, Value>, k: &str| m.get(k).map(|v| py::int(v).unwrap_or(0)).unwrap_or(0);
        let gd = |k: &str| d.get(k).map(|v| py::int(v).unwrap_or(0)).unwrap_or(0);
        let (bp, bt) = boundary_counts(gd("limit"));
        a.insert("evens".into(), Value::String((g(&a, "evens") + gd("evens")).to_string()));
        a.insert("primes".into(), Value::String((g(&a, "primes") + gd("primes") + bp).to_string()));
        a.insert("twins".into(), Value::String((g(&a, "twins") + gd("twins") + bt).to_string()));
        a.insert("pmax".into(), num(g(&a, "pmax").max(gd("pmax"))));
        let hist = a.entry("hist").or_insert(json!({})).as_object_mut().unwrap();
        for e in d.get("histByP").and_then(Value::as_array).into_iter().flatten() {
            let (p, v) = (py::int(&e[0]).unwrap_or(0), py::int(&e[1]).unwrap_or(0));
            let cur = hist.get(&p.to_string()).map(|x| py::int(x).unwrap_or(0)).unwrap_or(0);
            hist.insert(p.to_string(), num(cur + v));
        }
        let mut ridges: Vec<Value> = a.get("ridges").and_then(Value::as_array).cloned().unwrap_or_default();
        for r in d.get("ridges").and_then(Value::as_array).into_iter().flatten() {
            ridges.push(json!({"n": r["n"], "p": r["p"]}));
        }
        ridges.sort_by_key(|r| (-py::int(&r["p"]).unwrap_or(0), -py::int(&r["n"]).unwrap_or(0)));
        ridges.truncate(100);
        a.insert("ridges".into(), Value::Array(ridges));
        let start = d.get("start").cloned().unwrap_or(Value::Null);
        let lowest = a.get("lowest").cloned().unwrap_or(Value::Null);
        if lowest.is_null() || py::int(&start).unwrap_or(0) < py::int(&lowest).unwrap_or(0) {
            a.insert("lowest".into(), start);
        }
        self.set_meta(AGG_KEY, &Value::Object(a).to_string())
    }

    fn agg_sub(&self, d: &Value) -> R<()> {
        let mut a = self.agg_load()?;
        let g = |m: &Map<String, Value>, k: &str| m.get(k).map(|v| py::int(v).unwrap_or(0)).unwrap_or(0);
        let gd = |k: &str| d.get(k).map(|v| py::int(v).unwrap_or(0)).unwrap_or(0);
        let (bp, bt) = boundary_counts(gd("limit"));
        a.insert("evens".into(), Value::String((g(&a, "evens") - gd("evens")).to_string()));
        a.insert("primes".into(), Value::String((g(&a, "primes") - gd("primes") - bp).to_string()));
        a.insert("twins".into(), Value::String((g(&a, "twins") - gd("twins") - bt).to_string()));
        let hist = a.entry("hist").or_insert(json!({})).as_object_mut().unwrap();
        for e in d.get("histByP").and_then(Value::as_array).into_iter().flatten() {
            let (p, v) = (py::int(&e[0]).unwrap_or(0), py::int(&e[1]).unwrap_or(0));
            let cur = hist.get(&p.to_string()).map(|x| py::int(x).unwrap_or(0)).unwrap_or(0);
            hist.insert(p.to_string(), num(cur - v));
        }
        let (lo, hi) = (gd("start"), gd("limit"));
        let ridges: Vec<Value> = a
            .get("ridges")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|r| {
                let n = py::int(&r["n"]).unwrap_or(0);
                !(lo <= n && n <= hi)
            })
            .cloned()
            .collect();
        a.insert("ridges".into(), Value::Array(ridges));
        self.set_meta(AGG_KEY, &Value::Object(a).to_string())
    }

    // ---- audits ----------------------------------------------------------
    pub fn audit_next(&self) -> R<Option<Value>> {
        let r = self.q1("SELECT idx FROM slices WHERE state='verified' AND audit IS NULL ORDER BY verified_at LIMIT 1", &[])?;
        Ok(r.map(|r| {
            let idx = vi(&r[0]);
            let (s, l) = slice_bounds(idx);
            json!({"slice": num(idx), "start": s.to_string(), "limit": l.to_string()})
        }))
    }

    pub fn audit_result(&self, idx: i128, res: &Value) -> R<Value> {
        let i = idx as i64;
        let Some(row) = self.q1("SELECT state, hash FROM slices WHERE idx=?", &[&i])? else {
            return Ok(json!({"status": "unknown slice"}));
        };
        let (start, lim) = slice_bounds(idx);
        let canon = match validate(res, start, lim) {
            Ok(c) => c,
            Err(why) => return Ok(json!({"status": "audit result invalid", "why": why})),
        };
        let h = py::sha256_hex(canon.as_bytes());
        let t = now();
        if vs(&row[0]).as_deref() != Some("verified") {
            return Ok(json!({"status": "slice no longer verified"}));
        }
        let vh = vs(&row[1]);
        if vh.as_deref() == Some(h.as_str()) {
            self.exec("UPDATE slices SET audit='pass', audit_at=? WHERE idx=?", &[&t, &i])?;
            self.exec("DELETE FROM results WHERE slice_idx=?", &[&i])?;
            return Ok(json!({"status": "pass"}));
        }
        let cheats: Vec<String> = self
            .q("SELECT client FROM leases WHERE slice_idx=? AND status='done' AND hash=?", &[&i, &vh])?
            .iter()
            .filter_map(|r| vs(&r[0]))
            .collect();
        let alert = json!({"slice": i, "client": cheats.join(","), "result": "a wrong result caught by the audit", "time": t});
        self.set_meta("alert", &alert.to_string())?;
        self.ban(&cheats, &format!("audit of slice {idx} failed"))?;
        self.exec("UPDATE slices SET audit='fail', audit_at=? WHERE idx=?", &[&t, &i])?;
        Ok(json!({"status": "fail", "banned": cheats.len()}))
    }

    pub fn floor(&self) -> R<i128> {
        let d = RANGE_LO + 100_000_000_000_000_000;
        Ok(self.meta("floor")?.map(|s| s.trim().parse().unwrap_or(d)).unwrap_or(d))
    }

    // ---- clients -----------------------------------------------------------
    pub fn register(&self, name: &str, gpu: &str, ip: &str) -> R<String> {
        let b: [u8; 16] = rand::random();
        let cid: String = b.iter().map(|x| format!("{x:02x}")).collect();
        let t = now();
        self.exec(
            "INSERT INTO clients(id, name, gpu, ip, created, last_seen) VALUES(?,?,?,?,?,?)",
            &[&cid, &py::chars(name, 40), &py::chars(gpu, 120), &ip, &t, &t],
        )?;
        Ok(cid)
    }

    pub fn client(&self, cid: &str) -> R<bool> {
        Ok(self.q1("SELECT id, name, gpu FROM clients WHERE id=? AND COALESCE(banned, 0)=0", &[&cid])?.is_some())
    }

    fn trusted(&self, cid: &str) -> R<bool> {
        Ok(match self.q1("SELECT agree, created, COALESCE(banned, 0) FROM clients WHERE id=?", &[&cid])? {
            Some(r) => vi(&r[2]) == 0 && vi(&r[0]) >= TRUST_AFTER && now() - vf(&r[1]) >= TRUST_MIN_AGE,
            None => false,
        })
    }

    pub fn net_banned(&self, ip: &str) -> R<bool> {
        Ok(self.q1("SELECT 1 FROM bans WHERE net=?", &[&net_of(ip)])?.is_some())
    }

    fn ban(&self, cids: &[String], why: &str) -> R<()> {
        let t = now();
        let mut seen = std::collections::HashSet::new();
        for cid in cids.iter().filter(|c| seen.insert(c.as_str())) {
            self.exec("UPDATE clients SET banned=1 WHERE id=?", &[cid])?;
            for r in self.q("SELECT DISTINCT net FROM leases WHERE client=? AND net IS NOT NULL", &[cid])? {
                self.exec("INSERT OR IGNORE INTO bans(net, t, why) VALUES(?,?,?)", &[&r[0], &t, &py::chars(why, 200)])?;
            }
            for r in self.q("SELECT DISTINCT slice_idx FROM leases WHERE client=? AND status='done'", &[cid])? {
                self.reopen(vi(&r[0]) as i64)?;
            }
            self.exec("UPDATE leases SET status='banned' WHERE client=? AND status IN ('active','done')", &[cid])?;
        }
        self.invalidate();
        Ok(())
    }

    fn settle_gaps(&self, idx: i64) -> R<()> {
        let Some(row) = self.q1("SELECT state, hash FROM slices WHERE idx=?", &[&idx])? else { return Ok(()) };
        if vs(&row[0]).as_deref() != Some("verified") {
            return Ok(());
        }
        let got: Vec<String> = self
            .q(
                "SELECT gaps FROM leases WHERE slice_idx=? AND status='done' AND hash=? AND gaps IS NOT NULL",
                &[&idx, &row[1]],
            )?
            .iter()
            .filter_map(|r| vs(&r[0]))
            .collect();
        let distinct: std::collections::HashSet<&String> = got.iter().collect();
        if distinct.len() > 1 {
            self.exec("UPDATE slices SET gaps=NULL, gaps_state='disputed' WHERE idx=?", &[&idx])?;
            let alert = json!({"slice": idx, "client": "-", "result": "devices disagree on the prime gaps", "time": now()});
            self.set_meta("alert", &alert.to_string())?;
        } else if let Some(g) = got.first() {
            let st = if got.len() > 1 { "double" } else { "single" };
            self.exec("UPDATE slices SET gaps=?, gaps_state=? WHERE idx=?", &[g, &st, &idx])?;
        } else {
            self.exec("UPDATE slices SET gaps=NULL, gaps_state='missing' WHERE idx=?", &[&idx])?;
        }
        Ok(())
    }

    /// The browsers' prime gaps over their verified slices (exact sums).
    pub fn web_gaps(&self) -> R<Value> {
        let rows = self.q("SELECT gaps, gaps_state, start, lim FROM slices WHERE state='verified'", &[])?;
        let mut states = Map::new();
        let mut hist: HashMap<i128, i128> = HashMap::new();
        let mut tail: HashMap<i128, (i128, String)> = HashMap::new();
        let (mut d, mut n, mut ev) = ([0i128; 4], 0i128, 0i128);
        let mut gmax: Option<(i128, i128)> = None;
        let (mut rlo, mut rhi): (Option<i128>, Option<i128>) = (None, None);
        let int = |v: &Value| py::int(v).unwrap_or(0);
        for r in &rows {
            let st = vs(&r[1]).filter(|s| !s.is_empty()).unwrap_or_else(|| "missing".into());
            let c = states.get(&st).map(int).unwrap_or(0);
            states.insert(st.clone(), num(c + 1));
            let g = vs(&r[0]).unwrap_or_default();
            if g.is_empty() || !(st == "single" || st == "double") {
                continue;
            }
            let Ok(o) = serde_json::from_str::<Value>(&g) else { continue };
            let (a, b) = (vi(&r[2]), vi(&r[3]));
            n += int(&o["n"]);
            ev += (b - a) / 2 + 1;
            rlo = Some(match rlo { Some(x) if x != 0 => x.min(a), _ => a });
            rhi = Some(match rhi { Some(x) if x != 0 => x.max(b), _ => b });
            for k in 0..4 {
                d[k] += int(&o["d"][k]);
            }
            for e in o["hist"].as_array().into_iter().flatten() {
                *hist.entry(int(&e[0])).or_insert(0) += int(&e[1]);
            }
            for e in o["tail"].as_array().into_iter().flatten() {
                let lo = py::str_of(&e[2]);
                let t = tail.entry(int(&e[0])).or_insert((0, lo.clone()));
                t.0 += int(&e[1]);
                if lo.trim().parse::<i128>().unwrap_or(0) < t.1.trim().parse::<i128>().unwrap_or(0) {
                    t.1 = lo;
                }
            }
            let m = (int(&o["max"][0]), -int(&o["max"][1]));
            if m.0 != 0 && gmax.is_none_or(|g| m > g) {
                gmax = Some(m);
            }
        }
        let mut hist: Vec<(i128, i128)> = hist.into_iter().collect();
        hist.sort();
        let mut tail: Vec<(i128, i128, String)> = tail.into_iter().map(|(x, (c, lo))| (x, c, lo)).collect();
        tail.sort();
        let range = match (rlo, rhi) {
            (Some(lo), Some(hi)) if lo != 0 => json!([lo.to_string(), hi.to_string()]),
            _ => Value::Null,
        };
        Ok(json!({
            "slices": rows.len(), "states": states, "evens": ev.to_string(), "n": n.to_string(),
            "d": d.iter().map(|x| x.to_string()).collect::<Vec<_>>(),
            "hist": hist.iter().map(|(x, c)| json!([num(*x), num(*c)])).collect::<Vec<_>>(),
            "tail": tail.iter().map(|(x, c, lo)| json!([num(*x), num(*c), lo])).collect::<Vec<_>>(),
            "max": gmax.map(|g| json!([num(g.0), (-g.1).to_string()])).unwrap_or(Value::Null),
            "range": range,
        }))
    }

    fn undo_verified(&self, idx: i64) -> R<bool> {
        let h = self.val("SELECT hash FROM slices WHERE idx=?", &[&idx])?;
        if let Some(r) = self.q1("SELECT canon FROM results WHERE slice_idx=? AND hash=?", &[&idx, &h])? {
            let d: Value = serde_json::from_str(&vs(&r[0]).unwrap_or_default()).unwrap_or(Value::Null);
            self.agg_sub(&d)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn reopen(&self, idx: i64) -> R<()> {
        let Some(r) = self.q1("SELECT state, single, audit FROM slices WHERE idx=?", &[&idx])? else { return Ok(()) };
        if vs(&r[0]).as_deref() == Some("verified") {
            if vs(&r[2]).as_deref() == Some("pass") {
                return Ok(());
            }
            if !self.undo_verified(idx)? && matches!(r[1], SV::Null) {
                return Ok(());
            }
        }
        self.exec(
            "UPDATE slices SET state='open', hash=NULL, summary=NULL, verified_at=NULL, audit=NULL, single=NULL, spot=0 WHERE idx=?",
            &[&idx],
        )?;
        Ok(())
    }

    // ---- leases ------------------------------------------------------------
    pub fn expire(&self) -> R<()> {
        let t = now();
        self.exec("UPDATE leases SET status='expired' WHERE status='active' AND heartbeat < ?", &[&(t - LEASE_S)])?;
        self.exec(
            "DELETE FROM results WHERE slice_idx IN (SELECT idx FROM slices WHERE state='verified'
                           AND verified_at < ?)",
            &[&(now() - KEEP_FULL_S)],
        )?;
        Ok(())
    }

    pub fn lease(&self, cid: &str, ip: &str, ahead: bool) -> R<Option<Value>> {
        if self.net_banned(ip)? {
            return Ok(None);
        }
        let net = net_of(ip);
        self.expire()?;
        let t = now();
        self.exec("UPDATE clients SET last_seen=?, ip=? WHERE id=?", &[&t, &ip, &cid])?;
        if ahead {
            self.exec(
                "UPDATE leases SET status='released' WHERE client=? AND status='active' AND id <
                                   (SELECT MAX(id) FROM leases WHERE client=? AND status='active')",
                &[&cid, &cid],
            )?;
        } else {
            self.exec("UPDATE leases SET status='released' WHERE client=? AND status='active'", &[&cid])?;
        }
        let mut rows = self.q(
            "
                SELECT s.idx,
                  (SELECT COUNT(*) FROM leases l WHERE l.slice_idx=s.idx AND l.status IN ('active','done')) AS n,
                  (SELECT COUNT(*) FROM leases l WHERE l.slice_idx=s.idx AND (l.client=? OR l.net=?)
                                                       AND l.status IN ('active','done')) AS mine
                FROM slices s WHERE s.state IN ('open','disputed') OR (s.state='verified' AND s.spot=1)
                ORDER BY s.idx",
            &[&cid, &net],
        )?;
        if LOCAL_IPS.contains(ip) {
            rows = self.server_order(rows)?;
        }
        let mut pick: Option<i64> = None;
        for r in &rows {
            let idx = vi(&r[0]) as i64;
            let disputed = vs(&self.val("SELECT state FROM slices WHERE idx=?", &[&idx])?).as_deref() == Some("disputed");
            let need = REPLICAS + disputed as i128;
            if vi(&r[2]) == 0 && vi(&r[1]) < need {
                pick = Some(idx);
                break;
            }
        }
        let pick = match pick {
            Some(p) => p,
            None => {
                let m = self.val("SELECT MAX(idx) FROM slices", &[])?;
                let nxt = if matches!(m, SV::Null) { 0 } else { vi(&m) + 1 };
                let (start, lim) = slice_bounds(nxt);
                if start < self.floor()? {
                    return Ok(None);
                }
                let n = nxt as i64;
                self.exec("INSERT INTO slices(idx, start, lim) VALUES(?,?,?)", &[&n, &start.to_string(), &lim.to_string()])?;
                n
            }
        };
        let id = self.insert(
            "INSERT INTO leases(slice_idx, client, ip, net, leased, heartbeat) VALUES(?,?,?,?,?,?)",
            &[&pick, &cid, &ip, &net, &t, &t],
        )?;
        let (start, lim) = slice_bounds(pick as i128);
        Ok(Some(json!({"lease": id, "slice": pick, "start": start.to_string(), "limit": lim.to_string()})))
    }

    /// The server's own verifier takes the work that unblocks the most.
    fn server_order(&self, mut rows: Vec<Row>) -> R<Vec<Row>> {
        if rows.is_empty() {
            return Ok(rows);
        }
        let ids: Vec<String> = rows.iter().map(|r| (vi(&r[0]) as i64).to_string()).collect();
        let sql = format!(
            "
            SELECT s.idx, s.state, s.spot, MAX(COALESCE(c.agree, 0))
            FROM slices s LEFT JOIN leases l ON l.slice_idx = s.idx AND l.status = 'done'
                          LEFT JOIN clients c ON c.id = l.client
            WHERE s.idx IN ({}) GROUP BY s.idx",
            ids.join(",")
        );
        let mut info: HashMap<i128, (Option<String>, i128, i128)> = HashMap::new();
        for r in self.q(&sql, &[])? {
            info.insert(vi(&r[0]), (vs(&r[1]), vi(&r[2]), vi(&r[3])));
        }
        rows.sort_by_cached_key(|r| {
            let idx = vi(&r[0]);
            let (state, spot, agree) = info.get(&idx).cloned().unwrap_or((Some("open".into()), 0, 0));
            let rank = match state.as_deref() {
                Some("disputed") => 0,
                Some("verified") if spot != 0 => 1,
                _ => 2,
            };
            (rank, -agree.min(TRUST_AFTER), idx)
        });
        Ok(rows)
    }

    pub fn heartbeat(&self, cid: &str, lease: i64, done: i128) -> R<bool> {
        match self.q1("SELECT status FROM leases WHERE id=? AND client=?", &[&lease, &cid])? {
            Some(r) if vs(&r[0]).as_deref() == Some("active") => {}
            _ => return Ok(false),
        }
        let t = now();
        self.tx(|| {
            self.exec("UPDATE leases SET heartbeat=?, done_evens=? WHERE id=?", &[&t, &done.to_string(), &lease])?;
            self.exec("UPDATE clients SET last_seen=? WHERE id=?", &[&t, &cid])
        })?;
        Ok(true)
    }

    pub fn release(&self, cid: &str, lease: i64) -> R<()> {
        self.exec("UPDATE leases SET status='released' WHERE id=? AND client=? AND status='active'", &[&lease, &cid])?;
        Ok(())
    }

    fn state_of(&self, idx: i64) -> R<Option<String>> {
        Ok(vs(&self.val("SELECT state FROM slices WHERE idx=?", &[&idx])?))
    }

    pub fn result(&self, cid: &str, lease: i64, res: &Value, elapsed: f64, version: Option<&Value>) -> R<Value> {
        let r = match self.q1("SELECT slice_idx, status, ip FROM leases WHERE id=? AND client=?", &[&lease, &cid])? {
            Some(r) if vs(&r[1]).as_deref() == Some("active") => r,
            _ => return Ok(json!({"status": "rejected", "why": "no active lease"})),
        };
        let idx = vi(&r[0]) as i64;
        let (start, lim) = slice_bounds(idx as i128);
        let checked = validate(res, start, lim).and_then(|c| validate_gaps(res, start, lim).map(|g| (c, g)));
        let (canon, gaps) = match checked {
            Ok(x) => x,
            Err(why) => {
                self.exec("UPDATE leases SET status='invalid' WHERE id=?", &[&lease])?;
                return Ok(json!({"status": "rejected", "why": why}));
            }
        };
        let h = py::sha256_hex(canon.as_bytes());
        self.exec("UPDATE leases SET gaps=? WHERE id=?", &[&gaps, &lease])?;
        let evens = (lim - start) / 2 + 1;
        let speed = if elapsed > 0.0 { evens as f64 / elapsed } else { 0.0 };
        self.exec(
            "UPDATE leases SET status='done', done_evens=?, hash=?, elapsed=?, speed=?, heartbeat=?, finished=? WHERE id=?",
            &[&evens.to_string(), &h, &elapsed, &speed, &now(), &now(), &lease],
        )?;
        // the server's own CPU worker names the deployed code it ran
        if let (Some(ip), Some(Value::Object(v))) = (vs(&r[2]), version) {
            if LOCAL_IPS.contains(ip.as_str()) {
                let mut m = Map::new();
                for k in ["commit", "date", "subject", "engine"] {
                    m.insert(k.into(), Value::String(py::chars(&py::str_of(v.get(k).unwrap_or(&json!(""))), 100)));
                }
                self.exec("UPDATE leases SET version=? WHERE id=?", &[&Value::Object(m).to_string(), &lease])?;
            }
        }
        self.exec(
            "UPDATE clients SET slices=slices+1, evens=CAST(CAST(evens AS INTEGER)+? AS TEXT) WHERE id=?",
            &[&(evens as i64), &cid],
        )?;
        // a result is confirmed only by devices on DIFFERENT networks
        let hashes: Vec<(SV, i128)> = self
            .q(
                "SELECT hash, COUNT(DISTINCT COALESCE(net, client)) FROM leases WHERE slice_idx=? AND status='done' GROUP BY hash",
                &[&idx],
            )?
            .into_iter()
            .map(|mut r| {
                let c = vi(&r[1]);
                (r.swap_remove(0), c)
            })
            .collect();
        let mut best = &hashes[0];
        for x in &hashes[1..] {
            if x.1 > best.1 {
                best = x;
            }
        }
        self.exec("INSERT OR IGNORE INTO results(slice_idx, hash, canon) VALUES(?,?,?)", &[&idx, &h, &canon])?;
        let already = self.state_of(idx)?.as_deref() == Some("verified");
        let spot = vi(&self.val("SELECT spot FROM slices WHERE idx=?", &[&idx])?) != 0;
        let small = |full: &Value| {
            let mut m = Map::new();
            for k in ["start", "limit", "evens", "primes", "twins", "pmax", "ridges"] {
                m.insert(k.into(), full.get(k).cloned().unwrap_or(Value::Null));
            }
            Value::Object(m).to_string()
        };
        let canon_v: Value = serde_json::from_str(&canon).unwrap_or(Value::Null);
        if already && spot {
            let row = self.q1("SELECT hash, single FROM slices WHERE idx=?", &[&idx])?.unwrap_or_default();
            if vs(&row[0]).as_deref() == Some(h.as_str()) {
                self.exec("UPDATE slices SET spot=0, single=NULL WHERE idx=?", &[&idx])?;
                self.settle_gaps(idx)?;
                self.exec("UPDATE clients SET agree=agree+1 WHERE id IN (?, ?)", &[&cid, &row[1]])?;
                self.invalidate();
                return Ok(json!({"status": "verified (spot check confirmed)", "hash": &h[..16]}));
            }
            self.undo_verified(idx)?;
            self.exec("UPDATE slices SET state='disputed', spot=0, single=NULL WHERE idx=?", &[&idx])?;
            self.invalidate();
            return Ok(json!({"status": "disputed", "hash": &h[..16]}));
        }
        if !already
            && best.1 < REPLICAS
            && hashes.len() == 1
            && self.trusted(cid)?
            && self.state_of(idx)?.as_deref() == Some("open")
        {
            let sp: i64 = (rand::random::<f64>() < f64::from_bits(SPOT_RATE.load(Ordering::SeqCst))) as i64;
            self.exec(
                "UPDATE slices SET state='verified', hash=?, summary=?, verified_at=?, single=?, spot=? WHERE idx=?",
                &[&h, &small(&canon_v), &now(), &cid, &sp, &idx],
            )?;
            self.agg_add(&canon_v)?;
            self.settle_gaps(idx)?;
            self.invalidate();
            let status = format!("verified (trusted device){}", if sp == 1 { ", spot check queued" } else { "" });
            return Ok(json!({"status": status, "hash": &h[..16]}));
        }
        let state: String;
        if best.1 >= REPLICAS && !already {
            let full_s = vs(&self.val("SELECT canon FROM results WHERE slice_idx=? AND hash=?", &[&idx, &best.0])?);
            let full: Value = serde_json::from_str(&full_s.unwrap_or_default()).unwrap_or(Value::Null);
            self.exec(
                "UPDATE slices SET state='verified', hash=?, summary=?, verified_at=? WHERE idx=?",
                &[&best.0, &small(&full), &now(), &idx],
            )?;
            self.agg_add(&full)?;
            self.exec("DELETE FROM results WHERE slice_idx=? AND hash<>?", &[&idx, &best.0])?;
            let who = |op: &str| -> R<Vec<String>> {
                Ok(self
                    .q(
                        &format!("SELECT DISTINCT client FROM leases WHERE slice_idx=? AND status='done' AND hash{op}?"),
                        &[&idx, &best.0],
                    )?
                    .iter()
                    .filter_map(|r| vs(&r[0]))
                    .collect())
            };
            for w in who("=")? {
                self.exec("UPDATE clients SET agree=agree+1 WHERE id=?", &[&w])?;
            }
            let losers = who("<>")?;
            if !losers.is_empty() {
                self.ban(&losers, &format!("lost the dispute on slice {idx}"))?;
            }
            state = "verified".into();
        } else if already {
            let vh = vs(&self.val("SELECT hash FROM slices WHERE idx=?", &[&idx])?);
            if vh.as_deref() != Some(h.as_str()) {
                self.ban(&[cid.to_string()], &format!("result of slice {idx} contradicts the verified one"))?;
                state = "rejected: contradicts the verified result".into();
            } else {
                state = "verified".into();
            }
        } else if hashes.len() > 1 {
            self.exec("UPDATE slices SET state='disputed' WHERE idx=?", &[&idx])?;
            state = "disputed".into();
        } else {
            state = "waiting for a second device".into();
        }
        if !res.get("pass").is_some_and(py::truthy) {
            let failure = res.get("failure").cloned().unwrap_or(json!("FAIL"));
            let alert = json!({"slice": idx, "client": cid, "result": failure, "time": now()});
            self.set_meta("alert", &alert.to_string())?;
        }
        self.settle_gaps(idx)?;
        self.invalidate();
        Ok(json!({"status": state, "hash": &h[..16]}))
    }

    pub fn report(&self, cid: &str, lease: i64, msg: &str) -> R<()> {
        let t = now();
        self.exec("INSERT INTO errors(t, client, lease, msg) VALUES(?,?,?,?)", &[&t, &cid, &lease, &py::chars(msg, 500)])?;
        self.exec("DELETE FROM errors WHERE id <= (SELECT MAX(id) - 200 FROM errors)", &[])?;
        self.exec("UPDATE leases SET status='error' WHERE id=? AND client=? AND status='active'", &[&lease, &cid])?;
        if msg.contains("COUNTEREXAMPLE") {
            let s = self.q1("SELECT slice_idx FROM leases WHERE id=?", &[&lease])?.map(|r| vj(&r[0])).unwrap_or(json!(-1));
            let alert = json!({"slice": s, "client": cid, "result": "a possible counterexample", "time": t});
            self.set_meta("alert", &alert.to_string())?;
            self.invalidate();
        }
        Ok(())
    }

    /// The dashboard statistics (server.py Store.stats, the part under the lock).
    fn stats_obj(&self) -> R<Value> {
        let t = now();
        self.expire()?;
        let a = self.agg()?;
        let int = |v: &Value| py::int(v).unwrap_or(0);
        let native = self.meta_json("native", "{}")?;
        let mut ridges: Vec<Value> = Vec::new();
        for r in a["ridges"].as_array().into_iter().flatten() {
            ridges.push(json!({"n": r["n"], "p": r["p"], "src": "web"}));
        }
        for r in native.get("ridges").and_then(Value::as_array).into_iter().flatten() {
            ridges.push(json!({"n": r["n"], "p": r["p"], "src": "B580"}));
        }
        ridges.sort_by_key(|r| (-int(&r["p"]), -int(&r["n"])));
        let mut seen: Vec<Value> = Vec::new();
        let mut top: Vec<Value> = Vec::new();
        for r in ridges {
            if seen.contains(&r["n"]) {
                continue;
            }
            seen.push(r["n"].clone());
            top.push(r);
            if top.len() == 30 {
                break;
            }
        }
        let mut devices = Vec::new();
        for r in self.q(
            "SELECT c.name, c.gpu, l.slice_idx, l.done_evens, l.leased FROM leases l
                               JOIN clients c ON c.id = l.client WHERE l.status='active' ORDER BY l.leased",
            &[],
        )? {
            let idx = vi(&r[2]);
            let (s, l) = slice_bounds(idx);
            let tot = (l - s) / 2 + 1;
            let el = (t - vf(&r[4])).max(1.0);
            let done = vi(&r[3]);
            devices.push(json!({"name": vj(&r[0]), "gpu": vj(&r[1]), "slice": num(idx),
                                "progress": done as f64 / tot as f64, "rate": done as f64 / el}));
        }
        let mut counts: HashMap<String, i128> = HashMap::new();
        for r in self.q("SELECT state, COUNT(*) FROM slices GROUP BY state", &[])? {
            counts.insert(vs(&r[0]).unwrap_or_default(), vi(&r[1]));
        }
        let count = |k: &str| num(*counts.get(k).unwrap_or(&0));
        let recent: Vec<Value> = self
            .q("SELECT start, lim, verified_at FROM slices WHERE state='verified' ORDER BY verified_at DESC LIMIT 8", &[])?
            .iter()
            .map(|r| json!({"start": vj(&r[0]), "limit": vj(&r[1]), "at": vj(&r[2])}))
            .collect();
        // tiered speed history: 10 s buckets for 1 h, 5 min for 24 h, hourly for 7 d
        let tiers: [(i64, i64); 3] = [(10, 3600), (300, 86400), (3600, 7 * 86400)];
        self.tx(|| {
            self.exec("DELETE FROM speed_samples", &[])?;
            for (res, keep) in tiers {
                let tier_start = (t / res as f64).floor() as i64 * res - keep + res;
                self.exec("DELETE FROM speed_history WHERE resolution=? AND bucket < ?", &[&res, &tier_start])?;
                let rows = self.q(
                    "SELECT CAST(finished / ? AS INTEGER) * ? AS bucket,
                                            SUM(CAST(done_evens AS REAL)) AS evens,
                                            SUM(elapsed) AS seconds
                                     FROM leases
                                     WHERE status='done' AND elapsed > 0 AND finished IS NOT NULL AND finished >= ?
                                     GROUP BY bucket ORDER BY bucket",
                    &[&res, &res, &tier_start],
                )?;
                self.exec("DELETE FROM speed_history WHERE resolution=? AND bucket >= ?", &[&res, &tier_start])?;
                for r in rows {
                    if vf(&r[2]) != 0.0 && !matches!(r[1], SV::Null) {
                        let ev = vf(&r[1]).trunc();
                        let rounded = (py::round(ev / 1_000_000.0) as i128 * 1_000_000).to_string();
                        let secs = py::round_to(vf(&r[2]), 1);
                        self.exec(
                            "INSERT OR REPLACE INTO speed_history(resolution,bucket,evens,seconds) VALUES(?,?,?,?)",
                            &[&res, &r[0], &rounded, &secs],
                        )?;
                    }
                }
            }
            Ok(())
        })?;
        let mut speed_history = Vec::new();
        for (res, _) in tiers {
            for r in self.q("SELECT bucket, evens, seconds FROM speed_history WHERE resolution=? ORDER BY bucket", &[&res])? {
                let secs = vf(&r[2]);
                let rate = if secs != 0.0 { fnum(vi(&r[1]) as f64 / secs) } else { json!(0) };
                speed_history.push(json!({"t": vj(&r[0]), "rate": rate, "resolution": res}));
            }
        }
        // the server's CPU worker alone (it leases from a local address)
        let mut local: Vec<&String> = LOCAL_IPS.iter().collect();
        local.sort();
        let sql = format!(
            "SELECT finished, speed, version FROM leases
                                   WHERE status='done' AND elapsed > 0 AND finished IS NOT NULL
                                     AND ip IN ({})
                                   ORDER BY finished DESC LIMIT ?",
            vec!["?"; local.len()].join(",")
        );
        let mut p: Vec<&dyn ToSql> = local.iter().map(|s| *s as &dyn ToSql).collect();
        p.push(&SERVER_RUNS);
        let mut server_runs: Vec<Value> = self
            .q(&sql, &p)?
            .iter()
            .map(|r| {
                let v = vs(&r[2]).filter(|s| !s.is_empty()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
                json!({"t": vj(&r[0]), "rate": vj(&r[1]), "v": v})
            })
            .collect();
        server_runs.reverse();
        let cnt = |sql: &str, p: &[&dyn ToSql]| -> R<Value> { Ok(vj(&self.val(sql, p)?)) };
        let web = json!({
            "evens": int(&a["evens"]).to_string(), "verified": count("verified"), "open": count("open"),
            "disputed": count("disputed"), "primes": int(&a["primes"]).to_string(), "twins": int(&a["twins"]).to_string(),
            "pmax": a["pmax"], "lowest": a["lowest"], "recent": recent,
        });
        let mut hist: Vec<(i128, Value)> =
            a["hist"].as_object().into_iter().flatten().map(|(p, v)| (p.trim().parse().unwrap_or(0), v.clone())).collect();
        hist.sort_by(|x, y| x.0.cmp(&y.0));
        Ok(json!({
            "time": t, "range": [RANGE_LO.to_string(), RANGE_HI.to_string()], "slice": SLICE.to_string(),
            "floor": self.floor()?.to_string(),
            "web": web,
            "native": native.get("progress").cloned().unwrap_or(json!({})),
            "nativeHist": native.get("tail").cloned().unwrap_or(json!([])),
            "hist": hist.into_iter().map(|(p, v)| json!([num(p), v])).collect::<Vec<_>>(),
            "speedHistory": speed_history,
            "serverRuns": server_runs,
            "ridges": top,
            "devices": devices,
            "clients": cnt("SELECT COUNT(*) FROM clients WHERE last_seen > ?", &[&(t - 86400.0)])?,
            "trusted": cnt("SELECT COUNT(*) FROM clients WHERE agree >= ? AND created <= ? AND COALESCE(banned, 0) = 0",
                           &[&(TRUST_AFTER as i64), &(t - TRUST_MIN_AGE)])?,
            "banned": cnt("SELECT COUNT(*) FROM clients WHERE COALESCE(banned, 0) = 1", &[])?,
            "alert": self.meta_json("alert", "null")?,
        }))
    }
}

/// One consistent dashboard state: the /api/stats body (plain and gzipped), the
/// boards and their hashes.
pub struct Snapshot {
    pub built: f64,
    pub body: Vec<u8>,
    pub body_gz: Vec<u8>,
    pub obj: Value,
    pub boards: [String; 2],
    pub hashes: [String; 2],
}

pub struct Store {
    db: Mutex<Db>,
    snap: RwLock<Arc<Snapshot>>,
    dirty: Arc<AtomicBool>,
}

pub fn gzip(data: &[u8], level: u32) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::with_capacity(data.len() / 3 + 64), flate2::Compression::new(level));
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

impl Store {
    pub fn open(path: &str) -> R<Arc<Store>> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(SCHEMA)?;
        for m in MIGRATIONS {
            let _ = conn.execute_batch(m); // column exists
        }
        conn.execute_batch("CREATE INDEX IF NOT EXISTS leases_finished_status ON leases(finished, status)")?;
        conn.set_prepared_statement_cache_capacity(64);
        let dirty = Arc::new(AtomicBool::new(false));
        let db = Db { conn, dirty: dirty.clone() };
        let snap = Arc::new(Snapshot {
            built: 0.0,
            body: vec![],
            body_gz: vec![],
            obj: Value::Null,
            boards: Default::default(),
            hashes: Default::default(),
        });
        let st = Arc::new(Store { db: Mutex::new(db), snap: RwLock::new(snap), dirty });
        st.rebuild()?;
        Ok(st)
    }

    /// The store lock, booking the wait (Server-Timing "lock").
    pub fn lock(&self) -> MutexGuard<'_, Db> {
        let t = Instant::now();
        let g = self.db.lock().unwrap_or_else(|e| e.into_inner());
        timed("lock", t.elapsed().as_secs_f64());
        g
    }

    /// The store lock and one transaction around f: a request's writes commit
    /// together (server.py autocommits each statement: an fsync each on disk).
    pub fn write<T>(&self, f: impl FnOnce(&Db) -> R<T>) -> R<T> {
        let db = self.lock();
        db.tx(|| f(&db))
    }

    pub fn snap(&self) -> Arc<Snapshot> {
        self.snap.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn rebuild(&self) -> R<()> {
        let obj = {
            let db = self.lock();
            let t = Instant::now();
            let o = db.stats_obj()?;
            timed("rebuild", t.elapsed().as_secs_f64());
            o
        };
        let body = obj.to_string().into_bytes();
        let body_gz = gzip(&body, 6);
        let boards = [board(&obj, false), board(&obj, true)];
        let hashes = [py::sha256_hex(boards[0].as_bytes())[..12].to_string(), py::sha256_hex(boards[1].as_bytes())[..12].to_string()];
        let built = obj["time"].as_f64().unwrap_or(0.0);
        *self.snap.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(Snapshot { built, body, body_gz, obj, boards, hashes });
        Ok(())
    }

    /// Keeps the snapshot fresh in a background thread, so no request waits for it.
    pub fn start_snapshots(self: &Arc<Self>) {
        let st = self.clone();
        std::thread::Builder::new()
            .name("snapshot".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    let age = now() - st.snap().built;
                    if (st.dirty.load(Ordering::SeqCst) && age >= SNAPSHOT_DIRTY_S) || age >= *SNAPSHOT_MAX_S {
                        st.dirty.store(false, Ordering::SeqCst);
                        if let Err(e) = st.rebuild() {
                            eprintln!("snapshot: {e:?}");
                        }
                    }
                }
            })
            .expect("snapshot thread");
    }
}

/// What an open page needs, and only what changed since its last poll: the
/// board (unless its hash bh is current), the speed buckets of the chart tiers
/// it holds that a slice finished after `since` can have touched (all of a tier
/// in `full`), and the server CPU's per-version summary (unless `runs` is current).
pub fn live_of(snap: &Snapshot, narrow: bool, since: f64, runs: Option<f64>, bh: Option<&str>, tiers: &[i64], full: &[i64]) -> Map<String, Value> {
    let i = narrow as usize;
    let o = &snap.obj;
    let mut out = Map::new();
    out.insert("t".into(), o["time"].clone());
    out.insert("bh".into(), Value::String(snap.hashes[i].clone()));
    if bh != Some(snap.hashes[i].as_str()) {
        out.insert("board".into(), Value::String(snap.boards[i].clone()));
    }
    let mut speed = Map::new();
    for p in o["speedHistory"].as_array().into_iter().flatten() {
        let res = p["resolution"].as_i64().unwrap_or(0);
        let t = p["t"].as_f64().unwrap_or(0.0);
        if tiers.contains(&res) && (full.contains(&res) || t + res as f64 > since) {
            let rate = py::round(p["rate"].as_f64().unwrap_or(0.0));
            let e = speed.entry(res.to_string()).or_insert(json!([]));
            e.as_array_mut().unwrap().push(json!([p["t"], rate]));
        }
    }
    out.insert("speed".into(), Value::Object(speed));
    let srv = o["serverRuns"].as_array();
    if let Some(last) = srv.and_then(|s| s.last()) {
        if runs != last["t"].as_f64() {
            out.insert("srv".into(), server_versions(srv.unwrap()));
        }
    }
    out
}

/// The server CPU's slices grouped by the commit that ran them.
pub fn server_versions(runs: &[Value]) -> Value {
    let mut groups: Vec<(String, Map<String, Value>, i64, f64)> = Vec::new();
    for r in runs {
        let v = if r["v"].is_object() { r["v"].clone() } else { json!({}) };
        let c = v.get("commit").cloned().unwrap_or(json!("untagged"));
        let key = py::str_of(&c);
        let rate = r["rate"].as_f64().unwrap_or(0.0);
        if let Some(g) = groups.iter_mut().find(|g| g.0 == key) {
            g.2 += 1;
            g.3 += rate;
        } else {
            let mut m = Map::new();
            m.insert("c".into(), c);
            m.insert("s".into(), v.get("subject").cloned().unwrap_or(json!("")));
            m.insert("from".into(), r["t"].clone());
            groups.push((key, m, 1, rate));
        }
    }
    groups.sort_by(|a, b| a.1["from"].as_f64().unwrap_or(0.0).total_cmp(&b.1["from"].as_f64().unwrap_or(0.0)));
    let rows: Vec<Value> = groups
        .into_iter()
        .map(|(_, mut m, n, sum)| {
            m.insert("n".into(), json!(n));
            m.insert("mean".into(), json!(py::round(sum / n as f64)));
            Value::Object(m)
        })
        .collect();
    let last = runs.last().unwrap();
    json!({"versions": rows, "last": {"t": last["t"], "rate": py::round(last["rate"].as_f64().unwrap_or(0.0))}})
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
