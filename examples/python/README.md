# Python example

Creates a bucket, uploads, lists, downloads and deletes an object with `google-cloud-storage` 3.17.1. Needs uv 0.13.6 or newer (it installs Python 3.15.2).

## Start gcs-mimic

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  -e GCS_MIMIC_SCHEME=http ghcr.io/shiroemons/gcs-mimic:0.18.0
```

## Run

```sh
./run.sh
```

Set `GCSM_ENDPOINT` to use another address (default `http://localhost:4443`).

## Client setup

```python
client = storage.Client(project="local-project",
    credentials=AnonymousCredentials(),
    client_options={"api_endpoint": "http://localhost:4443"})
```
