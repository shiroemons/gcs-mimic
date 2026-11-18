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

Object masks: `metadata`, `metadata.KEY`, the five fixed serving metadata strings, `custom_time`, `temporary_hold` and `event_based_hold`. `create_time` is ignored as an output-only path because the pinned Go SDK sends it for ordinary empty updates. `acl` replaces the object ACL and `retention` sets or removes object retention ([SDK parity](grpc-v2-sdk-parity.md)). `*`, contexts and other masks return UNIMPLEMENTED. A bare `metadata` mask merges the request's entries into the stored map and clears the map when the request names none; `metadata.` names the empty key. Cloud Storage did both on 2026-11-17 (case `obj-gaps-empty-metadata-key`), and the pinned Go SDK sends the bare path only with an empty map, to delete every key. A `labels` mask replaces the bucket's whole label map; no recording covers it. These changes apply in the shared mutation transaction without adding preconditions; explicit client preconditions remain enforced. Key-level mutations merge individual entries in that transaction.

An `etag` in UpdateObject's object is a condition, as the pinned proto says of `Object.etag`:
"If included in the metadata of an update or delete request message, the operation is only
performed if the etag matches that of the live object." A nonempty etag is compared exactly with
the etag of the generation the request updates, in the mutation transaction after the generation
and metageneration conditions and before the customer-supplied key. A mismatch changes nothing and
answers FAILED_PRECONDITION with the shared precondition wording. The proto names no status, so
this is the status of a failed generation or metageneration condition. It names only the live
object, so a request that selects an older generation compares that generation's etag, as its other
conditions do. An empty etag sets no condition. Both choices and the unrecorded provider answer are
recorded under SPEC-GRPC-OBJECT-REQUEST-CHOICES. Until 2026-11-18 the etag was ignored.

UpdateObject constructs `ObjectMetadataUpdate` directly, including typed ACL and
nanosecond retention values. WriteObject, ComposeObject and RewriteObject share
the proto metadata conversion and shared validation. Rewrite still stores its
existing token-binding representation for compatibility, but never reparses it
through the JSON metadata parser when applying a new request.

The adapter currently accepts one match or not-match condition per generation kind; simultaneous match/not-match for the same kind returns INVALID_ARGUMENT. JSON's broader combined-condition precedence remains outside this scoped RPC acceptance.

IAM supports bucket resources, unconditional predefined-role policies, versions 0 through 3 and opaque shared policy ETags. SetPolicy constructs the shared typed policy input directly and accepts its absent/empty default mask or `bindings,etag`; other masks, conditions, audit configs and managed-folder/object resources remain unsupported. TestPermissions validates the supplied permission strings directly, deduplicates them in request order and retains per-permission authorization, including project grants, bucket grants and PAP. Its input is bounded by the gRPC message limit rather than the JSON query-string limit. Permission evaluation still makes per-permission catalog calls; batching those decisions is deferred to an authorization-layer change. Locking requires a positive matching metageneration and uses shared irreversible emulator-local state.

Rewrite inherits or replaces the JSON-supported editable metadata subset, selects source generations and applies source/destination conditions. Same-location/class copies finish in one call; changed location/class uses the shared 8 MiB default budget or a positive whole-MiB caller budget. Token-only continuation, mismatched-request rejection and last-token replay use the shared durable session. Completed gRPC responses are stored as protobuf bytes inside the existing bounded response envelope; continuation tokens must stay on the gRPC transport. Cross-transport token reuse is not supported. No body bytes are collected: the shared engine stages and copies with a fixed 64 KiB buffer.

