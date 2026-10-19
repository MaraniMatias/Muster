#!/usr/bin/env bash
# Called inside an isolated display/session, under an outer timeout.
set -uo pipefail
log="${1:?usage: linux-window-smoke.sh log [executable]}"
app="${2:-/usr/bin/Muster}"
setsid "$app" >"$log" 2>&1 & pid=$!
finish() {
  kill -TERM -- "-$pid" "$pid" 2>/dev/null || true
  sleep 1
  kill -KILL -- "-$pid" "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
}
trap finish EXIT
trap 'exit 1' HUP INT TERM
observed=0
for _ in $(seq 1 30); do
  windows="$(timeout 3s xwininfo -root -tree 2>/dev/null)" || windows=""
  if ! kill -0 "$pid" 2>/dev/null; then
    wait "$pid"; status=$?
    echo "app exited before window (status $status)" >&2
    exit 1
  fi
  if [[ "$windows" == *Muster* ]]; then observed=1; break; fi
  sleep 1
done
[[ $observed == 1 ]] || { echo 'window not observed' >&2; exit 1; }
sleep 3
if ! kill -0 "$pid" 2>/dev/null; then
  wait "$pid"; status=$?
  echo "app exited after window (status $status)" >&2
  exit 1
fi
