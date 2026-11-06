//! The page's traffic contract: /api/live sends only what changed, the page
//! carries its first data inline, static files are compressed once, versioned
//! URLs are cached for good and every other URL is revalidated.
mod common;
use common::*;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64()
}

fn done(db: &rusqlite::Connection, ip: &str, t: f64, rate: f64, v: Option<Value>) {
    let evens = 1_000_000_000_000f64 / 2.0; // the default slice, GB_SLICE unset
    db.execute(
        "INSERT INTO leases(slice_idx, client, ip, leased, heartbeat, status, done_evens, elapsed, speed, finished, version)
         VALUES(0, 'c', ?, ?, ?, 'done', ?, ?, ?, ?, ?)",
        rusqlite::params![ip, t - evens / rate, t, (evens as i64).to_string(), evens / rate, rate, t, v.map(|v| v.to_string())],
    )
    .unwrap();
}

#[test]
fn live_traffic() {
    let mut srv = Server::new("live");
    srv.start(); // creates the schema
    srv.stop();
    let db = rusqlite::Connection::open(&srv.db).unwrap();
    let n = now();
    for i in 0..200 {
        done(&db, "1.2.3.4", n - 900.0 * i as f64 - 30.0, 7e9, None); // browsers over the last ~2 days
    }
    done(&db, "127.0.0.1", n - 7200.0, 4e7, Some(json!({"commit": "aaa1111", "subject": "old"})));
    done(&db, "127.0.0.1", n - 3600.0, 5e7, Some(json!({"commit": "bbb2222", "subject": "new"})));
    srv.start();
    let mut c = Checks::default();
    let gz = [("Accept-Encoding", "gzip")];

    // ---- /api/live: full, then only what changed ----
    let b = srv.get("/api/live", &[]);
    let full = b.json();
    let speed_keys: Vec<&str> = {
        let mut k: Vec<&str> = full["speed"].as_object().unwrap().keys().map(String::as_str).collect();
        k.sort();
        k
    };
    c.check(
        full.get("board").is_some() && !full["bh"].as_str().unwrap_or("").is_empty() && speed_keys == ["10", "300", "3600"]
            && full.get("srv").is_some() && !full["v"]["engine"].as_str().unwrap_or("").is_empty(),
        "first poll: board, all three speed tiers, server versions, versions",
    );
    let srv_ = &full["srv"];
    let commits: Vec<&str> = srv_["versions"].as_array().unwrap().iter().map(|r| r["c"].as_str().unwrap()).collect();
    c.check(
        commits == ["aaa1111", "bbb2222"] && srv_["versions"][1]["mean"] == 5e7 && srv_["last"]["rate"] == 5e7,
        "server versions summarised on the server",
    );
    // an unchanged poll sends neither board nor table; a rebuild across a minute boundary
    // changes the board's stamp, so a fresh full poll gets up to three tries
    let (mut full, mut q, mut b, mut again);
    let mut tries = 0;
    loop {
        full = srv.get("/api/live", &[]).json();
        let srv_ = &full["srv"];
        q = format!("/api/live?since={}&bh={}&runs={}", full["t"], full["bh"].as_str().unwrap(), srv_["last"]["t"]);
        b = srv.get(&q, &[]);
        again = b.json();
        tries += 1;
        if (again.get("board").is_none() && again.get("srv").is_none()) || tries == 3 {
            break;
        }
    }
    c.check(again.get("board").is_none() && again.get("srv").is_none(), format!("unchanged board and server table are not sent again (keys {:?}, t {} vs {})", again.as_object().unwrap().keys().collect::<Vec<_>>(), again["t"], full["t"]));
    let n_buckets: usize = again["speed"].as_object().unwrap().values().map(|v| v.as_array().unwrap().len()).sum();
    c.check(n_buckets <= 3, format!("only the speed buckets a new slice can touch ({})", again["speed"]));
    c.check(b.body.len() < 400, format!("an unchanged poll is small ({} bytes)", b.body.len()));
    let full_t = full["t"].as_f64().unwrap();
    // the server's snapshot is rebuilt within SNAPSHOT_DIRTY_S of a change; direct inserts
    // do not mark it dirty, so wait for the periodic rebuild by polling
    let poll_until = |q: &str, ok: &dyn Fn(&Value) -> bool| -> Value {
        let t0 = std::time::Instant::now();
        loop {
            let v = srv.get(q, &[]).json();
            if ok(&v) || t0.elapsed().as_secs() > 40 {
                return v;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    };
    done(&db, "1.2.3.4", now(), 8e9, None);
    let moved = poll_until(&q, &|v| v["speed"].as_object().is_some_and(|s| s.len() == 3 && s.values().all(|x| !x.as_array().unwrap().is_empty())));
    c.check(
        moved["speed"].as_object().unwrap().iter().all(|(r, v)| {
            let r: f64 = r.parse().unwrap();
            v.as_array().unwrap().iter().any(|p| p[0].as_f64().unwrap() + r > full_t)
        }) && moved["speed"].as_object().unwrap().len() == 3,
        "a newly finished slice: its bucket in every tier",
    );
    done(&db, "127.0.0.1", now(), 6e7, Some(json!({"commit": "bbb2222", "subject": "new"})));
    let moved = poll_until(&q, &|v| v.get("srv").is_some());
    c.check(moved["srv"]["versions"].as_array().and_then(|a| a.last()).map(|l| l["n"] == 2).unwrap_or(false), format!("a new server slice: the table is sent again ({})", moved.get("srv").map(|v| v.to_string()).unwrap_or_default()));
    let b = srv.get(&format!("/api/live?narrow=1&bh={}", full["bh"].as_str().unwrap()), &[]);
    c.check(b.json()["board"].as_str().unwrap_or("").starts_with("GOLDBACH"), "another width: the board is sent");
    let t = full["t"].to_string();
    let lazy = srv.get(&format!("/api/live?since={t}&bh={}&tiers=300,10&full=10", full["bh"].as_str().unwrap()), &[]).json();
    let whole = srv.get("/api/live?tiers=10", &[]).json()["speed"]["10"].clone();
    c.check(
        lazy["speed"].as_object().unwrap().keys().all(|k| k == "10" || k == "300") && lazy["speed"]["10"] == whole,
        "a chart tier loaded on demand comes whole, the others stay incremental",
    );
    c.check(srv.get("/api/live?since=x", &[]).status == 400 && srv.get("/api/live?tiers=a", &[]).status == 400, "a bad query is refused");

    // ---- static files: compressed once, versioned URLs immutable ----
    let raw = std::fs::read(srv.static_dir.join("app.js")).unwrap();
    let h = sha256_hex(&raw);
    let r = srv.get("/app.js", &gz);
    c.check(
        r.status == 200 && r.header("Content-Encoding") == Some("gzip") && gunzip(&r.body) == raw && r.header("Vary") == Some("Accept-Encoding"),
        "gzip with Vary",
    );
    c.check(r.header("Cache-Control") == Some("no-cache"), "an unversioned URL is revalidated");
    let r = srv.get(&format!("/app.js?v={}", &h[..16]), &[]);
    c.check(r.header("Cache-Control").unwrap_or("").ends_with("immutable") && r.body == raw, "its own hash: cached for good");
    c.check(srv.get("/app.js?v=0123456789abcdef", &[]).header("Cache-Control") == Some("no-cache"), "a stale hash is not cached for good");
    c.check(srv.get("/app.js", &[("If-None-Match", &format!("\"{}\"", &h[..32]))]).status == 304, "a current ETag: 304");
    let eng = full["v"]["engine"].as_str().unwrap().to_string();
    for f in ["worker.js", "core.wasm", "kernels/check.wgsl"] {
        let r = srv.get(&format!("/{f}?v={eng}"), &gz);
        c.check(
            r.status == 200 && r.header("Cache-Control").unwrap_or("").ends_with("immutable") && r.header("Content-Encoding") == Some("gzip"),
            format!("{f}: engine version cached for good, compressed"),
        );
    }

    // ---- shader packs: one request per engine mode, the exact kernel sources ----
    let kdir = srv.dir.join("kernels");
    let packs: [(&str, &[(&str, &str)]); 2] = [
        ("full", &[("tile", "tile"), ("check", "check"), ("count30", "count30"), ("large", "large"), ("harvest", "harvest")]),
        ("lite", &[("tile", "tile"), ("check", "check"), ("count30", "count30"), ("largeLite", "large_lite"), ("harvestLite", "harvest_lite")]),
    ];
    for (mode, files) in packs {
        let r = srv.get(&format!("/kernels/{mode}.json?v={eng}"), &gz);
        let pack: Value = serde_json::from_slice(&gunzip(&r.body)).unwrap();
        let want: serde_json::Map<String, Value> =
            files.iter().map(|(k, f)| (k.to_string(), Value::from(std::fs::read_to_string(kdir.join(format!("{f}.wgsl"))).unwrap()))).collect();
        c.check(
            r.status == 200 && pack == Value::Object(want) && r.header("Cache-Control").unwrap_or("").ends_with("immutable"),
            format!("kernels/{mode}.json: exactly the {mode} kernels, compressed, cached for good"),
        );
    }
    c.check(srv.get("/kernels/full.json", &[]).header("Cache-Control") == Some("no-cache"), "an unversioned pack is revalidated");

    // ---- the page: first data inline, versioned script ----
    let r = srv.get("/", &[("User-Agent", "Mozilla/5.0 (iPhone)")]);
    let page = r.text();
    let marker = "<script type=\"application/json\" id=\"live-init\">";
    let init: Value = page
        .find(marker)
        .and_then(|a| {
            let rest = &page[a + marker.len()..];
            serde_json::from_str(&rest[..rest.find("</script>")?]).ok()
        })
        .unwrap_or(Value::Null);
    c.check(page.contains(&format!("src=\"app.js?v={}\"", &h[..16])), "the page loads app.js by its hash");
    let live = srv.get("/api/live", &[]).json();
    c.check(
        init["narrow"] == false && init["v"] == live["v"] && init["speed"] == json!({}) && init.get("srv").is_some()
            && !init["bh"].as_str().unwrap_or("").is_empty(),
        "the page carries its first live data (desktop default, no speed buckets: no chart)",
    );
    let narrow_live = srv.get("/api/live?narrow=1", &[]).json();
    c.check(init["nb"] == narrow_live["board"] && init["nbh"] == narrow_live["bh"], "the page also carries the compact board: phones show it without a request");
    let cc = "public, max-age=0, s-maxage=15, stale-while-revalidate=15, stale-if-error=86400";
    c.check(
        r.header("Cache-Control") == Some(cc) && srv.get("/api/live", &[]).header("Cache-Control") == Some(cc) && srv.get("/board", &[]).header("Cache-Control") == Some(cc),
        "page, board and live poll: shared at the CDN for one snapshot period, not kept by browsers",
    );
    for u in ["/board", "/api/live"] {
        let r1 = srv.get(u, &[]);
        let etag = r1.header("ETag").unwrap_or("-").to_string();
        let r2 = srv.get(u, &[("If-None-Match", &etag)]);
        c.check(
            r1.status == 200 && r1.header("ETag") == Some(&format!("\"{}\"", &sha256_hex(&r1.body)[..32])) && r2.status == 304 && r2.header("ETag") == Some(etag.as_str()),
            format!("{u}: an ETag of its body, a 304 while unchanged"),
        );
    }
    c.check(srv.get("/board", &[("If-None-Match", "\"0123\"")]).status == 200, "a stale ETag: the whole reply");
    let etag = srv.get("/board", &[]).header("ETag").unwrap().to_string();
    c.check(srv.get("/board", &[("If-None-Match", &format!("W/{etag}"))]).status == 304, "a weak ETag (the CDN's form) also matches");
    let r1 = srv.get("/", &gz);
    c.check(
        r1.status == 200 && r1.header("ETag").is_some() && srv.get("/", &[gz[0], ("If-None-Match", r1.header("ETag").unwrap())]).status == 304,
        "the page (compressed): a 304 while unchanged",
    );
    let hour = Some("public, max-age=3600");
    c.check(
        srv.get("/favicon.svg", &[]).header("Cache-Control") == hour && srv.get("/favicon.ico", &[]).header("Cache-Control") == hour && srv.get("/og.png", &[]).header("Cache-Control") == hour,
        "files no version depends on: kept an hour",
    );
    c.check(srv.get("/worker.js", &[]).header("Cache-Control") == Some("no-cache"), "an unversioned engine file is still revalidated");
    c.check(
        srv.get("/api/stats", &[]).header("Cache-Control") == Some("no-store") && srv.get("/api/version", &[]).header("Cache-Control") == Some("no-store"),
        "the other API reads are never cached",
    );
    c.check(!page.contains("<!--BOARD-->") && !page.contains("<!--LIVE-->") && !page.contains("href=\"favicon.svg\""), "board, data and icon inlined");
    c.done("LIVE TRAFFIC OK");
}
