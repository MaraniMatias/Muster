#!/usr/bin/env bash
# Install a package and require its application window to appear under Xvfb.
set -euo pipefail
package="${1:?usage: linux-package-smoke.sh package.deb}"
package="$(realpath "$package")"
if [[ $EUID -eq 0 ]]; then
  apt-get update
  apt-get install -y "$package" xvfb x11-utils xauth xdg-utils dbus-x11 mesa-vulkan-drivers fonts-dejavu-core coreutils util-linux
else
  sudo apt-get update
  sudo apt-get install -y "$package" xvfb x11-utils xauth xdg-utils dbus-x11 mesa-vulkan-drivers fonts-dejavu-core coreutils util-linux
fi
for command in timeout setsid xvfb-run xwininfo runuser; do command -v "$command" >/dev/null || { echo "linux-package-smoke: missing prerequisite $command" >&2; exit 1; }; done
icd="$(find /usr/share/vulkan/icd.d -maxdepth 1 -name '*lvp*.json' -print -quit)"
[[ -n "$icd" ]] || { echo 'linux-package-smoke: lavapipe ICD not installed' >&2; exit 1; }
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/home" "$tmp/runtime"
chmod 0700 "$tmp/home" "$tmp/runtime"
chown nobody:nogroup "$tmp/home" "$tmp/runtime"
export VK_ICD_FILENAMES="$icd" WGPU_BACKEND=vulkan LIBGL_ALWAYS_SOFTWARE=1
# timeout bounds Xvfb, window discovery (including a stuck xwininfo), and application startup.
timeout --signal=TERM --kill-after=5s 60s xvfb-run -a runuser -u nobody -- bash -c '
  set -uo pipefail
  log="$1"; home="$2"; runtime="$3"
  export HOME="$home" XDG_CONFIG_HOME="$home/config" XDG_DATA_HOME="$home/data" XDG_CACHE_HOME="$home/cache" XDG_RUNTIME_DIR="$runtime"
  mkdir -p "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"
  setsid /usr/bin/Muster >"$log" 2>&1 & pid=$!
  finish() { kill -TERM -- "-$pid" 2>/dev/null || true; sleep 1; kill -KILL -- "-$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; }
  trap finish EXIT HUP INT TERM
  observed=0
  for _ in $(seq 1 30); do
    windows="$(timeout 3s xwininfo -root -tree 2>/dev/null)" || windows=""
    if [[ "$windows" == *Muster* ]]; then observed=1; break; fi
    if ! kill -0 "$pid" 2>/dev/null; then wait "$pid"; status=$?; echo "app exited before window (status $status)" >&2; exit 1; fi
    sleep 1
  done
  [[ $observed == 1 ]] || { echo "window not observed" >&2; exit 1; }
  sleep 3
  if ! kill -0 "$pid" 2>/dev/null; then wait "$pid"; status=$?; echo "app exited after window (status $status)" >&2; exit 1; fi
' _ "$tmp/app.log" "$tmp/home" "$tmp/runtime" || { status=$?; cat "$tmp/app.log" >&2; echo "linux-package-smoke: window-startup check failed ($status)" >&2; exit "$status"; }
echo 'linux-package-smoke: installed app stayed alive with a window under Xvfb (not a pixel-render test)'
