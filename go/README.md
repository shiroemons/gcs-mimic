# Go subprocess facade

`fakestorage` maps selected fake-gcs-server Go APIs to the Rust executable.
Each instance owns a Rust subprocess and a bounded memory catalog by default.
`StorageRoot` selects a native durable disk catalog. On Unix, `NoListener` uses
HTTP over a private Unix socket and opens no TCP or gRPC listener. `URL()` is
empty unless `ExternalURL` is supplied. Rust still runs in a separate process.
Windows rejects `NoListener` explicitly, without a TCP fallback.

The module is source code in this checkout; no separately published module is
claimed. Use the checkout's `go/` as a local module replacement when adopting it.
Build the Rust executable first, then set `GCS_MIMIC_BINARY` to that executable
or install `gcs-mimic` on `PATH`.

```go
import "github.com/shiroemons/gcs-mimic/go/fakestorage"

server, err := fakestorage.NewServerWithOptions(fakestorage.Options{
    Scheme: "http",
    InitialObjects: []fakestorage.Object{{
        ObjectAttrs: fakestorage.ObjectAttrs{
            BucketName: "fixtures", Name: "hello.txt", Generation: 420000,
            ContentType: "text/plain", Metadata: map[string]string{"fixture": "yes"},
        },
        Content: []byte("hello"),
    }},
})
// Check err before use; defer server.Stop(), or check server.Close() explicitly.
```

`server.Client()` returns the official Go Storage client configured for this
instance and without credential discovery. Close every client, streaming object
and server. `NewServerWithContext` also stops/reaps the child on cancellation.
`StorageRoot` denotes native gcs-mimic data; old fake-gcs-server data needs import.
Without it, the memory catalog disappears at process exit. Native defaults cap
blob memory at 64 MiB and catalog pages at 16 MiB; `MemoryMaxBytes` and
`MemoryCatalogMaxBytes` select explicit budgets. These are separate budgets,
not an RSS limit. `Close` removes only the owned Unix socket directory and
preserves any caller-owned `StorageRoot`.

Explicit fixture generations, timestamps and metadata use the opt-in
`POST /_internal/objects` capability. The helper creates missing fixture buckets
in `local-project`. Same-name/same-generation collisions fail with 409, preserving
the prior object. ACL entity/role and object retention use the shared validators;
unsupported identity decorations, deleted/archive identity, ETag and size
injection fail explicitly. Retention-bearing initial fixtures create a missing
bucket with retention enabled; existing buckets are never upgraded silently.
Checksums are validated by the Rust core. A streamed read
spools to a temporary file to preserve `io.ReadSeekCloser` without buffering the
object in RAM; callers must close it. Buffered reads stop at 16 MiB. Listing
pages stop at 1000 entries and use native opaque tokens.

`CreateBucket(name)` and `CreateBucketWithOpts(CreateBucketOpts{...})` support
Name, VersioningEnabled and DefaultEventBasedHold through one generated SDK
create request. Matching duplicate versioning is a no-op, preserving the
existing hold; different versioning or a backend failure panics, as in the pinned
memory backend. These helpers do not update existing bucket settings. Their
mock/race tests pass; actual Rust/SDK hold effects remain final integration work.

`EventOptions` preserves `ProjectID`, `TopicName`, `Bucket`, `ObjectPrefix` and
the `NotifyOn` selections `Finalize`, `Delete`, `MetadataUpdate` and `Archive`.
Set `PubSubEmulatorHost` or `PUBSUB_EMULATOR_HOST` to an explicit local emulator;
production Google publication is unavailable. All four selections false means
no global events. Shared Rust mutations enqueue matching notifications in the
durable outbox (volatile with the memory backend); delivery retains failed
messages for retry. Constructor InitialObjects and startup Seed are quiet;
subsequent CreateObject/SDK writes emit selected events. See
[local Pub/Sub delivery](../docs/en/slices/pubsub-notification-delivery.md).

`Options.AllowedCORSHeaders` is rejected; use bucket CORS. `Backend()`, arbitrary
external transport origins and literal in-process execution remain unsupported.
Use `GetObjectStreaming`, `GetObjectWithGenerationStreaming` and
`ListObjectsWithOptionsPaginated` instead of a backend read where applicable.
Separate calls do not provide a coherent live/history snapshot.
The dedicated [source exporter](sourceexport/README.md) accepts the original
fake-gcs-server `*Server` under an application writer barrier; it does not accept
this subprocess facade. See the [surface mapping](../docs/en/slices/go-subprocess-facade.md).

`ObjectAttrs` JSON uses the source metadata field names and preserves string
identities, timestamp precision, ACLs and retention. Object content is excluded;
the codec does not expand the supported fixture injection fields.

```sh
cd go
go build ./...
go test -race ./...
go vet ./...
# Final integrated runtime only, with GCS_MIMIC_BINARY already set:
go test -race -tags integration ./fakestorage
```

The normal unit tests use the Go test executable as a controllable child. Ordinary
listener cases require loopback permission; NoListener cases use Unix sockets.
They do not start the Rust runtime. Dedicated Rust wire tests verify Unix CRUD,
preconditions, parallel connections and socket cleanup independently.
`cmd/sdk-guard` normally restricts the HTTP origin. Its explicit `--grpc-endpoint`
mode provides one local HTTP/1 and h2c frontdoor for the standalone Rust SDK,
with fixed loopback JSON and gRPC backends, eight active requests and bounded
copy buffers. Foreign hosts/ports, metadata requests and CONNECT are rejected;
the wrapper fails if either client bypasses the guard. This test adapter does
not unify the product listeners or provide an OS network sandbox.
