//! Coordinator logic (no GPU): double verification across networks, disputes,
//! re-leasing, aggregates, exact claim checks, audits and bans; the speed tiers,
//! the server CPU's speed per version and the snapshot thread. One sequential
//! test per scenario (the spot-check rate is a process-wide knob).
use super::*;
use crate::validate::{is_prime, pn_is, validate_gaps};
use serde_json::json;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::Mutex as StdMutex;

/// The spot-check rate is global: tests that set it run one at a time.
static KNOB: StdMutex<()> = StdMutex::new(());

struct Checks(u32);
impl Checks {
    fn check(&mut self, cond: bool, what: &str) {
        println!("  {} {what}", if cond { "ok  " } else { "FAIL" });
        self.0 += !cond as u32;
    }
    fn done(self) {
        assert_eq!(self.0, 0, "{} check(s) failed", self.0);
    }
}

fn tmp_db(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("gb-{}-{name}-{}", std::process::id(), now()));
    std::fs::create_dir_all(&d).unwrap();
    d.join("t.db").display().to_string()
}

fn i(v: &Value) -> i128 {
    py::int(v).unwrap()
}

/// A synthetic result with exact-looking densities (no record claims).
fn result(lease: &Value, tweak: i128) -> Value {
    let (s, l) = (i(&lease["start"]), i(&lease["limit"]));
    let evens = (l - s) / 2 + 1;
    let x = (s + l) as f64 / 2.0;
    let primes = ((l - s) as f64 / x.ln()).round() as i128;
    let twins = (2.0 * 0.6601618158 * (l - s) as f64 / x.ln().powi(2)).round() as i128;
    let mut p30 = vec![primes / 8; 7];
    p30.push(primes - 7 * (primes / 8));
    let t30 = [twins / 3, twins / 3, twins - 2 * (twins / 3)];
    let strs = |v: &[i128]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    json!({"start": s.to_string(), "limit": l.to_string(), "evens": evens.to_string(), "pass": true,
           "primes": primes.to_string(), "twins": twins.to_string(), "primes30": strs(&p30), "twins30": strs(&t30),
           "pmax": 5003, "tailEvens": 10 + tweak, "histByP": [[7, evens - 10 - tweak], [5003, 10 + tweak]],
           "histByClass": [[[7, 1]]], "first": [], "beyond": [], "ridges": []})
}

fn slice_of(l: &Value) -> i64 {
    l["slice"].as_i64().unwrap()
}
fn lid(l: &Value) -> i64 {
    l["lease"].as_i64().unwrap()
}
fn status(v: &Value) -> String {
    v["status"].as_str().unwrap_or("").to_string()
}
fn state_of_slice(db: &Db, idx: i64) -> String {
    vs(&db.q("SELECT state FROM slices WHERE idx=?", &[&idx]).unwrap()[0][0]).unwrap()
}

#[test]
fn exact_checks_and_helpers() {
    let mut c = Checks(0);
    let n_rec: i128 = 4010506309418407798; // the B580's record: p(n) = 8627
    c.check(pn_is(n_rec, 8627), "pn_is accepts the B580 record 8627");
    c.check(!pn_is(n_rec, 8629), "pn_is rejects a wrong p");
    c.check(!pn_is(n_rec + 2, 8627), "pn_is rejects a wrong n");
    // count ranges (start, limit + 1]: the prime between two native slices (2026-10-09 count-check
    // false alarm) is counted once, by the slice below it; nothing above RANGE_HI
    c.check(
        boundary_counts(4035799999999999998) == (1, is_prime(4035799999999999997) as i128 + is_prime(4035800000000000001) as i128),
        "the prime 4035799999999999999 between two slices is added to the slice below",
    );
    c.check(boundary_counts(4035849999999999998) == (0, 0), "a composite boundary adds nothing");
    c.check(boundary_counts(RANGE_HI) == (0, 0), "nothing is added above RANGE_HI");
    c.check(net_of("1.2.3.4") == net_of("1.2.3.200") && net_of("1.2.3.4") != net_of("1.2.4.4"), "IPv4 networks are /24");
    // client address behind Cloudflare: CF-Connecting-IP only from an edge range
    c.check(client_ip("162.158.1.2", Some("203.0.113.7")) == "203.0.113.7", "Cloudflare edge: visitor from CF-Connecting-IP");
    c.check(client_ip("2606:4700::1", Some("2001:db8::5")) == "2001:db8::5", "Cloudflare IPv6 edge: visitor from CF-Connecting-IP");
    c.check(client_ip("198.51.100.9", Some("203.0.113.7")) == "198.51.100.9", "direct request: forged CF-Connecting-IP ignored");
    c.check(client_ip("162.158.1.2", Some("not-an-ip")) == "162.158.1.2", "malformed CF-Connecting-IP: edge address kept");
    c.check(client_ip("198.51.100.9", None) == "198.51.100.9", "no Cloudflare header: connecting address");
    c.done();
}

