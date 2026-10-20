#!/usr/bin/env bash
# Measure the native terminal's hot paths and print the table.
#
# The measurement lives in `src/perf.rs` as `#[ignore]`d cases that print
# rather than assert, so it is reproducible and cannot become a budget written
# on one machine and failed on another. This script is only the command.
#
#   scripts/perf-native.sh            # release build, as the app ships
#   scripts/perf-native.sh --debug    # the debug build, for comparison
#
# Report the toolchain, the machine and the numbers together: a figure without
# them is not a measurement.
set -euo pipefail

cd "$(dirname "$0")/.."

TOOLCHAIN="1.97.1"
PROFILE="release"

if [[ "${1:-}" == "--debug" ]]; then
  PROFILE="debug"
  shift
fi

echo "toolchain : $(rustup run "$TOOLCHAIN" rustc --version)"
echo "profile   : $PROFILE"
if [[ "$(uname -s)" == "Darwin" ]]; then
  echo "machine   : $(sysctl -n machdep.cpu.brand_string) / $(uname -mr)"
  echo "memory    : $(( $(sysctl -n hw.memsize) / 1073741824 )) GiB"
else
  echo "machine   : $(uname -m) / $(uname -sr)"
fi

profile_args=()
if [[ "$PROFILE" == "release" ]]; then profile_args+=(--release); fi
rustup run "$TOOLCHAIN" cargo test --locked -p muster perf:: \
  "${profile_args[@]}" -- --ignored --nocapture --test-threads=1 "$@"

# Hidden output must coalesce and dropping the view must release workers.
rustup run "$TOOLCHAIN" cargo test --locked -p muster \
  an_unmounted_view_coalesces_output_and_does_not_retain_its_session \
  "${profile_args[@]}" -- --test-threads=1
