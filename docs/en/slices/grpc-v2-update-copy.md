# Storage v2 updates and derived objects

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: RPC-STORAGE-005–010, RPC-STORAGE-012, RPC-STORAGE-017, RPC-STORAGE-021, RPC-STORAGE-024 and BEHAVIOR-RPC-002. This adapter reuses JSON metadata validation, shared authorization and transactional catalog mutations; it adds no settings or dependencies.

## Acceptance contract

| Case | Required behavior |
| --- | --- |
| grpc_update_masks | Only masked supported bucket/object fields change; other fields survive. Empty masks are invalid and unsupported paths return actionable UNIMPLEMENTED. Generation/metageneration conditions are checked atomically. |
| grpc_bucket_settings | JSON-supported labels, versioning, CORS, lifecycle, retention, soft deletion, default event hold and IAM configuration use JSON validation and shared persistence. |
| grpc_iam_config_presence | Updating only UBLA through the parent iam_config mask preserves PAP; updating only PAP preserves UBLA, including ordinary Go BucketAttrsToUpdate calls. Explicit false/inherited values still change their selected setting. |
| grpc_iam | Policy read/write uses the shared policy store and ETags; TestIamPermissions uses the same strict checks as actual operations. Unsupported conditional/audit policies fail explicitly. |
| grpc_lock | Positive if_metageneration_match is mandatory; shared retention locking enforces conditions and irreversible local lock semantics. |
| grpc_derived | Rewrite streams same/cross-bucket bytes with generation conditions and shared continuation sessions; compose selects generations, checks source/destination conditions and accepts 1–32 sources. |
| grpc_move_restore | Same-bucket move is atomic; soft-deleted generation restore uses shared publication and live destination conditions. |
| grpc_json_interoperation | JSON prepares resources and observes gRPC mutations and bytes; ordinary Go NewGRPCClient high-level calls work with deadlines. |
| grpc_strict | Registered Bearer principals use the shared authorization path; denied mutations leave resources unchanged. |

## Sources and boundaries