#[test]
fn double_verification_audits_bans_and_trust() {
    let _k = KNOB.lock().unwrap_or_else(|e| e.into_inner());
    let mut c = Checks(0);
    let st = Store::open(&tmp_db("main")).unwrap();
    let db = st.lock();
    let reg = |n: &str, ip: &str| db.register(n, "gpu", ip).unwrap();
    let (a, b, cc, d) = (reg("A", "1.1.1.1"), reg("B", "2.2.2.2"), reg("C", "3.3.3.3"), reg("D", "1.1.1.9"));
    let la = db.lease(&a, "1.1.1.1", false).unwrap().unwrap();
    let ld = db.lease(&d, "1.1.1.9", false).unwrap().unwrap();
    c.check(slice_of(&ld) != slice_of(&la), "a second device on the same network gets a different slice");
    let lb = db.lease(&b, "2.2.2.2", false).unwrap().unwrap();
    c.check(slice_of(&la) == slice_of(&lb) && slice_of(&la) == 0, "devices on different networks share a slice");
    c.check(status(&db.result(&a, lid(&la), &result(&la, 0), 10.0, None).unwrap()) == "waiting for a second device", "first result waits");
    c.check(status(&db.result(&b, lid(&lb), &result(&lb, 0), 10.0, None).unwrap()) == "verified", "matching result from another network verifies");
    c.check(db.agg().unwrap()["hist"]["5003"] == 10, "aggregates updated");
    // forged claims are rejected by the exact checks
    let lc = db.lease(&cc, "3.3.3.3", false).unwrap().unwrap();
    let mut forged = result(&lc, 0);
    let evens = i(&forged["evens"]);
    forged["histByP"] = json!([[7, evens - 11], [5003, 10], [8627, 1]]);
    forged["tailEvens"] = 11.into();
    forged["pmax"] = 8627.into();
    forged["ridges"] = json!([{"n": (i(&lc["start"]) + 4).to_string(), "p": 8627}]);
    c.check(status(&db.result(&cc, lid(&lc), &forged, 10.0, None).unwrap()) == "rejected", "forged ridge rejected by the exact check");
    let lc = db.lease(&cc, "3.3.3.3", false).unwrap().unwrap();
    let mut bad = result(&lc, 0);
    let doubled: Vec<String> = bad["primes30"].as_array().unwrap().iter().map(|v| (i(v) * 2).to_string()).collect();
    bad["primes"] = doubled.iter().map(|v| v.parse::<i128>().unwrap()).sum::<i128>().to_string().into();
    bad["primes30"] = json!(doubled);
    c.check(status(&db.result(&cc, lid(&lc), &bad, 10.0, None).unwrap()) == "rejected", "implausible prime count rejected");
    // audit pass, audit fail (bans the confirmers, reopens the slice)
    let nxt = db.audit_next().unwrap();
    c.check(nxt.as_ref().is_some_and(|n| n["slice"] == 0), "verified slice queued for audit");
    c.check(status(&db.audit_result(0, &result(&la, 0)).unwrap()) == "pass", "audit with the same result passes");
    c.check(db.audit_next().unwrap().is_none(), "nothing left to audit");
    // a second slice confirmed by A and C, but the audit disagrees (D, on A's network, steps aside)
    db.release(&d, lid(&ld)).unwrap();
    let lc = db.lease(&cc, "3.3.3.3", false).unwrap().unwrap();
    let la2 = db.lease(&a, "1.1.1.1", false).unwrap().unwrap();
    c.check(slice_of(&la2) == slice_of(&lc), "two networks on the next slice");
    db.result(&cc, lid(&lc), &result(&lc, 0), 10.0, None).unwrap();
    c.check(status(&db.result(&a, lid(&la2), &result(&la2, 0), 10.0, None).unwrap()) == "verified", "garbage confirmed by two colluding devices");
    let ev_before = i(&db.agg().unwrap()["evens"]);
    let r = db.audit_result(slice_of(&lc) as i128, &result(&lc, 1)).unwrap();
    c.check(status(&r) == "fail" && r["banned"] == 2, "audit catches the wrong result, bans both");
    c.check(!db.client(&a).unwrap() && !db.client(&cc).unwrap(), "banned clients are refused");
    let e = db.register("E", "gpu", "1.1.1.77").unwrap();
    c.check(db.lease(&e, "1.1.1.77", false).unwrap().is_none(), "a banned network gets no work");
    c.check(i(&db.agg().unwrap()["evens"]) < ev_before, "the wrong slice is subtracted from the totals");
    c.check(state_of_slice(&db, slice_of(&lc)) == "open", "the wrong slice is reopened");

    // ---- trust: earned after TRUST_AFTER confirmations and TRUST_MIN_AGE ----------
    let t = db.register("T", "gpu", "5.5.5.5").unwrap();
    db.exec("UPDATE clients SET agree=?, created=? WHERE id=?", &[&(TRUST_AFTER as i64), &0i64, &t]).unwrap();
    c.check(db.trusted(&t).unwrap(), "a device with enough confirmations and age is trusted");
    SPOT_RATE.store(0.0f64.to_bits(), SeqCst);
    let lt = db.lease(&t, "5.5.5.5", false).unwrap().unwrap();
    let r = db.result(&t, lid(&lt), &result(&lt, 0), 10.0, None).unwrap();
    c.check(status(&r).starts_with("verified (trusted"), "a trusted device's single result counts");
    SPOT_RATE.store(1.0f64.to_bits(), SeqCst); // force a spot check
    let lt2 = db.lease(&t, "5.5.5.5", false).unwrap().unwrap();
    c.check(status(&db.result(&t, lid(&lt2), &result(&lt2, 0), 10.0, None).unwrap()).contains("spot check queued"), "spot check queued");
    let u = db.register("U", "gpu", "6.6.6.6").unwrap();
    let lu = db.lease(&u, "6.6.6.6", false).unwrap().unwrap();
    c.check(slice_of(&lu) == slice_of(&lt2), "the spot-check slice goes to another network");
    c.check(status(&db.result(&u, lid(&lu), &result(&lu, 0), 10.0, None).unwrap()) == "verified (spot check confirmed)", "spot check confirms");
    // the trusted device lies on its next slice; the spot check disagrees, a third device decides
    let lt3 = db.lease(&t, "5.5.5.5", false).unwrap().unwrap();
    db.result(&t, lid(&lt3), &result(&lt3, 1), 10.0, None).unwrap(); // wrong, accepted alone for now
    let ev_lie = i(&db.agg().unwrap()["evens"]);
    let lu2 = db.lease(&u, "6.6.6.6", false).unwrap().unwrap();
    c.check(slice_of(&lu2) == slice_of(&lt3), "spot check of the lie");
    c.check(status(&db.result(&u, lid(&lu2), &result(&lu2, 0), 10.0, None).unwrap()) == "disputed", "the spot check disagrees: disputed");
    c.check(i(&db.agg().unwrap()["evens"]) < ev_lie, "the lie is undone from the totals");
    let v = db.register("V", "gpu", "7.7.7.7").unwrap();
    let lv = db.lease(&v, "7.7.7.7", false).unwrap().unwrap();
    c.check(slice_of(&lv) == slice_of(&lt3), "a third device settles the dispute");
    c.check(status(&db.result(&v, lid(&lv), &result(&lv, 0), 10.0, None).unwrap()) == "verified", "majority verifies");
    c.check(!db.client(&t).unwrap(), "the lying trusted device is banned");
    c.check(state_of_slice(&db, slice_of(&lt)) == "open", "the liar's single-device slices are reopened");
    SPOT_RATE.store(SPOT.to_bits(), SeqCst);
    db.report(&b, 0, "GOLDBACH COUNTEREXAMPLE candidate n = 42").unwrap();
    let stats = db.stats_obj().unwrap();
    c.check(stats["web"]["verified"].as_i64().unwrap_or(0) >= 1, "stats feed");

    // ---- the speed tiers ------------------------------------------------------
    let t_now = now() as i64;
    db.exec(
        "INSERT INTO leases(slice_idx, client, ip, leased, heartbeat, status, done_evens, elapsed, finished)
         VALUES(999999, 'speed-test', '1.2.3.4', ?, ?, 'done', ?, ?, ?)",
        &[&(t_now - 60), &(t_now - 60), &"1200000", &3.0f64, &(t_now - 60)],
    )
    .unwrap();
    let sp = db.stats_obj().unwrap();
    let samples = sp["speedHistory"].as_array().unwrap();
    c.check(
        [10, 300, 3600].iter().all(|r| samples.iter().any(|s| s["resolution"] == *r)),
        "stats feed exposes all three speed-history tiers",
    );
    c.check(
        samples.iter().any(|s| s["resolution"] == 10 && s["rate"].as_f64() == Some(1_000_000.0 / 3.0)),
        "10-second speed tier is rounded and exposed",
    );
    drop(db);
    c.done();
}

