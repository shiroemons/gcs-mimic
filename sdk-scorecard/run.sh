#!/usr/bin/env bash
# Runs the Go and Ruby SDK scorecard probes against each emulator with the same
# inputs and records raw JSONL results, product versions and a scored summary.
set -euo pipefail

usage() { echo 'Usage: bash sdk-scorecard/run.sh [--out DIR] [EMULATOR...]  (gcs-mimic fake-gcs-server gcp-storage-emulator)'; }
root=$(cd "$(dirname "$0")/.." && pwd)
out="$root/sdk-scorecard/results/$(date -u +%F)"
emulators=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --help) usage; exit 0 ;;
    --out) out=$2; shift 2 ;;
    gcs-mimic|fake-gcs-server|gcp-storage-emulator) emulators+=("$1"); shift ;;
    *) usage >&2; exit 2 ;;
  esac
done
[[ ${#emulators[@]} -gt 0 ]] || emulators=(gcs-mimic fake-gcs-server gcp-storage-emulator)

# Comparison inputs are pinned so a rerun measures the same products.
fake_image=fsouza/fake-gcs-server:1.56.1
gcp_image=oittaa/gcp-storage-emulator:v2026.07.19

work=$(mktemp -d "${TMPDIR:-/tmp}/sdk-scorecard.XXXXXX")
server_pid=''
container=''
cleanup() {
  [[ -n $server_pid ]] && kill "$server_pid" 2>/dev/null || true
  [[ -n $container ]] && docker rm -f "$container" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT
mkdir -p "$out"

(cd "$root/sdk-scorecard/go" && GOTOOLCHAIN=auto go build -o "$work/probe-go" .)
(cd "$root/sdk-scorecard/ruby" && BUNDLE_FROZEN=true bundle install --quiet)
port=$(cd "$root/sdk-tests/go" && GOTOOLCHAIN=auto go run ./cmd/free-port | cut -d: -f2)
grpc_port=$(cd "$root/sdk-tests/go" && GOTOOLCHAIN=auto go run ./cmd/free-port | cut -d: -f2)
endpoint="http://127.0.0.1:$port"

wait_ready() {
  for _ in $(seq 1 120); do
    if curl -fsS -o /dev/null "$endpoint/storage/v1/b?project=local-project" 2>/dev/null; then return 0; fi
    sleep 0.5
  done
  echo "emulator at $endpoint did not become ready" >&2
  return 1
}

start_emulator() {
  case "$1" in
    gcs-mimic)
      (cd "$root" && env -u RUSTUP_TOOLCHAIN cargo build --locked --release --quiet)
      "$root/target/release/gcs-mimic" serve --scheme http --listen "127.0.0.1:$port" \
        --grpc-listen "127.0.0.1:$grpc_port" --api-base-url "$endpoint" --data-dir "$work/gcs-mimic-data" \
        >"$work/gcs-mimic.log" 2>&1 &
      server_pid=$!
      grpc_endpoint="127.0.0.1:$grpc_port"
      version="$("$root/target/release/gcs-mimic" --version) ($(git -C "$root" rev-parse --short HEAD))"
      ;;
    fake-gcs-server)
      # The memory backend is the only one with retained object versions.
      container=$(docker run -d -p "127.0.0.1:$port:4443" "$fake_image" -scheme http -port 4443 \
        -backend memory -external-url "$endpoint" -public-host "127.0.0.1:$port")
      # gRPC shares the HTTP listener.
      grpc_endpoint="127.0.0.1:$port"
      version=$fake_image
      ;;
    gcp-storage-emulator)
      container=$(docker run -d -p "127.0.0.1:$port:9023" "$gcp_image" start --host 0.0.0.0 --port 9023 --in-memory)
      # No gRPC listener; the gRPC cases fail against a closed port.
      grpc_endpoint="127.0.0.1:$grpc_port"
      version=$gcp_image
      ;;
  esac
  wait_ready
}

stop_emulator() {
  if [[ -n $server_pid ]]; then kill "$server_pid" 2>/dev/null || true; wait "$server_pid" 2>/dev/null || true; server_pid=''; fi
  if [[ -n $container ]]; then docker rm -f "$container" >/dev/null; container=''; fi
}

for emulator in "${emulators[@]}"; do
  echo "== $emulator" >&2
  start_emulator "$emulator"
  jq -n --arg emulator "$emulator" --arg version "$version" --arg date "$(date -u +%FT%TZ)" \
    '{emulator: $emulator, version: $version, measured_at: $date}' >"$out/$emulator.meta.json"
  status=0
  SCORECARD_ENDPOINT=$endpoint SCORECARD_GRPC_ENDPOINT=$grpc_endpoint \
    SCORECARD_SIGNING_KEY="$root/fixtures/signing/test-only-private.pem" \
    "$work/probe-go" >"$out/$emulator.go.jsonl" || status=$?
  [[ $status == 0 ]] || echo "Go probe exited with $status for $emulator" >&2
  status=0
  (cd "$root/sdk-scorecard/ruby" && SCORECARD_ENDPOINT=$endpoint \
    SCORECARD_SIGNING_KEY="$root/fixtures/signing/test-only-private.pem" \
    bundle exec ruby probe.rb >"$out/$emulator.ruby.jsonl") || status=$?
  [[ $status == 0 ]] || echo "Ruby probe exited with $status for $emulator" >&2
  stop_emulator
done

bash "$root/sdk-scorecard/score.sh" "$out" | tee "$out/summary.md"
