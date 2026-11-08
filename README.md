# gcs-mimic

<!-- markdownlint-configure-file {"MD033": {"allowed_elements": ["p", "img"]}} -->

<p align="center"><img src="docs/assets/micket.png" alt="Micket, the gcs-mimic mascot" width="120"></p>

<p align="center">The mascot is Micket (ミケット), from the "mi" of mimic and the "cket" of bucket.</p>

<p align="center">gcs-mimic is an independent project and is not an official Google product.</p>

[![Foundation checks](https://github.com/shiroemons/gcs-mimic/actions/workflows/ci.yaml/badge.svg?branch=main)](https://github.com/shiroemons/gcs-mimic/actions/workflows/ci.yaml)
[![Release](https://img.shields.io/github/v/release/shiroemons/gcs-mimic)](https://github.com/shiroemons/gcs-mimic/releases/latest)
[![Docker Hub](https://img.shields.io/docker/v/shiroemons/gcs-mimic?sort=semver&label=docker%20hub)](https://hub.docker.com/r/shiroemons/gcs-mimic)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

[日本語ドキュメント](docs/ja/README.md)

A local Google Cloud Storage emulator for development and CI, written in Rust.
Use the official Cloud Storage SDKs against it without a Google Cloud account.

- **Works with your SDK**: Go, Ruby, Node.js, PHP, Python, Java, .NET and Rust pass
  every case in the [SDK scorecard](#sdk-scorecard).
- **Maps common fake-gcs-server settings**: most old flags and
  `FAKE_GCS_*` variables keep working; data conversion and client adapters
  have explicit limits (see
  [migrating](#migrating-from-fake-gcs-server)).
- **One container**: preload files from a directory, keep data in a volume.
- **Production-like when you need it**: strict mode keeps private buckets
  private and verifies signed URLs, while your servers keep their own Cloud Storage calls (see
  [production-like access](#production-like-access)).

## Quick start

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  -e GCS_MIMIC_SCHEME=http ghcr.io/shiroemons/gcs-mimic:0.14.0
curl --retry 5 --retry-all-errors -H 'Content-Type: application/json' \
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
- Docker Hub has the same image: `shiroemons/gcs-mimic:0.14.0`.
- Without `GCS_MIMIC_SCHEME=http`, the emulator serves HTTPS with a built-in
  development certificate; see [local HTTPS](docs/en/local-https.md).

## Docker Compose

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.14.0
    ports:
      - "127.0.0.1:4443:4443"
    environment:
      GCS_MIMIC_SCHEME: http
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
service differently, replace `gcs` in the app's setting.

Links that the emulator returns, such as resumable upload URLs, use the address
each request connected to, so the app and programs on the host can share one
server. Set `GCS_MIMIC_API_BASE_URL` only to fix that address, for example
behind a reverse proxy that rewrites `Host`. Releases up to 0.11.0 return one
fixed address, so with them add `GCS_MIMIC_API_BASE_URL: http://gcs:4443` here.

## Production-like access

The setup above behaves like fake-gcs-server: every request is allowed and no
signature is checked. When private buckets and signed URLs need the checks that
Cloud Storage makes, use strict mode with a trusted listener:

| Goal | Setup |
| --- | --- |
| Behave like fake-gcs-server | The default, as above |
| Private buckets stay private, public buckets open through `allUsers`, browsers use signed URLs, your servers keep their own Cloud Storage calls | Strict mode with a trusted listener: the [strict local recipe](examples/strict-local/README.md) |

The minimal setup is five settings of the `gcs` service. Nothing needs
preparing: no certificate, no key and no service account.

- `GCS_MIMIC_SCHEME: http`
- `GCS_MIMIC_AUTH_MODE: strict`
- `GCS_MIMIC_TRUSTED_LISTEN: 0.0.0.0:4444`, a port for your servers on the
  Compose network. Requests there without credentials act as the built-in
  `trusted-app@gcs-mimic.invalid`. Never publish this port.
- `GCS_MIMIC_BUCKETS`, the buckets as inline JSON, with an `allUsers`
  `iamPolicy` for each public bucket
- `GCS_MIMIC_TEST_SIGNER: enabled`, for signed URLs and for Rails or any client
  that uses the `test-signer credentials` key. Leave it out only when nothing
  signs URLs or uses those credentials.

Go servers then set `STORAGE_EMULATOR_HOST: gcs:4444`. Rails reads the signer's
key from `gcs-mimic test-signer credentials`; see
[Rails Active Storage](docs/en/rails-active-storage.md#6-production-like-checks-in-strict-mode).
The recipe has the block to paste and explains each part. This needs 0.14.0 or
later. To check signatures only, while every request stays allowed,
see [which setup do I need](docs/en/container-usage.md#which-setup-do-i-need).

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

The examples connect from the host. From a Compose service, use `http://SERVICE:4443` with the emulator's service name, such as `http://gcs:4443`. Clients are anonymous and never contact Google. [SDK setup](docs/en/sdk-setup.md) has the tested versions, gRPC, signed URLs and strict authentication.

Runnable programs for every client below are in [examples/](examples/README.md).

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

**Java**:

```java
Storage storage = HttpStorageOptions.newBuilder()
    .setHost("http://localhost:4443")
    .setProjectId("local-project")
    .setCredentials(NoCredentials.getInstance())
    .build()
    .getService();
```

**.NET**:

```csharp
var storage = new StorageClientBuilder
{
    BaseUri = "http://localhost:4443/storage/v1/",
    UnauthenticatedAccess = true,
}.Build();
```

**Rust**: `Storage` reads and writes objects over HTTP on 4443, while `StorageControl` manages buckets over gRPC on 4444, so also set `GCS_MIMIC_GRPC_LISTEN=0.0.0.0:4444` and publish that port.

```rust
let storage = Storage::builder()
    .with_endpoint("http://localhost:4443")
    .with_credentials(anonymous::Builder::new().build())
    .build()
    .await?;
let control = StorageControl::builder()
    .with_endpoint("http://localhost:4444")
    .with_credentials(anonymous::Builder::new().build())
    .build()
    .await?;
```

**gcloud**: a throwaway `CLOUDSDK_CONFIG` keeps your own configuration untouched.

```sh
export CLOUDSDK_CONFIG="$(mktemp -d)"
export CLOUDSDK_CORE_PROJECT=local-project
export CLOUDSDK_API_ENDPOINT_OVERRIDES_STORAGE=http://localhost:4443/
export CLOUDSDK_AUTH_DISABLE_CREDENTIALS=True
```

**Rails Active Storage**: add one `config/storage.yml` entry for development
and test; production keeps its own. A non-Google `universe_domain` keeps
googleauth from contacting Google OAuth:

```yaml
gcs_emulator:
  service: GCS
  endpoint: http://localhost:4443/
  universe_domain: gcs-emulator.invalid
  # plus project, bucket and local credentials
```

[Rails Active Storage](docs/en/rails-active-storage.md) has the full entry,
bucket and CORS setup, Rails in containers, strict mode with the built-in test
signer and CI.

## Common settings

| Setting | Purpose |
| --- | --- |
| `GCS_MIMIC_SCHEME` | `https` (default), `http` or `both` |
| `GCS_MIMIC_AUTH_MODE` | `permissive` (default) allows every request, as fake-gcs-server does; `strict` authorizes each request by IAM and ACLs for local identities ([strict authentication](docs/en/getting-started.md#strict-local-authentication)) |
| `GCS_MIMIC_BUCKETS` | Buckets to create at startup, as inline JSON with settings such as `cors` and `iamPolicy`; `GCS_MIMIC_BUCKETS_FILE` reads the same document from a file ([bucket settings](docs/en/container-usage.md#initialize-bucket-settings)) |
| `GCS_MIMIC_IDENTITIES` | Local identities, tokens and roles for strict mode, as inline JSON; `GCS_MIMIC_IDENTITY_FILE` reads the same document from a file |
| `GCS_MIMIC_TRUSTED_LISTEN` | Strict mode: an unpublished listener, such as `0.0.0.0:4444`, where your servers' requests without credentials act as one identity ([trusted listener](docs/en/container-usage.md#keep-private-buckets-private-with-a-trusted-listener)) |
| `GCS_MIMIC_TEST_SIGNER` | `enabled` turns on the built-in signer `test-signer@gcs-mimic.invalid`, whose private key is published, so signed URLs need no key setup ([test signer](docs/en/signed-urls.md#built-in-test-signer)) |
| `GCS_MIMIC_SIGNED_URLS` | `ignore` (the permissive default) accepts signed URLs without checking them; `verify` checks their signatures and expiry ([verification](docs/en/signed-urls.md#verifying-signed-requests-in-permissive-mode)) |
| `GCS_MIMIC_CORS_FALLBACK` | CORS for public and XML URLs of buckets without CORS rules: `allow-all` (the permissive default) lets pages on any origin read them, `none` (the strict default) sends no CORS headers |
| `GCS_MIMIC_API_BASE_URL` | Fixes the address in links that the emulator returns; unset, links use the address of each request |
| `GCS_MIMIC_GRPC_LISTEN` | Optional gRPC listener, for example `0.0.0.0:4444` |
| `GCS_MIMIC_LOG_LEVEL` | `debug`, `info` (default), `warn` or `error` |

`GCS_MIMIC_BUCKETS`, `GCS_MIMIC_IDENTITIES`, `GCS_MIMIC_TRUSTED_LISTEN`,
`GCS_MIMIC_TEST_SIGNER` and `GCS_MIMIC_SIGNED_URLS` need 0.14.0 or later. Up
to 0.13.0, `GCS_MIMIC_CORS_FALLBACK` defaults to `none` in both
modes. `docker run --rm ghcr.io/shiroemons/gcs-mimic:0.14.0 --help` lists every
option.

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
| Load bucket/object seed bytes | ✅ Startup or opt-in reseed | ✅ Startup or reseed | ❌ |
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
| Java | 70/70 | 35/70 | 35/70 |
| .NET | 70/70 (1 n/a) | 31/70 (1 n/a) | 31/70 (1 n/a) |
| Rust | 47/47 (4 n/a) | 0/47 (4 n/a) | 0/47 (4 n/a) |
| Score (/100) | 100 | 39.8 | 37.7 |

Recorded on 2026-10-31 at commit `35de4e1`
([results](sdk-scorecard/results/2026-10-31-2/summary.md)). A passing case
means an ordinary SDK workflow works, not that the API is complete. n/a marks a
case the SDK has no public API for; it is excluded from that SDK's denominator.

## Migrating from fake-gcs-server

For the scoped container workflow, change the image (for example from `fsouza/fake-gcs-server:1.56.1`) and mount a separate data volume at `/var/lib/gcs-mimic` instead of `/storage`; most `command:` flags, `FAKE_GCS_*` variables and the `./data:/data` mount keep working, and each old setting is logged as a warning with its `GCS_MIMIC_*` name. This is not a blanket drop-in replacement claim. The **0.14.0** image has these differences:

| fake-gcs-server | gcs-mimic |
| --- | --- |
| Data in `/storage` | Kept in `/var/lib/gcs-mimic` instead; `-filesystem-root` is ignored. Convert old data with [`gcs-mimic import fake-gcs-server`](docs/en/slices/import-filesystem.md). |
| `-backend memory` | Selects the bounded RAM backend (`--backend memory`): data is lost at exit and the data directory is never opened. |
| `-event.*` (Pub/Sub notifications) | Delivered to a local Pub/Sub emulator named by `PUBSUB_EMULATOR_HOST`. Without one, startup fails, so tests that wait for notifications do not hang. |
| Deleting or overwriting objects | Since 0.12.0 the bytes go at once by default, unlike Cloud Storage. Set `GCS_MIMIC_DEFAULT_SOFT_DELETE_RETENTION_SECONDS` to a number of seconds to keep them. Releases up to 0.11.0 keep them for seven days, and buckets they created keep that policy. |
| `-external-url` | Still fixes the address in returned links. Since 0.12.0 you can remove it, so links use the address of each request. |
| `-public-host` | Accepted; matching the Host header as before also needs `GCS_MIMIC_COMPAT_PROFILE=legacy-emulator`. |
| `GET /storage/v1/b` without `?project=` | Returns 400, as in Google Cloud Storage. SDKs send the project already. |
| Cancelling a resumable upload | Returns 499, which Google documents for a successful cancel; it is not an error. |

Since 0.8.0, gcs-mimic also has scoped RAM storage, local Pub/Sub delivery, HTTP batch/method override, XML list/copy/multipart, opt-in fixture administration/reset/reseed, strict local JWTs, client adapters and a watched sync directory with `gcs-mimic sync` controls; 0.7.0 and earlier lack them, and broader rows stay partial until all their acceptance cases pass ([scope and verification map](docs/en/evidence/migration-integration.md)). Go embedding uses a Rust subprocess facade, with Unix HTTP for `NoListener`; wrappers that hard-code the `fake-gcs-server` executable need an explicit adapter, so changing their image alone is insufficient. The [migration guide](docs/en/fake-gcs-server-migration.md) covers the full workflow, the [setting mapping](docs/en/fake-gcs-server-configuration.md) and [observed GitHub uses](docs/en/fake-gcs-server-migration.md#observed-github-uses-and-migration-boundaries).

## More information

- [Docker usage](docs/en/container-usage.md), [SDK setup](docs/en/sdk-setup.md)
  and [Rails Active Storage](docs/en/rails-active-storage.md)
- [Build from source and API walkthroughs](docs/en/getting-started.md),
  [local HTTPS](docs/en/local-https.md), [signed URLs](docs/en/signed-urls.md)
  and the [native binary](docs/en/native-binary.md)
- [fake-gcs-server migration guide](docs/en/fake-gcs-server-migration.md)
- [Storage design](docs/en/storage-backends.md), [architecture](docs/en/design.md)
  and [roadmap](docs/en/roadmap.md)
- [Development](docs/en/development.md) and [release policy](docs/en/release-policy.md)
- [0.14.0 release notes](docs/en/releases/0.14.0.md) and [all releases](https://github.com/shiroemons/gcs-mimic/releases)

## License

Copyright (c) 2026 shiroemons

[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE).
Third-party material retains its own license.
This is an independent project and is not an official Google product.
