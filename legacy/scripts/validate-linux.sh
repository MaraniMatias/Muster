#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
IMAGE=muster-release-validation:ubuntu-22.04-node22-rust1.97.1

command -v docker >/dev/null || { echo "Docker is required for release validation (install/start Docker Desktop or OrbStack)." >&2; exit 1; }
docker info >/dev/null 2>&1 || { echo "Docker is installed but its Linux daemon is unavailable; start Docker Desktop or OrbStack and retry." >&2; exit 1; }

version="$(node -p "require('./package.json').version")"
sha="$(git rev-parse HEAD)"
tmp_root="${TMPDIR:-/tmp}"
if [[ "$(uname -s)" == Darwin && "$tmp_root" == /var/* ]]; then tmp_root="/private/var${tmp_root#/var}"; fi
out="$(mktemp -d "$tmp_root/muster-linux-release.XXXXXX")"
trap 'rm -rf "$out"' EXIT

package_manager="$(node -p "require('./package.json').packageManager")"
[[ "$package_manager" == pnpm@* ]] || { echo "package.json must declare pnpm in packageManager" >&2; exit 1; }
pnpm_version="${package_manager#pnpm@}"
pnpm_version="${pnpm_version%%+*}"
[[ "$pnpm_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "unsupported pnpm packageManager version: $package_manager" >&2; exit 1; }

docker build --platform linux/amd64 --build-arg "PNPM_VERSION=$pnpm_version" -t "$IMAGE" -f "$ROOT/scripts/release-validation.Dockerfile" "$ROOT/scripts"

COPYFILE_DISABLE=1 tar -cf - -C "$ROOT" \
  --exclude='.git' --exclude='node_modules' --exclude='src-tauri/target' --exclude='dist' \
  --exclude='.pnpm-store' --exclude='.env' --exclude='.env.*' --exclude='.npmrc' \
  --exclude='.netrc' --exclude='.ssh' --exclude='.aws' . |
  docker run --platform linux/amd64 --rm -i \
    --mount type=volume,src=muster-release-pnpm-linux-amd64,dst=/root/.local/share/pnpm/store \
    --mount type=volume,src=muster-release-cargo-linux-amd64,dst=/root/.cargo/registry \
    --mount type=volume,src=muster-release-target-rust1.97.1-linux-amd64,dst=/workspace/src-tauri/target \
    --mount "type=bind,src=$out,dst=/out" \
    -e RELEASE_VERSION="$version" -e RELEASE_SHA="$sha" -e APPIMAGE_EXTRACT_AND_RUN=1 \
    "$IMAGE" bash -euo pipefail -c '
      mkdir -p /workspace
      tar -xf - -C /workspace
      cd /workspace
      timeout --kill-after=30s 20m pnpm install --frozen-lockfile
      timeout --kill-after=30s 60m pnpm run build:app -- --target x86_64-unknown-linux-gnu --bundles deb,appimage
      node scripts/release-artifacts.mjs collect --root src-tauri/target --target x86_64-unknown-linux-gnu --version "$RELEASE_VERSION" --sha "$RELEASE_SHA" --out /out
    '

for ext in deb AppImage; do
  count="$(find "$out" -maxdepth 1 -type f -name "*.$ext" -size +0c | wc -l | tr -d ' ')"
  [[ "$count" == 1 ]] || { echo "Linux validation expected one nonempty .$ext artifact; found $count" >&2; exit 1; }
done
[[ -s "$out/release-target.json" ]] || { echo "Linux validation did not produce its target manifest" >&2; exit 1; }
printf 'Docker Linux validation passed; collected artifacts:\n'
find "$out" -maxdepth 1 -type f -print | sort