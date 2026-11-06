#!/usr/bin/env bash
# Builds the Rust programs (coordinator, CPU worker, native sync; release, Cargo.lock
# as committed) and prints the coordinator binary's path (the others sit beside it). On the CI runner the target directory lives outside the
# checkout (every job starts from a clean tree: a rebuild from scratch otherwise).
#   BIN=$(bash server-rs/build.sh)          # build
#   BIN=$(bash server-rs/build.sh test)     # run the unit tests first
set -euo pipefail
cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:$PATH"
if [ -n "${GITHUB_ACTIONS:-}" ]; then
  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/goldbach-web/server-rs-target}"
fi
if [ "${1:-}" = test ]; then
  cargo test --release --locked --quiet >&2
fi
cargo build --release --locked --quiet >&2
echo "${CARGO_TARGET_DIR:-$PWD/target}/release/goldbach-server"
