#!/usr/bin/env bash
# Runs on the VPS (as root) once server/deploy.sh has unpacked a release into
# /opt/goldbach/app: builds the CPU verifier, installs the coordinator (the Rust
# server, goldbach-web.service) and the CPU worker program and restarts the
# coordinator. When it does not answer within ~30 s, the previous coordinator
# binary is put back and the deploy fails. Then the CPU worker service (it
# switches to the new code after its slice).
set -euo pipefail
APP=/opt/goldbach/app
UNIT=/etc/systemd/system/goldbach-web.service
HEALTH=http://172.18.0.1:8090/api/health

chown -R root:root "$APP"
bash "$APP/server/build_cpu.sh" "$APP/server/cpu_worker" >/dev/null
# the new binaries replace the running ones by rename, never in place; the
# coordinator's predecessor stays as goldbach-server.prev (the rollback)
[ ! -f "$APP/server/goldbach-server" ] || cp -p "$APP/server/goldbach-server" "$APP/server/goldbach-server.prev"
mv -f "$APP/server/goldbach-server.new" "$APP/server/goldbach-server"
mv -f "$APP/server/goldbach-cpu-worker.new" "$APP/server/goldbach-cpu-worker"
chmod 755 "$APP/server/goldbach-server" "$APP/server/goldbach-cpu-worker"
chmod -R a+rX "$APP"

healthy() {
  for _ in $(seq 60); do
    if curl -sf -m 2 -o /dev/null "$HEALTH"; then
      return 0
    fi
    sleep 0.5
  done
  return 1
}

install -m 644 "$APP/server/goldbach-web.service" "$UNIT"
systemctl daemon-reload
systemctl restart goldbach-web
if ! healthy || ! systemctl is-active --quiet goldbach-web; then
  echo "install: the new coordinator does not answer; putting the previous one back" >&2
  journalctl -u goldbach-web -n 30 --no-pager >&2 || true
  if [ -f "$APP/server/goldbach-server.prev" ]; then
    mv -f "$APP/server/goldbach-server.prev" "$APP/server/goldbach-server"
    systemctl restart goldbach-web
    if healthy; then echo "install: the previous coordinator serves again" >&2; fi
  fi
  exit 1
fi
echo "goldbach-web: $(systemctl show -p MainPID --value goldbach-web | xargs -I{} readlink /proc/{}/exe)"

install -d -o goldbach -g goldbach -m 750 /opt/goldbach/cpu
install -m 644 "$APP/server/goldbach-cpu.service" /etc/systemd/system/goldbach-cpu.service
systemctl daemon-reload
systemctl enable --quiet --now goldbach-cpu
systemctl is-active goldbach-cpu
