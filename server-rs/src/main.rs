//! goldbach-web coordinator, in Rust (a port of server/server.py: same routes,
//! same responses, same SQLite schema, so it runs on the production database).
//!
//! Hands out slices of the campaign range to browser clients from the TOP down,
//! gives every slice to two different clients and accepts it only when their
//! configuration-independent results are identical (sha256 of a canonical JSON),
//! expires stale leases, serves the dashboard and a small stats feed.
//!
//!   goldbach-server --db goldbach.db --static ../client --bind 127.0.0.1 --port 8090
//!
//! Admin calls (native campaign sync, floor) need the token in <db>.token.
mod seed;
mod board;
mod py;
mod store;
mod timing;
mod validate;

use axum::body::{Body, to_bytes};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::Response;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;
use store::{R, Store, gzip, now};

pub const RANGE_LO: i128 = 4_000_000_000_000_000_000;
pub const RANGE_HI: i128 = 5_000_000_000_000_000_000;
pub const AGG_KEY: &str = "agg2";
/// integers per web slice (GB_SLICE: tests only)
pub static SLICE: LazyLock<i128> =
    LazyLock::new(|| std::env::var("GB_SLICE").ok().and_then(|s| s.trim().parse().ok()).unwrap_or(1_000_000_000_000));
pub const REPLICAS: i128 = 2;
pub const TRUST_AFTER: i128 = 10;
pub const TRUST_MIN_AGE: f64 = 86400.0;
pub const SPOT: f64 = 0.10;
/// the spot-check rate in use (f64 bits): SPOT, except where a test forces it
pub static SPOT_RATE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0x3FB999999999999A);
pub const KEEP_FULL_S: f64 = 7.0 * 86400.0;
pub const LEASE_S: f64 = 240.0;
pub const SNAPSHOT_DIRTY_S: f64 = 2.0;
/// a snapshot older than this is rebuilt even when nothing marked it dirty (GB_SNAPSHOT_MAX_S: tests only)
pub static SNAPSHOT_MAX_S: LazyLock<f64> =
    LazyLock::new(|| std::env::var("GB_SNAPSHOT_MAX_S").ok().and_then(|s| s.trim().parse().ok()).unwrap_or(15.0));
pub const SERVER_RUNS: i64 = 2000;
const MAX_BODY: usize = 4 << 20;
pub const SITE: &str = "https://goldbach.net01.io";
pub const PAGES: &[(&str, &str, &str)] = &[("/", "always", "1.0"), ("/paper", "hourly", "0.8")];
const INDEXNOW_KEY_FILE: &str = "indexnow-070ede81046057733a264f2be8185e89.txt";
const STATIC_FILES: &[&str] = &[
    "index.html", "app.js", "worker.js", "engine.js", "pcache.js", "selftest.js", "core.js", "core.wasm",
    "kernels/tile.wgsl", "kernels/large.wgsl", "kernels/harvest.wgsl", "kernels/check.wgsl", "kernels/count30.wgsl",
    "kernels/large_lite.wgsl", "kernels/harvest_lite.wgsl", "favicon.svg", "favicon.ico", "google3dd777e111e53294.html", "og.png",
    "robots.txt", "paper.html", "paper.js", "paper.json", INDEXNOW_KEY_FILE, "llms.txt", "llms-full.txt", "auth.md", "openapi.json",
    "goldbach-skill.md",
];
const ALIASES: &[(&str, &str)] = &[("/.well-known/agent-skills/goldbach/SKILL.md", "goldbach-skill.md")];
const SKILL_URL: &str = "https://goldbach.net01.io/.well-known/agent-skills/goldbach/SKILL.md";
const CONTENT_SIGNAL: &str = "ai-train=yes, search=yes, ai-input=yes";
const LINKS: &str = concat!(
    "</.well-known/api-catalog>; rel=\"api-catalog\", </openapi.json>; rel=\"service-desc\"; type=\"application/openapi+json\", ",
    "</.well-known/agent-skills/goldbach/SKILL.md>; rel=\"service-doc\"; type=\"text/markdown\", ",
    "</llms.txt>; rel=\"describedby\"; type=\"text/plain\", ",
    "</llms-full.txt>; rel=\"describedby\"; type=\"text/plain\""
);
const MCP_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const ENGINE_FILES: &[&str] = &[
    "worker.js", "engine.js", "pcache.js", "selftest.js", "core.js", "core.wasm", "kernels/tile.wgsl",
    "kernels/large.wgsl", "kernels/harvest.wgsl", "kernels/check.wgsl", "kernels/count30.wgsl",
    "kernels/large_lite.wgsl", "kernels/harvest_lite.wgsl",
];
const PAGE_FILES: &[&str] = &["index.html", "app.js"];
/// the shader pack of each engine mode: source key -> kernels/<file>.wgsl
const KERNEL_PACKS: &[(&str, &[(&str, &str)])] = &[
    ("kernels/full.json", &[("tile", "tile"), ("check", "check"), ("count30", "count30"), ("large", "large"), ("harvest", "harvest")]),
    (
        "kernels/lite.json",
        &[("tile", "tile"), ("check", "check"), ("count30", "count30"), ("largeLite", "large_lite"), ("harvestLite", "harvest_lite")],
    ),
];
/// the dashboard reads (/, /board, /api/live): the CDN keeps each a snapshot period, may
/// serve it stale while it fetches the next, and for a day when this server fails
const EDGE_CACHE: &str = "public, max-age=0, s-maxage=15, stale-while-revalidate=15, stale-if-error=86400";
/// static files no page or engine version depends on: kept an hour without a request
const LOOSE_FILES: &[&str] = &["favicon.svg", "favicon.ico", "og.png", "robots.txt", "llms.txt", "llms-full.txt", "auth.md", "openapi.json", "goldbach-skill.md"];
const LOOSE_CACHE: &str = "public, max-age=3600";
const CSP: &str = concat!(
    "default-src 'self'; script-src 'self' blob: 'wasm-unsafe-eval' https://static.cloudflareinsights.com; ",
    "worker-src 'self' blob:; connect-src 'self' https://cloudflareinsights.com; style-src 'self' 'unsafe-inline'; img-src 'self' data:; ",
    "object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
);
/// Cloudflare edge ranges (https://www.cloudflare.com/ips/, 2026-10-18): CF-Connecting-IP is trusted only from these.
const CLOUDFLARE_NETS: &[&str] = &[
    "173.245.48.0/20", "103.21.244.0/22", "103.22.200.0/22", "103.31.4.0/22", "141.101.64.0/18", "108.162.192.0/18",
    "190.93.240.0/20", "188.114.96.0/20", "197.234.240.0/22", "198.41.128.0/17", "162.158.0.0/15", "104.16.0.0/13",
    "104.24.0.0/14", "172.64.0.0/13", "131.0.72.0/22", "2400:cb00::/32", "2606:4700::/32", "2803:f800::/32",
    "2405:b500::/32", "2405:8100::/32", "2a06:98c0::/29", "2c0f:f248::/32",
];

