#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
: "${GCSM_ENDPOINT:?set a bare loopback HTTP endpoint}"
: "${GCSM_GRPC_ENDPOINT:?set the distinct loopback gRPC host:port}"
# Dropping RUSTUP_TOOLCHAIN lets rust-toolchain.toml choose the compiler, as scripts/sdk-go.sh
# does for the server, so a global toolchain override cannot build the suite with another one.
env -u RUSTUP_TOOLCHAIN cargo build --locked
for client in official arrow; do
  (cd ../../go && go run ./cmd/sdk-guard --endpoint "$GCSM_ENDPOINT" \
    --grpc-endpoint "http://$GCSM_GRPC_ENDPOINT" -- "../sdk-tests/rust/target/debug/$client")
done
