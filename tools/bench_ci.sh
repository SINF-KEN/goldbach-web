#!/usr/bin/env bash
# Benchmark CI: run by .github/workflows/bench.yml on the self-hosted runner of
# the B580 machine (label b580) for every pushed branch.
#
#   candidate = the branch merged with the current main (what would land)
#   1. GPU slot: if the native production campaign runs, pause it at a slice
#      boundary (goldbach-l0 ci/goldbach_ci.py) and take the shared GPU
#      flock; the slot is released right after the last measurement (report,
#      merge and deploy run with production back on the GPU); production
#      resumes GB_RESUME_DELAY s later if another benchmark job is queued
#      (queued branches share one pause), else after GB_RESUME_SOON s
#   2. exactness: golden result hashes (test/golden.sh gpu), modq vs BigInt
#   3. benchmark: base (main) and candidate in one process, alternating segment
#      by segment, full and phone mode, GB_REPS repetitions; every repetition's
#      results must be identical
#   4. report (goldbach-tools bench-report, server-rs/src/bin/goldbach-tools): commit comment + job summary
#   5. exact and significantly faster: push the candidate to main and deploy
#
# env: GB_L0_DIR (default ~/coding/goldbach-l0), GB_REPS (8 = 4 ABBA pairs),
#      GB_RESUME_DELAY (120), GB_RESUME_SOON (10), GB_NO_MERGE=1 (report only)
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$PWD
L0=${GB_L0_DIR:-$HOME/coding/goldbach-l0}
REPO=${GITHUB_REPOSITORY:-SINF-KEN/goldbach-web}
REPS=${GB_REPS:-8}
SHA=$(git rev-parse HEAD)
BRANCH=${GITHUB_REF_NAME:-$(git rev-parse --abbrev-ref HEAD)}
OUT=$ROOT/bench-out
STATE=$HOME/.cache/goldbach-web-bench
mkdir -p "$STATE"
rm -rf "$OUT"
mkdir -p "$OUT"
log() { echo "[bench $(date +%H:%M:%S)] $*"; }
# the network from this machine to GitHub drops now and then: retry gh calls
gh() { local i; for i in 1 2 3 4 5; do command gh "$@" && return 0; sleep $((i * 5)); done; return 1; }

# ---- the branch's pull request (every experiment is a PR: one list, one verdict each) ----
label() { gh label create "$1" --color "$2" --description "$3" --force >/dev/null 2>&1 || true; }
label "bench: running" fbca04 "benchmark CI is measuring this"
label "bench: faster" 0e8a16 "exact and significantly faster: merged by the benchmark CI"
label "bench: not faster" b60205 "exact but no significant speedup: closed by the benchmark CI"
label "bench: not exact" 5c0000 "results differ from main or golden hashes failed"
label "bench: error" d93f0b "the benchmark job failed before a verdict (see the job log)"
PR=""
if [ "$BRANCH" != main ]; then
  PR=$(gh pr list --head "$BRANCH" --state open --json number -q '.[0].number' 2>/dev/null || true)
  if [ -z "$PR" ]; then
    gh pr create --head "$BRANCH" --base main --title "$(git log -1 --format=%s "$SHA")" \
      --body "$(git log -1 --format=%b "$SHA" | sed '/^Co-Authored-By/,$d')

Opened by the benchmark CI: it measures this branch against main on the Arc B580, merges it when it is exact and significantly faster, and closes it otherwise." >/dev/null 2>&1 || true
    PR=$(gh pr list --head "$BRANCH" --state open --json number -q '.[0].number' 2>/dev/null || true)
  fi
  if [ -n "$PR" ]; then
    gh pr edit "$PR" --add-label "bench: running" --remove-label "bench: faster,bench: not faster,bench: not exact,bench: error" >/dev/null 2>&1 || true
  fi
fi
verdict() {   # $1 label, $2 close (yes|no), $3 message
  VERDICT_DONE=1
  [ -z "$PR" ] && return 0
  gh pr edit "$PR" --remove-label "bench: running" --add-label "$1" >/dev/null 2>&1 || true
  if [ "$2" = yes ]; then gh pr close "$PR" --comment "$3" >/dev/null 2>&1 || true
  else gh pr comment "$PR" --body "$3" >/dev/null 2>&1 || true; fi
}