#[test]
fn the_server_verifier_priorities_and_lease_ahead() {
    let mut c = Checks(0);
    // ---- the server's own verifier: disputes, spot checks, trust bootstrap, then new work ----
    let st2 = Store::open(&tmp_db("srv")).unwrap();
    let db = st2.lock();
    let x = db.register("X", "gpu", "8.8.8.1").unwrap(); // close to trust
    let y = db.register("Y", "gpu", "9.9.9.1").unwrap(); // new device
    db.exec("UPDATE clients SET agree=9 WHERE id=?", &[&x]).unwrap();
    let ly = db.lease(&y, "9.9.9.1", false).unwrap().unwrap(); // slice 0, waits for a 2nd result
    db.result(&y, lid(&ly), &result(&ly, 0), 10.0, None).unwrap();
    let lx = db.lease(&x, "8.8.8.1", false).unwrap().unwrap();
    c.check(slice_of(&lx) == 0, "a browser confirms the lowest waiting slice");
    db.release(&x, lid(&lx)).unwrap();
    let srv = db.register("server (idle CPU)", "CPU", "172.18.0.1").unwrap();
    let ly2 = db.lease(&y, "9.9.9.1", false).unwrap().unwrap(); // Y takes slice 1 (new): Y's slices 0, 1 wait
    db.result(&y, lid(&ly2), &result(&ly2, 0), 10.0, None).unwrap();
    // X's own result on slice 2 waits too
    let (s2, l2) = slice_bounds(2);
    db.exec("INSERT INTO slices(idx, start, lim) VALUES(2, ?, ?)", &[&s2.to_string(), &l2.to_string()]).unwrap();
    let lx3 = db.lease(&x, "8.8.8.1", false).unwrap().unwrap();
    c.check(slice_of(&lx3) == 0, "browsers keep the lowest-slice order");
    db.release(&x, lid(&lx3)).unwrap();
    db.exec(
        "INSERT INTO leases(slice_idx, client, ip, net, leased, heartbeat, status, hash) VALUES(2, ?, '8.8.8.1', ?, 0, 0, 'done', 'h')",
        &[&x, &net_of("8.8.8.1")],
    )
    .unwrap();
    let ls = db.lease(&srv, "172.18.0.1", false).unwrap().unwrap();
    c.check(slice_of(&ls) == 2, "the server confirms the device closest to trust first (slice 2 of X, agree 9)");
    db.release(&srv, lid(&ls)).unwrap();
    db.exec("UPDATE slices SET state='verified', spot=1 WHERE idx=1", &[]).unwrap();
    let ls = db.lease(&srv, "172.18.0.1", false).unwrap().unwrap();
    c.check(slice_of(&ls) == 1, "spot checks of trusted devices come before trust bootstrap");
    db.release(&srv, lid(&ls)).unwrap();
    db.exec("UPDATE slices SET state='disputed' WHERE idx=0", &[]).unwrap();
    let ls = db.lease(&srv, "172.18.0.1", false).unwrap().unwrap();
    c.check(slice_of(&ls) == 0, "disputes come first");
    db.release(&srv, lid(&ls)).unwrap();
    db.exec("UPDATE slices SET state='verified', spot=0", &[]).unwrap();
    let ls = db.lease(&srv, "172.18.0.1", false).unwrap().unwrap();
    c.check(slice_of(&ls) == 3, "nothing waiting: the server takes a new slice");
    drop(db);

    // ---- lease-ahead: the next slice while the current one runs (at most two per client)
    let st3 = Store::open(&tmp_db("ahead")).unwrap();
    let db = st3.lock();
    let z = db.register("Z", "gpu", "4.4.4.4").unwrap();
    let l1 = db.lease(&z, "4.4.4.4", false).unwrap().unwrap();
    let l2 = db.lease(&z, "4.4.4.4", true).unwrap().unwrap();
    c.check(slice_of(&l2) != slice_of(&l1), "lease-ahead: the next lease is another slice");
    c.check(db.heartbeat(&z, lid(&l1), 5).unwrap() && db.heartbeat(&z, lid(&l2), 0).unwrap(), "lease-ahead: the current lease stays active");
    let l3 = db.lease(&z, "4.4.4.4", true).unwrap().unwrap();
    c.check(
        !db.heartbeat(&z, lid(&l1), 5).unwrap() && db.heartbeat(&z, lid(&l2), 0).unwrap() && db.heartbeat(&z, lid(&l3), 0).unwrap(),
        "lease-ahead: never more than two active leases (the oldest is released)",
    );
    let l4 = db.lease(&z, "4.4.4.4", false).unwrap().unwrap();
    c.check(
        !db.heartbeat(&z, lid(&l2), 0).unwrap() && !db.heartbeat(&z, lid(&l3), 0).unwrap() && db.heartbeat(&z, lid(&l4), 0).unwrap(),
        "a plain lease still releases every other lease of the client (older clients unchanged)",
    );
    let completed = db.result(&z, lid(&l4), &result(&l4, 0), 10.0, None).unwrap();
    let expected = (i(&l4["limit"]) - i(&l4["start"])) / 2 + 1;
    let lease_id = lid(&l4);
    let recorded = vs(&db.q("SELECT done_evens FROM leases WHERE id=?", &[&lease_id]).unwrap()[0][0]).unwrap();
    c.check(status(&completed) != "rejected" && recorded == expected.to_string(), "completed lease records the full slice count, not the last heartbeat progress");
    drop(db);
    c.done();
}

