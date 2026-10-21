# Build and run gcs-mimic

The current slice supports JSON bucket create/get/list/delete with a persistent SQLite
catalog. Objects, fixtures, IAM/ACL, signed URLs, CORS and TLS remain unavailable.
Requests have no authentication enforcement; use an isolated local development environment.
The [slice contract](slices/json-bucket-crud.md) lists supported fields and deviations.

## Build and start with Compose

Install Git, Docker Engine/Desktop with Buildx and Compose v2 (`up --wait` and
`start --wait`), curl and jq. No Google credentials or cloud resources are needed.
Build from a checkout; no image is published. The commands use an isolated Compose
project and a dynamically allocated loopback port to avoid other local emulators.

```sh
docker build -t gcs-mimic:dev .
export COMPOSE_PROJECT_NAME=gcs-mimic-bucket-demo
export GCS_MIMIC_PORT=0
docker compose config --quiet
docker compose up -d --wait
address=$(docker compose port gcs 4443)
endpoint="http://$address"
docker compose exec -T --interactive=false gcs gcs-mimic healthcheck --url http://127.0.0.1:4443/_internal/ready
curl --disable --noproxy '*' --fail "$endpoint/_internal/health"
curl --disable --noproxy '*' --fail "$endpoint/_internal/ready"
```

The image runs as UID/GID 10001 on a read-only root. Compose mounts the named
`gcs-data` volume at `/var/lib/gcs-mimic`, prepared with writable ownership by the
image. SQLite, WAL and the ownership lock live there. Memory is limited to 128 MiB,
CPU to one, PIDs to 64 and file descriptors to 1024; these are configured bounds,
not measured resource requirements. The image also works with `docker run` when
an explicit writable volume is mounted at that path.

The default fixed `selfLink` origin is `http://127.0.0.1:4443`, independent of the
selected listener. With a dynamic port, set the actual advertised endpoint and
recreate the service; the named volume preserves its catalog:

```sh
export GCS_MIMIC_PORT=${address##*:}
export GCS_MIMIC_API_BASE_URL="$endpoint"
docker compose up -d --wait
```

Host clients use `$endpoint`. Peers on the Compose network use `http://gcs:4443`.
Inside a container, localhost refers to that container; `0.0.0.0` is a listener
address. `selfLink` always uses the one configured origin. Set a peer-reachable
origin instead if peers must follow those links. Host and forwarded headers are
never used to construct links. Public object URLs are not implemented.

## Create, inspect, list and delete buckets

```sh
curl --disable --noproxy '*' --fail -H 'Content-Type: application/json' \
  --data '{"name":"demo-bucket","location":"EU","labels":{"team":"local"}}' \
  "$endpoint/storage/v1/b?project=local-project"
curl --disable --noproxy '*' --fail "$endpoint/storage/v1/b/demo-bucket"
curl --disable --noproxy '*' --fail "$endpoint/storage/v1/b?project=local-project&prefix=demo-&maxResults=1"
```

Create/get/list return 200 with JSON. Duplicate names return 409 across all local
projects. `metageneration` is the string `"1"`; timestamps and labels survive restart.
List is sorted and project/prefix scoped, with opaque continuation tokens. Limits
are 1000 for `noAcl` and 200 for `full`; `full` currently omits ACL/owner fields.
Only US/EU/ASIA locations and the documented storage classes are accepted.
Recognized unsupported settings return 501; invalid names/queries return 400.
Insert bodies are capped at 64 KiB; get/list/delete require no body.

Peer-container curl uses a digest-pinned, test-only official curl image:

```sh
peer_image=curlimages/curl:8.23.0@sha256:11e64e28c5f58c8d93ee95fcf22ac055f48b0bc440e0ce7d8013bbb5ad6aa8ac
docker run --rm --read-only --network "${COMPOSE_PROJECT_NAME}_default" "$peer_image" \
  --disable --noproxy '*' --fail -H 'Content-Type: application/json' \
  --data '{"name":"peer-bucket"}' 'http://gcs:4443/storage/v1/b?project=local-project'
docker run --rm --read-only --network "${COMPOSE_PROJECT_NAME}_default" "$peer_image" \
  --disable --noproxy '*' --fail http://gcs:4443/storage/v1/b/peer-bucket
docker run --rm --read-only --network "${COMPOSE_PROJECT_NAME}_default" "$peer_image" \
  --disable --noproxy '*' --fail 'http://gcs:4443/storage/v1/b?project=local-project&prefix=peer-'
docker run --rm --read-only --network "${COMPOSE_PROJECT_NAME}_default" "$peer_image" \
  --disable --noproxy '*' --fail -X DELETE http://gcs:4443/storage/v1/b/peer-bucket
```

## Stop, restart and deliberate reset

```sh
docker compose stop
docker compose start --wait
curl --disable --noproxy '*' --fail "$endpoint/storage/v1/b/demo-bucket"
docker compose down
docker compose up -d --wait
curl --disable --noproxy '*' --fail "$endpoint/storage/v1/b/demo-bucket"
curl --disable --noproxy '*' --fail -X DELETE "$endpoint/storage/v1/b/demo-bucket"
curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}\n' \
  "$endpoint/storage/v1/b/demo-bucket"
```