# ---- candidate and base worktrees ----------------------------------------------
git fetch -q origin main
BASE=$(git rev-parse origin/main)
cleanup_worktrees() {
  git worktree remove --force "$OUT/base" 2>/dev/null || true
  git worktree remove --force "$OUT/cand" 2>/dev/null || true
}
git worktree prune                                   # a cancelled job may have left registrations
git worktree add -q -f --detach "$OUT/base" "$BASE"
git worktree add -q -f --detach "$OUT/cand" "$SHA"
if ! git -C "$OUT/cand" -c user.name="goldbach bench" -c user.email="bench@goldbach.net01.io" \
     merge -q --no-edit -m "Merge $BRANCH (benchmarked: exact and faster)" "$BASE"; then
  log "the branch does not merge cleanly with main"
  echo "**Benchmark not run**: \`$BRANCH\` does not merge cleanly with main ($BASE). Rebase it." > "$OUT/report.md"
  gh api "repos/$REPO/commits/$SHA/comments" -F body=@"$OUT/report.md" >/dev/null || true
  if [ -n "$PR" ]; then
    gh pr edit "$PR" --remove-label "bench: running" --add-label "bench: error" >/dev/null 2>&1 || true
    gh pr comment "$PR" --body-file "$OUT/report.md" >/dev/null 2>&1 || true
  fi
  cleanup_worktrees
  exit 1
fi
CAND=$(git -C "$OUT/cand" rev-parse HEAD)
log "base main $BASE, candidate $CAND (branch $BRANCH at $SHA)"
# nothing the benchmark loads changed (page, worker, docs, CI, coordinator), or only
# // comments and blank lines in it: no production pause, no verdict; the PR stays open
# for review. A changed # line counts as code (C++ preprocessor), and so does a binary file.
# The diff is captured first: grep -q on a pipe would end git early, and pipefail would
# then read a code change as none.
engine_diff=$(git -C "$OUT/cand" diff -U0 --no-color "$BASE" -- client/engine.js kernels core shared server/cpu_worker.cpp server/build_cpu.sh)
engine_code=$(grep -E '^[-+]|^Binary files' <<<"$engine_diff" | grep -vE '^(\+\+\+|---) (a/|b/|/dev/null)' |
  grep -vE '^[-+][[:space:]]*(//.*)?$' || true)
if [ -z "$engine_code" ]; then
  if [ -z "$engine_diff" ]; then what="changes no engine, kernel, core or CPU-verifier file"
  else what="changes only comments in engine, kernel, core or CPU-verifier files"; fi
  log "$what: nothing to benchmark"
  echo "**Nothing to benchmark**: \`$BRANCH\` $what (production was not paused). Merge by review." > "$OUT/report.md"
  gh api "repos/$REPO/commits/$SHA/comments" -F body=@"$OUT/report.md" >/dev/null || true
  if [ -n "$PR" ]; then
    gh pr edit "$PR" --remove-label "bench: running" >/dev/null 2>&1 || true
    gh pr comment "$PR" --body-file "$OUT/report.md" >/dev/null 2>&1 || true
  fi
  cleanup_worktrees
  exit 0
fi
for d in "$OUT/base" "$OUT/cand"; do (cd "$d" && bash core/build.sh >/dev/null && bash tools/setup_node.sh >/dev/null); done
# the tools (cross-check, CPU benchmark, report) always come from main, like the harness
TOOLS=$(dirname "$(bash "$OUT/base/server-rs/build.sh")")/goldbach-tools
# GPU vs CPU cross-check, CPU half before the GPU slot (main's CPU verifier is the
# reference; random windows seeded by the commit: every job checks other ones)
xc=ok
(cd "$OUT/base" && bash server/build_cpu.sh >/dev/null)
"$TOOLS" crosscheck cpu "$SHA" "${GB_XC_WINDOWS:-12}" "$OUT/xc.json" "$OUT/base/build/cpu_worker-native" \
  > "$OUT/crosscheck.log" 2>&1 || xc=FAIL

# ---- GPU slot ---------------------------------------------------------------------
systemctl --user stop gbweb-resume.timer gbweb-resume.service 2>/dev/null || true
prod() { timeout 120 python3 "$L0/ci/goldbach_ci.py" status 2>/dev/null | grep -m1 '^production' || true; }
if prod | grep -q RUNNING; then
  log "pausing production at the next slice boundary"
  for _ in $(seq 1 60); do
    timeout 3600 python3 "$L0/ci/goldbach_ci.py" production stop >/dev/null 2>&1 || true
    prod | grep -q RUNNING || break
    sleep 60                                       # the local CI holds ci.lock: retry
  done
  prod | grep -q RUNNING && { log "could not pause production"; exit 1; }
  touch "$STATE/paused-by-bench"
