#!/usr/bin/env bash
# Run as root in a disposable Ubuntu runtime container, never on a user's desktop.
set -euo pipefail
[[ $EUID -eq 0 ]] || { echo 'Run in the disposable Ubuntu container documented in release-safety.md' >&2; exit 1; }
package="$(realpath "${1:?usage: linux-package-smoke.sh package.deb}")"
root="$(cd "$(dirname "$0")" && pwd)"
apt-get update
apt-get install --no-install-recommends -y "$package" xvfb x11-utils xauth dbus-x11 mesa-vulkan-drivers fonts-dejavu-core coreutils util-linux
for command in timeout setsid xvfb-run xwininfo runuser dbus-run-session; do command -v "$command" >/dev/null; done
icd="$(find /usr/share/vulkan/icd.d -maxdepth 1 -name '*lvp*.json' -print -quit)"
[[ -n "$icd" ]] || { echo 'lavapipe ICD not installed' >&2; exit 1; }
# No compiler/development packages may mask an undeclared runtime dependency.
if dpkg-query -W -f='${binary:Package} ${db:Status-Status}\n' | grep -E -- '-dev(:[^ ]+)? installed$'; then
  echo 'Runtime container contains development packages' >&2; exit 1
fi
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/home" "$tmp/runtime"
chmod 0700 "$tmp" "$tmp/runtime"
chown -R nobody:nogroup "$tmp"
# Xvfb and its xauth file must belong to the same user as the app.
timeout --signal=TERM --kill-after=5s 60s runuser -u nobody -- env \
  SHELL=/bin/bash HOME="$tmp/home" XDG_CONFIG_HOME="$tmp/home/config" XDG_DATA_HOME="$tmp/home/data" \
  XDG_CACHE_HOME="$tmp/home/cache" XDG_RUNTIME_DIR="$tmp/runtime" \
  VK_ICD_FILENAMES="$icd" WGPU_BACKEND=vulkan LIBGL_ALWAYS_SOFTWARE=1 \
  xvfb-run -a dbus-run-session -- bash "$root/linux-window-smoke.sh" "$tmp/app.log" || {
    status=$?; test ! -f "$tmp/app.log" || cat "$tmp/app.log" >&2
    echo "linux-package-smoke: window-startup check failed ($status)" >&2; exit "$status"
  }
echo 'Installed app stayed alive with a window under Xvfb (not a pixel-render test)'
