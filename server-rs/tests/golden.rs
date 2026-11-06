//! The pure functions of the coordinator against outputs recorded from the last
//! Python coordinator (tests/golden/README.md), through `goldbach-server dev ...`.
use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};

fn dev(args: &[&str], input: &str) -> String {
    let mut c = Command::new(env!("CARGO_BIN_EXE_goldbach-server"))
        .arg("dev")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let o = c.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap()
}

fn fixture(name: &str) -> Value {
    let p = format!("{}/tests/golden/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[test]
fn primes_partitions_and_boundary_counts() {
    let f = fixture("primes");
    let ns: Vec<&str> = f["ns"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    let got = dev(&["primes"], &ns.join(" "));
    let want: Vec<&str> = f["want"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    for ((n, g), w) in ns.iter().zip(got.lines()).zip(want) {
        assert_eq!(g, w, "is_prime / p(n) / boundary_counts of {n}");
    }
}

#[test]
fn validation_canonical_json_and_gaps() {
    let cases = fixture("validate");
    for c in cases.as_array().unwrap() {
        let what = c["what"].as_str().unwrap();
        let got = dev(&["validate", c["start"].as_str().unwrap(), c["limit"].as_str().unwrap()], &c["res"].to_string());
        let got: Vec<&str> = got.split('\n').collect();
        assert_eq!(got[0], c["canon"].as_str().unwrap(), "validate: {what}");
        if let Some(g) = c["gaps"].as_str() {
            assert_eq!(got[1], g, "validate_gaps: {what}");
        }
    }
}

#[test]
fn the_board_wide_and_narrow() {
    let stats: Value = serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../test/stats_fixture.json")).unwrap()).unwrap();
    for c in fixture("board").as_array().unwrap() {
        let what = c["what"].as_str().unwrap();
        let mut s = stats.clone();
        match c["variant"].as_str() {
            Some("plain") => {}
            Some("alert") => {
                s["alert"] = json!({"slice": 12, "client": "x", "result": "a possible counterexample with a very long-winded re-explanation", "time": 1});
                s["devices"] = json!([{"name": "Ünïcødé device with a long name", "gpu": "g", "slice": 4, "progress": 0.5, "rate": 123456789.0},
                                      {"name": "b", "gpu": "g", "slice": 123456, "progress": 0.99999, "rate": 9999.4}]);
            }
            Some("nonative") => {
                s["native"] = json!({});
                s["ridges"] = json!([]);
                s["web"]["lowest"] = Value::Null;
            }
            _ => s = c["stats"].clone(),
        }
        let got = dev(&["board"], &s.to_string());
        let (wide, narrow) = got.split_once('\x0c').unwrap();
        assert_eq!(wide, c["wide"].as_str().unwrap(), "board (wide): {what}");
        assert_eq!(narrow, c["narrow"].as_str().unwrap(), "board (narrow): {what}");
    }
}

#[test]
fn markdown_for_agents() {
    for c in fixture("markdown").as_array().unwrap() {
        assert_eq!(dev(&["markdown"], c["html"].as_str().unwrap()), c["want"].as_str().unwrap(), "markdown of {}", c["what"]);
    }
}

#[test]
fn textwrap() {
    for c in fixture("wrap").as_array().unwrap() {
        let (w, t) = (c["width"].to_string(), c["text"].as_str().unwrap());
        assert_eq!(dev(&["wrap", &w], t), c["want"].as_str().unwrap(), "textwrap {w}: {t:?}");
    }
}
