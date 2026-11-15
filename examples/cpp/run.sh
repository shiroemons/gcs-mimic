#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

# The SDK is compiled inside the image on the first build; later builds reuse the cached layers.
docker build --quiet -t gcs-mimic-example-cpp . >/dev/null
# Joining the emulator container's network makes localhost:4443 inside this container reach it.
# Only the variables the example reads are passed on, and only when they are set.
env_args=()
for name in GCSM_ENDPOINT GCS_MIMIC_ENDPOINT CLOUD_STORAGE_EMULATOR_ENDPOINT; do
  [[ -z ${!name+set} ]] || env_args+=(-e "$name")
done
# macOS ships Bash 3.2, where an empty array is unbound under set -u.
docker run --rm --network "${GCSM_DOCKER_NETWORK:-container:gcs-mimic}" \
  ${env_args[@]+"${env_args[@]}"} gcs-mimic-example-cpp
