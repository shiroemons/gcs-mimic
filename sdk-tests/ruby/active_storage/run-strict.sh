#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/../../.."
root=$PWD
binary=${GCS_MIMIC_BINARY:-$root/target/debug/gcs-mimic}
if [[ ! -x $binary ]]; then echo 'Build the final emulator with cargo build --locked first' >&2; exit 1; fi
(cd sdk-tests/ruby/active_storage && BUNDLE_FROZEN=true bundle check)
work=$(mktemp -d)
pid=
cleanup() {
  status=$?
  trap - EXIT
  if [[ -n $pid ]]; then
    if kill -0 "$pid" 2>/dev/null; then kill -TERM "$pid" || status=$?; fi
    wait "$pid" || status=$?
  fi
  if [[ $status -ne 0 && -f $work/server.log ]]; then sed -n '1,160p' "$work/server.log" >&2; fi
  rm -rf "$work"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
jq -n --rawfile key fixtures/signing/test-only-public.pem \
  '{identities:[{id:"local-admin@example.invalid",tokens:[],public_keys:[$key],project_roles:{"local-project":["roles/storage.admin"]}}]}' \
  >"$work/identities.json"
env -u GCS_MIMIC_CONFIG -u GCS_MIMIC_BINARY -u GCS_MIMIC_API_BASE_URL -u GCS_MIMIC_PUBLIC_BASE_URL \
  -u GCS_MIMIC_XML_VIRTUAL_HOST_DOMAINS -u GCS_MIMIC_TLS_CERT -u GCS_MIMIC_TLS_KEY \
  "$binary" serve --scheme http --listen 127.0.0.1:0 --compat-profile native \
  --auth-mode strict --identity-file "$work/identities.json" --data-dir "$work/data" \
  >"$work/server.log" 2>&1 &
pid=$!
origin=
for ((attempt = 0; attempt < 100; attempt++)); do
  if ! kill -0 "$pid" 2>/dev/null; then echo 'Strict server exited before readiness' >&2; exit 1; fi
  address=$(jq -Rr 'fromjson? | select(.event == "ready") | .listen // empty' "$work/server.log")
  if [[ $address =~ ^127\.0\.0\.1:[0-9]+$ ]]; then
    origin="http://$address"
    if "$binary" healthcheck --url "$origin/_internal/ready" >/dev/null 2>&1; then break; fi
  fi
  origin=
  sleep 0.1
done
if [[ -z $origin ]]; then echo 'Strict server readiness timed out' >&2; exit 1; fi
cd sdk-tests/ruby/active_storage
bundle exec ruby signing_self_check.rb
GCS_MIMIC_ENDPOINT="$origin" bundle exec ruby strict.rb
