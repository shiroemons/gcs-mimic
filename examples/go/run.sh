#!/usr/bin/env bash
# Runs the example against a local gcs-mimic using only the committed go.sum.
set -euo pipefail
cd "$(dirname "$0")"

export GCS_MIMIC_ENDPOINT=${GCS_MIMIC_ENDPOINT:-http://localhost:4443}
if [[ ! $GCS_MIMIC_ENDPOINT =~ ^http://(localhost|127\.0\.0\.1|\[::1\])(:[0-9]+)?/?$ ]]; then
  echo "GCS_MIMIC_ENDPOINT must be a loopback http:// origin, got: $GCS_MIMIC_ENDPOINT" >&2
  exit 2
fi

# Keep ambient credentials, projects and proxies out of the run; the program
# sets STORAGE_EMULATOR_HOST itself from GCS_MIMIC_ENDPOINT.
unset STORAGE_EMULATOR_HOST GOOGLE_APPLICATION_CREDENTIALS GOOGLE_CLOUD_PROJECT \
  http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy ALL_PROXY no_proxy NO_PROXY

# -mod=readonly fails instead of rewriting go.mod or go.sum.
GOFLAGS=-mod=readonly go run .
