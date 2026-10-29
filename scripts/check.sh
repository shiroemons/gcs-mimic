#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
cargo fetch --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
bash scripts/check-cross.sh
bash scripts/test-rust.sh
cargo xtask compatibility check
bash scripts/test-container-release.sh
bash scripts/test-container-mirror.sh
bash scripts/test-dockerhub-description.sh
for script in scripts/*.sh; do bash -n "$script"; done
bash scripts/smoke-containers.sh --help
if bash scripts/smoke-containers.sh linux/unknown; then
  echo 'Container smoke accepted an unsupported platform' >&2
  exit 1
else
  test "$?" = 2
fi
