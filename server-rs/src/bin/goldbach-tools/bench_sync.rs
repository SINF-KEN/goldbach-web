//! Brings experiment PRs in line with the benchmark CI's verdicts: for every open PR whose
//! head commit carries a CI report (commit comment ending in <!-- decision: ... -->) but no
//! verdict on the PR yet, posts the report on the PR, labels it and closes it (decision
//! keep); merged PRs need nothing (GitHub marks them merged when main contains their
//! head). Safe to run any time.
//!   goldbach-tools bench-sync
use crate::util::{e, R};
use serde_json::Value;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

const REPO: &str = "SINF-KEN/goldbach-web";

fn gh(args: &[&str], input: Option<&str>) -> R<String> {
    let mut err = String::new();
    for attempt in 0..5 {
        // the network to GitHub drops now and then
        let mut c = Command::new("gh").args(args).stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|x| format!("gh: {x}"))?;
        if let (Some(i), Some(mut s)) = (input, c.stdin.take()) {
            s.write_all(i.as_bytes()).map_err(e)?;
        }
        let o = c.wait_with_output().map_err(e)?;
        if o.status.success() {
            return Ok(String::from_utf8_lossy(&o.stdout).into_owned());
        }
        err = String::from_utf8_lossy(&o.stderr).into_owned();
        if !err.contains("timeout") && !err.to_lowercase().contains("connection") {
            break;
        }
        std::thread::sleep(Duration::from_secs(5 * (attempt + 1)));
    }
    Err(err.trim().to_string())
}

/// `<!-- decision: WORD -->` -> WORD
fn decision(body: &str) -> Option<&str> {
    let r = body.split("<!-- decision: ").nth(1)?;
    let w = r.split_once(" -->")?.0;
    (!w.is_empty() && w.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')).then_some(w)
}

pub fn run(_: &[String]) -> R<i32> {
    let prs: Value = serde_json::from_str(&gh(&["pr", "list", "--repo", REPO, "--state", "open", "--limit", "200",
                                                 "--json", "number,headRefName,headRefOid,labels"], None)?).map_err(e)?;
    for pr in prs.as_array().ok_or("pr list")? {
        let labels: Vec<&str> = pr["labels"].as_array().into_iter().flatten().filter_map(|l| l["name"].as_str()).collect();
        if labels.iter().any(|l| ["bench: faster", "bench: not faster", "bench: not exact"].contains(l)) {
            continue;
        }
        let sha = pr["headRefOid"].as_str().unwrap_or("");
        let comments: Value = serde_json::from_str(&gh(&["api", &format!("repos/{REPO}/commits/{sha}/comments")], None)?).map_err(e)?;
        let Some(body) = comments.as_array().into_iter().flatten().filter_map(|c| c["body"].as_str()).filter(|b| b.contains("<!-- decision:")).last() else {
            continue;
        };
        let dec = decision(body).ok_or("a report without a readable decision")?;
        let n = pr["number"].to_string();
        let branch = pr["headRefName"].as_str().unwrap_or("");
        gh(&["pr", "comment", &n, "--repo", REPO, "--body-file", "-"], Some(body))?;
        if dec == "merge" {
            println!("#{n} {branch}: merge verdict (main will show it merged)");
            continue;
        }
        let exact = body.contains("results identical to main in every repetition | yes") && body.contains("GOLDEN OK");
        let label = if exact { "bench: not faster" } else { "bench: not exact" };
        gh(&["pr", "edit", &n, "--repo", REPO, "--add-label", label, "--remove-label", "bench: running"], None)?;
        let msg = format!("Closed by the benchmark CI: {}", if exact { "exact, but **not significantly faster**." } else { "**not exact**." });
        gh(&["pr", "close", &n, "--repo", REPO, "--comment", &msg], None)?;
        println!("#{n} {branch}: {label}, closed");
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_decision() {
        assert_eq!(decision("report\n<!-- borderline -->\n<!-- decision: keep -->"), Some("keep"));
        assert_eq!(decision("<!-- decision: merge -->"), Some("merge"));
        assert_eq!(decision("nothing"), None);
    }
}
