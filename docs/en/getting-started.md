# Build and run gcs-mimic

The current slice supports JSON bucket create/get/list/delete and object media upload,
metadata get, whole-object download and delete, with a persistent SQLite catalog and disk blobs.
Multipart/resumable uploads, object listing/patch, ranges, fixtures, IAM/ACL, signed URLs,
CORS and TLS remain unavailable.
Requests have no authentication enforcement; use an isolated local development environment.
The [bucket contract](slices/json-bucket-crud.md) and
[object contract](slices/json-object-media.md) list supported fields and deviations.
Run the shell blocks in order in one Bash session from the repository root.

## Build and start with Compose

Install Git, Docker Engine/Desktop with Buildx and Compose v2 (`up --wait` and
`start --wait`), curl and jq. No Google credentials or cloud resources are needed.
Build from a checkout; no image is published. The commands use an isolated Compose
project and a dynamically allocated loopback port to avoid other local emulators.

```sh
export COMPOSE_PROJECT_NAME=gcs-mimic-object-demo-$$
export GCS_MIMIC_IMAGE="gcs-mimic:object-demo-$$"
docker build -t "$GCS_MIMIC_IMAGE" .
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
image. SQLite, WAL, the ownership lock, staging and opaque blobs live there. Memory is limited to 128 MiB,
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

## Upload, inspect, download and conditionally create an object

This exact payload includes its final newline; the supplied base64 MD5 covers all 22 bytes.
The server also calculates CRC32C. An incorrect declared checksum returns 400 and publishes
no replacement. Encode a slash in the object path as `%2F`; `%252F` denotes a literal `%2F`.

```sh
work_dir=$(mktemp -d)
printf 'Hello from gcs-mimic.\n' > "$work_dir/upload.txt"
curl --disable --noproxy '*' --fail -H 'Content-Type: text/plain' \
  -H 'x-goog-hash: md5=b9CmrqnrVfHIcCH+TLg1NA==' \
  --data-binary @"$work_dir/upload.txt" \
  "$endpoint/upload/storage/v1/b/demo-bucket/o?uploadType=media&name=notes%2Fhello.txt&ifGenerationMatch=0" \
  > "$work_dir/object.json"
jq . "$work_dir/object.json"
generation=$(jq -r .generation "$work_dir/object.json")
curl --disable --noproxy '*' --fail \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt" | jq .
curl --disable --noproxy '*' --fail \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt?alt=media&generation=$generation" \
  --output "$work_dir/download.txt"
cmp "$work_dir/upload.txt" "$work_dir/download.txt"
status=$(curl --disable --noproxy '*' --silent --output "$work_dir/condition.json" \
  --write-out '%{http_code}' -H 'Content-Type: text/plain' \
  --data-binary @"$work_dir/upload.txt" \
  "$endpoint/upload/storage/v1/b/demo-bucket/o?uploadType=media&name=notes%2Fhello.txt&ifGenerationMatch=0")
