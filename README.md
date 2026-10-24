# gcs-mimic

[日本語ドキュメント](docs/ja/README.md)

A local Google Cloud Storage emulator for development and CI, written in Rust.
Point your application at it to create buckets and upload, download or delete
objects without a Google Cloud account.

## Quick start

Requires Docker with Compose and curl. No checkout, Rust toolchain or certificate
setup is needed for this HTTP example.

**Image publication is pending.** The example targets the planned first image,
`ghcr.io/shiroemons/gcs-mimic:0.1.0`; it cannot be pulled yet.
The [0.1.0 release notes](docs/en/releases/0.1.0.md) describe the selected scope;
the [preparation evidence](docs/en/evidence/dist-release-notes.md#remaining-gates)
records the remaining publication gates. To run today, follow the
[source-build guide](docs/en/getting-started.md).

Save this as `compose.yaml`:

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.1.0
    ports:
      - "127.0.0.1:4443:4443"
    environment:
      GCS_MIMIC_SCHEME: http
      GCS_MIMIC_API_BASE_URL: http://localhost:4443
    volumes:
      - gcs-data:/var/lib/gcs-mimic
    healthcheck:
      test: [CMD, gcs-mimic, healthcheck, --url, "http://127.0.0.1:4443/_internal/ready"]
volumes:
  gcs-data:
```

Start the server, create a bucket, upload a file and read it:

```sh
docker compose up -d --wait
curl --fail \
  -H 'Content-Type: application/json' -d '{"name":"demo-bucket"}' \
  'http://localhost:4443/storage/v1/b?project=demo-project'
curl --fail --data-binary 'hello' \
  'http://localhost:4443/upload/storage/v1/b/demo-bucket/o?uploadType=media&name=hello.txt'
curl --fail 'http://localhost:4443/storage/v1/b/demo-bucket/o/hello.txt?alt=media'
```

The last command prints `hello`. This local example accepts unauthenticated
requests. Stop with `docker compose down`; the volume retains your data.
See [Docker usage](docs/en/container-usage.md) for `docker run`, application
connections and configuration, or [HTTPS setup](docs/en/getting-started.md#local-https-with-an-explicit-development-ca)
when you need TLS.

Already have fake-gcs-server fixtures? Use the same `data/BUCKET/OBJECT` layout;
see [loading local files](docs/en/container-usage.md#load-local-files).

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
| API writes visible as bucket/object files | ❌ | ✅ Filesystem | ✅ Disk |
| Load bucket/object seed bytes | ✅ Once | ✅ Startup or reseed | ❌ |
| Default CORS for JSON API requests | ✅ | ✅ | ❌ |
| Enforce bucket-specific XML CORS rules | ✅ | ❌ | ❌ |
| Upload/download with signed URLs | ✅ | ✅ | ✅ |
| Verify V4 RSA signatures and expiry | ✅ | ❌ | ❌ |
| gRPC | ❌ | ◐ Storage v1 subset | ❌ |
| Published Docker image | ❌ | ✅ | ✅ |

Comparison baseline: fake-gcs-server `v1.56.1` and gcp-storage-emulator
`v2026.07.19`; gcs-mimic is unreleased `0.1.0`.
Seed loading excludes migration of existing backend metadata and history.
See the [detailed comparison](docs/en/feature-comparison.md) for criteria,
sources and verification limits, and the generated
[feature matrix](docs/en/feature-matrix.md) for implementation status.

## Storage and configuration

- Multiple buckets can have separate CORS rules, IAM policies, labels and
  supported versioning settings, managed through the API. Bucket CORS applies
  to XML requests; JSON uses fixed defaults.
- Data persists in embedded SQLite metadata and separate object files.
  No database server is required. Internal filenames are not object names.
- A `bucket/object` seed directory can be imported once. Live directory sync
  and a file for initial per-bucket settings are not implemented yet.
- Existing emulator backend metadata/history import is not implemented.
  `FAKE_GCS_*` environment variables are silently ignored in 0.1.0; use the
  documented `GCS_MIMIC_*` equivalents.

For existing fake-gcs-server setups, see the
[option mapping and migration guide](docs/en/fake-gcs-server-migration.md#startup-options-and-current-equivalents).

## More information

- [Detailed feature comparison](docs/en/feature-comparison.md)
- [Generated implementation matrix](docs/en/feature-matrix.md)
- [Docker usage](docs/en/container-usage.md)
- [Setup, configuration and examples](docs/en/getting-started.md)
- [fake-gcs-server storage investigation](docs/en/fake-gcs-server-storage.md)
- [Storage design](docs/en/storage-backends.md) and [architecture](docs/en/design.md)
- [Roadmap](docs/en/roadmap.md)
- [Development and checks](docs/en/development.md)
- [Release policy](docs/en/release-policy.md)
- [0.1.0 release notes and known limitations](docs/en/releases/0.1.0.md)

## License

Copyright (c) 2026 shiroemons

[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE).
Third-party material retains its own license.
This is an independent project and is not an official Google product.
