#!/usr/bin/env bash
# Runs nine official SDK scorecard probes against each emulator with the same
# inputs and records raw JSONL results, product versions and a scored summary.
# Every probe gets a freshly started emulator, so state one SDK leaves behind
# (for example consumed resumable sessions) cannot change another SDK's result.
set -euo pipefail

usage() { echo 'Usage: bash sdk-scorecard/run.sh [--out DIR] [EMULATOR...]  (gcs-mimic fake-gcs-server gcp-storage-emulator floci-gcp)'; }
root=$(cd "$(dirname "$0")/.." && pwd)
out="$root/sdk-scorecard/results/$(date -u +%F)"
explicit_out=false
emulators=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --help) usage; exit 0 ;;
    --out) out=${2:?--out requires a directory}; explicit_out=true; shift 2 ;;
    gcs-mimic|fake-gcs-server|gcp-storage-emulator|floci-gcp) emulators+=("$1"); shift ;;
    *) usage >&2; exit 2 ;;
  esac
done
[[ ${#emulators[@]} -gt 0 ]] || emulators=(gcs-mimic fake-gcs-server gcp-storage-emulator floci-gcp)
[[ -z ${SCORECARD_RUN:-} ]] || { echo 'Use SCORECARD_RUN with an individual probe; scored runs require every case.' >&2; exit 2; }
if [[ $explicit_out == false ]]; then
  base=$out
  suffix=2
  while [[ -e $out ]]; do out="$base-$suffix"; suffix=$((suffix + 1)); done
fi
[[ ! -e $out || -z $(ls -A "$out") ]] || { echo "results directory is not empty: $out" >&2; exit 2; }

# Record the input tree before output files and SDK setup can change it.
source_dirty=$(git -C "$root" status --porcelain --untracked-files=normal)
# The catalog defines every case; a broken one would only show after the whole run, so check it first.
catalog="$root/sdk-scorecard/cases.json"
bash "$root/sdk-scorecard/score.sh" --check-catalog "$catalog"
[[ $(jq -c '.sdks' "$catalog") == '["go","ruby","node","php","python","java","dotnet","rust","cpp"]' ]] || {
  echo 'sdk-scorecard/cases.json must list the SDKs this script runs, in its order' >&2; exit 1;
}
# Each probe emits one line per catalog case for its SDK, unsupported ones included.
case_counts=$(jq -c '. as $c | [$c.sdks[] as $s |
  {key: $s, value: ([$c.cases[] | select(.sdks == "all" or (.sdks | index($s)))] | length)}] | from_entries' "$catalog")
go_sdk=$(awk '$1 == "cloud.google.com/go/storage" {print $2}' "$root/sdk-scorecard/go/go.mod")
[[ $go_sdk =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo 'expected exactly one stable cloud.google.com/go/storage version in sdk-scorecard/go/go.mod' >&2; exit 1;
}
ruby_sdk=$(sed -n 's/^    google-cloud-storage (\(.*\))$/\1/p' "$root/sdk-scorecard/ruby/Gemfile.lock")
[[ $ruby_sdk =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo 'expected exactly one stable google-cloud-storage version in sdk-scorecard/ruby/Gemfile.lock' >&2; exit 1;
}
# The C++ SDK and its compiler image are pinned only in the probe's Dockerfile.
cpp_dockerfile="$root/sdk-scorecard/cpp/Dockerfile"
cpp_sdk=$(sed -n 's#^ADD --checksum=sha256:[0-9a-f]\{64\} https://github\.com/googleapis/google-cloud-cpp/archive/refs/tags/v\([0-9.]*\)\.tar\.gz .*#\1#p' "$cpp_dockerfile")
[[ $cpp_sdk =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo 'expected exactly one checksum-pinned google-cloud-cpp release in sdk-scorecard/cpp/Dockerfile' >&2; exit 1;
}
cpp_base=$(sed -n 's/^FROM \(gcc:[^ ]*@sha256:[0-9a-f]\{64\}\)$/\1/p' "$cpp_dockerfile")
[[ -n $cpp_base && $cpp_base != *$'\n'* ]] || { echo 'expected one digest-pinned gcc image in sdk-scorecard/cpp/Dockerfile' >&2; exit 1; }

# Comparison inputs are pinned so a rerun measures the same products.
fake_image=fsouza/fake-gcs-server:1.56.1
gcp_image=oittaa/gcp-storage-emulator:v2026.07.19
floci_image=floci/floci-gcp:0.13.0
php_image=composer:2.11.2@sha256:cd3a90dc5624c59a2f9ddf80dad08569f06416db31de519024f249d63bca554b
# The Rust SDK trusts only the operating system's roots. On macOS that is the Keychain, which
# ignores SSL_CERT_FILE, so the probe cannot trust the scorecard CA there; it runs in this
# Linux image on every host instead. The pin matches the Dockerfile's build image.
rust_image=rust:1.100.0-slim-trixie@sha256:86993c97389ebb5487eb09e88168e2a00b0de29aa08dff9ae896049b9be96ed9

work=$(mktemp -d "${TMPDIR:-/tmp}/sdk-scorecard.XXXXXX")
server_pid=''
frontend_pid=''
container=''
probe_container="scorecard-php-$$"
rust_container="scorecard-rust-$$"
rust_volume="scorecard-rust-target-$$"
cpp_container="scorecard-cpp-$$"
# A per-run tag keeps concurrent runs apart; Docker's build cache keeps the library layers.
cpp_image="gcs-mimic-scorecard-cpp:run-$$"
cleanup() {
  [[ -n $server_pid ]] && kill "$server_pid" 2>/dev/null || true
  [[ -n $frontend_pid ]] && kill "$frontend_pid" 2>/dev/null || true
  [[ -n $container ]] && docker rm -f "$container" >/dev/null 2>&1 || true
  docker rm -f "$probe_container" "$rust_container" "$cpp_container" >/dev/null 2>&1 || true
  docker volume rm -f "$rust_volume" >/dev/null 2>&1 || true
  docker image rm "$cpp_image" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT
mkdir -p "$out"
# The Ruby probe runs from its own directory, so a relative --out must be resolved first.
out=$(cd "$out" && pwd)
# score.sh checks every recorded case against the catalog the run used.
cp "$catalog" "$out/cases.json"

(cd "$root/sdk-scorecard/go" && GOTOOLCHAIN=auto go build -o "$work/probe-go" .)
(cd "$root/sdk-scorecard/go" && GOTOOLCHAIN=auto go build -o "$work/tls-front" ./cmd/tls-front)
bash "$root/scripts/tls-fixtures.sh" "$work/tls"
ca="$work/tls/ca.pem"
(cd "$root/sdk-scorecard/ruby" && BUNDLE_FROZEN=true bundle install --quiet)
(cd "$root/sdk-scorecard/node" && npm ci --ignore-scripts --no-audit --no-fund)
(cd "$root/sdk-scorecard/python" && uv sync --locked)
docker run --rm --name "$probe_container" -v "$root/sdk-scorecard/php:/app" "$php_image" \
  install --no-interaction --no-scripts --prefer-dist
bash "$root/sdk-scorecard/java/run.sh" build
bash "$root/sdk-scorecard/dotnet/run.sh" build
# Build the Rust probe once in the Linux image; a volume keeps the binary for the per-emulator runs.
docker run --rm -v "$root:/app:ro" -v "$rust_volume:/target" -e CARGO_TARGET_DIR=/target -w /app/sdk-scorecard/rust \
  "$rust_image" cargo build --locked --release >&2
# The C++ probe is built and run in Linux for the same reason as the Rust one, and its image
# also carries the pinned socat. The context is the scorecard directory (see Dockerfile.dockerignore).
docker build -f "$cpp_dockerfile" -t "$cpp_image" "$root/sdk-scorecard" >&2
node --check "$root/sdk-scorecard/node/probe.mjs"
"$root/sdk-scorecard/python/.venv/bin/python" -m py_compile "$root/sdk-scorecard/python/probe.py"
docker run --rm --name "$probe_container" -v "$root:/app:ro" "$php_image" php -l /app/sdk-scorecard/php/probe.php
docker run --rm --name "$probe_container" -v "$root:/app:ro" "$php_image" php /app/sdk-scorecard/php/test-resumable-wire.php
# "cases" is each probe's output line count, taken from the catalog.
sdks=$(jq -n --argjson counts "$case_counts" --arg go "$(cd "$root/sdk-scorecard/go" && go version)" \
  --arg ruby "$(cd "$root/sdk-scorecard/ruby" && ruby --version)" --arg node "$(node --version)" \
  --arg python "$("$root/sdk-scorecard/python/.venv/bin/python" --version)" \
  --arg php "$(docker run --rm "$php_image" php --version | sed -n '1p')" \
  --arg go_sdk "${go_sdk#v}" --arg ruby_sdk "$ruby_sdk" \
  --arg node_sdk "$(jq -er '.packages["node_modules/@google-cloud/storage"].version' "$root/sdk-scorecard/node/package-lock.json")" \
  --arg php_sdk "$(jq -er '.packages[] | select(.name == "google/cloud-storage") | .version | ltrimstr("v")' "$root/sdk-scorecard/php/composer.lock")" \
  --arg python_sdk "$(uv pip show --python "$root/sdk-scorecard/python/.venv/bin/python" google-cloud-storage | sed -n 's/^Version: //p')" \
  --arg php_image "$php_image" \
  --arg java "$("${JAVA_HOME:+$JAVA_HOME/bin/}java" -version 2>&1 | sed -n '1p')" \
  --arg java_sdk "$(sed -n 's/^.*com\.google\.cloud:google-cloud-storage:jar:\([^:]*\):compile$/\1/p' "$root/sdk-scorecard/java/dependencies.lock")" \
  --arg dotnet "dotnet SDK $(cd "$root/sdk-scorecard/dotnet" && dotnet --version)" \
  --arg dotnet_sdk "$(jq -er '.dependencies["net11.0"]["Google.Cloud.Storage.V1"].resolved' "$root/sdk-scorecard/dotnet/packages.lock.json")" \
  --arg rust "$(docker run --rm "$rust_image" rustc --version)" \
  --arg rust_sdk "$(awk '$0 == "name = \"google-cloud-storage\"" {getline; gsub(/"/, "", $3); print $3}' "$root/sdk-scorecard/rust/Cargo.lock")" \
  --arg rust_image "$rust_image" \
  --arg cpp "$(docker run --rm "$cpp_image" g++ --version | sed -n '1p')" --arg cpp_sdk "$cpp_sdk" --arg cpp_base "$cpp_base" \
  '{go: {version: $go_sdk, runtime: $go, cases: $counts.go}, ruby: {version: $ruby_sdk, runtime: $ruby, cases: $counts.ruby},
    node: {version: $node_sdk, runtime: $node, cases: $counts.node},
    php: {version: $php_sdk, runtime: $php, image: $php_image, cases: $counts.php},
    python: {version: $python_sdk, runtime: $python, cases: $counts.python},
    java: {version: $java_sdk, runtime: $java, cases: $counts.java}, dotnet: {version: $dotnet_sdk, runtime: $dotnet, cases: $counts.dotnet},
    rust: {version: $rust_sdk, runtime: $rust, image: $rust_image, cases: $counts.rust},
    cpp: {version: $cpp_sdk, runtime: $cpp, image: $cpp_base, cases: $counts.cpp}}')
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
      # Each start gets an empty catalog; the binary is built once before the loop.
      rm -rf "$work/gcs-mimic-data"
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
    floci-gcp)
      # The default memory storage starts empty, and no Docker socket is needed for Cloud Storage.
      container=$(docker run -d -p "127.0.0.1:$backend_port:4588" -e FLOCI_GCP_BASE_URL="$endpoint" "$floci_image")
      # gRPC shares the REST port.
      grpc_endpoint="127.0.0.1:$backend_port"
      version=$floci_image
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

signing_key="$root/fixtures/signing/test-only-private.pem"

# Runs one SDK probe against the running emulator and writes its JSONL to $2.
run_probe() {
  local sdk=$1 file=$2
  case "$sdk" in
    go)
      SCORECARD_ENDPOINT=$endpoint SCORECARD_GRPC_ENDPOINT=$grpc_endpoint SCORECARD_CA_CERT="$ca" \
        SCORECARD_SIGNING_KEY="$signing_key" "$work/probe-go" >"$file" ;;
    ruby)
      (cd "$root/sdk-scorecard/ruby" && SCORECARD_ENDPOINT=$endpoint SCORECARD_CA_CERT="$ca" \
        SCORECARD_SIGNING_KEY="$signing_key" bundle exec ruby probe.rb >"$file") ;;
    node)
      SCORECARD_ENDPOINT=$endpoint NODE_EXTRA_CA_CERTS="$ca" SCORECARD_SIGNING_KEY="$signing_key" \
        node "$root/sdk-scorecard/node/probe.mjs" >"$file" ;;
    python)
      SCORECARD_ENDPOINT=$endpoint SCORECARD_GRPC_ENDPOINT=$grpc_endpoint SCORECARD_CA_CERT="$ca" \
        SCORECARD_SIGNING_KEY="$signing_key" \
        "$root/sdk-scorecard/python/.venv/bin/python" "$root/sdk-scorecard/python/probe.py" >"$file" ;;
    php)
      docker run --rm --name "$probe_container" --add-host host.docker.internal:host-gateway -v "$root:/app:ro" -v "$ca:/scorecard-ca.pem:ro" \
        -e SCORECARD_ENDPOINT="$endpoint" -e SCORECARD_DOCKER_GATEWAY=1 \
        -e SCORECARD_SIGNING_KEY=/app/fixtures/signing/test-only-private.pem \
        -e SCORECARD_CA_CERT=/scorecard-ca.pem \
        "$php_image" php /app/sdk-scorecard/php/probe.php >"$file" ;;
    java)
      SCORECARD_ENDPOINT=$endpoint SCORECARD_GRPC_ENDPOINT=$grpc_endpoint SCORECARD_CA_CERT="$ca" \
        SCORECARD_SIGNING_KEY="$signing_key" bash "$root/sdk-scorecard/java/run.sh" run >"$file" ;;
    dotnet)
      SCORECARD_ENDPOINT=$endpoint SCORECARD_GRPC_ENDPOINT=$grpc_endpoint SCORECARD_CA_CERT="$ca" \
        SCORECARD_SIGNING_KEY="$signing_key" bash "$root/sdk-scorecard/dotnet/run.sh" --no-build >"$file" ;;
    rust)
      # The probe requires loopback URLs, and a container's loopback is not the host's. socat
      # forwards the two loopback ports to the host gateway, so URLs, Host headers and
      # signatures stay unchanged. SSL_CERT_FILE is set by the probe from SCORECARD_CA_CERT.
      # install-socat.sh sends apt output to stderr to keep stdout a pure JSONL stream.
      docker run --rm --name "$rust_container" --add-host host.docker.internal:host-gateway \
        -v "$root:/app:ro" -v "$rust_volume:/target:ro" -v "$ca:/scorecard-ca.pem:ro" \
        -e SCORECARD_ENDPOINT="$endpoint" -e SCORECARD_GRPC_ENDPOINT="$grpc_endpoint" \
        -e SCORECARD_SIGNING_KEY=/app/fixtures/signing/test-only-private.pem -e SCORECARD_CA_CERT=/scorecard-ca.pem \
        -e HTTP_PORT="${endpoint##*:}" -e GRPC_PORT="${grpc_endpoint##*:}" \
        "$rust_image" bash -c '
          set -euo pipefail
          /app/sdk-scorecard/install-socat.sh
          for port in "$HTTP_PORT" "$GRPC_PORT"; do
            socat "TCP-LISTEN:$port,bind=127.0.0.1,fork,reuseaddr" "TCP:host.docker.internal:$port" &
          done
          for port in "$HTTP_PORT" "$GRPC_PORT"; do
            until (echo >/dev/tcp/127.0.0.1/"$port") 2>/dev/null; do sleep 0.1; done
          done
          /target/release/gcs-mimic-scorecard-rust' >"$file" ;;
    cpp)
      # Same loopback forwarding as Rust; socat is already in the image. The probe uses only the
      # HTTP origin in this scope, so the gRPC port is not forwarded.
      docker run --rm --name "$cpp_container" --add-host host.docker.internal:host-gateway \
        -v "$root:/app:ro" -v "$ca:/scorecard-ca.pem:ro" \
        -e SCORECARD_ENDPOINT="$endpoint" -e SCORECARD_CA_CERT=/scorecard-ca.pem \
        -e SCORECARD_SIGNING_KEY=/app/fixtures/signing/test-only-private.pem -e HTTP_PORT="${endpoint##*:}" \
        "$cpp_image" bash -c '
          set -euo pipefail
          socat "TCP-LISTEN:$HTTP_PORT,bind=127.0.0.1,fork,reuseaddr" "TCP:host.docker.internal:$HTTP_PORT" &
          until (echo >/dev/tcp/127.0.0.1/"$HTTP_PORT") 2>/dev/null; do sleep 0.1; done
          gcs-mimic-scorecard-cpp' >"$file" ;;
  esac
}