test "$status" = 412
jq . "$work_dir/condition.json"
status=$(curl --disable --noproxy '*' --silent --output "$work_dir/not-modified" \
  --write-out '%{http_code}' \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt?ifGenerationNotMatch=$generation")
test "$status" = 304
test ! -s "$work_dir/not-modified"
status=$(curl --disable --noproxy '*' --silent --output "$work_dir/nonempty.json" \
  --write-out '%{http_code}' -X DELETE "$endpoint/storage/v1/b/demo-bucket")
test "$status" = 409
jq . "$work_dir/nonempty.json"
```

Successful media upload returns 200 Object JSON. A failed create-if-absent returns 412;
a GET whose not-match condition is false returns 304 with an empty body. Mutations use
412 for that condition instead (local rule, SPEC-008). A nonempty bucket returns 409.
Ordinary `alt=media` responds directly with 200, a declared deviation from the documented
redirect flow. Metadata/media GET and DELETE accept Go SDK retry tokens without replay or
deduplication; repeated DELETE can therefore return 404 (SPEC-013). Upload tokens return 501.

Peer-container upload, metadata, download and delete use the same media subset:

```sh
docker run --rm --read-only --network "${COMPOSE_PROJECT_NAME}_default" "$peer_image" \
  --disable --noproxy '*' --fail -H 'Content-Type: text/plain' --data-binary 'peer data' \
  'http://gcs:4443/upload/storage/v1/b/demo-bucket/o?uploadType=media&name=peer%2Fhello.txt'
docker run --rm --read-only --network "${COMPOSE_PROJECT_NAME}_default" "$peer_image" \
  --disable --noproxy '*' --fail 'http://gcs:4443/storage/v1/b/demo-bucket/o/peer%2Fhello.txt'
docker run --rm --read-only --network "${COMPOSE_PROJECT_NAME}_default" "$peer_image" \
  --disable --noproxy '*' --fail 'http://gcs:4443/storage/v1/b/demo-bucket/o/notes%2Fhello.txt?alt=media' \
  > "$work_dir/peer-download.txt"
cmp "$work_dir/upload.txt" "$work_dir/peer-download.txt"
docker run --rm --read-only --network "${COMPOSE_PROJECT_NAME}_default" "$peer_image" \
  --disable --noproxy '*' --fail -X DELETE 'http://gcs:4443/storage/v1/b/demo-bucket/o/peer%2Fhello.txt'
```

## Stop, restart and deliberate reset

```sh
docker compose stop
docker compose start --wait
curl --disable --noproxy '*' --fail "$endpoint/storage/v1/b/demo-bucket"
curl --disable --noproxy '*' --fail \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt?alt=media&generation=$generation" \
  --output "$work_dir/restarted.txt"
cmp "$work_dir/upload.txt" "$work_dir/restarted.txt"
docker compose down
docker compose up -d --wait
curl --disable --noproxy '*' --fail "$endpoint/storage/v1/b/demo-bucket"
curl --disable --noproxy '*' --fail \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt" > "$work_dir/restarted.json"
cmp "$work_dir/object.json" "$work_dir/restarted.json"
curl --disable --noproxy '*' --fail -X DELETE \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt?ifGenerationMatch=$generation"
status=$(curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
  "$endpoint/storage/v1/b/demo-bucket/o/notes%2Fhello.txt")
test "$status" = 404
curl --disable --noproxy '*' --fail -X DELETE "$endpoint/storage/v1/b/demo-bucket"
status=$(curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
  "$endpoint/storage/v1/b/demo-bucket")
test "$status" = 404
```

Stop/start and ordinary down/up preserve the named volume, exact object bytes and metadata.
Object and empty-bucket delete return 204 with
an empty body; the subsequent get returns 404. No reset/admin route exists.
To demonstrate a deliberate reset, first create a disposable bucket:

```sh
curl --disable --noproxy '*' --fail -H 'Content-Type: application/json' \
  --data '{"name":"reset-bucket"}' "$endpoint/storage/v1/b?project=local-project"
curl --disable --noproxy '*' --fail -H 'Content-Type: text/plain' --data-binary 'reset data' \
  "$endpoint/upload/storage/v1/b/reset-bucket/o?uploadType=media&name=reset.txt"
docker compose down --volumes
docker compose up -d --wait
status=$(curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
  "$endpoint/storage/v1/b/reset-bucket")
test "$status" = 404
status=$(curl --disable --noproxy '*' --silent --output /dev/null --write-out '%{http_code}' \
  "$endpoint/storage/v1/b/reset-bucket/o/reset.txt?alt=media")
test "$status" = 404
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
bucket CRUD/paging and object Attrs/NewReader/Delete, exact bytes/hashes, generation
conditions, encoded names and nonempty bucket errors. The harness uploads raw media with
its confined HTTP client because the SDK Writer uses unsupported multipart/resumable
uploads; it verifies that Writer fails 501 without creating an object. It creates and
cleans up its own random buckets. ACLs, token deduplication and unrestricted SDK support
are not claimed. Ruby/Rails remains unverified.
For an isolated native server plus vet/test, use `bash scripts/sdk-go.sh`.

Finish the Compose walkthrough by stopping and removing its containers and
network, disposable volume, temporary files and this walkthrough's exact image tag:

```sh
docker compose down --volumes
docker image rm "$GCS_MIMIC_IMAGE"
rm -r "$work_dir"
unset GCS_MIMIC_IMAGE GCS_MIMIC_PORT GCS_MIMIC_API_BASE_URL
```

## Native binary and configuration

Build with Rust 1.99.1 and the committed lockfile:

```sh
cargo +1.99.1 build --release --locked --bin gcs-mimic
native_dir=$(mktemp -d)
./target/release/gcs-mimic serve --listen 127.0.0.1:0 --data-dir "$native_dir/data" \
  --api-base-url http://127.0.0.1:4443 > "$native_dir/server.log" 2>&1 &
native_pid=$!
trap 'kill -TERM "$native_pid" 2>/dev/null || true' EXIT
for attempt in $(seq 1 100); do
  if jq -e 'select(.event == "ready")' "$native_dir/server.log" >/dev/null; then break; fi
  kill -0 "$native_pid"
  sleep 0.1
done
native_endpoint="http://$(jq -r 'select(.event == "ready") | .listen' "$native_dir/server.log")"
./target/release/gcs-mimic healthcheck --url "$native_endpoint/_internal/ready"
curl --disable --noproxy '*' --fail -H 'Content-Type: application/json' \
  --data '{"name":"native-bucket"}' "$native_endpoint/storage/v1/b?project=local-project"
curl --disable --noproxy '*' --fail -H 'Content-Type: text/plain' --data-binary 'native data' \
  "$native_endpoint/upload/storage/v1/b/native-bucket/o?uploadType=media&name=hello.txt"
curl --disable --noproxy '*' --fail \
  "$native_endpoint/storage/v1/b/native-bucket/o/hello.txt?alt=media" > "$native_dir/download.txt"
printf 'native data' | cmp - "$native_dir/download.txt"
kill -TERM "$native_pid"
wait "$native_pid"
trap - EXIT
jq . "$native_dir/server.log"
rm -r "$native_dir"
```

Compose-only `GCS_MIMIC_IMAGE` and `GCS_MIMIC_PORT` must be unset before native startup:
the binary rejects unknown `GCS_MIMIC_` settings. The temporary native catalog above is
removed after a successful SIGTERM shutdown; use `--data-dir data` to keep a local catalog.
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
| `--object-request-timeout-ms` | `GCS_MIMIC_OBJECT_REQUEST_TIMEOUT_MS` | 900000 (15 minutes) | 1–86400000 (24 hours) |
| `--catalog-wait-timeout-ms` | `GCS_MIMIC_CATALOG_WAIT_TIMEOUT_MS` | 30000 | 1–60000 |
| `--shutdown-timeout-ms` | `GCS_MIMIC_SHUTDOWN_TIMEOUT_MS` | 5000 | 1–60000 |
| `--max-object-bytes` | `GCS_MIMIC_MAX_OBJECT_BYTES` | 1073741824 (1 GiB) | 1–5497558138880 (5 TiB) |

Each HTTP/1 connection handles one request, with a 16 KiB parser buffer, 64 headers
and a deadline. Object routes use a header deadline followed by renewed chunk-inactivity
deadlines; active transfers can exceed the default five seconds. After confirmed upload EOF,
body inactivity is suspended while storage finishes. Upload/media requests additionally
have an absolute 15-minute ceiling that chunks cannot renew. Expiry during upload body
streaming closes the connection without an HTTP error response or publication; expiry
during download truncates the stream after its headers and closes the connection.
For large/slow transfers, use `--object-request-timeout-ms 3600000` for one hour or set
the corresponding environment/JSON value. Advisory checks and post-EOF storage waits each default to 30 seconds,
capped by the remaining absolute request budget, and return 503 `backendError` on expiry.
After a post-EOF timeout the mutation outcome is unknown: the commit may still complete,
so re-read state or retry with generation/metageneration preconditions. Complete bodies
may also commit after disconnect or shutdown. SQLite lock waiting retains five seconds;
background filesystem work and shutdown catalog close have no separate execution bound.
Bucket/probe routes retain a total deadline.
Uploads have eight admission slots, a two-slot channel with at most 64 KiB
per copied chunk, and independent size checks before publication. Downloads use one channel
slot and a 64 KiB reader. No transaction or blocking worker waits for incoming body data.
These inspected bounds are not an RSS/cgroup/CPU benchmark; the 1 GiB default cap has not
been verified by a 1 GiB transfer under the Compose limit.
Unknown routes return 404 without body reads; probes retain
GET/HEAD support and body rejection. Catalog work has 32 admitted calls, one
executing blocking job, a 2 MiB SQLite cache and no memory map. The runtime bounds
blocking threads; queued/cancelled operations retain directory ownership until
execution finishes. Before confirmed EOF, a timeout/disconnect aborts the upload without
publishing partial bytes. Early rejections send a response and half-close the socket, then
discard at most 256 KiB for at most 100 ms. Generic admitted catalog writes and completed
upload bodies can still commit after the client disconnects; clients must read back state
when the response is lost. Trickle uploads cannot renew the absolute request ceiling;
cancelled uploads release slots after their cleanup finishes.

Schema-v1 bucket catalogs contain no objects and migrate directly to v3 atomically. Unreleased schema-v2 catalogs
are rejected without changing their data; automatic recovery of deleted v2 generations is
unavailable. Recovery ignores `.DS_Store` and `._*` OS metadata; other invalid storage
entries stop startup with a path diagnostic. Object `selfLink` addresses the current
resource without a generation query; `mediaLink` selects the returned generation.

## Verification and troubleshooting

```text
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
ownership, disk space, another owner process and config overrides. Unknown routes return
404; recognized unsupported object/bucket semantics return 501 rather than false success.
Missing/truncated referenced blobs prevent readiness; host power loss and every individual
fsync crash window remain unverified. See the [integration evidence](evidence/object-media.md),
[executed guide transcript](evidence/object-media-guide.txt) and the
[ledger](../../compatibility/features.json) for exact verification scope.
