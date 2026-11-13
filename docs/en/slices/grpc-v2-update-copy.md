# Storage v2 updates and derived objects

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: RPC-STORAGE-005–010, RPC-STORAGE-012, RPC-STORAGE-017, RPC-STORAGE-021, RPC-STORAGE-024 and BEHAVIOR-RPC-002. This adapter reuses JSON metadata validation, shared authorization and transactional catalog mutations; it adds no settings or dependencies.

## Acceptance contract

| Case | Required behavior |
| --- | --- |
| grpc_update_masks | Only masked supported bucket/object fields change; other fields survive. Empty masks are invalid and unsupported paths return actionable UNIMPLEMENTED. Generation/metageneration conditions are checked atomically. |
| grpc_bucket_settings | JSON-supported labels, versioning, CORS, lifecycle, retention, soft deletion, default event hold and IAM configuration use JSON validation and shared persistence. |
| grpc_iam_config_presence | Under the parent iam_config mask an omitted PAP resets to inherited while an omitted UBLA is kept, as Cloud Storage did for an empty iam_config, a UBLA-only and a PAP-only iam_config on 2026-11-13. Leaf masks such as `iam_config.public_access_prevention` change only their setting. |
| grpc_iam | Policy read/write uses the shared policy store and ETags; TestIamPermissions uses the same strict checks as actual operations. Unsupported conditional/audit policies fail explicitly. |
| grpc_lock | Positive if_metageneration_match is mandatory; shared retention locking enforces conditions and irreversible local lock semantics. |
| grpc_derived | Rewrite streams same/cross-bucket bytes with generation conditions and shared continuation sessions; compose selects generations, checks source/destination conditions and accepts 1–32 sources. |
| grpc_move_restore | Same-bucket move is atomic; soft-deleted generation restore uses shared publication and live destination conditions. |
| grpc_json_interoperation | JSON prepares resources and observes gRPC mutations and bytes; ordinary Go NewGRPCClient high-level calls work with deadlines. |
| grpc_strict | Registered Bearer principals use the shared authorization path; denied mutations leave resources unchanged. |

## Sources and boundaries

