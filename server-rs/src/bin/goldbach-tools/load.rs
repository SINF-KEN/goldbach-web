//! What the load tools share: a keep-alive client that records latency and the server's own
//! Server-Timing split, synthetic results, and a server process pinned to one core.
use crate::util::{e, free_port, pct, text, wait_listening, R};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::Instant;

/// The server's own split of one response (Server-Timing: lock, sql, rebuild, total ... in ms).
pub type Timing = HashMap<String, f64>;

pub struct Client {
    agent: ureq::Agent,
    base: String,
    /// per endpoint: (client-side ms, server timing)
    pub lat: HashMap<String, Vec<(f64, Timing)>>,
}

impl Client {
    pub fn new(port: u16) -> Client {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(std::time::Duration::from_secs(120)))
            .build()
            .into();
        Client { agent, base: format!("http://127.0.0.1:{port}"), lat: HashMap::new() }
    }

    /// One request; `key` names the endpoint in the latency table (default: the path). A
    /// failed request counts as status 0.
    pub fn call(&mut self, method: &str, path: &str, body: Option<&Value>, ip: &str, key: Option<&str>) -> (u16, Vec<u8>) {
        let url = format!("{}{path}", self.base);
        let t = Instant::now();
        let r = if method == "GET" {
            self.agent.get(&url).header("X-Forwarded-For", ip).call()
        } else {
            let data = body.map(|b| b.to_string()).unwrap_or_default();
            self.agent.post(&url).header("X-Forwarded-For", ip).content_type("application/json").send(data.as_bytes())
        };
        let (status, raw, timing) = match r {
            Ok(mut resp) => {
                let timing = resp.headers().get("server-timing").and_then(|v| v.to_str().ok()).map(parse_timing).unwrap_or_default();
                let status = resp.status().as_u16();
                let mut raw = vec![];
                let _ = resp.body_mut().as_reader().read_to_end(&mut raw);
                (status, raw, timing)
            }
            Err(_) => (0, vec![], Timing::new()),
        };
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let key = key.map(str::to_string).unwrap_or_else(|| path.split('?').next().unwrap_or(path).to_string());
        self.lat.entry(key).or_default().push((ms, timing));
        (status, raw)
    }

    /// POST/GET and parse the answer (Null when it is not JSON).
    pub fn json(&mut self, method: &str, path: &str, body: Option<&Value>, ip: &str, key: Option<&str>) -> (u16, Value) {
        let (s, raw) = self.call(method, path, body, ip, key);
        (s, serde_json::from_slice(&raw).unwrap_or(Value::Null))
    }
}

pub fn parse_timing(h: &str) -> Timing {
    h.split(',').filter_map(|p| p.trim().split_once(";dur=")).filter_map(|(k, v)| Some((k.to_string(), v.parse().ok()?))).collect()
}

/// Merges per-endpoint samples.
pub fn merge(into: &mut HashMap<String, Vec<(f64, Timing)>>, from: HashMap<String, Vec<(f64, Timing)>>) {
    for (k, v) in from {
        into.entry(k).or_default().extend(v);
    }
}

/// A result the coordinator accepts for a leased slice: exact-looking densities, no record claims.
pub fn synthetic(start: &Value, lim: &Value) -> Value {
    let (s, l): (u64, u64) = (text(start).parse().unwrap_or(0), text(lim).parse().unwrap_or(0));
    let evens = (l - s) / 2 + 1;
    let x = (s as f64 + l as f64) / 2.0;
    let primes = ((l - s) as f64 / x.ln()).round() as u64;
    let twins = (2.0 * 0.6601618158 * (l - s) as f64 / x.ln().powi(2)).round() as u64;
    let mut p30 = vec![primes / 8; 7];
    p30.push(primes - 7 * (primes / 8));
    let t30 = [twins / 3, twins / 3, twins - 2 * (twins / 3)];
    let strs = |v: &[u64]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    json!({ "start": s.to_string(), "limit": l.to_string(), "evens": evens.to_string(), "pass": true,
            "primes": primes.to_string(), "twins": twins.to_string(), "primes30": strs(&p30), "twins30": strs(&t30),
            "pmax": 5003, "tailEvens": 10, "histByP": [[7, evens - 10], [5003, 10]],
            "histByClass": [[[7, 1]]], "first": [], "beyond": [], "ridges": [] })
}

