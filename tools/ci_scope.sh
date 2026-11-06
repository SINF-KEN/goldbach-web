#!/usr/bin/env bash
# Decide how much of tools/ci_local.sh a CI run needs; prints "scope=<s>" for
# $GITHUB_OUTPUT (.github/workflows/ci.yml):
#   skip  push to main whose exact tree already passed CI as a pull request
#         (merged up to date): deploy without testing it twice
#   fast  no engine, kernel, core, CPU-verifier or test-harness file changed:
#         coordinator tests, page tests, syntax, ShellCheck (a few minutes)
#   full  everything, including the WASM build, lavapipe WebGPU tests and the
#         native CPU golden windows
# Anything unknown (no base, a failed lookup) means full: skipping is opt-in by
# evidence, never by default.
#   BASE=<sha> bash tools/ci_scope.sh
set -uo pipefail
cd "$(git rev-parse --show-toplevel)" || exit 1

say() { echo "scope=$1"; echo "CI scope: $1 ($2)" >&2; exit 0; }

BASE=${BASE:-}
if [ -z "$BASE" ] || [ "$BASE" = 0000000000000000000000000000000000000000 ] \
   || ! git cat-file -e "$BASE^{commit}" 2>/dev/null; then
  say full "no usable base commit"
fi

# Push to main: the pull request it came from already ran full CI on this tree.
if [ "${GITHUB_EVENT_NAME:-}" = push ] && [ "${GITHUB_REF:-}" = refs/heads/main ] && [ -n "${GITHUB_REPOSITORY:-}" ]; then
  sha=$(git rev-parse HEAD)
  for head in $(gh api "repos/$GITHUB_REPOSITORY/commits/$sha/pulls" -q '.[] | select(.merged_at != null) | .head.sha' 2>/dev/null); do
    git cat-file -e "$head^{commit}" 2>/dev/null || git fetch -q origin "$head" 2>/dev/null || continue
    [ "$(git rev-parse "$head^{tree}")" = "$(git rev-parse "HEAD^{tree}")" ] || continue
    ok=$(gh api "repos/$GITHUB_REPOSITORY/actions/runs?head_sha=$head&event=pull_request&status=success" \
           -q '[.workflow_runs[] | select(.name == "CI")] | length' 2>/dev/null || echo 0)
    [ "${ok:-0}" -gt 0 ] && say skip "tree identical to PR head $head, which passed CI"
  done
fi

changed=$(git diff --name-only "$BASE" HEAD) || say full "cannot diff against $BASE"
[ -z "$changed" ] && say fast "no file changed"
# Files whose change needs the WASM build, the WebGPU tests or the CPU goldens.
FULL='^(client/(engine|worker|pcache|selftest|core)\.js|client/core\.wasm|kernels/|core/|shared/|server/(cpu_worker\.cpp|build_cpu\.sh)|test/.*\.(mjs|sh)$|tools/(ci_local|ci_scope|setup_node)\.sh|\.github/workflows/ci\.yml)'
# The page's own tests (no GPU, no build) run in every scope.
if hit=$(grep -Ev '^test/(live_update|worker_mode)_test\.mjs$' <<<"$changed" | grep -E -m1 "$FULL"); then
  say full "$hit changed"
fi
say fast "only page, coordinator or docs files changed"
