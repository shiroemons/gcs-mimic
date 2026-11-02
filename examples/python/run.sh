#!/usr/bin/env bash
# Installs the locked packages and runs the example against the gcs-mimic at GCS_MIMIC_ENDPOINT.
set -euo pipefail
cd "$(dirname "$0")"

export GCS_MIMIC_ENDPOINT=${GCS_MIMIC_ENDPOINT:-http://localhost:4443}
if [[ ! $GCS_MIMIC_ENDPOINT =~ ^http://(localhost|127\.[0-9]+\.[0-9]+\.[0-9]+|\[::1\])(:[0-9]+)?/?$ ]]; then
  echo "GCS_MIMIC_ENDPOINT must be a loopback http:// origin, not $GCS_MIMIC_ENDPOINT" >&2
  exit 2
fi
# Keep the run local: no proxies and no ambient Google settings.
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy ALL_PROXY \
  GOOGLE_APPLICATION_CREDENTIALS GOOGLE_CLOUD_PROJECT GCLOUD_PROJECT STORAGE_EMULATOR_HOST

# --locked installs exactly uv.lock and fails instead of rewriting it.
uv run --locked --quiet python main.py
