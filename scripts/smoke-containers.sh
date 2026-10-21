#!/usr/bin/env bash
set -euo pipefail

if [[ ${1:-} == --help ]]; then
  echo "Usage: bash scripts/smoke-containers.sh [linux/amd64|linux/arm64 ...]"
  exit 0
fi
if [[ $# == 0 ]]; then set -- linux/amd64 linux/arm64; fi
for platform in "$@"; do
  case "$platform" in
    linux/amd64|linux/arm64) ;;
    *) echo "Unsupported platform: $platform" >&2; exit 2 ;;
  esac
done
cd "$(dirname "$0")/.."

container=
project=
cleanup() {
  local status=$?
  if [[ $status != 0 && -n "$project" ]]; then
    docker compose -p "$project" logs --no-color || echo "Could not collect Compose logs" >&2
  fi
  if [[ -n "$container" ]]; then
    docker stop --timeout 15 "$container" >/dev/null
    docker rm "$container" >/dev/null
  fi
  if [[ -n "$project" ]]; then docker compose -p "$project" down; fi
}
trap cleanup EXIT

expect_status() {
  local actual
  actual=$(curl --disable --noproxy '*' --silent --show-error --max-time 5 \
    --request "$1" --output /dev/null --write-out '%{http_code}' "http://$address$2")
  if [[ $actual != "$3" ]]; then
    echo "Expected HTTP $3 for $1 $2, received $actual" >&2
    return 1
  fi
}

for platform in "$@"; do
  arch=${platform#linux/}
  export DOCKER_DEFAULT_PLATFORM=$platform
  export GCS_MIMIC_IMAGE="gcs-mimic:p0-smoke-${arch}-$$"
  export GCS_MIMIC_PORT=0
  project="gcs-mimic-p0-${arch}-$$"
  docker buildx build --platform "$platform" --load --tag "$GCS_MIMIC_IMAGE" .
  [[ $(docker image inspect --format '{{.Os}}/{{.Architecture}}' "$GCS_MIMIC_IMAGE") == "$platform" ]]
  [[ $(docker image inspect --format '{{.Config.User}}' "$GCS_MIMIC_IMAGE") == 10001:10001 ]]
  docker run --rm --network none --read-only "$GCS_MIMIC_IMAGE" --help
  docker run --rm --network none --read-only "$GCS_MIMIC_IMAGE" --version

  container=$(docker run --detach --network none --read-only --cap-drop ALL \
    --security-opt no-new-privileges --memory 128m --cpus 1 --pids-limit 64 \
    --ulimit nofile=1024:1024 "$GCS_MIMIC_IMAGE")
  ready=false
  for ((attempt = 0; attempt < 40; attempt++)); do
    if [[ $(docker inspect --format '{{.State.Health.Status}}' "$container") == healthy ]]; then ready=true; break; fi
    sleep 1
  done
  if [[ $ready != true ]]; then docker logs "$container"; exit 1; fi
  docker exec "$container" gcs-mimic healthcheck --url http://127.0.0.1:4443/_internal/health
  docker stop --signal SIGINT --timeout 15 "$container" >/dev/null
  [[ $(docker inspect --format '{{.State.ExitCode}}' "$container") == 0 ]]
  docker rm "$container" >/dev/null
  container=

  docker compose -p "$project" config --quiet
  docker compose -p "$project" up --detach --no-build --wait --wait-timeout 40
  service=$(docker compose -p "$project" ps --quiet gcs)
  address=$(docker compose -p "$project" port gcs 4443)
  [[ $address == 127.0.0.1:* ]]
  [[ $(docker inspect --format '{{.HostConfig.ReadonlyRootfs}} {{.HostConfig.Memory}} {{.HostConfig.NanoCpus}} {{.HostConfig.PidsLimit}}' "$service") == 'true 134217728 1000000000 64' ]]
  expect_status GET /_internal/health 200
  expect_status GET /_internal/ready 200
  expect_status GET '/storage/v1/b?project=local-project' 404
  expect_status POST /_internal/health 405
  docker compose -p "$project" run --rm --no-deps gcs healthcheck --url http://gcs:4443/_internal/ready
  docker compose -p "$project" stop --timeout 15
  [[ $(docker inspect --format '{{.State.ExitCode}}' "$service") == 0 ]]
  docker compose -p "$project" down
  project=
  echo "PASS: $platform build, isolated health, host/DNS access, resource limits, SIGINT and SIGTERM"
done
