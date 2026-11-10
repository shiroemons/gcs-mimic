#!/usr/bin/env bash
# Keep this grammar in agreement with the parsed release version through xtask's shared cases.
set -euo pipefail

if [[ $# != 1 ]]; then
  echo 'Usage: bash scripts/release-version.sh VERSION' >&2
  exit 2
fi
if [[ ! "$1" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-rc\.[1-9][0-9]*)?$ ]]; then
  echo 'Releases require MAJOR.MINOR.PATCH or MAJOR.MINOR.PATCH-rc.N (N >= 1), without leading zeros or build metadata' >&2
  exit 1
fi
