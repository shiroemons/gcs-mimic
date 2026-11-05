# gcloud example

Creates a bucket, uploads (a file, stdin, a resumable upload, a parallel composite upload), lists, downloads (including a sliced download) and deletes objects with `gcloud storage`. Requires the Google Cloud SDK (tested with 591.0.0).

## Start gcs-mimic

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  -e GCS_MIMIC_SCHEME=http ghcr.io/shiroemons/gcs-mimic:0.11.0
```

## Run

```sh
./gcloud-example.sh
```

Set `GCS_MIMIC_ENDPOINT` to use an endpoint other than `http://localhost:4443`. `gcloud storage rm -r gs://BUCKET` is not supported yet (gcs-mimic has no managed folders API); delete with `gcloud storage rm "gs://BUCKET/**"` and `gcloud storage buckets delete gs://BUCKET`, as the script does. Without a local SDK, run it in the official image on Linux:

```sh
docker run --rm --network host -v "$PWD:/example:ro" \
  gcr.io/google.com/cloudsdktool/google-cloud-cli:591.0.0-alpine \
  bash /example/gcloud-example.sh
```

## Client setup

These variables send `gcloud storage` to gcs-mimic without credentials. A throwaway `CLOUDSDK_CONFIG` keeps your own gcloud configuration untouched:

```sh
export CLOUDSDK_CONFIG="$(mktemp -d)"
export CLOUDSDK_CORE_PROJECT=local-project
export CLOUDSDK_API_ENDPOINT_OVERRIDES_STORAGE=http://localhost:4443/
export CLOUDSDK_AUTH_DISABLE_CREDENTIALS=True
```
