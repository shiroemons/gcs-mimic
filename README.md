# gcs-mimic

A Google Cloud Storage emulator in Rust for local development and CI.

The current implementation provides persistent JSON bucket create/get/list/delete and streaming object media/multipart upload, metadata get/patch/update, prefix/delimiter listing with keyset paging, whole-object download and delete, with a bounded SQLite catalog, opaque disk blobs, health/readiness checks and graceful shutdown. Authentication and permissions are not enforced; use an isolated local development environment.

## Implementation status

| Capability | Status | Current scope |
| --- | --- | --- |
| Standalone binary | ✅ Available | Build and run from source; no Rust installation is needed at runtime. |
| Prebuilt binary distribution | 🚧 Partial | CI is configured for macOS/Windows ARM64 and x86-64, plus Linux GNU/musl on both CPUs. All-platform execution and public release are pending. |
| Docker and Compose | 🚧 Partial | Non-root runtime; native amd64/arm64 Foundation CI passed. GHCR publication is configured but has not run. |
| CLI, environment and JSON configuration | ✅ Available | Command-line options override environment variables, configuration files and defaults. |
| Health, readiness and graceful shutdown | 🚧 Partial | Catalog locking/recovery completes before readiness; SIGINT/SIGTERM drain works. Fixture initialization is unavailable. |
| HTTP and TLS | 🚧 Partial | HTTP/1 is available; TLS is not implemented. |
| JSON bucket APIs | 🚧 Partial | Create/get/list/delete, metadata, scoped keyset paging and conditions; ACL/IAM and advanced settings are unavailable. |
| JSON object APIs | 🚧 Partial | Media/multipart upload with fixed/custom metadata and MD5/CRC32C, metadata/whole-media get and delete, prefix/delimiter/offset listing with keyset paging, metadata patch/update, atomic generation/metageneration conditions and nonempty bucket protection. Resumable, ranges, glob/version listing and ACL/hold/retention metadata are unavailable. |
| XML and gRPC APIs | ❌ Unavailable | Unsupported routes return 404 without reading request bodies. |
| Persistence | 🚧 Partial | Transactional bucket/object catalog, opaque blobs, exclusive directory lock and restart/crash recovery. Individual fsync crash windows, power loss and fixtures remain unverified or unavailable. |
| Authentication, IAM and ACLs | ❌ Unavailable | Neither permissive nor strict storage authentication is implemented. |
| Public and signed URLs | ❌ Unavailable | No URL helpers, signing or signature verification. |
| Browser CORS | ❌ Unavailable | JSON and XML CORS behavior is not implemented. |
| Official SDK workflows | 🚧 Partial | Go 1.27.3, storage v1.69.0 and API v0.307.0 verified for JSON bucket CRUD/paging, single-request Writer (`ChunkSize=0`), Attrs/NewReader/Delete, Objects iterators and conditional Update. Upload tokens require generation-match; replay and resumable Writer are unsupported. Ruby and other SDKs remain pending. |

See the [detailed compatibility matrix](docs/en/feature-matrix.md) for supported scope and verification status.

## Feature comparison

Snapshot reviewed on **2026-10-22**: gcs-mimic's unreleased `0.1.0`
compared with fake-gcs-server `v1.56.1` and gcp-storage-emulator `v2026.07.19`.
Planned features are not counted.

**Legend:** ✅ implementation available for the named operation;
🚧 partial implementation or an explicit limitation; ❌ unavailable.
Cells describe the reviewed scope, not complete Google API compatibility.
gcs-mimic's status follows the project's feature ledger.
The other products were reviewed through their documentation, source and tests;
their tests were not run as part of this comparison.

### 🧩 JSON API operations

