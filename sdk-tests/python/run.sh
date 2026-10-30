#!/usr/bin/env bash
# Runs the official Python SDK suite against the permissive server at GCS_MIMIC_ENDPOINT and the
# strict server at GCS_MIMIC_STRICT_ENDPOINT. HTTPS origins also require GCS_MIMIC_CA_CERT.
set -euo pipefail

cd "$(dirname "$0")"
: "${GCS_MIMIC_ENDPOINT:?GCS_MIMIC_ENDPOINT must name the permissive emulator origin}"
: "${GCS_MIMIC_STRICT_ENDPOINT:?GCS_MIMIC_STRICT_ENDPOINT must name the strict emulator origin}"

# --locked fails instead of re-resolving; a warm uv cache makes this step offline-capable.
uv sync --locked --quiet
python=.venv/bin/python
# -B keeps bytecode out of the checkout; -u keeps per-case lines ordered with SDK output.
run() { env -u GCS_MIMIC_TOKEN "$@"; }

echo '== Python SDK self-check' >&2
run "$python" -B -u -m unittest -v test_self_check
echo '== Python SDK migration scenario (permissive)' >&2
run GCS_MIMIC_ENDPOINT="$GCS_MIMIC_ENDPOINT" "$python" -B -u -m unittest -v test_migration
echo '== Python SDK journeys (permissive)' >&2
run GCS_MIMIC_ENDPOINT="$GCS_MIMIC_ENDPOINT" "$python" -B -u -m unittest -v test_journeys
echo '== Python SDK journeys and signed URLs (strict)' >&2
# local-admin-token is a fixture principal from config/identities.json, not a Google credential.
run GCS_MIMIC_ENDPOINT="$GCS_MIMIC_STRICT_ENDPOINT" GCS_MIMIC_TOKEN=local-admin-token \
  "$python" -B -u -m unittest -v test_journeys test_strict
sdk=$(uv pip show --python "$python" google-cloud-storage | sed -n 's/^Version: //p')
echo "$("$python" --version); google-cloud-storage $sdk; permissive and strict Python SDK suites passed"
