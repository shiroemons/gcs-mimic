# Storage v2 bidirectional ranged reads

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: RPC-STORAGE-016 and BEHAVIOR-RPC-004.

The integrated adapter maps the storage domain errors to a Status directly
(`src/grpc/v2/status.rs`), including a storage failure to UNAVAILABLE and an
unsupported setting to ErrorInfo-bearing UNIMPLEMENTED. Like every Storage v2
call it must carry `x-goog-request-params` (INVALID_ARGUMENT without it), and
the routed bucket is compared with the bucket of the first message's
`read_object_spec` once that message arrives. Cloud Storage refused
BidiReadObject for the recording project, so neither the requirement nor the
comparison was recorded for this RPC: both follow by inference from the RPCs
that were (cases `grpc-probe-routing` and `grpc-probe-inferred-routing`). Transport limits and `Storage.max_object_bytes` remain the shared
foundation/object-I/O implementation.

## Acceptance contract

- Every rich error's google.rpc.Status code and message match its outer gRPC status, including storage I/O failures and duplicate read_ids. Existing range details survive normalization. Read handle tokens are compared across all 32 bytes after checking their length.

- Select bucket, object, generation and all four preconditions from the first read_object_spec through shared authorized catalog selection; send metadata and a read_handle first.
- Read multiple read_ids, including later requests on the same stream; preserve increasing offsets per range and emit range_end, including empty ranges. Zero length reads to EOF, negative offsets are relative to EOF and clamp at zero, lengths truncate at EOF, negative lengths and offsets beyond EOF return OUT_OF_RANGE.
- Send CRC32C for every bounded data chunk; interleave ranges without collecting their bodies.
- Reconnect using read_handle by reopening the selected generation with current authorization; omit metadata on reconnection. A deleted or replaced nonversioned generation returns NOT_FOUND and discards the handle; retained versioned generations remain readable. Active streams keep their original reader through overwrite.
- Return NOT_FOUND for absent objects/generations and FAILED_PRECONDITION for failed generation/metageneration conditions. Reject repeated specs, duplicate outstanding IDs and unsupported meaningful fields explicitly.
- Reuse shared strict authorization on initial reads and reconnects; JSON API creates fixtures. Go storage.NewGRPCClient with storage.WithGRPCBidiReads supports ordinary NewMultiRangeDownloader, repeated Add, Wait and Close.
- Bind strict read handles to the selected owning project; recreation of the bucket in another project invalidates the old handle.
- Accept more than 100 outstanding ranges across multiple messages with bounded input backpressure; ordinary Go Add/Wait must complete for 600 ranges larger than a chunk. Reject more than 100 ranges in one message with non-retryable INVALID_ARGUMENT.

## Local boundaries