#[test]
fn server_cpu_speed_per_version() {
    // the server's own worker (local address) has its version stored with each slice and
    // listed in serverRuns; a browser's is ignored
    let st = Store::open(&tmp_db("ver")).unwrap();
    let db = st.lock();
    let t = now();
    let rows = [
        ("172.18.0.1", 4e7, json!({"commit": "aaa1111", "subject": "old", "engine": "cpu_worker segment 3e9"})),
        ("172.18.0.1", 5e7, json!({"commit": "bbb2222", "subject": "new", "engine": "cpu_worker segment 3e9"})),
        ("1.2.3.4", 9e9, json!({"commit": "fake"})),
    ];
    for (k, (ip, speed, v)) in rows.iter().enumerate() {
        let k = k as i64;
        let (s, l) = slice_bounds(k as i128);
        db.exec("INSERT OR IGNORE INTO slices(idx, start, lim) VALUES(?, ?, ?)", &[&k, &s.to_string(), &l.to_string()]).unwrap();
        let cid = db.register(&format!("dev{k}"), "cpu", ip).unwrap();
        let lease_id = db
            .insert("INSERT INTO leases(slice_idx, client, ip, leased, heartbeat) VALUES(?, ?, ?, ?, ?)", &[&k, &cid, ip, &t, &t])
            .unwrap();
        let fake = json!({"start": s.to_string(), "limit": l.to_string()});
        let evens = *SLICE as f64 / 2.0;
        let got = db.result(&cid, lease_id, &result(&fake, 0), evens / speed, Some(v)).unwrap();
        assert_ne!(status(&got), "rejected", "{got}");
        db.exec("UPDATE leases SET finished=? WHERE id=?", &[&(t - 300.0 + 100.0 * k as f64), &lease_id]).unwrap();
    }
    db.exec(
        "INSERT INTO leases(slice_idx, client, ip, leased, heartbeat, status, done_evens, elapsed, speed, finished)
         VALUES(9, 'c', '127.0.0.1', ?, ?, 'done', '1', 3600, 3e7, ?)",
        &[&(t - 7200.0), &(t - 400.0), &(t - 400.0)],
    )
    .unwrap();
    let runs = db.stats_obj().unwrap()["serverRuns"].as_array().unwrap().clone();
    let rates: Vec<f64> = runs.iter().map(|r| r["rate"].as_f64().unwrap()).collect();
    assert_eq!(rates, vec![3e7, 4e7, 5e7], "oldest first, browsers excluded: {runs:?}");
    assert!(runs[0]["v"].is_null() && runs[1]["v"]["commit"] == "aaa1111" && runs[2]["v"]["commit"] == "bbb2222", "{runs:?}");
    let browser = db.q("SELECT COUNT(*) FROM leases WHERE ip='1.2.3.4' AND version IS NOT NULL", &[]).unwrap();
    assert_eq!(vi(&browser[0][0]), 0, "a browser's version is ignored");
}

