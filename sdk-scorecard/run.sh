#!/usr/bin/env bash
# Runs five official SDK scorecard probes against each emulator with the same
# inputs and records raw JSONL results, product versions and a scored summary.
set -euo pipefail

usage() { echo 'Usage: bash sdk-scorecard/run.sh [--out DIR] [EMULATOR...]  (gcs-mimic fake-gcs-server gcp-storage-emulator)'; }
root=$(cd "$(dirname "$0")/.." && pwd)
out="$root/sdk-scorecard/results/$(date -u +%F)"
explicit_out=false
emulators=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --help) usage; exit 0 ;;
    --out) out=${2:?--out requires a directory}; explicit_out=true; shift 2 ;;
    gcs-mimic|fake-gcs-server|gcp-storage-emulator) emulators+=("$1"); shift ;;
    *) usage >&2; exit 2 ;;
  esac
done
[[ ${#emulators[@]} -gt 0 ]] || emulators=(gcs-mimic fake-gcs-server gcp-storage-emulator)
[[ -z ${SCORECARD_RUN:-} ]] || { echo 'Use SCORECARD_RUN with an individual probe; scored runs require every case.' >&2; exit 2; }
if [[ $explicit_out == false ]]; then
  base=$out
  suffix=2
  while [[ -e $out ]]; do out="$base-$suffix"; suffix=$((suffix + 1)); done
fi
[[ ! -e $out || -z $(ls -A "$out") ]] || { echo "results directory is not empty: $out" >&2; exit 2; }

# Record the input tree before output files and SDK setup can change it.
source_dirty=$(git -C "$root" status --porcelain --untracked-files=normal)
go_sdk=$(awk '$1 == "cloud.google.com/go/storage" {print $2}' "$root/sdk-scorecard/go/go.mod")
[[ $go_sdk =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo 'expected exactly one stable cloud.google.com/go/storage version in sdk-scorecard/go/go.mod' >&2; exit 1;
}
ruby_sdk=$(sed -n 's/^    google-cloud-storage (\(.*\))$/\1/p' "$root/sdk-scorecard/ruby/Gemfile.lock")
[[ $ruby_sdk =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo 'expected exactly one stable google-cloud-storage version in sdk-scorecard/ruby/Gemfile.lock' >&2; exit 1;
}

# Comparison inputs are pinned so a rerun measures the same products.
fake_image=fsouza/fake-gcs-server:1.56.1
gcp_image=oittaa/gcp-storage-emulator:v2026.07.19
php_image=composer:2.11.0@sha256:278dfc7bd35169965d03f87194816f43bf4e6b6ea61707c2114e52ad12fe24ce

work=$(mktemp -d "${TMPDIR:-/tmp}/sdk-scorecard.XXXXXX")
server_pid=''
frontend_pid=''
container=''
probe_container="scorecard-php-$$"
cleanup() {
  [[ -n $server_pid ]] && kill "$server_pid" 2>/dev/null || true
  [[ -n $frontend_pid ]] && kill "$frontend_pid" 2>/dev/null || true
  [[ -n $container ]] && docker rm -f "$container" >/dev/null 2>&1 || true
  docker rm -f "$probe_container" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT
mkdir -p "$out"
# The Ruby probe runs from its own directory, so a relative --out must be resolved first.
out=$(cd "$out" && pwd)

(cd "$root/sdk-scorecard/go" && GOTOOLCHAIN=auto go build -o "$work/probe-go" .)
(cd "$root/sdk-scorecard/go" && GOTOOLCHAIN=auto go build -o "$work/tls-front" ./cmd/tls-front)
bash "$root/scripts/tls-fixtures.sh" "$work/tls"
ca="$work/tls/ca.pem"
(cd "$root/sdk-scorecard/ruby" && BUNDLE_FROZEN=true bundle install --quiet)
(cd "$root/sdk-scorecard/node" && npm ci --ignore-scripts --no-audit --no-fund)
(cd "$root/sdk-scorecard/python" && uv sync --locked)
docker run --rm --name "$probe_container" -v "$root/sdk-scorecard/php:/app" "$php_image" \
  install --no-interaction --no-scripts --prefer-dist
node --check "$root/sdk-scorecard/node/probe.mjs"
"$root/sdk-scorecard/python/.venv/bin/python" -m py_compile "$root/sdk-scorecard/python/probe.py"
docker run --rm --name "$probe_container" -v "$root:/app:ro" "$php_image" php -l /app/sdk-scorecard/php/probe.php
docker run --rm --name "$probe_container" -v "$root:/app:ro" "$php_image" php /app/sdk-scorecard/php/test-resumable-wire.php
sdks=$(jq -n --arg go "$(cd "$root/sdk-scorecard/go" && go version)" \
  --arg ruby "$(cd "$root/sdk-scorecard/ruby" && ruby --version)" --arg node "$(node --version)" \
  --arg python "$("$root/sdk-scorecard/python/.venv/bin/python" --version)" \
  --arg php "$(docker run --rm "$php_image" php --version | sed -n '1p')" \
  --arg go_sdk "${go_sdk#v}" --arg ruby_sdk "$ruby_sdk" \
  --arg node_sdk "$(jq -er '.packages["node_modules/@google-cloud/storage"].version' "$root/sdk-scorecard/node/package-lock.json")" \
  --arg php_sdk "$(jq -er '.packages[] | select(.name == "google/cloud-storage") | .version | ltrimstr("v")' "$root/sdk-scorecard/php/composer.lock")" \
  --arg python_sdk "$(uv pip show --python "$root/sdk-scorecard/python/.venv/bin/python" google-cloud-storage | sed -n 's/^Version: //p')" \
  --arg php_image "$php_image" \
  '{go: {version: $go_sdk, runtime: $go, cases: 57}, ruby: {version: $ruby_sdk, runtime: $ruby, cases: 51},
    node: {version: $node_sdk, runtime: $node, cases: 51}, php: {version: $php_sdk, runtime: $php, image: $php_image, cases: 51},
    python: {version: $python_sdk, runtime: $python, cases: 51}}')
port=$(cd "$root/sdk-tests/go" && GOTOOLCHAIN=auto go run ./cmd/free-port | cut -d: -f2)
backend_port=$(cd "$root/sdk-tests/go" && GOTOOLCHAIN=auto go run ./cmd/free-port | cut -d: -f2)
grpc_port=$(cd "$root/sdk-tests/go" && GOTOOLCHAIN=auto go run ./cmd/free-port | cut -d: -f2)
endpoint="https://127.0.0.1:$port"
backend="http://127.0.0.1:$backend_port"

wait_ready() {
  for _ in $(seq 1 120); do
    if curl --disable --noproxy '*' --cacert "$ca" -fsS -o /dev/null "$endpoint/storage/v1/b?project=local-project" 2>/dev/null; then return 0; fi
    sleep 0.5
  done
  echo "emulator at $endpoint did not become ready" >&2
  return 1
}

start_emulator() {
  case "$1" in
    gcs-mimic)
      (cd "$root" && env -u RUSTUP_TOOLCHAIN cargo build --locked --release --quiet)
      "$root/target/release/gcs-mimic" serve --scheme http --listen "127.0.0.1:$backend_port" \
        --grpc-listen "127.0.0.1:$grpc_port" --api-base-url "$endpoint" --data-dir "$work/gcs-mimic-data" \
        >"$work/gcs-mimic.log" 2>&1 &
      server_pid=$!
      grpc_endpoint="127.0.0.1:$grpc_port"
      version="$("$root/target/release/gcs-mimic" --version) ($(git -C "$root" rev-parse --short HEAD))"
      ;;
    fake-gcs-server)
      # The memory backend is the only one with retained object versions.
      container=$(docker run -d -p "127.0.0.1:$backend_port:4443" "$fake_image" -scheme http -port 4443 \
        -backend memory -external-url "$endpoint" -public-host "127.0.0.1:$port")
      # gRPC shares the HTTP listener.
      grpc_endpoint="127.0.0.1:$backend_port"
      version=$fake_image
      ;;
    gcp-storage-emulator)
      container=$(docker run -d -p "127.0.0.1:$backend_port:9023" "$gcp_image" start --host 0.0.0.0 --port 9023 --in-memory)
      # No gRPC listener; the gRPC cases fail against a closed port.
      grpc_endpoint="127.0.0.1:$grpc_port"
      version=$gcp_image
      ;;
  esac
  "$work/tls-front" --origin "$endpoint" --backend "$backend" \
    --cert "$work/tls/server.pem" --key "$work/tls/server.key" >"$work/tls-front.log" 2>&1 &
  frontend_pid=$!
  wait_ready
}

stop_emulator() {
  if [[ -n $frontend_pid ]]; then kill "$frontend_pid" 2>/dev/null || true; wait "$frontend_pid" 2>/dev/null || true; frontend_pid=''; fi
  if [[ -n $server_pid ]]; then kill "$server_pid" 2>/dev/null || true; wait "$server_pid" 2>/dev/null || true; server_pid=''; fi
  if [[ -n $container ]]; then docker rm -f "$container" >/dev/null; container=''; fi
}

failed=0
for emulator in "${emulators[@]}"; do
  echo "== $emulator" >&2
  start_emulator "$emulator"
  jq -n --arg emulator "$emulator" --arg version "$version" --arg date "$(date -u +%FT%TZ)" \
    --arg ca_sha256 "$(openssl x509 -in "$ca" -outform DER | openssl dgst -sha256 | awk '{print $NF}')" \
    --argjson sdks "$sdks" --arg dirty "$source_dirty" \
    '{emulator: $emulator, version: $version, measured_at: $date, source_dirty: ($dirty != ""), sdks: $sdks,
      transport: {scheme: "https", frontend: "go-stdlib", ca_sha256: $ca_sha256}}' >"$out/$emulator.meta.json"
  status=0
  SCORECARD_ENDPOINT=$endpoint SCORECARD_GRPC_ENDPOINT=$grpc_endpoint SCORECARD_CA_CERT="$ca" \
    SCORECARD_SIGNING_KEY="$root/fixtures/signing/test-only-private.pem" \
    "$work/probe-go" >"$out/$emulator.go.jsonl" || status=$?
  if [[ $status != 0 ]]; then echo "Go probe exited with $status for $emulator" >&2; failed=1; fi
  status=0
  (cd "$root/sdk-scorecard/ruby" && SCORECARD_ENDPOINT=$endpoint SCORECARD_CA_CERT="$ca" \
    SCORECARD_SIGNING_KEY="$root/fixtures/signing/test-only-private.pem" \
    bundle exec ruby probe.rb >"$out/$emulator.ruby.jsonl") || status=$?
  if [[ $status != 0 ]]; then echo "Ruby probe exited with $status for $emulator" >&2; failed=1; fi
  status=0
  SCORECARD_ENDPOINT=$endpoint NODE_EXTRA_CA_CERTS="$ca" SCORECARD_SIGNING_KEY="$root/fixtures/signing/test-only-private.pem" \
    node "$root/sdk-scorecard/node/probe.mjs" >"$out/$emulator.node.jsonl" || status=$?
  if [[ $status != 0 ]]; then echo "Node.js probe exited with $status for $emulator" >&2; failed=1; fi
  status=0
  SCORECARD_ENDPOINT=$endpoint SCORECARD_CA_CERT="$ca" SCORECARD_SIGNING_KEY="$root/fixtures/signing/test-only-private.pem" \
    "$root/sdk-scorecard/python/.venv/bin/python" "$root/sdk-scorecard/python/probe.py" >"$out/$emulator.python.jsonl" || status=$?
  if [[ $status != 0 ]]; then echo "Python probe exited with $status for $emulator" >&2; failed=1; fi
  status=0
  docker run --rm --name "$probe_container" --add-host host.docker.internal:host-gateway -v "$root:/app:ro" -v "$ca:/scorecard-ca.pem:ro" \
    -e SCORECARD_ENDPOINT="$endpoint" -e SCORECARD_DOCKER_GATEWAY=1 \
    -e SCORECARD_SIGNING_KEY=/app/fixtures/signing/test-only-private.pem \
    -e SCORECARD_CA_CERT=/scorecard-ca.pem \
    "$php_image" php /app/sdk-scorecard/php/probe.php >"$out/$emulator.php.jsonl" || status=$?
  if [[ $status != 0 ]]; then echo "PHP probe exited with $status for $emulator" >&2; failed=1; fi
  stop_emulator
done

[[ $failed == 0 ]] || { echo 'Probe process failed; raw results retained, no score produced.' >&2; exit 1; }
bash "$root/sdk-scorecard/score.sh" "$out" | tee "$out/summary.md"
