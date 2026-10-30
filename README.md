# gcs-mimic

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

[![Foundation checks](https://github.com/shiroemons/gcs-mimic/actions/workflows/ci.yaml/badge.svg?branch=main)](https://github.com/shiroemons/gcs-mimic/actions/workflows/ci.yaml)
[![Release](https://img.shields.io/github/v/release/shiroemons/gcs-mimic)](https://github.com/shiroemons/gcs-mimic/releases/latest)
[![Docker Hub](https://img.shields.io/docker/v/shiroemons/gcs-mimic?sort=semver&label=docker%20hub)](https://hub.docker.com/r/shiroemons/gcs-mimic)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

[日本語ドキュメント](docs/ja/README.md)

A local Google Cloud Storage emulator for development and CI, written in Rust.
Use the official Cloud Storage SDKs against it without a Google Cloud account.

- **Works with your SDK**: Go, Ruby, Node.js, Python and PHP pass every case in
  the [SDK scorecard](#sdk-scorecard).
- **Replaces fake-gcs-server**: swap the image; most old flags and
  `FAKE_GCS_*` variables keep working (after 0.6.0; see
  [migrating](#migrating-from-fake-gcs-server)).
- **One container**: preload files from a directory, keep data in a volume.

## Quick start

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  -e GCS_MIMIC_SCHEME=http ghcr.io/shiroemons/gcs-mimic:0.6.0
curl --retry 5 --retry-connrefused -H 'Content-Type: application/json' \
  -d '{"name":"demo-bucket"}' 'http://localhost:4443/storage/v1/b?project=demo-project'
curl --data-binary 'hello' \
  'http://localhost:4443/upload/storage/v1/b/demo-bucket/o?uploadType=media&name=hello.txt'
curl 'http://localhost:4443/storage/v1/b/demo-bucket/o/hello.txt?alt=media'
```

The last command prints `hello`. Next, [connect your SDK](#connect-your-sdk).

- The port is published on `127.0.0.1` only, because the emulator accepts
  unauthenticated requests by default.
- Add `-v gcs-mimic-data:/var/lib/gcs-mimic` to keep data after the container
  is removed.
- Docker Hub has the same image: `shiroemons/gcs-mimic:0.6.0`.
- Without `GCS_MIMIC_SCHEME=http`, the emulator serves HTTPS with a built-in
  development certificate; see [local HTTPS](docs/en/local-https.md).

## Docker Compose

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.6.0
    ports:
      - "127.0.0.1:4443:4443"
    environment:
      GCS_MIMIC_SCHEME: http
      GCS_MIMIC_API_BASE_URL: http://gcs:4443
    volumes:
      - ./data:/data:ro # optional: files to preload
      - gcs-data:/var/lib/gcs-mimic
  app:
    build: .
    environment:
      # Go and Python read this. For Node.js, remove it and use apiEndpoint.
      STORAGE_EMULATOR_HOST: http://gcs:4443
    depends_on:
      gcs:
        condition: service_healthy
volumes:
  gcs-data:
```

`docker compose up -d --wait` returns once the emulator is healthy.

`gcs` in `http://gcs:4443` is the Compose service name. If you name the
service differently, replace `gcs` in both `GCS_MIMIC_API_BASE_URL` and the
app's setting.

`GCS_MIMIC_API_BASE_URL` goes into links that the emulator returns, such as
resumable upload URLs. Set it to the address your app uses. Programs on the
host then get links they cannot resolve, so omit it if only the host connects.

## Preload files

Mount a directory at `/data`. Top-level directories become buckets, and files
become objects, as in fake-gcs-server:

```text
data/
  sample-bucket/
    some_file.txt
  empty-bucket/
```

The files are applied on every startup and replace API changes to the same
names; other objects are kept. See [loading local files](docs/en/container-usage.md#load-local-files).

## Connect your SDK

The examples connect from the host. From a Compose service, use
`http://SERVICE:4443` with the emulator's service name, such as
`http://gcs:4443`. Clients are anonymous and never contact Google.
[SDK setup](docs/en/sdk-setup.md) has the tested versions, gRPC, signed URLs
and strict authentication.

**Go**: set `STORAGE_EMULATOR_HOST=localhost:4443`; a plain client then skips
authentication. A fake-gcs-server setup works unchanged.

```go
client, err := storage.NewClient(ctx)
```

**Ruby**: set a project to create and list buckets.

```ruby
require "google/cloud/storage"
storage = Google::Cloud::Storage.anonymous(endpoint: "http://localhost:4443/")
storage.service.project = "local-project"
```

**Node.js**: use `apiEndpoint` and leave `STORAGE_EMULATOR_HOST` unset; this
SDK reads that variable as a path.

```js
import {Storage} from '@google-cloud/storage';
const storage = new Storage({projectId: 'local-project',
  apiEndpoint: 'http://localhost:4443'});
```

**Python**: the tests also set `STORAGE_EMULATOR_HOST=http://localhost:4443`,
with the scheme.

```python
from google.auth.credentials import AnonymousCredentials
from google.cloud import storage
client = storage.Client(project="local-project",
    credentials=AnonymousCredentials(),
    client_options={"api_endpoint": "http://localhost:4443"})
```

**PHP**:

```php
use Google\Cloud\Core\AnonymousCredentials;
use Google\Cloud\Storage\StorageClient;
$storage = new StorageClient(['projectId' => 'local-project',
    'apiEndpoint' => 'http://localhost:4443',
    'credentialsFetcher' => new AnonymousCredentials()]);
```

**Rails Active Storage**: `storage.yml` alone does not work, because googleauth
still contacts Google OAuth. See the
[Rails recipe](docs/en/sdk-setup.md#rails-active-storage).

## Migrating from fake-gcs-server

> [!NOTE]
> This needs a release newer than 0.6.0. With 0.6.0, old flags stop startup,
> and the error names the `GCS_MIMIC_*` setting to use instead.

Swap the image and mount a data volume. Most `command:` flags, `FAKE_GCS_*`
variables and the `./data:/data` mount keep working:

```diff
 gcs:
-  image: fsouza/fake-gcs-server:1.56.1
+  image: ghcr.io/shiroemons/gcs-mimic:0.6.0
   command: ["-scheme", "http", "-external-url", "http://gcs:4443"]
   volumes:
     - ./data:/data
-    - gcs-storage:/storage
+    - gcs-data:/var/lib/gcs-mimic
```

Each old setting is logged as a warning with its `GCS_MIMIC_*` name; rename
them when convenient. Exceptions and differences:

| fake-gcs-server | gcs-mimic |
| --- | --- |
| Data in `/storage` | Kept in `/var/lib/gcs-mimic` instead; `-filesystem-root` is ignored. Convert old data with [`gcs-mimic import fake-gcs-server`](docs/en/slices/import-filesystem.md). |
| `-backend memory` | Not available: data survives restarts. Remove the container for a clean state. |
| `-event.*` (Pub/Sub notifications) | Not available. Startup fails, so tests that wait for notifications do not hang. |
| `-public-host` | Accepted; matching the Host header as before also needs `GCS_MIMIC_COMPAT_PROFILE=legacy-emulator`. |
| `GET /storage/v1/b` without `?project=` | Returns 400, as in Google Cloud Storage. SDKs send the project already. |
| Cancelling a resumable upload | Returns 499, which Google documents for a successful cancel; it is not an error. |

See the [migration guide](docs/en/fake-gcs-server-migration.md) and the
[setting mapping](docs/en/fake-gcs-server-configuration.md).

## Common settings

| Setting | Purpose |
| --- | --- |
| `GCS_MIMIC_SCHEME` | `https` (default), `http` or `both` |
| `GCS_MIMIC_API_BASE_URL` | Address in links that the emulator returns |
| `GCS_MIMIC_GRPC_LISTEN` | Optional gRPC listener, for example `0.0.0.0:4444` |
| `GCS_MIMIC_CORS_FALLBACK` | `allow-all` lets browsers read public URLs from any origin |
| `GCS_MIMIC_AUTH_MODE` | `strict` checks local bearer tokens; see [strict authentication](docs/en/getting-started.md#strict-local-authentication) |
| `GCS_MIMIC_LOG_LEVEL` | `debug`, `info` (default), `warn` or `error` |

`docker run --rm ghcr.io/shiroemons/gcs-mimic:0.6.0 --help` lists every option.

## Features at a glance

✅ Available · ◐ Partly · ❌ Unavailable. Each row compares one workflow, not
full Google API compatibility.

| Workflow | gcs-mimic | fake-gcs-server | gcp-storage-emulator |
| --- | --- | --- | --- |
| Create, list and delete multiple buckets | ✅ | ✅ | ✅ |
| Upload, download and delete live objects | ✅ | ✅ | ✅ |
| Resumable upload: initiate, send chunks and finalize | ✅ | ✅ | ✅ |
| Copy live-object bytes | ✅ | ✅ | ✅ |
| Rewrite with multi-call continuation | ✅ | ❌ | ❌ |
| Retain object version history | ✅ | ◐ Memory backend | ❌ |
| Keep API-created bytes across restart | ✅ | ✅ Filesystem | ✅ Disk |
| API writes visible as bucket/object files | ✅ Mirror | ✅ Filesystem | ✅ Disk |
| Load bucket/object seed bytes | ✅ Every startup | ✅ Startup or reseed | ❌ |
| Default CORS for JSON API requests | ✅ | ✅ | ❌ |
| Enforce bucket-specific XML CORS rules | ✅ | ❌ | ❌ |
| Upload/download with signed URLs | ✅ | ✅ | ✅ |
| Verify V4 RSA signatures and expiry | ✅ | ❌ | ❌ |
| gRPC Storage v2 with the Go client | ✅ | ❌ | ❌ |
| gRPC legacy Storage v1 reference RPCs | ✅ | ◐ First upload message | ❌ |
| Published Docker image | ✅ GHCR and Docker Hub | ✅ | ✅ |

Compared with fake-gcs-server `v1.56.1` and gcp-storage-emulator
`v2026.07.19`. The mirror row uses an optional
[sync directory](docs/en/container-usage.md#mirror-objects-to-a-host-directory),
and the gRPC rows cover a subset of RPCs. See the
[detailed comparison](docs/en/feature-comparison.md) and the
[feature matrix](docs/en/feature-matrix.md).

### SDK scorecard

Each official SDK runs the same workflows against all three emulators
([details](sdk-scorecard/README.md)):

| SDK | gcs-mimic | fake-gcs-server | gcp-storage-emulator |
| --- | --- | --- | --- |
| Go | 57/57 | 24/57 | 20/57 |
| Ruby | 51/51 | 24/51 | 22/51 |
| Node.js | 51/51 | 25/51 | 22/51 |
| PHP | 51/51 | 25/51 | 20/51 |
| Python | 51/51 | 19/51 | 24/51 |
| Score (/100) | 100 | 44.9 | 41.5 |

Recorded on 2026-10-29 at commit `e7fb384`
([results](sdk-scorecard/results/2026-10-29-5/summary.md)). A passing case
means an ordinary SDK workflow works, not that the API is complete.

## More information

- [Docker usage](docs/en/container-usage.md) and [SDK setup](docs/en/sdk-setup.md)
- [Build from source and API walkthroughs](docs/en/getting-started.md),
  [local HTTPS](docs/en/local-https.md), [signed URLs](docs/en/signed-urls.md)
  and the [native binary](docs/en/native-binary.md)
- [fake-gcs-server migration guide](docs/en/fake-gcs-server-migration.md)
- [Storage design](docs/en/storage-backends.md), [architecture](docs/en/design.md)
  and [roadmap](docs/en/roadmap.md)
- [Development](docs/en/development.md) and [release policy](docs/en/release-policy.md)
- [0.6.0 release notes](docs/en/releases/0.6.0.md) and
  [all releases](https://github.com/shiroemons/gcs-mimic/releases)

## License

Copyright (c) 2026 shiroemons

[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE).
Third-party material retains its own license.
This is an independent project and is not an official Google product.