fi
release() {
  rc=$?
  if [ "$rc" -ne 0 ] && [ -n "$PR" ] && [ -z "${VERDICT_DONE:-}" ]; then
    # a crash (harness error, device refusal, cancelled job): say so on the PR instead of leaving "running"
    gh pr edit "$PR" --remove-label "bench: running" --add-label "bench: error" >/dev/null 2>&1 || true
    gh pr comment "$PR" --body "**Benchmark failed before a verdict** (exit $rc): see the job log (${GITHUB_SERVER_URL:-https://github.com}/$REPO/actions/runs/${GITHUB_RUN_ID:-}). The PR stays open; push again to re-run." >/dev/null 2>&1 || true
  fi
  release_gpu
  cleanup_worktrees
  exit $rc
}
# Give the GPU back (idempotent): close the flock and schedule the resume. A
# queued benchmark job would pause production again at once, so it gets the
# long delay (its own stop cancels the timer); otherwise resume soon.
release_gpu() {
  [ -n "${GPU_RELEASED:-}" ] && return 0
  GPU_RELEASED=1
  exec 9>&- 2>/dev/null || true
  if [ -e "$STATE/paused-by-bench" ]; then
    local delay=${GB_RESUME_DELAY:-120} queued
    queued=$(timeout 30 command gh run list --repo "$REPO" --workflow bench.yml --limit 50 \
               --json status -q '[.[] | select(.status != "completed" and .status != "in_progress")] | length' 2>/dev/null || true)
    [ "$queued" = 0 ] && delay=${GB_RESUME_SOON:-10}
    systemctl --user stop gbweb-resume.timer gbweb-resume.service 2>/dev/null || true
    systemctl --user reset-failed gbweb-resume.timer gbweb-resume.service 2>/dev/null || true
    systemd-run --user --quiet --unit=gbweb-resume --on-active="$delay" --timer-property=AccuracySec=1s \
      /bin/bash -c "python3 '$L0/ci/goldbach_ci.py' production start && rm -f '$STATE/paused-by-bench'" \
      && log "production resumes in $delay s unless another benchmark starts (queued jobs: ${queued:-unknown})"
  fi
}
trap release EXIT
trap 'exit 130' INT TERM HUP                       # a cancelled job still resumes production
exec 9>"$L0/.goldbach-gpu.lock"
flock -w 7200 9
log "GPU slot held: $(prod)"

# ---- exactness ----------------------------------------------------------------------
# base primes: generated once per job by the candidate's own core (the first
# golden window), checked against the known primes, then loaded in ~0.1 s by
# every later run (test/primes.mjs): ~10 generations of ~6.6 s saved per job
export GB_PRIMES_CACHE="$OUT/primes.bin"
cd "$OUT/cand"
exact=ok
bash test/golden.sh gpu > "$OUT/golden.log" 2>&1 || exact=FAIL
node test/modq_test.mjs 4194304 > "$OUT/modq.log" 2>&1 || exact=FAIL
[ "$xc" = ok ] && { "$TOOLS" crosscheck gpu "$OUT/xc.json" >> "$OUT/crosscheck.log" 2>&1 || xc=FAIL; }
[ "$xc" = ok ] || exact=FAIL
log "exactness: $exact"

# ---- benchmark ------------------------------------------------------------------------
# the harness always comes from main (a branch never judges itself)
node "$OUT/base/test/bench_ab.mjs" "$OUT/base" "$OUT/cand" full \
  4000000000000000000 4000000010000000000 "$REPS" "$OUT/full.json" | tee "$OUT/bench.log"
node "$OUT/base/test/bench_ab.mjs" "$OUT/base" "$OUT/cand" lite \
  4000000000000000000 4000000005000000000 "$REPS" "$OUT/lite.json" | tee -a "$OUT/bench.log"
