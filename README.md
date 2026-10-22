# gcs-mimic

A Google Cloud Storage emulator in Rust for local development and CI.

The current implementation provides persistent JSON bucket create/get/list/delete, bucket IAM policy get/set and streaming object media/multipart upload, metadata get/patch/update, prefix/delimiter listing with keyset paging, whole-object and single-range download, delete and initial seed directories, with a bounded SQLite catalog, opaque disk blobs, health/readiness checks and graceful shutdown. The default permissive profile enforces no authentication; use an isolated local development environment. An optional strict profile checks local bearer tokens against project and bucket grants of predefined storage roles.

## Implementation status

| Capability | Status | Current scope |
| --- | --- | --- |
| Standalone binary | ✅ Available | Build and run from source; no Rust installation is needed at runtime. |
| Prebuilt binary distribution | 🚧 Partial | CI is configured for macOS/Windows ARM64 and x86-64, plus Linux GNU/musl on both CPUs. All-platform execution and public release are pending. |
| Docker and Compose | 🚧 Partial | Non-root runtime; native amd64/arm64 Foundation CI passed. GHCR publication is configured but has not run. |
| CLI, environment and JSON configuration | ✅ Available | Command-line options override environment variables, configuration files and defaults. `gcs-mimic url public` builds encoded public object URLs without reading configuration. |
| Health, readiness and graceful shutdown | 🚧 Partial | Catalog locking/recovery and configured initial seeding complete before readiness; SIGINT/SIGTERM drain works. Seeded Compose startup is verified on ARM64 and emulated AMD64. |
| HTTP and TLS | 🚧 Partial | HTTP/1 is available; TLS is not implemented. |
| JSON bucket APIs | 🚧 Partial | Create/get/list/delete, metadata, scoped keyset paging and conditions; persistent IAM policy get/set with etag checks and scoped enforcement. ACLs, testIamPermissions and advanced settings are unavailable. |
| JSON object APIs | 🚧 Partial | Media/multipart upload with metadata and MD5/CRC32C, metadata/whole-media get, single byte ranges and delete, prefix/delimiter/offset listing with keyset paging, metadata patch/update and atomic conditions. Resumable, If-Range, transcoding, glob/version listing and ACL/hold/retention metadata are unavailable. |
| XML and gRPC APIs | ❌ Unavailable | Unsupported routes return 404 without reading request bodies. |
| Persistence | 🚧 Partial | Transactional catalog, opaque blobs, exclusive directory lock, restart/crash recovery and initial seed directory import. Individual fsync crash windows and power loss remain unverified. |
| Authentication, IAM and ACLs | 🚧 Partial | Explicit permissive (default) and strict profiles. Strict mode resolves local bearer tokens from a JSON identity registry and combines project grants with unconditional bucket policy bindings from a pinned `roles/storage.*` snapshot on JSON bucket/object routes. Object IAM, testIamPermissions, conditional/custom/basic roles, ACLs and public grants are unavailable. |
| Public and signed URLs | 🚧 Partial | `gcs-mimic url public` builds encoded path-style URLs without changing permissions. The server does not serve them yet (404); public grants, XML reads and signed URLs are unavailable. |
| Browser CORS | ❌ Unavailable | JSON and XML CORS behavior is not implemented. |
| Official SDK workflows | 🚧 Partial | Go 1.27.3, storage v1.70.0 and API v0.307.0 for JSON bucket CRUD/paging, IAM Policy/SetPolicy, single-request Writer (`ChunkSize=0`), Attrs/NewReader/NewRangeReader/Delete, Objects iterators and conditional Update, plus strict-mode project and bucket grants, etag rejection and revocation. Upload tokens require generation-match; replay and resumable Writer are unsupported. Ruby and other SDKs remain pending. |

See the [detailed compatibility matrix](docs/en/feature-matrix.md) for supported scope and verification status.

### Delivered workflows and next capabilities

The workflows below have executed evidence for their stated scope. They do not
promote their broader API rows to `IMPLEMENTED`: unresolved fields, permissions
and protocol differences remain in the same feature ledger.

