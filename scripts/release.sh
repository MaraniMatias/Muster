#!/usr/bin/env bash
# Checked build and local packaging entry point. Never publishes or tags.
set -euo pipefail

usage() {
  cat <<'HELP'
Usage: scripts/release.sh <command>

Commands:
  check    Run formatting, clippy, workspace tests, and RustSec audit
  build    Run check, then build the optimized native app
  release  Run check/build and package the macOS app (macOS only)
  help     Show this help

Requires Rust/rustup 1.97.1 and Node.js. Linux build prerequisites are listed
in .github/workflows/ci.yml. `release` creates dist/Muster.app locally; it does
not publish, sign for distribution, notarize, or create a git tag.
HELP
}

command="${1:-help}"
case "$command" in
  -h|--help|help) usage; exit 0 ;;
  check|build|release) ;;
  *) echo "release: unknown command: $command" >&2; usage >&2; exit 2 ;;
esac

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

rustup_path="$(rustup which --toolchain 1.97.1 rustc)" || {
  echo 'release: Rust toolchain 1.97.1 is required (install with rustup)' >&2
  exit 1
}
export PATH="$(dirname "$rustup_path"):$PATH"
[[ "$(rustc --version)" == rustc\ 1.97.1* ]] || { echo 'release: rustc must be 1.97.1' >&2; exit 1; }
[[ "$(cargo --version)" == cargo\ 1.97.1* ]] || { echo 'release: cargo must be 1.97.1' >&2; exit 1; }

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
