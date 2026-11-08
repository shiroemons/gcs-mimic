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
  > "$native_dir/server.log" 2>&1 &
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
curl --disable --noproxy '*' --fail "$native_endpoint/storage/v1/b/native-bucket?projection=full" > "$native_dir/after.json"
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
fake-gcs-server inputs are translated with a warning naming their `GCS_MIMIC_*`
replacement, inputs without an equivalent fail with native migration advice,
unknown `FAKE_GCS_*` names warn, and native values preserve their existing
precedence. See
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
and `-log-level` are translated with a warning: `warning` becomes `warn`, `trace`
becomes `debug`, and `fatal` or `panic` become `error`.

| CLI option | Environment | Default | Range |
| --- | --- | --- | --- |
| `--listen` | `GCS_MIMIC_LISTEN` | `127.0.0.1:4443` | Numeric IP:port; port 0 allowed |
| `--log-level` | `GCS_MIMIC_LOG_LEVEL` | `info` | `debug`, `info`, `warn`, `error`; `warning` aliases `warn`; JSON key `log_level` |
| `--scheme` | `GCS_MIMIC_SCHEME` | `https` | `http`, `https` or `both`; both uses primary HTTPS and secondary HTTP |
| `--listen-http` | `GCS_MIMIC_LISTEN_HTTP` | Primary listen IP:8000 | Numeric IP:port, port 0 allowed; requires `scheme=both`; reject equal nonzero endpoints |
| `--grpc-listen` | `GCS_MIMIC_GRPC_LISTEN` | none (disabled) | JSON key `grpc_listen`; numeric IP:port (for example `127.0.0.1:4444`), port 0 allowed; separate plaintext h2c listener. Set Go `STORAGE_EMULATOR_HOST_GRPC` to its address. It serves the scoped Storage v2 bucket, object, write and read RPCs and the legacy Storage v1 migration profile; other RPCs and unsupported fields return actionable, non-retryable `UNIMPLEMENTED`. See the [Storage v2 bucket](slices/grpc-v2-buckets.md), [object I/O](slices/grpc-v2-object-io.md), [update/copy](slices/grpc-v2-update-copy.md), [bidi read](slices/grpc-v2-bidi-read.md) and [Storage v1](slices/grpc-v1-legacy.md) contracts. |
| `--trusted-listen` | `GCS_MIMIC_TRUSTED_LISTEN` | none (disabled) | JSON key `trusted_listen`; numeric IP:port, port 0 allowed; requires `auth_mode=strict` and `trusted_principal`, rejects `unix_socket` and another listener's address. An extra plain HTTP listener, whatever `scheme`, for the application's own servers: a request without credentials acts as `trusted_principal` under the normal strict checks, credentials authenticate as on the main listener, and a request with `Origin` or a `Sec-Fetch-Site` other than `none` gets 403. No gRPC. Never publish it; see the [trusted listener](container-usage.md#keep-private-buckets-private-with-a-trusted-listener) |
| `--trusted-principal` | `GCS_MIMIC_TRUSTED_PRINCIPAL` | none | JSON key `trusted_principal`; an identity of the identity file, or the built-in test signer with `test_signer=enabled`; requires `trusted_listen`. Ready reports it with the bound `trusted_listen` |
| `--tls-cert` | `GCS_MIMIC_TLS_CERT` | built-in development certificate | Optional PEM leaf-first chain, at most 1 MiB; requires matching key |
| `--tls-key` | `GCS_MIMIC_TLS_KEY` | built-in development key | Optional matching unencrypted PEM private key, at most 1 MiB; requires certificate |
| `--tls-handshake-timeout-ms` | `GCS_MIMIC_TLS_HANDSHAKE_TIMEOUT_MS` | 5000 | 0 disables, no upper limit; HTTPS only, within connection admission |
| `--data-dir` | `GCS_MIMIC_DATA_DIR` | `data` relative to cwd | Nonempty path; exclusive catalog ownership |
| `--backend` | `GCS_MIMIC_BACKEND` | `disk` | `disk` or `memory`; memory never opens the data directory and loses all data at exit; see the [RAM backend](slices/memory-backend.md) |
| `--seed-dir` | `GCS_MIMIC_SEED_DIR` | none | JSON key `seed_dir`; stable read-only bucket-directory input applied before readiness on every startup. Wins over `default_seed_dir` |
| `--default-seed-dir` | `GCS_MIMIC_DEFAULT_SEED_DIR` | none; the image sets `/data` | JSON key `default_seed_dir`. Used only when `seed_dir` is unset, the directory exists and it does not overlap the data or sync directory; otherwise silently ignored |
| `--seed-content-type` | `GCS_MIMIC_SEED_CONTENT_TYPE` | `extension` | JSON key `seed_content_type`; `extension` uses the pinned filename-extension table, `octet-stream` stores `application/octet-stream`. Also applies to files ingested from `--sync-dir` |
| `--seed-project` | `GCS_MIMIC_SEED_PROJECT` | `local-project` | Existing bucket project validation; administrative import |
| `--max-seed-entries` | `GCS_MIMIC_MAX_SEED_ENTRIES` | 0 (unlimited) | 0–2^64−1; files and directories in `seed_dir`. A seed is one atomic publication, so its memory and time grow with the tree. A positive value stops there and names the setting. Without one, a tree past 10,000 entries logs `seed_tree_large` once per start |
| `--max-seed-depth` | `GCS_MIMIC_MAX_SEED_DEPTH` | 0 (unlimited) | 0–1024; directory levels below a seed bucket |
| `--max-sync-entries` | `GCS_MIMIC_MAX_SYNC_ENTRIES` | 0 (unlimited) | 0–2^64−1; requires `sync_dir`. Files and directories in `sync_dir`, also the watches Linux may use. Time grows with the tree. A positive value stops there and names the setting. Without one, a tree past 10,000 entries logs `sync_tree_large` once per process |
| `--max-sync-depth` | `GCS_MIMIC_MAX_SYNC_DEPTH` | 0 (unlimited) | 0–1024; requires `sync_dir`. Directory levels below `sync_dir`, counting the bucket. Without a limit a walk still stops at 1024 levels, which keeps its descriptors bounded and is deeper than any object name can nest |
| `--buckets-file` | `GCS_MIMIC_BUCKETS_FILE` | none | JSON v1; validate all entries and create missing buckets before seed; existing buckets are preserved. At most 1 GiB, or with `max_buckets_per_batch` set, 1 KiB per allowed bucket and at least 32 MiB |
| `--buckets-file-mode` | `GCS_MIMIC_BUCKETS_FILE_MODE` | `create` | `create` or `reconcile`; JSON key `buckets_file_mode`; requires `buckets_file`. `create` keeps existing buckets as they are; `reconcile` also writes each entry's written fields to its existing bucket, except its location and owner. See [bucket initialization](slices/bucket-initialization.md) |
| `--max-buckets-per-batch` | `GCS_MIMIC_MAX_BUCKETS_PER_BATCH` | 0 (unlimited) | 0–2^64−1; buckets one `--buckets-file` or one seed may declare, not all buckets of the server. Either is one atomic publication, so its memory grows with them |
| `--api-base-url` | `GCS_MIMIC_API_BASE_URL` | Each request's Host and connection scheme | Set it to fix the advertised origin. A Host that selects an XML virtual bucket uses the configured domain with the same port, never the bucket label. An invalid or missing Host falls back to the bound listener URL: wildcard IP becomes loopback and port 0 becomes the allocated port. Explicit absolute HTTP(S) base with optional base path, no credentials/query/fragment. Ready reports the explicit or bound base |
| `--max-connections` | `GCS_MIMIC_MAX_CONNECTIONS` | 1024 | 1–1048576; total across listeners, including TLS handshakes. Startup raises the soft open-file limit to three per connection plus 64, and warns when it cannot |
| `--max-grpc-streams` | `GCS_MIMIC_MAX_GRPC_STREAMS` | 100 | 1–65536; requires `grpc_listen`. Concurrent streams per gRPC connection. Each can buffer a request message of up to 4 MiB and its encoded response, so one connection can reach 400 MiB of requests; lower it to limit that memory. With a gRPC listener, startup also raises the open-file limit toward connections times streams times two, without a warning when that falls short |
| `--max-resumable-sessions` | `GCS_MIMIC_MAX_RESUMABLE_SESSIONS` | 0 (unlimited) | 0–4294967295; active resumable uploads |
| `--max-rewrite-sessions` | `GCS_MIMIC_MAX_REWRITE_SESSIONS` | 0 (unlimited) | 0–4294967295; unfinished rewrites |
| `--worker-threads` | `GCS_MIMIC_WORKER_THREADS` | Available CPUs from 2 to 8 | 1–1024; on Linux the available CPUs honor a container's quota and affinity |
| `--max-blocking-threads` | `GCS_MIMIC_MAX_BLOCKING_THREADS` | 8 | 1–1024 |
| `--header-read-timeout-ms` | `GCS_MIMIC_HEADER_READ_TIMEOUT_MS` | 30000 (30 seconds) | 0 disables, no upper limit; request head arrival, gRPC handshake and HTTP/2 ping acknowledgement |
| `--body-inactivity-timeout-ms` | `GCS_MIMIC_BODY_INACTIVITY_TIMEOUT_MS` | 0 (disabled) | 0 disables, no upper limit; silence while a request body on any route, a response body or a gRPC stream is in flight |
| `--request-timeout-ms` | `GCS_MIMIC_REQUEST_TIMEOUT_MS` | 180000 (3 minutes) | 0 disables, no upper limit; handler time before a response starts, excluding time spent waiting for request body bytes |
| `--connection-timeout-ms` | `GCS_MIMIC_CONNECTION_TIMEOUT_MS` | none | Alias: sets the three settings above to one value; each of them named explicitly wins, from any source |
| `--keep-alive-timeout-ms` | `GCS_MIMIC_KEEP_ALIVE_TIMEOUT_MS` | 120000 (2 minutes) | 0 disables, no upper limit; idle time between HTTP/1.1 requests on one connection and on gRPC connections |
| `--object-request-timeout-ms` | `GCS_MIMIC_OBJECT_REQUEST_TIMEOUT_MS` | 0 (disabled) | 0 disables, no upper limit; absolute upload/media deadline |
| `--catalog-wait-timeout-ms` | `GCS_MIMIC_CATALOG_WAIT_TIMEOUT_MS` | 120000 (2 minutes) | 0 waits without a limit; upload advisory/commit wait, the wait for a busy upload session, and the wait of a reset, reseed, publication or management call for its turn |
| `--shutdown-timeout-ms` | `GCS_MIMIC_SHUTDOWN_TIMEOUT_MS` | 5000 | 0 waits for unfinished requests without a limit |
| `--max-object-bytes` | `GCS_MIMIC_MAX_OBJECT_BYTES` | 5497558138880 (5 TiB) | 1–5497558138880 (5 TiB); also caps gzip upload expansion, so lower it on a shared or exposed server |
| `--auth-mode` | `GCS_MIMIC_AUTH_MODE` | `permissive` | `permissive` or `strict`; strict requires an identity file |
| `--compat-profile` | `GCS_MIMIC_COMPAT_PROFILE` | `native` | `native` or `legacy-emulator`; JSON key `compat_profile`; independent of authentication. The source bucket profile supplies `seed_project` when creation omits `project`; see the [contract](slices/bucket-core-parity.md). |
| `--cors-fallback` | `GCS_MIMIC_CORS_FALLBACK` | `none` | `none` or `allow-all`; JSON key `cors_fallback`. XML/public CORS for buckets without stored rules; `allow-all` allows any origin; see [CORS fallback](slices/cors-fallback.md) |
| `--signed-host-port` | `GCS_MIMIC_SIGNED_HOST_PORT` | `exact` | `exact` or `lenient`; JSON key `signed_host_port`. `lenient` also accepts a V4 signature whose canonical host omits the port of the request Host, as Go storage signs; every other signed part stays bound. Requires signature verification (strict, or `signed_urls=verify`). See [signed URLs](signed-urls.md) |
| `--identity-file` | `GCS_MIMIC_IDENTITY_FILE` | none | JSON identity registry, at most 16 MiB with no limit on its number of identities; with `strict`, or with permissive `signed_urls=verify`, where identities may declare only `id` and `public_keys` |
| `--signed-urls` | `GCS_MIMIC_SIGNED_URLS` | `ignore` when permissive, `verify` when strict | `ignore` or `verify`; JSON key `signed_urls`. Permissive `verify` [checks signed XML requests](signed-urls.md#verifying-signed-requests-in-permissive-mode) as strict mode does, then grants permissive access; strict rejects `ignore` |
| `--test-signer` | `GCS_MIMIC_TEST_SIGNER` | `enabled` when permissive, `disabled` when strict | `enabled` or `disabled`; JSON key `test_signer`. The [built-in test signer](signed-urls.md#built-in-test-signer) `test-signer@gcs-mimic.invalid`, whose private key is published, so anyone can authenticate as it; enabling it in strict mode warns at startup. Local tests only |
| `--default-soft-delete-retention-seconds` | `GCS_MIMIC_DEFAULT_SOFT_DELETE_RETENTION_SECONDS` | 0 (off) | 0, or 1–3155760000; applies to new native buckets created without `softDeletePolicy`, whichever backend. Deleted and replaced objects keep their bytes for this long; 0 frees them at once. Cloud Storage defaults to 604800 and accepts 7 to 90 days, so the default and the range are both local. Buckets that store a policy keep it |
| `--local-creator-email` | `GCS_MIMIC_LOCAL_CREATOR_EMAIL` | `local-user@gcs-mimic.invalid` | Native permissive profile only: new objects record `user-EMAIL` as `owner` and an OWNER ACL entry. Empty keeps `project-owners-N` alone. It labels the writer and grants nothing |
| `--admin-enabled` | `GCS_MIMIC_ADMIN_ENABLED` | `false` | Enables the local `/_internal` administration routes, such as reset and reseed; isolate the listener when enabled |

The table lists the common options. Run `gcs-mimic serve --help` for every option,
including the sync, Pub/Sub event, Unix socket, public URL and XML virtual-host
settings.

To accept HTTP and HTTPS together, run:

```sh
target/debug/gcs-mimic serve --scheme both \
  --listen 127.0.0.1:4443 --listen-http 127.0.0.1:8000 --data-dir data
```

Both listeners use the same catalog, authentication and total connection limit.
Ready retains the primary `listen` and adds a `listeners` array with each actual
address/scheme. Without `--api-base-url`, links use the scheme of the listener each
request arrived on; the base that ready reports, which a request without a usable Host
falls back to, uses primary HTTPS. Supply `--api-base-url` for one fixed reachable origin.
Port zero allocates each listener independently. Either bind failure prevents
ready and releases the sockets/catalog; shutdown closes both. Compose users
must publish the second HTTP port explicitly. TLS clients still need CA trust
and a matching SAN, as described in the HTTPS section.

The identity file is read once at startup; an invalid registry, `strict` without a
file or a file with `permissive` fails before readiness. Run
`gcs-mimic url public --help` for the URL helper, which accepts no serve options.

HTTP/1.1 connections stay open for further requests, with a 16 KiB parser buffer and
64 headers. Each request has its own deadlines. An idle connection closes when no next
request starts within `--keep-alive-timeout-ms` (default two minutes). When a client waits
for a slot at a full server, the connection that turned idle first closes, and only that
one, so pooled clients keep the others. A request head must arrive within
`--header-read-timeout-ms` (default 30 seconds).

Cloud Storage [documents no seconds](sources.md#timeouts-and-limits-behind-the-local-defaults)
for these limits, so the defaults are local choices that a slow client should not reach.
A stalled request body on any route, such as an upload, a metadata PATCH or a batch, and a
stalled download have no limit by default; `--body-inactivity-timeout-ms` sets one, and each
progressing chunk renews it. Upload/media requests have no absolute ceiling by
default; `--object-request-timeout-ms` sets one that chunks cannot renew.
A handler that has not started its response gets `--request-timeout-ms` (default three minutes),
which never counts time spent waiting for request body bytes. Expiry before a response
returns a retryable error without publication, chosen by the limit that expired: a silent
request body answers JSON 408 `uploadBrokenConnection` or XML 400 `RequestTimeout`, an
upload past `--object-request-timeout-ms` answers 408 `uploadBrokenConnection`, and a handler
past `--request-timeout-ms` answers 503 `backendError`. Expiry during download truncates the
stream after its headers and closes the connection. A deadline response, or a rejection
that leaves the request body unread, closes the connection; the server then keeps
reading and discarding client input for up to 30 seconds, so a rejected upload still
reads its response instead of a connection reset. Accepted TCP connections enable TCP
keep-alive (30 seconds idle, then four probes 15 seconds apart), so an idle connection to a
dead peer releases its slot after 90 seconds, even with every limit disabled. A socket with
unacknowledged data falls under the kernel's retransmission limit instead.
Advisory checks and post-EOF storage waits each default to two minutes,
capped by any remaining absolute request budget, and return 503 `backendError` on expiry.
After a post-EOF timeout the mutation outcome is unknown: the commit may still complete,
so re-read state or retry with generation/metageneration preconditions. Complete bodies
may also commit after disconnect or shutdown. SQLite lock waiting retains five seconds;
background filesystem work and shutdown catalog close have no separate execution bound.
Uploads share the `max_connections` limit (default 1024), with excess connections
waiting for capacity in the listen backlog (requested at 1024; macOS lowers it to
`kern.ipc.somaxconn`, 128 by default, so several hundred connections opened in the same instant
can be [reset by the kernel](evidence/zero-config-defaults.md#simultaneous-connections-on-macos)).
There is no separate upload admission cap. Each upload uses
a two-slot channel with at most 64 KiB
per chunk, and independent size checks before publication. Downloads use one channel
slot and a 64 KiB reader. No transaction or blocking worker waits for incoming body data.
Unknown routes return 404 without body reads; probes retain
GET/HEAD support and body rejection. Catalog work has 32 admission slots that later calls
wait for, one
executing job on a dedicated catalog thread, a 2 MiB SQLite cache, at most 64 cached
prepared statements and no memory map. With the disk backend, object and bucket reads also run
on up to eight read-only connections with their own threads, started only as reads overlap, each
with a 1 MiB SQLite cache. Repeated reads of live objects with permissive access answer from
memory until the next catalog write: at most 4096 records and 16 MiB of them, and at most 1024
files kept open or a quarter of the open-file limit, whichever is lower. Object bytes stay in the
operating system's page cache. The runtime bounds the blocking threads used
for file I/O; catalog jobs do not wait for them. Queued/cancelled operations retain
directory ownership until execution finishes. Before confirmed EOF, a timeout/disconnect aborts the upload without
publishing partial bytes. Early rejections send a response and half-close the socket, then
discard at most 256 KiB for at most 100 ms. Generic admitted catalog writes and completed
upload bodies can still commit after the client disconnects; clients must read back state
when the response is lost. Trickle uploads cannot renew the absolute request ceiling;
cancelled uploads remain tracked until their cleanup finishes. A client that drops its
request gives up its place in the catalog queue. Requests that arrive during a reset, reseed
or seed commit wait and then run against the state it left. A client that stays connected
without sending its upload body holds up a reset for up to `catalog_wait_timeout_ms`, and new
uploads wait behind the reset meanwhile. The reset then answers 503 and changes nothing.

Older catalogs migrate automatically, one way and in order, to the current schema 52;
the log names the versions. A binary supporting only schema 51 cannot open a directory
migrated to 52. Version 0.8.0 used schema 48 and 0.7.0 schema 35. Back up the stopped
data directory before upgrading, and restore it with its matching executable to roll back.
Migrated catalogs do not import fixtures. Unreleased schema-v2 catalogs are rejected
without changing their data; automatic recovery of deleted v2 generations is unavailable.
Recovery ignores `.DS_Store` and `._*` OS metadata; other invalid storage entries stop
startup with a path diagnostic. Object `selfLink` addresses the current resource without
a generation query; `mediaLink` selects the returned generation.

## Initialize missing buckets from a file

After building the binary, run this from the repository root:

```sh
target/debug/gcs-mimic serve --scheme http --buckets-file config/buckets.json
```

This creates `initialized-assets` with XML CORS and versioning and
`initialized-private` in a different project with PAP enforced. The stdout ready
event includes `buckets_file` with `created`, `existing`, `project_mismatches`,
`unapplied` and `reconciled`.
Without a selector its outcome is `disabled`; an empty list is `applied` with zeros.
The selector also accepts `GCS_MIMIC_BUCKETS_FILE` or server JSON `buckets_file`,
using CLI > environment > config. Relative paths use the process working directory.

Restart with the same data directory to retain API changes and object versions;
only missing buckets are created, even if settings in the file change, and an
edited entry that was not applied warns on stderr and counts as `unapplied`.
`--buckets-file-mode reconcile` instead writes each entry's written fields to its
existing bucket, except its location and owner. Removed
entries remain stored. Different existing projects warn without changing owners.
Initialization runs before seed, using local administrative authority in strict
and permissive modes, and grants no API permissions beyond an entry's
`iamPolicy`. Invalid entries stop startup with complete diagnostics and no ready
event. ACLs, lifecycle, UBLA, `iam`, policy fields other than `bindings` and
unknown fields are rejected even when empty or disabled. Limits are
16 MiB per entry and a file of 1 GiB. With `max_buckets_per_batch` set, the file instead
has 1 KiB per allowed bucket and at least 32 MiB. See the [schema](slices/bucket-initialization.md)
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
multiple requests. Unsatisfiable ranges return 416, a JSON media download ignores
If-Range as Cloud Storage does, and a multi-range request returns 501. A
gzip object that must be gunzipped ignores Range and returns the whole expanded
object with 200. See the [range contract](slices/json-object-range.md)
for local malformed/multiple-range fallback rules and header-parity limits.

Each top-level seed directory names a bucket; nested regular files name objects
with `/` separators. Empty bucket directories work. Imported objects take their
MIME from the pinned filename-extension table (`seed_content_type=extension`,
the default); `octet-stream` stores
`application/octet-stream`. Size, MD5 and CRC32C come from
their bytes. Keep fixtures stable and read-only throughout startup. Symlinks,
special files, invalid names and overlapping roots fail before readiness.
Traversal has no limit by default, so memory and time grow with the tree; a tree past 10,000
entries logs `seed_tree_large` once per start. A positive `max_seed_entries` or
`max_seed_depth` stops there and names the setting. Seeding uses absolute paths, so on macOS
a path past `PATH_MAX` (1024 bytes) fails with the operating system's error for a name that is
too long. `max_object_bytes` applies per new file. No sidecar metadata is imported.

Seeding runs on every startup while a seed directory is
in effect. Missing buckets are created and existing ones keep their settings;
a bucket owned by a project other than `seed_project` stops startup. A missing
object is created. An object exactly matching what seeding would write (same
bytes and seed Content-Type, with no Content-Encoding, Content-Disposition,
Content-Language, Cache-Control, custom time or custom metadata) keeps its
generation. Any other difference, including API metadata edits, is replaced as
a new live generation with seed default metadata (versioned buckets keep the
previous one as noncurrent). Since 0.8.0, a changed protected fixture
fails the complete prepared-tree replay, preserving the preceding namespace and
emitting no ready event. An identical protected fixture is unchanged. Holds and
storage class are not part of same-body comparison. Objects without a seed file
are untouched, and removing a seed file never deletes an object. Seed input is
not watched. Since 0.8.0, an opt-in [reseed endpoint](slices/atomic-seed-reseed.md)
(with `--admin-enabled`) offers disk merge and RAM replacement. The ready event's `seed` field reports `buckets` and
`objects` (newly created), `replaced`, `unchanged`, `blocked` and
`skipped_os_metadata`; successful atomic replay has `blocked=0`. 0.7.0 and earlier
skipped a changed protected fixture and counted it as `blocked`. 0.2.1 and earlier imported seed files once into a fresh
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
