#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
# --locked builds from the committed Cargo.lock and never rewrites it.
cargo run --locked --quiet
