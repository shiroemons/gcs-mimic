#!/usr/bin/env bash
# Runs the official Node.js SDK suite against the permissive and strict servers started by
# scripts/sdk-go.sh. Inputs: GCS_MIMIC_ENDPOINT, GCS_MIMIC_STRICT_ENDPOINT and, for HTTPS,
# GCS_MIMIC_CA_CERT.
set -euo pipefail

cd "$(dirname "$0")"
: "${GCS_MIMIC_ENDPOINT:?GCS_MIMIC_ENDPOINT must name the permissive emulator origin}"
: "${GCS_MIMIC_STRICT_ENDPOINT:?GCS_MIMIC_STRICT_ENDPOINT must name the strict emulator origin}"

major=$(node -p 'process.versions.node.split(".")[0]')
if (( major < 24 )); then
  echo "Node.js 24 or newer is required; found $(node --version)" >&2
  exit 1
fi

# --ignore-scripts keeps dependency install hooks from running; the lockfile pins every package.
npm ci --ignore-scripts --no-audit --no-fund

# Proxies, credential discovery and TLS overrides would make the run non-hermetic or less strict.
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy ALL_PROXY no_proxy NO_PROXY \
  STORAGE_EMULATOR_HOST GOOGLE_APPLICATION_CREDENTIALS GOOGLE_CLOUD_PROJECT GCLOUD_PROJECT \
  NODE_TLS_REJECT_UNAUTHORIZED NODE_OPTIONS GCS_MIMIC_TOKEN GCS_MIMIC_SIGNED_TESTS
export METADATA_SERVER_DETECTION=none
if [[ -n ${GCS_MIMIC_CA_CERT:-} ]]; then
  if [[ ! -r $GCS_MIMIC_CA_CERT ]]; then echo 'GCS_MIMIC_CA_CERT is unreadable' >&2; exit 1; fi
  # NODE_EXTRA_CA_CERTS is read at process start and adds the CA without disabling verification.
  export NODE_EXTRA_CA_CERTS="$GCS_MIMIC_CA_CERT"
fi

run() {
  echo "== node: $1" >&2
  shift
  node --test --test-reporter=spec --test-timeout=60000 "$@"
}

run 'origin self-check' self-check.test.mjs
GCS_MIMIC_ENDPOINT="$GCS_MIMIC_ENDPOINT" run 'migration scenario (anonymous)' migration-scenario.test.mjs
GCS_MIMIC_ENDPOINT="$GCS_MIMIC_ENDPOINT" run 'permissive suite (anonymous)' suite.test.mjs
GCS_MIMIC_ENDPOINT="$GCS_MIMIC_STRICT_ENDPOINT" GCS_MIMIC_TOKEN=local-admin-token GCS_MIMIC_SIGNED_TESTS=1 \
  run 'strict suite (static token, signed URLs)' suite.test.mjs
