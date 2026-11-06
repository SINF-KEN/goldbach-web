#!/usr/bin/env bash
# Builds the native CPU verifier (server/cpu_worker.cpp + the exact core) for
# THIS machine: build/cpu_worker (+ build/cpu_worker-native), or the path given. The VPS builds its own
# copy at deploy time (a binary built against an AVX-512 glibc would not run there).
#   bash server/build_cpu.sh [out]
set -euo pipefail
cd "$(dirname "$0")/.."
OUT=${1:-build/cpu_worker}
mkdir -p "$(dirname "$OUT")"
# Two candidates (the worker benchmarks them and runs the fastest exact one):
# $OUT portable (x86-64-v2), $OUT-native tuned to this CPU. Each is built beside
# its target and renamed: a running verifier keeps its old file.
build() {
    g++ -std=c++20 -O3 "$1" -Wall -Wextra -Ishared/include \
        server/cpu_worker.cpp core/core.cpp shared/src/primes.cpp -o "$2.new"
    mv -f "$2.new" "$2"
    echo "built $2"
}
build -march=x86-64-v2 "$OUT"
build -march=native "$OUT-native" || echo "no native build (the portable one is used)"
