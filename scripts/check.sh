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
for script in scripts/*.sh; do bash -n "$script"; done
bash scripts/smoke-containers.sh --help
if bash scripts/smoke-containers.sh linux/unknown; then
  echo 'Container smoke accepted an unsupported platform' >&2
  exit 1
else
  test "$?" = 2
fi
