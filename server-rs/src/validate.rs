//! Exact arithmetic for the server-side checks and the validation of a slice
//! result into its canonical JSON (whose sha256 two devices must agree on: the
//! bytes are exactly Python's json.dumps(canon, separators=(",", ":"), sort_keys=True)).
use crate::py::{self, PyErr};
use crate::{RANGE_HI};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

/// The odd primes below 9800 (_SMALL).
pub static SMALL: LazyLock<Vec<u64>> =
    LazyLock::new(|| (3..9800u64).step_by(2).filter(|&p| (3..).step_by(2).take_while(|d| d * d <= p).all(|d| p % d != 0)).collect());

fn small_set(p: i128) -> bool {
    p <= 9781 && p >= 3 && SMALL.binary_search(&(p as u64)).is_ok()
}

fn mulmod(a: u64, b: u64, m: u64) -> u64 {
    ((a as u128 * b as u128) % m as u128) as u64
}

fn powmod(mut a: u64, mut e: u64, m: u64) -> u64 {
    let mut r = 1u64 % m;
    a %= m;
    while e > 0 {
        if e & 1 == 1 {
            r = mulmod(r, a, m);
        }
        a = mulmod(a, a, m);
        e >>= 1;
    }
    r
}

const BASES: [u64; 12] = [2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37];

/// Deterministic Miller-Rabin, exact for every n < 2^64. Values beyond 64 bits
/// (never part of an honest result) count as not prime.
pub fn is_prime(n: i128) -> bool {
    if n < 2 || n > u64::MAX as i128 {
        return false;
    }
    let n = n as u64;
    for p in BASES {
        if n % p == 0 {
            return n == p;
        }
    }
    let (mut d, mut r) = (n - 1, 0);
    while d % 2 == 0 {
        d /= 2;
        r += 1;
    }
    'a: for a in BASES {
        let mut x = powmod(a, d, n);
        if x == 1 || x == n - 1 {
            continue;
        }
        for _ in 0..r - 1 {
            x = mulmod(x, x, n);
            if x == n - 1 {
                continue 'a;
            }
        }
        return false;
    }
    true
}

/// (primes, twin pairs) at the odd integer limit + 1 above a slice [start, limit]
/// (server.py boundary_counts: each slice's count range is (start, limit + 1]).
pub fn boundary_counts(limit: i128) -> (i128, i128) {
    if limit + 2 > RANGE_HI || !is_prime(limit + 1) {
        return (0, 0);
    }
    (1, is_prime(limit - 1) as i128 + is_prime(limit + 3) as i128)
}

/// p(n): the smallest prime p with n - p prime, for an even n >= 4.
pub fn min_partition(n: i128) -> i128 {
    if n == 4 {
        return 2;
    }
    for &p in SMALL.iter() {
        if is_prime(n - p as i128) {
            return p as i128;
        }
    }
    let mut p = *SMALL.last().unwrap() as i128 + 2;
    while !(is_prime(p) && is_prime(n - p)) {
        p += 2;
    }
    p
}

/// True iff p is exactly p(n).
pub fn pn_is(n: i128, p: i128) -> bool {
    if !(is_prime(p) && is_prime(n - p)) {
        return false;
    }
    for &q in SMALL.iter() {
        let q = q as i128;
        if q >= p {
            return true;
        }
        if is_prime(n - q) {
            return false;
        }
    }
    let mut q = *SMALL.last().unwrap() as i128 + 2;
    while q < p {
        if is_prime(q) && is_prime(n - q) {
            return false;
        }
        q += 2;
    }
    true
}

// ---- Python-style access to the untrusted result --------------------------

pub fn get<'a>(v: &'a Value, k: &str) -> Result<&'a Value, PyErr> {
    match v {
        Value::Object(o) => o.get(k).ok_or_else(|| py::str_repr(k)),
        Value::Array(_) => Err("list indices must be integers or slices, not str".into()),
        Value::String(_) => Err("string indices must be integers, not 'str'".into()),
        Value::Null => Err("'NoneType' object is not subscriptable".into()),
        Value::Bool(_) => Err("'bool' object is not subscriptable".into()),
        Value::Number(n) if n.is_f64() => Err("'float' object is not subscriptable".into()),
        Value::Number(_) => Err("'int' object is not subscriptable".into()),
    }
}

