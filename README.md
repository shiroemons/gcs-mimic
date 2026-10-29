# gcs-mimic

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

[日本語ドキュメント](docs/ja/README.md)

A local Google Cloud Storage emulator for development and CI, written in Rust.
Point your application at it to create buckets and upload, download or delete
objects without a Google Cloud account. Start it with one `docker run`, preload
fixtures by mounting a directory, and keep data with a volume.

## Quick start

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 ghcr.io/shiroemons/gcs-mimic:0.5.0
curl -k 'https://localhost:4443/storage/v1/b?project=local-project'
```

The server uses HTTPS by default with a built-in public development
certificate, so `curl -k` skips verification. The second command prints
`{"kind":"storage#buckets"}`. Bucket listing requires `?project=`, as in Google
Cloud Storage; the error message says so when it is missing.

SDKs are easiest to point at plain HTTP. Replace the container with an HTTP one,
then create a bucket, upload a file and read it:

```sh
docker rm -f gcs-mimic
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  -e GCS_MIMIC_SCHEME=http ghcr.io/shiroemons/gcs-mimic:0.5.0
curl --fail \
  -H 'Content-Type: application/json' -d '{"name":"demo-bucket"}' \
  'http://localhost:4443/storage/v1/b?project=demo-project'
curl --fail --data-binary 'hello' \
  'http://localhost:4443/upload/storage/v1/b/demo-bucket/o?uploadType=media&name=hello.txt'
curl --fail 'http://localhost:4443/storage/v1/b/demo-bucket/o/hello.txt?alt=media'
```

The last command prints `hello`. The examples publish the port on `127.0.0.1`
only, because the default profile accepts unauthenticated requests. Quote URLs
so that shells such as zsh do not expand `?`. Data stays in the container until
it is removed; add `-v gcs-mimic-data:/var/lib/gcs-mimic` to keep it.

The image is published for `linux/amd64` and `linux/arm64` on
[GHCR](https://github.com/shiroemons/gcs-mimic/pkgs/container/gcs-mimic) and
[Docker Hub](https://hub.docker.com/r/shiroemons/gcs-mimic) with the same
digest, so either name works in every example. The quick start from Docker Hub:

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 shiroemons/gcs-mimic:0.5.0
curl -k 'https://localhost:4443/storage/v1/b?project=local-project'
```

In Compose, use `image: shiroemons/gcs-mimic:0.5.0`.

See the
[0.5.0 release notes](docs/en/releases/0.5.0.md), the
[0.1.0 features and known limitations](docs/en/releases/0.1.0.md), or the
[source-build guide](docs/en/getting-started.md).

### Compose

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.5.0
    ports:
      - "127.0.0.1:4443:4443"
    environment:
      GCS_MIMIC_SCHEME: http
    volumes:
      - gcs-data:/var/lib/gcs-mimic
volumes:
  gcs-data:
```

`docker compose up -d --wait` waits for the image healthcheck. The curl commands
above work unchanged, and `docker compose down` keeps the named volume.

## Preload files

Mount a directory at `/data` with the same `BUCKET/OBJECT` layout as
fake-gcs-server:

```text
data/
  sample-bucket/
    some_file.txt
  empty-bucket/
```

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  -e GCS_MIMIC_SCHEME=http \
  -v "$PWD/data:/data:ro" ghcr.io/shiroemons/gcs-mimic:0.5.0
curl 'http://localhost:4443/storage/v1/b?project=local-project'
curl 'http://localhost:4443/storage/v1/b/sample-bucket/o'
```

