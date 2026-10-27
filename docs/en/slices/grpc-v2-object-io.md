# Storage v2 object I/O

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: RPC-STORAGE-011, RPC-STORAGE-014, RPC-STORAGE-015, RPC-STORAGE-018, RPC-STORAGE-019, RPC-STORAGE-020, RPC-STORAGE-022, RPC-STORAGE-023 and BEHAVIOR-RPC-003. RestoreObject, Rapid append and CancelResumableWrite are excluded.

## Acceptance contract

- Ordinary Go `storage.NewGRPCClient(ctx)` Writer, with default chunk size, writes small and multi-MiB bodies; JSON reads the result. JSON uploads are readable through gRPC NewReader and NewRangeReader.
- BidiWriteObject and WriteObject share bounded staging, CRC32C chunk validation, final CRC32C/MD5 validation and transactional publication. Incorrect offsets and checksums never publish an object.
- StartResumableWrite returns a capability; QueryWriteStatus returns a durable offset or the completed resource. Reopening a stream resumes at that offset, including after catalog recovery.
- Generation and metageneration conditions, including create-if-absent, are checked in the publication transaction. Supported fixed/custom metadata is preserved; unsupported semantics return actionable UNIMPLEMENTED.
- ReadObject supports positive/negative offsets, bounded lengths, out-of-range errors, selected generations, per-message CRC32C and complete-object checksums. Response messages stay below 4 MiB.
- GetObject supports generations, conditions and read masks. DeleteObject uses shared hold, retention, versioning and soft-delete behavior.
- ListObjects supports prefix, delimiter prefixes, page size and query-bound continuation tokens; Go Objects iterates them.
- Strict authentication uses shared bucket permissions and rechecks them inside catalog operations. No credentials are discovered and no Google Cloud calls are made.

## Sources and boundaries

The contract uses the foundation's pinned [Storage v2 proto](https://github.com/googleapis/googleapis/blob/933dd1be8180a22bca2dc473b53d4f4297ee3b0c/google/storage/v2/storage.proto), SHA-256 `a0df6132967b97a89be248f1f24d277ee8df8f8d885db1d4af6249469ff0c1cf`, and the existing Go Storage v1.70.0 SDK. Existing source gaps remain open; JSON error choices are not assumed to define gRPC status codes. No dependency, setting, catalog schema or startup order is changed.

