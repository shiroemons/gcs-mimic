# SDK scorecard: gRPC cases per SDK

<!-- markdownlint-configure-file {"MD013": false} -->

Status: implemented on 2026-11-15; see "Verification".

Scope: the optional comparison harness under `sdk-scorecard/`. The `grpc`
area gets SDK-neutral case IDs that every SDK with a public gRPC API for the
operation emits, instead of Go-only IDs and Java `sdk.grpc_*` cases. No
server behavior, configuration or ledger status changes; the scorecard stays a
comparison, and `compatibility/features.json` remains the only status ledger.

Out of scope, each a later slice: the C++ gRPC transport (see "C++"), a gRPC
column in `summary.md`, SDK-specific cases outside `grpc`, and any server
change a new case motivates.

## Participants

The pinned SDKs' public gRPC APIs were read:

| SDK | Public gRPC API | Participates in |
| --- | --- | --- |
| Go v1.70.0 | `storage.NewGRPCClient`; `NewMultiRangeDownloader` with `WithGRPCBidiReads`; `Writer.Append`, `Flush` and `FinalizeOnClose` | every case below |
| Java 2.78.0 | `StorageOptions.grpc()`; `blobReadSession`; `blobWriteSession`; `blobAppendableUpload` | every case below, plus two write-session cases |
| Python 3.17.1 | `google.cloud.storage.asyncio`: `AsyncGrpcClient`, `AsyncMultiRangeDownloader`, `AsyncAppendableObjectWriter` | `grpc.multi_range_read`, `grpc.appendable_object` |
| Rust 1.22.0 | `Storage::open_object` (bidi read); appendable uploads exist only behind `--cfg google_cloud_unstable_storage_bidi` | `grpc.multi_range_read` |

Python's `GrpcClient` exposes only the generated client of the private
`google.cloud._storage_v2` package, and its `AsyncGrpcClient` adds only
object get and delete, so Python's bucket, write, list and update calls have
no public gRPC API. Ruby, Node.js, PHP and .NET have no gRPC transport. Rust
already sends its core bucket and metadata calls over gRPC (`notes.rust`).

A case lists the SDKs that emit it; an SDK without a public API for it does
not emit it, rather than emitting `n/a`, because `n/a` marks a missing API for
an operation the SDK otherwise supports.

### C++

google-cloud-cpp 3.13.0 builds its gRPC client only with gRPC 1.76 or later
and Protobuf. Pinned from source like the REST build, that adds six archives
(Protobuf, gRPC, grpc-proto, c-ares, RE2 and googleapis) to the Dockerfile and
to daily detection. googleapis and grpc-proto are pinned by commit and publish
no releases, which the source-archive ecosystem cannot yet compare. The C++
gRPC cases are therefore a separate slice that first extends that detection.

## Cases

All in the `grpc` area. Objects named here are written by the case itself;
setup may use HTTP where the SDK's gRPC client cannot (Python), and the
operation the case tests is always gRPC.

| ID | SDKs | Checks |
| --- | --- | --- |
| `grpc.bucket_create_get_delete` | Go, Java | A bucket created over gRPC is read back over gRPC with its name; after a gRPC delete, a gRPC get reports that the bucket does not exist. |
| `grpc.write_read` | Go, Java | 1 MiB + 5 bytes written over gRPC in 256 KiB chunks read back over gRPC whole and as the 5 bytes at offset 10. |
| `grpc.list` | Go, Java | Over a/1, a/2 and b, a `/` delimiter lists exactly the prefix a/ and the name b; prefix a/ lists exactly a/1 and a/2. |
| `grpc.update_delete` | Go, Java | An update that sets metadata k=v reads back over gRPC as exactly that map; after a delete, a gRPC get reports that the object does not exist. |
| `grpc.multi_range_read` | Go, Java, Python, Rust | One bidi read session over 0123456789 returns 012 for (0, 3) and 6789 for (6, 4). |
| `grpc.appendable_object` | Go, Java, Python | After appending 100 KiB + 1 bytes and flushing, the unfinalized object reports that size; appending 50 KiB + 2 more and finalizing gives the total size and both parts' bytes. |
| `grpc.checksum_rejected` | Go, Java | A gRPC write that declares a wrong CRC32C for the object fails with INVALID_ARGUMENT whose message names a checksum or CRC32C, and no object exists afterwards. |
| `grpc.precondition_failed` | Go, Java | A second gRPC write with ifGenerationMatch=0 fails with FAILED_PRECONDITION, and the object keeps its first bytes. |
| `grpc.write_session_bidi` | Java | A gRPC bidi `blobWriteSession` stores 1 MiB + 3 bytes with matching size, CRC32C and bytes. |
| `grpc.write_session_journaling` | Java | A gRPC journaling `blobWriteSession` stores 1 MiB + 3 bytes with matching size, CRC32C and bytes. |

