# Go embedding migration facade

<!-- markdownlint-configure-file {"MD013": false} -->

This extends the migration gate and the direct fixture scope of DESIGN-068.
It is a selected Go source facade, not full fake-gcs-server embedding parity.
The feature ledger retains the implementation status and broader missing cases.

## Pinned source and contract

The comparison source is fake-gcs-server v1.56.1, commit
`896ece51e49eed0946089f3089f6343855c0d106`, checked 2026-10-30:

- [server.go](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/server.go), Git blob `da158fa063131002fa06dc43d4a26c4f250f6681`, SHA-256 `abe448cd575fbc5687a2527031c3b72fc1ff4b109b12333d1b1d031ab533c005`.
- [object.go](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/object.go), Git blob `4dbe7e768b452a2b23d5407d4d2d660c8c7583ed`, SHA-256 `cfd884eb03aa429f6b681bef8ca1923c600b24beb850c0620121c0c23110a56b`.

The facade implements no GCS catalog, generation allocator, checksum engine,
authorization or publication logic. The Rust runtime owns those operations.
The official Go client owns ordinary API calls. Direct fixture writes use the
[local administrative contract](local-fixture-administration.md), whose explicit
identity injection is an emulator extension rather than a Google upload field.

## Surface mapping

| Source surface | Facade behavior and explicit difference |
| --- | --- |
| `NewServer`, `NewServerWithOptions`, `NewServerWithHostPort` | Start one executable, wait for its structured ready event, then load InitialObjects before returning. Error-returning constructors preserve the cause; convenience constructors panic on failure. |
| Lifecycle `Stop` | Delegates to error-returning `Close`; graceful interrupt, ten-second forced-kill bound, child reaping and owned temporary-directory cleanup. On Windows the child starts in its own process group and receives CTRL_BREAK, because Windows cannot deliver an interrupt signal to another process; an interrupt that cannot be delivered forces the kill at once instead of after the ten seconds. `NewServerWithContext` supports startup/lifetime cancellation. |
| `Client`, `HTTPClient` | Official Go client and a transport restricted to the instance/API/public origins. Known Google SDK hostnames route locally with their original Host; arbitrary hosts fail before dialing. No OAuth or metadata lookup. |
| `HTTPHandler` | Streams to the subprocess and preserves the incoming Host. Calls after Close return 503. Hosting the handler externally still requires an appropriate advertised endpoint. |
| `NoListener` | HTTPClient/HTTPHandler use a private HTTP-over-Unix socket, with no TCP/gRPC listener. `URL()` is empty unless `ExternalURL` is supplied. Windows fails explicitly; this remains subprocess execution. |
| `InitialObjects`, `CreateObject`, `CreateObjectStreaming` | Create missing buckets, stream multipart fixtures and preserve supported generation/timestamp/metadata, ACL entity/role and retention. Retention-bearing missing buckets enable object retention through the official SDK; constructor precreation handles mixed-fixture order, while existing buckets are never upgraded. Explicit identity collision returns 409; no same-generation byte replacement. |
| `GetObject`, `GetObjectWithGeneration` | Read an explicitly selected generation with metadata; buffered content has a 16 MiB safety bound. |
| Streaming get variants, `StreamingObject.Close` | Pin the generation selected by metadata, spool with bounded buffers to a seekable temporary file and remove it on close. |
| List methods and `ListOptions`/`ListResponse` | Prefix, delimiter, versions, offsets, trailing delimiters and at most 1000 records per page; native opaque page tokens differ from source name tokens. Unpaged overflow fails explicitly. |
| Scheme, host/port, external/public URL, TLS pair, seed, location, Writer | Map to explicit Rust runtime settings and verified TLS trust. Host environment settings cannot override the helper's runtime configuration. |
| StorageRoot/default backend | Nonempty StorageRoot selects a native durable disk catalog; otherwise an isolated bounded memory catalog, matching the source backend selection. Never opens old source storage. Native memory budgets are explicit local safety limits. |
| AllowedCORSHeaders | Explicit error; bucket CORS remains available through the SDK. |
| `ObjectAttrs` JSON codec | Source metadata field names, string size/generation, timestamp precision, ACL/project-team and retention round trips; `Object` and `StreamingObject` exclude body content. This codec does not enable unsupported fixture injection. |
| `EventOptions` | Exact ProjectID/TopicName/Bucket/ObjectPrefix and four NotifyOn booleans map to the shared Rust outbox and explicit local Pub/Sub delivery. All-false NotifyOn disables the global rule; incomplete options fail. |
| Initial versus runtime notifications | Constructor InitialObjects and startup Seed suppress notifications through a trusted request-scoped mode; subsequent CreateObject/CreateObjectStreaming and API writes emit selected events. Publication conditions, hashes, generations, origin and sync journal remain active. |
| `Backend()`, fixture deleted/ETag/size and nonempty unmodeled ACL identity decorations | Unsupported. Public read helpers remain available, but separate reads are not a coherent snapshot. Unsupported fixture fields return errors. Original source memory capture uses the separate sourceexport module, not a lookalike backend interface. |

