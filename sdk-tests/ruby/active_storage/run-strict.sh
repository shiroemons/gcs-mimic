#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/../../.."
root=$PWD
binary=${GCS_MIMIC_BINARY:-$root/target/debug/gcs-mimic}
if [[ ! -x $binary ]]; then echo 'Build the final emulator with cargo build --locked first' >&2; exit 1; fi
(cd sdk-tests/ruby/active_storage && BUNDLE_FROZEN=true bundle check)
work=$(mktemp -d)
pid=
log=
cleanup() {
  status=$?
  trap - EXIT
  if [[ -n $pid ]]; then
    if kill -0 "$pid" 2>/dev/null; then kill -TERM "$pid" || status=$?; fi
    wait "$pid" || status=$?
  fi
  if [[ $status -ne 0 && -n $log && -f $log ]]; then sed -n '1,160p' "$log" >&2; fi
  rm -rf "$work"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Starts an isolated strict server, runs strict.rb against it with the named storage.yml service,
# then stops the server. The PID is stored before the readiness wait so the cleanup trap always
# stops a server that fails to become ready.
# Arguments: variant name, Active Storage service, identity file, extra serve arguments.
run_variant() {
  local name=$1 service=$2 identities=$3 origin= address attempt server_status=0
  shift 3
  log="$work/$name-server.log"
  env -u GCS_MIMIC_CONFIG -u GCS_MIMIC_BINARY -u GCS_MIMIC_API_BASE_URL -u GCS_MIMIC_PUBLIC_BASE_URL \
    -u GCS_MIMIC_XML_VIRTUAL_HOST_DOMAINS -u GCS_MIMIC_TLS_CERT -u GCS_MIMIC_TLS_KEY \
    "$binary" serve --scheme http --listen 127.0.0.1:0 --compat-profile native \
    --auth-mode strict --identity-file "$identities" --data-dir "$work/$name-data" "$@" \
    >"$log" 2>&1 &
  pid=$!
  for ((attempt = 0; attempt < 100; attempt++)); do
    if ! kill -0 "$pid" 2>/dev/null; then echo "Strict server exited before readiness: $name" >&2; exit 1; fi
    address=$(jq -Rr 'fromjson? | select(.event == "ready") | .listen // empty' "$log")
    if [[ $address =~ ^127\.0\.0\.1:[0-9]+$ ]]; then
      origin="http://$address"
      if "$binary" healthcheck --url "$origin/_internal/ready" >/dev/null 2>&1; then break; fi
    fi
    origin=
    sleep 0.1
  done
  if [[ -z $origin ]]; then echo "Strict server readiness timed out: $name" >&2; exit 1; fi
  (cd sdk-tests/ruby/active_storage && GCS_MIMIC_ENDPOINT="$origin" bundle exec ruby strict.rb "$service")
  kill -TERM "$pid"
  wait "$pid" || server_status=$?
  pid=
  if [[ $server_status -ne 0 ]]; then echo "Strict server exited with status $server_status: $name" >&2; exit "$server_status"; fi
}

(cd sdk-tests/ruby/active_storage && bundle exec ruby signing_self_check.rb)

# A registered identity whose public key is the committed fixture key.
jq -n --rawfile key fixtures/signing/test-only-public.pem \
  '{identities:[{id:"local-admin@example.invalid",tokens:[],public_keys:[$key],project_roles:{"local-project":["roles/storage.admin"]}}]}' \
  >"$work/identities.json"
run_variant registered-key gcs_emulator_strict "$work/identities.json"

# The built-in test signer with the documented Rails recipe: the key comes from
# `gcs-mimic test-signer credentials`, and the identity file declares the signer with project roles
# but no keys, because enabling the signer adds its published key. The server reads only GCS_MIMIC_
# and FAKE_GCS_ variables, so the key path for storage.yml uses another prefix.
"$binary" test-signer credentials | jq -r .private_key >"$work/test-signer.pem"
jq -n '{identities:[{id:"test-signer@gcs-mimic.invalid",tokens:[],project_roles:{"local-project":["roles/storage.admin"]}}]}' \
  >"$work/test-signer-identities.json"
export GCS_EMULATOR_TEST_SIGNER_KEY="$work/test-signer.pem"
run_variant test-signer gcs_emulator_test_signer "$work/test-signer-identities.json" --test-signer enabled
