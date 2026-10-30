# Native binary, bucket initialization and seed files

<!-- markdownlint-configure-file {"MD013": false} -->

Part of the [setup and integration guides](getting-started.md#guides). These
native journeys build their own binary and run from the repository root.

## Native binary and configuration

Build with Rust 1.99.1 and the committed lockfile:

```sh
cargo +1.99.1 build --release --locked --bin gcs-mimic
native_dir=$(mktemp -d)
./target/release/gcs-mimic serve --scheme http --listen 127.0.0.1:0 --data-dir "$native_dir/data" \
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
native_public_url=$(./target/release/gcs-mimic url public --base-url "$native_endpoint" \
  --bucket native-bucket --object hello.txt)
status=$(curl --disable --noproxy '*' --silent --show-error --output "$native_dir/xml.txt" \
  --write-out '%{http_code}' "$native_public_url")
test "$status" = 200
printf 'native data' | cmp - "$native_dir/xml.txt"
curl --disable --noproxy '*' --fail -X PATCH -H 'Content-Type: application/json' \
  --data '{"labels":{"team":"local","stage":"dev"},"storageClass":"NEARLINE"}' \
  "$native_endpoint/storage/v1/b/native-bucket?ifMetagenerationMatch=1" > "$native_dir/patched.json"
jq -e '.labels == {"team":"local","stage":"dev"} and .storageClass == "NEARLINE" and .metageneration == "2"' \
  "$native_dir/patched.json"
curl --disable --noproxy '*' --fail -X PATCH -H 'Content-Type: application/json' \
  --data '{"labels":{"stage":null}}' "$native_endpoint/storage/v1/b/native-bucket" > "$native_dir/removed.json"
jq -e '.labels == {"team":"local"} and .storageClass == "NEARLINE"' "$native_dir/removed.json"
curl --disable --noproxy '*' --fail "$native_endpoint/storage/v1/b/native-bucket" > "$native_dir/after.json"
cmp "$native_dir/removed.json" "$native_dir/after.json"
kill -TERM "$native_pid"
wait "$native_pid"
trap - EXIT
jq . "$native_dir/server.log"
rm -r "$native_dir"
```

Compose-only `GCS_MIMIC_IMAGE`, `GCS_MIMIC_PORT` and `GCS_MIMIC_TLS_DIR` must be
unset before native startup; the binary explains their image, host-port and
mount roles. The temporary native catalog above is
removed after a successful SIGTERM shutdown; use `--data-dir data` to keep a local catalog.
The native example uses permissive mode, so XML reads bypass authorization.
On macOS and other Unix systems outside Linux/Android, restarting on the same
port may wait for TIME_WAIT to drain (about 30 seconds on the verified macOS
host, at most 65 seconds). A `bind_wait` event explains the bounded wait;
an existing live listener still causes startup to fail. Linux/Android enables
address reuse while preserving live-listener exclusivity. Windows is unchanged.
For a wildcard listen address, the loopback probe may miss a live listener on
a specific non-loopback IP; startup then waits up to 65 seconds before failing
with address-in-use rather than taking over that listener's traffic.
SIGINT/SIGTERM cancel the bind wait and release the catalog promptly.
For private objects, start with `--auth-mode strict --identity-file config/identities.json`
and follow the [public/private policy journey](getting-started.md#public-and-private-object-reads)
using the native URL helper.
The resulting binary needs no Rust, Python, curl or Go installation. CLI overrides
`GCS_MIMIC_` environment, JSON config and defaults; `--config` overrides
`GCS_MIMIC_CONFIG`. Config files are capped at 64 KiB; invalid settings fail before
readiness. The current configuration is JSON; future TOML settings are unavailable.
Independent configuration, TLS and identity errors are collected into stderr
JSON diagnostic batches before catalog access; warnings precede errors. Known
legacy inputs fail with native migration advice, unknown `FAKE_GCS_*` names
warn, and native values preserve their existing precedence. See
[migration diagnostics](fake-gcs-server-configuration.md#migration-diagnostics)
for the bounded input, redaction and `/data` inspection rules.

Runtime stderr is newline-delimited JSON with `event` and `level` fields.
`log_level` in JSON, `GCS_MIMIC_LOG_LEVEL` or `--log-level` selects a minimum
severity: `debug`, `info` (default), `warn` or `error`; `warning` aliases `warn`.
Debug adds connection completion, info includes seeding/stopping, warn includes
transport failures/timeouts and deferred cleanup, and error includes lock-release
failure. Validation diagnostics, including input warnings, remain visible at
every level. Stdout `ready` and `stopped` are always emitted for automation.
Empty/unknown levels fail before catalog access. Legacy `FAKE_GCS_LOG_LEVEL`
and `-log-level` remain errors with native migration advice.

| CLI option | Environment | Default | Range |
| --- | --- | --- | --- |
| `--listen` | `GCS_MIMIC_LISTEN` | `127.0.0.1:4443` | Numeric IP:port; port 0 allowed |
| `--log-level` | `GCS_MIMIC_LOG_LEVEL` | `info` | `debug`, `info`, `warn`, `error`; `warning` aliases `warn`; JSON key `log_level` |
| `--scheme` | `GCS_MIMIC_SCHEME` | `https` | `http`, `https` or `both`; both uses primary HTTPS and secondary HTTP |
| `--listen-http` | `GCS_MIMIC_LISTEN_HTTP` | Primary listen IP:8000 | Numeric IP:port, port 0 allowed; requires `scheme=both`; reject equal nonzero endpoints |
| `--grpc-listen` | `GCS_MIMIC_GRPC_LISTEN` | none (disabled) | JSON key `grpc_listen`; numeric IP:port (for example `127.0.0.1:4444`), port 0 allowed; separate plaintext h2c listener. Set Go `STORAGE_EMULATOR_HOST_GRPC` to its address. It serves the scoped Storage v2 bucket, object, write and read RPCs and the legacy Storage v1 migration profile; other RPCs and unsupported fields return actionable, non-retryable `UNIMPLEMENTED`. See the [Storage v2 bucket](slices/grpc-v2-buckets.md), [object I/O](slices/grpc-v2-object-io.md), [update/copy](slices/grpc-v2-update-copy.md), [bidi read](slices/grpc-v2-bidi-read.md) and [Storage v1](slices/grpc-v1-legacy.md) contracts. |
| `--tls-cert` | `GCS_MIMIC_TLS_CERT` | built-in development certificate | Optional PEM leaf-first chain, at most 1 MiB; requires matching key |
| `--tls-key` | `GCS_MIMIC_TLS_KEY` | built-in development key | Optional matching unencrypted PEM private key, at most 1 MiB; requires certificate |
| `--tls-handshake-timeout-ms` | `GCS_MIMIC_TLS_HANDSHAKE_TIMEOUT_MS` | 5000 | 1–60000; HTTPS only, within connection admission |
| `--data-dir` | `GCS_MIMIC_DATA_DIR` | `data` relative to cwd | Nonempty path; exclusive catalog ownership |
| `--seed-dir` | `GCS_MIMIC_SEED_DIR` | none | JSON key `seed_dir`; stable read-only bucket-directory input applied before readiness on every startup. Wins over `default_seed_dir` |
| `--default-seed-dir` | `GCS_MIMIC_DEFAULT_SEED_DIR` | none; the image sets `/data` | JSON key `default_seed_dir`. Used only when `seed_dir` is unset, the directory exists and it does not overlap the data or sync directory; otherwise silently ignored |
| `--seed-content-type` | `GCS_MIMIC_SEED_CONTENT_TYPE` | `extension` | JSON key `seed_content_type`; `extension` uses the pinned filename-extension table, `octet-stream` stores `application/octet-stream`. Also applies to files ingested from `--sync-dir` |
| `--seed-project` | `GCS_MIMIC_SEED_PROJECT` | `local-project` | Existing bucket project validation; administrative import |
| `--buckets-file` | `GCS_MIMIC_BUCKETS_FILE` | none | JSON v1; validate all entries and create missing buckets before seed; existing buckets are preserved |
| `--api-base-url` | `GCS_MIMIC_API_BASE_URL` | Actual bound listener URL | Wildcard IP becomes loopback; port 0 becomes the allocated port. Explicit absolute HTTP(S) base remains independent; optional base path, no credentials/query/fragment. Ready includes the effective string |
| `--max-connections` | `GCS_MIMIC_MAX_CONNECTIONS` | 32 | 1–1024; total across listeners, including TLS handshakes |
| `--worker-threads` | `GCS_MIMIC_WORKER_THREADS` | 2 | 1–32 |
| `--max-blocking-threads` | `GCS_MIMIC_MAX_BLOCKING_THREADS` | 8 | 1–32 |
| `--connection-timeout-ms` | `GCS_MIMIC_CONNECTION_TIMEOUT_MS` | 5000 | 1–60000; request headers, then object response startup and chunk inactivity |
| `--keep-alive-timeout-ms` | `GCS_MIMIC_KEEP_ALIVE_TIMEOUT_MS` | 120000 (2 minutes) | 1–600000 (10 minutes); idle time between HTTP/1.1 requests on one connection |
| `--object-request-timeout-ms` | `GCS_MIMIC_OBJECT_REQUEST_TIMEOUT_MS` | 900000 (15 minutes) | 1–86400000 (24 hours) |
| `--catalog-wait-timeout-ms` | `GCS_MIMIC_CATALOG_WAIT_TIMEOUT_MS` | 30000 | 1–60000 |
| `--shutdown-timeout-ms` | `GCS_MIMIC_SHUTDOWN_TIMEOUT_MS` | 5000 | 1–60000 |
| `--max-object-bytes` | `GCS_MIMIC_MAX_OBJECT_BYTES` | 1073741824 (1 GiB) | 1–5497558138880 (5 TiB) |
| `--auth-mode` | `GCS_MIMIC_AUTH_MODE` | `permissive` | `permissive` or `strict`; strict requires an identity file |
| `--compat-profile` | `GCS_MIMIC_COMPAT_PROFILE` | `native` | `native` or `legacy-emulator`; JSON key `compat_profile`; independent of authentication. The source bucket profile supplies `seed_project` when creation omits `project`; see the [contract](slices/bucket-core-parity.md). |
| `--cors-fallback` | `GCS_MIMIC_CORS_FALLBACK` | `none` | `none` or `allow-all`; JSON key `cors_fallback`. XML/public CORS for buckets without stored rules; `allow-all` allows any origin; see [CORS fallback](slices/cors-fallback.md) |
| `--identity-file` | `GCS_MIMIC_IDENTITY_FILE` | none | JSON identity registry, at most 64 KiB; only with `strict` |

To accept HTTP and HTTPS together, run:

```sh
target/debug/gcs-mimic serve --scheme both \
  --listen 127.0.0.1:4443 --listen-http 127.0.0.1:8000 --data-dir data
```

Both listeners use the same catalog, authentication and total connection limit.
Ready retains the primary `listen` and adds a `listeners` array with each actual
address/scheme. An implicit API base uses primary HTTPS, including for requests
made over HTTP; supply `--api-base-url` for another fixed reachable origin.
Port zero allocates each listener independently. Either bind failure prevents
ready and releases the sockets/catalog; shutdown closes both. Compose users
must publish the second HTTP port explicitly. TLS clients still need CA trust
and a matching SAN, as described in the HTTPS section.

The identity file is read once at startup; an invalid registry, `strict` without a
file or a file with `permissive` fails before readiness. Run
`gcs-mimic url public --help` for the URL helper, which accepts no serve options.

HTTP/1.1 connections stay open for further requests, with a 16 KiB parser buffer and
64 headers. Each request has its own deadline. An idle connection closes when no next
request starts within `--keep-alive-timeout-ms` (default two minutes), or at once when
every `--max-connections` slot is taken, so a waiting client can connect; a request
head that starts must finish within the connection timeout (default five seconds).
Object routes use a header deadline followed by renewed chunk-inactivity
deadlines; active transfers can exceed the default five seconds. After confirmed upload EOF,
body inactivity is suspended while storage finishes. Upload/media requests additionally
have an absolute 15-minute ceiling that chunks cannot renew. Expiry before a response
returns a retryable error without publication: 408 `uploadBrokenConnection` for object
uploads and 503 `backendError` for other requests. Expiry during download truncates the
stream after its headers and closes the connection. A deadline response, or a rejection
that leaves the request body unread, closes the connection; the server then keeps
reading and discarding client input for up to 30 seconds, so a rejected upload still
reads its response instead of a connection reset.
For large/slow transfers, use `--object-request-timeout-ms 3600000` for one hour or set
the corresponding environment/JSON value. Advisory checks and post-EOF storage waits each default to 30 seconds,
capped by the remaining absolute request budget, and return 503 `backendError` on expiry.
After a post-EOF timeout the mutation outcome is unknown: the commit may still complete,
so re-read state or retry with generation/metageneration preconditions. Complete bodies
may also commit after disconnect or shutdown. SQLite lock waiting retains five seconds;
background filesystem work and shutdown catalog close have no separate execution bound.
Bucket/probe routes retain a total deadline.
Uploads share the `max_connections` limit (default 32), with excess connections
waiting for capacity. There is no separate upload admission cap. Each upload uses
a two-slot channel with at most 64 KiB
per chunk, and independent size checks before publication. Downloads use one channel
slot and a 64 KiB reader. No transaction or blocking worker waits for incoming body data.
These inspected bounds are not an RSS/cgroup/CPU benchmark; the 1 GiB default cap has not
been verified by a 1 GiB transfer under the Compose limit.
Unknown routes return 404 without body reads; probes retain
GET/HEAD support and body rejection. Catalog work has 32 admitted calls, one
executing job on a dedicated catalog thread, a 2 MiB SQLite cache, at most 64 cached
prepared statements and no memory map. The runtime bounds the blocking threads used
for file I/O; catalog jobs do not wait for them. Queued/cancelled operations retain
directory ownership until execution finishes. Before confirmed EOF, a timeout/disconnect aborts the upload without
publishing partial bytes. Early rejections send a response and half-close the socket, then
discard at most 256 KiB for at most 100 ms. Generic admitted catalog writes and completed
upload bodies can still commit after the client disconnects; clients must read back state
when the response is lost. Trickle uploads cannot renew the absolute request ceiling;
cancelled uploads remain tracked until their cleanup finishes. Upload prechecks
and resumable session creation/read/checkpoint calls wait for catalog admission
within the catalog wait deadline; other catalog calls still reject saturation.

Schema-v1 bucket catalogs contain no objects and migrate transactionally through v3,
v4 and v5 to v6. Existing v3 objects gain default metadata columns; v5 adds the durable
seed initialization marker, and v6 adds default bucket IAM policies and an etag
counter. Migrated catalogs do not import fixtures. Unreleased schema-v2 catalogs
are rejected without changing their data; automatic recovery of deleted v2 generations is
unavailable. Recovery ignores `.DS_Store` and `._*` OS metadata; other invalid storage
entries stop startup with a path diagnostic. Object `selfLink` addresses the current
resource without a generation query; `mediaLink` selects the returned generation.

## Initialize missing buckets from a file

After building the binary, run this from the repository root:

```sh
target/debug/gcs-mimic serve --scheme http --buckets-file config/buckets.json
```

This creates `initialized-assets` with XML CORS and versioning and
`initialized-private` in a different project with PAP enforced. The stdout ready
event includes `buckets_file` with `created`, `existing` and `project_mismatches`.
Without a selector its outcome is `disabled`; an empty list is `applied` with zeros.
The selector also accepts `GCS_MIMIC_BUCKETS_FILE` or server JSON `buckets_file`,
using CLI > environment > config. Relative paths use the process working directory.

Restart with the same data directory to retain API changes and object versions;
only missing buckets are created, even if settings in the file change. Removed
entries remain stored. Different existing projects warn without changing owners.
Initialization runs before seed, using local administrative authority in strict
and permissive modes, and grants no API permissions. Invalid entries stop startup
with complete diagnostics and no ready event. IAM policies, ACLs, lifecycle, UBLA
and unknown fields are rejected even when empty or disabled. Limits are 1 MiB,
1000 buckets and 64 KiB per entry. See the [schema](slices/bucket-initialization.md)
and [read-only Compose example](container-usage.md#initialize-bucket-settings).

## Seed a native catalog and read a byte range

After building the native binary above, run this standalone example in Bash from
the repository root. It uses fresh data and a separate seed directory.

```sh
unset GCS_MIMIC_IMAGE GCS_MIMIC_PORT GCS_MIMIC_API_BASE_URL
seed_demo=$(mktemp -d)
mkdir -p "$seed_demo/fixtures/example-bucket"
printf 'hello world' > "$seed_demo/fixtures/example-bucket/hello.txt"
./target/release/gcs-mimic serve --scheme http --listen 127.0.0.1:0 \
  --data-dir "$seed_demo/data" --seed-dir "$seed_demo/fixtures" \
  --seed-project local-project > "$seed_demo/server.log" 2>&1 &
seed_pid=$!
trap 'kill -TERM "$seed_pid" 2>/dev/null || true' EXIT
for attempt in $(seq 1 100); do
  if jq -e 'select(.event == "ready")' "$seed_demo/server.log" >/dev/null; then break; fi
  kill -0 "$seed_pid"
  sleep 0.1
done
seed_endpoint="http://$(jq -r 'select(.event == "ready") | .listen' "$seed_demo/server.log")"
./target/release/gcs-mimic healthcheck --url "$seed_endpoint/_internal/ready"
curl --disable --noproxy '*' --fail \
  "$seed_endpoint/storage/v1/b/example-bucket/o/hello.txt?alt=media" > "$seed_demo/full.txt"
printf 'hello world' | cmp - "$seed_demo/full.txt"
curl --disable --noproxy '*' --fail-with-body -sS -D "$seed_demo/range.headers" \
  -H 'Range: bytes=0-4' \
  "$seed_endpoint/download/storage/v1/b/example-bucket/o/hello.txt?alt=media" > "$seed_demo/range.txt"
printf 'hello' | cmp - "$seed_demo/range.txt"
sed -n '1,20p' "$seed_demo/range.headers"
printf 'Range body: '; sed -n '1p' "$seed_demo/range.txt"; printf '\n'
kill -TERM "$seed_pid"
wait "$seed_pid"
trap - EXIT
jq . "$seed_demo/server.log"
rm -r "$seed_demo"
```

Expect 206, `Content-Range: bytes 0-4/11`, `Content-Length: 5` and `hello`.
Closed/open/suffix ranges use stored bytes; pin `generation` when assembling
multiple requests. Unsatisfiable ranges return 416. If-Range and required gzip
decompression return 501. See the [range contract](slices/json-object-range.md)
for local malformed/multiple-range fallback rules and header-parity limits.

Each top-level seed directory names a bucket; nested regular files name objects
with `/` separators. Empty bucket directories work. Imported objects take their
MIME from the pinned filename-extension table (`seed_content_type=extension`,
the default); `octet-stream` stores
`application/octet-stream`. Size, MD5 and CRC32C come from
their bytes. Keep fixtures stable and read-only throughout startup. Symlinks,
special files, invalid names and overlapping roots fail before readiness.
Traversal is limited to 10000 entries and 32 nested directories;
`max_object_bytes` applies per new file. No sidecar metadata is imported.

Seeding runs on every startup while a seed directory is
in effect. Missing buckets are created and existing ones keep their settings;
a bucket owned by a project other than `seed_project` stops startup. A missing
object is created. An object exactly matching what seeding would write (same
bytes and seed Content-Type, with no Content-Encoding, Content-Disposition,
Content-Language, Cache-Control, custom time or custom metadata) keeps its
generation. Any other difference, including API metadata edits, is replaced as
a new live generation with seed default metadata (versioned buckets keep the
previous one as noncurrent). A differing object under a hold or retention is
left unchanged and reported as `blocked`; holds and storage class are not
compared. Objects without a seed file are untouched, and removing a
seed file never deletes an object. Restart to apply fixture edits; there is no
watcher or reseed endpoint. The ready event's `seed` field reports `buckets` and
`objects` (newly created), `replaced`, `unchanged`, `blocked` and
`skipped_os_metadata`. 0.2.1 and earlier imported seed files once into a fresh
catalog with an `octet-stream` default; see the
[0.3.0 release notes](releases/0.3.0.md). Import is administrative in both profiles; strict API reads
still need a permitted local bearer token. See the [seed contract](slices/seed.md).

### Seed with Compose

Use a separate project with a new volume. Build its image from this checkout;
the seed source is the checked-in `fixtures/local-bucket/hello.txt`.

```sh
export COMPOSE_PROJECT_NAME=gcs-mimic-seed-demo-$$
export GCS_MIMIC_IMAGE="gcs-mimic:seed-demo-$$"
docker build -t "$GCS_MIMIC_IMAGE" .
export GCS_MIMIC_PORT=0
docker compose -f compose.yaml -f compose.http.yaml -f compose.seed.yaml up -d --wait
seed_address=$(docker compose -f compose.yaml -f compose.http.yaml -f compose.seed.yaml port gcs 4443)
curl --disable --noproxy '*' --fail "http://$seed_address/storage/v1/b?project=local-project"
curl --disable --noproxy '*' --fail "http://$seed_address/storage/v1/b/local-bucket/o"
curl --disable --noproxy '*' --fail \
  "http://$seed_address/storage/v1/b/local-bucket/o/hello.txt?alt=media"
docker compose -f compose.yaml -f compose.http.yaml -f compose.seed.yaml stop
```

The last response is `Hello from the emulator.` followed by a newline. The override
mounts fixtures read-only. Keep both `-f` arguments on restart/recreate commands;
add `-f compose.strict.yaml` and the local admin bearer token for strict mode.
Ordinary `stop` or `down` preserves data. To delete only this disposable demo's
emulator data, explicitly run `docker compose -f compose.yaml -f compose.http.yaml -f compose.seed.yaml
down --volumes`; the next `up -d --wait` imports into the fresh volume. A plain
restart also reapplies the fixtures as described above.
This complete guide, including bucket versioning/default holds, resumable upload,
seeded Compose and strict bucket IAM/public/private journeys, passed on 2026-10-22;
see the [final bucket integration evidence](evidence/bucket-core-parity.md).