| Operation | gcs-mimic | fake-gcs-server | gcp-storage-emulator |
| --- | --- | --- | --- |
| Bucket create (`buckets.insert`) | 🚧 Validated metadata subset | ✅ Implemented | ✅ Implemented |
| Bucket metadata GET (`buckets.get`) | 🚧 Modeled fields; no ACL/owner | ✅ Implemented | ✅ Implemented |
| Bucket list (`buckets.list`) | 🚧 Project/prefix filters and keyset paging | ✅ Prefix and paging | 🚧 All buckets; no paging |
| Bucket delete (`buckets.delete`) | 🚧 Empty buckets; metageneration checks | ✅ Implemented | ✅ Implemented |
| Bucket metadata PATCH (`buckets.patch`) | ❌ | 🚧 Versioning/default event-based hold fields | 🚧 Writable metadata subset |
| Bucket metadata PUT (`buckets.update`) | ❌ | ❌ | ❌ |
| Raw media upload | 🚧 Single-request media subset | ✅ | ✅ |
| JSON multipart upload | 🚧 Bounded metadata/media; optional checksum part | ✅ | ✅ |
| Resumable upload | ❌ | ✅ Sessions and chunk uploads | 🚧 Chunk uploads and status queries |
| Object metadata GET (`objects.get`) | 🚧 Current live generation only | ✅ | ✅ |
| Whole-object media download | 🚧 Whole object; direct HTTP 200 | ✅ | ✅ |
| Byte-range download | ❌ | ✅ | 🚧 Single range; explicit start required |
| Object listing (`objects.list`) | 🚧 Prefix/delimiter/offsets; no glob, versions or soft-deleted | ✅ Prefix/delimiter/offsets | 🚧 Prefix/delimiter/glob filters |
| Object list pagination | 🚧 `maxResults` / query-bound `pageToken`; bounded keyset seeks | ✅ `maxResults` / `pageToken` | ❌ |
| Object metadata PATCH (`objects.patch`) | 🚧 Content/custom metadata merge; atomic conditions; no ACL | 🚧 Metadata/content/ACL subset | 🚧 Writable metadata subset |
| Object metadata PUT (`objects.update`) | 🚧 Content/custom metadata replacement; atomic conditions; no ACL | 🚧 Metadata/content/ACL subset | ❌ |
| Object delete (`objects.delete`) | 🚧 Live generation; atomic conditions | ✅ Current or selected generation | ✅ Local soft delete by default |
| Object copy (`objects.copy`) | ❌ | ✅ | ✅ |
| Object rewrite (`objects.rewrite`) | ❌ | 🚧 Immediate copy; no continuation | 🚧 Immediate completion; no continuation |
| Object compose (`objects.compose`) | ❌ | ✅ | ✅ |
| Retained object version history | ❌ One live generation | 🚧 Memory backend only | ❌ Versioning metadata is not history |
| Soft delete / restore | ❌ | ❌ | 🚧 Local deleted-object listing and restore |
| Generation/metageneration preconditions | 🚧 Atomic object upload/delete/metadata checks; scoped bucket checks | 🚧 Upload generation checks only | ❌ Not evaluated |
| MD5 and CRC32C calculation | 🚧 Media/multipart metadata and download headers | ✅ | ✅ |
| Supplied MD5/CRC32C validation | 🚧 Media `X-Goog-Hash`; multipart metadata/final JSON; no `Content-MD5` | 🚧 Multipart/resumable metadata checks | 🚧 Multipart/resumable metadata checks |

### 🔐 Protocols and access

| Capability | gcs-mimic | fake-gcs-server | gcp-storage-emulator |
| --- | --- | --- | --- |
| XML-style object endpoints | ❌ | 🚧 Selected listing/download/upload routes | 🚧 Public-path GET/PUT only |
| gRPC API | ❌ | 🚧 Storage **v1** subset; not Storage v2 | ❌ |
| TLS listener | ❌ HTTP only | ✅ HTTPS, HTTP or both | ❌ HTTP only |
| Browser CORS | ❌ | 🚧 Wildcard CORS middleware | ❌ Stored CORS metadata only |
| ACL operations | ❌ | 🚧 Metadata operations; no enforcement | 🚧 Metadata/list operations; no enforcement |
| IAM policy operations | ❌ | ❌ | 🚧 Stored policies and permissive stubs |
| Authentication / permission enforcement | ❌ | ❌ | ❌ |
| Signed-URL GET/PUT requests | ❌ | 🚧 Accepted without signature/expiry checks | 🚧 Accepted without signature/expiry checks |
| Signed-URL signature / expiry validation | ❌ | ❌ | ❌ |

