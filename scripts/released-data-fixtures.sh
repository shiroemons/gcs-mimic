#!/usr/bin/env bash
# Regenerates tests/fixtures/released-data/<version>: a data directory written by a
# published release image plus the JSON that release returned for it. The upgrade
# test opens these directories with the current build, so the fixtures must come
# from the real release rather than from a hand-downgraded current catalog.
set -euo pipefail

if [[ ${1:-} == --help ]]; then
  echo "Usage: bash scripts/released-data-fixtures.sh [VERSION ...]"
  exit 0
fi

# Stable releases and release candidates whose data directories the contract upgrades. Pin digests so a
# regenerated fixture comes from the same published bytes.
# A case statement instead of an associative array keeps macOS's bash 3.2 working.
image_for() {
  case $1 in
    0.8.0) echo ghcr.io/shiroemons/gcs-mimic@sha256:12fb3dab04c3ba04809a88633534c79fbab23293aec3d299fefe82ae66cd9b63 ;;
    0.9.0) echo ghcr.io/shiroemons/gcs-mimic@sha256:6ebafefe846bcd7a0b52870a60ca95512c907694857391c916888eacdb2a8e50 ;;
    0.10.0) echo ghcr.io/shiroemons/gcs-mimic@sha256:8da0792404585bb8cf2971a225588a635c86f633948025ff9db06a2357a03e2a ;;
    0.11.0) echo ghcr.io/shiroemons/gcs-mimic@sha256:431a91841b1640042963b2de61f6e17b6e78e321157524e7b50576e8d728a6cc ;;
    0.12.0) echo ghcr.io/shiroemons/gcs-mimic@sha256:82ab6b8eb8d08d5ae04f3306ebc95c014f55ed19a31bb0dc172ebbb3a17b73c5 ;;
    0.13.0) echo ghcr.io/shiroemons/gcs-mimic@sha256:14d0a1eb9d2b2cb08a6f371c3ec379c25e6ddedfc80e2f13c15ead463b1a8892 ;;
    0.14.0) echo ghcr.io/shiroemons/gcs-mimic@sha256:f45b1324f63aff31575e0836f1892424ce360af6c081148b0407aa2dfa43eb4f ;;
    0.15.0) echo ghcr.io/shiroemons/gcs-mimic@sha256:be9f91e659b17a9fd4b65b4ddee8d26b7472e80cb9b9566605c72eead893bd49 ;;
    0.16.0) echo ghcr.io/shiroemons/gcs-mimic@sha256:ff15b8064c1cef73c2f4578d3b84e6b219e17729c3227e6577879c52e8562bc4 ;;
    0.17.0) echo ghcr.io/shiroemons/gcs-mimic@sha256:65807307c2565a4e0e1e0005827f71633d166547336b1258aead8f811a13b8eb ;;
    0.17.1) echo ghcr.io/shiroemons/gcs-mimic@sha256:2263fadc282c4d5486f58785c00cad6e0b0e81a43855c472e7acc3a832b0ce22 ;;
    0.18.0) echo ghcr.io/shiroemons/gcs-mimic@sha256:ed8328f03202da4300b48eea94ac4eeceadbbda5800f39a20abb2660ff66c7b9 ;;
    0.19.0) echo ghcr.io/shiroemons/gcs-mimic@sha256:9b0f0c0a276b4d16684a0cfa6b07ad87755be91c025c1e7d16139ed716e3d4d1 ;;
    *) return 1 ;;
  esac
}
if [[ $# == 0 ]]; then set -- 0.8.0 0.9.0 0.10.0 0.11.0 0.12.0 0.13.0 0.14.0 0.15.0 0.16.0 0.17.0 0.17.1 0.18.0 0.19.0; fi
for version in "$@"; do
  bash "$(dirname "$0")/release-version.sh" "$version" >/dev/null
  if ! image_for "$version" >/dev/null; then echo "No pinned image for $version" >&2; exit 2; fi
done

cd "$(dirname "$0")/.."
port=14480
base=http://127.0.0.1:$port
container=
cleanup() {
  if [[ -n $container ]]; then docker rm -f "$container" >/dev/null 2>&1 || true; fi
}
trap cleanup EXIT

request() {
  local method=$1 path=$2 type=$3 body=$4
  curl -fsS -X "$method" -H "Content-Type: $type" --data-binary "$body" "$base$path"
}

for version in "$@"; do
  target=tests/fixtures/released-data/$version
  container=gcs-mimic-fixture-$version
  docker rm -f "$container" >/dev/null 2>&1 || true
  docker run -d --name "$container" -p "127.0.0.1:$port:4443" \
    -e GCS_MIMIC_SCHEME=http -e GCS_MIMIC_API_BASE_URL="$base" \
    "$(image_for "$version")" >/dev/null
  ready=
  for _ in $(seq 1 60); do
    if curl -fsS "$base/storage/v1/b?project=fixture" >/dev/null 2>&1; then ready=1; break; fi
    sleep 0.5
  done
  if [[ -z $ready ]]; then docker logs "$container" >&2; exit 1; fi

  # A versioned bucket with an archived generation, a patched live generation and a
  # non-ASCII name, plus an unversioned bucket: the state a typical user keeps.
  request POST '/storage/v1/b?project=fixture' application/json \
    '{"name":"released","versioning":{"enabled":true}}' >/dev/null
  request POST '/storage/v1/b?project=fixture' application/json '{"name":"plain"}' >/dev/null
  request POST '/upload/storage/v1/b/released/o?uploadType=media&name=dir%2Fhello.txt' \
    text/plain 'first generation' >/dev/null
  request POST '/upload/storage/v1/b/released/o?uploadType=media&name=dir%2Fhello.txt' \
    text/plain 'second generation' >/dev/null
  request PATCH '/storage/v1/b/released/o/dir%2Fhello.txt' application/json \
    '{"metadata":{"origin":"released"},"cacheControl":"no-cache"}' >/dev/null
  request POST '/upload/storage/v1/b/released/o?uploadType=media&name=%E6%97%A5%E6%9C%AC%E8%AA%9E%20file.txt' \
    text/plain 'unicode name' >/dev/null
  request POST '/upload/storage/v1/b/plain/o?uploadType=media&name=a.bin' \
    application/octet-stream 'plain bytes' >/dev/null

  rm -rf "$target"
  mkdir -p "$target"
  {
    printf '{"version":"%s","image":"%s",' "$version" "$(image_for "$version")"
    printf '"buckets":%s,' "$(curl -fsS "$base/storage/v1/b?project=fixture")"
    printf '"released":%s,' "$(curl -fsS "$base/storage/v1/b/released/o?versions=true")"
    printf '"plain":%s}' "$(curl -fsS "$base/storage/v1/b/plain/o")"
  } | jq --sort-keys . >"$target/expected.json"

  # A graceful stop checkpoints the catalog, so the copied directory is what a user
  # upgrading from this release would have on disk.
  docker stop -t 20 "$container" >/dev/null
  docker cp "$container:/var/lib/gcs-mimic" "$target/data"
  # The lock carries no persisted state and is recreated when the catalog opens.
  rm -f "$target/data/catalog.lock"
  docker rm "$container" >/dev/null
  container=
  echo "$version: $(find "$target/data" -type f | wc -l | tr -d ' ') files"
done
