#!/usr/bin/env bash
# Assemble the macOS application bundle for the native build.
#
# On macOS an executable carries no icon of its own. The icon Finder and the Dock
# show, the version in Get Info and the category the system files the app under
# are all what `Info.plist` says about the `.app` around it, so a binary run
# from `target/` has no icon. This is that `.app`; its name, its identifier and
# its copyright are already in both.
#
# The identity in it is the one Legacy shipped -- same bundle identifier, same
# name, same copyright, same artwork -- so the OS files the native build as the
# same application rather than as a second one that happens to look like it. The
# fields that are not copied are explained where they are.
#
# Usage: scripts/bundle-macos.sh [binary] [output-dir]
#   binary      default target/release/Muster
#   output-dir  default dist/
set -euo pipefail

[ "$(uname -s)" = "Darwin" ] || {
  echo "bundle-macos: builds a macOS bundle, and this is not macOS" >&2
  exit 1
}

root="$(cd "$(dirname "$0")/.." && pwd)"
binary="${1:-$root/target/release/Muster}"
out="${2:-$root/dist}"
icon="$root/app/assets/icon.icns"
app="$out/Muster.app"

for required in "$binary" "$icon"; do
  [ -f "$required" ] || {
    echo "bundle-macos: $required does not exist" >&2
    exit 1
  }
done

# The version the bundle advertises is the one the crate declares, read from
# where it is written down. A literal here would be a second answer to a
# question that has one.
version="$(awk -F'"' '/^version = "/{ print $2; exit }' "$root/app/Cargo.toml")"
[ -n "$version" ] || {
  echo "bundle-macos: app/Cargo.toml declares no version" >&2
  exit 1
}

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
# `Muster`, the name `CFBundleExecutable` below declares and the name the binary
# carries in `Cargo.toml`: inside the bundle the executable name is the
# one the system reports in Activity Monitor and in Get Info, and it is the same
# word `CFBundleName` gives the app, so the bundle and the bare executable in
# `target/` are one application under one name.
install -m 755 "$binary" "$app/Contents/MacOS/Muster"
install -m 644 "$icon" "$app/Contents/Resources/Muster.icns"

cat >"$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>English</string>
  <key>CFBundleDisplayName</key>
  <string>Muster</string>
  <key>CFBundleExecutable</key>
  <string>Muster</string>
  <key>CFBundleIconFile</key>
  <string>Muster.icns</string>
  <key>CFBundleIdentifier</key>
  <string>dev.muster.workspace</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>Muster</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>$version</string>
  <key>CFBundleVersion</key>
  <string>$version</string>
  <key>CSResourcesFileMapped</key>
  <true/>
  <key>LSApplicationCategoryType</key>
  <string>public.app-category.developer-tools</string>
  <!-- Legacy said 10.13, which described the webview build. The floor here is the
       toolkit's own, macOS 15: see docs/release-safety.md. -->
  <key>LSMinimumSystemVersion</key>
  <string>15.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>NSHumanReadableCopyright</key>
  <string>Copyright (c) 2026 Marani Matias Ezequiel</string>
</dict>
</plist>
PLIST

# Ad-hoc signing with the hardened runtime, verified below for local integrity.
# This establishes neither Gatekeeper trust nor a Developer ID identity and does
# not notarize the bundle. Distribution signing is a separate release decision.
codesign --force --sign - --options runtime "$app"

plutil -lint "$app/Contents/Info.plist" >/dev/null
codesign --verify --strict "$app"
echo "bundle-macos: $app ($version)"
