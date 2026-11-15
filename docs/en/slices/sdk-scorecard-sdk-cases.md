# SDK scorecard: SDK-specific cases

<!-- markdownlint-configure-file {"MD013": false} -->

Status: contract, 2026-11-15. Not yet implemented.

Scope: the optional comparison harness under `sdk-scorecard/`. The seven SDKs
without an `sdk` area (Go, Ruby, Node.js, PHP, Python, Rust and C++) get cases
for workflows their SDK exposes beyond the shared core, and the Java and .NET
cases that measure the same workflows move to the same IDs. No server
behavior, configuration or ledger status changes; the scorecard stays a
comparison, and `compatibility/features.json` remains the only status ledger.

Out of scope, each a later slice: the C++ gRPC transport and any server change
a new case motivates.

## Choice of cases

A case belongs here when an ordinary user of the SDK reaches a workflow
through an API the shared core does not exercise, and gcs-mimic's ledger
records the server side as implemented or partial (resumable sessions,
`objects.update`, `objects.rewrite`, the XML multipart upload, the JSON batch
format). The behaviors kept out of the shared core because most SDKs cannot
express them (resume, cancel, PUT replacement and bounded rewrite
continuation) appear here for the SDKs that have them.

An `sdk` case lists the SDKs that emit it. When two SDKs reach the same
workflow, they share one ID and its checks, as the shared core does, and each
may check more. An SDK without the API does not emit the case.

A helper that only changes how the client walks results the core already
checks is not a case: Ruby's `Bucket#files(...).all` sends the same requests
as the paging in `object.list_paging`.

## Cases

| ID | SDKs | Checks |
| --- | --- | --- |
| `sdk.resumable_resume` | Node.js, PHP, Java, .NET, C++ | An upload of 1 MiB + 17 bytes interrupted after its first chunk is resumed from its saved session (URI, ID or captured state) by a new upload, which sends only the remaining bytes; the object reads back the exact bytes. |
| `sdk.resumable_cancel` | C++ | After `DeleteResumableUpload`, restoring the session fails with an HTTP error and no object exists. |
| `sdk.object_update_replace` | Python, C++ | An update by PUT with custom metadata {c: 3} on an object with {a: 1, b: 2} leaves exactly {c: 3}. |
| `sdk.rewrite_token_loop` | .NET, Rust, C++ | A rewrite of a 3 MiB + 9 bytes object to NEARLINE with a 1 MiB per-call limit takes at least two calls carrying the rewrite token, reports total bytes rewritten equal to the size, and copies the exact bytes. |
| `sdk.json_batch` | Python, Java | One batch request deletes one object and patches another's custom metadata; afterwards the first is gone and the second has the patched metadata. |
| `sdk.sliced_download` | Node.js, Python, Java | A transfer helper downloads an object larger than its slice size to a file, in more than one ranged request, with the exact bytes. |
| `sdk.xml_multipart_upload` | Python | A transfer helper uploads a 10 MiB + 1 byte file in 5 MiB parts through the XML multipart API (initiate, part uploads, complete), and the object reads back the exact bytes. |
| `sdk.parallel_composite_upload` | Java, C++ | A parallel composite upload of a file stores the exact bytes as a composite object (componentCount > 1) and leaves no part objects. |
| `sdk.file_upload_download` | Ruby, Node.js, PHP, Python, Java, C++ | The SDK's file helpers upload a 1 MiB + 3 bytes local file and download it to another local file with the exact bytes. |
| `sdk.json_reads` | Go | A client created with `WithJSONReads` reads an object whole and the range 10 to 19 through the JSON media endpoint (`alt=media`) with the exact bytes. |
| `sdk.writer_progress` | Go | A `Writer` with a 256 KiB `ChunkSize` and a `ProgressFunc` uploads 1 MiB + 5 bytes; the reported offsets increase and end at the size, and the bytes read back. |
| `sdk.pager_resume` | Go | `iterator.NewPager` over five objects with a page size of 2 returns two names and a token; a new pager started from that token returns exactly the remaining three. |
| `sdk.download_to_io` | Ruby | `File#download` into a `StringIO` returns the exact bytes of a 1 MiB + 3 bytes object, and a ranged download into another `StringIO` returns its bytes 10 to 19. |
| `sdk.stream_wrapper` | PHP | With `StorageClient::registerStreamWrapper`, `file_put_contents` and `file_get_contents` on a `gs://` path round-trip the bytes, `scandir` of the bucket lists the object, and `unlink` deletes it. |
| `sdk.download_as_stream` | PHP | `StorageObject::downloadAsStream` returns a stream with the exact bytes of a 1 MiB + 3 bytes object. |
| `sdk.write_unbuffered` | Rust | `write_object(...).send_unbuffered()` from a streaming source of 3 MiB + 7 bytes stores the exact bytes. |

"Only the remaining bytes", "in more than one ranged request", "through the
JSON media endpoint" and "through the XML multipart API" are observed on the
wire: the probe records the requests its guarded HTTP client or transport
sends.

Node.js has `TransferManager.uploadFileInChunks` for XML multipart uploads,
but 9.0.0 builds its URL as `https://<bucket>.<endpoint host>/<object>`,
dropping the endpoint's port and scheme choice, so it cannot reach a local
emulator; Node.js does not emit `sdk.xml_multipart_upload`.

Case counts become: Go 67, Ruby 58, Node.js 59, PHP 60, Python 63, Java 80
(14 `sdk`), .NET 73 (17 `sdk`), Rust 59 and C++ 62.

### Moved Java and .NET cases

| From | To |
| --- | --- |
| Java `sdk.default_retry_resumable_recovery` | `sdk.resumable_resume` |
| .NET `sdk.resumable_resume_from_uri` | `sdk.resumable_resume` |
| .NET `sdk.service_rewrite_token_loop` | `sdk.rewrite_token_loop` |
| Java `sdk.storage_batch` | `sdk.json_batch` |
| Java `sdk.transfer_manager_download` | `sdk.sliced_download` |
| Java `sdk.transfer_manager_parallel_composite` | `sdk.parallel_composite_upload` |
| Java `sdk.create_from_path_download_to` | `sdk.file_upload_download` |

Each keeps the checks it already makes beyond the shared ones, for example the
.NET status query that sends only the missing bytes, the gets and missing
objects in Java's batch, and Java's download to a stream.

## Acceptance

| Case | Required behavior |
| --- | --- |
| SD-CATALOG | `cases.json` defines the sixteen cases with their SDK lists and checks and none of the seven moved IDs; `score.sh --check-catalog` passes. |
| SD-PROBES | Every listed probe emits its cases through the SDK's public API and asserts every check, including the wire observations. |
| SD-MOVED | Java and .NET emit the moved cases under the new IDs and keep their additional checks. |
| SD-HARNESS | The Areas table in `sdk-scorecard/README.md` counts the catalog; `test-score.sh` passes. |
| SD-RESULTS | All four products are rerun into a new results directory. Each new case's result is reported per product, and every score change is explained. A case gcs-mimic fails stays failed and is reported with its cause. |
