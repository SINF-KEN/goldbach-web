//! The bits of Python semantics the coordinator's behaviour depends on: int()
//! and str() of JSON values, truthiness, round() (half to even), float repr,
//! textwrap.wrap, so results, hashes and the board come out byte-identical.
use serde_json::Value;

/// A Python exception's message (what str(e) gives), for the "why" of a rejection.
pub type PyErr = String;

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// repr() of a str (single quotes unless the text has one and no double quote).
pub fn str_repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::from(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c)
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

const PY_WS: &[char] = &[' ', '\t', '\n', '\r', '\x0b', '\x0c'];

/// int(s) for a str: surrounding whitespace, a sign, digits with single
/// underscores between them. Values beyond 128 bits are refused (Python would
/// take them; no honest result comes near).
fn int_of_str(s: &str) -> Result<i128, PyErr> {
    let bad = || format!("invalid literal for int() with base 10: {}", str_repr(s));
    let t = s.trim_matches(PY_WS);
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return Err(bad());
    }
    let mut x: i128 = 0;
    for c in digits.chars() {
        if c == '_' {
            continue;
        }
        let d = c.to_digit(10).ok_or_else(bad)? as i128;
        x = x.checked_mul(10).and_then(|x| x.checked_add(d)).ok_or_else(bad)?;
    }
    Ok(if neg { -x } else { x })
}

/// int(v) for a JSON value.
pub fn int(v: &Value) -> Result<i128, PyErr> {
    match v {
        Value::Bool(b) => Ok(*b as i128),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i as i128)
            } else if let Some(u) = n.as_u64() {
                Ok(u as i128)
            } else {
                let f = n.as_f64().unwrap_or(f64::NAN);
                if f.is_nan() {
                    Err("cannot convert float NaN to integer".into())
                } else if f.is_infinite() {
                    Err("cannot convert float infinity to integer".into())
                } else {
                    Ok(f.trunc() as i128)
                }
            }
        }
        Value::String(s) => int_of_str(s),
        other => Err(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            type_name(other)
        )),
    }
}

