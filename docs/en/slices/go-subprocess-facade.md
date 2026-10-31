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
| Lifecycle `Stop` | Delegates to error-returning `Close`; graceful interrupt, ten-second forced-kill bound, child reaping and owned temporary-directory cleanup. `NewServerWithContext` supports startup/lifetime cancellation. |
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
