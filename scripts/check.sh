#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
cargo fetch --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo xtask lint
# CI type-checks the other targets in a parallel job; set GCSM_CHECK_CROSS=0 to skip them here.
case ${GCSM_CHECK_CROSS:-1} in
  0) ;;
  1) bash scripts/check-cross.sh ;;
  *) echo 'GCSM_CHECK_CROSS must be 0 or 1' >&2; exit 2 ;;
esac
bash scripts/test-rust.sh
bash scripts/check-docs.sh
bash scripts/test-container-release.sh
bash scripts/test-container-mirror.sh
bash scripts/test-apply-pin-patch.sh
bash scripts/test-find-release-dry-run.sh
for script in scripts/*.sh; do bash -n "$script"; done
# macOS ships bash 3.2, whose `set -e` ignores a failed `[[ ]]` statement, so such a check stops
# only on CI. Spell out the failure instead: `[[ ... ]] || exit 1`, or `|| return 1` in a function.
if grep -nE '^[[:space:]]*\[\[ .* \]\][[:space:]]*(#.*)?$' scripts/*.sh; then
  echo 'A bare [[ ]] check does nothing under bash 3.2' >&2
  exit 1
fi
bash scripts/smoke-containers.sh --help
if bash scripts/smoke-containers.sh linux/unknown; then
  echo 'Container smoke accepted an unsupported platform' >&2
  exit 1
else
  test "$?" = 2
fi
