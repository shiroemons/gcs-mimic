# SDK scorecard: SDK-specific cases

<!-- markdownlint-configure-file {"MD013": false} -->

Status: implemented on 2026-11-15; see "Verification".

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

## Found during implementation

- **gcs-mimic gap: XML multipart part hashes.** A part upload answers with
  an ETag and no `x-goog-hash`. `transfer_manager.upload_chunks_concurrently`
  in google-cloud-storage 3.17.1 for Python validates each part's CRC32C
  from that header by default, so it fails on the first part. With the
  validation turned off, every check of the case, including the wire
  sequence, passed; the probe keeps the default. EXT-XML-023 records the
  gap. fake-gcs-server, gcp-storage-emulator and floci-gcp fail the case too.
- **Open question: a declared length of 0.** google/cloud-storage 2.6.1 for
  PHP writes `gs://` paths through `StreamableUploader`, which starts the
  session with `X-Upload-Content-Length: 0` and then sends chunks whose total
  is unknown (`bytes 0-262143/*`). gcs-mimic takes the 0 as the session's
  total and rejects the first chunk with 400 `Inconsistent upload total`, so
  `sdk.stream_wrapper` fails. fake-gcs-server and floci-gcp accept the
  sequence. No Cloud Storage response is recorded, so
  `SPEC-RESUMABLE-ZERO-DECLARED-LENGTH` in `compatibility/gaps.json` holds
  the question instead of a ledger deviation.
- **Node.js multipart helper.** See "Cases": `uploadFileInChunks` cannot
  reach a custom endpoint, so Node.js does not emit `sdk.xml_multipart_upload`.
- **Size thresholds.** Node.js `downloadFileInChunks` reads an object under
  32 MiB in one request whatever the slice size, so its sliced download uses
  32 MiB + 5 bytes. C++ `ParallelUploadFile` makes one shard below its 32 MiB
  default minimum, so the case sets `MinStreamSize` to 1 MiB.
- **Wire observation.** Each probe records requests at a public extension
  point of its transport: Go wraps the guarded `RoundTripper`, Node.js
  subscribes to `node:diagnostics_channel` HTTP client events, PHP and
  Python add a recording layer to the guarded Guzzle stack and HTTP session,
  Java overrides `HttpTransportOptions.getHttpRequestInitializer`, and C++
  parses the SDK's own `http` log component. PHP now runs its Docker-gateway
  route through Guzzle's cURL handler, because the `gs://` wrapper streams
  reads and Guzzle's stream handler refuses cURL options.
- **Stricter moved checks.** Java's resumed upload must send exactly the
  786,449 remaining bytes to the same session, its sliced download needs
  more than one request with a `Range` header, and its file case uses
  1 MiB + 3 bytes instead of 5 MiB + 3 bytes. The .NET resume checks the
  same byte count, and its rewrite loop counts the calls that carry a token.
  None of these changed a result.

## Verification

Verified on 2026-11-15 at `1f5268a0`, linux/arm64, with no uncommitted
change, after the branch was rebased onto main.

| Case | Result | Evidence |
| --- | --- | --- |
| SD-CATALOG | PASS | `bash sdk-scorecard/score.sh --check-catalog sdk-scorecard/cases.json` exits 0; the catalog has the sixteen cases and none of the seven moved IDs. |
| SD-PROBES | PASS | Every probe emits exactly its catalog IDs; `score.sh` checked all 36 result files of [results/2026-11-15-6](../../../sdk-scorecard/results/2026-11-15-6/summary.md) against the catalog. Against gcs-mimic every new case passes except PHP's `sdk.stream_wrapper` and Python's `sdk.xml_multipart_upload`. |
| SD-MOVED | PASS | Java and .NET emit the moved cases under the new IDs, each with the same result as under its former ID in [results/2026-11-15-5](../../../sdk-scorecard/results/2026-11-15-5/summary.md) for all four products. |
| SD-HARNESS | PASS | `bash sdk-scorecard/test-score.sh` passes, including the Areas table check; `bash sdk-scorecard/test-tls.sh` passes its 32 transport checks with the changed PHP handler. |
| SD-RESULTS | PASS | All four products ran in 24 min 16 s, including the gcs-mimic release build. Every result outside the `sdk` area is the same as in the fifth comparison for all four products. The run records 2,324 results, 2,224 before: 25 new results per product. |

Results of the new and moved cases. Each cell counts the SDKs that pass out
of the SDKs that emit the case.

| Case | gcs-mimic | fake-gcs-server | gcp-storage-emulator | floci-gcp |
| --- | --- | --- | --- | --- |
| `sdk.resumable_resume` | 5 of 5 | 1 of 5 | 4 of 5 | 5 of 5 |
| `sdk.resumable_cancel` | 1 of 1 | 0 of 1 | 0 of 1 | 0 of 1 |
| `sdk.object_update_replace` | 2 of 2 | 2 of 2 | 0 of 2 | 2 of 2 |
| `sdk.rewrite_token_loop` | 3 of 3 | 0 of 3 | 0 of 3 | 2 of 3 |
| `sdk.json_batch` | 2 of 2 | 1 of 2 | 0 of 2 | 1 of 2 |
| `sdk.sliced_download` | 3 of 3 | 3 of 3 | 2 of 3 | 2 of 3 |
| `sdk.xml_multipart_upload` | 0 of 1 (no part `x-goog-hash`) | 0 of 1 | 0 of 1 | 0 of 1 |
| `sdk.parallel_composite_upload` | 2 of 2 | 0 of 2 | 0 of 2 | 1 of 2 |
| `sdk.file_upload_download` | 6 of 6 | 6 of 6 | 6 of 6 | 6 of 6 |
| `sdk.json_reads` | 1 of 1 | 1 of 1 | 1 of 1 | 1 of 1 |
| `sdk.writer_progress` | 1 of 1 | 1 of 1 | 0 of 1 | 1 of 1 |
| `sdk.pager_resume` | 1 of 1 | 1 of 1 | 0 of 1 | 1 of 1 |
| `sdk.download_to_io` | 1 of 1 | 1 of 1 | 1 of 1 | 1 of 1 |
| `sdk.stream_wrapper` | 0 of 1 (declared length 0) | 1 of 1 | 0 of 1 | 1 of 1 |
| `sdk.download_as_stream` | 1 of 1 | 1 of 1 | 1 of 1 | 1 of 1 |
| `sdk.write_unbuffered` | 1 of 1 | 0 of 1 | 0 of 1 | 1 of 1 |

Scores, with the fifth 2026-11-15 comparison in parentheses: gcs-mimic 99.1
(99.5), floci-gcp 74.2 (74.0), fake-gcs-server 39.2 (38.3) and
gcp-storage-emulator 35.7 (35.3). Only the Go, Ruby, Node.js, PHP, Python,
Rust and C++ contributions changed, through the new cases above. The Core
scores are unchanged: 99.8, 74.6, 38.5 and 36.3.

Also passed: `go vet`, `gofmt` and `go test` for the Go probe, `ruby -c`,
`node --check`, `php -l`, Python `py_compile`, the Java and .NET builds,
`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and the
case-count tests for the Rust probe, the C++ build with `-Wall -Wextra
-Wpedantic` and no warning, and Markdown lint of the changed documents.