Stop/start and ordinary down/up preserve the named volume. Delete returns 204 with
an empty body; the subsequent get returns 404. No reset/admin route exists.
To demonstrate a deliberate reset, first create a disposable bucket:

```sh
curl --disable --noproxy '*' --fail -H 'Content-Type: application/json' \
  --data '{"name":"reset-bucket"}' "$endpoint/storage/v1/b?project=local-project"
docker compose down --volumes
docker compose up -d --wait
curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}\n' \
  "$endpoint/storage/v1/b/reset-bucket"
```

`down --volumes` deletes **all data in this Compose project's managed volume**;
the final get returns 404. Use it only for a disposable local catalog. There is no
fixture replay or seeding. SIGINT/SIGTERM stop acceptance and drain; recovery and
exclusive data-directory ownership must finish before bind/readiness. A second
process using the same directory fails without reporting ready. Future-schema,
corrupt or inaccessible catalogs also prevent startup.

## Official Go SDK

Install Go (the harness downloads pinned Go 1.27.3), then run against the Compose
endpoint from the repository root:

```sh
(cd sdk-tests/go && GCS_MIMIC_ENDPOINT="$endpoint" GOTOOLCHAIN=auto go test -count=1 -v ./...)
```

The harness pins `cloud.google.com/go/storage` v1.69.0 and `google.golang.org/api`
v0.306.0. It sets `STORAGE_EMULATOR_HOST`, disables credentials, metrics, proxies
and redirects, and confines requests to the explicit emulator. It exercises
bucket CRUD, prefix/paged listing and 409/404 errors. It creates and cleans up its
own random buckets. No object/ACL or unrestricted SDK support is claimed.
For an isolated native server plus vet/test, use `bash scripts/sdk-go.sh`.

## Native binary and configuration

Build with Rust 1.99.1 and the committed lockfile:

```sh
cargo +1.99.1 build --release --locked --bin gcs-mimic
./target/release/gcs-mimic serve --listen 127.0.0.1:0 --data-dir data
```

The resulting binary needs no Rust, Python, curl or Go installation. CLI overrides
`GCS_MIMIC_` environment, JSON config and defaults; `--config` overrides
`GCS_MIMIC_CONFIG`. Config files are capped at 64 KiB; invalid settings fail before
readiness. The current configuration is JSON; future TOML settings are unavailable.

| CLI option | Environment | Default | Range |
| --- | --- | --- | --- |
| `--listen` | `GCS_MIMIC_LISTEN` | `127.0.0.1:4443` | Numeric IP:port; port 0 allowed |
| `--data-dir` | `GCS_MIMIC_DATA_DIR` | `data` relative to cwd | Nonempty path; exclusive catalog ownership |
| `--api-base-url` | `GCS_MIMIC_API_BASE_URL` | `http://127.0.0.1:4443` | Absolute HTTP(S), optional base path; no credentials/query/fragment |
| `--max-connections` | `GCS_MIMIC_MAX_CONNECTIONS` | 32 | 1–1024 |
| `--worker-threads` | `GCS_MIMIC_WORKER_THREADS` | 2 | 1–32 |
| `--max-blocking-threads` | `GCS_MIMIC_MAX_BLOCKING_THREADS` | 2 | 1–32 |
| `--connection-timeout-ms` | `GCS_MIMIC_CONNECTION_TIMEOUT_MS` | 5000 | 1–60000 |
| `--shutdown-timeout-ms` | `GCS_MIMIC_SHUTDOWN_TIMEOUT_MS` | 5000 | 1–60000 |

Each HTTP/1 connection handles one request, with a 16 KiB parser buffer, 64 headers
and a deadline. Unknown routes return 404 without body reads; probes retain
GET/HEAD support and body rejection. Catalog work has 32 admitted calls, one
executing blocking job, a 2 MiB SQLite cache and no memory map. The runtime bounds
blocking threads; queued/cancelled operations retain directory ownership until
execution finishes. A connection timeout can leave an already admitted write
committed, as with an uncertain network outcome; clients must read back state.

## Verification and troubleshooting

```sh
cargo +1.99.1 test --workspace --locked
cargo +1.99.1 fmt --all --check
cargo +1.99.1 clippy --workspace --all-targets --locked -- -D warnings
cargo +1.99.1 xtask compatibility check
bash scripts/smoke-containers.sh
```

The smoke script builds arm64/amd64 images, checks isolated probes, host and peer
curl CRUD, stop/start and down/up persistence, deliberate reset, resource settings
and both shutdown signals. It removes only its own test volumes and containers.
On Apple Silicon, arm64 is native and amd64 uses emulation. Native amd64, remote
CI, real-cloud conformance and performance measurements remain unverified.

For connection errors, inspect `docker compose ps`, `docker compose logs gcs`,
port mapping and the caller's network. For startup errors, check catalog path
ownership, disk space, another owner process and config overrides. Objects and
other unsupported routes still return 404; unsupported bucket semantics return
501 rather than false success. See [evidence](evidence/bucket-crud.md) and the
[ledger](../../compatibility/features.json) for exact verification scope.
