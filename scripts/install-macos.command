#!/bin/bash
# Run from the mounted Muster disk image. Never disable Gatekeeper globally.
set -euo pipefail

source_app="$(cd "$(dirname "$0")" && pwd)/Muster.app"
destination=/Applications/Muster.app
stage=""
trap 'if [[ -n "$stage" ]]; then rm -rf "$stage"; fi' EXIT

[[ "$(uname -s)" == Darwin ]] || { echo 'This installer requires macOS.' >&2; exit 1; }
[[ -d "$source_app" && ! -L "$source_app" ]] || { echo 'Muster.app is missing from the disk image.' >&2; exit 1; }
codesign --verify --strict "$source_app"

printf '%s\n' 'Muster is ad-hoc signed, not notarized by Apple.' \
  'Only continue if you trust this download. This will copy Muster to Applications' \
  'and remove the quarantine attribute from that copy only.'
read -r -p 'Install Muster? [y/N] ' answer
[[ "$answer" == y || "$answer" == Y ]] || { echo 'Installation cancelled.'; exit 0; }

if [[ -e "$destination" || -L "$destination" ]]; then
  echo 'Quit Muster and move the existing /Applications/Muster.app to the Trash first.' >&2
  exit 1
fi
[[ -w /Applications ]] || {
  echo 'Applications is not writable. Copy Muster.app there using Finder instead.' >&2
  echo 'Then, only if you trust it, run: xattr -dr com.apple.quarantine /Applications/Muster.app' >&2
  exit 1
}

# Prepare and check the copy before publishing it. Never merge into an old app.
stage="$(mktemp -d /Applications/.muster-install.XXXXXX)"
ditto "$source_app" "$stage/Muster.app"
codesign --verify --strict "$stage/Muster.app"
xattr -dr com.apple.quarantine "$stage/Muster.app"
mv -n "$stage/Muster.app" /Applications/
[[ ! -e "$stage/Muster.app" ]] || { echo 'Another installation appeared; nothing was replaced.' >&2; exit 1; }
echo 'Muster is installed in Applications. You can eject the disk image and open Muster.'
