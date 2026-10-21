# gcs-mimic

A Google Cloud Storage emulator in Rust for local development and CI.

The current implementation provides persistent JSON bucket create/get/list/delete and streaming object media upload, metadata get, whole-object download and delete, with a bounded SQLite catalog, opaque disk blobs, health/readiness checks and graceful shutdown. Authentication and permissions are not enforced; use an isolated local development environment.

## Implementation status

| Capability | Status | Current scope |
| --- | --- | --- |
| Standalone binary | ✅ Available | Build and run from source; no Rust installation is needed at runtime. |
| Docker and Compose | 🚧 Partial | Non-root runtime; arm64 tested natively, amd64 tested under emulation. Native amd64 verification is pending. |
| CLI, environment and JSON configuration | ✅ Available | Command-line options override environment variables, configuration files and defaults. |
| Health, readiness and graceful shutdown | 🚧 Partial | Catalog locking/recovery completes before readiness; SIGINT/SIGTERM drain works. Fixture initialization is unavailable. |
| HTTP and TLS | 🚧 Partial | HTTP/1 is available; TLS is not implemented. |
| JSON bucket APIs | 🚧 Partial | Create/get/list/delete, metadata, scoped keyset paging and conditions; ACL/IAM and advanced settings are unavailable. |
| JSON object APIs | 🚧 Partial | Media upload, metadata/whole-media get and delete; MD5/CRC32C, generation conditions and nonempty bucket protection. Multipart/resumable, ranges, listing and metadata mutation are unavailable. |
| XML and gRPC APIs | ❌ Unavailable | Unsupported routes return 404 without reading request bodies. |
| Persistence | 🚧 Partial | Transactional bucket/object catalog, opaque blobs, exclusive directory lock and restart/crash recovery. Individual fsync crash windows, power loss and fixtures remain unverified or unavailable. |
| Authentication, IAM and ACLs | ❌ Unavailable | Neither permissive nor strict storage authentication is implemented. |
| Public and signed URLs | ❌ Unavailable | No URL helpers, signing or signature verification. |
| Browser CORS | ❌ Unavailable | JSON and XML CORS behavior is not implemented. |
| Official SDK workflows | 🚧 Partial | Go 1.27.3, storage v1.69.0 and API v0.306.0 verified for JSON bucket CRUD/paging and object Attrs/NewReader/Delete after raw media upload. SDK Writer is rejected; Ruby and other SDKs remain pending. |

See the [detailed compatibility matrix](docs/en/feature-matrix.md) for supported scope and verification status.

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

## Run the binary

Requires rustup to build; the resulting binary needs no Rust installation or Python to run.

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

Object APIs support only the [media subset](docs/en/slices/json-object-media.md): no multipart/resumable uploads, ranges, version history, listing, metadata patch or ACL/IAM. Go SDK Writer is unsupported. Ordinary media GET returns a direct 200; accepted GET/DELETE retry tokens are not deduplicated (SPEC-008..013 record remaining contract gaps). Objects default to a configurable 1 GiB cap; bounded streaming is functionally tested, without RSS/cgroup/CPU benchmarks or a 1 GiB constrained-container claim.

[Regression evidence](docs/en/evidence/object-media.md) covers upload admission, advisory checks, explicit upload EOF, committed outcomes and recovery. Transfers have a configurable 15-minute absolute ceiling in addition to body inactivity; advisory and post-EOF waits default to 30 seconds and return 503 on expiry. After a post-EOF timeout the mutation outcome is unknown: re-read state or retry with preconditions, because the commit may still complete. Filesystem work and shutdown catalog close have no separate execution bound. Unreleased schema-v2 catalogs are rejected; object-free v1 catalogs migrate directly to v3.

Fixture loading, authentication, signed URLs, CORS and TLS are not available. Bucket APIs support only the [documented subset](docs/en/slices/json-bucket-crud.md); `full` projection omits ACL/owner, project IDs are locally simulated, and recognized unsupported settings return 501. Native data defaults to cwd-relative `data`; `--data-dir`, `--api-base-url` and `--max-object-bytes` are explicit overrides. Native amd64, host power-loss durability and Ruby/Rails remain unverified.

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

## License

Copyright (c) 2026 shiroemons

Original project code and documentation are dual-licensed under the
[MIT License](LICENSE-MIT) or the [Apache License, Version 2.0](LICENSE-APACHE),
at your option (`MIT OR Apache-2.0`). Third-party material retains its own license.

This is an independent project and is not an official Google product.
