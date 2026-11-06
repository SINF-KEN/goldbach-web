//! Shared helpers of the integration tests: a coordinator process on a temp
//! database and a raw HTTP/1.1 client (Connection: close, no automatic
//! decompression, full control of the request headers).
#![allow(dead_code)]
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const SITE: &str = "https://goldbach.net01.io";
pub const PAGES: [&str; 2] = ["/", "/paper"];

pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Resp {
    pub fn header(&self, k: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str())
    }
    pub fn all(&self, k: &str) -> Vec<&str> {
        self.headers.iter().filter(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str()).collect()
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.text()))
    }
}

pub struct Server {
    pub dir: PathBuf,
    pub static_dir: PathBuf,
    pub db: PathBuf,
    pub port: u16,
    child: Option<Child>,
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let t = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &t);
        } else {
            std::fs::copy(e.path(), t).unwrap();
        }
    }
}

impl Server {
    /// A temp layout `<dir>/client` (a copy of ../client; core.js and core.wasm
    /// are build outputs, so a clean checkout gets stand-ins) and `<dir>/kernels`
    /// (the server finds them at <static>/../kernels).
    pub fn new(name: &str) -> Server {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let dir = std::env::temp_dir().join(format!("gb-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let static_dir = dir.join("client");
        copy_dir(&root.join("client"), &static_dir);
        copy_dir(&root.join("kernels"), &dir.join("kernels"));
        if !static_dir.join("core.wasm").exists() {
            let mut wasm = b"\0asm\x01\0\0\0".to_vec();
            wasm.extend(vec![0u8; 8192]);
            std::fs::write(static_dir.join("core.wasm"), wasm).unwrap();
            std::fs::write(static_dir.join("core.js"), "export default async () => ({});\n").unwrap();
        }
        Server { db: dir.join("t.db"), dir, static_dir, port: free_port(), child: None }
    }

    pub fn start(&mut self) {
        self.port = free_port();
        let child = Command::new(env!("CARGO_BIN_EXE_goldbach-server"))
            .args(["--db", self.db.to_str().unwrap(), "--static", self.static_dir.to_str().unwrap(), "--port", &self.port.to_string()])
            .env("GB_SNAPSHOT_MAX_S", "1") // directly inserted rows show up within a second
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn goldbach-server");
        self.child = Some(child);
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(20) {
            if TcpStream::connect(("127.0.0.1", self.port)).is_ok() && self.try_get("/api/health", &[]).is_some_and(|r| r.status == 200) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the coordinator did not come up");
    }

    pub fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    pub fn try_get(&self, path: &str, headers: &[(&str, &str)]) -> Option<Resp> {
        request(self.port, "GET", path, headers, b"")
    }

    pub fn get(&self, path: &str, headers: &[(&str, &str)]) -> Resp {
        self.try_get(path, headers).unwrap_or_else(|| panic!("GET {path}: no response"))
    }

    pub fn post(&self, path: &str, body: &str) -> Resp {
        request(self.port, "POST", path, &[("Content-Type", "application/json")], body.as_bytes()).expect("POST")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub fn request(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Option<Resp> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(20))).ok()?;
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req += &format!("{k}: {v}\r\n");
    }
    if method == "POST" {
        req += &format!("Content-Length: {}\r\n", body.len());
    }
    req += "\r\n";
    s.write_all(req.as_bytes()).ok()?;
    s.write_all(body).ok()?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).ok()?;
    let at = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&raw[..at]).into_owned();
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
    let headers: Vec<(String, String)> =
        lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
    let mut body = raw[at + 4..].to_vec();
    if headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("transfer-encoding") && v.contains("chunked")) {
        let (mut out, mut rest) = (Vec::new(), &body[..]);
        loop {
            let nl = rest.windows(2).position(|w| w == b"\r\n")?;
            let n = usize::from_str_radix(std::str::from_utf8(&rest[..nl]).ok()?.split(';').next()?.trim(), 16).ok()?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&rest[nl + 2..nl + 2 + n]);
            rest = &rest[nl + 2 + n + 2..];
        }
        body = out;
    }
    Some(Resp { status, headers, body })
}

pub fn gunzip(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(b).read_to_end(&mut out).expect("gzip body");
    out
}

pub fn sha256_hex(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

#[derive(Default)]
pub struct Checks {
    pub fails: usize,
}

impl Checks {
    pub fn check(&mut self, ok: bool, what: impl AsRef<str>) {
        println!("{} {}", if ok { "ok  " } else { "FAIL" }, what.as_ref());
        self.fails += !ok as usize;
    }
    pub fn done(self, ok_msg: &str) {
        assert!(self.fails == 0, "{} check(s) failed", self.fails);
        println!("{ok_msg}");
    }
}