The pinned [Storage v2 proto](https://github.com/googleapis/googleapis/blob/933dd1be8180a22bca2dc473b53d4f4297ee3b0c/google/storage/v2/storage.proto) and [RPC documentation](https://docs.cloud.google.com/storage/docs/reference/rpc/google.storage.v2) define the contract, checked 2026-10-27. Proto digest and dependencies are unchanged from the foundation slice. Rust 1.99.1 remains the verified stable toolchain; no dependency is added.

Each message accepts up to 100 ranges; larger messages return non-retryable INVALID_ARGUMENT with splitting guidance. A stream pauses request consumption while at least 100 descriptors remain queued and resumes as ranges finish, so arbitrarily many Add calls across messages use backpressure instead of rejection. A just-received batch can raise the queue to at most 199 descriptors. The stream performs one 64 KiB read at a time, rotating ranges between chunks; two response slots bound output backpressure. Handles store identifiers and the issued size limit, never open readers or blob reservations. The process-local FIFO holds at most 1024 entries, with a ten-minute expiry checked through the injected clock on insert/lookup. Eviction, expiry or restart returns FAILED_PRECONDITION; an absent generation returns NOT_FOUND and invalidates the handle. Active streams retain their reader until completion. Entries never cross server instances, bind strict selections to their owning project, and reopen the selected generation with current authorization. An appendable handle keeps its original size limit after later flush or finalization. Handles are not authorization credentials. No configuration key, catalog change, polling or startup scan is added. The lifetime and absent-generation status are recorded local choices: Google documents neither, and Cloud Storage refused BidiReadObject to the recording project, so none was recorded (SPEC-GRPC-READ-HANDLES, a scoped difference of RPC-STORAGE-016 and BEHAVIOR-RPC-004 since 2026-11-18).

[Go Storage v1.70.0's multi-range reader](https://github.com/googleapis/google-cloud-go/blob/storage/v1.70.0/storage/grpc_reader_multi_range.go) defaults to 500 pending ranges and its handleReconnectStreamCmd sends all remaining ranges together as rangesToResend. An interrupted SDK stream with more than 100 pending ranges can therefore violate the proto's per-message limit on retry; this is rejected with INVALID_ARGUMENT, which storage.ShouldRetry does not retry. No provider-contract exception is added for that invalid batch. Resume recovery with more than 100 pending ranges remains a client-side limitation; ordinary uninterrupted Add/Wait is covered with 600 ranges and no tuning options.

Partial scope: a `routing_token` returns UNIMPLEMENTED on purpose, because the emulator never redirects a stream (SPEC-GRPC-UNMODELED-SETTINGS). The first response's object follows the deprecated `read_mask` as storage.proto lines 1185-1187 define it: an absent or empty mask returns every field except `acl` and `owner`, and `*`, in the field or in the `x-goog-fieldmask` header the proto names as its alternative, returns every field, the ACL and owner included, the same object ReadObject returns for `*`. A `read_mask` other than absent, empty or `*`, and an `x-goog-fieldmask` header other than `*`, return UNIMPLEMENTED with ErrorInfo: the deprecated field's text is copied from ReadObjectRequest, says the `checksummed_data` field is always present, which BidiReadObjectResponse does not have, and breaks off where it names the grpc metadata alternative (storage.proto lines 1183-1190), so which response paths a mask selects is undefined. A `read_handle` combined with a generation or metageneration condition returns UNIMPLEMENTED with ErrorInfo as well, because the proto does not say whether the condition applies to the generation the handle reopens or to the live object; reopen without the handle to apply conditions. Both refusals are recorded local choices (SPEC-GRPC-LOCAL-CHOICES). A stream without a first message, or whose first message lacks `read_object_spec`, and a later message that repeats `read_object_spec` answer INVALID_ARGUMENT. Reads that follow later appends belong to BEHAVIOR-RAPID-005. Customer-supplied keys follow [grpc-v2-customer-supplied-keys.md](grpc-v2-customer-supplied-keys.md): the spec's key is checked on every open and resume. Apply preconditions on an initial read without a handle. Handle survival across restarts and provider-exact error parity are not claimed. Stream errors carry BidiReadObjectError for every admitted unfinished read_id. The failing range keeps its specific status; other unfinished ranges receive ABORTED with retry guidance. A failed range within an otherwise admissible batch also reports the other submitted IDs. Oversized batches are not admitted. ABORTED is a recorded local choice: the proto requires an entry per pending read_id without naming its status (SPEC-GRPC-PENDING-RANGES). So is the order of the entries, which the proto does not name either: the failed range first, then the ranges already queued, in queue order, then the rest of the rejected message in request order. The input bound above is recorded as a local bound under SPEC-GRPC-LOCAL-CHOICES.

## Verification

On 2026-11-18, tests pinned the BidiReadObject answers above that no test named. They pin existing behavior and passed on their first run. `the_spec_refuses_redirects_nondefault_masks_and_conditions_on_a_handle` checks the spec: a routing token, a nondefault read mask and each of the four conditions beside a read handle answer UNIMPLEMENTED with ErrorInfo, an empty routing token and an absent, empty or `*` mask are accepted, and a negative generation answers INVALID_ARGUMENT. A negative generation or metageneration condition is no refusal: it is compared when the object is read, as GetObject and ReadObject compare it (recorded 2026-11-18, case `grpc-probe-inferred-conditions`; for BidiReadObject itself inferred from those two reads), and the same test pins that the four negative conditions pass validation. `the_stream_refuses_a_missing_or_late_spec_and_a_field_mask_header` runs streams over the wire: no message, a first message without a spec and a later spec answer INVALID_ARGUMENT, and an `x-goog-fieldmask` header other than `*` answers UNIMPLEMENTED with one ErrorInfo detail before any response, while `*` reads. `bidi_range_errors_list_the_failed_range_first_then_queue_and_request_order` pins the order of the `read_range_errors` entries. `cargo test --locked --offline --lib grpc::` passed 270 tests. No SDK suite was run for these tests.

Later on 2026-11-18, a `*` mask started to return the object's ACL and owner. Until then BidiReadObject accepted `*` but answered with the default fields, so its first response lacked the ACL and owner that the proto's "all fields" and ReadObject's `*` include. `a_wildcard_mask_returns_the_acl_and_owner_the_default_omits` was written first and failed with an empty ACL and no owner; after the change a `*` read mask, an `x-goog-fieldmask` header of `*`, or both, return the object ReadObject returns for `*`, and an absent or empty mask still returns ReadObject's default object without them. ReadObject and BidiReadObject now build that object through one helper, and `the_spec_refuses_redirects_nondefault_masks_and_conditions_on_a_handle` checks that `validate_spec` reads `*` as every field and an absent or empty mask as the default. `cargo test --locked --offline --lib grpc::` passed 273 tests. No SDK suite was run.

On 2026-11-04, the eleven BidiRead library tests and the wire storage-read-failure
test passed. The new three-range regression failed before the change with only
the invalid ID reported, then passed with OUT_OF_RANGE for that ID and ABORTED
for both other IDs. Truncating a stored blob verifies UNAVAILABLE for the failed
read_id and ABORTED for both queued IDs over the wire.

On 2026-11-04, `cargo test --lib grpc::v2::bidi_read -- --test-threads=4`
passed ten tests, including deletion/invalidation, versioned and nonversioned
overwrite, bucket recreation, RAM budget release, injected-clock expiry, FIFO
eviction and appendable size bounds. `cargo check --workspace --all-targets`
passed before these tests. The Go wire expectation now requires NOT_FOUND after
nonversioned overwrite; the updated SDK suite has not yet been run.

Acceptance recorded before implementation. The new high-level Go test first failed with UNIMPLEMENTED on the unmodified RPC; the implementation then passed it. On 2026-10-28, the final source rebased onto integrated main `31fb5b0` passed the targeted Rust/Go tests and one complete run each of `just check` and `just check-sdk`:

- `cargo check --workspace --all-targets --locked --offline`: PASS before Rust tests.
- `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`: PASS.
- `cargo test --locked --offline --lib grpc::v2::bidi_read::tests`: PASS, signed-offset extremes, EOF, invalid lengths/offsets, duplicate IDs, per-message admission, strict handle ownership binding and shared status conversion including unsupported ErrorInfo.
- `GCS_MIMIC_GO_TEST_RUN='TestGRPCBidiRead|TestGRPCFoundation' bash scripts/sdk-go.sh`: PASS, Go vet, ordinary high-level multi-range calls, exact bytes, bounded wire chunks with CRC32C, later requests, nonversioned overwrite/reconnect generation stability, preconditions, NOT_FOUND/OUT_OF_RANGE details, and strict admin/viewer versus creator/anonymous authorization. Foundation multi-range rejection is replaced by unsupported Rapid Append; unrelated RPC assertions remain unchanged.
- `TestGRPCBidiReadManyRanges` first failed before the admission fix with `Add/Wait did not complete before the deadline: context deadline exceeded`. With input backpressure, its 600 ranges of 65537 bytes pass using the SDK's default settings and every range's bytes are verified without retaining downloaded payloads. The wire test also verifies immediate non-retryable INVALID_ARGUMENT for 101 ranges in one message. This is correctness evidence, not a performance measurement.
- `just check-sdk`: PASS on the final source, including Go vet/full suite and Ruby self-check, migration, permissive, strict and signing scenarios. The existing Go V2 signed-transfer case skips HTTP because the SDK emits HTTPS; none of the new gRPC tests skip.
- `RUST_TEST_THREADS=8 just check`: PASS on the final source, including formatting, native/Windows/Linux clippy, all Rust unit/integration/doc tests, the feature ledger/generated views and offline script/publication checks. No Rust test failed or was ignored.
- `cargo xtask compatibility render`: PASS; English/Japanese matrices regenerated from the two updated PARTIAL rows.
- Markdownlint for the contract, Japanese companion and migration table: PASS.

No benchmark, container build, guide walkthrough, HTTPS SDK mode or real-cloud request is run because this slice changes only the gRPC operation. Next: merge the integrated Storage v2 slice; Storage v1 compatibility proceeds separately.
