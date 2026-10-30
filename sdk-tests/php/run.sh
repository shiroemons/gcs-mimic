#!/usr/bin/env bash
# Runs the official PHP SDK suite in the digest-pinned Composer/PHP image against the
# permissive (GCS_MIMIC_ENDPOINT) and strict (GCS_MIMIC_STRICT_ENDPOINT) emulators.
# For HTTPS endpoints, GCS_MIMIC_CA_CERT names the CA that signed the server certificate.
set -euo pipefail

# Keep this pin identical to sdk-scorecard/run.sh; daily detection checks it there.
image=composer:2.11.0@sha256:278dfc7bd35169965d03f87194816f43bf4e6b6ea61707c2114e52ad12fe24ce
root=$(cd "$(dirname "$0")/../.." && pwd)
endpoint=${GCS_MIMIC_ENDPOINT:?GCS_MIMIC_ENDPOINT must name the permissive emulator origin}
strict_endpoint=${GCS_MIMIC_STRICT_ENDPOINT:?GCS_MIMIC_STRICT_ENDPOINT must name the strict emulator origin}
command -v docker >/dev/null || { echo 'The PHP SDK suite requires Docker' >&2; exit 2; }

container="gcs-mimic-sdk-php-$$"
ca_copy="$root/sdk-tests/php/.ca/ca-$$.pem"
cleanup() {
  docker rm -f "$container" >/dev/null 2>&1 || true
  rm -f "$ca_copy"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# The emulators bind host loopback. Native Linux Docker shares that loopback only with
# --network host. Docker Desktop (macOS, Windows) has no such host namespace; its
# host.docker.internal gateway does reach host loopback, so cURL CONNECT_TO dials it
# while request URLs, Host headers and signatures keep the original origin.
network=${GCS_MIMIC_PHP_DOCKER_NETWORK:-}
if [[ -z $network ]]; then
  if [[ $(uname -s) == Linux ]]; then network=host; else network=gateway; fi
fi
case "$network" in
  host) network_args=(--network host); gateway=0 ;;
  gateway) network_args=(--add-host host.docker.internal:host-gateway); gateway=1 ;;
  *) echo 'GCS_MIMIC_PHP_DOCKER_NETWORK must be host or gateway' >&2; exit 2 ;;
esac

ca_args=()
if [[ $endpoint == https://* || $strict_endpoint == https://* ]]; then
  ca=${GCS_MIMIC_CA_CERT:?HTTPS endpoints require GCS_MIMIC_CA_CERT}
  [[ -f $ca && -r $ca ]] || { echo 'GCS_MIMIC_CA_CERT is unreadable' >&2; exit 2; }
  # Docker Desktop cannot bind-mount files from every host volume (the macOS test RAM disk
  # fails with "file exists"), so the CA travels inside the checkout the suite already mounts.
  mkdir -p "${ca_copy%/*}"
  cp "$ca" "$ca_copy"
  ca_args=(-e "GCS_MIMIC_CA_CERT=/work/sdk-tests/php/.ca/${ca_copy##*/}")
fi

# Run as the invoking user so vendor/ stays removable; the image's COMPOSER_HOME is world-writable.
user_args=(--user "$(id -u):$(id -g)")

docker run --rm --name "$container" "${user_args[@]}" -v "$root/sdk-tests/php:/work/sdk-tests/php" \
  -w /work/sdk-tests/php "$image" install --no-interaction --no-scripts --prefer-dist --no-progress

# Arguments before -- are extra docker run options; the rest go to php. The suite mounts the
# checkout read-only: it needs vendor/ and the public signing fixture.
php() {
  local docker_args=()
  while [[ $1 != -- ]]; do docker_args+=("$1"); shift; done
  shift
  docker run --rm --name "$container" "${user_args[@]}" "${network_args[@]}" ${ca_args[@]+"${ca_args[@]}"} \
    -e GCS_MIMIC_DOCKER_GATEWAY="$gateway" ${docker_args[@]+"${docker_args[@]}"} \
    -v "$root:/work:ro" -w /work/sdk-tests/php "$image" php "$@"
}
for file in harness.php migration_scenario.php signed.php suite.php; do
  php -- -l "$file" >/dev/null
done
php -- suite.php --self-check
php -e GCS_MIMIC_ENDPOINT="$endpoint" -- suite.php --migration-scenario
php -e GCS_MIMIC_ENDPOINT="$endpoint" -- suite.php
php -e GCS_MIMIC_ENDPOINT="$strict_endpoint" -e GCS_MIMIC_TOKEN=local-admin-token -e GCS_MIMIC_SIGNED_TESTS=1 -- suite.php
