# Scoped bucket IAM policies

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

Contract checked: **2026-10-22**. Scope: JSON-BUCKETS-003,
JSON-BUCKETS-011, DESIGN-014, DESIGN-016 and DESIGN-027. This contract is
fixed before implementation; the feature ledger owns implementation status.

## Sources

All sources were checked on 2026-10-22. Storage pages displayed
"Last updated 2026-10-21 UTC" unless stated otherwise.

- [getIamPolicy](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/getIamPolicy)
  and [setIamPolicy](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/setIamPolicy):
  paths, verbs, fields and required permissions.
- [Policy](https://docs.cloud.google.com/iam/docs/reference/rest/v1/Policy)
  (last updated 2025-05-21): valid versions 0, 1 and 3, base64 etags,
  nonempty binding members and 1,500 principal occurrences per policy.
- [ProtoJSON](https://protobuf.dev/programming-guides/json/): bytes accept standard
  or URL-safe base64, padded or unpadded; empty bytes are the default value.
- [Allow policy versions](https://docs.cloud.google.com/iam/docs/allow-policies#versions)
  (last updated 2026-10-13): unconditional policies return version 1 even when
  the caller supplies version 3.
- [IAM overview and convenience values](https://docs.cloud.google.com/storage/docs/access-control/iam):
  additive project/bucket grants and convenience-member semantics.
- [Roles and modifiable defaults](https://docs.cloud.google.com/storage/docs/access-control/iam-roles#modifiable-behavior):
  the existing pinned `src/auth/roles.rs` snapshot and non-UBLA defaults.
- [JSON permissions](https://docs.cloud.google.com/storage/docs/access-control/iam-json)
  and [status codes](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes):
  route permissions, 401/403 and 412 `conditionNotMet`.
- [Principal identifiers](https://docs.cloud.google.com/iam/docs/principal-identifiers)
  (last updated 2026-10-13): recognized member kinds.
- [IAM quotas](https://docs.cloud.google.com/iam/quotas)
  (last updated 2026-10-13): principal occurrences are not deduplicated.
- [Live Storage v1 Discovery](https://storage.googleapis.com/$discovery/rest?version=v1):
  revision `20261013`, SHA-256
  `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`;
  re-fetched on 2026-10-22, identical to the source manifest. Its Storage-specific
  minimum requested version and required bindings override the generic Policy
  message's looser input rules.

## Supported subset

GET `/storage/v1/b/{bucket}/iam` accepts `optionsRequestedPolicyVersion`
omitted or 1, 3. PUT at the same path replaces the Policy supplied directly
as JSON. Both follow existing common-query rules: `userProject`, even empty,
returns 501 because requester billing is unsupported. Unknown query keys and
duplicates return 400. GET requires an empty body; PUT uses the existing 64 KiB
bounded JSON reader. Unknown JSON fields are ignored; recognized unsupported
semantics are rejected. `kind` and `resourceId` are ignored in requests.
`auditConfigs` returns 501 because it is absent from the
[Storage Discovery](https://storage.googleapis.com/$discovery/rest?version=v1)
`storage#policy` schema but recognized by the generic
[IAM Policy message](https://docs.cloud.google.com/iam/docs/reference/rest/v1/Policy).

Responses contain `kind: "storage#policy"`,
`resourceId: "projects/_/buckets/{bucket}"`, `version: 1`, `etag` and
`bindings: [{role, members}]`. Unconditional input versions 0, 1, 3 and omitted
versions normalize to 1. `bindings` is required; explicit `[]` clears the policy.
Each binding must name a role and at least one member. A Go SDK `SetPolicy` with
zero bindings omits the field and is rejected, as required by this Discovery
snapshot; send an explicit empty array to clear a policy over HTTP.

Roles are the bucket-grantable entries in the predefined storage snapshot.
The snapshot records every role's documented lowest grantable resource;
managed-folder roles also apply to buckets, while Project-only
`roles/storage.hmacKeyAdmin` and `roles/storage.expressModeUserAccess` return
400 `invalid`. Project-level identity registry grants are unchanged.
Supported members are
`user:<id>`, `serviceAccount:<id>` and the convenience members
`projectOwner:<project>`, `projectEditor:<project>`, `projectViewer:<project>`.
Local IDs follow the registry's 1..=254-byte visible-ASCII rule, and comparison
with a registry identity ID is exact and case-sensitive. Unknown local IDs may
be bound in advance; they match no caller until that identity is registered.
Convenience projects use the existing project validator. The registry has no
basic project roles, so no local principal belongs to a convenience set.
This follows the membership model rather than ignoring a matching grant.

New buckets have `roles/storage.legacyBucketOwner` bound to their owning
project's `projectOwner` and `projectEditor`, and
`roles/storage.legacyBucketReader` bound to its `projectViewer`. These are the
documented non-UBLA defaults. Migration gives existing buckets these same defaults.

Limits are 1,500 principal occurrences across all bindings, counting duplicates,
and 64 KiB JSON body/persisted bindings. The body cap can be reached before the
principal limit when IDs are long. Duplicate members and repeated bindings for
the same role are stored and returned in their submitted order, without merging
or deduplication. The official setIamPolicy and Policy references checked above
do not specify their normalization; this preservation is the supported local
behavior, with cloud normalization parity unverified.
No policy update increases bucket
metageneration. An opaque base64 etag changes on every successful set, including
identical sets. An omitted or empty-string etag overwrites, following the
ProtoJSON bytes default. Malformed base64 returns 400 `invalid`; standard and
URL-safe encodings, with or without padding, compare as the same bytes.
A supplied different valid etag returns 412 without changing policy or etag.
Comparison and update are one immediate
catalog transaction. A catalog-wide persistent counter prevents etag reuse
after bucket deletion/recreation. Policies are deleted with their bucket.

Strict get/set require `storage.buckets.getIamPolicy` and
`storage.buckets.setIamPolicy`, respectively. Each bucket-scoped operation
checks the union of the caller's project roles and matching bucket bindings
inside the transaction that selects or mutates state. Upload admission checks
are advisory; publication repeats authorization and overwrite-delete permission.
The owner observed during initial authorization is still bound to the request,
preserving cross-project recreation denial. Bucket insert/list stay project scoped.
Revocation is immediate for subsequent operations; an already authorized open
download can finish its selected immutable generation. Permissive mode persists
and validates policies and etags identically, without permission checks.

## Errors

| Condition | Result |
| --- | --- |
| Malformed JSON | 400 `parseError` |
| Invalid policy shape, version, role/member, non-bucket-grantable role, malformed base64 etag, empty binding or excessive principals | 400 `invalid` |
| Malformed/unknown/duplicate query or invalid bucket path | Existing 400 query/path error |
| Recognized public/group/domain/federated/deleted member, basic/custom role, condition | 501 `notImplemented` |
| Requester billing/common unsupported query semantics | 501 `notImplemented` |
| Audit logging configuration | 501 `notImplemented` |
| Oversized body | Existing 413 `uploadTooLarge` |
| Stale etag | 412 `conditionNotMet`; no mutation |
| Anonymous or missing permission | Existing 401 `required` or 403 `forbidden` |
| Missing bucket | Existing SPEC-017 project-permission-dependent 404/403 rule |
| Unsupported method on the IAM endpoint | 405 `methodNotAllowed`, `Allow: GET, PUT` |

Credential resolution, route/query parsing, anonymous rejection and advisory
authorization precede body parsing. The operation transaction rechecks current
authorization before comparing an etag or reading/mutating state. Rejections
preserve the existing policy, its etag, object data and bucket metadata.

## Acceptance cases

| Behavior | Rust test functions |
| --- | --- |
| Default policy and requested versions | `iam_default_policy_and_versions` |
| Replacement, ignored fields, empty policy and fresh etags | `iam_policy_round_trip_and_unconditional_versions` |
| Duplicate members and repeated role bindings preserve order | `iam_preserves_duplicate_members_and_role_bindings` |
| Persistence across restart | `iam_policy_survives_restart` |
| Existing v5 database migration preserves objects | `iam_migrates_v5_catalog` |
| Migration covers binary name boundaries with unique default revisions | `policy_migration_visits_every_bucket_in_binary_name_order` |
| Late migration failure rolls back and restart succeeds | `failed_policy_migration_rolls_back_and_restarts_from_version_five` |
| Missing/orphan policy and stale revision allocator reject startup without changing files | `rejects_future_schema_unversioned_data_and_corruption` |
| Permissive mode keeps validation and CAS | `iam_permissive_policy_and_cas` |
| Anonymous and get/set permission denials | `iam_requires_effective_get_and_set_permissions` |
| Project and bucket grants, scope isolation, revocation | `iam_bucket_grants_authorize_only_the_target_bucket` |
| All bucket/object routes and overwrite delete permission | `iam_bucket_grants_cover_existing_routes`, `iam_bucket_grants_authorize_only_the_target_bucket` |
| Empty, stale and concurrent etag sets; unpadded byte equivalence | `iam_stale_and_concurrent_etags` |
| Standard/URL-safe base64 byte equivalence | `etag_bytes_accept_protojson_encodings_and_normalize_for_comparison` |
| Revocation racing an object read | `iam_revocation_races_object_reads`, `revoked_bucket_binding_is_rechecked_inside_operations` |
| Delete/recreate resets policy and never reuses etag | `iam_recreated_bucket_has_fresh_policy` |
| Each invalid/unsupported policy class and query/body errors | `iam_rejects_invalid_policy_shapes`, `iam_rejects_unsupported_policy_semantics`, `iam_rejects_queries_paths_and_bodies` |
| Principal and body size boundaries | `iam_principal_and_body_limits` |
| Existing strict authentication/check order | `auth_http_strict_rejects_anonymous_and_unsupported_credentials`, `auth_http_project_roles_gate_each_route`, `auth_http_rejected_uploads_do_not_read_or_stage_the_body` |
| Existing cross-project recreation and missing-bucket races | `auth_http_upload_into_a_bucket_recreated_in_another_project_is_forbidden`, `operations_recheck_the_authorized_project_after_bucket_recreation`, `missing_bucket_access_reauthorizes_against_the_actual_project` |
| Existing overwrite concurrency and preconditions | `auth_http_concurrent_creates_without_delete_have_one_winner`, `forbidden_overwrite_is_rechecked_atomically_after_a_concurrent_create` |

The HTTP cases live in `tests/auth_http/iam.rs`, using the existing
`tests/auth_http.rs` server and `tests/common` process helpers. The deterministic
revocation test retains a previously authorized access, revokes its binding,
then proves each catalog operation and a prepared upload reject it. The HTTP
race permits a read selected before revocation to finish, and checks every
post-revocation request is denied.

## Verification

Executed on 2026-10-22:

- `cargo test --locked --offline`: 227 passed, 0 failed; includes 16 IAM HTTP
  test functions, deterministic catalog revocation, startup invariant rejection
  and successful/rolled-back policy migration cases.
- `cargo fmt --check`: passed.
- `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`:
  passed.
- The initial slice's `cargo xtask dependencies check` passed with all compiler,
  crate, SDK, action and container inputs CURRENT. These fixes add no dependency;
  the freshness check was not repeated.

SDK integration, container verification and compatibility-ledger updates are
separate integration work. No cloud differential request was made.

## Excluded scope

`testIamPermissions`; `allUsers`/`allAuthenticatedUsers`; groups, domains,
workforce/workload/federated and deleted principals; binding conditions;
custom/basic role grants; bucket/object/default ACLs; organization/folder
inheritance and deny policies; UBLA transitions and UBLA-dependent default
object-role bindings; public access prevention; XML; signed URLs; real Google
token verification; policy propagation delays and cloud differential evidence.
Default object ACLs are separate from non-UBLA bucket IAM and are not synthesized.
The inherited project scope is the local registry only. SPEC-017's error wording,
missing-bucket and check-order uncertainties remain open for their original rows.
