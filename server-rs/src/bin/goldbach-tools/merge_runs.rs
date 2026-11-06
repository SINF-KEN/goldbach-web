//! Appends the runs of a second bench-out JSON to the first (the CI's extended measurement
//! of a borderline result).
//!   goldbach-tools merge-runs A.json B.json
use crate::util::{read_json, write_json, R};

pub fn run(args: &[String]) -> R<i32> {
    let (pa, pb) = (args.first().ok_or("usage: merge-runs A.json B.json")?, args.get(1).ok_or("usage: merge-runs A.json B.json")?);
    let (mut a, b) = (read_json(pa)?, read_json(pb)?);
    let more = b["runs"].as_array().cloned().ok_or("B has no runs")?;
    a["runs"].as_array_mut().ok_or("A has no runs")?.extend(more);
    write_json(pa, &a)?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_runs() {
        let d = std::env::temp_dir().join(format!("gb-merge-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let (a, b) = (d.join("a.json"), d.join("b.json"));
        std::fs::write(&a, r#"{"mode":"cpu","runs":[{"rep":0}]}"#).unwrap();
        std::fs::write(&b, r#"{"mode":"cpu","runs":[{"rep":0},{"rep":1}]}"#).unwrap();
        run(&[a.to_string_lossy().into(), b.to_string_lossy().into()]).unwrap();
        assert_eq!(read_json(a.to_str().unwrap()).unwrap()["runs"].as_array().unwrap().len(), 3);
    }
}