#[test]
fn speed_history_has_three_bounded_tiers() {
    let st = Store::open(&tmp_db("speed")).unwrap();
    let db = st.lock();
    let now_i = now() as i64;
    let finished = now_i - 120;
    db.exec(
        "INSERT INTO leases(slice_idx, client, ip, leased, heartbeat, status, done_evens, elapsed, finished)
         VALUES(1, 'test-client', '1.2.3.4', ?, ?, 'done', ?, ?, ?)",
        &[&finished, &finished, &"1200000", &3.0f64, &finished],
    )
    .unwrap();
    let stats = db.stats_obj().unwrap();
    let samples = stats["speedHistory"].as_array().unwrap();
    let res: std::collections::HashSet<i64> = samples.iter().map(|p| p["resolution"].as_i64().unwrap()).collect();
    assert_eq!(res, [10, 300, 3600].into_iter().collect());
    let at = |r: i64, back: i64| samples.iter().filter(|p| p["resolution"] == r).all(|p| p["t"].as_i64().unwrap() >= now_i - back);
    assert!(at(10, 3600) && at(300, 86400) && at(3600, 7 * 86400));
    assert!(samples.iter().any(|p| p["resolution"] == 10 && p["rate"].as_f64() == Some(1_000_000.0 / 3.0)));
    let n = vi(&db.q("SELECT COUNT(*) FROM speed_history", &[]).unwrap()[0][0]);
    assert!(n <= 360 + 288 + 168);
}

