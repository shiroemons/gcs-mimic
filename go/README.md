# Go packages

The module `github.com/shiroemons/gcs-mimic/go` has two packages. Both start
the Rust executable as a child process; neither embeds the emulator.

| Package | Use it for |
| --- | --- |
| `gcsmimic` | New tests. gcs-mimic's own API, with helpers for per-test buckets, fixtures and reset |
| `fakestorage` | Tests that move from fake-gcs-server and keep its Go API; change only the import path |

## Install

```sh
go get github.com/shiroemons/gcs-mimic/go@latest
```

The module needs Go 1.26 or newer. It belongs to one gcs-mimic release, and a
release tags it `go/vX.Y.Z` once its archives are published, so
`go get github.com/shiroemons/gcs-mimic/go@vX.Y.Z` pins that release; until a
tag exists, `@latest` takes the newest commit of `main`. Both packages share
the module's version, so during `0.x` a minor release may change either of
them.

Nothing else is needed: on first use the packages download the gcs-mimic
binary of their release, as described next.

## Finding the binary

The packages start the first of these:

1. `WithExecutable` (`gcsmimic`) or `Options.Executable` (`fakestorage`), used as given.
2. `GCSM_BINARY`, used as given.
3. `gcs-mimic` on `PATH`, when its `--version` reports a
   [supported release](#supported-binaries); another release is skipped.
4. The module's own release, kept in the user cache directory as
   `gcs-mimic/VERSION/TARGET/gcs-mimic` below `os.UserCacheDir()`
   (`~/Library/Caches` on macOS, `~/.cache` on Linux, `%LocalAppData%` on
   Windows), and downloaded from GitHub Releases the first time.

The download takes the release archive for the platform: macOS and Windows on
x86-64 and ARM64, and on Linux the static musl build for x86-64 and ARM64,
which runs on any distribution. It checks the archive against the SHA-256 in
`internal/mimicproc/SHA256SUMS`, a copy of that release's `SHA256SUMS`. The Go
checksum database fixes that file together with the module, so an archive
replaced on GitHub fails the check instead of running. Only the executable is
extracted, and it is put in place by a rename, so test processes running in
parallel share one complete copy. The download is bounded to five minutes and
goes through `HTTPS_PROXY` when set. On another platform, build gcs-mimic from
source and set `GCSM_BINARY`.

`GCSM_DOWNLOAD=off` turns the download off, for machines that must not reach
GitHub; the lookup then ends with `gcs-mimic` on `PATH`, whatever its release.
Unset or `on` allows the download, and any other value fails the start. In CI,
cache the directory (for example `~/.cache/gcs-mimic` on Linux runners) to
download once rather than on every run.

A module revision that records no SHA-256 for its own release cannot
download. That is `main` between the commit that prepares a release and the
commit that records the published `SHA256SUMS`; set `GCSM_BINARY` there.

## Supported binaries

The packages pass options and read ready fields of their own release, so they
start only a binary of a supported release. During `0.x` that is the same
minor release as the module, such as any `0.20.y` for the `0.20.0` module,
because a `0.x` minor release may break; from `1.0.0` it is the same major
release at or above the module's minor. They read the `version` of the ready
event, and when a binary exits before ready they ask `--version`. Another
release fails with `*gcsmimic.UnsupportedBinaryError` or
`*fakestorage.UnsupportedBinaryError`, which are the same type; it names the
binary's path, both versions and the package that refused it. Use `errors.As`
to detect it. A binary built from the same checkout always matches.
The former `GCS_MIMIC_BINARY` fails with replacement advice when neither
`GCSM_BINARY` nor the package's executable option selects a binary. Inherited
`GCS_MIMIC_*` server settings are removed from each child so that the package's
options remain its only configuration; `GCSM_*` client variables pass through
unchanged.

Close asks the child to stop, forcibly kills it after ten seconds, and waits
for it to exit. On Windows the child runs in its own process group and receives
CTRL_BREAK, since Windows cannot send an interrupt signal to another process.
A child outlives a parent process that is itself killed.

## gcsmimic

```go
import "github.com/shiroemons/gcs-mimic/go/gcsmimic"

var server *gcsmimic.Server

func TestMain(m *testing.M) {
    s, err := gcsmimic.New(context.Background())
    if err != nil {
        log.Fatal(err)
    }
    server = s
    code := m.Run()
    if err := s.Close(); err != nil {
        log.Print(err)
    }
    os.Exit(code)
}

func TestUpload(t *testing.T) {
    t.Parallel()
    bucket := server.TempBucket(t)
    server.Seed(t, gcsmimic.Dir("testdata/fixtures", gcsmimic.Into(bucket.BucketName())))
    // Point the code under test at server.URL() or bucket.
}
```

One server per package and a `TempBucket` per test keeps parallel tests apart.
`gcsmimic.Start(t)` instead runs a server for one test and stops it when the
test ends.

The server serves the JSON API over plain HTTP on a loopback port with the
native profile, a memory catalog, permissive authentication and local
administration, which the helpers use. Nothing else can reach that listener
unless it shares the machine.

| Function or method | Behavior |
| --- | --- |
| `Start(tb, opts...) *Server` | Starts a server, fails `tb` if it cannot, and stops it at `tb`'s cleanup |
| `New(ctx, opts...) (*Server, error)` | Starts a server for code without a `testing.TB`, such as `TestMain`. `ctx` bounds the startup only |
| `(*Server).Close() error` | Stops the server; every call returns the first call's result |
| `(*Server).URL() string` | `http://127.0.0.1:PORT`, usable as `STORAGE_EMULATOR_HOST` |
| `(*Server).HTTPClient() *http.Client` | Reaches only this server and refuses any other origin before dialing. No overall time limit; bound requests with their context |
| `(*Server).Client(tb) *storage.Client` | An official client, closed at `tb`'s cleanup; fails `tb` instead of panicking |
| `(*Server).NewClient(ctx) (*storage.Client, error)` | An official client the caller closes. Both clients use the JSON API and never look for credentials |
| `(*Server).Version() string` | The release the server reported |
| `(*Server).TempBucket(tb, opts...) *storage.BucketHandle` | A bucket for this test only, deleted at cleanup |
| `(*Server).Seed(tb, items...)` | Writes fixtures from directories, `fs.FS` values and in-code objects |
| `(*Server).Reset(tb)` | Returns the server to its state at startup |

Start options:

| Option | Default | Effect |
| --- | --- | --- |
| `WithExecutable(path)` | [Found or downloaded](#finding-the-binary) | The binary to start |
| `WithProject(id)` | `local-project` | Project of the startup buckets and of the buckets the helpers create |
| `WithBuckets(names...)` | none | Buckets with default settings, created before ready |
| `WithBucketsDocument(json)` | none | A bucket initialization document (version 1), for startup buckets with CORS, IAM or other settings; not with `WithBuckets` |
| `WithSeedDir(path)` | none | A fixture tree loaded before ready, in one transaction |
| `WithDataDir(path)` | memory | A disk catalog in a caller-owned directory, which `Close` keeps |
| `WithLogs(w)` | discarded | A copy of the server's output; `w` must stay usable until the server stops |
| `WithArgs(args...)` | none | More `serve` arguments, such as `--memory-max-bytes`. Arguments the package sets itself are refused |

A failed start says why: a missing executable names `GCSM_BINARY` and
`WithExecutable`, a failed download names the archive, the reason and any
skipped `gcs-mimic` on `PATH` with its version, another release returns
`UnsupportedBinaryError`, an exit before ready includes the last 4 KiB of the
server's output, and a start that takes over 30 seconds says so.

### TempBucket

`TempBucket` creates the bucket through the JSON API in the server's project
and returns its handle. Its name is `tmp-`, the test's name in lower case with
other characters turned into dashes and shortened, and a part unique to the call
and the run, within Cloud Storage's 63-byte limit. `WithBucketAttrs(attrs)`
creates it with `storage.BucketAttrs` settings such as `VersioningEnabled`,
`CORS`, `Labels`, `RetentionPolicy` or `DefaultEventBasedHold`, leaving
`Name` empty; `WithObjectRetention()` enables object retention.

At cleanup the bucket is deleted with every object generation, soft-deleted
object and upload session, even objects under a hold or retention, through
`DELETE /_internal/buckets/{bucket}`. Deletion through the JSON API keeps
enforcing those protections; only this local administration route ignores them,
so tests of retention itself use the API. While an upload session of the bucket
is still open the route answers 503, and the cleanup retries up to five times
within ten seconds before it fails the test. A bucket that the test already
deleted, or that `Reset` removed, is not an error.

### Seed

`Seed(tb, items...)` writes its items in order, one object at a time through
`POST /_internal/objects`, without notifications. It creates each missing bucket
through the JSON API first. The items are:

- `gcsmimic.Dir(path)` and `gcsmimic.FS(fsys)`, such as an `embed.FS` or a
  `fstest.MapFS`, read as `WithSeedDir` reads its tree: each top-level directory
  is a bucket, and each file below it an object named by its path below that
  directory, joined with `/`. Top-level files are skipped and logged, and
  `.DS_Store` and files whose names start with `._` are skipped. A top-level
  directory creates its bucket even when empty; an empty directory below it
  creates nothing. A symbolic link, another special file or a name that is not
  UTF-8 fails the call before anything is written.
- `gcsmimic.Into(bucket)` on a `Dir` or `FS` writes every file into `bucket`,
  named by its path below the tree root. It lets fixed test data fill a
  `TempBucket`, whose name changes on every run.
- `gcsmimic.Object{Bucket, Name, Content, ContentType}`, stored with the
  content type as written, or `application/octet-stream` without one.

Files of a `Dir` or `FS` take their content type from their name's extension
and their owner as the startup seed sets them, because the server applies its
own seed rules (the request's `"defaults": "seed"`). Other differences from the
startup seed remain:

| | Startup seed (`WithSeedDir`) | `Seed` |
| --- | --- | --- |
| The same content written again | Keeps the generation | Creates a new generation |
| Atomicity | One transaction | One object at a time; a failure stops and fails the test, keeping what was written |
| Creation time | When the tree is published | When each object is written |

### Reset

`Reset(tb)` deletes every bucket and object, including `TempBucket` buckets and
the data of tests running in parallel, then applies the startup buckets and
fixtures again in the same step, so a startup bucket never goes missing. It
uses `POST /_internal/reset` with `"state": "startup"` and ignores holds and
retention. A server busy with an open upload makes no changes; `Reset` retries
it like the `TempBucket` cleanup, then fails the test and says that nothing
changed. Use `TempBucket`, not `Reset`, in parallel tests.

## fakestorage

`fakestorage` maps selected fake-gcs-server Go APIs to the Rust executable, for
code that moves from fake-gcs-server. New tests should use `gcsmimic`.
Each instance owns a Rust subprocess and a bounded memory catalog by default.
`StorageRoot` selects a native durable disk catalog. On Unix, `NoListener` uses
HTTP over a private Unix socket and opens no TCP or gRPC listener. `URL()` is
empty unless `ExternalURL` is supplied. Rust still runs in a separate process.
Windows rejects `NoListener` explicitly, without a TCP fallback. The child runs
with the `fake-gcs-server` compatibility profile and HTTPS by default.

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
blob memory at about half of the available memory, from 64 MiB to 1 GiB, and
catalog pages at 16 MiB; `MemoryMaxBytes` and
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

## Tests

```sh
cd go
go build ./...
go test -race ./...
go vet ./...
# Final integrated runtime only, with GCSM_BINARY already set:
go test -race -tags integration ./...
```

The normal unit tests use the Go test executable as a controllable child
(`internal/fakechild`), and the `gcsmimic` helper tests also use a local test
HTTP server that checks each request. Ordinary listener cases require loopback
permission; NoListener cases use Unix sockets. They do not start the Rust
runtime. The download tests serve archives from a local test HTTP server into a
temporary cache, so no test reaches GitHub. The examples are compiled but not
run, because running them needs a gcs-mimic binary. Dedicated Rust wire tests verify Unix CRUD, preconditions, parallel
connections and socket cleanup independently.
`cmd/sdk-guard` normally restricts the HTTP origin. Its explicit `--grpc-endpoint`
mode provides one local HTTP/1 and h2c frontdoor for the standalone Rust SDK,
with fixed loopback JSON and gRPC backends, eight active requests and bounded
copy buffers. Foreign hosts/ports, metadata requests and CONNECT are rejected;
the wrapper fails if either client bypasses the guard. This test adapter does
not unify the product listeners or provide an OS network sandbox.