/// This host's outgoing address (a UDP connect sends nothing: it only picks the route).
fn own_address() -> Option<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    Some(s.local_addr().ok()?.ip().to_string())
}

/// The server's own CPU verifier talks to the coordinator directly on the docker bridge.
pub static LOCAL_IPS: LazyLock<HashSet<String>> = LazyLock::new(|| match std::env::var("GOLDBACH_LOCAL_IPS") {
    Ok(v) => v.split(',').map(String::from).collect(),
    Err(_) => {
        let mut s: HashSet<String> = ["172.18.0.1", "127.0.0.1"].iter().map(|x| x.to_string()).collect();
        if let Some(a) = own_address() {
            s.insert(a);
        }
        s
    }
});

fn mcp_tools() -> Value {
    json!([
        {"name": "goldbach_board", "title": "Status board",
         "description": "The live status of the verification of Goldbach's conjecture from 4e18 to 5e18 as a plain-text board: progress, speed, time left, the top 30 ridges (largest minimal Goldbach partitions p(n)), the p(n) histogram, devices and the latest native slices.",
         "inputSchema": {"type": "object", "properties": {"narrow": {"type": "boolean", "description": "44 columns instead of 76"}},
                         "additionalProperties": false},
         "annotations": {"readOnlyHint": true, "openWorldHint": false}},
        {"name": "goldbach_stats", "title": "Statistics",
         "description": "The verification's statistics as JSON: range, native GPU verifier and browser progress, prime and twin-prime counts, record ridges, devices. Big integers are strings.",
         "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
         "annotations": {"readOnlyHint": true, "openWorldHint": false}},
        {"name": "goldbach_paper_data", "title": "Live paper data",
         "description": "The data behind the live paper (goldbach.net01.io/paper), which recomputes the tables and figures of Oliveira e Silva, Herzog and Pardi (Math. Comp. 83, 2014) above 4e18: p(n) tail, prime gaps, backfill progress. JSON, about 40 KB.",
         "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
         "annotations": {"readOnlyHint": true, "openWorldHint": false}},
        {"name": "goldbach_partition", "title": "Minimal Goldbach partition",
         "description": "p(n), the smallest prime p with n - p also prime, and the partition n = p + q, for an even 4 <= n < 2^64, computed exactly on request (deterministic Miller-Rabin).",
         "inputSchema": {"type": "object", "properties": {"n": {"type": "string", "pattern": "^[0-9]+$",
                                                                "description": "The even number, in decimal"}},
                         "required": ["n"], "additionalProperties": false},
         "annotations": {"readOnlyHint": true, "openWorldHint": false}}
    ])
}

/// The visitor's address: cf_ip when peer is a Cloudflare edge, else peer.
fn client_ip(peer: &str, cf_ip: Option<&str>) -> String {
    if let Some(cf) = cf_ip.filter(|c| !c.is_empty()) {
        if let Ok(a) = peer.parse::<IpAddr>() {
            let inside = CLOUDFLARE_NETS.iter().any(|n| {
                let (net, bits) = n.split_once('/').unwrap();
                let bits: u32 = bits.parse().unwrap();
                match (a, net.parse::<IpAddr>().unwrap()) {
                    (IpAddr::V4(a), IpAddr::V4(n)) => {
                        let m = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
                        u32::from(a) & m == u32::from(n) & m
                    }
                    (IpAddr::V6(a), IpAddr::V6(n)) => {
                        let m = if bits == 0 { 0 } else { u128::MAX << (128 - bits) };
                        u128::from(a) & m == u128::from(n) & m
                    }
                    _ => false,
                }
            });
            if inside {
                if let Ok(c) = cf.trim().parse::<IpAddr>() {
                    return c.to_string();
                }
            }
        }
    }
    peer.to_string()
}

/// Token bucket per client IP for POSTs: `rate` requests per second, burst `burst`.
struct RateLimit {
    rate: f64,
    burst: f64,
    b: Mutex<HashMap<String, (f64, f64)>>,
}

impl RateLimit {
    fn new(rate: f64, burst: f64) -> Self {
        RateLimit { rate, burst, b: Mutex::new(HashMap::new()) }
    }

    fn allow(&self, ip: &str) -> bool {
        let t = now();
        let mut b = self.b.lock().unwrap_or_else(|e| e.into_inner());
        let (tok, t0) = b.get(ip).copied().unwrap_or((self.burst, t));
        let tok = self.burst.min(tok + (t - t0) * self.rate);
        if b.len() > 50000 {
            b.clear();
        }
        if tok < 1.0 {
            b.insert(ip.to_string(), (tok, t));
            return false;
        }
        b.insert(ip.to_string(), (tok - 1.0, t));
        true
    }
}

/// A static file, its gzip form (compressed once, not per request) and its hash.
struct File {
    key: (i128, u64, String),
    raw: Vec<u8>,
    gz: Option<Vec<u8>>,
    hash: String,
    etag: String,
}

/// A response body built and gzipped once per snapshot (gz: when over 1024 bytes,
/// where send() would compress it).
struct Encoded {
    raw: Vec<u8>,
    gz: Option<Vec<u8>>,
    etag: String,
}

/// The encoded bodies of the current snapshot, by request.
#[derive(Default)]
struct BodyCache {
    built: f64,
    bodies: HashMap<String, Arc<Encoded>>,
}

struct App {
    store: Arc<Store>,
    bodies: Mutex<BodyCache>,
    limiter: RateLimit,
    registers: RateLimit,
    static_dir: String,
    kernel_dir: String,
    admin_token: String,
    version_file: String,
    files: Mutex<HashMap<String, Arc<File>>>,
}

/// One HTTP response in the making.
struct Resp {
    code: u16,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
}

/// What the route needs of the request.
struct Req {
    method: Method,
    target: String,
    path: String,
    query: Option<String>,
    headers: HeaderMap,
    peer: String,
    body: Option<Vec<u8>>,
}

impl Req {
    fn header(&self, k: &str) -> Option<&str> {
        self.headers.get(k).and_then(|v| v.to_str().ok())
    }

    /// If-None-Match names this ETag (weak or strong: the CDN forwards them as W/).
    fn if_none_match(&self, etag: &str) -> bool {
        self.header("if-none-match").unwrap_or("").split(',').any(|t| {
            let t = t.trim();
            t.strip_prefix("W/").unwrap_or(t) == etag
        })
    }

    fn accepts_gzip(&self) -> bool {
        self.header("accept-encoding").unwrap_or("").contains("gzip")
    }

    fn ip(&self) -> String {
        let xff = self.header("x-forwarded-for").filter(|s| !s.is_empty()).unwrap_or(&self.peer);
        let peer = xff.rsplit(',').next().unwrap_or("").trim();
        client_ip(peer, self.header("cf-connecting-ip"))
    }
}