Compose accepts 1–32 same-bucket generation-selected sources, source generation-match conditions, destination generation/metageneration-match conditions and the shared optional source-deletion behavior. A canned destination ACL, `temporary_hold` and `custom_time` apply to derived objects as on their JSON counterparts, and ComposeObject and RewriteObject verify `object_checksums` against the object they would write (see the 2026-11-17 notes below); KMS keys, destination storage class and a destination `retention` remain unsupported. Customer-supplied keys on compose, rewrite and update follow [grpc-v2-customer-supplied-keys.md](grpc-v2-customer-supplied-keys.md). Shared object-size limits, checksums and publication conditions remain enforced. Move and restore retain the same flat-namespace, retention, versioning and soft-delete semantics as JSON. HNS restore tokens and copying source ACLs remain unsupported. Empty common encryption parameters, empty request-level checksum messages and false ACL flags have their default meaning.

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
- `grpc_derived_writes_reject_unsupported_options_without_creating_objects`: ComposeObject `destination_predefined_acl`, `kms_key`, `object_checksums`, `storage_class`, `temporary_hold` and `custom_time`; RewriteObject `destination_kms_key`, `destination_predefined_acl`, `object_checksums`, `temporary_hold` and `custom_time`; RestoreObject `restore_token` and a customer-supplied key. No destination object is created.
- `grpc_iam_rejects_partial_masks_and_child_resources_without_changing_the_policy`: SetIamPolicy masks other than `bindings` plus `etag`, and managed folder or object resources on GetIamPolicy and SetIamPolicy, leave the policy unchanged; `requested_policy_version` 0 through 3 render the same policy and 4 returns INVALID_ARGUMENT, and the `bindings` plus `etag` mask replaces the policy.

`cargo test --locked --offline --lib grpc::` passed 168 tests. RPC-STORAGE-006, 007, 009, 010, 012, 017 and 021 record this as the passing case `grpc_unsupported_option_rejections`; `grpc_full_contract` stays PENDING for the remaining fields, error interactions and provider parity.

These tests pin gcs-mimic's own rejections; they do not show that Cloud Storage rejects the same requests. A later recording dated 2026-11-13 UTC (case `grpc-unsupported-options`, see the [probe evidence](../evidence/unsupported-option-probes.md)) found that Cloud Storage serves most of them: predefined ACLs on UpdateBucket, UpdateObject, ComposeObject and RewriteObject, a wildcard UpdateObject mask, `custom_time` on derived writes, policy version 2 and TestIamPermissions on an object all succeeded. It refused a wrong request checksum on ComposeObject and RewriteObject with INVALID_ARGUMENT and managed folder IAM on a bucket without uniform access with FAILED_PRECONDITION. gcs-mimic now serves policy version 2. The other rejections stay as recorded local choices under SPEC-SDK-UNSUPPORTED until each option is implemented. The same recording showed that ComposeObject and MoveObject answer without `acl` and `owner` while RewriteObject includes them; gcs-mimic now omits them on those two RPCs (`grpc_derived_writes_return_the_recorded_owner_and_acl_views`).

On 2026-11-17 the predefined ACLs on UpdateObject, ComposeObject and RewriteObject and `temporary_hold` and `custom_time` on ComposeObject and RewriteObject were implemented through the same catalog paths as their JSON counterparts, and their markers were removed. UpdateObject replaces the ACL and refuses a request that also updates `acl`; ComposeObject and RewriteObject apply the canned ACL at publication and refuse one from an anonymous caller, and a rewrite token binds `destinationPredefinedAcl` after the other parameters. ComposeObject answers with `acl` and `owner` when it names a canned ACL, as Cloud Storage did in the recording, and without them otherwise. `grpc_object_updates_and_rewrites_apply_predefined_acls`, `grpc_derived_writes_store_a_temporary_hold_and_custom_time` and `destination_acls_refuse_anonymous_callers_and_unknown_values` pin this; the two rejection tests above keep the options that remain refused. UpdateBucket predefined ACLs, `storage_class`, KMS keys and a destination `retention` stay rejected.

