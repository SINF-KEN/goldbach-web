//! goldbach-tools: the repository's benchmark, cross-check and data tools (one
//! subcommand each; what the Python scripts in tools/ used to be).
//!
//!   goldbach-tools make-paper-json T0 T1 GAPS_T0 GAPS_T1 > client/paper.json
//!   goldbach-tools bench-cpu BASE_ROOT CAND_ROOT REPS OUT.json [START LIMIT]
//!   goldbach-tools bench-report OUT_DIR [--branch B --sha S --base B --cand C --exact E --golden F --modq F --crosscheck F]
//!   goldbach-tools bench-sync
//!   goldbach-tools bench-server BIN DB [--cpu 2] [--seconds 10] ...
//!   goldbach-tools crosscheck cpu SEED N OUT.json [CPU_BIN] [--fuzz] | gpu OUT.json
//!   goldbach-tools load-probe BIN DB [--seconds 60] [--pages 30] ...
//!   goldbach-tools merge-runs A.json B.json
mod bench_cpu;
mod bench_report;
mod bench_server;
mod bench_sync;
mod crosscheck;
mod load;
mod load_probe;
mod merge_runs;
mod make_paper_json;
mod util;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or(&[]);
    let r = match args.first().map(String::as_str) {
        Some("make-paper-json") => make_paper_json::run(rest),
        Some("bench-cpu") => bench_cpu::run(rest),
        Some("bench-report") => bench_report::run(rest),
        Some("bench-sync") => bench_sync::run(rest),
        Some("bench-server") => bench_server::run(rest),
        Some("crosscheck") => crosscheck::run(rest),
        Some("load-probe") => load_probe::run(rest),
        Some("merge-runs") => merge_runs::run(rest),
        _ => Err("usage: goldbach-tools make-paper-json|bench-cpu|bench-report|bench-sync|bench-server|crosscheck|load-probe|merge-runs ARGS...".into()),
    };
    match r {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("goldbach-tools: {e}");
            std::process::exit(1);
        }
    }
}
