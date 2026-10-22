#!/usr/bin/env bash
# Checked build, local packaging and candidate release entry point.
#
# `check`, `build` and `release` are local: they never tag and never publish.
# `candidate` is the one command that publishes, and it publishes through the
# Native release workflow: this script prepares the tree, pushes it to `main`
# and asks the workflow to build, validate and publish it. The workflow is
# what creates the tag and the GitHub release.
set -euo pipefail

usage() {
  cat <<'HELP'
Usage: scripts/release.sh <command>

Commands:
  check      Run formatting, clippy, workspace tests, and RustSec audit
  build      Run check, then build the optimized native app
  release    Run check/build and package the macOS app (macOS only)
  candidate  Prepare, push and publish a release candidate (version argument)
  help       Show this help

Requires Rust/rustup 1.97.1 and Node.js. Linux build prerequisites are listed
in .github/workflows/ci.yml. `release` creates dist/Muster.app locally; it does
not publish, sign for distribution, notarize, or create a git tag.

`candidate <version> [--dry-run]` takes 0.<minor>.<patch> or 1.0.0-rc.<n>. It
writes the version into the manifest and the lock file, renames the
changelog's `Unreleased` heading, points the README at the new candidate,
commits that on `main`, pushes it, and asks the Native release workflow to
publish it. The workflow is what creates the tag and the release, so this
script never creates either, and it refuses a version whose tag already
exists. `--dry-run` stops after the preparation: the tree is left as it would
be, and nothing is committed, pushed or published.
HELP
}

command="${1:-help}"
case "$command" in
  -h|--help|help) usage; exit 0 ;;
  check|build|release) ;;
  candidate) ;;
  *) echo "release: unknown command: $command" >&2; usage >&2; exit 2 ;;
esac

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

require_toolchain() {
  local rustup_path
  rustup_path="$(rustup which --toolchain 1.97.1 rustc)" || {
    echo 'release: Rust toolchain 1.97.1 is required (install with rustup)' >&2
    exit 1
  }
  export PATH="$(dirname "$rustup_path"):$PATH"
  [[ "$(rustc --version)" == rustc\ 1.97.1* ]] || { echo 'release: rustc must be 1.97.1' >&2; exit 1; }
  [[ "$(cargo --version)" == cargo\ 1.97.1* ]] || { echo 'release: cargo must be 1.97.1' >&2; exit 1; }
}

# A file is rewritten through a temporary file and copied back over itself, so
# the file keeps its own mode: `sed -i` takes an argument on macOS and none on
# Linux, and this script runs on both.
rewrite() {
  local file="$1" tmp
  tmp="$(mktemp)"
  cat > "$tmp"
  cat "$tmp" > "$file"
  rm -f "$tmp"
}

# The version the manifest carries, and the one the lock file carries for it.
# The workflow refuses a release whose two disagree, so they are read the same
# way here.
manifest_version() {
  awk -F'"' '/^version = "/{print $2; exit}' app/Cargo.toml
}

lock_version() {
  awk '/^name = "muster"$/{found=1; next} found && /^version = /{gsub(/"/, "", $3); print $3; exit}' Cargo.lock
}

# The version is written in the two places the build reads it from. The
# manifest's own line is the package's; the lock file's is the one that
# follows the package's name.
prepare_version() {
  local version="$1"
  sed "s|^version = \".*\"|version = \"$version\"|" app/Cargo.toml | rewrite app/Cargo.toml
  awk -v version="$version" '
    /^name = "muster"$/ { found = 1 }
    found && /^version = / { print "version = \"" version "\""; found = 0; next }
    { print }
  ' Cargo.lock | rewrite Cargo.lock
}

# The changelog keeps the newest work under `Unreleased` and renames that
# heading to the version when it is tagged. A changelog with no such section
# still gets the notes GitHub generates from the commits.
prepare_changelog() {
  local version="$1"
  sed "s|^## Unreleased\$|## $version|" CHANGELOG.md | rewrite CHANGELOG.md
}

# The README names the candidate people download. Only the download block is
# rewritten: the sentence that says which candidates carry all four builds is
# about the past and keeps the version it was written for.
prepare_readme() {
  local version="$1" previous bare escaped
  previous="$(sed -n 's|^The current candidate is \*\*\(v[^*]*\)\*\*:$|\1|p' README.md | head -1)"
  if [[ -z "$previous" ]]; then
    echo 'release: candidate: the README names no current candidate' >&2
    exit 1
  fi
  bare="${previous#v}"
  escaped="${bare//./\\.}"
  sed -e "s|releases/download/v${escaped}/|releases/download/v${version}/|g" \
      -e "s|Muster_${escaped}_|Muster_${version}_|g" \
      -e "s|muster_${escaped}_|muster_${version}_|g" \
      -e "s|^The current candidate is \*\*v${escaped}\*\*:\$|The current candidate is **v${version}**:|" \
      README.md | rewrite README.md
}

