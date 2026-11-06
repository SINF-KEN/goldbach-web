//! The plain-text board (server-rendered, also served raw at /board), the
//! Markdown form of the pages (Markdown for Agents) and the sitemap.
use crate::py::{self, float, int, str_of, truthy, wrap};
use crate::*;
use serde_json::Value;

fn sci(x: f64) -> String {
    if x < 10000.0 {
        return (x.trunc() as i128).to_string();
    }
    let e = (x.trunc() as i128).to_string().len() - 1;
    let p: f64 = format!("1e{e}").parse().unwrap();
    format!("{:.2}e{e}", x / p)
}

fn grp(n: i128) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

fn fi(v: &Value) -> i128 {
    int(v).unwrap_or(0)
}

fn ff(v: &Value) -> f64 {
    float(v).unwrap_or(0.0)
}

/// v or default when v is missing or null (dict.get(k, default) with a None-able value kept as is).
fn get<'a>(o: &'a Value, k: &str) -> Option<&'a Value> {
    o.as_object().and_then(|m| m.get(k))
}

/// The board as plain text: 76 columns, or 44 for phones.
pub fn board(s: &Value, narrow: bool) -> String {
    let w: usize = if narrow { 44 } else { 76 };
    let span = RANGE_HI - RANGE_LO;
    let empty = Value::Object(Default::default());
    let nat = if truthy(&s["native"]) { &s["native"] } else { &empty };
    let resume = get(nat, "resume").map(fi).unwrap_or(RANGE_LO);
    let nat_evens = (resume - RANGE_LO).max(0) / 2;
    let web_evens = fi(&s["web"]["evens"]);
    let done = 2.0 * (nat_evens + web_evens) as f64 / span as f64;
    let nat_rate = get(nat, "rate").filter(|v| truthy(v)).map(ff).unwrap_or(0.0);
    let web_rate: f64 = s["devices"].as_array().into_iter().flatten().map(|d| ff(&d["rate"])).sum();
    let left = (span as f64 / 2.0 - nat_evens as f64 - web_evens as f64).max(0.0);
    let eta = if nat_rate + web_rate != 0.0 { left / (nat_rate + web_rate) / 86400.0 } else { 0.0 };
    let nb: usize = if narrow { 30 } else { 50 };
    let bar = py::round(done * nb as f64).max(0) as usize;
    let low = s["web"]["lowest"].clone();
    let low = (if truthy(&low) { fi(&low) } else { RANGE_HI }) as f64 / 1e18;
    let stamp = utc_stamp(s["time"].as_f64().unwrap_or(0.0), "%Y-%m-%d %H:%M UTC");
    let wr = |t: &str, ind: &str| wrap(t, w, ind);
    let mut l: Vec<String> = Vec::new();
    if narrow {
        l.extend(["GOLDBACH VERIFICATION NETWORK".to_string(), stamp.clone(), "=".repeat(w)]);
    } else {
        l.push(format!("{:<width$}{stamp}", "GOLDBACH VERIFICATION NETWORK", width = w - 20));
        l.push("=".repeat(w));
    }
    l.extend(wr(
        "Every even number from 4e18 to 5e18 is checked to be a sum of two primes, exactly. A B580 GPU works up from 4e18, browsers work down from 5e18.",
        "",
    ));
    l.push(String::new());
    l.push("PROGRESS".into());
    l.push(format!("  [{}{}] {:7.3} %", "#".repeat(bar), ".".repeat(nb.saturating_sub(bar)), 100.0 * done));
    l.push(format!("  evens checked  {:>10}", sci((nat_evens + web_evens) as f64)));
    l.push(format!("  speed          {:>10} evens/s", sci(nat_rate + web_rate)));
    l.push(format!("  time left      {:10.1} days{}", eta, if narrow { "" } else { " at this speed" }));
    l.push(String::new());
    let web = &s["web"];
    let recheck = format!(
        "every browser slice is recomputed by a 2nd device: {} waiting, {} disputed",
        str_of(&web["open"]),
        str_of(&web["disputed"])
    );
    let bf = get(nat, "backfill").filter(|v| truthy(v));
    let (mut what, mut bspan, mut beta, mut bnow, mut bpct, mut brate) =
        (String::new(), String::new(), String::new(), String::new(), 0.0, 0.0);
    if let Some(bf) = bf {
        bpct = get(bf, "pct").filter(|v| truthy(v)).map(ff).unwrap_or(0.0);
        brate = get(bf, "rate").filter(|v| truthy(v)).map(ff).unwrap_or(0.0);
        what = format!(
            "backfilling {}",
            get(bf, "what").filter(|v| truthy(v)).map(str_of).unwrap_or_else(|| "statistics".into())
        );
        bspan = format!("{:.4}e18-{:.4}e18", fi(&bf["start"]) as f64 / 1e18, fi(&bf["limit"]) as f64 / 1e18);
        if get(bf, "eta_days").is_some_and(|v| !v.is_null()) {
            beta = format!("~{:.1} days left", ff(&bf["eta_days"]));
        }
        if get(bf, "now").is_some_and(truthy) {
            bnow = format!("slice in progress {:.0} %", 100.0 * ff(&bf["now"]["done"]));
        }
    }
    let both = || [beta.as_str(), bnow.as_str()].iter().filter(|x| !x.is_empty()).copied().collect::<Vec<_>>().join(", ");
    let slices = get(nat, "slices").map(str_of).unwrap_or_else(|| "0".into());
    if narrow {
        if bf.is_some() {
            l.push(format!("  B580      {what} {bpct:5.1} %"));
            l.push(format!("            {bspan}, {}/s", sci(brate)));
            if !beta.is_empty() || !bnow.is_empty() {
                l.push(format!("            {}", both()));
            }
            l.push(format!("            then main range {:.6}e18", resume as f64 / 1e18));
        } else {
            l.push(format!("  B580      4.000000e18 -> {:.6}e18", resume as f64 / 1e18));
            l.push(format!("            {}/s, {slices} slices", sci(nat_rate)));
        }
        l.push(format!("  browsers  5.000000e18 <- {low:.6}e18"));
        l.push(format!("            {}/s, {} slices", sci(web_rate), str_of(&web["verified"])));
        l.extend(wr(&recheck, &" ".repeat(12)));
    } else {
        if bf.is_some() {
            l.push(format!("  B580 native    {what} {bpct:6.2} %   {bspan}   {}/s", sci(brate)));
            if !beta.is_empty() || !bnow.is_empty() {
                l.push(format!("                 {}", both()));
            }
            l.push(format!("                 main range: 4.000000e18 -> {:.6}e18, resumes afterwards", resume as f64 / 1e18));
        } else {
            l.push(format!(
                "  B580 native    4.000000e18 -> {:.6}e18   {}/s   {slices} slices",
                resume as f64 / 1e18,
                sci(nat_rate)
            ));
        }
        l.push(format!("  browsers       5.000000e18 <- {low:.6}e18   {}/s   {} slices", sci(web_rate), str_of(&web["verified"])));
        l.extend(wr(&recheck, &" ".repeat(17)));
    }
    if truthy(&s["alert"]) {
        let a = &s["alert"];
        l.push(String::new());
        l.extend(wr(&format!("!! slice #{} reported {} -- it is being re-verified", str_of(&a["slice"]), str_of(&a["result"])), "  "));
    }
    l.push(String::new());
    let ridges = s["ridges"].as_array().cloned().unwrap_or_default();
    if narrow {
        l.extend(["TOP 30 RIDGES".into(), "p(n) = the smallest prime p with n - p prime".into(), String::new()]);
        l.push(format!("{:>2}  {:<25} {:>5} by", "#", "even n", "p(n)"));
        for (i, r) in ridges.iter().enumerate() {
            l.push(format!("{:>2}  {:<25} {:>5} {}", i + 1, grp(fi(&r["n"])), str_of(&r["p"]), str_of(&r["src"])));
        }
    } else {
        l.push("TOP 30 RIDGES  p(n) = the smallest prime p with n - p also prime".into());
        l.push(format!("  {:>2}  {:<26} {:>5}  {:<26} by", "#", "even n", "p(n)", "q = n - p"));
        for (i, r) in ridges.iter().enumerate() {
            let n = fi(&r["n"]);
            l.push(format!(
                "  {:>2}  {:<26} {:>5}  {:<26} {}",
                i + 1,
                grp(n),
                str_of(&r["p"]),
                grp(n - fi(&r["p"])),
                str_of(&r["src"])
            ));
        }
    }
    if ridges.is_empty() {
        l.push("  (none yet)".into());
    }
    l.push(String::new());
    if narrow {
        l.extend(["HOW RARE IS A LARGE p(n)?".into(), "evens per band of p(n), log scale".into()]);
    } else {
        l.push("HOW RARE IS A LARGE p(n)?  evens per band of p(n), log scale".into());
    }
    let mut counts: Vec<(i128, i128)> = Vec::new();
    let mut add = |p: i128, v: i128| match counts.iter_mut().find(|c| c.0 == p) {
        Some(c) => c.1 += v,
        None => counts.push((p, v)),
    };
    for e in s["nativeHist"].as_array().into_iter().flatten() {
        add(fi(&e[0]), fi(&e[1]));
    }
    for e in s["hist"].as_array().into_iter().flatten() {
        let p = fi(&e[0]);
        if p >= 3000 {
            add(p, fi(&e[1]));
        }
    }
    let bands: Vec<(i128, i128)> = (3000..9782).step_by(500).map(|lo| (lo, (lo + 500).min(9782))).collect();
    let vals: Vec<i128> =
        bands.iter().map(|&(lo, hi)| counts.iter().filter(|c| lo <= c.0 && c.0 < hi).map(|c| c.1).sum()).collect();
    let top = vals.iter().copied().max().filter(|&m| m != 0).unwrap_or(1);
    let bw: usize = if narrow { 16 } else { 36 };
    for (&(lo, hi), &v) in bands.iter().zip(&vals) {
        let wd = if v != 0 {
            py::round(bw as f64 * (1.0 + v as f64).log10() / (1.0 + top as f64).log10()).max(0) as usize
        } else {
            0
        };
        let bar = "#".repeat(wd);
        l.push(if narrow {
            format!("{lo:>4}-{:<4} {bar:<bw$} {:>13}", hi - 1, grp(v))
        } else {
            format!("  {lo:>5}-{:<5}  {bar:<bw$} {:>15}", hi - 1, grp(v))
        });
    }
    l.push(String::new());
    l.push("DEVICES NOW".into());
    let devices = s["devices"].as_array().cloned().unwrap_or_default();
    for d in &devices {
        let name = str_of(&d["name"]);
        if narrow {
            l.push(format!(
                "  {:<14} #{:<5} {:5.1}% {}/s",
                py::chars(&name, 14),
                str_of(&d["slice"]),
                100.0 * ff(&d["progress"]),
                sci(ff(&d["rate"]))
            ));
        } else {
            l.push(format!(
                "  {:<24} slice #{:<6} {:5.1} %   {}/s",
                py::chars(&name, 24),
                str_of(&d["slice"]),
                100.0 * ff(&d["progress"]),
                sci(ff(&d["rate"]))
            ));
        }
    }
    if devices.is_empty() {
        l.extend(wr("no browser is verifying right now (start one below)", "  "));
    }
    let or0 = |k: &str| get(s, k).map(str_of).unwrap_or_else(|| "0".into());
    l.extend(wr(
        &format!("devices seen in the last day: {}, trusted: {}, banned: {}", str_of(&s["clients"]), or0("trusted"), or0("banned")),
        "  ",
    ));
    l.extend(wr(
        &format!(
            "a new device's slice counts once a device on another network agrees; after {TRUST_AFTER} agreements a device is trusted (its slices count alone, {}% re-checked).",
            (SPOT * 100.0) as i64
        ),
        "  ",
    ));
    l.push(String::new());
    let head = format!("B580 LATEST SLICES{}", if bf.is_some() { " (gap backfill)" } else { "" });
    if narrow {
        l.extend([head, "uploaded as each slice is banked".into()]);
    } else {
        l.push(format!("{head}  uploaded as each slice is banked"));
    }
    let src = bf.unwrap_or(nat);
    for r in get(src, "recent").and_then(Value::as_array).into_iter().flatten().take(8) {
        let at: String = get(r, "at").filter(|v| truthy(v)).map(str_of).unwrap_or_default();
        let at = py::chars(&at, 16).replace('T', " ");
        let pmax = get(r, "pmax").filter(|v| truthy(v)).map(str_of).unwrap_or_else(|| "-".into());
        if narrow {
            let tail: String = at.chars().skip(5).collect();
            l.push(format!("  {:.5}e18 {tail:<11} {:4.0} s p {pmax}", fi(&r["start"]) as f64 / 1e18, ff(&r["gpu_s"])));
        } else {
            l.push(format!(
                "  {:<26} {at:<16} {:6.0} s  {}/s  max p(n) {pmax}",
                grp(fi(&r["start"])),
                ff(&r["gpu_s"]),
                sci(ff(&r["rate"]))
            ));
        }
    }
    if get(nat, "primes").is_some_and(truthy) {
        l.extend(wr(&format!("primes counted {}, twin primes {}", sci(fi(&nat["primes"]) as f64), sci(fi(&nat["twins"]) as f64)), "  "));
    }
    l.join("\n") + "\n"
}