fn unquote(s: &str) -> String {
    let s = s.replace('+', " ");
    let b = s.as_bytes();
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// urllib.parse.parse_qs: blank values and parts without '=' are left out.
fn parse_qs(q: &str) -> HashMap<String, Vec<String>> {
    let mut m: HashMap<String, Vec<String>> = HashMap::new();
    for part in q.split('&') {
        if let Some((k, v)) = part.split_once('=') {
            if !v.is_empty() {
                m.entry(unquote(k)).or_default().push(unquote(v));
            }
        }
    }
    m
}

/// A request handler failed (Python: an exception, the connection dropped).
enum Fail {
    Db(rusqlite::Error),
    Py(py::PyErr),
}

impl From<rusqlite::Error> for Fail {
    fn from(e: rusqlite::Error) -> Self {
        Fail::Db(e)
    }
}

impl From<py::PyErr> for Fail {
    fn from(e: py::PyErr) -> Self {
        Fail::Py(e)
    }
}

type HR = Result<Resp, Fail>;

fn pyint(v: Option<&Value>, default: i128) -> Result<i128, py::PyErr> {
    v.map(py::int).unwrap_or(Ok(default))
}

fn to_i64(x: i128) -> Result<i64, py::PyErr> {
    i64::try_from(x).map_err(|_| "Python int too large to convert to SQLite INTEGER".to_string())
}

/// A 304 for an unchanged edge-cached reply (server.py: ETag, Cache-Control, Vary only).
fn not_modified(etag: String, cache: &str, vary: Option<&str>) -> Resp {
    Resp {
        code: 304,
        headers: vec![("ETag", etag), ("Cache-Control", cache.into()), ("Vary", vary.unwrap_or("Accept-Encoding").into())],
        body: vec![],
    }
}

fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut d = (a.len() != b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        d |= a.get(i).unwrap_or(&0) ^ b.get(i).unwrap_or(&0);
    }
    d == 0
}