The pinned Storage v2 protobuf closure and digests are recorded in the [foundation contract](grpc-v2-foundation.md). Official contract: [Storage v2 RPC](https://docs.cloud.google.com/storage/docs/reference/rpc/google.storage.v2) and [pinned protobuf](https://github.com/googleapis/googleapis/blob/933dd1be8180a22bca2dc473b53d4f4297ee3b0c/google/storage/v2/storage.proto), checked 2026-10-27; Storage SHA-256 `a0df6132967b97a89be248f1f24d277ee8df8f8d885db1d4af6249469ff0c1cf`. Go Storage 1.70.0 is pinned by the existing harness. Existing SPEC-001 flat-namespace move and SPEC-COMPOSE-EDGES provider questions remain open; no cloud resources are used.

## Supported subset

Bucket masks: `labels`, `labels.KEY`, `storage_class`, `versioning[.enabled]`, `default_event_based_hold`, `cors`, `lifecycle[.rule]`, `retention_policy[.retention_duration]`, `soft_delete_policy[.retention_duration]`, `iam_config`, `iam_config.uniform_bucket_level_access[.enabled]` and `iam_config.public_access_prevention`. JSON validators enforce label, CORS, lifecycle, class, retention and UBLA constraints. Lifecycle is validated metadata; this slice does not add lifecycle execution. Retention nanoseconds follow the proto's whole-second truncation; soft-delete fractional durations are unsupported.

The parent `iam_config` mask merges only present settings: `uniform_bucket_level_access: Some(...)` and a nonempty `public_access_prevention`. Go Storage 1.70.0 sends this parent path for either individual setting, so absent sibling settings must survive. Explicit UBLA false and PAP inherited still apply. The other supported parent masks have one editable scalar or repeated field: versioning false, an absent retention duration, zero soft-delete duration and an empty lifecycle/CORS list retain their existing reset/replace semantics; read-only lock/effective times are never copied into state. The serialization audit uses pinned [BucketAttrsToUpdate.toProtoBucket](https://github.com/googleapis/google-cloud-go/blob/storage/v1.70.0/storage/bucket.go) and its UpdateBucket field-mask construction.

Object masks: `metadata`, `metadata.KEY`, the five fixed serving metadata strings, `custom_time`, `temporary_hold` and `event_based_hold`. `create_time` is ignored as an output-only path because the pinned Go SDK sends it for ordinary empty updates. `*`, ACL, contexts, object retention and other masks return UNIMPLEMENTED. A whole nonempty map replacement reads the old keys and pins the observed metageneration (and object generation) at mutation; a racing replacement can fail with FAILED_PRECONDITION instead of retrying implicitly. Key-level mutations use the shared transaction directly.

The adapter currently accepts one match or not-match condition per generation kind; simultaneous match/not-match for the same kind returns INVALID_ARGUMENT. JSON's broader combined-condition precedence remains outside this scoped RPC acceptance.

IAM supports bucket resources, unconditional predefined-role policies, versions 0/1/3 and opaque shared policy ETags. SetPolicy accepts its absent/empty default mask or `bindings,etag`; other masks, conditions, audit configs and managed-folder/object resources remain unsupported. TestPermissions reuses the JSON permission-name validation and per-permission authorization, including project grants, bucket grants and PAP. Locking requires a positive matching metageneration and uses shared irreversible emulator-local state.

Rewrite inherits or replaces the JSON-supported editable metadata subset, selects source generations and applies source/destination conditions. Same-location/class copies finish in one call; changed location/class uses the shared 8 MiB default budget or a positive whole-MiB caller budget. Token-only continuation, mismatched-request rejection and last-token replay use the shared durable session. Completed gRPC responses are stored as protobuf bytes inside the existing bounded response envelope; continuation tokens must stay on the gRPC transport. Cross-transport token reuse is not supported. No body bytes are collected: the shared engine stages and copies with a fixed 64 KiB buffer.

Compose accepts 1–32 same-bucket generation-selected sources, source generation-match conditions, destination generation/metageneration-match conditions and the shared optional source-deletion behavior. Explicit checksum verification, ACL/encryption, destination storage class, temporary hold and custom time on derived objects remain unsupported. Shared object-size limits, checksums and publication conditions remain enforced. Move and restore retain the same flat-namespace, retention, versioning and soft-delete semantics as JSON. HNS restore tokens and copying source ACLs remain unsupported. Empty common encryption parameters, empty request-level checksum messages and false ACL flags have their default meaning.

No setting is added. The gRPC service receives the existing `max_object_bytes` setting so copies/compositions honor the same configured body cap as JSON. Shared storage and startup ordering are unchanged. Every broad ledger row remains PARTIAL because these explicit unsupported cases and broader provider parity remain open.

Storage errors use the common `status::from_json` mapping, including expired-session 410 as FAILED_PRECONDITION and unsupported 501 as UNIMPLEMENTED with ErrorInfo. Foundation fail-fast checks use unsupported Rapid writes; CancelResumableWrite is implemented in the [object I/O contract](grpc-v2-object-io.md). The bucket and object I/O implementations and their configured size limit remain shared.

UpdateBucket, UpdateObject and TestIamPermissions retain LOCAL_SIMULATION achieved fidelity, matching their shared JSON operations; the remaining delivered scopes have EXACT achieved fidelity with explicit pending cases. This does not turn local principals, policy propagation or lifecycle metadata into Google infrastructure.

## Verification

Before implementation, `GCS_MIMIC_GO_TEST_RUN=TestGRPCUpdateCopy bash scripts/sdk-go.sh` failed on UpdateBucket with immediate UNIMPLEMENTED. After implementation, the same command passed both ordinary and strict suites using Go Storage 1.70.0; JSON supplies fixtures and observes metadata and exact bytes. Rust tests exercise unsupported/empty masks, non-target preservation, full-map replacement, stale conditions, continuation/replay/mismatch, 32/33-source boundaries and local retention locking.

`cargo check --workspace --all-targets --locked --offline` passed before tests. Dedicated update/copy tests cover field masks, generation conditions, IAM presence preservation, continuation and compose boundaries. `just check-sdk` runs the complete Go and Ruby suites in permissive and strict profiles; the existing Go V2 signed-transfer case skips on HTTP because it requires HTTPS. Official stable-channel and Cargo-index checks confirmed the unchanged Rust 1.99.1, tonic 0.14.7 and prost 0.14.4 inputs. No dependency is added. Container/guide, benchmark, performance, real-cloud and cross-transport-token tests are excluded.

The IAM presence regression was reproduced before its fix in both Rust and Go: enabling UBLA changed an enforced PAP setting to inherited. Its regression test also changes only PAP and requires UBLA to survive. On the final source, `cargo test --locked --offline --lib grpc::v2::update_copy` passed all five tests, including shared status mapping, masks, IAM presence, rewrite continuation and compose boundaries. `just check-sdk` passed the complete Go/Ruby suites in both profiles, including default update/copy, strict and foundation fail-fast cases; the existing HTTPS-only Go V2 signature test was the sole HTTP-run skip. English/Japanese feature matrices are generated from the ledger. No container image is built.

`RUST_TEST_THREADS=8 just check` passed once on the final source with bucket and object I/O support integrated: formatting, native/Windows/Linux clippy, all workspace unit/integration/doc tests, 591-row ledger/generated-view validation and offline script checks. The main library reports 528 passing tests. The IAM presence issue is fixed; the other parent masks require no change for the single-field/reset semantics described above.
