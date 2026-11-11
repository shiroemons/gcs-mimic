#!/usr/bin/env bash
# Runs the official Python SDK suite against the permissive server at GCSM_ENDPOINT and the
# strict server at GCSM_STRICT_ENDPOINT. HTTPS origins also require GCSM_CA_CERT.
set -euo pipefail

cd "$(dirname "$0")"
: "${GCSM_ENDPOINT:?GCSM_ENDPOINT must name the permissive emulator origin}"
: "${GCSM_STRICT_ENDPOINT:?GCSM_STRICT_ENDPOINT must name the strict emulator origin}"

# --locked fails instead of re-resolving; a warm uv cache makes this step offline-capable.
uv sync --locked --quiet
python=.venv/bin/python
# -B keeps bytecode out of the checkout; -u keeps per-case lines ordered with SDK output.
run() { env -u GCSM_TOKEN "$@"; }

echo '== Python SDK self-check' >&2
run "$python" -B -u -m unittest -v test_self_check
echo '== Python SDK migration scenario (permissive)' >&2
run GCSM_ENDPOINT="$GCSM_ENDPOINT" "$python" -B -u -m unittest -v test_migration
echo '== Python SDK journeys (permissive)' >&2
run GCSM_ENDPOINT="$GCSM_ENDPOINT" "$python" -B -u -m unittest -v test_journeys
echo '== Python SDK journeys and signed URLs (strict)' >&2
# local-admin-token is a fixture principal from config/identities.json, not a Google credential.
run GCSM_ENDPOINT="$GCSM_STRICT_ENDPOINT" GCSM_TOKEN=local-admin-token \
  "$python" -B -u -m unittest -v test_journeys test_strict
sdk=$(uv pip show --python "$python" google-cloud-storage | sed -n 's/^Version: //p')
echo "$("$python" --version); google-cloud-storage $sdk; permissive and strict Python SDK suites passed"