impl App {
    fn send(&self, rq: &Req, code: u16, body: Vec<u8>, ctype: &str, gz: bool, cache: &str, etag: Option<&str>, vary: Option<&str>,
            extra: Vec<(&'static str, String)>) -> Resp {
        let t = Instant::now();
        // an edge-cached reply carries its body's hash: a revalidation of an unchanged one is a 304
        let mut etag = etag.map(String::from);
        if cache == EDGE_CACHE && code == 200 && etag.is_none() && !gz {
            let e = format!("\"{}\"", &py::sha256_hex(&body)[..32]);
            if rq.if_none_match(&e) {
                timing::timed("ser", t.elapsed().as_secs_f64());
                return not_modified(e, cache, vary);
            }
            etag = Some(e);
        }
        let (body, gz) = if !gz && body.len() > 1024 && rq.accepts_gzip() { (gzip(&body, 6), true) } else { (body, gz) };
        timing::timed("ser", t.elapsed().as_secs_f64());
        let vary = vary.map(String::from).or_else(|| gz.then(|| "Accept-Encoding".to_string()));
        let mut h: Vec<(&'static str, String)> = vec![
            ("Content-Type", ctype.into()),
            ("Content-Length", body.len().to_string()),
            ("Cache-Control", cache.into()),
        ];
        if let Some(e) = etag {
            h.push(("ETag", e));
        }
        h.push(("X-Content-Type-Options", "nosniff".into()));
        h.push(("Referrer-Policy", "no-referrer".into()));
        h.push(("Strict-Transport-Security", "max-age=31536000".into()));
        h.push(("X-Frame-Options", "DENY".into()));
        h.push(("Permissions-Policy", "camera=(), microphone=(), geolocation=()".into()));
        if ctype.starts_with("text/html") {
            h.push(("Content-Security-Policy", CSP.into()));
        }
        if gz {
            h.push(("Content-Encoding", "gzip".into()));
        }
        if let Some(v) = vary {
            h.push(("Vary", v));
        }
        h.extend(extra);
        if let Some(st) = timing::header() {
            h.push(("Server-Timing", st));
        }
        Resp { code, headers: h, body }
    }

    fn json(&self, rq: &Req, code: u16, v: &Value) -> Resp {
        self.send(rq, code, v.to_string().into_bytes(), "application/json", false, "no-store", None, None, vec![])
    }

    fn json_cached(&self, rq: &Req, v: &Value, ctype: &str, extra: Vec<(&'static str, String)>) -> Resp {
        self.send(rq, 200, v.to_string().into_bytes(), ctype, false, EDGE_CACHE, None, None, extra)
    }

    /// The body for `key` on snapshot `built`: from the cache, or built (and gzipped) now.
    fn cached(&self, built: f64, key: String, build: impl FnOnce() -> Vec<u8>) -> Arc<Encoded> {
        {
            let mut c = self.bodies.lock().unwrap();
            if c.built != built || c.bodies.len() > 512 {
                c.built = built;
                c.bodies.clear();
            }
            if let Some(e) = c.bodies.get(&key) {
                return e.clone();
            }
        }
        let t = Instant::now();
        let raw = build();
        let gz = (raw.len() > 1024).then(|| gzip(&raw, 6));
        let etag = format!("\"{}\"", &py::sha256_hex(&raw)[..32]);
        timing::timed("ser", t.elapsed().as_secs_f64());
        let e = Arc::new(Encoded { raw, gz, etag });
        let mut c = self.bodies.lock().unwrap();
        if c.built == built {
            c.bodies.insert(key, e.clone());
        }
        e
    }

    fn send_encoded(&self, rq: &Req, e: &Encoded, ctype: &str, vary: Option<&str>, extra: Vec<(&'static str, String)>) -> Resp {
        if rq.if_none_match(&e.etag) {
            return not_modified(e.etag.clone(), EDGE_CACHE, vary);
        }
        match (&e.gz, rq.accepts_gzip()) {
            (Some(gz), true) => self.send(rq, 200, gz.clone(), ctype, true, EDGE_CACHE, Some(&e.etag), vary, extra),
            _ => self.send(rq, 200, e.raw.clone(), ctype, false, EDGE_CACHE, Some(&e.etag), vary, extra),
        }
    }

    fn static_file(&self, name: &str) -> std::io::Result<Arc<File>> {
        let fp = match name.strip_prefix("kernels/") {
            Some(k) => format!("{}/{k}", self.kernel_dir),
            None => format!("{}/{name}", self.static_dir),
        };
        let md = std::fs::metadata(&fp)?;
        let mtime = md.modified()?.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as i128).unwrap_or(0);
        let key = (mtime, md.len(), String::new());
        if let Some(f) = self.files.lock().unwrap().get(name).filter(|f| f.key == key) {
            return Ok(f.clone());
        }
        let raw = std::fs::read(&fp)?;
        let gz = gzip(&raw, 9);
        let h = py::sha256_hex(&raw);
        let f = Arc::new(File {
            key,
            gz: (gz.len() + 64 < raw.len()).then_some(gz),
            raw,
            hash: h[..16].into(),
            etag: format!("\"{}\"", &h[..32]),
        });
        self.files.lock().unwrap().insert(name.into(), f.clone());
        Ok(f)
    }

    /// kernels/full.json or kernels/lite.json: the WGSL sources one engine mode compiles.
    fn kernel_pack(&self, name: &str) -> std::io::Result<Arc<File>> {
        let pack = KERNEL_PACKS.iter().find(|p| p.0 == name).unwrap().1;
        let parts: Vec<(&str, Arc<File>)> =
            pack.iter().map(|(k, f)| Ok((*k, self.static_file(&format!("kernels/{f}.wgsl"))?))).collect::<std::io::Result<_>>()?;
        let key = (0, 0, parts.iter().map(|p| p.1.hash.as_str()).collect::<Vec<_>>().join(","));
        if let Some(f) = self.files.lock().unwrap().get(name).filter(|f| f.key == key) {
            return Ok(f.clone());
        }
        let mut m = Map::new();
        for (k, f) in &parts {
            m.insert(k.to_string(), Value::String(String::from_utf8_lossy(&f.raw).into_owned()));
        }
        let raw = Value::Object(m).to_string().into_bytes();
        let h = py::sha256_hex(&raw);
        let f = Arc::new(File { key, gz: Some(gzip(&raw, 9)), raw, hash: h[..16].into(), etag: format!("\"{}\"", &h[..32]) });
        self.files.lock().unwrap().insert(name.into(), f.clone());
        Ok(f)
    }

    /// Hashes of the engine files and of the page (pages follow updates live).
    fn versions(&self) -> Value {
        let digest = |names: &[&str]| {
            use sha2::Digest;
            let mut h = sha2::Sha256::new();
            for n in names {
                h.update(n.as_bytes());
                match self.static_file(n) {
                    Ok(f) => {
                        h.update(b"\0");
                        h.update(f.hash.as_bytes());
                    }
                    Err(_) => h.update(b"\0missing"),
                }
            }
            let d = h.finalize();
            d.iter().take(8).map(|b| format!("{b:02x}")).collect::<String>()
        };
        json!({"engine": digest(ENGINE_FILES), "page": digest(PAGE_FILES)})
    }

    fn deployed_version(&self) -> String {
        std::fs::read_to_string(&self.version_file)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| v.get("commit").map(py::str_of))
            .unwrap_or_else(|| "dev".into())
    }

    fn route(&self, rq: &Req) -> HR {
        match rq.method {
            Method::GET | Method::HEAD => self.get(rq),
            Method::POST => self.post(rq),
            _ => Ok(Resp {
                code: 501,
                headers: vec![("Content-Type", "text/plain".into()), ("Content-Length", "21".into())],
                body: b"Unsupported method\r\n\r\n"[..21].to_vec(),
            }),
        }
    }

    fn get(&self, rq: &Req) -> HR {
        let path = rq.path.as_str();
        let st = &self.store;
        match path {
            "/api/stats" => {
                let s = st.snap();
                return Ok(if rq.accepts_gzip() {
                    self.send(rq, 200, s.body_gz.clone(), "application/json", true, "no-store", None, None, vec![])
                } else {
                    self.send(rq, 200, s.body.clone(), "application/json", false, "no-store", None, None, vec![])
                });
            }
            "/board" => {
                let narrow = rq.query.as_deref().unwrap_or("").split('&').any(|p| p == "narrow=1");
                let b = st.snap().boards[narrow as usize].clone().into_bytes();
                return Ok(self.send(rq, 200, b, "text/plain; charset=utf-8", false, EDGE_CACHE, None, None, vec![]));
            }
            "/api/health" => return Ok(self.json(rq, 200, &json!({"ok": true}))),
            "/api/version" => return Ok(self.json(rq, 200, &self.versions())),
            "/api/live" => {
                let q = parse_qs(rq.query.as_deref().unwrap_or(""));
                let arg = |k: &str| q.get(k).and_then(|v| v.first()).map(String::as_str);
                let parsed = (|| -> Result<_, py::PyErr> {
                    let since = match arg("since") {
                        Some(s) => py::float(&Value::String(s.into()))?,
                        None => 0.0,
                    };
                    let runs = arg("runs").map(|s| py::float(&Value::String(s.into()))).transpose()?;
                    let ints = |k: &str, d: &[i64]| -> Result<Vec<i64>, py::PyErr> {
                        match arg(k) {
                            Some(s) => s
                                .split(',')
                                .filter(|x| !x.is_empty())
                                .map(|x| py::int(&Value::String(x.into())).map(|v| v as i64))
                                .collect(),
                            None => Ok(d.to_vec()),
                        }
                    };
                    Ok((since, runs, ints("tiers", &[10, 300, 3600])?, ints("full", &[])?))
                })();
                let Ok((since, runs, tiers, full)) = parsed else {
                    return Ok(self.json(rq, 400, &json!({"error": "bad query"})));
                };
                let (snap, v) = (st.snap(), self.versions());
                let narrow = arg("narrow") == Some("1");
                let key = format!("live {narrow} {} {:?} {:?} {tiers:?} {full:?} {v}", since.to_bits(), runs.map(f64::to_bits), arg("bh"));
                let e = self.cached(snap.built, key, || {
                    let mut out = store::live_of(&snap, narrow, since, runs, arg("bh"), &tiers, &full);
                    out.insert("v".into(), v);
                    Value::Object(out).to_string().into_bytes()
                });
                return Ok(self.send_encoded(rq, &e, "application/json", None, vec![]));
            }
            "/" | "" | "/index.html" => return self.page(rq),
            "/api/results" => {
                let r = self.results()?;
                return Ok(self.send(rq, 200, r.into_bytes(), "application/json", false, EDGE_CACHE, None, None, vec![]));
            }
            "/api/partition" => {
                let q = parse_qs(rq.query.as_deref().unwrap_or(""));
                let out = self.partition_answer(q.get("n").and_then(|v| v.first()).map(String::as_str).unwrap_or(""));
                let code = if out.get("error").is_some() { 400 } else { 200 };
                return Ok(self.send(rq, code, out.to_string().into_bytes(), "application/json", false, EDGE_CACHE, None, None, vec![]));
            }
            "/paper" => return self.paper_page(rq),
            "/paper/" => {
                return Ok(Resp { code: 301, headers: vec![("Location", "/paper".into()), ("Content-Length", "0".into())], body: vec![] });
            }
            "/sitemap.xml" => {
                let (web, native) = {
                    let db = st.lock();
                    let native = db.meta_json("native", "{}")?;
                    let w = db.q("SELECT MAX(verified_at) FROM slices WHERE state='verified'", &[])?;
                    (w.first().and_then(|r| match &r[0] { rusqlite::types::Value::Null => None, v => Some(store::vf(v)) }), native)
                };
                let s = board::sitemap(web, &native);
                return Ok(self.send(rq, 200, s.into_bytes(), "application/xml; charset=utf-8", false, EDGE_CACHE, None, None, vec![]));
            }
            "/.well-known/api-catalog" => {
                return Ok(self.json_cached(rq, &self.api_catalog(), "application/linkset+json",
                                           vec![("Link", "</.well-known/api-catalog>; rel=\"api-catalog\"".into())]));
            }
            "/.well-known/agent-skills/index.json" => {
                let skill = self.static_file("goldbach-skill.md").map(|f| f.raw.clone()).unwrap_or_default();
                let v = json!({"$schema": "https://schemas.agentskills.io/discovery/0.2.0/schema.json", "skills": [{
                    "name": "goldbach", "type": "skill-md", "url": SKILL_URL,
                    "description": "Read the live state and statistics of the Goldbach conjecture verification from 4e18 to 5e18.",
                    "digest": format!("sha256:{}", py::sha256_hex(&skill))}]});
                return Ok(self.json_cached(rq, &v, "application/json", vec![]));
            }
            "/.well-known/mcp/server-card.json" => return Ok(self.json_cached(rq, &self.mcp_card(), "application/json", vec![])),
            "/mcp" => {
                let v = json!({"error": "MCP Streamable HTTP: POST JSON-RPC messages here"});
                return Ok(self.send(rq, 405, v.to_string().into_bytes(), "application/json", false, "no-store", None, None,
                                    vec![("Allow", "POST".into())]));
            }
            _ => {}
        }
        let name = ALIASES.iter().find(|a| a.0 == path).map(|a| a.1).unwrap_or_else(|| path.trim_start_matches('/'));
        let is_pack = KERNEL_PACKS.iter().any(|p| p.0 == name);
        if !STATIC_FILES.contains(&name) && !is_pack {
            return Ok(self.json(rq, 404, &json!({"error": "not found"})));
        }
        let Ok(f) = (if is_pack { self.kernel_pack(name) } else { self.static_file(name) }) else {
            return Ok(self.json(rq, 404, &json!({"error": "not found"})));
        };
        let ext = name.rsplit('.').next().unwrap_or("");
        let ctype = match ext {
            "wgsl" | "txt" => "text/plain; charset=utf-8",
            "wasm" => "application/wasm",
            "js" => "text/javascript; charset=utf-8",
            "json" => "application/json",
            "xml" => "application/xml; charset=utf-8",
            "html" => "text/html; charset=utf-8",
            "svg" => "image/svg+xml",
            "ico" => "image/x-icon",
            "md" => "text/markdown; charset=utf-8",
            "png" => "image/png",
            _ => "application/octet-stream",
        };
        // a URL naming the file's current version is cached for good; any other is revalidated
        let v = rq.query.as_deref().map(parse_qs).and_then(|q| q.get("v").and_then(|v| v.first().cloned())).unwrap_or_default();
        let fresh = !v.is_empty()
            && (v == f.hash || ((ENGINE_FILES.contains(&name) || is_pack) && Some(v.as_str()) == self.versions()["engine"].as_str()));
        let cache = if fresh {
            "public, max-age=31536000, immutable"
        } else if LOOSE_FILES.contains(&name) {
            LOOSE_CACHE
        } else {
            "no-cache"
        };
        if rq.if_none_match(&f.etag) {
            return Ok(Resp {
                code: 304,
                headers: vec![("ETag", f.etag.clone()), ("Cache-Control", cache.into()), ("Vary", "Accept-Encoding".into())],
                body: vec![],
            });
        }
        let gz = f.gz.is_some() && rq.accepts_gzip();
        let body = if gz { f.gz.clone().unwrap() } else { f.raw.clone() };
        Ok(self.send(rq, 200, body, ctype, gz, cache, Some(&f.etag), Some("Accept-Encoding"), vec![]))
    }

    /// index.html with the board, the page's first live data and versioned URLs filled in.
    fn page(&self, rq: &Req) -> HR {
        let (Ok(page), Ok(app)) = (self.static_file("index.html"), self.static_file("app.js")) else {
            return Ok(self.json(rq, 404, &json!({"error": "not found"})));
        };
        let (snap, v) = (self.store.snap(), self.versions());
        let render = || {
            let mut live = store::live_of(&snap, false, 0.0, None, None, &[], &[]); // the page has no speed chart
            live.insert("v".into(), v.clone());
            live.insert("narrow".into(), json!(false));
            let nb = store::live_of(&snap, true, 0.0, None, None, &[], &[]);
            live.insert("nb".into(), nb["board"].clone());
            live.insert("nbh".into(), nb["bh"].clone());
            let board = live.shift_remove("board").map(|b| py::str_of(&b)).unwrap_or_default();
            let esc = board.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
            String::from_utf8_lossy(&page.raw)
                .replace("<!--BOARD-->", &esc)
                .replace("<!--LIVE-->", &Value::Object(live).to_string().replace("</", "<\\/"))
                .replace("src=\"app.js\"", &format!("src=\"app.js?v={}\"", app.hash))
        };
        if board::wants_markdown(rq.header("accept").unwrap_or("")) {
            return Ok(self.send_page(rq, render()));
        }
        // the HTML is the same for every viewer of a snapshot: rendered and gzipped once
        let e = self.cached(snap.built, format!("page {v} {} {}", page.hash, app.hash), || render().into_bytes());
        Ok(self.send_encoded(rq, &e, "text/html; charset=utf-8", Some("Accept, Accept-Encoding"), self.page_links(rq)))
    }

    fn paper_page(&self, rq: &Req) -> HR {
        let (Ok(page), Ok(js), Ok(data)) = (self.static_file("paper.html"), self.static_file("paper.js"), self.static_file("paper.json"))
        else {
            return Ok(self.json(rq, 404, &json!({"error": "not found"})));
        };
        let page = String::from_utf8_lossy(&page.raw)
            .replace("src=\"paper.js\"", &format!("src=\"paper.js?v={}\"", js.hash))
            .replace("\"paper.json\"", &format!("\"paper.json?v={}\"", data.hash));
        Ok(self.send_page(rq, page))
    }

    /// A public page as HTML, or as Markdown when the request asks for it.
    fn page_links(&self, rq: &Req) -> Vec<(&'static str, String)> {
        vec![
            ("Link", LINKS.to_string()),
            ("Link", format!("<{}>; rel=\"alternate\"; type=\"text/markdown\"", rq.path)),
            ("Content-Signal", CONTENT_SIGNAL.to_string()),
        ]
    }

    fn send_page(&self, rq: &Req, page: String) -> Resp {
        let links = self.page_links(rq);
        if !board::wants_markdown(rq.header("accept").unwrap_or("")) {
            return self.send(rq, 200, page.into_bytes(), "text/html; charset=utf-8", false, EDGE_CACHE, None,
                             Some("Accept, Accept-Encoding"), links);
        }
        let md = board::markdown(&page);
        let mut h = links;
        h.push(("X-Markdown-Tokens", (md.chars().count() / 4).to_string())); // an estimate: ~4 characters a token
        self.send(rq, 200, md.into_bytes(), "text/markdown; charset=utf-8", false, "private, no-store",
                  None, Some("Accept, Accept-Encoding"), h)
    }

    /// /api/results: the live paper's native data and the browsers' prime gaps.
    fn results(&self) -> R<String> {
        let (native, paper) = {
            let db = self.store.lock();
            (db.meta("native")?.unwrap_or_else(|| "{}".into()), db.meta("paper")?.unwrap_or_else(|| "{}".into()))
        };
        let nat: Value = serde_json::from_str(&native).unwrap_or(Value::Null);
        let bf = nat.get("progress").and_then(|p| p.get("backfill")).cloned().unwrap_or(Value::Null);
        let tail = nat.get("tail").cloned().unwrap_or(json!([]));
        let gaps = self.store.lock().web_gaps()?;
        Ok(format!("{{\"time\":{},\"tail\":{tail},\"backfill\":{bf},\"paper\":{paper},\"webGaps\":{gaps}}}", py::float_repr(now())))
    }

    fn api_catalog(&self) -> Value {
        let doc = json!({"href": SKILL_URL, "type": "text/markdown"});
        let status = json!([{"href": format!("{SITE}/api/health"), "type": "application/json"}]);
        json!({"linkset": [
            {"anchor": format!("{SITE}/api/"), "service-desc": [{"href": format!("{SITE}/openapi.json"), "type": "application/openapi+json"}],
             "service-doc": [doc], "status": status},
            {"anchor": format!("{SITE}/mcp"), "service-desc": [{"href": format!("{SITE}/.well-known/mcp/server-card.json"), "type": "application/json"}],
             "service-doc": [doc], "status": status},
        ]})
    }

    fn mcp_card(&self) -> Value {
        let ver = self.deployed_version();
        let info = json!({"name": "goldbach-verification", "title": "Goldbach Verification Network", "version": ver});
        let tools: Vec<Value> = mcp_tools()
            .as_array()
            .unwrap()
            .iter()
            .map(|t| json!({"name": t["name"], "title": t["title"], "description": t["description"]}))
            .collect();
        json!({"serverInfo": info, "name": "io.net01.goldbach/verification", "title": "Goldbach Verification Network", "version": ver,
               "description": "Live data of a distributed verification of Goldbach's conjecture from 4e18 to 5e18, and exact minimal Goldbach partitions on request. Read-only, no authentication.",
               "websiteUrl": format!("{SITE}/"), "protocolVersion": MCP_VERSIONS[1],
               "endpoint": format!("{SITE}/mcp"), "transport": {"type": "streamable-http", "endpoint": format!("{SITE}/mcp")},
               "remotes": [{"type": "streamable-http", "url": format!("{SITE}/mcp"), "supportedProtocolVersions": MCP_VERSIONS,
                            "authentication": {"required": false}}],
               "capabilities": {"tools": {"listChanged": false}},
               "tools": tools})
    }

    /// MCP over Streamable HTTP, stateless: one JSON-RPC message per POST.
    fn mcp(&self, rq: &Req, msg: &Value) -> HR {
        let Some(m) = msg.as_object().filter(|m| m.get("jsonrpc") == Some(&json!("2.0"))) else {
            return Ok(self.json(rq, 400, &json!({"jsonrpc": "2.0", "id": null,
                                               "error": {"code": -32600, "message": "one JSON-RPC 2.0 message per POST"}})));
        };
        if !m.contains_key("id") || !m.contains_key("method") {
            return Ok(self.send(rq, 202, vec![], "application/json", false, "no-store", None, None, vec![]));
        }
        let (mid, method) = (m["id"].clone(), m["method"].clone());
        let params = m.get("params").filter(|p| py::truthy(p) && p.is_object()).cloned().unwrap_or(json!({}));
        let ok = |result: Value| self.json(rq, 200, &json!({"jsonrpc": "2.0", "id": mid, "result": result}));
        let err = |code: i64, why: String| self.json(rq, 200, &json!({"jsonrpc": "2.0", "id": mid, "error": {"code": code, "message": why}}));
        let method_s = method.as_str().unwrap_or("");
        if method_s == "initialize" {
            let want = params.get("protocolVersion").and_then(Value::as_str).filter(|w| MCP_VERSIONS.contains(w));
            return Ok(ok(json!({"protocolVersion": want.unwrap_or(MCP_VERSIONS[0]),
                                "capabilities": {"tools": {"listChanged": false}},
                                "serverInfo": {"name": "goldbach-verification", "title": "Goldbach Verification Network",
                                               "version": self.deployed_version()},
                                "instructions": "Live data of the verification of Goldbach's conjecture from 4e18 to 5e18 (goldbach.net01.io). Start with goldbach_board. Data refreshes every ~15 s."})));
        }
        if method_s == "ping" {
            return Ok(ok(json!({})));
        }
        if method_s == "tools/list" {
            return Ok(ok(json!({"tools": mcp_tools()})));
        }
        if method_s != "tools/call" {
            return Ok(err(-32601, format!("method not found: {}", py::str_of(&method))));
        }
        let name = params.get("name").cloned().unwrap_or(Value::Null);
        let args = params.get("arguments").filter(|a| py::truthy(a) && a.is_object()).cloned().unwrap_or(json!({}));
        let text = |t: String, data: Option<Value>| {
            let mut r = json!({"content": [{"type": "text", "text": t}], "isError": false});
            if let Some(d) = data {
                r["structuredContent"] = d;
            }
            ok(r)
        };
        match name.as_str().unwrap_or("") {
            "goldbach_board" => {
                let narrow = args.get("narrow").is_some_and(py::truthy);
                Ok(text(self.store.snap().boards[narrow as usize].clone(), None))
            }
            "goldbach_stats" => {
                let mut st: Value = self.store.snap().obj.clone();
                for big in ["hist", "nativeHist", "speedHistory", "serverRuns"] {
                    st.as_object_mut().map(|o| o.shift_remove(big));
                }
                Ok(text(st.to_string(), Some(st)))
            }
            "goldbach_paper_data" => Ok(text(self.results()?, None)),
            "goldbach_partition" => {
                let a = self.partition_answer(&py::str_of(args.get("n").unwrap_or(&json!(""))));
                if let Some(e) = a.get("error") {
                    return Ok(ok(json!({"content": [{"type": "text", "text": e}], "isError": true})));
                }
                Ok(text(format!("p({}) = {}: {}", py::str_of(&a["n"]), py::str_of(&a["p"]), py::str_of(&a["statement"])), Some(a)))
            }
            _ => Ok(err(-32602, format!("unknown tool: {}", py::str_of(&name)))),
        }
    }

    /// p(n) for an even 4 <= n < 2^64, computed exactly, with whether this
    /// verification has covered n (the native verifier's contiguous prefix).
    fn partition_answer(&self, n: &str) -> Value {
        let n = n.trim();
        let v: Option<i128> = (!n.is_empty() && n.bytes().all(|c| c.is_ascii_digit())).then(|| n.parse().ok()).flatten();
        let Some(n) = v.filter(|&v| v % 2 == 0 && (4..1i128 << 64).contains(&v)) else {
            return json!({"error": "n must be an even integer, 4 <= n < 2^64, in decimal"});
        };
        let p = validate::min_partition(n);
        let resume: i128 = self.store.snap().obj["native"]["resume"]
            .as_str()
            .and_then(|r| r.trim().parse().ok())
            .unwrap_or(RANGE_LO);
        json!({"n": n.to_string(), "p": py::num(p), "q": (n - p).to_string(), "isSum": true,
               "statement": format!("{n} = {p} + {}, both prime; {p} is the smallest prime that works.", n - p),
               "inRange": (RANGE_LO..=RANGE_HI).contains(&n), "verifiedByNative": (RANGE_LO..resume).contains(&n),
               "nativeResume": resume.to_string(), "source": format!("{SITE}/api/partition?n={n}")})
    }

    fn post(&self, rq: &Req) -> HR {
        let path = rq.path.as_str();
        let ip = rq.ip();
        if !self.limiter.allow(&ip) {
            return Ok(self.json(rq, 429, &json!({"error": "slow down"})));
        }
        let req: Value = match rq.body.as_deref().map(serde_json::from_slice::<Value>) {
            Some(Ok(v)) => v,
            _ => return Ok(self.json(rq, 400, &json!({"error": "bad request"}))),
        };
        if path == "/mcp" {
            return self.mcp(rq, &req);
        }
        if !req.is_object() {
            return Err(Fail::Py("'list' object has no attribute 'get'".into()));
        }
        let g = |k: &str| req.get(k);
        let st = &self.store;
        if path == "/api/register" {
            if st.lock().net_banned(&ip)? {
                return Ok(self.json(rq, 403, &json!({"error": "banned"})));
            }
            if !self.registers.allow(&ip) {
                return Ok(self.json(rq, 429, &json!({"error": "too many new devices from this address"})));
            }
            let name = g("name").map(py::str_of).unwrap_or_else(|| "device".into());
            let gpu = g("gpu").map(py::str_of).unwrap_or_else(|| "?".into());
            let cid = st.write(|db| db.register(&name, &gpu, &ip))?;
            return Ok(self.json(rq, 200, &json!({"client": cid})));
        }
        let token_ok = || ct_eq(&g("token").map(py::str_of).unwrap_or_default(), &self.admin_token);
        if path == "/api/native" {
            if !token_ok() {
                return Ok(self.json(rq, 403, &json!({"error": "forbidden"})));
            }
            let mut data = match g("data") {
                None => Map::new(),
                Some(Value::Object(o)) => o.clone(),
                Some(_) => return Err(Fail::Py("dictionary update sequence element #0 has length 1; 2 is required".into())),
            };
            let floor = g("floor").map(py::int).transpose()?;
            st.write(|db| {
                if let Some(p) = data.shift_remove("paper") {
                    db.set_meta("paper", &p.to_string())?;
                }
                db.set_meta("native", &Value::Object(data).to_string())?;
                if let Some(f) = floor {
                    db.set_meta("floor", &f.to_string())?;
                }
                db.invalidate();
                Ok(())
            })?;
            return Ok(self.json(rq, 200, &json!({"ok": true})));
        }
        if path == "/api/audit/next" || path == "/api/audit/result" {
            if !token_ok() {
                return Ok(self.json(rq, 403, &json!({"error": "forbidden"})));
            }
            if path == "/api/audit/next" {
                let r = st.lock().audit_next()?;
                return Ok(self.json(rq, 200, &r.unwrap_or(json!({"wait": 60}))));
            }
            let idx = pyint(g("slice"), -1)?;
            let r = st.write(|db| db.audit_result(idx, g("result").unwrap_or(&json!({}))))?;
            return Ok(self.json(rq, 200, &r));
        }
        if st.lock().net_banned(&ip)? {
            return Ok(self.json(rq, 403, &json!({"error": "banned"})));
        }
        let cid = g("client").map(py::str_of).unwrap_or_default();
        if !st.lock().client(&cid)? {
            return Ok(self.json(rq, 401, &json!({"error": "unknown or banned client"})));
        }
        match path {
            "/api/lease" => {
                let got = st.write(|db| db.lease(&cid, &ip, g("ahead").is_some_and(py::truthy)))?;
                Ok(self.json(rq, 200, &got.unwrap_or(json!({"wait": 300, "why": "nothing left above the floor"}))))
            }
            "/api/heartbeat" => {
                let lease = to_i64(pyint(g("lease"), 0)?)?;
                let done = pyint(g("done"), 0)?;
                let ok = st.write(|db| db.heartbeat(&cid, lease, done))?;
                Ok(self.json(rq, 200, &json!({"ok": ok})))
            }
            "/api/report" => {
                let lease = match g("lease").filter(|v| py::truthy(v)) {
                    Some(v) => to_i64(py::int(v)?)?,
                    None => 0,
                };
                let msg = py::chars(&g("error").map(py::str_of).unwrap_or_default(), 500);
                st.write(|db| db.report(&cid, lease, &msg))?;
                Ok(self.json(rq, 200, &json!({"ok": true})))
            }
            "/api/release" => {
                let lease = to_i64(pyint(g("lease"), 0)?)?;
                st.write(|db| db.release(&cid, lease))?;
                Ok(self.json(rq, 200, &json!({"ok": true})))
            }
            "/api/result" => {
                let lease = to_i64(pyint(g("lease"), 0)?)?;
                let elapsed = g("elapsed").map(py::float).transpose()?.unwrap_or(0.0);
                let empty = json!({});
                let r = st.write(|db| db.result(&cid, lease, g("result").unwrap_or(&empty), elapsed, g("version")))?;
                Ok(self.json(rq, 200, &r))
            }
            _ => Ok(self.json(rq, 404, &json!({"error": "not found"}))),
        }
    }
}

async fn handle(State(app): State<Arc<App>>, ConnectInfo(addr): ConnectInfo<SocketAddr>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let target = parts.uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or_else(|| "/".into());
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), Some(q.to_string())),
        None => (target.clone(), None),
    };
    // the body only when Content-Length announces 1 byte to MAX_BODY (else a 400, as server.py)
    let body = if parts.method == Method::POST {
        let n: i64 = parts.headers.get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok()).unwrap_or(0);
        if n > 0 && n as usize <= MAX_BODY {
            match to_bytes(body, MAX_BODY).await {
                Ok(b) => Some(b.to_vec()),
                Err(_) => None,
            }
        } else {
            None
        }
    } else {
        None
    };
    let peer = match addr.ip() {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(|v4| v4.to_string()).unwrap_or_else(|| v6.to_string()),
        v4 => v4.to_string(),
    };
    let rq = Req { method: parts.method, target, path, query, headers: parts.headers, peer, body };
    let resp = tokio::task::spawn_blocking(move || {
        timing::start();
        let r = app.route(&rq);
        timing::stop();
        (r, rq)
    })
    .await;
    let r = match resp {
        Ok((Ok(r), _)) => r,
        Ok((Err(e), rq)) => {
            match e {
                Fail::Db(e) => eprintln!("{} {}: database: {e}", rq.method, rq.target),
                Fail::Py(e) => eprintln!("{} {}: {e}", rq.method, rq.target),
            }
            let b = br#"{"error":"internal error"}"#.to_vec();
            Resp {
                code: 500,
                headers: vec![("Content-Type", "application/json".into()), ("Content-Length", b.len().to_string())],
                body: b,
            }
        }
        Err(e) => {
            eprintln!("handler panicked: {e}");
            Resp { code: 500, headers: vec![("Content-Length", "0".into())], body: vec![] }
        }
    };
    let mut out = Response::new(Body::from(r.body));
    *out.status_mut() = StatusCode::from_u16(r.code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let h = out.headers_mut();
    h.insert("server", HeaderValue::from_static("goldbach-web"));
    for (k, v) in r.headers {
        if let Ok(v) = HeaderValue::from_str(&v) {
            if let Ok(k) = HeaderName::from_bytes(k.as_bytes()) {
                h.append(k, v);
            }
        }
    }
    out
}