The [official RPC reference](https://docs.cloud.google.com/storage/docs/reference/rpc/google.storage.v2) and [Rust 1.99.1 stable announcement](https://blog.rust-lang.org/2026/10/15/Rust-1.99.1/) were checked on 2026-10-28. The foundation's verified dependency graph is retained; no dependency is added or updated.

## Supported subset and resource bounds

Object I/O uses the common status::from_json conversion shared with bucket RPCs. Read conditions mapped from JSON 304 retain FAILED_PRECONDITION, and unsupported JSON 501 retains UNIMPLEMENTED with ErrorInfo details.

The existing `max_object_bytes` is passed into the gRPC adapter, with its existing 1 GiB default. No new setting is needed. Incoming protobuf messages retain the foundation's 4 MiB limit. Outgoing object-body messages use the shared reader's 64 KiB chunks; upload bridges use two 64 KiB slots. Lists use bounded catalog pages of at most 1000 entries and shared query-bound continuation tokens, without scanning the whole catalog. No database transaction spans network input.

Single-request streams use shared `prepare_object_upload`/`put_object_prechecked`, not retained resumable sessions. The adapter uses the existing incremental hasher to validate final-message digests before sending shared publication's EOF marker. Only shared publication assigns generations and applies conditions/holds/retention. Both streaming RPCs use this same handler. StartResumableWrite uses shared persisted sessions and exclusive per-message writer leases; each message reaches the shared fsync/checkpoint before its offset is acknowledged. Reopening the catalog preserves those offsets. The opaque upload ID includes the bucket name and random shared capability. QueryWriteStatus rechecks current create permission and returns the original completion snapshot even after the live object changes.

Write metadata supports content type/encoding/disposition/language/cache control, custom metadata, temporary/event holds and custom time. Empty/default protobuf values are accepted. Nondefault unhandled object fields, predefined ACLs, appendable objects and customer encryption return actionable UNIMPLEMENTED. Metadata protobufs have an 8 KiB local limit; oversized values return RESOURCE_EXHAUSTED. Explicit object_size is checked before staging and at finish.

ReadObject clamps negative offsets beyond the start to zero, permits offset equal to size (empty read), rejects greater offsets with OUT_OF_RANGE and rejects negative limits with INVALID_ARGUMENT. Returned content ranges use an exclusive end. GetObject/list masks select supported top-level object fields; ReadObject additionally selects its metadata and top-level response fields. Nested object-field masks remain UNIMPLEMENTED. The unimplemented proto fields in the foundation conversion remain absent in default responses.

The broader rows remain PARTIAL. Pending scope includes non-final flush/state_lookup on non-resumable streams (use StartResumableWrite and upload_id), nested field masks, explicit soft-deleted GetObject/list requests, encryption/ACL and other unhandled proto fields, provider-exact error precedence/details, concurrent streams sharing one upload ID and exhaustive recovery/fault-injection combinations. The shared resumable engine's 128 retained-session/seven-day bound applies only to explicitly resumable writes. Bidi finish is terminal; extra messages after the terminal response are outside this slice's verified scope. Large metadata-heavy listings can reach the existing 4 MiB encoded-response limit and are not claimed as complete pagination conformance.

## Verification

For upload_id streams, object_checksums on a non-final message is an explicit local UNIMPLEMENTED subset: the pinned proto permits first-message checksums, but the current shared session interface cannot persist a changed checksum. Silently retaining it only for the current stream would bypass validation after reconnect. Supply checksums in StartResumableWrite, where the shared options persist them, or in a finish_write=true message. Rejection leaves the durable offset unchanged. Persisted CRC32C/MD5 is enforced after reconnect and catalog reopen; a final mismatch returns INVALID_ARGUMENT, publishes no object and invalidates the session, so QueryWriteStatus and retries return NOT_FOUND.

Acceptance was recorded before implementation. The new default Go Writer test failed on the baseline with BidiWriteObject UNIMPLEMENTED before adapter changes. Final targeted results on 2026-10-28:

- `cargo check --workspace --all-targets --locked --offline`: PASS before tests.
- `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`: PASS.
- `cargo test --locked --offline --lib grpc::v2::object_io`: PASS, five tests: ranges/masks, durable restart/resume/completion, checksum/condition/generation preservation, strict permissions/unsupported values and resumable checksum persistence/rejection/session invalidation.
- `GCS_MIMIC_GO_TEST_RUN='^TestGRPCObjectIO' bash scripts/sdk-go.sh`: PASS, Go vet and four tests: ordinary SDK calls with deadlines, strict authorization, hold/retention/soft-delete interaction and wire contracts. The default Writer uses its original chunk size throughout. The pinned SDK rejects negative offset plus positive length before transport, so that RPC combination is covered directly by the wire test.
- `just check-sdk`: PASS on the final sources, including the full Go suite, all four object-I/O cases, Foundation fail-fast checks and Ruby permissive/strict journeys.
- `RUST_TEST_THREADS=8 just check`: PASS on the final sources, including native/Windows/Linux clippy, all library tests, remaining Rust unit/integration/doc tests, ledger/generated views and offline script/publication checks; no failures or ignored tests.
- Markdownlint: PASS for this contract, its Japanese companion, the migration table and Go SDK README.

No benchmark, container build, full guide run, HTTPS-mode SDK run or real-cloud request is required for this gRPC-only slice. The migration table/SDK README changes describe the new calls and do not change the HTTP guide flow. The existing HTTP V2 signed-transfer test's HTTPS-only skip is not gRPC evidence. Next: integrate the parallel update/copy and bidi-read slices.