# The repository the release is published in, read from the remote.
repository() {
  local remote
  remote="$(git remote get-url origin)"
  remote="${remote##*:}"
  printf '%s' "${remote%.git}"
}

# The run that publishes this commit. It is looked up by the commit it builds,
# so a run of an earlier release is never mistaken for this one.
publish_run() {
  local sha="$1" run_id="" attempt
  for attempt in $(seq 1 30); do
    run_id="$(gh run list --workflow linux-release.yml --branch main --event workflow_dispatch --limit 20 \
      --json databaseId,headSha --jq "[.[] | select(.headSha == \"$sha\")] | first | .databaseId")"
    if [[ -n "$run_id" && "$run_id" != "null" ]]; then
      printf '%s' "$run_id"
      return 0
    fi
    sleep 2
  done
  echo 'release: candidate: the publish run did not start' >&2
  exit 1
}

# One candidate, from this tree to the release page.
#
# Everything the workflow refuses is refused here first: the version is one of
# the two shapes it publishes, the tag does not exist yet, the commit is on
# `main`, and the tree is clean. A mistake is a failed dispatch rather than a
# published release.
candidate() {
  local version="${1:-}" dry_run=false
  if [[ "${2:-}" == "--dry-run" ]]; then dry_run=true; fi
  if [[ ! "$version" =~ ^(0\.[0-9]+\.[0-9]+|1\.0\.0-rc\.[0-9]+)$ ]]; then
    echo "release: candidate: unsupported version: ${version:-<none>}" >&2
    echo 'release: a version is 0.<minor>.<patch> or 1.0.0-rc.<n>' >&2
    exit 2
  fi
  local tag="v$version" repo sha run_id url
  if [[ "$dry_run" != true ]]; then
    command -v gh >/dev/null || { echo 'release: candidate: the GitHub CLI (gh) is required' >&2; exit 1; }
  fi
  [[ -z "$(git status --porcelain)" ]] || { echo 'release: candidate: the working tree is not clean' >&2; exit 1; }
  [[ "$(git rev-parse --abbrev-ref HEAD)" == main ]] || { echo 'release: candidate: run this on main' >&2; exit 1; }
  git fetch --quiet origin main
  [[ "$(git rev-parse HEAD)" == "$(git rev-parse origin/main)" ]] || {
    echo 'release: candidate: main is not the commit origin/main points at; push or pull first' >&2
    exit 1
  }
  if git ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null 2>&1; then
    echo "release: candidate: the tag $tag already exists" >&2
    exit 1
  fi
  [[ "$(manifest_version)" != "$version" ]] || { echo "release: candidate: the tree is already $version" >&2; exit 1; }

  prepare_version "$version"
  prepare_changelog "$version"
  prepare_readme "$version"
  if [[ "$(manifest_version)" != "$version" || "$(lock_version)" != "$version" ]]; then
    echo 'release: candidate: the manifest and the lock file do not carry the same version' >&2
    exit 1
  fi

  if [[ "$dry_run" == true ]]; then
    echo "release: candidate: $tag is prepared; nothing was committed, pushed or published"
    git diff --stat
    exit 0
  fi

  git add app/Cargo.toml Cargo.lock CHANGELOG.md README.md
  git commit -q -m "release: prepare $tag" \
    -m "The next native testing candidate. The Native release workflow builds and validates every target, and it is what creates the tag and the release."
  git push origin main

  sha="$(git rev-parse HEAD)"
  repo="$(repository)"
  gh workflow run linux-release.yml --ref main -f sha="$sha" -f mode=publish -f tag="$tag"
  run_id="$(publish_run "$sha")"
  url="https://github.com/$repo/actions/runs/$run_id"
  echo "release: candidate: the publish run is $url"
  if ! gh run watch "$run_id" --exit-status; then
    echo "release: candidate: the publish run failed; the tag is $tag and the draft is at $url" >&2
    exit 1
  fi
  gh release view "$tag" --repo "$repo" --json url --jq '"release: candidate: published at " + .url'
}

if [[ "$command" == candidate ]]; then
  candidate "${2:-}" "${3:-}"
  exit 0
fi

require_toolchain

check() {
  cargo fmt --all --check
  cargo clippy --locked --workspace --all-targets -- -D warnings
  cargo test --locked --workspace
  node --test scripts/audit-rust.test.mjs scripts/release.test.mjs scripts/linux-release.test.mjs
  node scripts/audit-rust.mjs
}

check
if [[ "$command" == check ]]; then exit 0; fi
cargo build --locked --release -p muster
if [[ "$command" == release ]]; then
  [[ "$(uname -s)" == Darwin ]] || { echo 'release: packaging requires macOS' >&2; exit 1; }
  scripts/bundle-macos.sh
fi