| Workflow available in the documented scope | Main remaining boundary | Evidence |
| --- | --- | --- |
| Persistent JSON bucket/object round trip with checksums and atomic generation conditions | Advanced fields, full IAM/ACL and complete wire parity | [Media](docs/en/evidence/object-media.md) |
| Prefix/delimiter/keyset listing and metadata PATCH/PUT | History, glob/context filters, full projection and legacy-input parity | [Listing and metadata](docs/en/evidence/object-list-metadata.md) |
| Bounded JSON multipart upload with Go `ChunkSize=0` Writer | MIME edge parity, response replay and resumable uploads | [Multipart](docs/en/evidence/multipart.md) |
| Strict local tokens and project-level predefined storage roles | Public reads and full strict authentication | [Authentication](docs/en/evidence/strict-auth.md) |
| Bucket IAM policy get/set and enforcement with project-plus-bucket grants | Conditions, public grants, custom/basic roles, ACLs and testIamPermissions | [Runnable guide](docs/en/getting-started.md#grant-and-revoke-bucket-only-access) and [Go coverage](sdk-tests/go/README.md#strict-authentication-suite) |
| Single closed/open/suffix stored-byte ranges with pinned generation | If-Range, transcoding and exact fallback/header parity | [Range](docs/en/evidence/object-range.md) |
| Native initial seed import before readiness without replay over changes | Fixed octet-stream MIME; manifest metadata and legacy seed defaults remain separate work | [Seed](docs/en/evidence/seed-platform.md) |

Seed platform verification is complete: Linux rejects invalid-byte filenames;
Compose preserves user mutations and deletions with read-only fixtures, and
restores fixtures only after an explicit fresh-volume reset. UX-002 and UX-009
are IMPLEMENTED for their declared scope. ARM64 execution is native; AMD64 uses
emulation.

Next, implement public grants and XML GET/HEAD.
Continue with resumable uploads, V4 signed GET
then PUT, copy then compose, browser CORS and Ruby/Rails journeys. Add applicable
Ruby checks as each capability lands. Distribution verification has separate
release gates and publication authorization.

[The delivery roadmap](docs/en/roadmap.md) and the ledger's `delivery_plan` define
the ordered capabilities, prerequisites and completion conditions. P0-P7 remain
coverage groups; finishing a scope does not require every later extension in its
broad API row. Planning metadata adds no new implementation claim, and PARTIAL
is not assigned an arbitrary percentage.

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
| Byte-range download | 🚧 Single closed/open/suffix range; no If-Range or transcoding | ✅ | 🚧 Single range; explicit start required |
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
| Bucket IAM policy get/set and enforcement | 🚧 Persistent unconditional predefined-role bindings, strict project-plus-bucket grants and atomic etag checks; no conditions, public grants, ACLs or testIamPermissions | ❌ No bucket IAM get/set routes or enforcement | 🚧 Stored policies; no enforcement |
| Authentication / permission enforcement | 🚧 Strict profile: local bearer tokens and project-plus-bucket predefined storage roles on JSON routes | ❌ | ❌ |
| Public URL helper | 🚧 CLI builds encoded URLs; not served yet | ❌ | ❌ |
| Signed-URL GET/PUT requests | ❌ | 🚧 Accepted without signature/expiry checks | 🚧 Accepted without signature/expiry checks |
| Signed-URL signature / expiry validation | ❌ | ❌ | ❌ |

An ACL value, stored IAM policy, accepted signed URL or CORS response does not
establish access control.

The bucket IAM comparison uses the same get/set-and-enforcement scope for every
product. In fake-gcs-server
[`v1.56.1` route registration](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/server.go#L234-L340),
bucket ACL routes exist but `/b/{bucket}/iam` GET/PUT routes do not. Its
[object handlers](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/object.go)
access the backend without evaluating caller IAM grants. This is source review,
not a differential execution result.

### 🛠️ Local development and distribution

| Capability | gcs-mimic | fake-gcs-server | gcp-storage-emulator |
| --- | --- | --- | --- |
| Runtime / embedded use | Rust binary | Go binary or Go test library | Python CLI or Python test server |
| Disk persistence | 🚧 SQLite catalog and opaque blobs; documented recovery limits | ✅ Filesystem backend | ✅ Files and JSON metadata |
| Optional in-memory backend | ❌ | ✅ | ✅ |
| Fixture import | ✅ Native read-only initial seed before readiness; no replay; Linux and Compose lifecycle verified | ✅ Seed directory / initial objects | 🚧 Default bucket creation only |
| Published native binaries | 🚧 Eight-target CI configured; no release yet | ✅ macOS/Linux amd64+arm64; Windows amd64 | ❌ Python runtime required |
| Published Docker image | 🚧 Source build; GHCR workflow configured | ✅ Docker Hub | ✅ Docker Hub |
| Project license | MIT OR Apache-2.0 | BSD-2-Clause | BSD-3-Clause |

Disk persistence does not imply equivalent crash or power-loss guarantees.
CPU, memory, throughput and image size are not ranked: no equivalent cross-product
benchmark was run.

gcs-mimic stores metadata in SQLite and object bytes in opaque immutable files.
The storage investigation in `docs/en/storage-backends.md` compares filesystem,
xattr/JSON, memory, SQLite BLOB and embedded key/value approaches. Transactional
metadata plus streamed opaque files is the recommended durable default; this is
a requirement-based decision, with no comparative benchmark claim.

The product target is a complete behavioral superset of fake-gcs-server after
renaming `FAKE_GCS_*` to `GCS_MIMIC_*` and explicitly converting existing data.
Legacy keys will not be aliases or fallback inputs. The migration contract in
`docs/en/fake-gcs-server-migration.md` includes flags, TLS, endpoints, Go
embedding, Storage v1 gRPC, helper/event behavior and both source backends.
Lossless import, equivalent renamed settings and the legacy compatibility
profile remain planned. Existing comparison marks describe current behavior.

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

To require authentication, add `compose.strict.yaml`. It selects
`auth_mode=strict` and mounts the local fixture registry `config/identities.json`,
whose admin, object-viewer, object-creator and bucket-reader tokens are public emulator test
values, not Google credentials. Send them as `Authorization: Bearer <token>`;
anonymous requests return 401 and missing permissions return 403. Only
project and bucket grants of predefined storage roles are enforced. The
bucket-reader starts without project roles; follow the
[bucket-only grant walkthrough](docs/en/getting-started.md#grant-and-revoke-bucket-only-access)
to grant and revoke object reads on one bucket.

```sh
COMPOSE_PROJECT_NAME=gcs-mimic-bucket-demo GCS_MIMIC_PORT=0 \
  docker compose -f compose.yaml -f compose.strict.yaml up -d --wait
```

`gcs-mimic url public --base-url URL --bucket NAME --object NAME` prints an encoded
path-style object URL and changes no permission. The emulator does not serve that
URL yet; public grants and XML reads are planned for the next slice.

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

Object APIs support the documented [media](docs/en/slices/json-object-media.md), [multipart](docs/en/slices/json-object-multipart.md), [range](docs/en/slices/json-object-range.md), [listing](docs/en/slices/json-object-list.md) and [metadata](docs/en/slices/json-object-metadata.md) subsets. Resumable uploads, If-Range, transcoding, version history, object IAM and bucket/object ACLs remain unavailable. Single untranscoded closed/open/suffix ranges return 206; unsatisfiable ranges return 416. Malformed, multiple and repeated ranges use a documented local 200 fallback. Listing rejects nonempty `matchGlob`/`filter`, `versions=true`, soft-deleted and folder-prefix options with 501. Metadata patch/update covers content headers and custom metadata; unsupported holds/retention/ACL settings return 501. JSON media returns `Cache-Control: no-cache` because public access is not implemented. Go SDK `ChunkSize=0` Writer and JSON `NewRangeReader` work for the scoped subset. Ordinary media GET returns a direct 200; accepted GET/DELETE/update retry tokens are not deduplicated (SPEC-008..013, SPEC-015 and the listing/range gaps record remaining contract gaps). Objects default to a configurable 1 GiB cap; bounded streaming is functionally tested, without RSS/cgroup/CPU benchmarks or a 1 GiB constrained-container claim.

[Regression evidence](docs/en/evidence/object-media.md) covers upload admission, advisory checks, explicit upload EOF, committed outcomes and recovery. Transfers have a configurable 15-minute absolute ceiling in addition to body inactivity; advisory and post-EOF waits default to 30 seconds and return 503 on expiry. After a post-EOF timeout the mutation outcome is unknown: re-read state or retry with preconditions, because the commit may still complete. Filesystem work and shutdown catalog close have no separate execution bound. Unreleased schema-v2 catalogs are rejected; object-free v1 catalogs and v3/v4 catalogs migrate to v5, including stored object metadata and the seed initialization marker. Migrated catalogs do not import fixtures.

Initial [seed directory import](docs/en/slices/seed.md) runs before readiness and does not replay fixtures over existing data. Bucket IAM supports unconditional predefined-role bindings and etag concurrency; object IAM, testIamPermissions, ACLs, public grants, conditions, custom/basic roles, signed URLs, CORS and TLS are unavailable. Strict mode combines project and bucket grants; see the [strict authentication contract](docs/en/slices/strict-auth.md) and SPEC-017 for local error and missing-bucket rules. Bucket APIs support only the [documented subset](docs/en/slices/json-bucket-crud.md); `full` projection omits ACL/owner, project IDs are locally simulated, and recognized unsupported settings return 501. Native data defaults to cwd-relative `data`; `--data-dir`, `--api-base-url` and `--max-object-bytes` are explicit overrides. Host power-loss durability and Ruby/Rails remain unverified.

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
