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

An `etag` in UpdateObject's object is a condition, as the pinned proto says of `Object.etag` (lines 2902-2906): "If included in the metadata of an update or delete request message, the operation is only performed if the etag matches that of the live object." A nonempty etag is compared exactly with the etag of the generation the update selects, in the mutation transaction after the generation and metageneration conditions and before the customer-supplied key. A mismatch changes nothing and answers FAILED_PRECONDITION with the message of a failed generation or metageneration condition; an empty etag sets no condition. The proto names no status and speaks only of the live object, so the status and the generation compared are recorded local choices (SPEC-GRPC-LOCAL-CHOICES).

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

## Ledger cases

The ledger rows list one acceptance case per checkable behavior (decomposed on 2026-11-18). No real Cloud Storage runs are made, so an answer Google does not document and nothing recorded is a local choice, recorded as a scoped difference with a pinning test. In summary:

- Refused on purpose (scoped differences under SPEC-GRPC-UNMODELED-SETTINGS): UpdateBucket paths for settings the JSON API does not store either (`rpo`, `website`, `logging`, `encryption`, `billing`, `custom_placement_config`, `hierarchical_namespace`, `ip_filter`, `satisfies_pzs`), each pinned with its ErrorInfo; UpdateObject `contexts` and `contexts.custom` paths; ComposeObject `kms_key` and RewriteObject `destination_kms_key`; managed-folder IAM resources on GetIamPolicy, SetIamPolicy and TestIamPermissions; RestoreObject `restore_token`, which only hierarchical namespace buckets use.
- Recorded local choices: GetIamPolicy and SetIamPolicy on an object resource answer UNIMPLEMENTED, because the proto names only bucket and managed-folder resources for them; a stale policy etag, or a malformed one that is not the eight bytes every issued etag has, answers FAILED_PRECONDITION; a fractional soft delete duration is refused; UpdateBucket's implicit lifecycle check retries three times and then answers ABORTED; UpdateObject answers a mismatched `object.etag` with FAILED_PRECONDITION and compares the etag of the generation the update selects (all SPEC-GRPC-LOCAL-CHOICES). Locking a bucket without a policy, or twice, answers INVALID_ARGUMENT (SPEC-RETENTION-POLICY). RestoreObject refuses a customer-supplied key (SPEC-GRPC-CSEK) and answers with owner and ACL (SPEC-GRPC-DERIVED-OWNER-VIEW).
- Pending code work: a set `bucket.etag` is ignored although the proto makes the update conditional on it; `*` update masks; UpdateBucket predefined ACLs, which Cloud Storage and the JSON PATCH apply; SetIamPolicy masks that name only some policy fields; TestIamPermissions on an object, which the proto documents and Cloud Storage serves; a ComposeObject destination `storage_class`; destination `retention`, `acl` and output-only fields on ComposeObject and RewriteObject; RestoreObject `copy_source_acl`.
- Pending tests: strict LockBucketRetentionPolicy; the whole-second truncation of `retention_policy.retention_duration`, which follows the proto's rounding (CreateBucket still refuses the same input); the separate `storage.buckets.setIamPolicy` check for public access prevention and ACL changes; the refusal of conditional bindings and audit configs; ComposeObject's `storage.objects.get` and overwrite `storage.objects.delete` permissions; the owner view of a canned-ACL ComposeObject for a caller without ACL read permission; a rewrite token used on the other transport.
- Decisions: one validation rule for bucket and object conditions across RPCs, recorded on each row whose answer the choice changes or confirms: UpdateObject, MoveObject, RestoreObject, RewriteObject and ComposeObject refuse a metageneration condition of 0 and a match together with a not-match, while the object reads, DeleteObject and writes evaluate them (LockBucketRetentionPolicy's refusal of 0 follows the proto, which requires a positive value); MoveObject's permission set, which checks `storage.objects.get`, `.delete` and `.create` as the JSON move does where the proto lists `storage.objects.move`, `.create`, and `.delete` only when overwriting; whether a rewritten object keeps the source's content type, which Cloud Storage's recorded completed rewrites did not carry.

GetIamPolicy (RPC-STORAGE-006) has no pending case of its own. Like every Storage v2 row it depends on BEHAVIOR-RPC-001, because every RPC passes its routing and request-metadata handling, so it stays PARTIAL while that row's routing SDK lanes, server-side `grpc-timeout` and gzip message compression are pending.

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

`cargo test --locked --offline --lib grpc::` passed 168 tests. RPC-STORAGE-006, 007, 009, 010, 012, 017 and 021 record this as the passing case `grpc_unsupported_option_rejections`; `grpc_full_contract` stays PENDING for the remaining fields, error interactions and provider parity. On 2026-11-18 `grpc_full_contract` was replaced by the decomposed cases summarized under [ledger cases](#ledger-cases).

These tests pin gcs-mimic's own rejections; they do not show that Cloud Storage rejects the same requests. A later recording dated 2026-11-13 UTC (case `grpc-unsupported-options`, see the [probe evidence](../evidence/unsupported-option-probes.md)) found that Cloud Storage serves most of them: predefined ACLs on UpdateBucket, UpdateObject, ComposeObject and RewriteObject, a wildcard UpdateObject mask, `custom_time` on derived writes, policy version 2 and TestIamPermissions on an object all succeeded. It refused a wrong request checksum on ComposeObject and RewriteObject with INVALID_ARGUMENT and managed folder IAM on a bucket without uniform access with FAILED_PRECONDITION. gcs-mimic now serves policy version 2. The other rejections stay as recorded local choices under SPEC-SDK-UNSUPPORTED until each option is implemented. The same recording showed that ComposeObject and MoveObject answer without `acl` and `owner` while RewriteObject includes them; gcs-mimic now omits them on those two RPCs (`grpc_derived_writes_return_the_recorded_owner_and_acl_views`).

On 2026-11-17 the predefined ACLs on UpdateObject, ComposeObject and RewriteObject and `temporary_hold` and `custom_time` on ComposeObject and RewriteObject were implemented through the same catalog paths as their JSON counterparts, and their markers were removed. UpdateObject replaces the ACL and refuses a request that also updates `acl`; ComposeObject and RewriteObject apply the canned ACL at publication and refuse one from an anonymous caller, and a rewrite token binds `destinationPredefinedAcl` after the other parameters. ComposeObject answers with `acl` and `owner` when it names a canned ACL, as Cloud Storage did in the recording, and without them otherwise. `grpc_object_updates_and_rewrites_apply_predefined_acls`, `grpc_derived_writes_store_a_temporary_hold_and_custom_time` and `destination_acls_refuse_anonymous_callers_and_unknown_values` pin this; the two rejection tests above keep the options that remain refused. UpdateBucket predefined ACLs, `storage_class`, KMS keys and a destination `retention` stay rejected.

Also on 2026-11-17, request checksums on ComposeObject and RewriteObject were aligned with a new recording (case `grpc-gaps-derived-checksums`, see the [gap probe evidence](../evidence/provider-differential-gap-probes.md#sixth-recording-storage-v2-request-checksums-on-derived-writes)) and the two wrong-checksum markers were removed. ComposeObject checks a declared CRC32C against the CRC32C combined from its sources before any byte is read and refuses any MD5 with INVALID_ARGUMENT `Invalid argument.`, because a composite object has none. RewriteObject refuses an MD5 that is not 16 bytes with `Provided value (<base64>) is not a base64-encoded 128-bit MD5 hash.`, then checks the declared CRC32C and MD5 against the source's stored hashes before a session starts; the token binds them after `destinationPredefinedAcl`, so a continuation that repeats other checksums is refused as a token mismatch. A mismatch answers INVALID_ARGUMENT with Cloud Storage's CRC32C or MD5 message and a `BadRequest` repeating it, CRC32C first, and writes nothing. A rewrite whose source shows no MD5, such as a composite object, refuses a declared MD5 with `Invalid argument.` as a compose does (inferred). Unlike a JSON rewrite, which ignores a supplied `md5Hash`, the Storage v2 rewrite checks it, as Cloud Storage did. Bytes that fail the digests recorded for their source are reported as damaged storage (UNAVAILABLE), never as a checksum the client specified. `grpc_derived_writes_verify_request_checksums`, `grpc_rewrite_tokens_bind_declared_checksums` and `rewrite_tokens_bind_the_stored_parameter_spelling` pin this. Cloud Storage answered the completed rewrites in that case without the source's `contentType`, which gcs-mimic copies; when Storage v2 drops it remains open.

On 2026-11-18 UpdateObject started to apply the request etag described above. The three tests in `src/grpc/v2/update_object/object_etag.rs` were written before the change and failed while the etag was ignored: a stale etag updated the object, and a wrong customer key answered before the etag. They passed after it. `update_object_applies_only_when_the_request_etag_matches` refuses a different, quoted, malformed or superseded etag without changing the object, applies the current one, and sets no condition for an empty one; `a_selected_generation_is_compared_with_its_own_etag` compares a selected noncurrent generation with its own etag; `the_etag_is_checked_after_the_lookup_and_before_the_customer_key` answers NOT_FOUND for a missing object and the etag mismatch before a wrong customer key. The tests read each etag through GetObject. `cargo test --locked --offline --lib grpc::` passed 270 tests. No SDK suite or provider request was run for this change.
