#!/usr/bin/env bash
# Type-checks release targets that the host cannot test, so cfg and API
# portability errors surface in `just check` instead of at release time.
set -euo pipefail

cd "$(dirname "$0")/.."
host=$(rustc -vV | sed -n 's/^host: //p')
targets=(x86_64-pc-windows-msvc x86_64-unknown-linux-musl aarch64-apple-darwin)
stub="$PWD/scripts/cross-cc-stub.sh"
installed=$(rustup target list --installed)
for target in "${targets[@]}"; do
  if [[ $target == "$host" ]]; then
    continue
  fi
  if ! grep -qx "$target" <<<"$installed"; then
    rustup target add "$target"
  fi
  # Clippy needs build scripts to succeed but never links, so C dependencies
  # compile to empty objects instead of requiring a cross C toolchain.
  env_target=${target//-/_}
  env "CC_$env_target=$stub" "AR_$env_target=$stub" \
    CARGO_TARGET_DIR=target/cross \
    cargo clippy --workspace --all-targets --locked --offline --target "$target" -- -D warnings
done
