# Storage v2 bidirectional ranged reads

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: RPC-STORAGE-016 and BEHAVIOR-RPC-004.

The integrated adapter uses the shared `status::from_json` conversion, including
JSON 503 to UNAVAILABLE and unsupported JSON 501 to ErrorInfo-bearing
UNIMPLEMENTED. Transport limits and `Storage.max_object_bytes` remain the shared
foundation/object-I/O implementation.

## Acceptance contract

- Select bucket, object, generation and all four preconditions from the first read_object_spec through shared authorized catalog selection; send metadata and a read_handle first.
- Read multiple read_ids, including later requests on the same stream; preserve increasing offsets per range and emit range_end, including empty ranges. Zero length reads to EOF, negative offsets are relative to EOF and clamp at zero, lengths truncate at EOF, negative lengths and offsets beyond EOF return OUT_OF_RANGE.
- Send CRC32C for every bounded data chunk; interleave ranges without collecting their bodies.
- Reconnect using read_handle and preserve the selected generation after overwrite, including without versioning; omit metadata on handle reconnection as required by the proto.
- Return NOT_FOUND for absent objects/generations and FAILED_PRECONDITION for failed generation/metageneration conditions. Reject repeated specs, duplicate outstanding IDs and unsupported meaningful fields explicitly.
- Reuse shared strict authorization on initial reads and reconnects; JSON API creates fixtures. Go storage.NewGRPCClient with storage.WithGRPCBidiReads supports ordinary NewMultiRangeDownloader, repeated Add, Wait and Close.
- Bind strict read handles to the selected owning project; recreation of the bucket in another project invalidates the old handle.
- Accept more than 100 outstanding ranges across multiple messages with bounded input backpressure; ordinary Go Add/Wait must complete for 600 ranges larger than a chunk. Reject more than 100 ranges in one message with non-retryable INVALID_ARGUMENT.

## Local boundaries

The pinned [Storage v2 proto](https://github.com/googleapis/googleapis/blob/933dd1be8180a22bca2dc473b53d4f4297ee3b0c/google/storage/v2/storage.proto) and [RPC documentation](https://docs.cloud.google.com/storage/docs/reference/rpc/google.storage.v2) define the contract, checked 2026-10-27. Proto digest and dependencies are unchanged from the foundation slice. Rust 1.99.1 remains the verified stable toolchain; no dependency is added.

Each message accepts up to 100 ranges; larger messages return non-retryable INVALID_ARGUMENT with splitting guidance. A stream pauses request consumption while at least 100 descriptors remain queued and resumes as ranges finish, so arbitrarily many Add calls across messages use backpressure instead of rejection. A just-received batch can raise the queue to at most 199 descriptors. The stream performs one 64 KiB read at a time, rotating ranges between chunks; two response slots bound output backpressure. Handles retain already-opened immutable files in a process-local FIFO of 64 entries; eviction or restart invalidates a handle (FAILED_PRECONDITION: reopen without it). Active streams keep their file after eviction. Entries use weak state ownership, never cross server instances, bind strict selections to their owning project, and recheck bucket authorization on every reconnect. Handles are not authorization credentials. No configuration key, catalog change, polling or startup scan is added.

[Go Storage v1.70.0's multi-range reader](https://github.com/googleapis/google-cloud-go/blob/storage/v1.70.0/storage/grpc_reader_multi_range.go) defaults to 500 pending ranges and its handleReconnectStreamCmd sends all remaining ranges together as rangesToResend. An interrupted SDK stream with more than 100 pending ranges can therefore violate the proto's per-message limit on retry; this is rejected with INVALID_ARGUMENT, which storage.ShouldRetry does not retry. No provider-contract exception is added for that invalid batch. Resume recovery with more than 100 pending ranges remains a client-side limitation; ordinary uninterrupted Add/Wait is covered with 600 ranges and no tuning options.

Partial scope: redirects/routing tokens, encryption, nondefault read masks and combining handle reconnects with preconditions remain UNIMPLEMENTED. Apply preconditions on an initial read without a handle. Handle survival across restarts and provider-exact error parity are not claimed. Range errors carry BidiReadObjectError for the invalid range; enumerating all other outstanding ranges in error details remains pending.

## Verification

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
