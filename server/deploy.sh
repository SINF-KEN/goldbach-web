#!/usr/bin/env bash
# Deploys goldbach-web to the VPS (key login as root): client, kernels, coordinator
# (the Rust server, built here; restarted, server.py deployed as its fallback:
# server/install.sh) and the CPU worker (built there with the VPS's g++; it switches
# to the new code by itself after its current slice, so no work is lost). Browsers
# follow the update live (/api/version).
#   bash server/deploy.sh [host]
set -euo pipefail
HOST=${1:-${GB_DEPLOY_HOST:?pass user@host or set GB_DEPLOY_HOST}}
cd "$(dirname "$0")/.."
bash core/build.sh >/dev/null
# the coordinator and the CPU worker: built here (the VPS has one vCPU and no Rust
# toolchain), sent as server/goldbach-server.new and server/goldbach-cpu-worker.new
# (install.sh renames them over the running binaries)
BIN=$(bash server-rs/build.sh)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
install -D -m 755 "$BIN" "$stage/server/goldbach-server.new"
install -D -m 755 "$(dirname "$BIN")/goldbach-cpu-worker" "$stage/server/goldbach-cpu-worker.new"
# the deployed code's identity: the CPU worker reports it with every slice
# (dashboard: server speed per version)
git_ () { git "$@" 2>/dev/null | tr -d '\n'; }
commit=$(git_ log -1 --format=%h)
[ -z "$(git status --porcelain --untracked-files=no)" ] || commit="$commit+dirty"
esc () { sed 's/\\/\\\\/g; s/"/\\"/g'; }
printf '{"commit": "%s", "date": "%s", "subject": "%s"}' "$commit" "$(git_ log -1 --format=%cI)" "$(git_ log -1 --format=%s | esc)" > server/VERSION.json
tar czf - client/index.html client/app.js client/worker.js client/engine.js client/pcache.js client/selftest.js client/core.js client/core.wasm \
    client/favicon.svg client/favicon.ico client/google3dd777e111e53294.html client/og.png client/robots.txt client/llms.txt client/llms-full.txt client/auth.md client/openapi.json client/goldbach-skill.md client/indexnow-*.txt client/paper.html client/paper.js client/paper.json kernels/*.wgsl server/goldbach-cpu.service \
    server/goldbach-web.service server/install.sh \
    server/build_cpu.sh server/VERSION.json server/cpu_worker.cpp core/core.cpp shared \
    -C "$stage" server/goldbach-server.new server/goldbach-cpu-worker.new |
  ssh -o BatchMode=yes "$HOST" 'tar xzf - -C /opt/goldbach/app && bash /opt/goldbach/app/server/install.sh'
SITE=https://goldbach.net01.io
curl -sf "$SITE/api/health" >/dev/null
# smoke test: every public page (the coordinator's PAGES: what its sitemap lists), robots names the sitemap
pages=$(curl -sf "$SITE/sitemap.xml" | grep -o "<loc>[^<]*</loc>" | sed "s|<loc>$SITE||; s|</loc>||")
[ -n "$pages" ] || { echo "deploy smoke test: the sitemap lists no page" >&2; exit 1; }
for p in $pages; do
  curl -sf -o /dev/null "$SITE$p" || { echo "deploy smoke test: $SITE$p failed" >&2; exit 1; }
  curl -sf "$SITE/sitemap.xml" | grep -q "<loc>$SITE$p</loc>" || { echo "deploy smoke test: $p not in the sitemap" >&2; exit 1; }
done
curl -sf "$SITE/robots.txt" | grep -q "^Sitemap: $SITE/sitemap.xml" || { echo "deploy smoke test: robots.txt" >&2; exit 1; }
curl -sf "$SITE/api/live?tiers=300" | grep -q '"bh":' || { echo "deploy smoke test: /api/live" >&2; exit 1; }
echo "deployed: $SITE"
# IndexNow (Bing, Yandex, Seznam, ...): tell search engines the pages changed.
# Best effort: a failed ping never fails the deploy.
key=$(basename client/indexnow-*.txt .txt); key=${key#indexnow-}
urls=$(for p in $pages; do printf '"%s%s",' "$SITE" "$p"; done)
curl -s -m 20 -o /dev/null -w "IndexNow: HTTP %{http_code}\n" -H "Content-Type: application/json; charset=utf-8" \
  -d "{\"host\":\"goldbach.net01.io\",\"key\":\"$key\",\"keyLocation\":\"$SITE/indexnow-$key.txt\",\"urlList\":[${urls%,}]}" \
  https://api.indexnow.org/indexnow || echo "IndexNow ping failed (ignored)"
