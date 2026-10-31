# Source memory snapshot exporter

This separate migration module reads fake-gcs-server **v1.56.1**, commit
`896ece51e49eed0946089f3089f6343855c0d106`. The ordinary Go facade and Rust
runtime do not depend on the reference implementation.

`Export` writes the [versioned snapshot format](../../docs/en/slices/source-memory-snapshot.md)
from an application-owned memory instance. It preserves original names, empty
buckets, live and archived membership, exact generation identities and source
attributes, including archive `Deleted` timestamps, classes, ACLs and retention.
Source server options and its transient notification registry are outside this
bucket/object backend format; migrate runtime configuration separately.
It copies selected-generation bodies through a 64 KiB buffer and checks source
MD5 and CRC32C. The manifest has a complete SHA-256 digest and totals.

## Application barrier

The application must pass the **same** `sync.RWMutex` to every HTTP and direct
mutation path before capture is enabled. `Export` holds its exclusive `Lock`
through inventory, body reads and publication. Normal application operations
hold `RLock` until they finish; capture waits for existing operations to finish
and prevents new ones from entering.

```go
var barrier sync.RWMutex
source, err := fakestorage.NewServerWithOptions(fakestorage.Options{
    NoListener: true,
})
// Handle err before using source.
defer source.Stop()

// Use this handler wherever the application exposes source HTTP routes.
handler := sourceexport.Handler(&barrier, source.HTTPHandler())

// Gate the separate in-process HTTP access path too.
httpClient := source.HTTPClient()
httpClient.Transport = sourceexport.Transport(&barrier, httpClient.Transport)
// Construct SDK clients with this httpClient, never source.Client().

// Every direct mutation, including application background jobs, follows this.
func() {
    barrier.RLock()
    defer barrier.RUnlock()
    source.CreateBucket("fixture-bucket")
}()

// Stop or gate all other accesses before starting capture.
err = sourceexport.Export(ctx, source, &barrier, "new-memory-snapshot")
// Handle err; retain the source until native import and verification succeed.
```

Use one wrapper per access path. Do not nest `Handler` and `Transport` around
the same call: recursive `RLock` can deadlock when a capture is waiting.
Do not call `Export` while holding `RLock`. Acquiring the application's
`sync.RWMutex` itself cannot be canceled; cancellation is checked once acquired,
between records and each body chunk.

The source pointer, `Backend()`, source-provided clients, handlers, raw transports
and mutable fixture maps can bypass the barrier. All callers must cooperate,
including clients created before instrumentation and code retaining mutable
metadata or body slices. No wrapper can transparently freeze an independently
running or uncooperative source process. The manifest's `quiesced` declaration
records this application contract; it does not prove that arbitrary code obeyed
it. Do not use this exporter if every mutation path cannot be gated.

## Publication and limits

The destination must be new, in an application-owned parent directory. A private
staging directory holds incomplete output. Each file and containing directory is
synced before atomic publication, and the parent is synced afterward. macOS uses
`RENAME_EXCL` and Linux uses `RENAME_NOREPLACE`; neither overwrites an existing
file, directory or symlink, including one created during capture. Failure cleans
only the owned staging directory. A final parent sync failure returns a typed
`*PublishedError`: the complete capture is visible but directory durability is
unconfirmed. Inspect and verify that complete capture before any retry; retain it
and use a different destination if another capture is necessary. Physical power-loss
behavior has not been verified. Other platforms fail publication explicitly.

Accepted captures contain at most 1000 buckets, 10000 generations and 1 GiB per
body. Attributes are limited to 64 KiB and manifest records to 128 KiB. The pinned
attribute shapes undergo a raw-byte/element preflight before JSON encoding, so
oversized metadata is rejected before an input-sized encoding allocation.
Bucket and object names must fit the native UTF-8 name limits. The pinned
source API itself materializes bucket and per-bucket object metadata lists; these
limits cannot bound its internal allocation for an oversized source instance.
No whole body or whole multi-bucket catalog is additionally collected.

Bucket creation times retain RFC3339 nanoseconds. Object source timestamps retain
their original strings. Importer precision and unsupported metadata checks remain
explicit; exporter success alone does not establish successful lossless cutover.
Keep the old instance quiesced and unchanged until import, selected-generation
verification and any rollback decision finish.

```sh
cd go/sourceexport
go build ./...
go test -race ./...
go vet ./...
```

The tests use the actual pinned source memory backend, with no TCP listener.
They cover live generations lower than archived generations, archived-only names,
empty buckets, unusual names, complete manifest hashes, exact source attributes,
HTTP/direct writer exclusion and failed body writes/syncs. A checksum mismatch,
short body, existing destination or cancellation is an explicit failure.

The actual macOS capture used for native import/restart has six fractional
timestamp digits. The exporter preserves that exact value; it does not establish
nonzero sub-microsecond capture on this host. Nonzero nanosecond import behavior
has separate synthetic coverage in the Rust reader tests.

For the native reader interoperability check, optionally set
`GCS_MIMIC_SOURCE_FIXTURE_DIR` to a new output path when running
`TestExportRealSourceHistoryAndExactMetadata`; otherwise the test removes its
temporary fixture. This is a small migration-format check, separate from final
SDK and container validation.