/// float(v) for a JSON value.
pub fn float(v: &Value) -> Result<f64, PyErr> {
    match v {
        Value::Bool(b) => Ok(*b as u8 as f64),
        Value::Number(n) => Ok(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => {
            let t = s.trim_matches(PY_WS);
            t.replace('_', "")
                .parse::<f64>()
                .ok()
                .filter(|_| !t.starts_with('_') && !t.ends_with('_') && !t.contains("__"))
                .ok_or_else(|| format!("could not convert string to float: {}", str_repr(s)))
        }
        other => Err(format!("float() argument must be a string or a real number, not '{}'", type_name(other))),
    }
}

/// bool(v).
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// str(v) (what an f-string prints).
pub fn str_of(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => i.to_string(),
            (_, Some(u)) => u.to_string(),
            _ => float_repr(n.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// repr(float): the shortest round-trip digits, in exponent form below 1e-4 and from 1e16.
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let e = format!("{:e}", x); // shortest digits: "-1.2345e17"
    let (mant, exp) = e.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let neg = mant.starts_with('-');
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let sign = if neg { "-" } else { "" };
    if !(-4..16).contains(&exp) {
        let m = if digits.len() == 1 { digits.clone() } else { format!("{}.{}", &digits[..1], &digits[1..]) };
        return format!("{sign}{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs());
    }
    let point = exp + 1; // digits before the decimal point
    if point <= 0 {
        format!("{sign}0.{}{}", "0".repeat((-point) as usize), digits)
    } else if point as usize >= digits.len() {
        format!("{sign}{}{}.0", digits, "0".repeat(point as usize - digits.len()))
    } else {
        format!("{sign}{}.{}", &digits[..point as usize], &digits[point as usize..])
    }
}

/// round(x): to the nearest integer, ties to even.
pub fn round(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// round(x, n): correctly rounded to n decimals (as Python, via the exact value).
pub fn round_to(x: f64, n: usize) -> f64 {
    format!("{:.*}", n, x).parse().unwrap_or(x)
}

/// A JSON number for an integer that may not fit i64 (falls back to a float).
pub fn num(x: i128) -> Value {
    if let Ok(i) = i64::try_from(x) {
        Value::from(i)
    } else if let Ok(u) = u64::try_from(x) {
        Value::from(u)
    } else {
        Value::from(x as f64)
    }
}

/// A float JSON value (f64 that is not finite becomes null, as serde_json does).
pub fn fnum(x: f64) -> Value {
    serde_json::Number::from_f64(x).map(Value::Number).unwrap_or(Value::Null)
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    let d = sha2::Sha256::digest(data);
    let mut s = String::with_capacity(64);
    for b in d.iter() {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// s[:n] in characters.
pub fn chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

// ---- textwrap.wrap(text, width, initial_indent, subsequent_indent) ----------

fn is_w(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_letter(c: Option<char>) -> bool {
    c.is_some_and(|c| is_w(c) && !c.is_numeric())
}

fn is_wp(c: Option<char>) -> bool {
    c.is_some_and(|c| is_w(c) || "!\"'&.,?".contains(c))
}

/// TextWrapper.wordsep_re's split: whitespace runs, words (broken after the
/// hyphen of a hyphenated word), em-dashes.
fn chunks(t: &[char]) -> Vec<String> {
    let at = |i: usize| t.get(i).copied();
    let ws = |i: usize| at(i).is_some_and(|c| PY_WS.contains(&c));
    let dashes_then_w = |i: usize| {
        let mut j = i;
        while at(j) == Some('-') {
            j += 1;
        }
        j - i >= 2 && at(j).is_some_and(is_w)
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < t.len() {
        let s = i;
        if ws(i) {
            while ws(i) {
                i += 1;
            }
        } else if i > 0 && is_wp(at(i - 1)) && dashes_then_w(i) {
            while at(i) == Some('-') {
                i += 1;
            }
        } else {
            // nws+? then: hyphenated word | end of word | before an em-dash
            loop {
                i += 1;
                if at(i) == Some('-') {
                    let lb = (i >= 2 && is_letter(at(i - 1)) && is_letter(at(i - 2)))
                        || (i >= 3 && is_letter(at(i - 1)) && at(i - 2) == Some('-') && is_letter(at(i - 3)));
                    let la = is_letter(at(i + 1))
                        && (is_letter(at(i + 2)) || (at(i + 2) == Some('-') && is_letter(at(i + 3))));
                    if lb && la {
                        i += 1;
                        break;
                    }
                }
                if i >= t.len() || ws(i) {
                    break;
                }
                if is_wp(at(i - 1)) && dashes_then_w(i) {
                    break;
                }
            }
        }
        out.push(t[s..i].iter().collect());
    }
    out
}

pub fn wrap(text: &str, width: usize, indent: &str) -> Vec<String> {
    let t: Vec<char> = text.chars().map(|c| if PY_WS.contains(&c) { ' ' } else { c }).collect();
    let mut ch = chunks(&t);
    ch.reverse();
    let len = |s: &str| s.chars().count();
    let mut lines: Vec<String> = Vec::new();
    let ind_len = len(indent);
    while !ch.is_empty() {
        let mut cur: Vec<String> = Vec::new();
        let mut cur_len = 0;
        let w = width - ind_len;
        if ch.last().unwrap().trim().is_empty() && !lines.is_empty() {
            ch.pop();
        }
        while let Some(c) = ch.last() {
            let l = len(c);
            if cur_len + l <= w {
                cur_len += l;
                cur.push(ch.pop().unwrap());
            } else {
                break;
            }
        }
        if let Some(c) = ch.last() {
            let space_left = if w < 1 { 1 } else { w - cur_len };
            if len(c) > w && space_left > 0 {
                // _handle_long_word
                let cv: Vec<char> = c.chars().collect();
                let mut end = space_left;
                if cv.len() > space_left {
                    if let Some(h) = cv[..space_left].iter().rposition(|&x| x == '-') {
                        if h > 0 && cv[..h].iter().any(|&x| x != '-') {
                            end = h + 1;
                        }
                    }
                }
                let end = end.min(cv.len());
                cur.push(cv[..end].iter().collect());
                *ch.last_mut().unwrap() = cv[end..].iter().collect();
            }
        }
        if cur.last().is_some_and(|c| c.trim().is_empty()) {
            cur.pop();
        }
        if !cur.is_empty() {
            lines.push(format!("{indent}{}", cur.concat()));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reprs() {
        assert_eq!(float_repr(1e16), "1e+16");
        assert_eq!(float_repr(1.5e17), "1.5e+17");
        assert_eq!(float_repr(50000000.0), "50000000.0");
        assert_eq!(float_repr(0.0001), "0.0001");
        assert_eq!(float_repr(0.00001), "1e-05");
        assert_eq!(float_repr(123.456), "123.456");
        assert_eq!(round(2.5), 2);
        assert_eq!(round(3.5), 4);
    }

    #[test]
    fn ints() {
        assert_eq!(int(&Value::from(" +1_000 ")).unwrap(), 1000);
        assert!(int(&Value::from("1__0")).is_err());
        assert_eq!(int(&serde_json::json!(2.9)).unwrap(), 2);
    }

    #[test]
    fn wraps() {
        let w = wrap("aaa re-checked bbb", 8, "");
        assert_eq!(w, vec!["aaa re-", "checked", "bbb"]);
    }
}
