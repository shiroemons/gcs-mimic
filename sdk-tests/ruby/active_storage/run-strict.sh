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
# stops a server that fails to become ready. The ready event's authentication summary is printed
# so that each variant's output shows the identities and listeners it ran with.
# Arguments: variant name, Active Storage service, extra serve arguments such as the identities.
run_variant() {
  local name=$1 service=$2 origin= address attempt server_status=0
  shift 2
  log="$work/$name-server.log"
  env -u GCS_MIMIC_CONFIG -u GCS_MIMIC_BINARY -u GCS_MIMIC_API_BASE_URL -u GCS_MIMIC_PUBLIC_BASE_URL \
    -u GCS_MIMIC_XML_VIRTUAL_HOST_DOMAINS -u GCS_MIMIC_TLS_CERT -u GCS_MIMIC_TLS_KEY \
    -u GCS_MIMIC_IDENTITY_FILE -u GCS_MIMIC_IDENTITIES -u GCS_MIMIC_BUCKETS_FILE -u GCS_MIMIC_BUCKETS \
    -u GCS_MIMIC_TEST_SIGNER -u GCS_MIMIC_TRUSTED_LISTEN -u GCS_MIMIC_TRUSTED_PRINCIPAL \
    "$binary" serve --scheme http --listen 127.0.0.1:0 --compat-profile native \
    --auth-mode strict --data-dir "$work/$name-data" "$@" \
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
  echo "Variant $name: $(jq -Rc 'fromjson? | select(.event == "ready")
    | {auth_mode, identities, test_signer, listen, trusted_listen, trusted_principal}' "$log")"
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
run_variant registered-key gcs_emulator_strict --identity-file "$work/identities.json"

# The built-in test signer with the documented Rails recipe: the key comes from
# `gcs-mimic test-signer credentials`, and the server registers its published key itself. The
# signer holds only roles/storage.objectUser, because anyone can sign with its key; that role
# cannot create buckets, so the server's buckets file creates them. The server reads only
# GCS_MIMIC_ and FAKE_GCS_ variables, so the settings for storage.yml and strict.rb use another
# prefix.
"$binary" test-signer credentials | jq -r .private_key >"$work/test-signer.pem"
export GCS_EMULATOR_TEST_SIGNER_KEY="$work/test-signer.pem"
export GCS_EMULATOR_BUCKET=app-private GCS_EMULATOR_PUBLIC_BUCKET=app-public
buckets=sdk-tests/ruby/active_storage/strict-buckets.json

# The recipe's way: no identity document, so the enabled signer holds the implicit
# roles/storage.objectUser in the buckets document's project. The trusted listener needs no
# identity document either; Rails never uses it, because it always presents its own JWT.
run_variant test-signer-implicit gcs_emulator_test_signer \
  --test-signer enabled --buckets-file "$buckets" --trusted-listen 127.0.0.1:0

# An identity file that declares the signer with exactly that role and no keys, for applications
# that also register their own identities.
jq -n '{identities:[{id:"test-signer@gcs-mimic.invalid",tokens:[],project_roles:{"local-project":["roles/storage.objectUser"]}}]}' \
  >"$work/test-signer-identities.json"
run_variant test-signer-declared gcs_emulator_test_signer --identity-file "$work/test-signer-identities.json" \
  --test-signer enabled --buckets-file "$buckets"
