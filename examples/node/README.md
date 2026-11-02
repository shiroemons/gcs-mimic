# Node.js example

`main.mjs` uses the official `@google-cloud/storage` package (8.4.0) to create
a bucket, upload a text object, list the bucket, download the object and delete
both. It talks only to a local gcs-mimic and never contacts Google.

## Prerequisites

- Node.js 24 or newer, with npm
- Docker, to start gcs-mimic

## Start gcs-mimic

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  -e GCS_MIMIC_SCHEME=http ghcr.io/shiroemons/gcs-mimic:0.9.0
```

## Run

```sh
./run.sh
```

`run.sh` installs the packages pinned in `package-lock.json` with `npm ci` and
runs `main.mjs` against `GCS_MIMIC_ENDPOINT` (default `http://localhost:4443`).
It accepts only a loopback `http://` origin. The output looks like this:

```text
Created bucket example-node-1a2b3c4d
Uploaded hello.txt
Objects: hello.txt
Downloaded hello.txt: Hello from Node.js!
Deleted bucket example-node-1a2b3c4d
```

The pinned SDK's internal download streams also print a
`MaxListenersExceededWarning`; it does not affect the result.

Stop the emulator with `docker rm -f gcs-mimic`.

## Client setup

```js
import {Storage} from '@google-cloud/storage';
const storage = new Storage({projectId: 'local-project',
  apiEndpoint: 'http://localhost:4443'});
```

With a custom `apiEndpoint` and no credentials, the SDK sends anonymous
requests. Leave `STORAGE_EMULATOR_HOST` unset: this SDK reads it as a path.