# CPU verifier (VPS): only when the branch changes it or the core it builds on
if ! git -C "$OUT/cand" diff --quiet "$BASE" -- server/cpu_worker.cpp server/build_cpu.sh core/core.cpp shared; then
  log "the CPU verifier changed: benchmarking it"
  bash test/golden.sh cpu > "$OUT/golden-cpu.log" 2>&1 || exact=FAIL
  tail -1 "$OUT/golden-cpu.log" >> "$OUT/golden.log"
  "$TOOLS" bench-cpu "$OUT/base" "$OUT/cand" "$REPS" "$OUT/cpu.json" | tee -a "$OUT/bench.log"
fi
cd "$ROOT"

# ---- report and decision ----------------------------------------------------------------
"$TOOLS" bench-report "$OUT" --branch "$BRANCH" --sha "$SHA" --base "$BASE" --cand "$CAND" \
  --exact "$exact" --golden "$OUT/golden.log" --modq "$OUT/modq.log" --crosscheck "$OUT/crosscheck.log" > "$OUT/report.md"
if grep -q "<!-- borderline -->" "$OUT/report.md" && [ "${GB_EXTENDED:-}" != 1 ]; then
  # borderline: 8 more repetitions (ABBA pairs), then decide on all of them
  log "borderline result: extending the measurement"
  for m in full lite cpu; do
    if [ $m = cpu ]; then
      [ -e "$OUT/cpu.json" ] || continue
      "$TOOLS" bench-cpu "$OUT/base" "$OUT/cand" "$REPS" "$OUT/cpu.more.json" | tee -a "$OUT/bench.log"
    else
      lim=$([ $m = full ] && echo 4000000010000000000 || echo 4000000005000000000)
      node "$OUT/base/test/bench_ab.mjs" "$OUT/base" "$OUT/cand" $m \
        4000000000000000000 "$lim" "$REPS" "$OUT/$m.more.json" | tee -a "$OUT/bench.log"
    fi
    "$TOOLS" merge-runs "$OUT/$m.json" "$OUT/$m.more.json"
  done
  "$TOOLS" bench-report "$OUT" --branch "$BRANCH" --sha "$SHA" --base "$BASE" --cand "$CAND" \
    --exact "$exact" --golden "$OUT/golden.log" --modq "$OUT/modq.log" --crosscheck "$OUT/crosscheck.log" > "$OUT/report.md"
fi
release_gpu                                        # measurements done: production back on the GPU
decision=$(tail -1 "$OUT/report.md" | sed -n 's/^<!-- decision: \(.*\) -->$/\1/p')
cat "$OUT/report.md" >> "${GITHUB_STEP_SUMMARY:-/dev/null}"
gh api "repos/$REPO/commits/$SHA/comments" -F body=@"$OUT/report.md" >/dev/null && log "report posted on $SHA"
if [ -n "$PR" ] && gh pr comment "$PR" --body-file "$OUT/report.md" >/dev/null 2>&1; then log "report posted on PR #$PR"; fi
log "decision: $decision"
if [ "$decision" != merge ]; then
  if [ "$exact" != ok ] || grep -q "results identical to main in every repetition | \*\*NO" "$OUT/report.md"; then
    verdict "bench: not exact" yes "Closed by the benchmark CI: **not exact** (see the report above). Results must stay identical to main."
  else
    verdict "bench: not faster" yes "Closed by the benchmark CI: exact, but **not significantly faster** (see the report above)."
  fi
fi
if [ "$decision" = merge ] && [ "$BRANCH" != main ] && [ -z "${GB_NO_MERGE:-}" ]; then
  if git -C "$OUT/cand" push -q origin "HEAD:main"; then
    log "merged into main: $CAND"
    rm -rf "$OUT/deploy" && git worktree add -q --detach "$OUT/deploy" "$CAND"
    (cd "$OUT/deploy" && bash server/deploy.sh) > "$OUT/deploy.log" 2>&1 && dep="deployed" || dep="DEPLOY FAILED (see the job log)"
    git worktree remove --force "$OUT/deploy" || true
    msg="**Merged into main** as \`${CAND:0:9}\` and $dep to https://goldbach.net01.io (open pages switch after their current slice)."
    gh api "repos/$REPO/commits/$SHA/comments" -f body="$msg" >/dev/null || true
    verdict "bench: faster" no "$msg"
    cat "$OUT/deploy.log"
  else
    log "main moved while benchmarking: push rejected (the next push re-benchmarks)"
    gh api "repos/$REPO/commits/$SHA/comments" -f body="Not merged: main moved during the benchmark. Push again (or rebase) to re-run." >/dev/null || true
  fi
fi
