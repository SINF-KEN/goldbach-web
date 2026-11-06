#!/usr/bin/env bash
# CI entry point: .github/workflows/ci.yml
# Run on the self-hosted B580 machine. WebGPU tests use Mesa lavapipe only
# (software Vulkan, CPU only), never the production GPU.
set -euo pipefail
cd "$(dirname "$0")/.."

step() { echo; echo "==== $*"; }

step "Rust programs (server-rs): coordinator logic, HTTP contract, CPU worker, native sync, tools"
bash server-rs/build.sh test >/dev/null

step "No Python in the repository"
py=$(git ls-files '*.py')
if [ -n "$py" ]; then
  echo "::error::Python files in the repository (programs, tools and tests are Rust): $py"
  exit 1
fi

step "Host/shader contracts (constants, workgroup arrays)"
node test/contract_test.mjs

step "JavaScript syntax"
for f in client/app.js client/paper.js client/worker.js client/engine.js client/pcache.js client/selftest.js; do
  node --input-type=module --check < "$f"
done
node --check tools/page_traffic.mjs
for f in test/*.mjs server/auditor.mjs; do node --check "$f"; done
# test and tool scripts are plain JavaScript modules (#145): no TypeScript comes back
ts=$(git ls-files '*.ts' '*.mts' '*.cts')
if [ -n "$ts" ]; then
  echo "::error::TypeScript files in the repository (scripts are plain JavaScript .mjs): $ts"
  exit 1
fi

step "ShellCheck"
shellcheck core/build.sh server/deploy.sh server/install.sh server-rs/build.sh server/build_cpu.sh test/golden.sh tools/ci_local.sh tools/ci_scope.sh tools/server_update.sh

step "Live updates in the page"
node test/live_update_test.mjs
node test/worker_mode_test.mjs

# CI_SCOPE=fast (tools/ci_scope.sh: no engine, kernel, core, CPU-verifier or
# test-harness change) stops here; anything else, or no scope, runs it all.
if [ "${CI_SCOPE:-full}" = fast ]; then
  echo
  echo "CI OK (fast scope: no engine, kernel, core or CPU-verifier change)"
  exit 0
fi

bash tools/setup_node.sh                         # Dawn (WebGPU) for Node.js, cached

step "Build WASM core and native oracle"
bash core/build.sh
bash core/build.sh native

step "Core test (base primes, reference map vs Miller-Rabin)"
nice -n 19 node test/core_test.mjs

step "Base-prime cache"
nice -n 19 node test/pcache_test.mjs

# Do not report success when required WebGPU tests did not run. This CI relies
# on lavapipe, not the production GPU; a missing ICD is a setup failure.
LVP=${GB_LAVAPIPE_ICD:-$HOME/.local/share/lavapipe/lvp_icd.json}
if [ ! -f "$LVP" ]; then
  echo "::error::Required lavapipe ICD not found: $LVP"
  exit 1
fi

# Dawn on Vulkan only, pinned to lavapipe. Refuse to run against a real GPU.
export GB_WEBGPU_BACKEND=vulkan
export VK_DRIVER_FILES="$LVP"
export VK_ICD_FILENAMES="$LVP"
export LP_NUM_THREADS=${LP_NUM_THREADS:-4}
for pp in high-performance low-power; do
  name=$(node --input-type=module -e "await import('./test/gpu.mjs'); const a = await navigator.gpu.requestAdapter({ powerPreference: '$pp' }); console.log(a?.info?.description ?? '')" 2>/dev/null)
  case "$name" in
    *llvmpipe*) echo "WebGPU adapter ($pp): $name" ;;
    *) echo "::error::Refusing WebGPU tests: $pp adapter is '$name', not lavapipe"; exit 1 ;;
  esac
done
wgpu() { nice -n 19 node "$@"; }

step "Complete maps vs exact reference (phone mode, 4e18)"
LITE=1 wgpu test/map_test.mjs 4000000000000000000 4000000000060000000 20000000 2

step "Complete maps vs exact reference (phone mode, top of range, p^2 path)"
LITE=1 wgpu test/map_test.mjs 4999999999940000000 5000000000000000000 20000000 2

step "Exact 64-bit modulo of phone kernels vs BigInt (1M cases)"
wgpu test/modq_test.mjs 1048576

step "Per-run states on GPU vs core definition (packed and hit-list primes)"
wgpu test/states_test.mjs

step "Known-answer self-test (phone mode)"
out=$(wgpu test/golden.mjs 4999999998000000000 5000000000000000000 64000000 lite 2>&1 | tail -1)
echo "$out"
[ "${out%% *}" = 3672004b8c97290d ]

step "Prime gaps on the GPU vs the core's CPU walk (phone mode; the gap 1476, many segments)"
LITE=1 wgpu test/gaps_test.mjs 1425172824437600000 1425172824438400000 20000000
LITE=1 wgpu test/gaps_test.mjs 4999999999800000000 5000000000000000000 20000000

step "Ridge and leftover lists (regions, readback, overflow fails closed)"
wgpu test/lists_test.mjs lite

step "Full engine refuses a device that is too small (fail closed)"
set +e
out=$(wgpu test/map_test.mjs 4000000000000000000 4000000000060000000 20000000 2 2>&1)
rc=$?
set -e
echo "$out" | tail -3
[ "$rc" -ne 0 ] && echo "$out" | grep -q "allows .* MB per buffer"

step "Worker end to end (leases, lease-ahead, heartbeats, background results)"
wgpu test/worker_e2e.mjs 3

# Run synchronously so a failure cannot bypass result collection or leave a
# detached CPU test running after this job has failed.
step "Native CPU verifier golden windows"
nice -n 19 bash test/golden.sh cpu

echo
echo "CI OK"
