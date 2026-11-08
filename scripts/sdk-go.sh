#!/usr/bin/env bash
set -euo pipefail

usage='Usage: bash scripts/sdk-go.sh [--with-ruby] [--with-node] [--with-python] [--with-php] [--with-java] [--with-dotnet] [--with-rust] [--with-clients]'
with_ruby=false
with_clients=false
# Each listed suite provides sdk-tests/<sdk>/run.sh, which reads GCS_MIMIC_ENDPOINT,
# GCS_MIMIC_STRICT_ENDPOINT and, for HTTPS, GCS_MIMIC_CA_CERT.
suites=()
add_suite() {
  if [[ " ${suites[*]-} " != *" $1 "* ]]; then suites+=("$1"); fi
}
while [[ $# -gt 0 ]]; do
  case "$1" in
    --help) echo "$usage"; exit 0 ;;
    --with-ruby) with_ruby=true ;;
    --with-node) add_suite node ;;
    --with-python) add_suite python ;;
    --with-php) add_suite php ;;
    --with-java) add_suite java ;;
    --with-dotnet) add_suite dotnet ;;
    --with-rust) add_suite rust ;;
    --with-clients) with_clients=true; add_suite java; add_suite dotnet; add_suite rust ;;
    *) echo "$usage" >&2; exit 2 ;;
  esac
  shift
done

cd "$(dirname "$0")/.."
root=$PWD
scheme=${GCS_MIMIC_SDK_SCHEME:-http}
if [[ $with_clients == true && $scheme != http ]]; then
  echo 'Additional client harnesses currently require the HTTP profile' >&2
  exit 2
fi
# The Java, .NET and Rust suites guard a plain loopback HTTP origin only. Under the HTTPS
# profile they are announced as skipped rather than failing the other SDKs' TLS run.
http_only_suites=" java dotnet rust "
env -u RUSTUP_TOOLCHAIN cargo build --locked
server="$(env -u RUSTUP_TOOLCHAIN cargo metadata --locked --offline --no-deps --format-version 1 | jq -er .target_directory)/debug/gcs-mimic"
export GCS_MIMIC_BINARY="$server"
work=$(mktemp -d "${TMPDIR:-/tmp}/gcs-mimic-sdk-go.XXXXXX")
pid=
strict_pid=
local_strict_pid=
grpc_endpoint=
strict_grpc_endpoint=
tls_args=()
health_args=()
stop_server() {
  if kill -0 "$1" 2>/dev/null && ! kill -TERM "$1"; then
    echo 'Server exited while requesting shutdown; collecting its exit status' >&2
  fi
  wait "$1"
}
cleanup() {
  status=$?
  trap - EXIT
  for server_pid in "$pid" "$strict_pid" "$local_strict_pid"; do
    if [[ -n $server_pid ]]; then
      if stop_server "$server_pid"; then :; else
        server_status=$?
        if [[ $status -eq 0 ]]; then status=$server_status; fi
      fi
    fi
  done
  if [[ $status -ne 0 ]]; then
    for log in "$work/server.log" "$work/strict-server.log" "$work/local-strict-server.log"; do
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

case "$scheme" in
  http) ;;
  https)
    if [[ -n ${GCS_MIMIC_TLS_CERT:-} || -n ${GCS_MIMIC_TLS_KEY:-} ]]; then
      if [[ -z ${GCS_MIMIC_TLS_CERT:-} || -z ${GCS_MIMIC_TLS_KEY:-} || -z ${GCS_MIMIC_CA_CERT:-} ]]; then
        echo 'Custom HTTPS SDK tests require GCS_MIMIC_TLS_CERT, GCS_MIMIC_TLS_KEY and GCS_MIMIC_CA_CERT' >&2
        exit 2
      fi
      tls_args=(--tls-cert "$GCS_MIMIC_TLS_CERT" --tls-key "$GCS_MIMIC_TLS_KEY")
    elif [[ -z ${GCS_MIMIC_CA_CERT:-} ]]; then
      "$server" tls ca > "$work/ca.pem"
      export GCS_MIMIC_CA_CERT="$work/ca.pem"
    fi
    health_args=(--ca-cert "$GCS_MIMIC_CA_CERT")
    ;;
  *) echo 'GCS_MIMIC_SDK_SCHEME must be http or https' >&2; exit 1 ;;
esac

# Each server starts without any variable it would read as a setting; flags configure it.
source scripts/server-env.sh

