//! Builds client/paper.json, the reference data of the live paper page (/paper),
//! from Tomás Oliveira e Silva's published tables of the verification to 4e18
//! (https://sweet.ua.pt/tos/goldbach.html):
//!
//!   goldbach/t0.txt.gz  S(p) and L(p, 4e18) for every p(n) seen up to 4e18
//!   goldbach/t1.txt.gz  Hardy-Littlewood constants C(p, k) for p < 250
//!   gaps/t0.txt.gz      P(g) and N(g, 4e18) for every gap seen up to 4e18
//!   gaps/t1.txt.gz      Hardy-Littlewood constants A(g, k) for g <= 212
//!
//!   goldbach-tools make-paper-json t0.txt.gz t1.txt.gz gaps_t0.txt.gz gaps_t1.txt.gz > client/paper.json
use crate::util::R;
use serde_json::Value;
use std::collections::HashSet;
use std::io::Read;

fn read_gz(path: &str) -> R<Vec<String>> {
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(std::fs::File::open(path).map_err(|x| format!("{path}: {x}"))?)
        .read_to_end(&mut raw)
        .map_err(|x| format!("{path}: {x}"))?;
    Ok(String::from_utf8_lossy(&raw).lines().map(str::to_string).collect())
}

fn digits(s: &str) -> Option<(&str, &str)> {
    let n = s.bytes().take_while(u8::is_ascii_digit).count();
    (n > 0).then(|| s.split_at(n))
}

fn ws1(s: &str) -> Option<&str> {
    let t = s.trim_start();
    (t.len() < s.len()).then_some(t)
}

/// `\d+` or `\?`, then an optional `*`.
fn star(s: &str) -> &str {
    s.strip_prefix('*').unwrap_or(s)
}

/// t0 row: `\s*\d+\s+(\d+)\*?\s+(\d+|\?)\*?\s+(\d+)` (Goldbach: a leading row number) or
/// `\s*(\d+)\*?\s+(\d+|\?)\*?\s+(\d+)` (gaps).
fn table_row(line: &str, numbered: bool) -> Option<(u64, Option<String>, String)> {
    let mut s = line.trim_start();
    if numbered {
        s = ws1(digits(s)?.1)?;
    }
    let (a, s) = digits(s)?;
    let s = ws1(star(s))?;
    let (b, s) = match s.strip_prefix('?') {
        Some(r) => ("?", r),
        None => digits(s)?,
    };
    let s = ws1(star(s))?;
    let (c, _) = digits(s)?;
    Some((a.parse().ok()?, (b != "?").then(|| b.to_string()), c.to_string()))
}

fn is_const(t: &str) -> bool {
    // [0-9.]+e[+-]\d+
    let Some((m, x)) = t.split_once('e') else { return false };
    !m.is_empty()
        && m.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && x.len() > 1
        && matches!(x.as_bytes()[0], b'+' | b'-')
        && x[1..].bytes().all(|b| b.is_ascii_digit())
}

/// Constants row: `\s*(\d+)\s+(\d+)\s+\d+\s+([0-9.]+e[+-]\d+)\s*$`.
fn const_row(line: &str) -> Option<(String, u64, f64)> {
    let t: Vec<&str> = line.split_whitespace().collect();
    if t.len() != 4 || !t[..3].iter().all(|x| x.bytes().all(|b| b.is_ascii_digit())) || !is_const(t[3]) {
        return None;
    }
    Some((t[0].to_string(), t[1].parse().ok()?, t[3].parse().ok()?))
}

/// Python's repr of a float (what json.dump wrote): shortest digits, exponent form
/// below 1e-4 and from 1e16.
pub fn py_float(x: f64) -> String {
    if x == 0.0 {
        return "0.0".into();
    }
    let sci = format!("{x:e}"); // shortest round-trip digits: d.ddde-5
    let nd = sci.split_once('e').unwrap().0.bytes().filter(u8::is_ascii_digit).count();
    // an exact tie between two shortest candidates: Python rounds half to even, Rust's
    // shortest form rounds half up; the exact expansion at that length decides
    let exact = format!("{:.*e}", nd - 1, x);
    let sci = if exact.parse::<f64>() == Ok(x) { exact } else { sci };
    let (m, ex) = sci.split_once('e').unwrap();
    let ex: i32 = ex.parse().unwrap();
    let (neg, m) = (m.starts_with('-'), m.trim_start_matches('-'));
    let d: String = m.chars().filter(char::is_ascii_digit).collect();
    let body = if (-4..16).contains(&ex) {
        if ex >= 0 {
            let int = ex as usize + 1;
            let ip: String = d.chars().chain(std::iter::repeat('0')).take(int).collect();
            let fp = if d.len() > int { &d[int..] } else { "0" };
            format!("{ip}.{fp}")
        } else {
            format!("0.{}{d}", "0".repeat((-ex - 1) as usize))
        }
    } else {
        let mant = if d.len() > 1 { format!("{}.{}", &d[..1], &d[1..]) } else { d.clone() };
        format!("{mant}e{}{:02}", if ex < 0 { '-' } else { '+' }, ex.abs())
    };
    if neg { format!("-{body}") } else { body }
}

