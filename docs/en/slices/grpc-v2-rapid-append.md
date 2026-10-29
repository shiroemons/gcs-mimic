# Storage v2 Rapid appendable objects

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: BEHAVIOR-RAPID-002, BEHAVIOR-RAPID-003, BEHAVIOR-RAPID-004 and BEHAVIOR-RAPID-005, the appendable parts of RPC-STORAGE-019 (BidiWriteObject) and RPC-STORAGE-016 (BidiReadObject), and the local policy for BEHAVIOR-RAPID-001.

## Contract sources

- The pinned [Storage v2 proto](https://github.com/googleapis/googleapis/blob/933dd1be8180a22bca2dc473b53d4f4297ee3b0c/google/storage/v2/storage.proto): `WriteObjectSpec.appendable`, `AppendObjectSpec`, `BidiWriteHandle`, `BidiWriteObjectRequest.flush/state_lookup/finish_write`, `BidiWriteObjectResponse.persisted_size/resource/persisted_data_checksums/write_handle` and `Object.finalize_time`. The proto digest and dependencies are unchanged from the foundation slice.
- [Rapid Bucket](https://docs.cloud.google.com/storage/docs/rapid/rapid-bucket) and [Use objects in zonal buckets](https://docs.cloud.google.com/storage/docs/rapid/use-objects-in-zonal-buckets), checked 2026-10-29: appendable objects appear in the namespace as soon as writing starts, can be read while written, have one writer at a time, accept no appends after finalization, keep mutable metadata after finalization, rely on CRC32C only and have no MD5 hash.
- Go `cloud.google.com/go/storage` v1.70.0 (`grpc_writer.go`, `storage.go`, `grpc_reader.go`) as the observed client flow, not as the specification.

## Observed Go v1.70.0 wire flow

- `Writer.Append = true` opens BidiWriteObject with `x-goog-request-params: appendable=true&bucket=...`. The first message carries `WriteObjectSpec{resource, appendable: true}` plus generation preconditions. Small writes are buffered and sent at Close as one message with `flush = true` and `state_lookup = true`.
- `FinalizeOnClose` defaults to `false`, so Close sends no `finish_write`; the object stays appendable. With `FinalizeOnClose`, the last message sets `finish_write` and may carry a CRC32C-only `object_checksums` (no automatic checksum for appendable writes).
- The client switches later attempts to `AppendObjectSpec`, taking `generation` from a response `resource` and `write_handle` from any response.
- `NewWriterFromAppendableObject` (takeover) sends `AppendObjectSpec{bucket, object, generation, metageneration conditions}` without data or flush and blocks until a response carries `persisted_size` or `resource`, which becomes the write offset.
- `NewReader` without `WithGRPCBidiReads` uses ReadObject; `NewMultiRangeDownloader` uses BidiReadObject. The probe `grpc.appendable_object` writes with `Append = true` and reads with ReadObject.

## Acceptance contract

| Case | Behavior |
| --- | --- |
| create | `WriteObjectSpec.appendable = true` creates a new generation in the namespace before finalization. The generation comes from the shared allocator and never changes; metageneration is 1. Creation authorizes `storage.objects.create`. `if_generation_match = 0` against an existing name returns FAILED_PRECONDITION. |
| flush | Each message's data is written at once. `flush` makes all received bytes durable, then publishes the new size and CRC32C. Overlapping resends are skipped; a gap returns OUT_OF_RANGE. Per-chunk CRC32C mismatch returns INVALID_ARGUMENT. |
| state lookup | `flush` or `state_lookup` returns `persisted_size`, `persisted_data_checksums.crc32c` and `write_handle`. The first acknowledgement on a creating stream returns the unfinalized `resource` instead, so the client learns the generation. A clean close without `finish_write` flushes and reports the persisted size again. |
| read while appending | gRPC GetObject, ReadObject and BidiReadObject select the unfinalized generation when no finalized object exists, by name or by its generation, with generation/metageneration conditions. Reads stop at the persisted size; unflushed bytes are never visible. Metadata has no MD5 and no `finalize_time`; `checksums.crc32c` covers the persisted bytes. DeleteObject removes an unfinalized object. |
| takeover | `AppendObjectSpec` with the current generation reopens the object at its persisted size and answers the opening message immediately. Without `write_handle`, metageneration conditions apply. A matching `write_handle` resumes the caller's own writer; a stale handle returns FAILED_PRECONDITION. Every takeover issues a new handle; the previous writer's next write or flush returns FAILED_PRECONDITION. Unflushed bytes of the previous writer are truncated. Takeover requires `storage.objects.create`. |
| finalize | `finish_write` flushes, verifies an optional CRC32C, publishes the persisted bytes through shared publication with the reserved generation and creation time, and returns the finalized `resource`. The object then is an ordinary generation visible to JSON, XML and gRPC listing. A finalized generation rejects further takeover with FAILED_PRECONDITION. MD5 in `object_checksums` returns INVALID_ARGUMENT. A failed finalization keeps the object appendable. |
| durability | The unfinalized state survives restart: persisted bytes stay readable and a takeover continues from them. |
| explicit rejection | Replacing an existing live or unfinalized object with a new appendable generation, `object_size` on an appendable spec and customer-supplied encryption return ErrorInfo-bearing UNIMPLEMENTED. `object_checksums` before `finish_write`, a second spec and `generation <= 0` return INVALID_ARGUMENT; an unknown generation returns NOT_FOUND. WriteObject and StartResumableWrite with `appendable` keep their existing UNIMPLEMENTED rejection. |

## Design

Standard immutable objects and the Rapid state machine are separate. `src/storage/appendable.rs` owns appendable state; `src/grpc/v2/append.rs` is the protocol adapter. Shared files change only at their entry points: BidiWriteObject routes appendable first messages to the adapter, and GetObject, ReadObject, BidiReadObject and DeleteObject fall back to the appendable store when the catalog has no matching object.

- State: catalog schema 34 adds `appendable_objects`, one row per object name: generation, bucket incarnation, opaque blob ID, the shared resumable-options encoding of the written metadata, persisted size and CRC32C, creation/update times, the current write handle and a `finalizing` flag. Bytes live in `<data-dir>/appendable/<blob-id>`, never in a path derived from the object name.
- Durability boundary: the row's size. Data is written as it arrives; `flush` fsyncs the file, then commits size and CRC32C in a short transaction that also checks the write handle and the bucket incarnation. Bytes beyond the row's size were never acknowledged; readers ignore them and the next takeover truncates them.
- Single writer: the row's write handle is authoritative across restarts. A process-local per-object lock serializes file writes, flushes and takeovers, so a superseded writer cannot interleave bytes with the new owner.
- Generation and checksums: creation reserves a generation from the shared allocator, so finalization publishes it unchanged with an imported identity. CRC32C is extended incrementally per append and resumed from the row after a takeover. Unfinalized objects report no MD5.
- Finalization and recovery: finalization marks the row `finalizing`, streams the persisted bytes in 64 KiB chunks through shared publication guarded by `if_generation_match = 0`, then removes the row and file. Publication and removal are separate transactions. Catalog open, before any request, retires rows whose generation is published or whose bucket incarnation is gone, clears `finalizing` on the others (their publication never committed) and removes data files without a row. At runtime a row whose generation is published is treated as finalized.
- Shared publication effects: finalization goes through the same publication as other writes, so the bucket's default object ACL is inherited and the requested `storage_class` is resolved against the bucket default and Autoclass (STANDARD while Autoclass is enabled). The unfinalized view reports the class that finalization will publish. Object retention and customer-supplied encryption keys are rejected on appendable objects (UNIMPLEMENTED), so neither is published silently.
- Bucket lifecycle: rows are bound to the bucket incarnation. A deleted or recreated bucket hides them, makes their writers fail at the next flush, and the next creation or restart removes them.
- Resources: no object body is collected in memory. Each message's data (bounded by the gRPC message limit) is written immediately; finalization copies through a two-slot channel; reads use the shared 64 KiB reader.

## RAPID zonal restrictions (BEHAVIOR-RAPID-001)

Google creates appendable objects in zonal buckets: the location is a zone, the storage class is Rapid, hierarchical namespace and uniform bucket-level access are required, and Object Versioning, soft delete, holds, retention locks, composite objects, resumable uploads, rewrites, object ACLs, CSEK and HMAC are incompatible. gcs-mimic cannot create zonal Rapid buckets (bucket creation rejects the `RAPID` storage class), and SDK probes such as the scorecard use `Writer.Append` on an ordinary bucket. For local development, gcs-mimic therefore accepts appendable objects in any bucket and does not enforce the zonal-bucket restrictions. This is a local simulation policy: BEHAVIOR-RAPID-001 stays NOT_STARTED, and applications must not rely on the emulator to reject configurations that a zonal bucket would reject. Finalized objects follow the bucket's ordinary versioning, soft-delete, hold and retention settings.

## Visibility outside gRPC

Google documents early namespace visibility for appendable objects and no JSON writes to zonal buckets; it does not document JSON or XML reads of unfinalized objects. gcs-mimic shows unfinalized objects only through gRPC GetObject, ReadObject, BidiReadObject and DeleteObject. JSON, XML and gRPC ListObjects see the object after finalization. This differs from Google's namespace visibility and is recorded as a deviation.

## Limitations

- An unfinalized object is not listed, does not block bucket deletion, and is not a source for UpdateObject, copy, rewrite, compose or move (NOT_FOUND).
- Replacing an existing object with a new appendable generation returns UNIMPLEMENTED; delete the object first.
- BidiReadObject reads a snapshot bounded by the size persisted when the stream or read handle opened; reopen to observe later appends.
- A superseded writer learns of the takeover at its next write or flush, not immediately.
- `routing_token` is accepted without effect because the emulator never redirects.
- Finalized objects carry an MD5 hash because shared publication computes it, and `finalize_time` is not reported for any object.
- No provider-exact error messages are claimed.

## Verification

Results on 2026-10-29 from the worktree source:

- `cargo test --locked --lib grpc::v2::append`: PASS, 6 tests: creation, flush, close, ReadObject/GetObject, handle resume and finalization with the reserved generation; unflushed bytes hidden and truncated by a takeover, overlap and gap handling; superseded writers and stale handles; restart durability; explicit rejections and DeleteObject; strict create/viewer permissions.
- `cargo test --locked --lib appendable`: PASS, 6 storage tests: migration from schema 33, default object ACL and Autoclass class on finalization with retention/CSEK rejection, crash after the publication commit (runtime and startup recovery, orphan files), crash before it (row becomes appendable again and finalizes), bucket recreation, size limit, reader bound and read conditions.
- `GCS_MIMIC_GO_TEST_RUN='TestGRPCAppendable|TestGRPCFoundation|TestGRPCBidiRead|^TestGRPCObjectIO' bash scripts/sdk-go.sh`: PASS. Go v1.70.0 `Writer.Append`, ReadObject and MultiRangeDownloader reads of the unfinalized object, JSON invisibility before finalization, `NewWriterFromAppendableObject` takeover at offset 5 with `FinalizeOnClose`, JSON reads of the finalized generation, takeover rejection after finalization, and a superseded writer failing with FAILED_PRECONDITION. The foundation fail-fast case now checks that appendable replacement of a live object returns immediate non-retryable UNIMPLEMENTED.
- `GCS_MIMIC_GO_TEST_RUN='TestGRPC' bash scripts/sdk-go.sh`: PASS, every Go gRPC test (Storage v2 and legacy v1) on the final source.
- Startup recovery takes no SQLite write lock unless there is work; `cargo test --locked --test seed` first failed when recovery issued an unconditional UPDATE while a test held `BEGIN IMMEDIATE`, and passes with the conditional update.
- `bash sdk-scorecard/run.sh --out "$TMPDIR/scorecard-rapid" gcs-mimic`: `grpc.appendable_object` passes; all other Go and Ruby cases match the 2026-10-29 baseline (Go 46 of 57, Ruby 40 of 51).