/// Iterating a JSON value as Python would iterate it (lists; dict keys).
fn iter(v: &Value) -> Result<Vec<Value>, PyErr> {
    match v {
        Value::Array(a) => Ok(a.clone()),
        Value::Object(o) => Ok(o.keys().map(|k| Value::String(k.clone())).collect()),
        Value::String(s) => Ok(s.chars().map(|c| Value::String(c.to_string())).collect()),
        Value::Null => Err("'NoneType' object is not iterable".into()),
        Value::Bool(_) => Err("'bool' object is not iterable".into()),
        Value::Number(n) if n.is_f64() => Err("'float' object is not iterable".into()),
        Value::Number(_) => Err("'int' object is not iterable".into()),
    }
}

/// `a, b, ... = v` with exactly n values.
fn unpack(v: &Value, n: usize) -> Result<Vec<Value>, PyErr> {
    let items = match v {
        Value::Array(_) | Value::Object(_) | Value::String(_) => iter(v)?,
        Value::Null => return Err("cannot unpack non-iterable NoneType object".into()),
        Value::Bool(_) => return Err("cannot unpack non-iterable bool object".into()),
        Value::Number(x) if x.is_f64() => return Err("cannot unpack non-iterable float object".into()),
        Value::Number(_) => return Err("cannot unpack non-iterable int object".into()),
    };
    if items.len() < n {
        return Err(format!("not enough values to unpack (expected {n}, got {})", items.len()));
    }
    if items.len() > n {
        return Err(format!("too many values to unpack (expected {n}, got {})", items.len()));
    }
    Ok(items)
}

fn index(v: &Value, i: usize) -> Result<&Value, PyErr> {
    match v {
        Value::Array(a) => a.get(i).ok_or_else(|| "list index out of range".to_string()),
        _ => Err(format!("'{}' object is not subscriptable", match v {
            Value::Null => "NoneType",
            Value::Object(_) => "dict",
            _ => "int",
        })),
    }
}

fn ints(v: &Value) -> Result<Vec<i128>, PyErr> {
    iter(v)?.iter().map(py::int).collect()
}

fn pairs(v: &Value) -> Result<Vec<(i128, i128)>, PyErr> {
    iter(v)?
        .iter()
        .map(|e| {
            let u = unpack(e, 2)?;
            Ok((py::int(&u[0])?, py::int(&u[1])?))
        })
        .collect()
}

fn join<T, F: Fn(&T) -> String>(xs: &[T], f: F) -> String {
    let mut s = String::from("[");
    for (i, x) in xs.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&f(x));
    }
    s.push(']');
    s
}

fn qs(x: impl std::fmt::Display) -> String {
    format!("\"{x}\"")
}

fn rand_below(n: usize) -> usize {
    (rand::random::<u64>() % n as u64) as usize
}

/// Checks a result's shape and internal consistency: Ok(canonical JSON) or Err(why).
pub fn validate(res: &Value, start: i128, lim: i128) -> Result<String, String> {
    validate_inner(res, start, lim).map_err(|e| match e {
        Reject::Why(w) => w,
        Reject::Py(e) => format!("malformed result: {e}"),
    })
}

enum Reject {
    Why(String),
    Py(PyErr),
}

impl From<PyErr> for Reject {
    fn from(e: PyErr) -> Self {
        Reject::Py(e)
    }
}

fn why<T>(w: impl Into<String>) -> Result<T, Reject> {
    Err(Reject::Why(w.into()))
}

