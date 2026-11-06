//! Search and sharing basics of every public page (PAGES), served by a local
//! coordinator: one h1, title and description lengths, canonical URL, Open
//! Graph and X card tags, a parseable JSON-LD block, internal links that
//! resolve; robots.txt names the sitemap, the sitemap lists exactly PAGES, the
//! share image is a 1200x630 PNG, the IndexNow key file holds its key.
mod common;
use common::*;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};

const CONTENT_SIGNAL: &str = "ai-train=yes, search=yes, ai-input=yes";
const INDEXNOW_KEY_FILE: &str = "indexnow-070ede81046057733a264f2be8185e89.txt";

#[derive(Default)]
struct Page {
    meta: HashMap<String, String>,
    links: HashMap<String, String>,
    hrefs: Vec<String>,
    h1: usize,
    title: String,
    ld: Vec<String>,
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let mut rest = tag;
    while let Some(i) = rest.find(name) {
        let before_ok = rest[..i].chars().last().is_none_or(|c| c.is_whitespace());
        let after = &rest[i + name.len()..];
        if before_ok && after.starts_with('=') {
            let v = &after[1..];
            let q = v.chars().next()?;
            if q == '"' || q == '\'' {
                return Some(v[1..1 + v[1..].find(q)?].to_string());
            }
            return Some(v.split(|c: char| c.is_whitespace() || c == '>').next()?.to_string());
        }
        rest = after;
    }
    None
}

fn parse(html: &str) -> Page {
    let mut p = Page::default();
    let mut i = 0;
    while let Some(o) = html[i..].find('<') {
        let at = i + o;
        let Some(e) = html[at..].find('>') else { break };
        let tag = &html[at + 1..at + e];
        let name = tag.split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("").to_ascii_lowercase();
        i = at + e + 1;
        let body = |end: &str, from: usize| html[from..].find(end).map(|x| html[from..from + x].to_string()).unwrap_or_default();
        match name.as_str() {
            "meta" => {
                if let Some(k) = attr(tag, "name").or_else(|| attr(tag, "property")) {
                    p.meta.insert(k, attr(tag, "content").unwrap_or_default());
                }
            }
            "link" => {
                if let Some(r) = attr(tag, "rel") {
                    p.links.insert(r, attr(tag, "href").unwrap_or_default());
                }
            }
            "a" => {
                if let Some(h) = attr(tag, "href").filter(|h| !h.is_empty()) {
                    p.hrefs.push(h);
                }
            }
            "h1" => p.h1 += 1,
            "title" => p.title += &body("</title>", i),
            "script" if attr(tag, "type").as_deref() == Some("application/ld+json") => p.ld.push(body("</script>", i)),
            _ => {}
        }
    }
    p
}

fn md(srv: &Server, path: &str) -> Resp {
    srv.get(path, &[("Accept", "text/markdown")])
}

fn rpc(srv: &Server, m: Value) -> Value {
    srv.post("/mcp", &m.to_string()).json()
}