#[test]
fn prime_gaps_of_a_result_and_their_agreement() {
    // prime gaps of a result (validate_gaps): a real window around the gap 1476
    // (largest below 4e18) built here by Miller-Rabin, then tampered with
    let mut c = Checks(0);
    let (gs, gl): (i128, i128) = (1425172824437690000, 1425172824437710000);
    let pr: Vec<i128> = (gs + 1..gl).step_by(2).filter(|&q| is_prime(q)).collect();
    let mut p0 = gs - 1;
    while !is_prime(p0) {
        p0 -= 2;
    }
    let all: Vec<i128> = std::iter::once(p0).chain(pr.iter().copied()).collect();
    let gaps: Vec<i128> = all.windows(2).map(|w| w[1] - w[0]).collect();
    let mut hist: std::collections::BTreeMap<i128, i128> = Default::default();
    let mut lows: std::collections::BTreeMap<i128, i128> = Default::default();
    for (w, g) in all.windows(2).zip(&gaps) {
        *hist.entry(*g).or_default() += 1;
        if *g >= 700 {
            lows.entry(*g).or_insert(w[0]);
        }
    }
    let gm = *gaps.iter().max().unwrap();
    let real = json!({"primes": pr.len().to_string(), "gaps": {
        "n": pr.len().to_string(),
        "d": (1..=4u32).map(|k| gaps.iter().map(|g| g.pow(k)).sum::<i128>().to_string()).collect::<Vec<_>>(),
        "hist": hist.iter().map(|(g, c)| json!([g, c])).collect::<Vec<_>>(),
        "tail": lows.iter().map(|(g, lo)| json!([g, gaps.iter().filter(|x| *x == g).count(), lo.to_string()])).collect::<Vec<_>>(),
        "max": [gm, lows.get(&gm).map(|v| v.to_string())]}});
    let got = validate_gaps(&real, gs, gl);
    let gc = got.clone().ok().flatten().unwrap_or_default();
    let gcv: Value = serde_json::from_str(&gc).unwrap_or(Value::Null);
    c.check(got.is_ok() && gm == 1476 && gcv["max"] == json!([1476, "1425172824437699411"]), &format!("validate_gaps accepts a real window ({got:?})"));
    c.check(validate_gaps(&json!({"primes": "5"}), gs, gl) == Ok(None), "a result without gaps (older engine) is accepted, gaps missing");
    let mut bad = |f: &dyn Fn(&mut Value), what: &str| {
        let mut r = real.clone();
        f(&mut r["gaps"]);
        c.check(validate_gaps(&r, gs, gl).is_err(), &format!("validate_gaps rejects {what}"));
    };
    bad(&|o| o["n"] = (i(&o["n"]) + 1).to_string().into(), "a wrong gap count");
    bad(&|o| o["hist"][0][1] = py::num(i(&o["hist"][0][1]) + 1), "a histogram that does not add up");
    bad(&|o| o["d"][3] = (i(&o["d"][3]) + 1).to_string().into(), "a wrong sum of g^4");
    bad(&|o| o["max"][1] = (i(&o["max"][1]) + 2).to_string().into(), "a largest gap at the wrong place");
    bad(&|o| o["tail"] = json!([]), "a missing tail entry");
    bad(&|o| o["d"][0] = (i(&o["d"][0]) + 2).to_string().into(), "a wrong d1");

    // agreement on the gaps when a slice verifies (settle_gaps) and the browsers' totals
    // (web_gaps): two equal objects -> double, one -> single, different ones -> disputed
    // (not counted), none -> missing
    let sg = Store::open(&tmp_db("gaps")).unwrap();
    let db = sg.lock();
    let g1 = gc.clone();
    let g2 = gc.replacen("\"n\":\"", "\"n\":\"9", 1); // any other object
    let cases: [(i64, Vec<Option<&String>>); 4] =
        [(0, vec![Some(&g1), Some(&g1)]), (1, vec![Some(&g1)]), (2, vec![Some(&g1), Some(&g2)]), (3, vec![None, None])];
    let off: i128 = 1_000_000_000_000;
    for (idx, objs) in &cases {
        db.exec(
            "INSERT INTO slices(idx, start, lim, state, hash) VALUES(?,?,?, 'verified', 'h')",
            &[idx, &(gs + off * *idx as i128).to_string(), &(gl + off * *idx as i128).to_string()],
        )
        .unwrap();
        for o in objs {
            db.exec("INSERT INTO leases(slice_idx, client, status, hash, gaps) VALUES(?, 'c', 'done', 'h', ?)", &[idx, o]).unwrap();
        }
        db.settle_gaps(*idx).unwrap();
    }
    let states: Vec<(i128, String)> = db
        .q("SELECT idx, gaps_state FROM slices ORDER BY idx", &[])
        .unwrap()
        .iter()
        .map(|r| (vi(&r[0]), vs(&r[1]).unwrap()))
        .collect();
    let want: Vec<(i128, String)> = [(0, "double"), (1, "single"), (2, "disputed"), (3, "missing")].iter().map(|(a, b)| (*a, b.to_string())).collect();
    c.check(states == want, &format!("settle_gaps: double / single / disputed / missing ({states:?})"));
    let wg = db.web_gaps().unwrap();
    c.check(
        wg["n"] == (2 * pr.len()).to_string() && wg["max"] == json!([1476, "1425172824437699411"]) && wg["states"]["disputed"] == 1,
        "web_gaps sums the agreed slices only",
    );
    c.check(wg["range"] == json!([gs.to_string(), (gl + off).to_string()]), "web_gaps: the strip of the counted slices");
    c.done();
}

