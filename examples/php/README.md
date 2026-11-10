# PHP example

Creates a bucket, uploads, lists, downloads and deletes an object with `google/cloud-storage` 2.6.1. Needs PHP 8.5 and Composer, or only Docker.

## Start gcs-mimic

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  -e GCS_MIMIC_SCHEME=http ghcr.io/shiroemons/gcs-mimic:0.17.0
```

## Run

```sh
./run.sh
```

Set `GCS_MIMIC_ENDPOINT` to use another address (default `http://localhost:4443`).

Without a local PHP, run it in the official Composer image. The container joins the emulator container's network, so `localhost:4443` reaches it:

```sh
docker run --rm --network container:gcs-mimic --user "$(id -u):$(id -g)" \
  -v "$PWD:/app" -w /app composer:2.11.1 bash run.sh
```

## Client setup

```php
$storage = new StorageClient(['projectId' => 'local-project',
    'apiEndpoint' => 'http://localhost:4443',
    'credentialsFetcher' => new AnonymousCredentials()]);
```
