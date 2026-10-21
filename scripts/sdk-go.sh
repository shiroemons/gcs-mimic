#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
env -u RUSTUP_TOOLCHAIN cargo build --locked
server="$(env -u RUSTUP_TOOLCHAIN cargo metadata --locked --offline --no-deps --format-version 1 | jq -er .target_directory)/debug/gcs-mimic"
work=$(mktemp -d "${TMPDIR:-/tmp}/gcs-mimic-sdk-go.XXXXXX")
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
  if [[ $status -ne 0 && -f $work/server.log ]]; then sed -n '1,120p' "$work/server.log" >&2; fi
  rm -rf "$work"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Select the advertised origin before startup; bind failure remains fatal if the port is reused.
address=$(cd sdk-tests/go && GOTOOLCHAIN=auto go run ./cmd/free-port)
if [[ ! $address =~ ^127\.0\.0\.1:[0-9]+$ ]]; then echo 'Invalid loopback address' >&2; exit 1; fi
endpoint="http://$address"
env -u GCS_MIMIC_CONFIG -u GCS_MIMIC_ENDPOINT -u GCS_MIMIC_ALLOWED_HOSTS \
  "$server" serve --listen "$address" --api-base-url "$endpoint" \
  --data-dir "$work/data" >"$work/server.log" 2>&1 &
pid=$!
ready=false
for ((attempt = 0; attempt < 100; attempt++)); do
  if ! kill -0 "$pid" 2>/dev/null; then echo 'Server exited before readiness' >&2; exit 1; fi
  actual=$(jq -Rr 'fromjson? | select(.event == "ready") | .listen // empty' "$work/server.log")
  if [[ $actual == "$address" ]]; then
    if "$server" healthcheck --url "$endpoint/_internal/ready" >/dev/null 2>&1; then ready=true; break; fi
  fi
  sleep 0.1
done
if [[ $ready != true ]]; then echo 'Server readiness timed out' >&2; exit 1; fi
cd sdk-tests/go
GCS_MIMIC_ENDPOINT="$endpoint" GOTOOLCHAIN=auto go vet ./...
GCS_MIMIC_ENDPOINT="$endpoint" GOTOOLCHAIN=auto go test -count=1 -v ./...
