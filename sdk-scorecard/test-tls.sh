#!/usr/bin/env bash
# Exercise the probes' actual HTTP clients without starting an emulator or scoring cases.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
bash "$root/scripts/tls-fixtures.sh" "$work/tls"
port=$(cd "$root/sdk-tests/go" && GOTOOLCHAIN=auto go run ./cmd/free-port | cut -d: -f2)
export SCORECARD_ENDPOINT="https://127.0.0.1:$port"
export SCORECARD_SIGNING_KEY="$root/fixtures/signing/test-only-private.pem"
php_image=composer:2.11.2@sha256:5bcb28b8d0edc4091125f8218f9d088f998bdfc843f8fbb9a4beb4dcd8625684
# Keep this pin identical to sdk-scorecard/run.sh. The Rust SDK cannot trust the test CA on
# macOS (it uses the Keychain and ignores SSL_CERT_FILE), so its check runs in this Linux image.
rust_image=rust:1.99.1-slim-trixie@sha256:f6c4a35d21dbb3c89d7a8d056c193e94fc09037f2b58dd858b09c39a47dd02d7
rust_volume="scorecard-rust-tls-target-$$"
server_pid=''
cleanup() {
  if [[ -n $server_pid ]]; then kill "$server_pid" 2>/dev/null || true; wait "$server_pid" 2>/dev/null || true; fi
  docker volume rm -f "$rust_volume" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

# The Rust check has no host-native form, so Docker is required; failing here is deliberate,
# because skipping would let the Rust client go unchecked while the run reports success.
command -v docker >/dev/null || { echo 'The Rust transport check requires Docker' >&2; exit 2; }
bash "$root/sdk-scorecard/java/run.sh" build
bash "$root/sdk-scorecard/dotnet/run.sh" build
docker run --rm -v "$root:/app:ro" -v "$rust_volume:/target" -e CARGO_TARGET_DIR=/target -w /app/sdk-scorecard/rust \
  "$rust_image" cargo build --locked --release >&2

probe() {
  local sdk=$1 ca=$2
  case "$sdk" in
    node) NODE_EXTRA_CA_CERTS="$ca" node "$root/sdk-scorecard/node/probe.mjs" --transport-check ;;
    python) SCORECARD_CA_CERT="$ca" "$root/sdk-scorecard/python/.venv/bin/python" "$root/sdk-scorecard/python/probe.py" --transport-check ;;
    ruby) (cd "$root/sdk-scorecard/ruby" && SCORECARD_CA_CERT="$ca" bundle exec ruby probe.rb --transport-check) ;;
    php) docker run --rm --add-host host.docker.internal:host-gateway -v "$root:/app:ro" -v "$ca:/scorecard-ca.pem:ro" \
      -e SCORECARD_ENDPOINT -e SCORECARD_DOCKER_GATEWAY=1 -e SCORECARD_SIGNING_KEY=/app/fixtures/signing/test-only-private.pem \
      -e SCORECARD_CA_CERT=/scorecard-ca.pem \
      "$php_image" php /app/sdk-scorecard/php/probe.php --transport-check ;;
    java) SCORECARD_CA_CERT="$ca" bash "$root/sdk-scorecard/java/run.sh" run --transport-check ;;
    dotnet) SCORECARD_CA_CERT="$ca" bash "$root/sdk-scorecard/dotnet/run.sh" --no-build --transport-check ;;
    rust)
      # socat forwards the container's loopback port to the host so the probe keeps its loopback URL.
      docker run --rm --add-host host.docker.internal:host-gateway -v "$root:/app:ro" -v "$rust_volume:/target:ro" \
        -v "$ca:/scorecard-ca.pem:ro" -e SCORECARD_ENDPOINT -e SCORECARD_CA_CERT=/scorecard-ca.pem \
        -e SCORECARD_SIGNING_KEY=/app/fixtures/signing/test-only-private.pem -e HTTP_PORT="$port" \
        "$rust_image" bash -c '
          set -euo pipefail
          /app/sdk-scorecard/install-socat.sh
          socat "TCP-LISTEN:$HTTP_PORT,bind=127.0.0.1,fork,reuseaddr" "TCP:host.docker.internal:$HTTP_PORT" &
          until (echo >/dev/tcp/127.0.0.1/"$HTTP_PORT") 2>/dev/null; do sleep 0.1; done
          /target/release/gcs-mimic-scorecard-rust --transport-check' ;;
  esac
}

failed=0
passed=0
for certificate in server.pem wrong-host.pem; do
  openssl s_server -accept "127.0.0.1:$port" -cert "$work/tls/$certificate" -key "$work/tls/server.key" \
    -www -quiet >"$work/server.log" 2>&1 &
  server_pid=$!
  ready=false
  for _ in $(seq 1 50); do
    # Readiness uses a TCP check; the probes themselves must verify CA and SAN.
    if (echo >/dev/tcp/127.0.0.1/"$port") 2>/dev/null; then ready=true; break; fi
    sleep 0.1
  done
  [[ $ready == true ]] || { sed -n '1,160p' "$work/server.log" >&2; exit 1; }
  for ca in ca.pem other.pem; do
    for sdk in node python ruby php java dotnet rust; do
      status=0
      probe "$sdk" "$work/tls/$ca" >"$work/client.log" 2>&1 || status=$?
      if [[ $certificate == server.pem && $ca == ca.pem ]]; then
        if [[ $status != 0 ]]; then sed -n '1,200p' "$work/client.log" >&2; failed=1;
        else echo "PASS: $sdk trusts the fixture CA and rejects external origins"; passed=$((passed + 1)); fi
      elif [[ $status == 0 ]] || ! rg -qi 'certificate|cert_verify|SSL' "$work/client.log"; then
        echo "FAIL: $sdk did not reject $certificate with $ca as a certificate error" >&2
        sed -n '1,200p' "$work/client.log" >&2
        failed=1
      else
        echo "PASS: $sdk rejects $certificate with $ca"
        passed=$((passed + 1))
      fi
    done
  done
  kill "$server_pid"
  wait "$server_pid" 2>/dev/null || true
  server_pid=''
done
if [[ $failed != 0 || $passed != 28 ]]; then
  echo "FAIL: scorecard transport checks ($passed/28 passed)" >&2
  exit 1
fi
echo 'PASS: scorecard client CA/SAN and origin transport checks (28 cases)'
