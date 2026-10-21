# Build and run gcs-mimic

This guide runs the health/readiness shell. GCS APIs, fixtures, persistence, IAM/ACL, URL signing, CORS, TLS and SDK operations are unavailable. Unsupported routes return 404. The [future contract](getting-started-spec.md) describes later milestones, not runnable storage commands.

## Source, prepare and build

Install Git, Docker Engine/Desktop with Buildx, and curl. Linux amd64 and arm64 images build from source; Apple Silicon runs arm64 natively. No Python, Google credentials or cloud resources are needed.

```sh
git clone https://github.com/shiroemons/gcs-mimic.git
cd gcs-mimic
docker build -t gcs-mimic:dev .
docker run --detach --rm --name gcs-mimic \
  --read-only --memory 128m --cpus 1 --pids-limit 64 \
  -p 127.0.0.1:4443:4443 gcs-mimic:dev
docker exec gcs-mimic gcs-mimic healthcheck --url http://127.0.0.1:4443/_internal/ready
```

The local image is `gcs-mimic:dev`; there is no published image. Builder/runtime images and the BuildKit frontend are digest-pinned. The runtime is Debian stable slim with the binary and non-root UID/GID 10001. The server needs no writable mounts. If starting before the first probe succeeds, inspect `docker logs gcs-mimic` and rerun the readiness command.

```sh
docker stop gcs-mimic
```

## Use with Docker Compose

Compose is optional, useful when starting an application and emulator together. Requires Compose with `up --wait` support. After building the image, this repository's Compose configuration can be used directly:

```sh
docker compose config --quiet
docker compose up -d --wait
docker compose run --rm --no-deps gcs healthcheck --url http://gcs:4443/_internal/ready
```

For another project's Compose file, add a service using the locally built image:

```yaml
services:
  gcs:
    image: gcs-mimic:dev
    read_only: true
    mem_limit: 128m
    cpus: 1.0
    pids_limit: 64
```

Other services on that project's network reach `http://gcs:4443`; host port publishing is optional. The image includes a readiness healthcheck. This repository's Compose file additionally drops capabilities, limits descriptors to 1024 and publishes only host loopback.

## Health and connectivity

After starting with either Docker or Compose, run the host probes:

```sh
curl --disable --noproxy '*' --fail http://127.0.0.1:4443/_internal/health
curl --disable --noproxy '*' --fail http://127.0.0.1:4443/_internal/ready
curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}\n' \
  'http://127.0.0.1:4443/storage/v1/b?project=local-project'
```

Probes return 200 with `{"status":"ok","unix_time":...}`. The GCS request returns 404. Probe methods are GET/HEAD; other methods return 405 with `Allow: GET, HEAD`. Probe bodies return 413 without being collected. Oversized/malformed headers fail at the HTTP parser. Unknown internal routes return 404; no reset/admin route exists.

Host clients use `127.0.0.1:4443`; peers use `gcs:4443` on the Compose network. Inside a container, localhost refers to that container. `0.0.0.0` is a bind address. The server generates no public URLs or resumable Locations and accepts no ineffective URL settings. Public/signed URLs and SDK instructions will accompany their actual implementations.

Startup JSON reports the listener, effective limits, protocol and `gcs_api:false`. Ready follows config validation, signal registration and successful bind. There is no recovery/seeding work yet; these must finish before future storage readiness. Readiness becomes false on shutdown. The healthcheck client accepts explicit HTTP probe URLs, bypasses proxies, caps responses at 16 KiB and rejects malformed/truncated responses. After DNS resolution, connection attempts and request/response I/O share one two-second deadline, including slowly streamed responses. DNS uses the OS resolver; numeric loopback avoids DNS for container healthchecks.

## Native binary and configuration

Requires rustup. The verified exact stable compiler is Rust 1.99.1, edition 2024.

```sh
rustup toolchain install 1.99.1 --profile minimal --component rustfmt --component clippy
cargo +1.99.1 fetch --locked
cargo +1.99.1 xtask dependencies check
cargo +1.99.1 build --release --locked --offline --bin gcs-mimic
./target/release/gcs-mimic --version
./target/release/gcs-mimic --help
./target/release/gcs-mimic serve --config config/development.json
```

Use another terminal for probes, then Ctrl-C to stop. The binary needs no Rust installation, Python or curl at runtime.

```sh
./target/release/gcs-mimic healthcheck --url http://127.0.0.1:4443/_internal/ready
```