An ACL value, stored IAM policy, accepted signed URL or CORS response does not
establish access control.

### 🛠️ Local development and distribution

| Capability | gcs-mimic | fake-gcs-server | gcp-storage-emulator |
| --- | --- | --- | --- |
| Runtime / embedded use | Rust binary | Go binary or Go test library | Python CLI or Python test server |
| Disk persistence | 🚧 SQLite catalog and opaque blobs; documented recovery limits | ✅ Filesystem backend | ✅ Files and JSON metadata |
| Optional in-memory backend | ❌ | ✅ | ✅ |
| Fixture import | ❌ | ✅ Seed directory / initial objects | 🚧 Default bucket creation only |
| Published native binaries | 🚧 Eight-target CI configured; no release yet | ✅ macOS/Linux amd64+arm64; Windows amd64 | ❌ Python runtime required |
| Published Docker image | 🚧 Source build; GHCR workflow configured | ✅ Docker Hub | ✅ Docker Hub |
| Project license | MIT OR Apache-2.0 | BSD-2-Clause | BSD-3-Clause |

Disk persistence does not imply equivalent crash or power-loss guarantees.
CPU, memory, throughput and image size are not ranked: no equivalent cross-product
benchmark was run.

## Run with Docker

Requires Git, Docker Engine/Desktop with Buildx, and curl. Linux amd64 and arm64 are supported. Build from source; no container image is published.

```sh
git clone https://github.com/shiroemons/gcs-mimic.git
cd gcs-mimic
docker build -t gcs-mimic:dev .
COMPOSE_PROJECT_NAME=gcs-mimic-bucket-demo GCS_MIMIC_PORT=0 docker compose up -d --wait
COMPOSE_PROJECT_NAME=gcs-mimic-bucket-demo docker compose exec -T --interactive=false gcs \
  gcs-mimic healthcheck --url http://127.0.0.1:4443/_internal/ready
```

The service is exposed only on host loopback. Follow the [runnable guide](docs/en/getting-started.md) to discover the allocated port, set the advertised endpoint and exercise buckets and object media from host and peer containers.

```sh
COMPOSE_PROJECT_NAME=gcs-mimic-bucket-demo docker compose stop
```

The container runs as a non-root user on a read-only root, with a writable named data volume. Compose sets limits of 128 MiB memory, one CPU and 64 processes. These are configured limits, not measured resource requirements. Ordinary stop/down preserves the catalog; deliberate volume removal resets it.

The runtime uses digest-pinned Distroless Debian 13 with glibc/libgcc and a stripped
binary. It has no shell or package manager; use the built-in `healthcheck` command
and container logs for diagnostics. UID/GID `10001:10001` remain compatible with
existing data volumes. Project and third-party notices are included in the image,
along with the OS notices retained by Distroless. See the
[GHCR distribution procedure](docs/en/container-distribution.md) for future
version/minor/latest tags, digest pinning and first-publication setup.

A prior build comparison measured complete-image compressed layers decreasing from 30.57 to 12.14 MiB on
amd64 and from 30.92 to 12.38 MiB on arm64, about 60% in both cases. See the
[measurement and verification details](docs/en/evidence/distroless.md). These are
image-size results; runtime memory and CPU improvements have not been measured.

## Run the binary

Requires rustup to build; the resulting binary needs no Rust installation or Python to run.

The [native distribution foundation](docs/en/binary-distribution.md) defines eight
future release targets, archive contents, checksums and the manual/tag workflows.
No prebuilt release is available yet. Windows uses a separate persistence profile:
normal restart and process termination recovery are tested by its configured CI;
OS-crash and power-loss namespace durability are not guaranteed. All-platform CI
execution remains pending.