# Waits until the server's ready event names its listen address and its readiness probe answers.
# Arguments: PID, log name, listen address, origin, required ready-event auth_mode (empty for any).
wait_for_ready() {
  local server_pid=$1 name=$2 address=$3 origin=$4 auth_mode=$5 actual attempt
  for ((attempt = 0; attempt < 100; attempt++)); do
    if ! kill -0 "$server_pid" 2>/dev/null; then echo "Server exited before readiness: $name" >&2; exit 1; fi
    # A server whose ready event lacks the requested auth_mode must not pass as a strict server.
    actual=$(jq -Rr --arg mode "$auth_mode" \
      'fromjson? | select(.event == "ready" and ($mode == "" or .auth_mode == $mode)) | .listen // empty' \
      "$work/$name")
    if [[ $actual == "$address" ]]; then
      if "$server" healthcheck --url "$origin/_internal/ready" ${health_args[@]+"${health_args[@]}"} >/dev/null 2>&1; then return 0; fi
    fi
    sleep 0.1
  done
  echo "Server readiness timed out: $name" >&2
  exit 1
}

# Starts one isolated server, storing its PID and origin in the named variables. The PID is stored
# before the readiness wait so the cleanup trap always stops a server that fails to become ready.
# A port is selected only after the previous server listens, so both cannot receive the same port.
# Arguments: PID variable, endpoint variable, log name, required ready-event auth_mode (empty for
# any), extra serve arguments.
start_server() {
  # Local names must differ from the caller's variable names, or printf -v assigns the local.
  local pid_var=$1 endpoint_var=$2 name=$3 auth_mode=$4 address grpc_address origin server_pid
  shift 4
  # Select the advertised origin before startup; bind failure remains fatal if the port is reused.
  address=$(cd sdk-tests/go && GOTOOLCHAIN=auto go run ./cmd/free-port)
  if [[ ! $address =~ ^127\.0\.0\.1:[0-9]+$ ]]; then echo 'Invalid loopback address' >&2; exit 1; fi
  origin="$scheme://$address"
  grpc_address=$(cd sdk-tests/go && GOTOOLCHAIN=auto go run ./cmd/free-port)
  if [[ ! $grpc_address =~ ^127\.0\.0\.1:[0-9]+$ || $grpc_address == "$address" ]]; then echo 'Invalid or duplicate gRPC loopback address; rerun to select distinct free ports' >&2; exit 1; fi
  set_server_env
  "${server_env[@]}" \
    "$server" serve --scheme "$scheme" --listen "$address" --grpc-listen "$grpc_address" --api-base-url "$origin" ${tls_args[@]+"${tls_args[@]}"} "$@" >"$work/$name" 2>&1 &
  server_pid=$!
  printf -v "$pid_var" '%s' "$server_pid"
  printf -v "$endpoint_var" '%s' "$origin"
  if [[ $auth_mode == strict ]]; then strict_grpc_endpoint=$grpc_address; else grpc_endpoint=$grpc_address; fi
  wait_for_ready "$server_pid" "$name" "$address" "$origin" "$auth_mode"
}

local_strict_endpoint=
trusted_endpoint=
# Starts the Go suite's local-development server the way the strict local recipe does: strict mode
# with the built-in test signer and a trusted listener, and no identity document, trusted principal
# or host port setting. The built-in trusted-app@gcs-mimic.invalid then acts for requests without
# credentials on the trusted listener with roles/storage.admin, and the test signer holds
# roles/storage.objectUser, both on the default seed project local-project, which the Go suite
# uses; and signed_host_port defaults to lenient. Unlike start_server it fixes no API base URL,
# because links must follow each request's Host for a resumable session started on the trusted
# listener to continue there, and it opens no gRPC listener. The trusted listener binds port 0, so
# only its ready event reports the address.
# Arguments: extra serve arguments.
start_local_strict_server() {
  local address origin trusted
  address=$(cd sdk-tests/go && GOTOOLCHAIN=auto go run ./cmd/free-port)
  if [[ ! $address =~ ^127\.0\.0\.1:[0-9]+$ ]]; then echo 'Invalid loopback address' >&2; exit 1; fi
  origin="$scheme://$address"
  set_server_env
  "${server_env[@]}" \
    "$server" serve --scheme "$scheme" --listen "$address" ${tls_args[@]+"${tls_args[@]}"} \
    --auth-mode strict --test-signer enabled --trusted-listen 127.0.0.1:0 "$@" \
    >"$work/local-strict-server.log" 2>&1 &
  local_strict_pid=$!
  local_strict_endpoint=$origin
  wait_for_ready "$local_strict_pid" local-strict-server.log "$address" "$origin" strict
  # A server that ignored a setting or that did not apply the recipe's defaults must not pass as
  # the local-development server.
  trusted=$(jq -Rr \
    'fromjson? | select(.event == "ready" and .identities == 0 and .trusted_principal == "trusted-app@gcs-mimic.invalid" and .test_signer == "enabled" and .signed_host_port == "lenient") | .trusted_listen // empty' \
    "$work/local-strict-server.log")
  if [[ ! $trusted =~ ^127\.0\.0\.1:[0-9]+$ ]]; then echo 'Ready event lacks the trusted listener, the built-in trusted principal, the test signer or the lenient host port, or names identities' >&2; exit 1; fi
  # The trusted listener speaks plain HTTP whatever the scheme.
  trusted_endpoint="http://$trusted"
  if ! "$server" healthcheck --url "$trusted_endpoint/_internal/ready" >/dev/null 2>&1; then
    echo 'Trusted listener readiness probe failed' >&2
    exit 1
  fi
}