Also on 2026-11-17, request checksums on ComposeObject and RewriteObject were aligned with a new recording (case `grpc-gaps-derived-checksums`, see the [gap probe evidence](../evidence/provider-differential-gap-probes.md#sixth-recording-storage-v2-request-checksums-on-derived-writes)) and the two wrong-checksum markers were removed. ComposeObject checks a declared CRC32C against the CRC32C combined from its sources before any byte is read and refuses any MD5 with INVALID_ARGUMENT `Invalid argument.`, because a composite object has none. RewriteObject refuses an MD5 that is not 16 bytes with `Provided value (<base64>) is not a base64-encoded 128-bit MD5 hash.`, then checks the declared CRC32C and MD5 against the source's stored hashes before a session starts; the token binds them after `destinationPredefinedAcl`, so a continuation that repeats other checksums is refused as a token mismatch. A mismatch answers INVALID_ARGUMENT with Cloud Storage's CRC32C or MD5 message and a `BadRequest` repeating it, CRC32C first, and writes nothing. A rewrite whose source shows no MD5, such as a composite object, refuses a declared MD5 with `Invalid argument.` as a compose does (inferred). Unlike a JSON rewrite, which ignores a supplied `md5Hash`, the Storage v2 rewrite checks it, as Cloud Storage did. Bytes that fail the digests recorded for their source are reported as damaged storage (UNAVAILABLE), never as a checksum the client specified. `grpc_derived_writes_verify_request_checksums`, `grpc_rewrite_tokens_bind_declared_checksums` and `rewrite_tokens_bind_the_stored_parameter_spelling` pin this. Cloud Storage answered the completed rewrites in that case without the source's `contentType`, which gcs-mimic copies; when Storage v2 drops it remains open.

On 2026-11-18 UpdateObject started to apply the request etag described above, and tests pinned the refusals of the object RPCs that no test named. Each test was written before the change it covers: the three etag tests failed while the etag was ignored (a stale etag updated the object, and a wrong customer key answered before the etag), then passed. The refusal tests pin the existing answers and passed unchanged:

- `update_object_applies_only_when_the_request_etag_matches`, `a_selected_generation_is_compared_with_its_own_etag` and `the_etag_is_checked_after_the_lookup_and_before_the_customer_key`: a different, quoted or malformed etag and one an earlier update superseded answer FAILED_PRECONDITION and change nothing; the current etag applies the update; an empty etag sets no condition; a selected older generation compares its own etag; a missing object answers NOT_FOUND first, and a mismatched etag answers before a wrong customer key.
- `update_object_refuses_malformed_and_unmodeled_requests_without_changing_the_object`: a missing object or update_mask, a negative generation, a match and a not-match on the same value and a retention without a mode or without retain_until_time answer INVALID_ARGUMENT; the `*`, contexts and storage_class masks answer UNIMPLEMENTED with ErrorInfo.
- `compose_object_refuses_malformed_and_unmodeled_requests_without_creating_the_destination`: a missing destination and a negative source generation or source condition answer INVALID_ARGUMENT; a destination retention, KMS key or explicit ACL answers UNIMPLEMENTED.
- `rewrite_object_refuses_malformed_and_unmodeled_requests_without_creating_the_destination`: a negative source generation, a budget that is negative or not a whole MiB, a destination naming its bucket or name, a malformed token and conflicting source conditions answer INVALID_ARGUMENT; a destination KMS key answers UNIMPLEMENTED.
- `restore_object_refuses_a_missing_generation_and_copying_the_source_acl`: a missing or negative generation answers INVALID_ARGUMENT and `copy_source_acl` true answers UNIMPLEMENTED; `copy_source_acl` false restores.

`cargo test --locked --offline --lib grpc::` passed 262 tests. KMS keys, object contexts and restore tokens, which gcs-mimic does not model, are recorded as scoped differences under SPEC-GRPC-OBJECT-REQUEST-CHOICES. The `*` update mask, the storage_class mask, destination retention, explicit ACLs and storage classes on derived writes, and `copy_source_acl` remain pending: the proto defines them, so they stay UNIMPLEMENTED until implemented. The `*` mask's recorded local choice in case `grpc-unsupported-options` is unchanged. No SDK suite or provider request was run for this change.
