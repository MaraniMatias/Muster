#!/usr/bin/env bash
set -euo pipefail
version="${1:?usage: debian-version.sh semver}"
if [[ ! "$version" =~ ^([0-9]+\.[0-9]+\.[0-9]+)(-([0-9A-Za-z.-]+))?(\+[0-9A-Za-z.-]+)?$ ]]; then
  echo "invalid SemVer: $version" >&2
  exit 1
fi
printf '%s%s%s\n' "${BASH_REMATCH[1]}" "${BASH_REMATCH[3]:+~${BASH_REMATCH[3]}}" "${BASH_REMATCH[4]:+${BASH_REMATCH[4]}}"
