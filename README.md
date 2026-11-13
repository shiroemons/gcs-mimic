# gcs-mimic

<!-- markdownlint-configure-file {"MD033": {"allowed_elements": ["p", "img", "details", "summary"]}} -->

<p align="center"><img src="docs/assets/micket.png" alt="Micket, the gcs-mimic mascot" width="120"></p>

<p align="center">The mascot is Micket (ミケット), from the "mi" of mimic and the "cket" of bucket.</p>

<p align="center">gcs-mimic is an independent project and is not an official Google product.</p>

[![Foundation checks](https://github.com/shiroemons/gcs-mimic/actions/workflows/ci.yaml/badge.svg?branch=main)](https://github.com/shiroemons/gcs-mimic/actions/workflows/ci.yaml)
[![Release](https://img.shields.io/github/v/release/shiroemons/gcs-mimic)](https://github.com/shiroemons/gcs-mimic/releases/latest)
[![Docker Hub](https://img.shields.io/docker/v/shiroemons/gcs-mimic?sort=semver&label=docker%20hub)](https://hub.docker.com/r/shiroemons/gcs-mimic)
[![Image size](https://img.shields.io/docker/image-size/shiroemons/gcs-mimic?sort=semver&arch=amd64)](https://hub.docker.com/r/shiroemons/gcs-mimic)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

[日本語ドキュメント](docs/ja/README.md)

**Tiny container. Official SDKs. No Google Cloud account.**

A local Google Cloud Storage emulator for development and CI, written in Rust.

## Quick start

```sh
# Start the emulator
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 ghcr.io/shiroemons/gcs-mimic:latest

# Check that it answers
curl --retry 5 --retry-all-errors 'http://localhost:4443/storage/v1/b?project=demo-project'
```

It answers with an empty bucket list.

- The same port also serves HTTPS with a built-in development certificate:
  `curl -k 'https://localhost:4443/storage/v1/b?project=demo-project'`. To trust
  the certificate instead of using `-k`, see [local HTTPS](docs/en/local-https.md).
- The port is published on `127.0.0.1` only, because the emulator accepts
  unauthenticated requests by default.
- Add `-v gcs-mimic-data:/var/lib/gcs-mimic` to keep data after the container
  is removed.
- Docker Hub has the same image: `shiroemons/gcs-mimic:latest`.
- `latest` is the newest release and can move to a new minor version with
  breaking changes. In CI, pin an exact version from
  [the releases](https://github.com/shiroemons/gcs-mimic/releases).

### Store and read an object

SDK clients connect over plain HTTP without trusting a certificate, and the
examples below use it too:

```sh
# Create a bucket
curl --retry 5 --retry-all-errors \
  -H 'Content-Type: application/json' \
  -d '{"name":"demo-bucket"}' \
  'http://localhost:4443/storage/v1/b?project=demo-project'

# Upload an object
curl --data-binary 'hello' \
  'http://localhost:4443/upload/storage/v1/b/demo-bucket/o?uploadType=media&name=hello.txt'

# Read it back
curl 'http://localhost:4443/storage/v1/b/demo-bucket/o/hello.txt?alt=media'
```

The last command prints `hello`.

### Common options

Add options after the image name, with one or two hyphens. `--name=value` also works.

| Option | What it does | In a Compose `environment:` |
| --- | --- | --- |
| `--scheme https` | The default `auto` serves HTTP and HTTPS on one port. `https` refuses plain HTTP, `http` refuses HTTPS, and `both` serves HTTP on a second port. Up to 0.18.0 the default was `https` | `GCS_MIMIC_SCHEME: https` |
| `--port 9000` | Port to listen on. Publish the same port with `-p 127.0.0.1:9000:9000` | `GCS_MIMIC_LISTEN: 0.0.0.0:9000` |
| `--port-http 8000` | HTTP port with `--scheme both`. Publish it too | `GCS_MIMIC_LISTEN_HTTP: 0.0.0.0:8000` |
| `--backend memory` | Keep everything in RAM. Objects can use about half of the available memory by default, from 64 MiB to 1 GiB | `GCS_MIMIC_BACKEND: memory` |
| `--log-level debug` | `debug`, `info`, `warn` or `error` | `GCS_MIMIC_LOG_LEVEL: debug` |

Mount a directory with `-v "$PWD/data:/data:ro"` to preload it: top-level
directories become buckets and files become objects. Run
`docker run --rm ghcr.io/shiroemons/gcs-mimic:latest --help` for every option.

Since 0.18.0, fake-gcs-server flags either preserve working requests or stop startup
with a native alternative. Accepted translations emit a `native_args` JSON array
at info level, plus a shell line when its values need no JSON unescaping.
`--filesystem-root`, `--external-url`, `--cors-headers` and
`--location` are refused; `event.*` requires a local Pub/Sub emulator.
`--host`, `--port` and `--port-http` are native options; `--port-http` requires
`--scheme both`. See the [complete flag table](docs/en/slices/option-aliases.md).

## Connect your SDK

The examples connect from the host to the HTTP server above. From a Compose service, use
`http://SERVICE:4443` with the emulator's service name, such as
`http://gcs:4443`. Clients are anonymous and never contact Google.
[SDK setup](docs/en/sdk-setup.md) has the tested versions, gRPC, signed URLs and
strict authentication, and [examples/](examples/README.md) has a runnable
program for every client below.

<details>
<summary>Go</summary>

Set `STORAGE_EMULATOR_HOST=localhost:4443`; a plain client then skips
authentication.

```go
client, err := storage.NewClient(ctx)
```

</details>

<details>
<summary>Ruby</summary>

Set a project to create and list buckets.

```ruby
require "google/cloud/storage"
storage = Google::Cloud::Storage.anonymous(endpoint: "http://localhost:4443/")
storage.service.project = "local-project"
```

</details>

<details>
<summary>Node.js</summary>

Use `apiEndpoint` and leave `STORAGE_EMULATOR_HOST` unset; this SDK reads that
variable as a path. `PassThroughClient` sends no credentials; without it,
`@google-cloud/storage` 9.x looks for Application Default Credentials
([SDK setup](docs/en/sdk-setup.md#nodejs)).

```js
import {Storage} from '@google-cloud/storage';
import {PassThroughClient} from 'google-auth-library';
const storage = new Storage({projectId: 'local-project',
  apiEndpoint: 'http://localhost:4443', authClient: new PassThroughClient()});
```

</details>

<details>
<summary>Python</summary>

The tests also set `STORAGE_EMULATOR_HOST=http://localhost:4443`, with the
scheme.

```python
from google.auth.credentials import AnonymousCredentials
from google.cloud import storage
client = storage.Client(project="local-project",
    credentials=AnonymousCredentials(),
    client_options={"api_endpoint": "http://localhost:4443"})
```

</details>

<details>
<summary>PHP</summary>

```php
use Google\Cloud\Core\AnonymousCredentials;
use Google\Cloud\Storage\StorageClient;
$storage = new StorageClient(['projectId' => 'local-project',
    'apiEndpoint' => 'http://localhost:4443',
    'credentialsFetcher' => new AnonymousCredentials()]);
```

</details>

<details>
<summary>Java</summary>

```java
Storage storage = HttpStorageOptions.newBuilder()
    .setHost("http://localhost:4443")
    .setProjectId("local-project")
    .setCredentials(NoCredentials.getInstance())
    .build()
    .getService();
```

</details>

<details>
<summary>.NET</summary>

```csharp
var storage = new StorageClientBuilder
{
    BaseUri = "http://localhost:4443/storage/v1/",
    UnauthenticatedAccess = true,
}.Build();
```

</details>

<details>
<summary>Rust</summary>

`Storage` reads and writes objects over HTTP on 4443, while `StorageControl`
manages buckets over gRPC on 4444, so also set
`GCS_MIMIC_GRPC_LISTEN=0.0.0.0:4444` and publish that port.

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

</details>

<details>
<summary>gcloud</summary>

A throwaway `CLOUDSDK_CONFIG` keeps your own configuration untouched.

```sh
export CLOUDSDK_CONFIG="$(mktemp -d)"
export CLOUDSDK_CORE_PROJECT=local-project
export CLOUDSDK_API_ENDPOINT_OVERRIDES_STORAGE=http://localhost:4443/
export CLOUDSDK_AUTH_DISABLE_CREDENTIALS=True
```

</details>

<details>
<summary>Rails Active Storage</summary>

Add one `config/storage.yml` entry for development and test; production keeps
its own. A non-Google `universe_domain` keeps googleauth from contacting Google
OAuth:

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

</details>

## Why gcs-mimic

- **Small.** One static binary on an empty base image: no shell, no package
  manager, no runtime to install.
- **Your SDK already speaks it.** Go, Ruby, Node.js, PHP, Python, Java, .NET and
  Rust pass every case in the [SDK scorecard](#sdk-scorecard). Point the client
  at it and run your code.
- **Buckets the way you need them.** Declare as many as you like at startup,
  each with its own CORS rules, [public or private](#buckets-cors-and-public-access).
- **Able to say no.** Strict mode keeps private buckets private and rejects bad
  signed URLs, so a permissions bug fails on your laptop instead of in
  production.
- **Careful with your bytes.** Bodies stream in 64 KiB chunks, so memory does
  not grow with object size. Data and unfinished resumable uploads survive a
  restart.

The numbers are under [how it compares](#how-it-compares).

## Docker Compose

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.20.1
    ports:
      - "127.0.0.1:4443:4443"
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

`docker compose up -d --wait` returns once the emulator is healthy. `gcs` in
`http://gcs:4443` is the Compose service name; if you rename the service, change
it here too.

Links that the emulator returns, such as resumable upload URLs, use the address
each request connected to, so the app and programs on the host can share one
server. Every setting is in the [Docker guide](docs/en/container-usage.md#common-settings).

The optional `./data` mount preloads files: top-level directories become buckets
and files become objects. The files are applied on every startup and replace API
changes to the same names. See [loading local files](docs/en/container-usage.md#load-local-files).

## Buckets, CORS and public access

Declare every bucket in one setting, each with its own CORS rules. Add this to
the `gcs` service above:

```yaml
    environment:
      GCS_MIMIC_BUCKETS: |
        {"version": 1, "buckets": [
          {"name": "public-assets",
           "iamPolicy": {"bindings": [{"role": "roles/storage.objectViewer", "members": ["allUsers"]}]},
           "cors": [{"origin": ["http://localhost:3000"], "method": ["GET", "HEAD", "PUT"],
                     "responseHeader": ["Content-Type"], "maxAgeSeconds": 3600}]},
          {"name": "private-uploads"}
        ]}
```

Both buckets exist at startup, and pages on `http://localhost:3000` get CORS
headers for `public-assets`. Your app keeps working as it did. So far anyone can
still read both buckets, because the default mode allows every request.

### Make private buckets private

Add two lines to the same `environment`:

```yaml
      GCS_MIMIC_AUTH_MODE: strict
      GCS_MIMIC_TRUSTED_LISTEN: 0.0.0.0:4444 # never publish this port
```

Now anyone can read `public-assets`, because its policy grants `allUsers`, and
`private-uploads` answers 401 without credentials. The second line is the
trusted listener: a port for your servers on the Compose network, where requests
without credentials are allowed to act as your app. Point your app at port 4444
instead of 4443.

Strict mode needs no certificate, key or service account. Uploading from a
browser needs a signed URL, because this policy grants reads only. The
[strict local recipe](examples/strict-local/README.md) has the full block to
paste and explains each part.

### Two modes

`permissive`, the default, has no gatekeeper: every request is allowed. `strict`
checks permissions and signatures on each request.

| | `permissive` (default) | `strict` |
| --- | --- | --- |
| Multiple buckets and per-bucket CORS | ✅ | ✅ |
| Who can read an object | Anyone | Anyone for `allUsers` buckets; otherwise only those with permission |
| Signed URLs | Not checked, unless `GCS_MIMIC_SIGNED_URLS=verify` | Always checked, including expiry |
| CORS for buckets without rules | Any origin | None |
| Your servers | Connect as they are | Connect to the trusted listener |
| Use it for | Getting started and ordinary SDK tests | Testing permissions and signed URLs |

Start with the default and switch when a test needs those checks. See
[which setup do I need](docs/en/container-usage.md#which-setup-do-i-need).

## How it compares

All four emulators cover the everyday basics. These tables are here to help you
choose.

gcs-mimic, fake-gcs-server and gcp-storage-emulator emulate Cloud Storage alone.
[floci-gcp](https://github.com/floci-io/floci-gcp) covers more ground: one
container also emulates Pub/Sub, Firestore and many other Google Cloud services,
so consider it when your tests need several of them at once.

### By the numbers

| | gcs-mimic | fake-gcs-server | gcp-storage-emulator | floci-gcp |
| --- | --- | --- | --- | --- |
| Image, compressed (linux/amd64) | 8.4 MB | 25.4 MB | 48.8 MB | 67.2 MB |
| Memory: idle, then after reading 1,000 objects | 9.9, then 29.3 MiB | 33.9, then 57.2 MiB | 27.3, then 32.0 MiB | 103.6, then 320.0 MiB |
| Download speed, 1 KiB to 100 KiB objects | 1.2× to 2.7× | 1× | 0.08× to 0.29× | 0.62× to 1.5× |
| Download speed, 1 MiB objects | 0.99× to 1.3× | 1× | 0.09× to 0.57× | 0.87× to 1.06× |
| Download speed, 10 MiB objects | 0.73× to 1.15× | 1× | 0.1× to 0.66× | 0.48× to 1× |
| Upload speed | 1.1× to 3.5× | 1× | 0.97× to 1.5× | 1.2× to 25× |

Measured on 2026-11-09 with 0.16.0, 1.56.1, v2026.07.19 and 0.12.0 on one
Apple-silicon Mac under Docker Desktop; the badge at the top shows the current
image size. Each server held up to 1,000 objects. floci-gcp ran in its `hybrid`
mode, which keeps objects in memory and writes them to disk in the background,
so its uploads do not wait for the disk and its memory grows with the stored
objects. Memory is process RSS. See the
[conditions and full results](docs/en/evidence/product-comparison-0.16.0.md).
Past about 1,000 distinct objects between writes, 0.16.0 read the rest at around
7,500 requests per second. 0.17.0
[reads them at 36,000 to 63,000](docs/en/evidence/read-path-performance.md#reads-past-the-read-cache-and-ram-reads)
with 8 or more connections.

### Feature comparison

✅ Available · ◐ Partly · ❌ Unavailable. Each row compares one workflow, not
full Google API compatibility.

| Workflow | gcs-mimic | fake-gcs-server | gcp-storage-emulator | floci-gcp |
| --- | --- | --- | --- | --- |
| Create, list and delete multiple buckets | ✅ | ✅ | ✅ | ✅ |
| Upload, download and delete live objects | ✅ | ✅ | ✅ | ✅ |
| Resumable upload: initiate, send chunks and finalize | ✅ | ✅ | ✅ | ✅ |
| Copy live-object bytes | ✅ | ✅ | ✅ | ✅ |
| Rewrite with multi-call continuation | ✅ | ❌ | ❌ | ✅ |
| Retain object version history | ✅ | ◐ Memory backend | ❌ | ✅ |
| Keep API-created bytes across restart | ✅ | ✅ Filesystem | ✅ Disk | ✅ Hybrid mode |
| API writes visible as bucket/object files | ✅ Mirror | ✅ Filesystem | ✅ Disk | ❌ |
| Load bucket/object seed bytes | ✅ Startup or opt-in reseed | ✅ Startup or reseed | ❌ | ◐ Startup hook script |
| Default CORS for JSON API requests | ✅ | ✅ | ❌ | ❌ |
| Enforce bucket-specific XML CORS rules | ✅ | ❌ | ❌ | ✅ |
| Upload/download with signed URLs | ✅ | ✅ | ✅ | ✅ |
| Verify V4 RSA signatures and expiry | ✅ | ❌ | ❌ | ◐ Expiry only |
| Keep private buckets private and open public ones with `allUsers` | ✅ Strict mode | ❌ | ❌ | ✅ IAM enforce mode |
| gRPC Storage v2 with the Go client | ✅ | ❌ | ❌ | ◐ 4 of 6 cases |
| gRPC legacy Storage v1 reference RPCs | ✅ | ◐ First upload message | ❌ | ❌ |
| Published Docker image | ✅ GHCR and Docker Hub | ✅ | ✅ | ✅ Docker Hub |

Compared with fake-gcs-server `v1.56.1`, gcp-storage-emulator `v2026.07.19` and
floci-gcp `0.12.0`. The mirror row uses an optional
[sync directory](docs/en/container-usage.md#mirror-objects-to-a-host-directory),
and the gRPC rows cover a subset of RPCs. See the
[detailed comparison](docs/en/feature-comparison.md), the
[floci-gcp checks](docs/en/evidence/floci-gcp-comparison.md) and the
[feature matrix](docs/en/feature-matrix.md).

### SDK scorecard

Each official SDK runs the same workflows against all four emulators
([details](sdk-scorecard/README.md)):

| SDK | gcs-mimic | fake-gcs-server | gcp-storage-emulator | floci-gcp |
| --- | --- | --- | --- | --- |
| Go | 57/57 | 24/57 | 20/57 | 48/57 |
| Ruby | 51/51 | 24/51 | 22/51 | 42/51 |
| Node.js | 51/51 | 25/51 | 22/51 | 44/51 |
| PHP | 51/51 | 25/51 | 20/51 | 42/51 |
| Python | 51/51 | 19/51 | 24/51 | 39/51 |
| Java | 70/70 | 35/70 | 35/70 | 52/70 |
| .NET | 70/70 (1 n/a) | 31/70 (1 n/a) | 31/70 (1 n/a) | 51/70 (1 n/a) |
| Rust | 47/47 (4 n/a) | 0/47 (4 n/a) | 0/47 (4 n/a) | 29/47 (4 n/a) |
| Score (/100) | 100 | 39.8 | 37.7 | 77.6 |

Recorded on 2026-11-12 with the 0.19.1 source
([results](sdk-scorecard/results/2026-11-12/summary.md)). A passing case
means an ordinary SDK workflow works, not that the API is complete. n/a marks a
case the SDK has no public API for; it is excluded from that SDK's denominator.

## Coming from fake-gcs-server?

gcs-mimic takes its everyday ergonomics from fake-gcs-server, so most container
setups move with a few changed lines: replace the image and mount a new data
volume at `/var/lib/gcs-mimic`. Most command-line flags and the `./data:/data`
mount keep working. Since 0.15.0, `FAKE_GCS_*` variables are not read, so
set the matching flag or `GCS_MIMIC_*` variable instead.

The server reads `GCS_MIMIC_*`; every other program in this project reads `GCSM_*`.
For 0.18.0, rename client, sync, Go subprocess and Compose variables to `GCSM_*`;
see the [migration table](docs/en/releases/0.18.0.md#environment-variable-names).
Unknown `GCS_MIMIC_*` names still stop startup; `GCSM_*` names can share the server environment.

It is not a drop-in replacement for every setup. Check these first:

- **Objects in your `/storage` volume stay behind.** The format differs, and only
  names and bytes can be copied across today. Generations and custom metadata
  are lost.
- **The image has no shell and does not run as root.** `docker exec … sh`, shell
  healthchecks and shell entrypoints stop working.
- **Go tests that embed `fakestorage` in-process** move to a subprocess facade
  that covers a subset of the API.
- **Testcontainers modules and wrappers that hard-code the `fake-gcs-server`
  binary** need an adapter.
- **Listing buckets needs `?project=`,** as on Cloud Storage. SDKs send it
  already; add it to your `curl` commands.

The [migration guide](docs/en/migrate-from-fake-gcs-server.md) has the swap, the
full list and what to do instead.

## More information

- [Docker usage](docs/en/container-usage.md), [SDK setup](docs/en/sdk-setup.md)
  and [Rails Active Storage](docs/en/rails-active-storage.md)
- [Build from source and API walkthroughs](docs/en/getting-started.md),
  [local HTTPS](docs/en/local-https.md), [signed URLs](docs/en/signed-urls.md)
  and the [native binary](docs/en/native-binary.md)
- [Moving from fake-gcs-server](docs/en/migrate-from-fake-gcs-server.md) and the
  [full migration reference](docs/en/fake-gcs-server-migration.md)
- [Storage design](docs/en/storage-backends.md), [architecture](docs/en/design.md)
  and [roadmap](docs/en/roadmap.md)
- [Development](docs/en/development.md) and [release policy](docs/en/release-policy.md)
- [0.20.1 release notes](docs/en/releases/0.20.1.md) and [all releases](https://github.com/shiroemons/gcs-mimic/releases)
- [llms.txt](llms.txt), a short guide for coding agents wiring gcs-mimic into a project

## License

Copyright (c) 2026 shiroemons

[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE).
Third-party material retains its own license.
This is an independent project and is not an official Google product.
