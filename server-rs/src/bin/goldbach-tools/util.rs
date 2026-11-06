//! Small helpers shared by the tools.
use serde_json::Value;
use sha2::{Digest, Sha256};

pub type R<T> = Result<T, String>;

pub fn e<T: std::fmt::Display>(x: T) -> String {
    x.to_string()
}

/// First `n` hex digits of the sha256 of `data`.
pub fn sha_prefix(data: &[u8], n: usize) -> String {
    let hex: String = Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect();
    hex[..n].to_string()
}

/// Splits `--key value` / `--key=value` flags from positional arguments.
/// `bool_flags` take no value.
pub struct Args {
    pub pos: Vec<String>,
    flags: Vec<(String, String)>,
}

impl Args {
    pub fn parse(args: &[String], bool_flags: &[&str]) -> R<Args> {
        let (mut pos, mut flags) = (vec![], vec![]);
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if let Some(k) = a.strip_prefix("--") {
                if let Some((k, v)) = k.split_once('=') {
                    flags.push((k.to_string(), v.to_string()));
                } else if bool_flags.contains(&k) {
                    flags.push((k.to_string(), String::new()));
                } else {
                    flags.push((k.to_string(), it.next().ok_or(format!("--{k} needs a value"))?.clone()));
                }
            } else {
                pos.push(a.clone());
            }
        }
        Ok(Args { pos, flags })
    }
    pub fn get(&self, k: &str) -> Option<&str> {
        self.flags.iter().rev().find(|f| f.0 == k).map(|f| f.1.as_str())
    }
    pub fn str_or(&self, k: &str, d: &str) -> String {
        self.get(k).unwrap_or(d).to_string()
    }
    pub fn num<T: std::str::FromStr>(&self, k: &str, d: T) -> R<T> {
        match self.get(k) {
            Some(v) => v.parse().map_err(|_| format!("--{k}: not a number: {v}")),
            None => Ok(d),
        }
    }
    pub fn positional(&self, i: usize, what: &str) -> R<&str> {
        self.pos.get(i).map(String::as_str).ok_or(format!("missing argument {what}"))
    }
}

/// A small deterministic generator (splitmix64): the cross-check's windows come from a seed.
pub struct Rng(u64);

impl Rng {
    pub fn from_seed(seed: &str) -> Rng {
        let h = Sha256::digest(seed.as_bytes());
        Rng(u64::from_le_bytes(h[..8].try_into().unwrap()))
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    /// A uniform integer in [lo, hi).
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        assert!(hi > lo);
        lo + ((self.next_u64() as u128 * (hi - lo) as u128) >> 64) as u64
    }
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn choice<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.range(0, xs.len() as u64) as usize]
    }
}

/// p-th percentile (nearest rank as the Python tools did), NaN for no samples.
pub fn pct(xs: &[f64], p: f64) -> f64 {
    if xs.is_empty() {
        return f64::NAN;
    }
    let mut v = xs.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    v[((p / 100.0 * v.len() as f64) as usize).min(v.len() - 1)]
}

pub fn mean(xs: &[f64]) -> f64 {
    xs.iter().sum::<f64>() / xs.len() as f64
}

pub fn median(xs: &[f64]) -> f64 {
    let mut v = xs.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 }
}

/// Sample standard deviation.
pub fn stdev(xs: &[f64]) -> f64 {
    let m = mean(xs);
    (xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (xs.len() - 1) as f64).sqrt()
}

pub fn f(v: &Value, k: &str) -> f64 {
    v[k].as_f64().unwrap_or(0.0)
}

/// Text of a JSON string or number (the lease fields are either).
pub fn text(v: &Value) -> String {
    v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())
}

pub fn write_json(path: &str, v: &Value) -> R<()> {
    std::fs::write(path, serde_json::to_string_pretty(v).map_err(e)?).map_err(|x| format!("{path}: {x}"))
}

pub fn read_json(path: &str) -> R<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).map_err(|x| format!("{path}: {x}"))?).map_err(|x| format!("{path}: {x}"))
}

pub fn free_port() -> R<u16> {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(e)?;
    Ok(l.local_addr().map_err(e)?.port())
}

/// Waits (up to ~15 s) until something listens on the port.
pub fn wait_listening(port: u16, child: &mut std::process::Child) -> R<()> {
    for _ in 0..300 {
        if std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), std::time::Duration::from_millis(200)).is_ok() {
            return Ok(());
        }
        if let Ok(Some(st)) = child.try_wait() {
            return Err(format!("the server exited: {st}"));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Err("the server does not listen".into())
}

/// Pins the calling thread to the given CPUs (Linux; elsewhere a no-op).
#[allow(unused_variables)]
pub fn pin_thread(cpus: &[usize]) {
    #[cfg(target_os = "linux")]
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        for &c in cpus {
            libc::CPU_SET(c, &mut set);
        }
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
    }
}

pub fn ncpu() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}
