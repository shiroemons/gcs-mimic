# Node.js example

Creates a bucket, uploads, lists, downloads and deletes an object with
`@google-cloud/storage` 8.4.0. Needs Node.js 24 or newer.

## Start gcs-mimic

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  -e GCS_MIMIC_SCHEME=http ghcr.io/shiroemons/gcs-mimic:0.9.0
```

## Run

```sh
./run.sh
```

Set `GCS_MIMIC_ENDPOINT` to use another address (default
`http://localhost:4443`).

## Client setup

```js
const storage = new Storage({projectId: 'local-project',
  apiEndpoint: 'http://localhost:4443'});
```

Leave `STORAGE_EMULATOR_HOST` unset: this SDK reads it as a path.
