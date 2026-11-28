#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum --check src/grpc/proto/SHA256SUMS
else
  shasum -a 256 -c src/grpc/proto/SHA256SUMS
fi

output=$(mktemp -d)
trap 'rm -rf "$output"' EXIT
CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-4} cargo run --locked --offline -p grpc-codegen -- --out-dir "$output"
# The crate manifest and root are written by hand; the generator writes every other file.
diff -ru -x Cargo.toml -x lib.rs src/grpc/generated "$output"
