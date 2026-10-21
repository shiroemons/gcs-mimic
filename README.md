# gcs-mimic

A Google Cloud Storage emulator in Rust for local development and CI.

The current implementation provides persistent JSON bucket create/get/list/delete, a bounded SQLite catalog, health/readiness checks and graceful shutdown. **Object APIs are not available yet.** Authentication and permissions are not enforced; use an isolated local development environment.

## Implementation status

| Capability | Status | Current scope |
| --- | --- | --- |
| Standalone binary | ✅ Available | Build and run from source; no Rust installation is needed at runtime. |
| Docker and Compose | 🚧 Partial | Non-root runtime; arm64 tested natively, amd64 tested under emulation. Native amd64 verification is pending. |
| CLI, environment and JSON configuration | ✅ Available | Command-line options override environment variables, configuration files and defaults. |
| Health, readiness and graceful shutdown | 🚧 Partial | Catalog locking/recovery completes before readiness; SIGINT/SIGTERM drain works. Fixture initialization is unavailable. |
| HTTP and TLS | 🚧 Partial | HTTP/1 is available; TLS is not implemented. |
| JSON bucket APIs | 🚧 Partial | Create/get/list/delete, metadata, scoped keyset paging and conditions; ACL/IAM and advanced settings are unavailable. |
| Object, XML and gRPC APIs | ❌ Unavailable | Unsupported routes return 404 without reading request bodies. |
| Persistence | 🚧 Partial | Transactional bucket catalog, exclusive directory lock and restart/WAL recovery. Object storage and fixtures are unavailable. |
| Authentication, IAM and ACLs | ❌ Unavailable | Neither permissive nor strict storage authentication is implemented. |
| Public and signed URLs | ❌ Unavailable | No URL helpers, signing or signature verification. |
| Browser CORS | ❌ Unavailable | JSON and XML CORS behavior is not implemented. |
| Official SDK workflows | 🚧 Partial | Go 1.27.3, storage v1.69.0 and API v0.306.0 verified for JSON bucket CRUD/paging; objects and other SDKs remain pending. |

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

The service is exposed only on host loopback. Follow the [runnable guide](docs/en/getting-started.md) to discover the allocated port, set the advertised endpoint and exercise bucket CRUD from host and peer containers.

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

Objects, fixture loading, authentication, signed URLs, CORS and TLS are not available. Bucket APIs support only the [documented subset](docs/en/slices/json-bucket-crud.md); `full` projection omits ACL/owner, project IDs are locally simulated, and recognized unsupported settings return 501. Native data defaults to cwd-relative `data`; `--data-dir` and `--api-base-url` are explicit overrides.

For contributors, see the [development guide](docs/en/development.md).

This is an independent project and is not an official Google product.
