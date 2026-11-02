#!/usr/bin/env bash
# Package an already signed bundle; keep ZIP as an alternative download.
# Usage: scripts/package-macos-dmg.sh [bundle] [output-dir]
set -euo pipefail
[[ "$(uname -s)" == Darwin ]] || { echo 'DMG packaging requires macOS.' >&2; exit 1; }
root="$(cd "$(dirname "$0")/.." && pwd)"
app="${1:-$root/dist/Muster.app}"
out="${2:-$root/dist}"
version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist")"
arch="$(lipo -archs "$app/Contents/MacOS/Muster")"
[[ "$version" =~ ^(0\.[0-9]+\.[0-9]+|1\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?)$ ]]
[[ "$arch" == arm64 || "$arch" == x86_64 ]]
codesign --verify --strict "$app"
mkdir -p "$out"
out="$(cd "$out" && pwd)"
work="$(mktemp -d)"
mounted=false
cleanup() {
  if [[ "$mounted" == true ]]; then hdiutil detach "$work/mount" >/dev/null; fi
  rm -rf "$work"
}
trap cleanup EXIT
mkdir "$work/stage" "$work/mount"
ditto "$app" "$work/stage/Muster.app"
ln -s /Applications "$work/stage/Applications"
install -m 755 "$root/scripts/install-macos.command" "$work/stage/Install Muster.command"
cat > "$work/stage/Install.txt" <<'HELP'
Muster for macOS 15 or later

Quit any running Muster before installing or updating.
Drag Muster.app onto Applications, or run Install Muster.command to copy it
and remove its quarantine attribute after confirming you trust the download.
The installer refuses to replace an existing app; move the old app to the
Trash first. It does not touch your workspace or preferences.

This app and installer are not notarized by Apple. macOS may block the
installer too. Only if you trust the source, open Terminal and run:

  /bin/bash "/Volumes/Muster/Install Muster.command"

If Finder mounted the image with another name, use that path instead.
For a manual install, copy the app to Applications and then run:

  xattr -dr com.apple.quarantine /Applications/Muster.app

This removes quarantine only from Muster; it does not disable Gatekeeper.
HELP
hdiutil create -volname Muster -fs HFS+ -srcfolder "$work/stage" -format UDZO "$work/Muster.dmg"
hdiutil verify "$work/Muster.dmg"
hdiutil attach -nobrowse -readonly -mountpoint "$work/mount" "$work/Muster.dmg" >/dev/null
mounted=true
# Check what the customer mounts, not just the staging directory.
diff -r "$work/stage/Muster.app" "$work/mount/Muster.app"
cmp "$work/stage/Install Muster.command" "$work/mount/Install Muster.command"
cmp "$work/stage/Install.txt" "$work/mount/Install.txt"
[[ -x "$work/mount/Install Muster.command" ]]
[[ "$(readlink "$work/mount/Applications")" == /Applications ]]
codesign --verify --strict "$work/mount/Muster.app"
hdiutil detach "$work/mount" >/dev/null
mounted=false
mv "$work/Muster.dmg" "$out/Muster_${version}_macOS_${arch}.dmg"
echo "Packaged $out/Muster_${version}_macOS_${arch}.dmg"
