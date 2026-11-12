# Node.js example

Creates a bucket, uploads, lists, downloads and deletes an object with `@google-cloud/storage` 8.4.0. Needs Node.js 24 or newer.

## Start gcs-mimic

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  ghcr.io/shiroemons/gcs-mimic:0.19.1
```

## Run

```sh
./run.sh
```

Set `GCSM_ENDPOINT` to use another address (default `http://localhost:4443`).

The download prints two `MaxListenersExceededWarning` messages. They come from streams inside the SDK, are a known warning, and do not affect the result.

## Client setup

```js
const storage = new Storage({projectId: 'local-project',
  apiEndpoint: 'http://localhost:4443'});
```

Leave `STORAGE_EMULATOR_HOST` unset. When it is set, this SDK uses it instead of `apiEndpoint`, as the full JSON API base URL, so `http://localhost:4443` there breaks every request.