Each top-level directory becomes a bucket owned by `local-project`, and files
become objects. Like fake-gcs-server, the files are applied on every startup
and win over API changes at the same names, while other objects are kept.
See [loading local files](docs/en/container-usage.md#load-local-files) for the
exact rules and the differences from fake-gcs-server.

## Connect your application

| Caller | Endpoint |
| --- | --- |
| Application or browser on the host | `http://localhost:4443` |
| Application on the same Compose network | `http://gcs:4443`, with `GCS_MIMIC_API_BASE_URL: http://gcs:4443` on the emulator |

The emulator advertises one API base URL in generated links such as resumable
upload `Location`. It defaults to `http://127.0.0.1:4443` when the host and
container ports are both 4443; set `GCS_MIMIC_API_BASE_URL` when the host port
differs or peers use another hostname.

These connection forms are exercised by the repository's pinned SDK tests.
Go (`cloud.google.com/go/storage` v1.70.0, [tests](sdk-tests/go/README.md)):

```go
client, err := storage.NewClient(ctx,
    option.WithEndpoint("http://localhost:4443/storage/v1/"),
    option.WithoutAuthentication())
```

Ruby (`google-cloud-storage` 1.62.1, [tests](sdk-tests/ruby/README.md)):

```ruby
storage = Google::Cloud::Storage.anonymous(endpoint: "http://localhost:4443/")
```

Rails Active Storage uses an ordinary `service: GCS` entry with
`endpoint: http://localhost:4443/` (keep the trailing slash). Default credential
handling still contacts Google OAuth, so follow the
[local credential recipe](docs/en/slices/sdk-local-auth-softdelete-notes.md)
and the [scenario limits](docs/en/slices/sdk-active-storage-scenario.md).
Go gRPC clients use `STORAGE_EMULATOR_HOST_GRPC` with the optional gRPC listener
below. Node.js, PHP and Python use the explicit public endpoint and credential
options documented in the [five-SDK scorecard](sdk-scorecard/README.md).
Its permissive comparison scope does not establish arbitrary SDK configuration
or strict authentication support. See [Docker usage](docs/en/container-usage.md#connect-an-application)
and the [SDK and authentication guide](docs/en/getting-started.md) for more.

## Common settings

| Setting | Purpose |
| --- | --- |
| `GCS_MIMIC_SCHEME` | `https` (default), `http` or `both` |
| `GCS_MIMIC_API_BASE_URL` | Address placed in generated links; see above |
| `-v NAME:/var/lib/gcs-mimic` | Keep buckets and objects across container replacement |
| `GCS_MIMIC_GRPC_LISTEN` | Optional plaintext gRPC listener, for example `0.0.0.0:4444` with `-p 127.0.0.1:4444:4444` |
| `GCS_MIMIC_AUTH_MODE` | `strict` checks local bearer tokens; see [strict authentication](docs/en/getting-started.md#strict-local-authentication) |
| `GCS_MIMIC_LOG_LEVEL` | `debug`, `info` (default), `warn` or `error` for JSON stderr events |

List every option with:

```sh
docker run --rm ghcr.io/shiroemons/gcs-mimic:0.5.0 --help
```

Moving from fake-gcs-server? Its flags and the 19 known `FAKE_GCS_*` variables
are rejected with advice on the matching `GCS_MIMIC_*` setting, and unknown
`FAKE_GCS_*` names warn. `gcs-mimic import fake-gcs-server` converts live objects
from a stopped filesystem snapshot; archived history is not imported. See the
[migration guide](docs/en/fake-gcs-server-migration.md) and
[environment mapping](docs/en/fake-gcs-server-configuration.md).

## Features at a glance

Each row checks the same behavior for all three products.
✅ Available for that scope · ◐ Partly available · ❌ Unavailable.
These are workflow summaries, not claims of complete Google API compatibility.

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

The mirror row uses an optional `--sync-dir`
[directory](docs/en/container-usage.md#mirror-objects-to-a-host-directory)
beside the internal storage; external deletion is not imported. The gRPC rows
cover scoped subsets on standard buckets, and individual RPC rows in the feature
ledger remain `PARTIAL`; see the
[protocol comparison](docs/en/feature-comparison.md#-protocols-and-access) for
their limits.

Comparison baseline: fake-gcs-server `v1.56.1` and gcp-storage-emulator
`v2026.07.19`; gcs-mimic is `0.5.0`.
Seed loading excludes migration of existing backend metadata and history.
See the [detailed comparison](docs/en/feature-comparison.md) for criteria,
sources and verification limits, and the generated
[feature matrix](docs/en/feature-matrix.md) for implementation status.

### SDK scorecard

The [SDK scorecard](sdk-scorecard/README.md) runs the same workflow probes with
the official Cloud Storage SDKs against each emulator. Passed cases:

| SDK | gcs-mimic | fake-gcs-server | gcp-storage-emulator |
| --- | --- | --- | --- |
| Go | 57/57 | 24/57 | 20/57 |
| Ruby | 51/51 | 24/51 | 22/51 |
| Node.js | 51/51 | 25/51 | 22/51 |
| PHP | 51/51 | 25/51 | 20/51 |
| Python | 51/51 | 19/51 | 24/51 |
| Score (/100) | 100 | 44.9 | 41.5 |

Recorded on 2026-10-29 at gcs-mimic commit `e7fb384` over shared HTTPS with
permissive authentication; see the
[full results](sdk-scorecard/results/2026-10-29-5/summary.md). A passing probe
shows that an ordinary SDK workflow works; it does not mark the underlying API
method complete, and cross-SDK weighting is provisional.

## More information

- [Docker usage](docs/en/container-usage.md): seed files, sync directory,
  bucket initialization file and configuration
- [Setup, configuration and examples](docs/en/getting-started.md), with separate
  guides for [local HTTPS](docs/en/local-https.md),
  [signed URLs](docs/en/signed-urls.md) and the
  [native binary](docs/en/native-binary.md)
- [Detailed feature comparison](docs/en/feature-comparison.md)
- [Generated implementation matrix](docs/en/feature-matrix.md)
- [fake-gcs-server migration guide](docs/en/fake-gcs-server-migration.md) and
  [storage investigation](docs/en/fake-gcs-server-storage.md)
- [Storage design](docs/en/storage-backends.md) and [architecture](docs/en/design.md)
- [Roadmap](docs/en/roadmap.md)
- [Development and checks](docs/en/development.md)
- [Release policy](docs/en/release-policy.md)
- [0.5.0 release notes](docs/en/releases/0.5.0.md) and
  [0.1.0 features and known limitations](docs/en/releases/0.1.0.md)

## License

Copyright (c) 2026 shiroemons

[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE).
Third-party material retains its own license.
This is an independent project and is not an official Google product.