Metadata is capped at 64 KiB and object bodies use a reader chain rather than
a body-sized allocation or producer goroutine. Supplied MD5/CRC32C are checked
by shared publication. The existing Rust object-size, streaming, concurrency,
holds, retention and durability controls remain active. Empty/timestamp/class
validation and failures come from the same fixture route as other adapters.

## Acceptance and verification

The process start, release check, child environment, transport guard and stop
sequence live in `go/internal/mimicproc`, so that every Go package of the module
starts and stops its child alike; the facade passes its own profile, scheme and
options and keeps its public API and messages. The native
[`gcsmimic` package](#native-gcsmimic-package) uses the same code. The
controllable test child lives in `go/internal/fakechild` and serves the tests
of every package.

Normal tests use the Go test binary as a child, without running Rust:

- `TestFacadeLifecycleAndIsolation`: parallel isolated process/catalog ownership, Close idempotence and closed-client rejection.
- `TestNoListenerHandlerAndTransport`: empty URL without ExternalURL, exact bytes through the Unix transport/handler, original Host, 0600 socket, no TCP flags, out-of-origin/redirect rejection and official client construction.
- `TestNoListenerParallelRequestsDeadlineAndCleanup` and `TestNoListenerPreservesExplicitExternalURL`: parallel Unix requests, cancellation deadlines, idempotent close, owned directory cleanup and unchanged explicit advertised Host/URL.
- `TestMemoryAndEventOptionsReachTheRuntimeExactly`: default memory selection, exact budgets and topic/bucket/prefix/four-event mapping; no-selection maps to `none`, not all events. Incomplete configuration fails before process creation.
- `TestOnlyInitialFixturesSuppressNotifications`: only constructor metadata carries the trusted local suppression mode; ordinary fixture metadata retains emission. Constructor context cancellation also reaches fixture requests.
- `TestStartupErrorsCancellationAndOwnedData`: missing executable, nonzero early exit, cancellation before/after ready and preservation of caller-owned data.
- `TestCloseKillsAndReapsUnresponsiveChild`: visible timeout, forced termination and no return before reaping.
- Fixture tests: exact JSON string generation, microsecond timestamps, names/custom metadata, raw bytes, terminator and metadata cap.
- `TestObjectJSONPreservesSourceFixtureMetadata` and `TestObjectJSONDecodeRejectsInvalidIdentityAndClearsAbsentMetadata`: metadata round trips preserve large integer identities, microsecond timestamps, classes, hashes, ACL/project-team and retention; bodies are excluded, invalid input leaves the destination unchanged and absent fields clear stale metadata.
- `cmd/sdk-guard`: exact-origin proxy rejection of cloud, metadata, changed-port and CONNECT requests; ambient proxy/credential configuration is removed.

`go build ./...`, `go test -race ./...` and `go vet ./...` passed on 2026-10-30.
Loopback tests require permission to bind sockets. The integration-tagged
`TestRustRuntimeFixtureSDKHandlerAndParallelIsolation` compiles and is reserved
for the final integrated source: explicit fixture identity, same-generation
collision, SDK writer high-water mark, paged listing, handler bytes and two
parallel independent instances. Runtime execution is not claimed here.
The integration-tagged `TestRustRuntimeEventOptionsKeepInitialFixturesQuietAndPublishAllSelectedEvents`
adds actual four-event local delivery, initial/filtered/failed-write rejection,
and old-generation archive/deletion coverage. It has been compiled without
running; the coordinator owns its final runtime execution.

The module reuses exact Go Storage/API pins from the current Go SDK suite;
`go.mod`/`go.sum` record its separate graph. No dependency is added to Rust.
No performance result or full source Go API parity is claimed.

`CreateBucket` and `CreateBucketWithOpts` support the source's Name,
VersioningEnabled and DefaultEventBasedHold fields. One generated Go API create
request carries all settings: the high-level Storage 1.70.0 create conversion
omits defaultEventBasedHold, so a later PATCH would expose a partial setup.
For a duplicate bucket, matching versioning is a no-op that preserves the
existing hold; different versioning or backend errors panic, matching the
pinned memory backend. This does not turn creation into a settings update or
reproduce every source filesystem-backend difference.

The three helper mock tests pass under `go test -race ./fakestorage -run
TestCreateBucket -count=1 -v`; `go build ./...`, `go vet ./...` and integration-tag
compilation pass. `TestRustBucketHelpersRetainOptionsAndProtectObjects` is
compiled but awaits final-source runtime execution. It checks both helper
forms, duplicate hold preservation and the actual inherited hold effect.

The Unix/memory/event follow-up passed `cargo check --locked --offline --tests`,
`go build ./...`, `go test -race ./fakestorage` and `go vet ./...` on 2026-10-30.
The native [Unix transport contract](unix-http-transport.md) records separate
wire/lifecycle cases. Final integrated SDK execution remains reserved for the
coordinator's final source. Original source capture is covered by the separate
[Go memory exporter](go-source-memory-export.md). Ordinary facade backend access,
literal in-process execution and coherent live/history export remain unsupported;
no complete Go compatibility claim follows from wire or SDK coverage.

## Native gcsmimic package

`go/gcsmimic` is the module's package for new tests, with gcs-mimic's own API;
the facade above stays for migration. [The Go README](../../../go/README.md#gcsmimic)
documents its API and options. Its acceptance cases are the DESIGN-082 rows
`GO-SHARED-PROCESS-CONTROL`, `GO-GCSMIMIC-LIFECYCLE`,
`GO-GCSMIMIC-HELPER-REQUESTS`, `GO-GCSMIMIC-HELPER-RUNTIME` and
`GO-WINDOWS-STOP`.

The child runs `serve --auth-mode permissive --seed-project PROJECT
--admin-enabled true --listen 127.0.0.1:0 --scheme http --backend memory`,
without a compatibility profile; `WithDataDir` selects the disk backend, and
`WithBuckets`, `WithBucketsDocument` and `WithSeedDir` add `--buckets` and
`--seed-dir`. `WithArgs` refuses the flags the package sets. Its HTTP client
forwards only requests to the started origin and has no overall timeout. A start
that exits before ready reports the last 4 KiB of the child's output.

The helpers call three local administration contracts:

| Helper | Request | Handling |
| --- | --- | --- |
| `TempBucket` cleanup | `DELETE /_internal/buckets/{bucket}`, no query or body | 200 succeeds, `cleanupPending` included. 503 is retried as its `Retry-After` asks, capped at 2 seconds per wait, for at most five requests and 10 seconds of waiting. 404 succeeds only when a JSON API read confirms the bucket is gone, so a binary without the route cannot leave buckets unnoticed |
| `Seed` of a `Dir` or `FS` | `POST /_internal/objects` with `{"bucket","name","defaults":"seed","notificationMode":"suppress"}` and the file as the media part | The server chooses the content type and owner as the startup seed does. No generation or timestamp is sent, as `defaults: seed` refuses them |
| `Seed` of an `Object` | The same route with `{"bucket","name","notificationMode":"suppress"}` and `contentType` when set | The media part's `application/octet-stream` applies otherwise |
| `Reset` | `POST /_internal/reset` with `{"confirm":"gcs-mimic","state":"startup"}` | 503 is retried like the purge. Every failure says that the server made no changes; a 400 with reason `invalid` names the startup fixtures, any other 400 an older binary. The counts of the answer are not used |

`Seed` creates missing buckets through the JSON API in the server's project and
walks a tree twice: once to refuse symbolic links, other special files and names
that are not UTF-8 before any request, then to write. The tree rules follow
`src/seed.rs`: top-level directories are buckets, created even when empty; top-level
files are skipped and logged; `.DS_Store` and `._*` files are skipped; empty
nested directories create nothing. `Into(bucket)` instead writes every file of
the tree into one bucket. `TempBucket` names are
`tmp-<test>-<8 hex digits per process>-<sequence>`, at most 63 bytes, with any
spelling of `google` broken by a dash. The official client's create request
drops `DefaultEventBasedHold`, so `TempBucket` sets it with a follow-up update.

Verification on 2026-11-16, Go 1.27.4:

- `go vet ./...`, `GOOS=windows go vet ./...`, `GOOS=linux go vet ./...` and the
  same three with `-tags integration` passed, and `gofmt -l .` listed nothing.
- `go test -race -count=1 ./...` passed every package. The request-level tests
  use a local HTTP server that checks method, path, query, body and retries.
- With `GCSM_BINARY` set to a binary built from the same source as the purge
  route, the startup reset state and the fixture seed defaults,
  `go test -race -count=1 -tags integration ./...` passed every package on
  macOS on 2026-11-16, including `TestRustStartWritesReadsAndKeepsServersApart`,
  `TestRustTempBucketDeletesProtectedObjectsAndEveryGeneration`,
  `TestRustSeedMatchesTheStartupSeed` and `TestRustResetReturnsToTheStartupState`.
  Before those routes existed the last three failed: the purge answered 404
  `Route not found` while the bucket remained, `defaults` was refused with 400,
  and the reset refused `state` with 400 `invalidArgument`.
- On 2026-11-16 a temporary workflow ran the Go packages on `windows-2025`
  (x64) and `windows-11-arm` (ARM64) GitHub runners against a Windows build of
  gcs-mimic from this source. The fake-child tests passed in every package, and
  with `-tags integration` every `./gcsmimic` test passed, each within 0.2
  seconds. Their cleanup fails when `Close` cannot stop the child gracefully, so
  the CTRL_BREAK stop works on both architectures; the child's log shows its
  `stopping` event. In `./fakestorage` the four integration tests that use
  `NoListener` failed, because `NoListener` refuses Windows by design, as it
  does on `main`; the Go CI job runs only on Linux, where they pass.
