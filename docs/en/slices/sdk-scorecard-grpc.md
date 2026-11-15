# SDK scorecard: gRPC cases per SDK

<!-- markdownlint-configure-file {"MD013": false} -->

Status: contract, 2026-11-15. Not yet implemented.

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
