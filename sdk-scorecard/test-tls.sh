#!/usr/bin/env bash
# Exercise the probes' actual HTTP clients without starting an emulator or scoring cases.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
server_pid=''
cleanup() {
  if [[ -n $server_pid ]]; then kill "$server_pid" 2>/dev/null || true; wait "$server_pid" 2>/dev/null || true; fi
  rm -rf "$work"
}
trap cleanup EXIT
bash "$root/scripts/tls-fixtures.sh" "$work/tls"
port=$(cd "$root/sdk-tests/go" && GOTOOLCHAIN=auto go run ./cmd/free-port | cut -d: -f2)
export SCORECARD_ENDPOINT="https://127.0.0.1:$port"
export SCORECARD_SIGNING_KEY="$root/fixtures/signing/test-only-private.pem"
php_image=composer:2.11.0@sha256:278dfc7bd35169965d03f87194816f43bf4e6b6ea61707c2114e52ad12fe24ce

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
    for sdk in node python ruby php; do
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
if [[ $failed != 0 || $passed != 16 ]]; then
  echo "FAIL: scorecard transport checks ($passed/16 passed)" >&2
  exit 1
fi
echo 'PASS: scorecard client CA/SAN and origin transport checks (16 cases)'
