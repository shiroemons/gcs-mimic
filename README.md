# gcs-mimic

A Google Cloud Storage emulator in Rust for local development and CI.

The current implementation provides a configurable HTTP server with health/readiness checks and graceful shutdown. **Bucket and object APIs are not available yet.** Storage requests return 404, so applications cannot use it for storage yet.

## Implementation status

| Capability | Status | Current scope |
| --- | --- | --- |
| Standalone binary | ✅ Available | Build and run from source; no Rust installation is needed at runtime. |
| Docker and Compose | 🚧 Partial | Non-root runtime; arm64 tested natively, amd64 tested under emulation. Native amd64 verification is pending. |
| CLI, environment and JSON configuration | ✅ Available | Command-line options override environment variables, configuration files and defaults. |
| Health, readiness and graceful shutdown | 🚧 Partial | Server probes and SIGINT/SIGTERM shutdown work. Storage recovery and fixture initialization are not available. |
| HTTP and TLS | 🚧 Partial | HTTP/1 is available; TLS is not implemented. |
| Bucket and object APIs | ❌ Unavailable | JSON, XML and gRPC storage operations are not implemented; unsupported HTTP routes return 404. |
| Fixture loading and persistence | ❌ Unavailable | No initial data import or persistent bucket/object storage. |
| Authentication, IAM and ACLs | ❌ Unavailable | Neither permissive nor strict storage authentication is implemented. |
| Public and signed URLs | ❌ Unavailable | No URL helpers, signing or signature verification. |
| Browser CORS | ❌ Unavailable | JSON and XML CORS behavior is not implemented. |
| Official SDK workflows | ❌ Unavailable | No supported SDK integration workflow yet. |

See the [detailed compatibility matrix](docs/en/feature-matrix.md) for supported scope and verification status.

## Run with Docker

Requires Git, Docker Engine/Desktop with Buildx, and curl. Linux amd64 and arm64 are supported. Build from source; no container image is published.

```sh
git clone https://github.com/shiroemons/gcs-mimic.git
cd gcs-mimic
docker build -t gcs-mimic:dev .
docker run --detach --rm --name gcs-mimic \
  --read-only --memory 128m --cpus 1 --pids-limit 64 \
  -p 127.0.0.1:4443:4443 gcs-mimic:dev
docker exec gcs-mimic gcs-mimic healthcheck --url http://127.0.0.1:4443/_internal/ready
curl --disable --noproxy '*' --fail http://127.0.0.1:4443/_internal/health
curl --disable --noproxy '*' --fail http://127.0.0.1:4443/_internal/ready
```

Both checks return HTTP 200 when ready. The service is exposed only on host loopback at port 4443.

```sh
docker stop gcs-mimic
```

The container runs as a non-root user. The command above sets a read-only filesystem and limits of 128 MiB memory, one CPU and 64 processes. These are configured limits, not measured resource requirements. For an application stack, see the [Compose instructions](docs/en/getting-started.md#use-with-docker-compose).

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

Fixture loading, persisted buckets/objects, SDK workflows, authentication, signed URLs, CORS and TLS are not available. No user data is stored, so there is nothing to persist or reset.

For contributors, see the [development guide](docs/en/development.md).

This is an independent project and is not an official Google product.
