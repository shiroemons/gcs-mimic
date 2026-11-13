# Node.js example

Creates a bucket, uploads, lists, downloads and deletes an object with `@google-cloud/storage` 8.4.0 and `google-auth-library` 11.3.4. Needs Node.js 24 or newer. The example stays on 8.4.0 while it targets 0.20.0: `@google-cloud/storage` 9.0.0 needs a later gcs-mimic to delete and list objects ([SDK setup](../../docs/en/sdk-setup.md#nodejs)).

## Start gcs-mimic

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  ghcr.io/shiroemons/gcs-mimic:0.20.0
```

## Run

```sh
./run.sh
```

Set `GCSM_ENDPOINT` to use another address (default `http://localhost:4443`).

The download prints two `MaxListenersExceededWarning` messages. They come from streams inside the SDK, are a known warning, and do not affect the result.

## Client setup

```js
import {Storage} from '@google-cloud/storage';
import {PassThroughClient} from 'google-auth-library';

const storage = new Storage({projectId: 'local-project',
  apiEndpoint: 'http://localhost:4443', authClient: new PassThroughClient()});
```

`PassThroughClient` sends requests without credentials. 8.x does that for a custom `apiEndpoint` anyway; 9.x would look for Application Default Credentials without it.

Leave `STORAGE_EMULATOR_HOST` unset. When it is set, this SDK uses it instead of `apiEndpoint`, as the full JSON API base URL, so `http://localhost:4443` there breaks every request.