#[test]
fn seo_and_agent_readiness() {
    let mut srv = Server::new("seo");
    srv.start();
    let mut c = Checks::default();
    for path in PAGES {
        let r = srv.get(path, &[]);
        let pg = parse(&r.text());
        let url = format!("{SITE}{path}");
        let m = &pg.meta;
        let desc = m.get("description").map(|s| s.chars().count()).unwrap_or(0);
        c.check(r.header("Content-Type").unwrap_or("").starts_with("text/html"), format!("{path}: served as HTML"));
        c.check(pg.h1 == 1, format!("{path}: exactly one h1 ({})", pg.h1));
        let tl = pg.title.chars().count();
        c.check((10..=65).contains(&tl), format!("{path}: title {tl} chars (10..65)"));
        c.check((50..=165).contains(&desc), format!("{path}: description {desc} chars (50..165)"));
        c.check(pg.links.get("canonical") == Some(&url), format!("{path}: canonical {:?}", pg.links.get("canonical")));
        c.check(
            ["og:title", "og:description", "og:image", "og:type", "twitter:card"].iter().all(|k| m.get(*k).is_some_and(|v| !v.is_empty())),
            format!("{path}: Open Graph and X card tags"),
        );
        c.check(m.get("og:url").unwrap_or(&url) == &url, format!("{path}: og:url matches the canonical URL"));
        let ok = !pg.ld.is_empty() && pg.ld.iter().all(|b| serde_json::from_str::<Value>(b).is_ok_and(|v| v.get("@context").is_some()));
        c.check(ok, format!("{path}: JSON-LD parses"));
        let internal: BTreeSet<String> =
            pg.hrefs.iter().filter(|h| h.starts_with('/')).map(|h| h.split('#').next().unwrap().to_string()).filter(|h| !h.is_empty()).collect();
        let bad: Vec<&String> = internal.iter().filter(|h| srv.try_get(h, &[]).is_none_or(|r| r.status >= 400)).collect();
        c.check(bad.is_empty(), format!("{path}: internal links resolve {bad:?}"));
    }
    let robots = srv.get("/robots.txt", &[]).text();
    c.check(robots.contains(&format!("Sitemap: {SITE}/sitemap.xml")), "robots.txt names the sitemap");
    let sm = srv.get("/sitemap.xml", &[]).text();
    let mut locs: Vec<String> = sm.split("<loc>").skip(1).map(|x| x.split("</loc>").next().unwrap().to_string()).collect();
    locs.sort();
    let mut want: Vec<String> = PAGES.iter().map(|p| format!("{SITE}{p}")).collect();
    want.sort();
    c.check(locs == want, format!("sitemap lists exactly the public pages {locs:?}"));
    c.check(sm.matches("<lastmod>").count() == PAGES.len(), "sitemap: lastmod on every URL");
    let png = srv.get("/og.png", &[]).body;
    let (w, h) = if png.starts_with(b"\x89PNG\r\n\x1a\n") {
        (u32::from_be_bytes(png[16..20].try_into().unwrap()), u32::from_be_bytes(png[20..24].try_into().unwrap()))
    } else {
        (0, 0)
    };
    c.check((w, h) == (1200, 630), format!("share image is a 1200x630 PNG ({w}x{h})"));
    let r = srv.get("/favicon.ico", &[]);
    c.check(
        r.header("Content-Type") == Some("image/x-icon") && r.body.starts_with(&[0, 0, 1, 0]) && u16::from_le_bytes([r.body[4], r.body[5]]) >= 1,
        "/favicon.ico is an icon file (browsers and crawlers ask for it at the root)",
    );
    // agent readiness: content signals, Markdown for agents, Link headers, API catalog, skills index, MCP server and card
    c.check(robots.contains(&format!("Content-Signal: {CONTENT_SIGNAL}")), "robots.txt declares content signals");
    for path in PAGES {
        let r = md(&srv, path);
        let body = r.text();
        let tagged = body.as_bytes().windows(2).enumerate().any(|(i, w)| {
            w[0] == b'<' && {
                let rest = &body[i + 1..];
                let rest = rest.strip_prefix('/').unwrap_or(rest);
                let n = rest.bytes().take_while(u8::is_ascii_lowercase).count();
                n > 0 && matches!(rest.as_bytes().get(n), Some(b' ') | Some(b'>'))
            }
        });
        c.check(
            r.header("Content-Type").unwrap_or("").starts_with("text/markdown") && (body.starts_with("# ") || body.starts_with('[')) && !tagged,
            format!("{path}: Accept: text/markdown gets Markdown"),
        );
        c.check(
            r.header("Cache-Control").unwrap_or("").contains("no-store") && r.header("X-Markdown-Tokens").and_then(|t| t.parse::<i64>().ok()).unwrap_or(0) > 0,
            format!("{path}: Markdown is not edge-cached, has a token estimate"),
        );
    }
    let links = srv.get("/", &[]).all("Link").join(",");
    c.check(links.contains("rel=\"api-catalog\"") && links.contains("rel=\"service-desc\""), "homepage Link headers");
    let r = srv.get("/.well-known/api-catalog", &[]);
    let cat = r.json();
    let entries = cat["linkset"].as_array().unwrap();
    c.check(
        r.header("Content-Type") == Some("application/linkset+json")
            && entries.iter().all(|e| !e["anchor"].is_null() && e["service-desc"].as_array().is_some_and(|a| !a.is_empty()) && e["service-doc"].as_array().is_some_and(|a| !a.is_empty())),
        "API catalog (RFC 9727)",
    );
    for e in entries {
        for rel in ["service-desc", "service-doc", "status"] {
            for ln in e.get(rel).and_then(Value::as_array).into_iter().flatten() {
                let href = ln["href"].as_str().unwrap();
                c.check(srv.get(&href.replace(SITE, ""), &[]).status == 200, format!("api-catalog {rel} {href} resolves"));
            }
        }
    }
    let idx = srv.get("/.well-known/agent-skills/index.json", &[]).json();
    let sk = &idx["skills"][0];
    let skill = srv.get(&sk["url"].as_str().unwrap().replace(SITE, ""), &[]).body;
    c.check(
        idx["$schema"].as_str().unwrap().ends_with("/0.2.0/schema.json") && sk["digest"] == format!("sha256:{}", sha256_hex(&skill)),
        "agent skills index: digest matches SKILL.md",
    );
    srv.get("/openapi.json", &[]).json();
    let pa = srv.get("/api/partition?n=3325581707333960528", &[]).json();
    c.check(
        pa["p"] == 9781 && pa["q"] == (3325581707333960528i64 - 9781).to_string() && pa.get("verifiedByNative").is_some(),
        "/api/partition: p(n) and coverage",
    );
    let full = srv.get("/llms-full.txt", &[]).text();
    c.check(full.contains("/api/partition") && full.contains("How to cite"), "llms-full.txt served");
    let auth = srv.get("/auth.md", &[]);
    let text = auth.text();
    c.check(
        auth.header("Content-Type").unwrap_or("").starts_with("text/markdown")
            && text.lines().next().unwrap_or("").contains("auth.md")
            && text.starts_with("# ")
            && text.contains("You are an agent")
            && text.contains("`anonymous`")
            && ["Supported registration methods", "Step 1 — Register and provision a credential", "Step 2 — Use the credential", "Step 3 — Rotation and revocation"]
                .iter()
                .all(|h| text.contains(&format!("## {h}")))
            && text.contains("POST https://goldbach.net01.io/api/register"),
        "auth.md: the self-contained registration flow the authMd check recognises (em-dash step headings)",
    );
    let card = srv.get("/.well-known/mcp/server-card.json", &[]).json();
    c.check(
        card["serverInfo"]["name"].as_str().is_some_and(|s| !s.is_empty()) && card["endpoint"].as_str().unwrap().ends_with("/mcp") && card["capabilities"].get("tools").is_some(),
        "MCP server card",
    );
    let init = rpc(&srv, json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}));
    c.check(init["result"]["protocolVersion"] == "2025-06-18", "MCP initialize");
    let names = |v: &Value| -> Vec<String> {
        let mut n: Vec<String> = v.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
        n.sort();
        n
    };
    let tools = rpc(&srv, json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    c.check(names(&tools["result"]["tools"]) == names(&card["tools"]), format!("MCP tools match the card {:?}", names(&tools["result"]["tools"])));
    // the record of Oliveira e Silva, Herzog and Pardi below 4e18
    let got = rpc(&srv, json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                               "params": {"name": "goldbach_partition", "arguments": {"n": "3325581707333960528"}}}));
    c.check(got["result"]["structuredContent"]["p"] == 9781, "MCP goldbach_partition: p(3325581707333960528) = 9781");
    let key = &INDEXNOW_KEY_FILE["indexnow-".len()..INDEXNOW_KEY_FILE.len() - ".txt".len()];
    c.check(srv.get(&format!("/{INDEXNOW_KEY_FILE}"), &[]).text().trim() == key && key.len() >= 8, "IndexNow key file serves its key");
    c.done("SEO OK");
}
