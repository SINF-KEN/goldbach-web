# goldbach-web Rust programs

One cargo package, no Python anywhere in the repository:

| binary | what |
| --- | --- |
| `goldbach-server` | the coordinator (axum, rusqlite, serde_json): pages, the device protocol, MCP, the dashboard snapshot |
| `goldbach-cpu-worker` | the VPS's idle-CPU worker (`goldbach-cpu.service`): leases slices and verifies them with the native verifier `server/cpu_worker.cpp` |
| `goldbach-sync-native` | pushes the B580 campaign's summary to `/api/native` (user timer, every 10 min) |
| `goldbach-tools` | the paper-data builder, the benchmarks and the cross-check (`goldbach-tools --help`) |

    cargo build --release
    target/release/goldbach-server --db goldbach.db --static ../client --bind 127.0.0.1 --port 8090

Coordinator options: `--db`, `--static`, `--bind`, `--port`; `--version-file`
(default `<static>/../server/VERSION.json`); `--threads N` (tokio workers,
default: the CPUs it may run on). Environment: `GOLDBACH_LOCAL_IPS`, `GB_SLICE`.

The coordinator was ported from the Python `server.py` (retired); the schema and
every stored value are unchanged. How it differs from that server, on purpose:

- One SQLite transaction per request: on disk a result costs one fsync, not ~13.
  A request's writes commit together or not at all.
- The page and `/api/live` bodies are rendered and gzipped once per stats
  snapshot, not per request.
- HTTP/1.1 keep-alive.
- A request that fails (malformed numbers, a database error) gets a 500.
- Values beyond 64 bits in a result's record claims (n of a "beyond" entry, a
  gap end) count as not prime: rejected (no honest result comes near). Integers
  beyond 128 bits are refused as malformed.
- A naive timestamp in the native sync's "recent" list is read as UTC.

Checks:

    cargo test --release          # unit tests, the HTTP contract (tests/), golden fixtures (tests/golden/)
    bash server-rs/build.sh test  # the same, locked, as CI runs it

Maintenance subcommands of the coordinator binary: `goldbach-server backup DB OUT`
(a consistent copy of a live database) and `goldbach-server dev ...` (pure
functions on stdin for the golden tests, `rebuild` for the benchmark,
`lease-summary DB` for `test/worker_e2e.mjs`).