```sh
rustup toolchain install 1.99.1 --profile minimal
cargo +1.99.1 build --release --locked --bin gcs-mimic
./target/release/gcs-mimic serve --config config/development.json
```

Check readiness from another terminal, then stop with Ctrl-C:

```sh
./target/release/gcs-mimic healthcheck --url http://127.0.0.1:4443/_internal/ready
```

Configuration precedence is command-line options, `GCS_MIMIC_` environment variables, JSON configuration, then defaults. Use `--help` for options and limits. See the [build and run guide](docs/en/getting-started.md) for configuration, container connectivity and troubleshooting.

## Current limitations

Object APIs support only the documented [media](docs/en/slices/json-object-media.md), [listing](docs/en/slices/json-object-list.md) and [metadata](docs/en/slices/json-object-metadata.md) subsets: no multipart/resumable uploads, ranges, version history or ACL/IAM. Listing rejects nonempty `matchGlob`/`filter`, `versions=true`, soft-deleted and folder-prefix options with 501. Metadata patch/update covers content headers and custom metadata; unknown fields, changed read-only fields and holds/retention/ACL settings return 501, and JSON media always returns `Cache-Control: no-cache` because public access is not implemented. Go SDK Writer is unsupported. Ordinary media GET returns a direct 200; accepted GET/DELETE/update retry tokens are not deduplicated (SPEC-008..013, SPEC-015 and the listing gaps record remaining contract gaps). Objects default to a configurable 1 GiB cap; bounded streaming is functionally tested, without RSS/cgroup/CPU benchmarks or a 1 GiB constrained-container claim.

[Regression evidence](docs/en/evidence/object-media.md) covers upload admission, advisory checks, explicit upload EOF, committed outcomes and recovery. Transfers have a configurable 15-minute absolute ceiling in addition to body inactivity; advisory and post-EOF waits default to 30 seconds and return 503 on expiry. After a post-EOF timeout the mutation outcome is unknown: re-read state or retry with preconditions, because the commit may still complete. Filesystem work and shutdown catalog close have no separate execution bound. Unreleased schema-v2 catalogs are rejected; object-free v1 catalogs and v3 catalogs migrate to v4, which adds stored object metadata.

Fixture loading, authentication, signed URLs, CORS and TLS are not available. Bucket APIs support only the [documented subset](docs/en/slices/json-bucket-crud.md); `full` projection omits ACL/owner, project IDs are locally simulated, and recognized unsupported settings return 501. Native data defaults to cwd-relative `data`; `--data-dir`, `--api-base-url` and `--max-object-bytes` are explicit overrides. Host power-loss durability and Ruby/Rails remain unverified.

For contributors, install [just](https://github.com/casey/just#installation) and
run `just check` while editing, then `just pre-push` before pushing to run the
Rust, Go SDK and both Linux container architecture checks. See the
[development guide](docs/en/development.md) for prerequisites and CI limitations.

## Versioning and releases

The current `0.1.0` is an unreleased development version. During `0.x`, compatible
fixes increment patch; new functionality and breaking changes increment minor.
Breaking changes include documented CLI, configuration, API and persistent-data
compatibility changes and require release notes and migration guidance. Published
versions are immutable. See the [versioning and licensing policy](docs/en/release-policy.md)
for the full contract and first-release requirements.

The **Release binaries** workflow prepares verification artifacts on manual
dispatch. Future `vX.Y.Z` tags require a matching Cargo version, committed release
notes, freshness checks, the existing Foundation CI and all eight native suites
before publishing archives with SHA-256 checksums. See the
[release procedure and platform limits](docs/en/binary-distribution.md).

## License

Copyright (c) 2026 shiroemons

Original project code and documentation are dual-licensed under the
[MIT License](LICENSE-MIT) or the [Apache License, Version 2.0](LICENSE-APACHE),
at your option (`MIT OR Apache-2.0`). Third-party material retains its own license.

This is an independent project and is not an official Google product.
