#!/usr/bin/env bash
set -euo pipefail

if [[ ! ${GCSM_ENDPOINT+x} && ${GCS_MIMIC_ENDPOINT+x} ]]; then
  echo 'GCS_MIMIC_ENDPOINT was renamed to GCSM_ENDPOINT' >&2
  exit 1
fi

# Point gcloud at gcs-mimic with no credentials, using a throwaway configuration
# so your own gcloud settings are neither read nor changed.
export CLOUDSDK_CONFIG="$(mktemp -d)"
export CLOUDSDK_CORE_PROJECT=local-project
export CLOUDSDK_API_ENDPOINT_OVERRIDES_STORAGE="${GCSM_ENDPOINT:-http://localhost:4443}/"
export CLOUDSDK_AUTH_DISABLE_CREDENTIALS=True
export CLOUDSDK_CORE_DISABLE_USAGE_REPORTING=True
export CLOUDSDK_COMPONENT_MANAGER_DISABLE_UPDATE_CHECK=True
cd "$CLOUDSDK_CONFIG"

bucket="gs://example-gcloud-$RANDOM$RANDOM"

gcloud storage buckets create "$bucket"
gcloud storage buckets list --format='value(name)'

echo 'Hello from gcloud!' > hello.txt
gcloud storage cp hello.txt "$bucket/hello.txt"
echo 'Streamed from stdin' | gcloud storage cp - "$bucket/stdin.txt"

gcloud storage objects list "$bucket" --format='value(name,size)'
gcloud storage ls "$bucket"
gcloud storage objects describe "$bucket/hello.txt" --format='value(name,size,contentType)'

gcloud storage cat "$bucket/hello.txt"
gcloud storage cp "$bucket/stdin.txt" downloaded.txt
cat downloaded.txt

# Files of 8 MiB or more use a resumable upload. Lower thresholds also show a
# parallel composite upload and a sliced download without a huge file.
head -c 9437184 /dev/urandom > large.bin
gcloud storage cp large.bin "$bucket/large.bin"
CLOUDSDK_STORAGE_PARALLEL_COMPOSITE_UPLOAD_ENABLED=True \
CLOUDSDK_STORAGE_PARALLEL_COMPOSITE_UPLOAD_THRESHOLD=4M \
CLOUDSDK_STORAGE_PARALLEL_COMPOSITE_UPLOAD_COMPONENT_SIZE=4M \
  gcloud storage cp large.bin "$bucket/composite.bin"
CLOUDSDK_STORAGE_SLICED_OBJECT_DOWNLOAD_THRESHOLD=4M \
  gcloud storage cp "$bucket/composite.bin" large-copy.bin
cmp large.bin large-copy.bin

gcloud storage rm "$bucket/**"
gcloud storage buckets delete "$bucket"
rm -rf "$CLOUDSDK_CONFIG"
