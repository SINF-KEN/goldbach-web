#!/usr/bin/env bash
# Runs one command on the production machine's GPU the way the benchmark CI
# does (tools/bench_ci.sh, "GPU slot"): the native campaign is paused at its
# next slice boundary, the shared GPU lock is held while the command runs, and
# production resumes GB_RESUME_DELAY s (default 120) after it, unless another
# benchmark takes the slot first. For profiling tools (test/timeline.mjs,
# test/prof.mjs); experiments that could merge go through the benchmark CI.
#   bash tools/gpu_slot.sh node test/timeline.mjs
# Exit status: the command's, or 1 if production could not be paused.
set -euo pipefail
L0=${GB_L0_DIR:-$HOME/coding/goldbach-l0}
STATE=$HOME/.cache/goldbach-web-bench           # shared with bench_ci.sh: one pause marker
mkdir -p "$STATE"
log() { echo "[gpu_slot $(date +%T)] $*" >&2; }
[ $# -gt 0 ] || { echo "usage: $0 COMMAND [ARGS...]" >&2; exit 2; }

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
  exec 9>&- 2>/dev/null || true
  if [ -e "$STATE/paused-by-bench" ]; then
    systemctl --user stop gbweb-resume.timer gbweb-resume.service 2>/dev/null || true
    systemctl --user reset-failed gbweb-resume.timer gbweb-resume.service 2>/dev/null || true
    systemd-run --user --quiet --unit=gbweb-resume --on-active="${GB_RESUME_DELAY:-120}" --timer-property=AccuracySec=1s \
      /bin/bash -c "python3 '$L0/ci/goldbach_ci.py' production start && rm -f '$STATE/paused-by-bench'" \
      && log "production resumes in ${GB_RESUME_DELAY:-120} s unless another benchmark starts"
  fi
  exit $rc
}
trap release EXIT
trap 'exit 130' INT TERM HUP                       # an interrupted run still resumes production
exec 9>"$L0/.goldbach-gpu.lock"
flock -w 7200 9
log "GPU slot held: $(prod)"
"$@"
