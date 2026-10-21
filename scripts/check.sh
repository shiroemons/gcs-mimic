#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
cargo fetch --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo test --workspace --locked --offline
cargo xtask compatibility check
for script in scripts/*.sh; do bash -n "$script"; done
bash scripts/smoke-containers.sh --help
if bash scripts/smoke-containers.sh linux/unknown; then
  echo 'Container smoke accepted an unsupported platform' >&2
  exit 1
else
  test "$?" = 2
fi
