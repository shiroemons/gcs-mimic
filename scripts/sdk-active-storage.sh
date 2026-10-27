#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
suite="$PWD/sdk-tests/ruby/active_storage"
(cd "$suite" && BUNDLE_FROZEN=true bundle check)
env -u RUSTUP_TOOLCHAIN cargo build --locked
server="$(env -u RUSTUP_TOOLCHAIN cargo metadata --locked --offline --no-deps --format-version 1 | jq -er .target_directory)/debug/gcs-mimic"
work=$(mktemp -d)
pid=
cleanup() {
  status=$?
  trap - EXIT
  if [[ -n $pid ]]; then
    if kill -0 "$pid" 2>/dev/null && ! kill -TERM "$pid"; then
      echo 'Server exited while requesting shutdown; collecting its exit status' >&2
    fi
    if wait "$pid"; then :; else
      server_status=$?
      if [[ $status -eq 0 ]]; then status=$server_status; fi
    fi
  fi
  if [[ $status -ne 0 && -f $work/server.log ]]; then
    sed -n '1,120p' "$work/server.log" >&2
  fi
  rm -rf "$work"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

address=$(cd sdk-tests/go && GOTOOLCHAIN=auto go run ./cmd/free-port)
if [[ ! $address =~ ^127\.0\.0\.1:[0-9]+$ ]]; then echo 'Invalid loopback address' >&2; exit 1; fi
origin="http://$address"
env -u GCS_MIMIC_CONFIG -u GCS_MIMIC_AUTH_MODE -u GCS_MIMIC_IDENTITY_FILE \
  -u GCS_MIMIC_XML_VIRTUAL_HOST_DOMAINS -u GCS_MIMIC_TLS_CERT -u GCS_MIMIC_TLS_KEY \
  "$server" serve --scheme http --listen "$address" --api-base-url "$origin" \
  --auth-mode permissive --data-dir "$work/data" >"$work/server.log" 2>&1 &
pid=$!
ready=false
for ((attempt = 0; attempt < 100; attempt++)); do
  if ! kill -0 "$pid" 2>/dev/null; then echo 'Server exited before readiness' >&2; exit 1; fi
  actual=$(jq -Rr 'fromjson? | select(.event == "ready") | .listen // empty' "$work/server.log")
  if [[ $actual == "$address" ]] && "$server" healthcheck --url "$origin/_internal/ready" >/dev/null 2>&1; then
    ready=true
    break
  fi
  sleep 0.1
done
if [[ $ready != true ]]; then echo 'Server readiness timed out' >&2; exit 1; fi
cd "$suite"
bundle exec ruby scenario.rb --self-check
GCS_MIMIC_ENDPOINT="$origin" bundle exec ruby scenario.rb