pub fn rand_ip(rng: &mut crate::util::Rng) -> String {
    format!("10.{}.{}.{}", rng.range(0, 256), rng.range(0, 256), rng.range(1, 255))
}

/// The coordinator on a copy of a database, pinned to one core (the VPS has one vCPU).
pub struct Server {
    pub child: Child,
    pub port: u16,
    pub tmp: std::path::PathBuf,
}

impl Server {
    pub fn start(bin: &str, db: &str, root: &str, cpu: usize, wrap: &[String], tmpdir: Option<&str>) -> R<Server> {
        let tmp = std::path::PathBuf::from(tmpdir.map(str::to_string).unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned()))
            .join(format!("gb-load-{}-{}", std::process::id(), free_port()?));
        std::fs::create_dir_all(&tmp).map_err(e)?;
        let copy = tmp.join("b.db");
        std::fs::copy(db, &copy).map_err(|x| format!("{db}: {x}"))?;
        let port = free_port()?;
        let mut argv: Vec<String> = vec![];
        if cfg!(target_os = "linux") {
            argv.extend(["taskset".into(), "-c".into(), cpu.to_string()]);
        }
        argv.extend(wrap.iter().cloned());
        argv.extend([bin.into(), "--db".into(), copy.to_string_lossy().into(), "--static".into(), format!("{root}/client"), "--port".into(), port.to_string()]);
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .env("GOLDBACH_LOCAL_IPS", "127.0.0.1")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|x| format!("{}: {x}", argv[0]))?;
        if let Err(m) = wait_listening(port, &mut child) {
            let mut err = String::new();
            if let Some(mut s) = child.stderr.take() {
                let _ = s.read_to_string(&mut err);
            }
            let _ = child.kill();
            return Err(format!("{m}: {err}"));
        }
        Ok(Server { child, port, tmp })
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Resident memory in MB (Linux; 0 elsewhere).
    pub fn rss_mb(&self) -> f64 {
        std::fs::read_to_string(format!("/proc/{}/status", self.child.id()))
            .ok()
            .and_then(|s| s.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok()))
            .map_or(0.0, |kb| kb / 1024.0)
    }

    /// CPU seconds used so far (Linux; 0 elsewhere).
    pub fn cpu_s(&self) -> f64 {
        let Ok(s) = std::fs::read_to_string(format!("/proc/{}/stat", self.child.id())) else { return 0.0 };
        let v: Vec<&str> = s.rsplit_once(')').map_or("", |x| x.1).split_whitespace().collect();
        let tick = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
        (v.get(11).and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0) + v.get(12).and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0)) / tick
    }

    /// SIGINT first (a perf wrapper writes its data), then kill.
    pub fn close(mut self) {
        unsafe { libc::kill(self.child.id() as i32, libc::SIGINT) };
        for _ in 0..100 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

/// n / rps / p50 / p95 / p99 / max of the client-side latencies.
pub fn summary(v: &[(f64, Timing)], el: f64) -> Value {
    let ms: Vec<f64> = v.iter().map(|x| x.0).collect();
    json!({ "n": v.len(), "rps": v.len() as f64 / el, "p50": pct(&ms, 50.0), "p95": pct(&ms, 95.0), "p99": pct(&ms, 99.0),
            "max": ms.iter().cloned().fold(f64::NAN, f64::max) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_timing_header() {
        let t = parse_timing("lock;dur=0.12, sql;dur=3.5, total;dur=4.1");
        assert_eq!((t["lock"], t["sql"], t["total"]), (0.12, 3.5, 4.1));
        assert!(parse_timing("garbage").is_empty());
    }

    #[test]
    fn synthetic_result_adds_up() {
        let r = synthetic(&json!("4000000000000000000"), &json!(4000000000999999998u64));
        assert_eq!(r["evens"], "500000000");
        let sum: u64 = r["primes30"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().parse::<u64>().unwrap()).sum();
        assert_eq!(sum.to_string(), r["primes"].as_str().unwrap());
        assert_eq!(r["pass"], true);
    }
}