#[test]
fn the_board_shows_a_gap_backfill() {
    // a backfill (production re-running a range below the main resume point) is sent as
    // progress.backfill; the board shows it instead of a B580 that seems to stall
    let mut c = Checks(0);
    let bf = json!({"what": "gaps", "start": "4000000000000000000", "limit": "4000199999999999998", "resume": "4000100000000000000",
        "slices": 2, "pct": 50.0, "rate": 5e13 / 900.0, "eta_days": 1.0, "desired": "running",
        "recent": [{"start": "4000050000000000000", "limit": "4000099999999999998", "at": "2026-10-19T10:09:22+02:00", "gpu_s": 500.0, "rate": 1e11, "pmax": 0}],
        "now": {"start": "4000100000000000000", "done": 0.25, "elapsed_s": 99.0, "at": 0}});
    let mut bs = json!({"native": {"resume": "4116600000000000000", "rate": 6e10, "slices": 2333, "backfill": bf,
                 "recent": [{"start": "4116550000000000000", "at": "2026-10-19T09:54", "gpu_s": 362, "rate": 6.9e10}]},
      "web": {"evens": "0", "lowest": null, "open": 0, "disputed": 0, "verified": 0},
      "devices": [], "time": 0, "ridges": [], "clients": 0});
    for nw in [false, true] {
        let txt = board::board(&bs, nw);
        let mine: Vec<&str> = txt.lines().filter(|l| l.contains("backfill") || l.contains("main range") || l.contains("e18-")).collect();
        let tag = if nw { "narrow" } else { "wide" };
        c.check(txt.contains("days left") && txt.contains("slice in progress 25 %"), &format!("board ({tag}): days left and the slice in progress"));
        c.check(
            txt.contains("backfilling gaps") && txt.contains(" 50.0") && txt.contains("4.116600e18") && txt.contains("(gap backfill)")
                && txt.contains(if nw { "4.00005e18" } else { "4 000 050 000" })
                && mine.len() >= 3
                && mine.iter().map(|l| l.chars().count()).max().unwrap() <= if nw { 44 } else { 76 },
            &format!("board ({tag}): the B580 line shows the backfill, its slices, the main resume point"),
        );
    }
    bs["native"].as_object_mut().unwrap().remove("backfill");
    let plain = board::board(&bs, false);
    c.check(!plain.contains("backfilling") && plain.contains("4.116600e18"), "board without a backfill: unchanged");
    c.done();
}