fn consts(lines: &[String]) -> String {
    let mut order: Vec<String> = vec![];
    let mut map: std::collections::HashMap<String, Vec<(u64, f64)>> = Default::default();
    for l in lines {
        if let Some((k, i, c)) = const_row(l) {
            if !map.contains_key(&k) {
                order.push(k.clone());
            }
            map.entry(k).or_default().push((i, c));
        }
    }
    let body: Vec<String> = order
        .iter()
        .map(|k| {
            let rows: Vec<String> = map[k].iter().map(|(i, c)| format!("[{i},{}]", py_float(*c))).collect();
            format!("{}:[{}]", Value::from(k.as_str()), rows.join(","))
        })
        .collect();
    format!("{{{}}}", body.join(","))
}

fn rows(lines: &[String], numbered: bool) -> Vec<String> {
    let mut seen = HashSet::new(); // the table lists the '?' rows twice
    let mut out = vec![];
    for l in lines {
        if let Some((k, s, n)) = table_row(l, numbered) {
            if seen.insert(k) {
                out.push(format!("[{k},{},{}]", s.map(|s| Value::from(s).to_string()).unwrap_or("null".into()), Value::from(n)));
            }
        }
    }
    out
}

pub fn build(t0: &[String], t1: &[String], g0: &[String], g1: &[String]) -> R<String> {
    let (r, c) = (rows(t0, true), consts(t1));
    let (g, a) = (rows(g0, false), consts(g1));
    if !(r.len() > 1200 && r[0].starts_with("[2,") && c.contains("\"241\":")) {
        return Err("unexpected table layout".into());
    }
    if !(g.len() > 700 && g[1].starts_with("[2,\"3\",") && a.contains("\"212\":")) {
        return Err("unexpected gap table layout".into());
    }
    Ok(format!(
        "{{\"source\":{},\"x\":\"4000000000000000000\",\"t0\":[{}],\"C\":{c},\"g0\":[{}],\"A\":{a}}}",
        Value::from("T. Oliveira e Silva, https://sweet.ua.pt/tos/goldbach.html and gaps.html (t0, t1; 2012)"),
        r.join(","),
        g.join(",")
    ))
}

pub fn run(args: &[String]) -> R<i32> {
    if args.len() < 4 {
        return Err("usage: make-paper-json t0.txt.gz t1.txt.gz gaps_t0.txt.gz gaps_t1.txt.gz > client/paper.json".into());
    }
    let [t0, t1, g0, g1] = [0, 1, 2, 3].map(|i| read_gz(&args[i]));
    print!("{}", build(&t0?, &t1?, &g0?, &g1?)?);
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_float_repr() {
        for (x, s) in [(1.0, "1.0"), (1.3203236316937392, "1.3203236316937392"), (1.5e-5, "1.5e-05"), (0.0001, "0.0001"),
                       (2.5e20, "2.5e+20"), (1e16, "1e+16"), (123456.0, "123456.0"), (3.2e-7, "3.2e-07")] {
            assert_eq!(py_float(x), s);
        }
    }

    #[test]
    fn goldbach_rows() {
        assert_eq!(table_row("  12     97   3041*    6215", true), Some((97, Some("3041".into()), "6215".into())));
        assert_eq!(table_row("1 241 ? 1234", true), Some((241, None, "1234".into())));
        assert_eq!(table_row("# header", true), None);
        assert_eq!(table_row(" 2 3* 3023463123235320", false), Some((2, Some("3".into()), "3023463123235320".into())));
    }

    #[test]
    fn constants_rows() {
        assert_eq!(const_row(" 241  3  17  1.2345e-05 "), Some(("241".into(), 3, 1.2345e-5)));
        assert_eq!(const_row("241 3 17 1.2345"), None);
        assert_eq!(const_row("241 3 17 1.2e+5 x"), None);
    }

    /// Every constant in the checked-in paper.json prints back the way Python wrote it.
    #[test]
    fn checked_in_floats_round_trip() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../client/paper.json");
        let Ok(txt) = std::fs::read_to_string(path) else { return };
        let (from, to) = (txt.find("\"C\":").unwrap(), txt.find("\"g0\":").unwrap());
        let tail = &txt[to..];
        let mut n = 0;
        for part in [&txt[from..to], &tail[tail.find("\"A\":").unwrap()..]] {
            for tok in part.split("],[").flat_map(|t| t.rsplit_once(',')).map(|(_, v)| v.trim_end_matches([']', '}'])) {
                if let Ok(x) = tok.parse::<f64>() {
                    if tok.contains('.') {
                        assert_eq!(py_float(x), tok);
                        n += 1;
                    }
                }
            }
        }
        assert!(n > 100, "only {n} floats checked");
    }

    #[test]
    fn layout_is_checked() {
        assert!(build(&[], &[], &[], &[]).is_err());
    }

    #[test]
    fn synthetic_tables_build() {
        let mut t0 = vec!["header".to_string()];
        for (i, p) in [2u64, 3].into_iter().chain(5..1300).enumerate() {
            t0.push(format!("{i} {p} {}{} 99", p * 2, if p % 7 == 0 { "*" } else { "" }));
        }
        let t1 = vec!["241 1 3 1.5e-05".to_string(), "241 2 3 2.5e+00".to_string()];
        let mut g0 = vec![];
        for g in 1..800u64 {
            g0.push(format!("{g} {}  7", g + 1));
        }
        g0.insert(2, "2 3 7".into());
        let g1 = vec!["212 2 3 1.0e+00".to_string()];
        let j = build(&t0, &t1, &g0, &g1).unwrap();
        let v: Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["C"]["241"][0][1], 1.5e-5);
        assert!(j.contains("\"241\":[[1,1.5e-05],[2,2.5]]") || j.contains("[[1,1.5e-05],[2,2.5]]"));
        assert_eq!(v["x"], "4000000000000000000");
    }
}