fn validate_inner(res: &Value, start: i128, lim: i128) -> Result<String, Reject> {
    if py::int(get(res, "start")?)? != start || py::int(get(res, "limit")?)? != lim {
        return why("range mismatch");
    }
    let evens = (lim - start) / 2 + 1;
    if py::int(get(res, "evens")?)? != evens {
        return why("even count");
    }
    let hist = pairs(get(res, "histByP")?)?;
    if hist.iter().map(|h| h.1).sum::<i128>() != evens {
        return why("histogram does not cover every even");
    }
    let p30 = ints(get(res, "primes30")?)?;
    let t30 = ints(get(res, "twins30")?)?;
    let (sp, st): (i128, i128) = (p30.iter().sum(), t30.iter().sum());
    if p30.len() != 8 || t30.len() != 3 || sp != py::int(get(res, "primes")?)? || st != py::int(get(res, "twins")?)? {
        return why("counts");
    }
    // densities: primes ~ h / ln x, twins ~ 2 C2 h / ln^2 x
    let x = (start + lim) as f64 / 2.0;
    let h = (lim - start) as f64;
    let expect = h / x.ln();
    if !(0.998 * expect < sp as f64 && (sp as f64) < 1.002 * expect) {
        return why("implausible prime count");
    }
    let lx = x.ln();
    let texp = 2.0 * 0.6601618158 * h / (lx * lx);
    if !(0.98 * texp < st as f64 && (st as f64) < 1.02 * texp) {
        return why("implausible twin count");
    }
    // hp = dict(hist): the last value of a repeated p wins, first-seen order
    let mut order: Vec<i128> = Vec::new();
    let mut hp: HashMap<i128, i128> = HashMap::new();
    for &(p, v) in &hist {
        if hp.insert(p, v).is_none() {
            order.push(p);
        }
    }
    if order.iter().any(|p| !small_set(*p) && *p <= 9781) || hp.values().any(|v| *v < 0) {
        return why("histogram has a non-prime or negative entry");
    }
    let tail: i128 = hp.iter().filter(|(p, _)| **p >= 3000).map(|(_, v)| v).sum();
    if tail != py::int(get(res, "tailEvens")?)? {
        return why("tail / pmax inconsistent with the histogram");
    }
    if tail != 0 {
        let m = hp.iter().filter(|(p, v)| **p >= 3000 && **v != 0).map(|(p, _)| *p).max();
        if m != Some(py::int(get(res, "pmax")?)?) {
            return why("tail / pmax inconsistent with the histogram");
        }
    }
    let mut ridges: Vec<(i128, i128)> = Vec::new(); // (n, p)
    for r in iter(get(res, "ridges")?)? {
        let n = py::int(get(&r, "n")?)?;
        ridges.push((n, py::int(get(&r, "p")?)?));
    }
    ridges.sort_by_key(|r| r.0);
    if ridges.iter().any(|r| r.1 < 7000) || ridges.iter().any(|r| !(start <= r.0 && r.0 <= lim)) {
        return why("ridges");
    }
    if ridges.len() as i128 != hp.iter().filter(|(p, _)| **p >= 7000).map(|(_, v)| v).sum::<i128>() {
        return why("ridge list does not match the histogram");
    }
    // exact re-proof of every record-type claim
    for &(n, p) in &ridges {
        if !pn_is(n, p) {
            return why(format!("ridge {n} is not p(n) = {p}"));
        }
    }
    let firsts = pairs(get(res, "first")?)?;
    let mut check: Vec<(i128, i128)> = firsts.iter().copied().filter(|f| f.0 >= 6000).collect();
    let mut rest: Vec<(i128, i128)> = firsts.iter().copied().filter(|f| f.0 < 6000).collect();
    let k = rest.len().min(25);
    for i in 0..k {
        // partial Fisher-Yates: a uniform sample without replacement
        let j = i + rand_below(rest.len() - i);
        rest.swap(i, j);
        check.push(rest[i]);
    }
    for &(p, n) in &check {
        if !(start <= n && n <= lim && n.rem_euclid(2) == 0) || !pn_is(n, p) {
            return why(format!("first occurrence {p}:{n} is not exact"));
        }
    }
    let beyond: Vec<Value> = iter(get(res, "beyond")?)?;
    for b in &beyond {
        let n = py::int(get(b, "n")?)?;
        if !pn_is(n, py::int(get(b, "p")?)?) {
            return why("an even beyond the candidate list is not exact");
        }
    }
    let pass = py::truthy(get(res, "pass")?);
    let pmax = py::int(get(res, "pmax")?)?;
    let tail_evens = py::int(get(res, "tailEvens")?)?;
    let mut by_class: Vec<Vec<(i128, i128)>> = Vec::new();
    for c in iter(get(res, "histByClass")?)? {
        by_class.push(pairs(&c)?);
    }
    let mut beyond_c: Vec<(i128, i128)> = Vec::new();
    for b in &beyond {
        let p = py::int(get(b, "p")?)?;
        beyond_c.push((p, py::int(get(b, "n")?)?));
    }
    let pair = |x: &(i128, i128)| format!("[{},{}]", x.0, x.1);
    let pair_s = |x: &(i128, i128)| format!("[{},\"{}\"]", x.0, x.1);
    // keys in sorted (code point) order
    Ok(format!(
        concat!(
            "{{\"beyond\":{},\"evens\":\"{}\",\"first\":{},\"histByClass\":{},\"histByP\":{},\"limit\":\"{}\",",
            "\"pass\":{},\"pmax\":{},\"primes\":\"{}\",\"primes30\":{},\"ridges\":{},\"start\":\"{}\",",
            "\"tailEvens\":{},\"twins\":\"{}\",\"twins30\":{}}}"
        ),
        join(&beyond_c, pair_s),
        evens,
        join(&firsts, pair_s),
        join(&by_class, |c| join(c, pair)),
        join(&hist, pair),
        lim,
        pass,
        pmax,
        sp,
        join(&p30, |x| qs(x)),
        join(&ridges, |r| format!("{{\"n\":\"{}\",\"p\":{}}}", r.0, r.1)),
        start,
        tail_evens,
        st,
        join(&t30, |x| qs(x)),
    ))
}

