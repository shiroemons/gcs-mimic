#!/usr/bin/env bash
# Runs the example against a local gcs-mimic using only the committed Cargo.lock.
set -euo pipefail
cd "$(dirname "$0")"

export GCS_MIMIC_ENDPOINT=${GCS_MIMIC_ENDPOINT:-http://localhost:4443}
export GCS_MIMIC_GRPC_ENDPOINT=${GCS_MIMIC_GRPC_ENDPOINT:-http://localhost:4444}
for endpoint in "$GCS_MIMIC_ENDPOINT" "$GCS_MIMIC_GRPC_ENDPOINT"; do
  if [[ ! $endpoint =~ ^http://(localhost|127\.0\.0\.1|\[::1\])(:[0-9]+)?/?$ ]]; then
    echo "endpoints must be loopback http:// origins, got: $endpoint" >&2
    exit 2
  fi
done

# The example uses anonymous credentials; also keep ambient credentials and
# proxies out of the run.
unset GOOGLE_APPLICATION_CREDENTIALS GOOGLE_CLOUD_PROJECT \
  http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy ALL_PROXY no_proxy NO_PROXY

cargo run --locked --quiet
