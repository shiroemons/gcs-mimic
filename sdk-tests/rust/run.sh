#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
: "${GCS_MIMIC_ENDPOINT:?set a bare loopback HTTP endpoint}"
: "${GCS_MIMIC_GRPC_ENDPOINT:?set the distinct loopback gRPC host:port}"
cargo build --locked
for client in official arrow; do
  (cd ../../go && go run ./cmd/sdk-guard --endpoint "$GCS_MIMIC_ENDPOINT" \
    --grpc-endpoint "http://$GCS_MIMIC_GRPC_ENDPOINT" -- "../sdk-tests/rust/target/debug/$client")
done