/// A result's prime gaps: Ok(None) for an engine without the gap counter,
/// Ok(Some(canonical JSON)) when consistent, Err(why) otherwise.
pub fn validate_gaps(res: &Value, start: i128, lim: i128) -> Result<Option<String>, String> {
    let has = matches!(res, Value::Object(o) if o.contains_key("gaps"));
    if !has {
        return Ok(None);
    }
    gaps_inner(res, start, lim).map(Some).map_err(|e| match e {
        Reject::Why(w) => w,
        Reject::Py(e) => format!("gaps: malformed ({e})"),
    })
}

fn gaps_inner(res: &Value, start: i128, lim: i128) -> Result<String, Reject> {
    let o = get(res, "gaps")?;
    let n = py::int(get(o, "n")?)?;
    let d = ints(get(o, "d")?)?;
    let hist = pairs(get(o, "hist")?)?;
    let mut tail: Vec<(i128, i128, i128)> = Vec::new();
    for t in iter(get(o, "tail")?)? {
        let u = unpack(&t, 3)?;
        tail.push((py::int(&u[0])?, py::int(&u[1])?, py::int(&u[2])?));
    }
    let mx = get(o, "max")?;
    let gm = py::int(index(mx, 0)?)?;
    let glo = py::int(index(mx, 1)?)?;
    if d.len() != 4 || n != py::int(get(res, "primes")?)? {
        return why("gaps: one gap per prime");
    }
    if hist.iter().any(|&(g, c)| g < 2 || g % 2 != 0 || c <= 0) || hist.iter().map(|h| h.1).sum::<i128>() != n {
        return why("gaps: histogram");
    }
    let sums: Vec<Option<i128>> = (1..=4u32)
        .map(|k| {
            hist.iter().try_fold(0i128, |acc, &(g, c)| g.checked_pow(k).and_then(|x| x.checked_mul(c)).and_then(|x| acc.checked_add(x)))
        })
        .collect();
    if sums.iter().zip(&d).any(|(s, d)| *s != Some(*d)) {
        return why("gaps: sums of g^k do not match the histogram");
    }
    let big: BTreeMap<i128, i128> = hist.iter().filter(|h| h.0 >= 700).copied().collect();
    let tl: BTreeMap<i128, i128> = tail.iter().map(|t| (t.0, t.1)).collect();
    if big != tl {
        return why("gaps: tail list does not match the histogram");
    }
    if n != 0 && Some(gm) != hist.iter().map(|h| h.0).max() {
        return why("gaps: largest gap");
    }
    if n != 0 {
        let real = start - 2000 < glo
            && glo < lim
            && is_prime(glo)
            && is_prime(glo + gm)
            && !(glo + 2..glo + gm).step_by(2).any(is_prime);
        if !real {
            return why("gaps: the largest gap is not a prime gap");
        }
        let mut f = start + 1;
        while f < lim && !is_prime(f) {
            f += 2;
        }
        let mut last = lim - 1;
        while !is_prime(last) {
            last -= 2;
        }
        let mut p = f - 2;
        while !is_prime(p) {
            p -= 2;
        }
        if d[0] != last - p {
            return why("gaps: d1 is not last prime - predecessor of the first");
        }
    }
    Ok(format!(
        "{{\"d\":{},\"hist\":{},\"max\":[{},\"{}\"],\"n\":\"{}\",\"tail\":{}}}",
        join(&d, |x| qs(x)),
        join(&hist, |h| format!("[{},{}]", h.0, h.1)),
        gm,
        glo,
        n,
        join(&tail, |t| format!("[{},{},\"{}\"]", t.0, t.1, t.2)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records() {
        let n = 4010506309418407798i128;
        assert!(pn_is(n, 8627));
        assert!(!pn_is(n, 8629));
        assert!(!pn_is(n + 2, 8627));
        assert_eq!(boundary_counts(4035849999999999998), (0, 0));
        assert_eq!(boundary_counts(RANGE_HI), (0, 0));
        assert_eq!(boundary_counts(4035799999999999998).0, 1);
        assert_eq!(SMALL.len(), 1207);
    }
}
