# Go source memory capture

<!-- markdownlint-configure-file {"MD013": false} -->

The dedicated [source exporter](../../../go/sourceexport/README.md) reads an application-quiesced fake-gcs-server v1.56.1 instance. It is a migration adapter, separate from the ordinary subprocess facade and Rust runtime. Its exact output contract is [the memory snapshot format](source-memory-snapshot.md); the feature ledger owns status.

## Pinned source and dependencies

The Go module proxy and GitHub tag were checked on 2026-10-30. Both resolve v1.56.1 to `896ece51e49eed0946089f3089f6343855c0d106`. The comparison module is intentionally pinned rather than upgraded to a different source contract. Its compressed Go module archive is 188125 bytes, SHA-256 `27643a0846d610906fa817c979768d40fb57728fafd3623a199eeb553bd55e13`, licensed BSD-2-Clause. This is source archive size, not a runtime or binary-size measurement.

The source surfaces used are `Server.Backend`, backend `ListBuckets`, `ListObjects` and `GetObjectWithGeneration`. The source's internal packages are not imported, copied or reimplemented: Go type inference permits reading the public method results, and their existing JSON codec supplies PascalCase source attributes.

| Source file at the pinned revision | SHA-256 |
| --- | --- |
| `internal/backend/storage.go` | `b7f16162936b48224b8e72c73f17defcc933950abd9d388c7d4f46bac7c3480f` |
| `internal/backend/memory.go` | `d466e797aa68c85d76d5125064782389afc944387d9156b2e49291f801c1d5b1` |
| `internal/backend/object.go` | `d5fdc4350a09071849b39281d566d76a0799982d11d3082d7f6d211bc7094d12` |
| `internal/backend/bucket.go` | `c3d6a664ea330eabe6b5bc81de29999240c3e4b33c8a3185cf9f087ea5268518` |

`golang.org/x/sys` was already a source transitive dependency and becomes a direct migration-module dependency at v0.49.0 for race-safe, no-replace directory publication on Linux/macOS. The official proxy reports this as its latest stable release, revision `b6b8557e8e2974925aec7a136ba0e323eedc5f43`; the version's manifest has no retraction and requires Go 1.26.0. Its module archive is 2115196 bytes, SHA-256 `e73a06591ca0d7016ce3ccb45c36a8ed7128468aecc1a357f94d815c760c6b31`. The publisher repository is active and not archived, with BSD-3-Clause licensing. These are source package sizes, not measured binary growth. The module has its own `go.mod`/`go.sum`, Go 1.27.3 toolchain and Go 1.27 language baseline. No dependency is added to Cargo or the ordinary Go facade. Central daily dependency discovery must include the new nested module before release.

## Acceptance and evidence

- `TestExportRealSourceHistoryAndExactMetadata` starts the actual source memory backend with `NoListener`, captures an empty bucket, unusual original names, live generation 7 after archived generation 900, and an archived-only generation. It compares exact selected-generation source JSON attributes and raw bucket creation timestamps, verifies selected bytes and the complete manifest digest/totals.
- `TestExportBarrierBlocksHTTPAndDirectWrites` holds capture under the application-owned exclusive lock and proves both HTTP access paths and a direct mutation cannot enter until capture releases it. All application mutation paths must honor the same lock; uncooperative external processes are unsupported.
- `TestExportFailureKeepsSourceAndDoesNotPublish` verifies cancellation and existing-destination rejection, including no-replace publication over an empty directory, while preserving the source.
- `TestBodyWriteAndSyncFailuresNeverPublishManifestRow` injects failed writes, failed sync, source checksum mismatch and short bodies. No object manifest row or successful object count is emitted after any failure.
- `TestPublishedCaptureSurvivesFinalParentSyncFailureAndRetry` injects a final directory-sync failure after publication. Its typed `PublishedError` identifies the already visible complete capture, which survives failure and cannot be overwritten by retry.
- `TestInvalidUTF8CannotBeSilentlyReplacedByJSON` rejects invalid original names or attribute text rather than letting the JSON encoder replace source bytes.
- `TestAttributeBoundsRejectLargeStringsAndEmptyEntryArraysBeforeEncoding` verifies that ordinary 60 KiB metadata remains accepted while excessive raw strings or element counts are rejected before JSON serialization.
- `TestInterruptedBodyVerificationRemovesOwnedStagingAndKeepsSource` uses a genuinely corrupted source-backend checksum to prove incomplete staging is removed and the original source remains unchanged.

Executed on 2026-10-30: `cargo check --locked --offline --tests`, then, from `go/sourceexport`, `go build ./...`, `go test -race ./...` and `go vet ./...` all passed. `cargo test --locked --offline --lib pinned_source_export_imports_selected_bodies_and_metadata_after_restart` also passed against the actual Go-exported fixture, including empty buckets and selected live/history bytes after two catalog opens. The checked-in `tests/fixtures/import/memory-snapshot.jsonl` is byte-identical to that capture, SHA-256 `ab6971b15bc96ae3ef77a93efefc09791e3c652d6bce4103bd4089296a46a975`. Its actual bucket timestamps have six fractional digits on the tested macOS host; nonzero sub-microsecond source capture is not established by this fixture. Separate synthetic reader cases exercise nonzero nanosecond preservation. Heavy SDK/container checks and physical power-loss tests were not run for this slice.

## Sources

- [Pinned source backend API](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/internal/backend/storage.go)
- [Pinned memory backend](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/internal/backend/memory.go)
- [Pinned source module metadata](https://proxy.golang.org/github.com/fsouza/fake-gcs-server/@v/v1.56.1.info)
- [Official x/sys module metadata](https://proxy.golang.org/golang.org/x/sys/@v/v0.49.0.info)