/// `goldbach-server dev <what>`: the pure functions on stdin -> stdout, so the
/// golden tests (tests/golden.rs) and the benchmark can drive them.
fn dev(args: &[String]) {
    use std::io::Read;
    if args.first().map(String::as_str) == Some("lease-summary") {
        // lease counts by status and stored results of a database (test/worker_e2e.mjs)
        let path = args.get(1).expect("lease-summary DB");
        let conn = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open database");
        let mut st = conn.prepare("SELECT status, COUNT(*) FROM leases GROUP BY status").expect("prepare");
        let leases: Vec<Value> = st
            .query_map([], |r| Ok(json!([r.get::<_, String>(0)?, r.get::<_, i64>(1)?])))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect();
        let results: i64 = conn.query_row("SELECT COUNT(*) FROM results", [], |r| r.get(0)).expect("count");
        println!("{}", json!({"leases": leases, "results": results}));
        return;
    }
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).expect("stdin");
    let arg = |i: usize| args.get(i).and_then(|s| s.parse::<i128>().ok()).unwrap_or(0);
    let out = match args.first().map(String::as_str) {
        Some("validate") => {
            let res: Value = serde_json::from_str(&input).expect("json");
            let (s, l) = (arg(1), arg(2));
            let canon = match validate::validate(&res, s, l) {
                Ok(c) => format!("OK {c}"),
                Err(w) => format!("REJECT {w}"),
            };
            let gaps = match validate::validate_gaps(&res, s, l) {
                Ok(Some(g)) => format!("OK {g}"),
                Ok(None) => "OK None".into(),
                Err(w) => format!("REJECT {w}"),
            };
            format!("{canon}\n{gaps}\n")
        }
        Some("board") => {
            let s: Value = serde_json::from_str(&input).expect("json");
            format!("{}\x0c{}", board::board(&s, false), board::board(&s, true))
        }
        Some("markdown") => board::markdown(&input),
        Some("wrap") => py::wrap(&input, arg(1) as usize, "").join("\n"),
        Some("rebuild") => {
            // the dashboard snapshot rebuild on a copy of a database: N times, ms each
            let st = Store::open(input.trim()).expect("database");
            let n = arg(1).max(1);
            (0..n).map(|_| {
                let t = Instant::now();
                st.rebuild().expect("rebuild");
                format!("{:.3}\n", t.elapsed().as_secs_f64() * 1000.0)
            }).collect()
        }
        Some("primes") => input.split_whitespace().map(|n| {
            let n: i128 = n.parse().unwrap_or(0);
            format!("{} {} {}\n", validate::is_prime(n) as u8, if n >= 4 && n % 2 == 0 { validate::min_partition(n) } else { 0 },
                    validate::boundary_counts(n).0 * 10 + validate::boundary_counts(n).1)
        }).collect(),
        _ => {
            eprintln!("dev validate START LIMIT | board | markdown | wrap WIDTH | primes   (input on stdin)");
            std::process::exit(2);
        }
    };
    print!("{out}");
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("dev") {
        return dev(&argv[2..]);
    }
    if argv.get(1).map(String::as_str) == Some("seed-demo-db") {
        // goldbach-server seed-demo-db STATS_JSON DB (see seed.rs)
        let (Some(stats), Some(db)) = (argv.get(2), argv.get(3)) else {
            eprintln!("usage: goldbach-server seed-demo-db STATS_JSON DB");
            std::process::exit(2);
        };
        match seed::seed(stats, db) {
            Ok(m) => println!("{m}"),
            Err(e) => {
                eprintln!("seed-demo-db: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    if argv.get(1).map(String::as_str) == Some("backup") {
        // goldbach-server backup DB OUT: a consistent copy of a live database (tools/server_update.sh)
        let (Some(src), Some(dst)) = (argv.get(2), argv.get(3)) else {
            eprintln!("usage: goldbach-server backup DB OUT");
            std::process::exit(2);
        };
        let conn = rusqlite::Connection::open_with_flags(src, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open database");
        conn.execute("VACUUM INTO ?1", [dst]).expect("backup");
        return;
    }
    let mut db = "goldbach.db".to_string();
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
    let mut static_dir = exe_dir.as_ref().map(|d| d.join("../../../client").display().to_string()).unwrap_or_else(|| "client".into());
    let mut bind = "127.0.0.1".to_string();
    let mut port: u16 = 8090;
    let mut version_file: Option<String> = None;
    let mut threads: usize = 0;
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--db" => db = v,
            "--static" => static_dir = v,
            "--bind" => bind = v,
            "--port" => port = v.parse().expect("--port"),
            "--version-file" => version_file = Some(v),
            "--threads" => threads = v.parse().expect("--threads"),
            a => {
                eprintln!("unknown argument {a}\nusage: goldbach-server [--db DB] [--static DIR] [--bind ADDR] [--port N] [--version-file F] [--threads N]");
                std::process::exit(2);
            }
        }
        i += 2;
    }
    let tok_path = format!("{db}.token");
    if !std::path::Path::new(&tok_path).exists() {
        let b: [u8; 24] = rand::random();
        std::fs::write(&tok_path, b.iter().map(|x| format!("{x:02x}")).collect::<String>()).expect("token file");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tok_path, std::fs::Permissions::from_mode(0o600)).expect("token file mode");
    }
    let abs = |p: &str| std::fs::canonicalize(p).map(|p| p.display().to_string()).unwrap_or_else(|_| p.to_string());
    let static_dir = abs(&static_dir);
    let kernel_dir = abs(&format!("{static_dir}/../kernels"));
    let version_file = version_file.unwrap_or_else(|| format!("{static_dir}/../server/VERSION.json"));
    let store = Store::open(&db).expect("database");
    store.start_snapshots();
    let app = Arc::new(App {
        store,
        limiter: RateLimit::new(2.0, 30.0),
        registers: RateLimit::new(1.0 / 60.0, 5.0),
        admin_token: std::fs::read_to_string(&tok_path).expect("token").trim().to_string(),
        static_dir,
        kernel_dir,
        version_file,
        files: Mutex::new(HashMap::new()),
        bodies: Mutex::new(BodyCache::default()),
    });
    let mut rt = tokio::runtime::Builder::new_multi_thread();
    // request handlers run on the blocking pool (the store is one SQLite
    // connection behind a lock: more threads only queue); 32 keeps the whole
    // process well inside goldbach-web.service's TasksMax=64
    rt.enable_all().max_blocking_threads(32);
    if threads > 0 {
        rt.worker_threads(threads);
    }
    rt.build().expect("runtime").block_on(async move {
        let listener = tokio::net::TcpListener::bind((bind.as_str(), port)).await.expect("bind");
        println!("goldbach-web (rust) on {bind}:{port}, static {}", app.static_dir);
        let router = axum::Router::new().fallback(handle).with_state(app);
        axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>()).await.expect("serve");
    });
}