/// time.strftime(fmt, time.gmtime(t)) for the few fields used here.
pub fn utc_stamp(t: f64, fmt: &str) -> String {
    let secs = t.floor() as i64;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    // civil from days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    fmt.replace("%Y", &format!("{y:04}"))
        .replace("%m", &format!("{m:02}"))
        .replace("%d", &format!("{d:02}"))
        .replace("%H", &format!("{:02}", rem / 3600))
        .replace("%M", &format!("{:02}", rem % 3600 / 60))
        .replace("%S", &format!("{:02}", rem % 60))
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// datetime.fromisoformat(s).timestamp() for "YYYY-MM-DD[THH:MM[:SS[.f]]][Z|±HH:MM]"
/// (a time without an offset is taken as UTC, the server's zone).
pub fn iso_timestamp(s: &str) -> Option<f64> {
    let s = s.trim();
    let num = |a: usize, b: usize| s.get(a..b).and_then(|x| x.parse::<i64>().ok());
    let (y, m, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    if s.as_bytes().get(4) != Some(&b'-') || s.as_bytes().get(7) != Some(&b'-') || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let mut t = days_from_civil(y, m, d) as f64 * 86400.0;
    let rest = &s[10..];
    if rest.is_empty() {
        return Some(t);
    }
    let rest = rest.strip_prefix(['T', ' '])?;
    let (clock, off) = match rest.find(['Z', '+', '-']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let mut parts = clock.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let mi: f64 = parts.next().map(|x| x.parse().ok()).unwrap_or(Some(0.0))?;
    let sec: f64 = parts.next().map(|x| x.parse().ok()).unwrap_or(Some(0.0))?;
    t += h * 3600.0 + mi * 60.0 + sec;
    if !off.is_empty() && off != "Z" {
        let sign = if off.starts_with('-') { -1.0 } else { 1.0 };
        let o = &off[1..];
        let (oh, om) = o.split_once(':').unwrap_or((o.get(..2)?, o.get(2..).unwrap_or("0")));
        let om = if om.is_empty() { "0" } else { om };
        t -= sign * (oh.parse::<f64>().ok()? * 3600.0 + om.parse::<f64>().ok()? * 60.0);
    }
    Some(t)
}

/// True when an Accept header prefers text/markdown to text/html.
pub fn wants_markdown(accept: &str) -> bool {
    let mut q: Vec<(String, f64)> = Vec::new();
    for part in accept.to_lowercase().split(',') {
        let mut it = part.split(';').map(|x| x.trim());
        let typ = it.next().unwrap_or("").to_string();
        let mut w = 1.0;
        for p in it {
            if let Some(x) = p.strip_prefix("q=") {
                w = x.trim().parse::<f64>().unwrap_or(0.0);
            }
        }
        match q.iter_mut().find(|e| e.0 == typ) {
            Some(e) => e.1 = e.1.max(w),
            None => q.push((typ, w)),
        }
    }
    let g = |k: &str| q.iter().find(|e| e.0 == k).map(|e| e.1);
    let md = g("text/markdown").unwrap_or(0.0);
    md > 0.0 && md >= g("text/html").or_else(|| g("text/*")).or_else(|| g("*/*")).unwrap_or(0.0)
}

// ---- HTML -> Markdown (server.py Markdown, an html.parser.HTMLParser) --------

const SKIP: &[&str] = &["head", "script", "style", "svg", "button", "noscript"];
const VOID: &[&str] = &["br", "hr", "img", "input", "meta", "link", "source", "wbr"];
const BLOCK: &[&str] =
    &["p", "div", "section", "figure", "figcaption", "main", "ul", "ol", "li", "h1", "h2", "h3", "h4", "h5", "h6", "pre", "table", "tr"];

struct Md {
    blocks: Vec<String>,
    cur: String,
    skip: Vec<String>,
    pre: bool,
    link: Option<String>,
}

impl Md {
    fn flush(&mut self) {
        let text = if self.pre { self.cur.clone() } else { self.cur.split_whitespace().collect::<Vec<_>>().join(" ") };
        if !text.trim().is_empty() {
            self.blocks.push(text);
        }
        self.cur.clear();
    }

    fn start(&mut self, tag: &str, attrs: &[(String, Option<String>)]) {
        let attr = |k: &str| attrs.iter().rev().find(|a| a.0 == k).and_then(|a| a.1.clone());
        if !self.skip.is_empty() || SKIP.contains(&tag) || attr("data-md").as_deref() == Some("skip") {
            if !VOID.contains(&tag) {
                self.skip.push(tag.to_string());
            }
            return;
        }
        if BLOCK.contains(&tag) || tag == "br" || tag == "hr" {
            self.flush();
        }
        match tag {
            "hr" => {
                if self.blocks.last().is_some_and(|b| b != "---") {
                    self.blocks.push("---".into());
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => self.cur = format!("{} ", "#".repeat((tag.as_bytes()[1] - b'0') as usize)),
            "li" => self.cur = "- ".into(),
            "pre" => self.pre = true,
            "a" => {
                if let Some(href) = attr("href").filter(|h| !h.is_empty()) {
                    self.link = Some(if href.starts_with('/') { format!("{SITE}{href}") } else { href });
                    self.cur.push('[');
                }
            }
            "sup" => self.cur.push('^'),
            "sub" => self.cur.push('_'),
            "cite" => self.cur.push('*'),
            _ => {}
        }
    }

    fn end(&mut self, tag: &str) {
        if !self.skip.is_empty() {
            if self.skip.last().map(String::as_str) == Some(tag) {
                self.skip.pop();
            }
            return;
        }
        if tag == "a" && self.link.is_some() {
            let l = self.link.take().unwrap();
            self.cur.push_str(&format!("]({l})"));
        } else if tag == "cite" {
            self.cur.push('*');
        } else if tag == "pre" {
            self.cur = format!("```\n{}\n```", self.cur.trim_matches('\n'));
            self.flush();
            self.pre = false;
        } else if BLOCK.contains(&tag) {
            self.flush();
        }
    }

    fn data(&mut self, d: &str) {
        if self.skip.is_empty() {
            self.cur.push_str(d);
        }
    }
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[1..].find(|c: char| !(c.is_ascii_alphanumeric() || c == '#')).map(|j| j + 1).unwrap_or(rest.len());
        let name = &rest[1..end];
        let semi = rest[end..].starts_with(';');
        let ch: Option<String> = if let Some(n) = name.strip_prefix('#') {
            let code = if let Some(h) = n.strip_prefix(['x', 'X']) { u32::from_str_radix(h, 16).ok() } else { n.parse().ok() };
            code.map(|c| char::from_u32(c).unwrap_or('\u{fffd}').to_string())
        } else {
            match name {
                "amp" => Some("&"),
                "lt" => Some("<"),
                "gt" => Some(">"),
                "quot" => Some("\""),
                "apos" if semi => Some("'"),
                "nbsp" => Some("\u{a0}"),
                "ndash" if semi => Some("\u{2013}"),
                "mdash" if semi => Some("\u{2014}"),
                "middot" => Some("\u{b7}"),
                "times" => Some("\u{d7}"),
                "hellip" if semi => Some("\u{2026}"),
                "copy" => Some("\u{a9}"),
                _ => None,
            }
            .map(String::from)
        };
        match ch {
            Some(c) if !name.is_empty() => {
                out.push_str(&c);
                rest = &rest[end + semi as usize..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn parse_attrs(s: &str) -> Vec<(String, Option<String>)> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        let k0 = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' && b[i] != b'>' && !(b[i] == b'/' && i > k0) {
            i += 1;
        }
        if i == k0 {
            break;
        }
        let key = s[k0..i].to_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            let v = if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                i += 1;
                let v0 = i;
                while i < b.len() && b[i] != q {
                    i += 1;
                }
                let v = &s[v0..i];
                i += 1;
                v
            } else {
                let v0 = i;
                while i < b.len() && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                &s[v0..i]
            };
            out.push((key, Some(unescape(v))));
        } else {
            out.push((key, None));
        }
    }
    out
}

/// A page's HTML as Markdown for agents.
pub fn markdown(page: &str) -> String {
    let mut md = Md { blocks: vec![], cur: String::new(), skip: vec![], pre: false, link: None };
    let mut rest = page;
    let mut raw_until: Option<String> = None; // inside <script>/<style>: text up to its end tag
    while !rest.is_empty() {
        if let Some(tag) = raw_until.take() {
            let close = format!("</{tag}");
            let lower = rest.to_lowercase();
            let i = lower.find(&close).unwrap_or(rest.len());
            md.data(&rest[..i]);
            rest = &rest[i..];
            continue;
        }
        let Some(i) = rest.find('<') else {
            md.data(&unescape(rest));
            break;
        };
        if i > 0 {
            md.data(&unescape(&rest[..i]));
            rest = &rest[i..];
            continue;
        }
        if rest.starts_with("<!--") {
            rest = rest.find("-->").map(|j| &rest[j + 3..]).unwrap_or("");
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            rest = rest.find('>').map(|j| &rest[j + 1..]).unwrap_or("");
            continue;
        }
        let closing = rest.starts_with("</");
        let body0 = if closing { 2 } else { 1 };
        if !rest[body0..].starts_with(|c: char| c.is_ascii_alphabetic()) {
            md.data("<");
            rest = &rest[1..];
            continue;
        }
        // the tag's end: the first '>' outside a quoted attribute value
        let b = rest.as_bytes();
        let (mut j, mut q) = (body0, 0u8);
        while j < b.len() {
            match (q, b[j]) {
                (0, b'"') | (0, b'\'') => q = b[j],
                (0, b'>') => break,
                (x, c) if x != 0 && c == x => q = 0,
                _ => {}
            }
            j += 1;
        }
        let inner = &rest[body0..j.min(rest.len())];
        rest = if j < rest.len() { &rest[j + 1..] } else { "" };
        let name_end = inner.find(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>').unwrap_or(inner.len());
        let name = inner[..name_end].to_lowercase();
        if closing {
            md.end(&name);
            continue;
        }
        let self_closing = inner.trim_end().ends_with('/');
        let attrs = parse_attrs(&inner[name_end..]);
        md.start(&name, &attrs);
        if self_closing {
            md.end(&name);
        } else if name == "script" || name == "style" {
            raw_until = Some(name);
        }
    }
    md.flush();
    while md.blocks.last().is_some_and(|b| b == "---") {
        md.blocks.pop();
    }
    md.blocks.join("\n\n") + "\n"
}

/// sitemap.xml of PAGES; lastmod: the latest verified or native data.
pub fn sitemap(web: Option<f64>, native: &Value) -> String {
    let mut stamps = vec![web.unwrap_or(0.0)];
    let at = native["progress"]["recent"].as_array().and_then(|r| r.first()).map(|r| &r["at"]).filter(|a| truthy(a));
    let at = at.map(str_of).unwrap_or_default();
    if !at.is_empty() {
        if let Some(t) = iso_timestamp(&at) {
            stamps.push(t);
        }
    }
    let m = stamps.iter().copied().fold(f64::MIN, f64::max);
    let t = if m != 0.0 { m } else { crate::store::now() };
    let day = utc_stamp(t, "%Y-%m-%dT%H:%M:%SZ");
    let urls: String = PAGES
        .iter()
        .map(|(p, f, pr)| {
            format!("  <url><loc>{SITE}{p}</loc><lastmod>{day}</lastmod><changefreq>{f}</changefreq><priority>{pr}</priority></url>\n")
        })
        .collect();
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n{urls}</urlset>\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(sci(123.0), "123");
        assert_eq!(sci(12345.0), "1.23e4");
        assert_eq!(grp(1234567), "1 234 567");
        assert_eq!(utc_stamp(0.0, "%Y-%m-%d %H:%M UTC"), "1970-01-01 00:00 UTC");
        assert_eq!(iso_timestamp("2026-10-18T12:00:00+00:00"), Some(1792324800.0));
    }
}