endpoint=
strict_endpoint=
jq -n '{version: 1, project: "local-project", buckets: [{name: "assets"}, {name: "exports"}]}' > "$work/buckets.json"
start_server pid endpoint server.log '' --data-dir "$work/data" --buckets-file "$work/buckets.json"
# The identity file holds local-only fixture principals; its tokens are not Google credentials.
jq --rawfile key fixtures/signing/test-only-public.pem \
  '.identities |= map(if .id == "local-admin@example.invalid" then . + {public_keys: [$key]} else . end)' \
  config/identities.json > "$work/identities.json"
start_server strict_pid strict_endpoint strict-server.log strict --data-dir "$work/strict-data" \
  --auth-mode strict --identity-file "$work/identities.json" \
  --xml-virtual-host-domains storage.example.invalid
start_local_strict_server --data-dir "$work/local-strict-data"

cd sdk-tests/go
export GCS_MIMIC_ENDPOINT="$endpoint" GCS_MIMIC_STRICT_ENDPOINT="$strict_endpoint"
export STORAGE_EMULATOR_HOST_GRPC="$grpc_endpoint" GCS_MIMIC_STRICT_GRPC_ENDPOINT="$strict_grpc_endpoint"
GOTOOLCHAIN=auto go vet ./...
# Only the Go suite uses the local-development server, so the other suites keep their environment.
GCS_MIMIC_LOCAL_STRICT_ENDPOINT="$local_strict_endpoint" GCS_MIMIC_TRUSTED_ENDPOINT="$trusted_endpoint" \
  GOTOOLCHAIN=auto go test -count=1 -v -run "${GCS_MIMIC_GO_TEST_RUN:-.}" ./...

if [[ $with_clients == true && -z ${GCS_MIMIC_GO_TEST_RUN:-} ]]; then
  (cd "$root/go" && GOTOOLCHAIN=auto go test -race -tags integration ./...)
  (cd "$root/go/sourceexport" && GOTOOLCHAIN=auto go test -race ./...)
fi

if [[ $with_ruby == true && -z ${GCS_MIMIC_GO_TEST_RUN:-} ]]; then
  cd ../ruby
  BUNDLE_FROZEN=true bundle check
  bundle exec ruby https.rb --self-check
  env -u GCS_MIMIC_TOKEN GCS_MIMIC_ENDPOINT="$endpoint" bundle exec ruby https.rb --migration-scenario
  env -u GCS_MIMIC_TOKEN GCS_MIMIC_ENDPOINT="$endpoint" bundle exec ruby https.rb
  GCS_MIMIC_ENDPOINT="$strict_endpoint" GCS_MIMIC_TOKEN=local-admin-token GCS_MIMIC_SIGNED_TESTS=1 bundle exec ruby https.rb
fi

if [[ -z ${GCS_MIMIC_GO_TEST_RUN:-} ]]; then
  for sdk in ${suites[@]+"${suites[@]}"}; do
    if [[ $scheme != http && $http_only_suites == *" $sdk "* ]]; then
      echo "== $sdk SDK suite SKIPPED: it supports only the HTTP profile (GCS_MIMIC_SDK_SCHEME=$scheme)" >&2
      continue
    fi
    echo "== $sdk SDK suite" >&2
    (
      if [[ $sdk == rust ]]; then export GCS_MIMIC_GRPC_ENDPOINT="$grpc_endpoint"; fi
      GCS_MIMIC_ENDPOINT="$endpoint" GCS_MIMIC_STRICT_ENDPOINT="$strict_endpoint" \
        bash "$root/sdk-tests/$sdk/run.sh"
    )
  done
fi
