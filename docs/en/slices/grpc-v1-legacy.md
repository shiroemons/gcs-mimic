# Legacy Storage v1 gRPC

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: RPC-LEGACY-001–012, all PARTIAL. This is the legacy migration profile for fake-gcs-server v1.56.1, not Google GA Storage or Google EXACT fidelity. The ledger uses LOCAL_SIMULATION to distinguish this retired protocol from the supported Google v2 contract. It does not add a runtime compatibility-profile setting or bypass strict authorization.

## Inventory and acceptance contract

The pinned [reference server](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/internal/grpc/server.go) implements exactly twelve RPCs:

| Group | RPCs | Acceptance |
| --- | --- | --- |
| Buckets | GetBucket, DeleteBucket, InsertBucket, UpdateBucket, ListBuckets | Create/get/list/update/delete through shared validation, catalog and strict authorization; conditions and bounded pagination. |
| Objects | InsertObject, GetObject, DeleteObject, UpdateObject, PatchObject, ComposeObject, ListObjects | Multi-message bounded upload, metadata updates, composition, listing/deletion, atomic conditions and shared strict authorization. |
| Unsupported | All other v1 RPCs and unknown methods | Immediate UNIMPLEMENTED with ErrorInfo and JSON remedy, including streams without input; never retryable UNAVAILABLE. |
| Interoperability | JSON and v1 | JSON-created records are visible over v1; v1 writes and mutations are visible over JSON. |
| Failure safety | Conditions, checksums, cancellation | Incorrect offsets/checksums, missing finish and stale conditions never publish bytes; unsupported recognized settings never succeed silently. |

| Feature ID | Inventoried RPC | Supported subset |
| --- | --- | --- |
| RPC-LEGACY-001 | GetBucket | Bare bucket name, metageneration conditions and shared metadata. |
| RPC-LEGACY-002 | DeleteBucket | Atomic metageneration conditions, nonempty-bucket protection and shared authorization. |
| RPC-LEGACY-003 | InsertBucket | Explicit project; name, location, storage class, labels, versioning and default event hold. |
| RPC-LEGACY-004 | UpdateBucket | Default event hold plus supplied versioning and storage class; labels replace the complete label map, including clearing omitted labels, with shared replacement validation and atomic metageneration conditions. |
| RPC-LEGACY-005 | ListBuckets | Explicit project, prefix, maximum 1000 per page and query-bound continuation tokens. |
| RPC-LEGACY-006 | InsertObject | Non-resumable multi-message body, offsets, chunk CRC32C, complete CRC32C/hexadecimal MD5, metadata and atomic publication conditions. |
| RPC-LEGACY-007 | GetObject | Metadata, generation selection and all four generation/metageneration conditions. Body download is outside the reference inventory. |
| RPC-LEGACY-008 | DeleteObject | Generation selection and all four conditions; shared hold/retention/versioning/soft-delete enforcement. |
| RPC-LEGACY-009 | UpdateObject | Fixed content fields, custom metadata and supplied holds, replacing mutable content metadata. Empty content strings clear fields through shared null semantics. |
| RPC-LEGACY-010 | PatchObject | Nonempty fixed fields and custom-metadata merge; optional event hold. Nonempty update_mask remains unsupported, matching the explicit legacy merge subset. |
| RPC-LEGACY-011 | ComposeObject | 1–32 same-bucket source names, source generations/conditions, destination metadata and atomic destination conditions; bounded shared composition. |
| RPC-LEGACY-012 | ListObjects | Prefix, delimiter, trailing delimiter, versions, maximum 1000 per page and query-bound tokens. |

The reference's first-message-only upload and incomplete responses are not safety contracts. InsertObject consumes all messages with bounded staging and requires finish_write plus transport EOF. No new configuration, dependency, storage interface or startup phase is introduced. The existing explicit grpc_listen h2c endpoint serves both services.

Request/response protobufs retain the foundation's 4 MiB limit. Upload metadata is capped at 8 KiB; body messages feed two 64 KiB queue slots. The existing max_object_bytes bounds total body size. No catalog transaction spans network input. Conditions and current permissions are checked again by the shared publication transaction. No catalog scan, new timer or polling loop is added.

Bucket responses expose name/ID, metageneration, location/type, class, labels, creation/update times, versioning and default hold. Object responses expose names, generations, size/class, fixed/custom metadata, checksums, component count, timestamps, holds and retention expiry. Unrepresented fields remain empty; numeric project resolution, complete ACL/lifecycle/encryption response parity and metadata-heavy response pagination are not claimed. Shared JSON checksum rendering is reused without copying v2 conversion code. No v2 adapter or existing v2 generated file is changed.

Empty/default common request messages and unspecified/NO_ACL projection are accepted. Nondefault billing, quota, common field masks, customer encryption, ACL/full projection, predefined ACLs, KMS, unsupported resource fields, resumable upload IDs and reference upload data return ErrorInfo-bearing UNIMPLEMENTED. Legacy routing/billing headers are also rejected with a remedy. Unknown protobuf fields follow Prost's normal unknown-field behavior.

ListBuckets deliberately requires a project, including in permissive mode; project-free global listing in the reference is not claimed. UpdateBucket uses the pinned proto's replacement semantics for labels, while preserving unrelated unsupported settings. The reference's UpdateBucket only mutates default event hold, so it does not establish label semantics; the pinned v1 proto supplies that contract. UpdateObject replaces mutable content metadata; PatchObject merges nonempty fields and does not model scalar clearing through a field mask. Use UpdateObject to clear fixed strings. A new setting is unnecessary: the existing compat_profile controls shared bucket-name validation, while auth_mode independently enforces strict permissions.