An SDK reports a gRPC status through its own error type; a probe checks the
status code itself where the SDK exposes it, and the HTTP code the SDK maps
it to otherwise (Go's `apierror` keeps both).

### Renamed and moved cases

| From | To |
| --- | --- |
| Go `grpc.multi_range_downloader` | `grpc.multi_range_read` |
| Java `sdk.grpc_basic_object` | `grpc.bucket_create_get_delete`, `grpc.write_read`, `grpc.list` and `grpc.update_delete` |
| Java `sdk.grpc_blob_read_session` | `grpc.multi_range_read` |
| Java `sdk.grpc_appendable_upload` | `grpc.appendable_object` |
| Java `sdk.grpc_write_session_bidi` | `grpc.write_session_bidi` |
| Java `sdk.grpc_write_session_journaling` | `grpc.write_session_journaling` |

Java keeps the checks its cases already make beyond the shared ones, for
example the three 1 MiB ranges and the channel projection of its read
session, and the content type it writes and reads back.

### Stricter Go checks

Go's existing cases checked less than the table: the bucket case did not
compare the name or look the bucket up after deletion, the list case did not
list by prefix, the update case read the metadata from the update's response,
and the appendable case wrote once without a flush. Each now asserts the
catalog checks.

## Harness

- `run.sh` passes `SCORECARD_GRPC_ENDPOINT` to the Python probe, as it does
  to Go, Java, .NET and Rust.
- The Rust probe builds a second `Storage` client on the gRPC endpoint for
  `open_object`, which uses the client's endpoint for its bidi stream.
- Case counts become: Go 64, Ruby 56, Node.js 56, PHP 56, Python 58, Java 80
  (56 core, 10 `grpc`, 14 `sdk`), .NET 73, Rust 57 and C++ 56.

## Expected results

From the gcs-mimic sources, before running: the gRPC adapter implements every
operation above, rejects an object checksum mismatch with INVALID_ARGUMENT
(`object_checksums mismatch; send checksums for the complete object`) and a
failed precondition with FAILED_PRECONDITION. gcp-storage-emulator has no gRPC
listener, so it fails every `grpc` case, as before.

## Acceptance

| Case | Required behavior |
| --- | --- |
| GR-CATALOG | `cases.json` defines the ten cases with their SDK lists and checks and none of the renamed or moved IDs; `score.sh --check-catalog` passes. |
| GR-PROBES | Every participating probe emits its cases through the SDK's public gRPC API and asserts every check; the gRPC client of each stays on the scorecard's gRPC endpoint. |
| GR-JAVA | Java emits the moved cases under the new IDs and keeps its additional checks; its `sdk` area has 14 cases. |
| GR-HARNESS | `run.sh` gives Python the gRPC endpoint; the Areas table in `sdk-scorecard/README.md` counts the catalog; `test-score.sh` passes. |
| GR-RESULTS | All four products are rerun into a new results directory. Each `grpc` case's result is reported per product, and every score change is explained. A case gcs-mimic fails stays failed and is reported with its cause. |

## Found during implementation

- **gcs-mimic gap: Java's bucket read mask.** google-cloud-storage 2.78.0
  for Java sends GetBucket with a default read mask that names `billing`.
  gcs-mimic does not model billing and answers UNIMPLEMENTED (`gcs-mimic does
  not support gRPC bucket read_mask billing`), so Java fails
  `grpc.bucket_create_get_delete`. The former `sdk.grpc_basic_object` created
  a bucket but never read it back over gRPC, so it passed. RPC-STORAGE-002
  records the gap. Resolved later on 2026-11-15: the default mask names
  eight settings gcs-mimic never stores, and a read mask now returns them
  unset, as Cloud Storage does for a bucket without them
  ([grpc-v2-buckets.md](grpc-v2-buckets.md)); a gcs-mimic-only run passes
  the case. The tables below keep the recorded comparison.
- **gcs-mimic gap: Python's appendable open.** `AsyncAppendableObjectWriter`
  in google-cloud-storage 3.17.1 for Python waits for a response to its first
  BidiWriteObject message, which carries only an appendable
  `write_object_spec`. gcs-mimic answers an appendable stream only at a
  flush, a `state_lookup` or finalization, so the open gets no response and
  the probe stops after 20 seconds. The proto does not require that
  response, but the official client depends on it. Resolved later on
  2026-11-15: every appendable open is now answered, a creating stream with
  its unfinalized resource ([grpc-v2-rapid-append.md](grpc-v2-rapid-append.md));
  a gcs-mimic-only run passes the case for Go, Java and Python, and
  BEHAVIOR-RAPID-002 records it. The tables below keep the recorded
  comparison.
- **Status code of a checksum mismatch.** floci-gcp rejects a wrong object
  CRC32C with DATA_LOSS. `grpc.checksum_rejected` requires INVALID_ARGUMENT.
  Cloud Storage was recorded rejecting a wrong CRC32C on JSON uploads with
  400 (`obj-upload-hash-edges` in the
  [provider probes](../evidence/provider-differential-gap-probes.md)), and
  `google.rpc.Code` maps 400 to INVALID_ARGUMENT and DATA_LOSS to 500. The
  gRPC code itself is inferred from these, not observed.
- **Probe defect, fixed.** A failed gRPC call in the Python probe carries
  the RPC error as its `response`, which has no body. Reading the body
  stopped the probe at floci-gcp's first `grpc` failure, before its
  remaining cases. The probe now records the error's message instead
  (`d208ae26`).
- **No gRPC service.** fake-gcs-server gets the gRPC cases on its HTTP
  port, and nothing listens on gcp-storage-emulator's gRPC port. For both, Go
  times out, Java, Python and Rust fail with transport errors, and Python's
  multi-range read runs into the 60-second case limit.

## Verification

Verified on 2026-11-15 at `d208ae26`, linux/arm64, with no uncommitted
change.

| Case | Result | Evidence |
| --- | --- | --- |
| GR-CATALOG | PASS | `bash sdk-scorecard/score.sh --check-catalog sdk-scorecard/cases.json` exits 0; the catalog has the ten cases and none of the six renamed or moved IDs. |
| GR-PROBES | PASS | Every probe emits exactly its catalog IDs; `score.sh` checked all 36 result files of [results/2026-11-15-5](../../../sdk-scorecard/results/2026-11-15-5/summary.md) against the catalog. Against gcs-mimic every `grpc` case passes except Java's `grpc.bucket_create_get_delete` and Python's `grpc.appendable_object`. |
| GR-JAVA | PASS | Java emits ten `grpc` cases and no `sdk.grpc_*` ID; its `sdk` area has 14 cases. |
| GR-HARNESS | PASS | `run.sh` passes `SCORECARD_GRPC_ENDPOINT` to Python; `bash sdk-scorecard/test-score.sh` passes, including the Areas table check. |
| GR-RESULTS | PASS | All four products ran in 18 min 37 s, including the gcs-mimic release build. Every result outside the `grpc` area is the same as in [results/2026-11-15-4](../../../sdk-scorecard/results/2026-11-15-4/summary.md) for all four products. The run records 2,224 results, 2,184 before: 84 `grpc` results replace 24 Go `grpc` results and 20 Java `sdk.grpc_*` results. |

Results of the `grpc` cases. Each cell counts the SDKs that pass out of the
SDKs that emit the case.

| Case | gcs-mimic | fake-gcs-server | gcp-storage-emulator | floci-gcp |
| --- | --- | --- | --- | --- |
| `grpc.bucket_create_get_delete` | 1 of 2 (Java fails: billing mask) | 0 of 2 | 0 of 2 | 2 of 2 |
| `grpc.write_read` | 2 of 2 | 0 of 2 | 0 of 2 | 2 of 2 |
| `grpc.list` | 2 of 2 | 0 of 2 | 0 of 2 | 2 of 2 |
| `grpc.update_delete` | 2 of 2 | 0 of 2 | 0 of 2 | 2 of 2 |
| `grpc.multi_range_read` | 4 of 4 | 0 of 4 | 0 of 4 | 0 of 4 (BidiReadObject is UNIMPLEMENTED) |
| `grpc.appendable_object` | 2 of 3 (Python fails: no response to the open) | 0 of 3 | 0 of 3 | 0 of 3 (appendable objects are not supported) |
| `grpc.checksum_rejected` | 2 of 2 | 0 of 2 | 0 of 2 | 0 of 2 (DATA_LOSS) |
| `grpc.precondition_failed` | 2 of 2 | 0 of 2 | 0 of 2 | 2 of 2 |
| `grpc.write_session_bidi` | 1 of 1 | 0 of 1 | 0 of 1 | 1 of 1 |
| `grpc.write_session_journaling` | 1 of 1 | 0 of 1 | 0 of 1 | 1 of 1 |

Scores, with the fourth 2026-11-15 comparison in parentheses: gcs-mimic 99.5
(99.8), floci-gcp 74.0 (74.4), fake-gcs-server 38.3 (38.9) and
gcp-storage-emulator 35.3 (35.9). Only the Go, Python, Java and Rust
contributions changed, through the `grpc` cases above. The Core scores are
unchanged: 99.8, 74.6, 38.5 and 36.3.

Also passed: `go vet` and `gofmt` for the Go probe, `cargo clippy
--all-targets -- -D warnings` and the case-count tests for the Rust probe,
the `xtask` dependency tests including the pyproject extras, and the Python,
Java and C++ probe builds in the scored run.
