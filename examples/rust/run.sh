#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
# --locked builds from the committed Cargo.lock and never rewrites it. Dropping RUSTUP_TOOLCHAIN
# lets rust-toolchain.toml choose the compiler, as the repository's own builds do, so a global
# toolchain override cannot build the example with a compiler other than the pinned one.
env -u RUSTUP_TOOLCHAIN cargo run --locked --quiet