#[test]
fn snapshots_are_prepared_and_a_result_refreshes_them() {
    let _k = KNOB.lock().unwrap_or_else(|e| e.into_inner());
    let st = Store::open(&tmp_db("snap")).unwrap();
    st.start_snapshots();
    // requests read the prepared snapshot without the store lock
    let held = st.lock();
    let t0 = Instant::now();
    for _ in 0..200 {
        let s = st.snap();
        assert!(!s.body.is_empty());
    }
    println!("200 polls with the store lock held: {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0);
    drop(held);
    let before = st.snap().built;
    let (cid, lease) = st
        .write(|db| {
            let cid = db.register("dev", "gpu", "1.2.3.4")?;
            let lease = db.lease(&cid, "1.2.3.4", false)?.unwrap();
            db.result(&cid, lid(&lease), &result(&lease, 0), 100.0, None)?;
            Ok((cid, lease))
        })
        .unwrap();
    let _ = (cid, lease);
    let t = Instant::now();
    while st.snap().built <= before && t.elapsed().as_secs_f64() < 10.0 {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let waited = t.elapsed().as_secs_f64();
    assert!(st.snap().built > before && waited <= SNAPSHOT_DIRTY_S + 1.5, "a result: snapshot rebuilt after {waited:.1} s");
}

#[test]
fn the_live_papers_reference_tables() {
    // client/paper.json (tools: make-paper-json): each p once, 105 primes with S(p) above 4e18
    let ref_: Value = serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../client/paper.json")).unwrap()).unwrap();
    let t0 = ref_["t0"].as_array().unwrap();
    let ps: std::collections::HashSet<i128> = t0.iter().map(|r| i(&r[0])).collect();
    assert_eq!(ps.len(), t0.len(), "each p once");
    assert_eq!(t0.iter().filter(|r| r[1].is_null()).count(), 105, "105 primes with S(p) above 4e18");
    let s = |p: i128| t0.iter().find(|r| i(&r[0]) == p).unwrap()[1].clone();
    assert_eq!(s(9781), json!("3325581707333960528"));
    assert!(s(8627).is_null(), "S(9781) and the unknown S(8627)");
    assert!([3, 241].iter().all(|p| ref_["C"].get(p.to_string()).is_some()), "Hardy-Littlewood constants for 3 .. 241");
}