if [[ " ${emulators[*]} " == *' gcs-mimic '* ]]; then
  (cd "$root" && env -u RUSTUP_TOOLCHAIN cargo build --locked --release --quiet)
fi

failed=0
for emulator in "${emulators[@]}"; do
  echo "== $emulator" >&2
  first_start=true
  # A new emulator per SDK: probes consume resumable sessions, create buckets and change
  # settings, and that leftover state must not affect the next SDK's results.
  for sdk in go ruby node python php java dotnet rust cpp; do
    echo "-- $emulator / $sdk" >&2
    start_emulator "$emulator"
    if [[ $first_start == true ]]; then
      jq -n --arg emulator "$emulator" --arg version "$version" --arg date "$(date -u +%FT%TZ)" \
        --arg ca_sha256 "$(openssl x509 -in "$ca" -outform DER | openssl dgst -sha256 | awk '{print $NF}')" \
        --argjson sdks "$sdks" --arg dirty "$source_dirty" \
        '{emulator: $emulator, version: $version, measured_at: $date, source_dirty: ($dirty != ""), sdks: $sdks,
          transport: {scheme: "https", frontend: "go-stdlib", ca_sha256: $ca_sha256}}' >"$out/$emulator.meta.json"
      first_start=false
    fi
    status=0
    run_probe "$sdk" "$out/$emulator.$sdk.jsonl" || status=$?
    if [[ $status != 0 ]]; then echo "$sdk probe exited with $status for $emulator" >&2; failed=1; fi
    stop_emulator
  done
done

[[ $failed == 0 ]] || { echo 'Probe process failed; raw results retained, no score produced.' >&2; exit 1; }
bash "$root/sdk-scorecard/score.sh" "$out" | tee "$out/summary.md"
