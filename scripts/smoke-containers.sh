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
peer_image=curlimages/curl:8.23.0@sha256:11e64e28c5f58c8d93ee95fcf22ac055f48b0bc440e0ce7d8013bbb5ad6aa8ac

container=
project=
volume=
work=$(mktemp -d)
cleanup() {
  local status=$?
  if [[ $status != 0 && -n "$project" ]]; then
    docker compose -p "$project" logs --no-color || echo "Could not collect Compose logs" >&2
  fi
  if [[ -n "$container" ]]; then
    docker stop --timeout 15 "$container" >/dev/null
    docker rm "$container" >/dev/null
  fi
  if [[ -n "$project" ]]; then docker compose -p "$project" down --volumes; fi
  if [[ -n "$volume" ]]; then docker volume rm "$volume" >/dev/null; fi
  rm -rf "$work"
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

printf '\000gcs-mimic\377\n' >"$work/expected"
expect_object() {
  curl --disable --noproxy '*' --fail --silent --show-error --max-time 5 \
    "http://$address/storage/v1/b/smoke-bucket/o/dir%2Ffile.bin?alt=media" >"$work/actual"
  cmp "$work/expected" "$work/actual"
}

for platform in "$@"; do
  arch=${platform#linux/}
  export DOCKER_DEFAULT_PLATFORM=$platform
  export GCS_MIMIC_IMAGE="gcs-mimic:bucket-smoke-${arch}-$$"
  export GCS_MIMIC_PORT=0
  project="gcs-mimic-bucket-${arch}-$$"
  docker buildx build --platform "$platform" --load --tag "$GCS_MIMIC_IMAGE" .
  [[ $(docker image inspect --format '{{.Os}}/{{.Architecture}}' "$GCS_MIMIC_IMAGE") == "$platform" ]]
  [[ $(docker image inspect --format '{{.Config.User}}' "$GCS_MIMIC_IMAGE") == 10001:10001 ]]
  docker run --rm --network none --read-only "$GCS_MIMIC_IMAGE" --help
  docker run --rm --network none --read-only "$GCS_MIMIC_IMAGE" --version

  volume="${project}-isolated"
  docker volume create "$volume" >/dev/null
  container=$(docker run --detach --network none --read-only --cap-drop ALL \
    --mount "type=volume,src=$volume,dst=/var/lib/gcs-mimic" \
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
  docker volume rm "$volume" >/dev/null
  volume=

  docker compose -p "$project" config --quiet
  docker compose -p "$project" up --detach --no-build --wait --wait-timeout 40
  service=$(docker compose -p "$project" ps --quiet gcs)
  address=$(docker compose -p "$project" port gcs 4443)
  [[ $address == 127.0.0.1:* ]]
  [[ $(docker inspect --format '{{.HostConfig.ReadonlyRootfs}} {{.HostConfig.Memory}} {{.HostConfig.NanoCpus}} {{.HostConfig.PidsLimit}}' "$service") == 'true 134217728 1000000000 64' ]]
  expect_status GET /_internal/health 200
  expect_status GET /_internal/ready 200
  expect_status GET '/storage/v1/b?project=local-project' 200
  curl --disable --noproxy '*' --fail --silent --show-error \
    -H 'Content-Type: application/json' --data '{"name":"smoke-bucket"}' \
    "http://$address/storage/v1/b?project=local-project" | jq -e '.name == "smoke-bucket"' >/dev/null
  expect_status GET /storage/v1/b/smoke-bucket 200
  curl --disable --noproxy '*' --fail --silent --show-error --max-time 5 \
    -H 'Content-Type: application/octet-stream' --data-binary @"$work/expected" \
    "http://$address/upload/storage/v1/b/smoke-bucket/o?uploadType=media&name=dir%2Ffile.bin" \
    | jq -e '.name == "dir/file.bin" and .size == "12"' >/dev/null
  expect_object
  curl --disable --noproxy '*' --fail --silent --show-error \
    "http://$address/storage/v1/b?project=local-project" | jq -e '.items[0].name == "smoke-bucket"' >/dev/null
  expect_status POST /_internal/health 405
  docker compose -p "$project" run --rm --no-deps gcs healthcheck --url http://gcs:4443/_internal/ready
  docker run --rm --read-only --network "${project}_default" "$peer_image" \
    --disable --noproxy '*' --fail --silent --show-error -H 'Content-Type: application/json' \
    --data '{"name":"peer-bucket"}' 'http://gcs:4443/storage/v1/b?project=local-project' >/dev/null
  docker run --rm --read-only --network "${project}_default" "$peer_image" \
    --disable --noproxy '*' --fail --silent --show-error http://gcs:4443/storage/v1/b/peer-bucket \
    | jq -e '.name == "peer-bucket"' >/dev/null
  docker run --rm --read-only --network "${project}_default" "$peer_image" \
    --disable --noproxy '*' --fail --silent --show-error 'http://gcs:4443/storage/v1/b?project=local-project&prefix=peer-' \
    | jq -e '.items[0].name == "peer-bucket"' >/dev/null
  docker run --rm --read-only --network "${project}_default" "$peer_image" \
    --disable --noproxy '*' --fail --silent --show-error -X DELETE http://gcs:4443/storage/v1/b/peer-bucket
  docker compose -p "$project" stop --timeout 15
  [[ $(docker inspect --format '{{.State.ExitCode}}' "$service") == 0 ]]
  # `start --wait` needs a newer Compose than the hosted runners ship; `up --no-recreate`
  # restarts the same stopped container and waits for health on every supported version.
  docker compose -p "$project" up --detach --no-build --no-recreate --wait --wait-timeout 40
  [[ $(docker compose -p "$project" ps --quiet gcs) == "$service" ]]
  address=$(docker compose -p "$project" port gcs 4443)
  expect_status GET /storage/v1/b/smoke-bucket 200
  expect_object
  docker compose -p "$project" down
  docker compose -p "$project" up --detach --no-build --wait --wait-timeout 40
  address=$(docker compose -p "$project" port gcs 4443)
  expect_status GET /storage/v1/b/smoke-bucket 200
  expect_object
  expect_status DELETE '/storage/v1/b/smoke-bucket/o/dir%2Ffile.bin' 204
  expect_status GET '/storage/v1/b/smoke-bucket/o/dir%2Ffile.bin?alt=media' 404
  expect_status DELETE /storage/v1/b/smoke-bucket 204
  expect_status GET /storage/v1/b/smoke-bucket 404
  curl --disable --noproxy '*' --fail --silent --show-error \
    -H 'Content-Type: application/json' --data '{"name":"reset-bucket"}' \
    "http://$address/storage/v1/b?project=local-project" >/dev/null
  docker compose -p "$project" down --volumes
  docker compose -p "$project" up --detach --no-build --wait --wait-timeout 40
  address=$(docker compose -p "$project" port gcs 4443)
  expect_status GET /storage/v1/b/reset-bucket 404
  docker compose -p "$project" down --volumes
  project=
  echo "PASS: $platform build, isolated health, host/DNS access, bucket/object media CRUD, stop/start and down/up persistence, deliberate volume reset, resource limits, SIGINT and SIGTERM"
done
