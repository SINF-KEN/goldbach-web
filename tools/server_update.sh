#!/usr/bin/env bash
# Keeps the VPS's Ubuntu up to date: .github/workflows/server-update.yml runs it on
# the B580 runner (the same root key login as server/deploy.sh), weekly and by hand.
#   1. all package updates, the kernel included (within the installed release);
#   2. a reboot when Ubuntu asks for one (/var/run/reboot-required), then waits
#      for the server to come back;
#   3. RELEASE_UPGRADE=1 only: a database backup, the next Ubuntu release
#      (26.04 -> 26.10 brings Linux 7.3), a reboot, and a redeploy so the CPU
#      worker is rebuilt with the new release's compiler;
#   4. checks the coordinator and the CPU worker are running and the site answers.
# Any failure fails the job (GitHub notifies). A reboot costs about a minute of
# downtime; browsers retry, and the CPU worker's interrupted slice is leased again.
#   bash tools/server_update.sh [host]
#   RELEASE_UPGRADE=1 bash tools/server_update.sh [host]
# shellcheck disable=SC2016  # the single-quoted commands run on the VPS, where they expand
set -euo pipefail
HOST=${1:-${GB_DEPLOY_HOST:?pass user@host or set GB_DEPLOY_HOST}}
SITE=https://goldbach.net01.io
SUMMARY=${GITHUB_STEP_SUMMARY:-/dev/null}
cd "$(dirname "$0")/.."

on() { ssh -o BatchMode=yes -o ConnectTimeout=10 "$HOST" "$@"; }
info() { on '. /etc/os-release; echo "$PRETTY_NAME, kernel $(uname -r)"'; }
step() { echo; echo "==== $*"; }

reboot_wait() {
  local boot0 b
  boot0=$(on cat /proc/sys/kernel/random/boot_id)
  step "reboot ($1)"
  on systemctl reboot || true                    # the connection may drop at once
  sleep 30
  for _ in $(seq 1 60); do
    if b=$(on cat /proc/sys/kernel/random/boot_id 2>/dev/null) && [ "$b" != "$boot0" ]; then
      echo "back up: $(info)"
      return 0
    fi
    sleep 10
  done
  echo "::error::$HOST did not come back within 10 minutes of the reboot"
  exit 1
}
reboot_if_needed() { if on test -e /var/run/reboot-required; then reboot_wait "$1"; else echo "no reboot needed"; fi; }

before=$(info)
echo "before: $before"

step "package updates"
on 'export DEBIAN_FRONTEND=noninteractive
    apt-get update -q &&
    apt-get -q -y -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold full-upgrade'
reboot_if_needed "package updates"             # do-release-upgrade refuses to run with one pending

release=""
if [ "${RELEASE_UPGRADE:-0}" = 1 ]; then
  step "database backup"
  on 'set -e; d=/root/backup-$(date -u +%Y%m%d-%H%M); mkdir -p "$d"
      for db in $(find /opt/goldbach -maxdepth 3 -name "*.db"); do
        /opt/goldbach/app/server/goldbach-server backup "$db" "$d/$(basename "$db")"
      done
      ls -l "$d"'
  step "Ubuntu release upgrade"
  # "No new release found" exits non-zero too: Ubuntu enables the upgrade path
  # from the previous release some days after a release, so that is no failure
  rc=0
  out=$(on 'sed -i "s/^Prompt=.*/Prompt=normal/" /etc/update-manager/release-upgrades
            DEBIAN_FRONTEND=noninteractive do-release-upgrade -f DistUpgradeViewNonInteractive 2>&1' | tee /dev/stderr) || rc=$?
  if grep -q "No new release found" <<<"$out"; then
    echo "::notice::Ubuntu offers no new release to this server yet"
    release="no new Ubuntu release offered yet"
  elif [ "$rc" != 0 ]; then
    echo "::error::do-release-upgrade failed (exit $rc)"
    exit 1
  else
    reboot_wait "release upgrade"
    step "redeploy (rebuilds the CPU worker with the new compiler)"
    bash server/deploy.sh "$HOST"
    release="upgraded to the next Ubuntu release"
  fi
fi

step "checks"
on systemctl is-active goldbach-web goldbach-cpu
for _ in $(seq 1 12); do curl -sf "$SITE/api/health" >/dev/null && break; sleep 5; done
curl -sf "$SITE/api/health" >/dev/null || { echo "::error::$SITE/api/health does not answer"; exit 1; }
after=$(info)
echo "after: $after"
{
  echo "### VPS update"
  echo "- before: $before"
  echo "- after: $after"
  [ -n "$release" ] && echo "- release upgrade: $release"
  echo "- coordinator and CPU worker running, $SITE answers"
} >> "$SUMMARY"