GetBucket/GetObject resources can be edited and sent back to UpdateBucket/UpdateObject. Supplied nondefault output-only fields must equal the selected current record; changed values return UNIMPLEMENTED with a remedy. Object validation runs in the shared metadata transaction. Bucket validation reads an authorized snapshot and fences replacement with its metageneration in the mutation transaction, so a concurrent metadata change fails its condition instead of accepting stale output fields. Unsupported writable fields are still rejected.

## Sources and generation

Google removed the retired v1 service in 2022. The v1 service/messages/resources are therefore pinned separately to googleapis commit b2e2bc62fab90e6829e62d3d189906d9b79899e4, compatible with the already pinned Go genproto legacy client. Shared imports retain the foundation pins; the added wrappers.proto uses its protobuf v36.2 commit. SHA-256 entries and regeneration commands accompany the committed source. Ordinary builds do not require protoc. Existing Google-source gaps remain open and no provider conformance is inferred.

| Source | SHA-256 |
| --- | --- |
| Reference internal/grpc/server.go at 896ece51e49eed0946089f3089f6343855c0d106 | 19935d8e1db0d376abb851826ca8f51d5723d97803a6b6c54bf979a93802b40e |
| v1 storage.proto | be9583dad18a6b087ae94b064b44d9ab1ce74c8647b88f85afbd7b10de50a192 |
| v1 storage_resources.proto | 875af563ab150552b440d6c8d358cf5c7896b666eef0321d07b149febc6639d7 |
| protobuf wrappers.proto | 6878dc7534cf9805eee56345cdc38b5e9ca39dece246710c663c7007a67fef49 |

```sh
shasum -a 256 -c src/grpc/proto/SHA256SUMS
cargo run -p grpc-codegen --locked --offline
cargo fmt --all
git diff -- src/grpc/generated
```

The generator emits legacy first and foundation last to preserve the foundation's common generated packages. The legacy module includes only google.storage.v1.rs. On 2026-10-28, official Rust stable metadata confirmed 1.99.1; the official Cargo sparse index confirmed tonic/tonic-prost/tonic-prost-build 0.14.7 and prost/prost-types 0.14.4 as the latest non-yanked stable releases. No dependency version or lockfile changes are needed. The existing Go graph pins genproto v0.0.0-20260715232425-e75dac1f907d, grpc-go v1.85.0 and protobuf v1.36.12. The raw client promotes the already present genproto requirement to direct; no new Go module is added.

## Verification

Acceptance was recorded before implementation. The first new-test run stopped during compilation because the initial generation order replaced shared IAM output; that generator defect was repaired and all existing generated files returned to their original content. A red baseline wire result was therefore not established. The first completed v1 journey then exposed incorrect empty-string conversion on metadata replacement; mapping empty strings to shared null-clearing semantics repaired it.

On 2026-10-28:

- `cargo check --workspace --all-targets --locked --offline`: PASS before tests.
- `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`: PASS.
- `cargo test --locked --offline --lib grpc::v1::`: PASS; unchanged output fields are accepted and changed output fields are rejected. The raw Go journey also verifies full GetBucket/GetObject resource write-back, complete label-map replacement and changed ID/generation rejection.
- `shasum -a 256 -c src/grpc/proto/SHA256SUMS`: PASS, all 24 proto files.
- `GCS_MIMIC_GO_TEST_RUN='^TestGRPCV1Legacy' bash scripts/sdk-go.sh`: PASS, Go vet and three tests. The raw v1 client exercises all twelve inventoried RPCs, JSON interoperability, metadata merge/replacement, composed bytes and one-item continuation pages. Every other descriptor method returns immediate UNIMPLEMENTED with ErrorInfo under a two-second deadline; GetObjectMedia is also rejected without any request message or EOF. The safety test rejects offset, missing finish, chunk CRC32C and whole-object MD5 failures without publication, stale delete/patch conditions and nondefault billing. Strict tests verify admin/viewer reads, anonymous denial, viewer upload/delete/bucket-update denial.
- `just check-sdk`: PASS on final sources, including the raw v1 tests and a regression for UpdateObject clearing temporary_hold=false, the full existing Go suite and Ruby self-check/migration/permissive/strict journeys. The existing Go V2 signer case skips this HTTP profile because it requires HTTPS; no v1 case is skipped.
- `RUST_TEST_THREADS=8 just check`: PASS on the final sources rebased onto main 414114c, including native/Windows/Linux clippy, all 532 library tests, remaining unit/integration/doc tests, ledger/generated views and offline script/publication checks. No Rust test failed or was ignored; all nine TLS tests passed.
- `cargo xtask compatibility check`: PASS, 603 feature rows with twelve separate legacy operation rows and both generated matrices.
- Markdownlint: PASS for the English/Japanese contracts and migration table, zero issues.

An earlier `just check` attempt failed once in the existing time-dependent `tls_streaming_progress_and_upload_deadlines_preserve_existing_object` test at its initial HTTP 200 assertion (`tests/tls.rs:999`; 8 TLS tests passed, 1 failed). Its single-test rerun passed without changing TLS sources or timeouts. The final rebased check passed all nine TLS tests.

The twelve rows remain PARTIAL because the explicit unsupported input/response fields and unverified crash/concurrency combinations remain pending. No benchmark, container build, guide walkthrough, HTTPS-mode rerun or real-cloud request is required for this RPC-only change. The final sources are based on the integrated v2 track at main commit 414114c. Next: address remaining legacy field/response and recovery/concurrency cases when required, preserving the independent adapters.