The pinned Storage v2 protobuf closure and digests are recorded in the [foundation contract](grpc-v2-foundation.md). Official contract: [Storage v2 RPC](https://docs.cloud.google.com/storage/docs/reference/rpc/google.storage.v2) and [pinned protobuf](https://github.com/googleapis/googleapis/blob/933dd1be8180a22bca2dc473b53d4f4297ee3b0c/google/storage/v2/storage.proto), checked 2026-10-27; Storage SHA-256 `a0df6132967b97a89be248f1f24d277ee8df8f8d885db1d4af6249469ff0c1cf`. Go Storage 1.70.0 is pinned by the existing harness. Existing SPEC-001 flat-namespace move and SPEC-COMPOSE-EDGES provider questions remain open; no cloud resources are used.

## Supported subset

Bucket masks: `acl`, `default_object_acl` ([SDK parity](grpc-v2-sdk-parity.md)), `labels`, `labels.KEY`, `storage_class`, `versioning[.enabled]`, `default_event_based_hold`, `cors`, `lifecycle[.rule]`, `retention_policy[.retention_duration]`, `soft_delete_policy[.retention_duration]`, `iam_config`, `iam_config.uniform_bucket_level_access[.enabled]` and `iam_config.public_access_prevention`. The adapter constructs `BucketMetadataUpdate` directly, using shared label, CORS, lifecycle, class, retention and UBLA validation without JSON bodies. Lifecycle is validated metadata; this slice does not add lifecycle execution. Retention nanoseconds follow the proto's whole-second truncation; soft-delete fractional durations are unsupported.

Under the parent `iam_config` mask the two settings differ: an empty `public_access_prevention` resets to inherited, while `uniform_bucket_level_access` changes only when its message is present. Cloud Storage answered that mask with an empty `iam_config` by resetting public access prevention to inherited and advancing the metageneration (case `grpc-empty-iam-update`), with a UBLA-only `iam_config` by enabling UBLA and resetting enforced public access prevention to inherited, and with a PAP-only `iam_config` by keeping the enabled UBLA (case `grpc-iam-config-partial-update`), all [recorded 2026-11-13](../evidence/provider-differential-gap-probes.md). Until 2026-11-13 the parent mask merged only present settings. Go Storage 1.70.0 sends this parent path for either individual setting, so a Go `BucketAttrsToUpdate` that names only UBLA resets public access prevention, as Cloud Storage does, and one that names only public access prevention keeps UBLA; the leaf masks `iam_config.uniform_bucket_level_access[.enabled]` and `iam_config.public_access_prevention` change only their own setting. The other supported parent masks have one editable scalar or repeated field: versioning false, an absent retention duration, zero soft-delete duration and an empty lifecycle/CORS list retain their existing reset/replace semantics; read-only lock/effective times are never copied into state. The serialization audit uses pinned [BucketAttrsToUpdate.toProtoBucket](https://github.com/googleapis/google-cloud-go/blob/storage/v1.70.0/storage/bucket.go) and its UpdateBucket field-mask construction.

Every accepted nonempty bucket mask counts as a metadata update, including an empty
`iam_config` parent mask. The latter resets public access prevention, keeps UBLA and advances the
metageneration, as recorded under the resolved SPEC-GRPC-EMPTY-IAM-UPDATE.

Shared bucket validation errors name protobuf fields in gRPC responses, including
Autoclass, lifecycle and retention failures raised by the mutation transaction.
JSON responses retain their existing vocabulary and messages. Locked-retention
guidance uses retention_policy.retention_duration without a JSON body example.

Field-dependent metadata diagnostics carry typed reasons and render adapter field
names without rewriting client values. UpdateObject rejects masks combining
`metadata` and `metadata.KEY` in either order, with guidance to choose one form.
Unsupported IAM roles and principal kinds recommend supported policy inputs;
switching transports does not make those inputs supported. Convenience project
members and JSON project queries share one identifier validator.

Object masks: `metadata`, `metadata.KEY`, the five fixed serving metadata strings, `custom_time`, `temporary_hold` and `event_based_hold`. `create_time` is ignored as an output-only path because the pinned Go SDK sends it for ordinary empty updates. `acl` replaces the object ACL and `retention` sets or removes object retention ([SDK parity](grpc-v2-sdk-parity.md)). `*`, contexts and other masks return UNIMPLEMENTED. Whole metadata and label maps are replaced in the shared mutation transaction without adding preconditions; explicit client preconditions remain enforced. Key-level mutations merge individual entries in that transaction.

UpdateObject constructs `ObjectMetadataUpdate` directly, including typed ACL and
nanosecond retention values. WriteObject, ComposeObject and RewriteObject share
the proto metadata conversion and shared validation. Rewrite still stores its
existing token-binding representation for compatibility, but never reparses it
through the JSON metadata parser when applying a new request.

The adapter currently accepts one match or not-match condition per generation kind; simultaneous match/not-match for the same kind returns INVALID_ARGUMENT. JSON's broader combined-condition precedence remains outside this scoped RPC acceptance.

IAM supports bucket resources, unconditional predefined-role policies, versions 0 through 3 and opaque shared policy ETags. SetPolicy constructs the shared typed policy input directly and accepts its absent/empty default mask or `bindings,etag`; other masks, conditions, audit configs and managed-folder/object resources remain unsupported. TestPermissions validates the supplied permission strings directly, deduplicates them in request order and retains per-permission authorization, including project grants, bucket grants and PAP. Its input is bounded by the gRPC message limit rather than the JSON query-string limit. Permission evaluation still makes per-permission catalog calls; batching those decisions is deferred to an authorization-layer change. Locking requires a positive matching metageneration and uses shared irreversible emulator-local state.

Rewrite inherits or replaces the JSON-supported editable metadata subset, selects source generations and applies source/destination conditions. Same-location/class copies finish in one call; changed location/class uses the shared 8 MiB default budget or a positive whole-MiB caller budget. Token-only continuation, mismatched-request rejection and last-token replay use the shared durable session. Completed gRPC responses are stored as protobuf bytes inside the existing bounded response envelope; continuation tokens must stay on the gRPC transport. Cross-transport token reuse is not supported. No body bytes are collected: the shared engine stages and copies with a fixed 64 KiB buffer.

Compose accepts 1–32 same-bucket generation-selected sources, source generation-match conditions, destination generation/metageneration-match conditions and the shared optional source-deletion behavior. Explicit checksum verification, ACL/encryption, destination storage class, temporary hold and custom time on derived objects remain unsupported. Shared object-size limits, checksums and publication conditions remain enforced. Move and restore retain the same flat-namespace, retention, versioning and soft-delete semantics as JSON. HNS restore tokens and copying source ACLs remain unsupported. Empty common encryption parameters, empty request-level checksum messages and false ACL flags have their default meaning.

No setting is added. The gRPC service receives the existing `max_object_bytes` setting so copies/compositions honor the same configured body cap as JSON. Shared storage and startup ordering are unchanged. Every broad ledger row remains PARTIAL because these explicit unsupported cases and broader provider parity remain open.

Storage errors use the common `status::from_json` mapping, including expired-session 410 as FAILED_PRECONDITION and unsupported 501 as UNIMPLEMENTED with ErrorInfo. Foundation fail-fast checks use unsupported Rapid writes; CancelResumableWrite is implemented in the [object I/O contract](grpc-v2-object-io.md). The bucket and object I/O implementations and their configured size limit remain shared.

UpdateBucket, UpdateObject and TestIamPermissions retain LOCAL_SIMULATION achieved fidelity, matching their shared JSON operations; the remaining delivered scopes have EXACT achieved fidelity with explicit pending cases. This does not turn local principals, policy propagation or lifecycle metadata into Google infrastructure.

## Verification

All 92 Storage v2 library tests passed on 2026-11-04 after adapting bucket errors
to protobuf field names. The new unsupported-class assertion failed with
storageClass on the old response. Lifecycle negative cases and the full
class/precondition precedence snapshot retain exact checks with the protobuf
names; a disabled Autoclass terminal-class mutation exercises transaction errors.
Workspace checking, production lint, formatting and compatibility rendering passed.

Write results share one permission-aware owner/ACL view, including ComposeObject,
RewriteObject, MoveObject and RestoreObject. Callers without ACL read permission
and UBLA buckets receive no owner/ACL fields. The derived-method behavior is
inferred from WriteObject/UpdateObject observations, not provider-verified;
SPEC-GRPC-DERIVED-OWNER-VIEW tracks that boundary. Completion snapshots stay
caller-independent and the response applies the current caller's view.

On 2026-11-04, all 116 gRPC library tests passed, including
`grpc_derived_writes_return_the_recorded_owner_and_acl_views`,
`grpc_derived_write_views_respect_acl_permissions_and_uniform_access` and Rapid's
`append_strict_requires_create_and_allows_viewer_reads`. The derived-result and
Rapid owner assertions failed before the shared response view was applied.
Workspace checking, production lint, formatting and compatibility rendering
passed. No SDK or provider request was made for this response change.

The direct IAM conversion passed ten IAM-related library tests and the shared
typed-policy test on 2026-11-04. Cases include JSON mixed-error precedence and
opaque ETags, invalid/raw permission strings, unsupported predefined permissions,
1024 duplicate permission values and typed policy publication. No SDK or provider
request was made for this change.

The typed object conversion passed all 109 gRPC library tests on 2026-11-04,
including the class-versus-stale-condition precedence snapshot and three new
typed update/retention tests. The three JSON derived-metadata contract tests also
passed, including exact storage-class messages. Workspace checking, production
lint, formatting and compatibility generation passed.

On 2026-11-04, `cargo test --lib grpc::v2::update_copy -- --test-threads=4`
passed sixteen tests. The new concurrent whole-map replacement regression failed
with an implicit FAILED_PRECONDITION before the fix and now passes for object
metadata and bucket labels. The shared `metadata_map_modes_apply_atomically_and_validate_the_result`
test verifies merge/removal, replacement, clearing and the resulting 8 KiB limit.
Workspace checking, production lint and formatting passed.

Before implementation, `GCS_MIMIC_GO_TEST_RUN=TestGRPCUpdateCopy bash scripts/sdk-go.sh` failed on UpdateBucket with immediate UNIMPLEMENTED. After implementation, the same command passed both ordinary and strict suites using Go Storage 1.70.0; JSON supplies fixtures and observes metadata and exact bytes. Rust tests exercise unsupported/empty masks, non-target preservation, full-map replacement, stale conditions, continuation/replay/mismatch, 32/33-source boundaries and local retention locking.

`cargo check --workspace --all-targets --locked --offline` passed before tests. Dedicated update/copy tests cover field masks, generation conditions, IAM presence preservation, continuation and compose boundaries. `just check-sdk` runs the complete Go and Ruby suites in permissive and strict profiles; the existing Go V2 signed-transfer case skips on HTTP because it requires HTTPS. Official stable-channel and Cargo-index checks confirmed the unchanged Rust 1.99.1, tonic 0.14.7 and prost 0.14.4 inputs. No dependency is added. Container/guide, benchmark, performance, real-cloud and cross-transport-token tests are excluded.

The IAM presence regression was reproduced before its fix in both Rust and Go: enabling UBLA changed an enforced PAP setting to inherited. Its regression test also changes only PAP and requires UBLA to survive. On the final source, `cargo test --locked --offline --lib grpc::v2::update_copy` passed all five tests, including shared status mapping, masks, IAM presence, rewrite continuation and compose boundaries. `just check-sdk` passed the complete Go/Ruby suites in both profiles, including default update/copy, strict and foundation fail-fast cases; the existing HTTPS-only Go V2 signature test was the sole HTTP-run skip. English/Japanese feature matrices are generated from the ledger. No container image is built.

`RUST_TEST_THREADS=8 just check` passed once on the final source with bucket and object I/O support integrated: formatting, native/Windows/Linux clippy, all workspace unit/integration/doc tests, 591-row ledger/generated-view validation and offline script checks. The main library reports 528 passing tests. The IAM presence issue is fixed; the other parent masks require no change for the single-field/reset semantics described above.

On 2026-11-14 three tests pinned the unsupported-option rejections that had no test. Each option returns UNIMPLEMENTED with ErrorInfo naming it, before any state changes:

- `grpc_updates_reject_predefined_acls_without_changing_metadata`: UpdateBucket `predefined_acl` and `predefined_default_object_acl` leave labels and metageneration unchanged; UpdateObject `predefined_acl` leaves `content_type` and metageneration unchanged.
- `grpc_derived_writes_reject_unsupported_options_without_creating_objects`: ComposeObject `destination_predefined_acl`, `kms_key`, `object_checksums`, `storage_class`, `temporary_hold` and `custom_time`; RewriteObject `destination_kms_key`, `destination_predefined_acl`, the `copy_source_encryption_*` fields, `object_checksums`, `temporary_hold` and `custom_time`; RestoreObject `restore_token`. No destination object is created.
- `grpc_iam_rejects_partial_masks_and_child_resources_without_changing_the_policy`: SetIamPolicy masks other than `bindings` plus `etag`, and managed folder or object resources on GetIamPolicy and SetIamPolicy, leave the policy unchanged; `requested_policy_version` 0 through 3 render the same policy and 4 returns INVALID_ARGUMENT, and the `bindings` plus `etag` mask replaces the policy.

`cargo test --locked --offline --lib grpc::` passed 168 tests. RPC-STORAGE-006, 007, 009, 010, 012, 017 and 021 record this as the passing case `grpc_unsupported_option_rejections`; `grpc_full_contract` stays PENDING for the remaining fields, error interactions and provider parity.

These tests pin gcs-mimic's own rejections; they do not show that Cloud Storage rejects the same requests. A later recording dated 2026-11-13 UTC (case `grpc-unsupported-options`, see the [probe evidence](../evidence/unsupported-option-probes.md)) found that Cloud Storage serves most of them: predefined ACLs on UpdateBucket, UpdateObject, ComposeObject and RewriteObject, a wildcard UpdateObject mask, `custom_time` on derived writes, policy version 2 and TestIamPermissions on an object all succeeded. It refused a wrong request checksum on ComposeObject and RewriteObject with INVALID_ARGUMENT and managed folder IAM on a bucket without uniform access with FAILED_PRECONDITION. gcs-mimic now serves policy version 2. The other rejections stay as recorded local choices under SPEC-SDK-UNSUPPORTED until each option is implemented. The same recording showed that ComposeObject and MoveObject answer without `acl` and `owner` while RewriteObject includes them; gcs-mimic now omits them on those two RPCs (`grpc_derived_writes_return_the_recorded_owner_and_acl_views`).
