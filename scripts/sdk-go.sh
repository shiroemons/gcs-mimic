#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
env -u RUSTUP_TOOLCHAIN cargo build --locked
server="$(env -u RUSTUP_TOOLCHAIN cargo metadata --locked --offline --no-deps --format-version 1 | jq -er .target_directory)/debug/gcs-mimic"
work=$(mktemp -d "${TMPDIR:-/tmp}/gcs-mimic-sdk-go.XXXXXX")
pid=
strict_pid=
stop_server() {
  if kill -0 "$1" 2>/dev/null && ! kill -TERM "$1"; then
    echo 'Server exited while requesting shutdown; collecting its exit status' >&2
  fi
  wait "$1"
}
cleanup() {
  status=$?
  trap - EXIT
  for server_pid in "$pid" "$strict_pid"; do
    if [[ -n $server_pid ]]; then
      if stop_server "$server_pid"; then :; else
        server_status=$?
        if [[ $status -eq 0 ]]; then status=$server_status; fi
      fi
    fi
  done
  if [[ $status -ne 0 ]]; then
    for log in "$work/server.log" "$work/strict-server.log"; do
      if [[ -f $log ]]; then
        echo "--- ${log##*/} ---" >&2
        sed -n '1,120p' "$log" >&2
      fi
    done
  fi
  rm -rf "$work"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Starts one isolated server, storing its PID and origin in the named variables. The PID is stored
# before the readiness wait so the cleanup trap always stops a server that fails to become ready.
# A port is selected only after the previous server listens, so both cannot receive the same port.
# Arguments: PID variable, endpoint variable, log name, required ready-event auth_mode (empty for
# any), extra serve arguments.
start_server() {
  # Local names must differ from the caller's variable names, or printf -v assigns the local.
  local pid_var=$1 endpoint_var=$2 name=$3 auth_mode=$4 address origin actual server_pid ready=false
  shift 4
  # Select the advertised origin before startup; bind failure remains fatal if the port is reused.
  address=$(cd sdk-tests/go && GOTOOLCHAIN=auto go run ./cmd/free-port)
  if [[ ! $address =~ ^127\.0\.0\.1:[0-9]+$ ]]; then echo 'Invalid loopback address' >&2; exit 1; fi
  origin="http://$address"
  env -u GCS_MIMIC_CONFIG -u GCS_MIMIC_ENDPOINT -u GCS_MIMIC_STRICT_ENDPOINT -u GCS_MIMIC_ALLOWED_HOSTS \
    -u GCS_MIMIC_GO_TEST_RUN -u GCS_MIMIC_AUTH_MODE -u GCS_MIMIC_IDENTITY_FILE \
    "$server" serve --listen "$address" --api-base-url "$origin" "$@" >"$work/$name" 2>&1 &
  server_pid=$!
  printf -v "$pid_var" '%s' "$server_pid"
  printf -v "$endpoint_var" '%s' "$origin"
  for ((attempt = 0; attempt < 100; attempt++)); do
    if ! kill -0 "$server_pid" 2>/dev/null; then echo "Server exited before readiness: $name" >&2; exit 1; fi
    # A server whose ready event lacks the requested auth_mode must not pass as a strict server.
    actual=$(jq -Rr --arg mode "$auth_mode" \
      'fromjson? | select(.event == "ready" and ($mode == "" or .auth_mode == $mode)) | .listen // empty' \
      "$work/$name")
    if [[ $actual == "$address" ]]; then
      if "$server" healthcheck --url "$origin/_internal/ready" >/dev/null 2>&1; then ready=true; break; fi
    fi
    sleep 0.1
  done
  if [[ $ready != true ]]; then echo "Server readiness timed out: $name" >&2; exit 1; fi
}

endpoint=
strict_endpoint=
start_server pid endpoint server.log '' --data-dir "$work/data"
# The identity file holds local-only fixture principals; its tokens are not Google credentials.
start_server strict_pid strict_endpoint strict-server.log strict --data-dir "$work/strict-data" \
  --auth-mode strict --identity-file config/identities.json

cd sdk-tests/go
export GCS_MIMIC_ENDPOINT="$endpoint" GCS_MIMIC_STRICT_ENDPOINT="$strict_endpoint"
GOTOOLCHAIN=auto go vet ./...
GOTOOLCHAIN=auto go test -count=1 -v -run "${GCS_MIMIC_GO_TEST_RUN:-.}" ./...