Precedence is CLI > `GCS_MIMIC_` environment > JSON file > defaults. `--config` overrides `GCS_MIMIC_CONFIG`. JSON files are capped at 64 KiB. Unknown settings, duplicate CLI options, invalid addresses and out-of-range limits fail before readiness. The current configuration is JSON; the future TOML specification is not implemented.

| CLI option | Environment | Default | Range |
| --- | --- | --- | --- |
| `--listen` | `GCS_MIMIC_LISTEN` | `127.0.0.1:4443` | Numeric IP:port; port 0 allowed |
| `--max-connections` | `GCS_MIMIC_MAX_CONNECTIONS` | 32 | 1–1024 |
| `--worker-threads` | `GCS_MIMIC_WORKER_THREADS` | 2 | 1–32 |
| `--max-blocking-threads` | `GCS_MIMIC_MAX_BLOCKING_THREADS` | 2 | 1–32 |
| `--connection-timeout-ms` | `GCS_MIMIC_CONNECTION_TIMEOUT_MS` | 5000 | 1–60000 |
| `--shutdown-timeout-ms` | `GCS_MIMIC_SHUTDOWN_TIMEOUT_MS` | 5000 | 1–60000 |

```sh
GCS_MIMIC_MAX_CONNECTIONS=8 ./target/release/gcs-mimic serve \
  --config config/development.json --max-connections 4
```

This selects a connection cap of 4. Native defaults bind loopback; Docker binds `0.0.0.0:4443` inside the container and Compose publishes only host loopback. Auth, seed, storage, URL and TLS settings fail as unknown. Neither strict nor permissive authentication is claimed by a shell without GCS handlers.

## Limits, stop and restart

HTTP/1 keep-alive is disabled. Each connection handles one request with a 16 KiB parser buffer, 64 headers and a lifetime deadline. Admission stops at the connection cap, with no application work queue; the requested kernel backlog is 128. Async/blocking thread counts are bounded. No database caches or maintenance workers exist yet.

SIGINT/SIGTERM stop acceptance and drain within the configured deadline. A drain timeout aborts remaining tasks and exits nonzero. No storage writes exist in the current implementation. The default five-second deadline fits Compose's 15-second grace period; keep the latter longer if overriding limits.

When using Compose:

```sh
docker compose stop
docker compose up -d --wait
docker compose down
```

There is no user data to persist, seed or reset. Future storage will use separate read-only fixture input and durable volumes.

## Isolated CI and updates

```sh
cargo +1.99.1 test --workspace --locked --offline
cargo +1.99.1 fmt --all --check
cargo +1.99.1 clippy --workspace --all-targets --locked --offline -- -D warnings
cargo +1.99.1 xtask compatibility check
bash scripts/smoke-containers.sh
```

The smoke script optionally takes `linux/amd64` and/or `linux/arm64`. It builds each image, probes with `--network none`, then checks host and peer-container connectivity in an isolated Compose project. Unique project/image names and dynamic loopback ports avoid collisions. It verifies non-root/read-only/resource settings, 404/405 and both signals, and removes only its own containers/network. No volumes are created. Builds require registry access; runtime requests use local endpoints. Ordinary Rust tests make no outbound requests. Only the explicit freshness check fetches release metadata.

Pinned/current-stable Rust CI and native amd64/arm64 container CI are configured. Daily freshness runs at 03:17 UTC every calendar day, without cooldown. Remote runs and schedule activation have not been observed. Run `cargo +1.99.1 xtask dependencies check` at slice start and before release and inspect exceptions weekly. Exit 1 means updates; exit 2 means check failure. The checker does not modify manifests or publish changes.

## Troubleshooting and limits of verification

| Symptom | Check |
| --- | --- |
| Connection refused | `docker compose ps`, `docker compose logs gcs`, port mapping and host/container address. |
| Port in use | Set `GCS_MIMIC_PORT=0`; inspect `docker compose port gcs 4443`. |
| Config error | JSON syntax, option spelling, environment overrides and allowed ranges. |
| Healthcheck fails | Ready URL, protocol and port; incomplete or malformed responses are rejected. |
| GCS/SDK returns 404 | Expected in the current implementation; bucket/object APIs and authentication are pending. |
| Build/update metadata unreachable | Check registry/network access; failures never count as current versions. |

See [tests](evidence/p0-tests.txt), [containers](evidence/p0-containers.txt), [dependencies](evidence/p0-dependencies.txt) and the [ledger](../../compatibility/features.json). Local Docker testing was arm64 native and amd64 emulated. Native amd64 and remote CI are pending. No performance, SDK or real-cloud compatibility claims are made.
