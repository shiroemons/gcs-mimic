#!/usr/bin/env bash
# Builds the example with the pinned dependency graph and runs it against a local gcs-mimic.
set -euo pipefail
cd "$(dirname "$0")"

export GCS_MIMIC_ENDPOINT="${GCS_MIMIC_ENDPOINT:-http://localhost:4443}"
if [[ ! "$GCS_MIMIC_ENDPOINT" =~ ^http://(localhost|127\.0\.0\.1|\[::1\])(:[0-9]+)?/?$ ]]; then
  echo "GCS_MIMIC_ENDPOINT must be a loopback HTTP origin, got: $GCS_MIMIC_ENDPOINT" >&2
  exit 2
fi

# Never fall back to real Google credentials, projects or the metadata server.
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy ALL_PROXY no_proxy NO_PROXY \
  STORAGE_EMULATOR_HOST GOOGLE_APPLICATION_CREDENTIALS GOOGLE_CLOUD_PROJECT GCLOUD_PROJECT
export GCE_METADATA_DISABLED=true

# Maven has no native lockfile: fail if the resolved graph differs from dependencies.lock.
mvn --batch-mode --no-transfer-progress --quiet dependency:tree \
  -DoutputType=text -DoutputFile=target/dependencies.lock
cmp dependencies.lock target/dependencies.lock

exec mvn --batch-mode --no-transfer-progress --quiet compile exec:java
