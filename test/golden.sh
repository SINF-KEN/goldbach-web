#!/usr/bin/env bash
# Golden results: the canonical result hash (the fields the server hashes) of
# fixed windows must never change. Established on the Arc B580 before the
# 2026-10 speed work and reproduced since by the WebGPU engine (full and phone
# mode, several segment sizes), lavapipe and the native CPU verifier.
# 2026-10-19: re-established with the prime gaps in the result (the WebGPU
# engine on lavapipe and the CPU verifier agree byte for byte).
#   bash test/golden.sh cpu    native CPU verifier (build/cpu_worker; no GPU)
#   bash test/golden.sh gpu    WebGPU engine (Node.js + Dawn). Uses the GPU: on the
#                              production machine, pause production first.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 2
WINDOWS=(
  "4000000000000000000 4000000040000000000 467500000 full e92c9bc8c8c2238e"
  "4999999980000000000 5000000000000000000 467500000 full 73bcb70b520dcefd"
  "4321000000000000000 4321000010000000000 233750000 full 7387137caf615055"
  "4000000000000000000 4000000003000000000 128000000 lite 6cef37b84f8a3120"
  "4999999997000000000 5000000000000000000 64000000 lite 266c9267d1d7cddc"
  "4999999998000000000 5000000000000000000 64000000 lite 3672004b8c97290d"
)
fail=0
case "${1:-}" in
cpu)
  # the build this machine runs (the VPS runs its -march=native build too; the
  # portable one only differs in the checker's SIMD path); GB_CPU_BIN overrides
  bash server/build_cpu.sh >/dev/null
  bin=${GB_CPU_BIN:-build/cpu_worker-native}
  [ -x "$bin" ] || bin=build/cpu_worker
  cache=$(mktemp -d)
  run_window() {   # $1 index: result line to $cache/w$1
    read -r a b _seg _mode want <<<"${WINDOWS[$1]}"
    local t0 got r
    t0=$(date +%s)
    got=$("$bin" "$a" "$b" --primes "$cache/primes.bin" 2>/dev/null | tr -d '\n' | sha256sum | cut -c1-16)
    if [ "$got" = "$want" ]; then r=ok; else r=FAIL; fi
    echo "$r  cpu  [$a, $b]  $got  $(( $(date +%s) - t0 )) s" > "$cache/w$1"
  }
  # the shortest window first (it writes the base-prime cache), the rest in parallel
  last=$(( ${#WINDOWS[@]} - 1 ))
  run_window "$last"
  for i in $(seq 0 $(( last - 1 ))); do run_window "$i" & done
  wait
  for i in $(seq 0 "$last"); do
    cat "$cache/w$i"
    grep -q '^ok' "$cache/w$i" || fail=1
  done
  rm -rf "$cache" ;;
gpu)
  for w in "${WINDOWS[@]}"; do
    read -r a b seg mode want <<<"$w"
    extra=()
    [ "$mode" = lite ] && extra=(lite)
    line=$(node test/golden.mjs "$a" "$b" "$seg" "${extra[@]}" 2>&1 | grep -v '^\s*$' | tail -1)
    got=${line%% *}
    if [ "$got" = "$want" ]; then r=ok; else r=FAIL; fail=1; fi
    echo "$r  $mode  $line"
  done ;;
*) echo "usage: bash test/golden.sh cpu|gpu"; exit 2 ;;
esac
if [ $fail = 0 ]; then echo "GOLDEN OK"; else echo "GOLDEN FAILED"; exit 1; fi
