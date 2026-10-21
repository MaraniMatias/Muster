#!/usr/bin/env bash
# Build a minimal Ubuntu/Debian package from the already-built native executable.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
binary="${1:-$root/target/release/Muster}"
out="${2:-$root/dist}"
[[ "$binary" = /* ]] || binary="$PWD/$binary"
[[ "$out" = /* ]] || out="$PWD/$out"
command -v dpkg-deb >/dev/null && command -v dpkg-shlibdeps >/dev/null || { echo 'package-linux-deb: dpkg-deb and dpkg-shlibdeps (dpkg-dev) are required' >&2; exit 1; }
[[ "$(uname -s)" == Linux ]] || { echo 'package-linux-deb: Linux host required' >&2; exit 1; }
[[ -x "$binary" ]] || { echo "package-linux-deb: executable not found: $binary" >&2; exit 1; }
arch="$(dpkg --print-architecture)"
[[ "$arch" == amd64 ]] || { echo "package-linux-deb: only amd64 is supported (got $arch)" >&2; exit 1; }
command -v readelf >/dev/null || { echo 'package-linux-deb: readelf is required' >&2; exit 1; }
readelf -h "$binary" | grep -Eq 'Machine: +Advanced Micro Devices X86-64' || { echo 'package-linux-deb: binary is not x86_64 ELF' >&2; exit 1; }
semver="$(awk -F'"' '/^version = "/{ print $2; exit }' "$root/app/Cargo.toml")"
version="$(bash "$root/scripts/debian-version.sh" "$semver")"
mkdir -p "$out"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
stage="$tmp/payload"
metadata="$tmp/source"
mkdir -p "$stage/DEBIAN" "$stage/usr/bin" "$stage/usr/share/applications" "$stage/usr/share/icons/hicolor/512x512/apps" "$stage/usr/share/doc/muster" "$metadata/debian"
install -m 0755 "$binary" "$stage/usr/bin/Muster"
install -m 0644 "$root/app/assets/icon.png" "$stage/usr/share/icons/hicolor/512x512/apps/muster.png"
cat >"$stage/usr/share/applications/dev.muster.workspace.desktop" <<'DESKTOP'
[Desktop Entry]
Type=Application
Name=Muster
Comment=Desktop workspace for coding agents in Git worktrees
Exec=/usr/bin/Muster
Icon=muster
StartupWMClass=dev.muster.workspace
Categories=Development;IDE;
Terminal=false
DESKTOP
install -m 0644 "$root/LICENSE" "$stage/usr/share/doc/muster/copyright"
cat >"$metadata/debian/control" <<'CONTROL'
Source: muster
Section: utils
Priority: optional
Maintainer: Marani Matias Ezequiel <maranimatias@gmail.com>
Build-Depends: dpkg-dev
Standards-Version: 4.6.2

Package: muster
Architecture: any
Description: Desktop workspace for coding agents in Git worktrees
 Native Rust desktop application.
CONTROL
# dpkg-shlibdeps requires source-package metadata in its working directory.
cd "$metadata"
deps="$(dpkg-shlibdeps -O -e"$binary" -l"$(dirname "$binary")" | sed -n 's/^shlibs:Depends=//p')"
[[ -n "$deps" ]] || { echo 'package-linux-deb: could not derive runtime dependencies' >&2; exit 1; }
deps="$deps, git, xdg-utils, libvulkan1"
# The native directory picker is the desktop portal on Linux, and a machine
# without it cannot open a directory at all: the package asks for it.
deps="$deps, xdg-desktop-portal, xdg-desktop-portal-gtk"
cat >"$stage/DEBIAN/control" <<CONTROL
Package: muster
Version: $version
Section: utils
Priority: optional
Architecture: amd64
Depends: $deps
Maintainer: Marani Matias Ezequiel <maranimatias@gmail.com>
Homepage: https://github.com/MaraniMatias/Muster
Description: Desktop workspace for coding agents in Git worktrees
 Native Rust desktop application for terminals, worktrees, files and code review.
CONTROL
artifact="$out/muster_${version}_amd64.deb"
dpkg-deb --root-owner-group --build "$stage" "$artifact" >/dev/null
[[ "$(dpkg-deb -f "$artifact" Package)" == muster ]]
[[ "$(dpkg-deb -f "$artifact" Version)" == "$version" ]]
[[ "$(dpkg-deb -f "$artifact" Architecture)" == amd64 ]]
contents="$(dpkg-deb --contents "$artifact")"
for file in './usr/bin/Muster' './usr/share/applications/dev.muster.workspace.desktop' './usr/share/icons/hicolor/512x512/apps/muster.png' './usr/share/doc/muster/copyright'; do
  [[ "$contents" == *"$file"* ]] || { echo "package-linux-deb: missing payload $file" >&2; exit 1; }
done
[[ "$contents" != *'./debian/'* ]] || { echo 'package-linux-deb: source metadata leaked into payload' >&2; exit 1; }
echo "package-linux-deb: $artifact (Depends: $deps)"
