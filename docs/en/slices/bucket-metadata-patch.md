# Bucket metadata PATCH

<!-- markdownlint-configure-file {"MD013": false} -->

Contract checked on **2026-10-23**. Scope: JSON-BUCKETS-008,
`PATCH /storage/v1/b/{bucket}`, in native and legacy-emulator profiles.
The supported subset is labels, default storage class, versioning and default
event-based hold. Broader bucket settings and PUT are separate work.

## Sources and reconciliation

- [PATCH](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/patch),
  [Bucket resource](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets),
  [partial updates](https://docs.cloud.google.com/storage/docs/json_api#partial-updates),
  [label rules](https://docs.cloud.google.com/storage/docs/tags-and-labels),
  [label updates](https://docs.cloud.google.com/storage/docs/using-bucket-labels),
  [storage classes](https://docs.cloud.google.com/storage/docs/storage-classes),
  [status codes](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes).
- [Discovery](https://storage.googleapis.com/$discovery/rest?version=v1), fetched
  on the check date: revision `20261013`, SHA-256
  `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`.
- Supplementary [Storage v2 proto](https://github.com/googleapis/googleapis/blob/14a161a98ef045be80c12ab69d924504a756f109/google/storage/v2/storage.proto)
  and [output-only guidance](https://google.aip.dev/203#output-only).
- Comparison only: fake-gcs-server v1.56.1, commit
  `896ece51e49eed0946089f3089f6343855c0d106`,
  [updateBucket/getBucketAttrsToUpdate](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/bucket.go).

The method page defaults projection to noAcl while Discovery describes full;
both return the same modeled subset here. Existing SPEC-017 authentication
wire questions also apply to PATCH. Legacy JSON unknown/output-only handling
and scalar deletion are not established by Discovery: this slice explicitly
uses insert's ignored unknown/output-only input rule and rejects null/empty
storageClass with 400. These local boundaries require authorized service
differential evidence before any whole-method fidelity claim. No real bucket
is contacted. Labels are optional metadata: null removes the map, a map merges,
and per-key null removes that key, following partial-update rules and the
official label-removal examples. Empty maps merge no keys.
Whole-map label removal is an interpretation of the general PATCH deletion
rule, not a direct-service observation. Scalar deletion/default behavior,
unknown inputs and output-only inputs remain explicitly unverified against
the legacy service, as the earlier insert/object-metadata contracts record.

## Frozen behavior and executable acceptance cases

| Test ID | Case | Expected behavior |
| --- | --- | --- |
| `bucket_patch_merges_labels_class_and_preserves_resources` | Omitted fields; set, replace, remove and remove missing labels; empty values | Merge only supplied label keys, retain omitted values; null key removes; empty string is stored |
| `bucket_patch_empty_null_and_invalid_inputs` | `labels:{}`, `labels:null`, empty object/body, invalid JSON/types | Empty map/object preserves settings, null map clears; empty wire body/invalid input is 400 |
| `bucket_patch_label_boundaries_and_classes` | 64/65 final keys, invalid key/value, Unicode/63/64 characters | Same insert label rules, maximum 64 final labels; validation and update are atomic |
| `bucket_patch_label_boundaries_and_classes` | Supported/unsupported/default storage classes | STANDARD, NEARLINE, COLDLINE, ARCHIVE, MULTI_REGIONAL, DURABLE_REDUCED_AVAILABILITY; REGIONAL/RAPID return 501; invalid/null/empty class returns 400 |
| `bucket_patch_unsupported_and_output_inputs_preserve_state` | Recognized unsupported settings, unknown and output-only fields | Unsupported settings return 501 without mutation, even null/empty; unknown/output-only inputs cannot override stored identity |
| `bucket_patch_conditions_and_query_contract` | match/not-match/both, signed int64 boundaries and malformed/duplicate values, missing bucket | Failed valid comparisons return 412; missing bucket 404; malformed query 400; no read-style 304 |
| `bucket_patch_conditions_and_query_contract` | projection, predefined ACL queries, userProject | noAcl/full return modeled fields; invalid projection 400; predefinedAcl/predefinedDefaultObjectAcl/userProject return 501 without mutation |
| `bucket_patch_conditioned_race_has_one_winner` | Concurrent matching updates | Exactly one 200 and all other requests 412; one revision increment |
| `bucket_patch_strict_permissions_and_policy_preservation` | admin/viewer/other-project/anonymous/invalid token, bucket policy grant | Require storage.buckets.update before reading body; existing local 401/403/404 ordering and transaction-time grant check; policy etag/bindings unchanged |
| `bucket_patch_rechecks_revocation_and_bucket_recreation` | Revoke a previously authorized grant; recreate a bucket in another project | Transaction-time authorization fails 403 before stale conditions; replacement metadata is unchanged |
| `bucket_patch_merges_labels_class_and_preserves_resources` | Identity, object bytes/metadata, GET/list consistency, restart | One metageneration increment, changed etag and clock-derived updated, stable creation/project/name; object and IAM state unchanged; persistent catalog |
| `bucket_patch_uses_clock_and_rolls_back_invalid_merge` | Injected clock, final-map validation and revision overflow | updated uses catalog clock at millisecond precision; invalid final map/overflow rolls back every field |
| `bucket_patch_settings_control_future_object_effects` | Enable/disable versioning and default hold | Future uploads/overwrites use new settings; prior holds/history survive setting changes |
| `bucket_patch_legacy_rendering_and_restart` | Source-only name, settings, class/labels, restart and native carry-over | Source location/framing retained; PATCH exposes actual mutable metadata/revisions consistently on GET/list |

Every successful PATCH, including an empty object or removal of a missing key,
increments metageneration exactly once and sets updated from the injected catalog
clock. With a non-decreasing clock, updated is non-decreasing; successive
PATCHes in the same millisecond may return identical updated values.
Metageneration and etag identify every successful change; timestamps are not
artificially incremented. The injected clock is used as supplied, without a
monotonicity override. All field merging,
authorization, condition checks, final-label validation and publication share
one short immediate SQLite transaction. Schema v8 already stores every field;
no migration, blob access or object enumeration is required.

## Supported and rejected fields

Omitted settings are preserved. `versioning.enabled` and
`defaultEventBasedHold` accept booleans; false disables their future effect.
Empty versioning objects preserve enabled; null settings are invalid. Existing
event holds are not released by disabling the default, and retained versions
are not removed by disabling versioning. Storage class is bucket default
metadata, not physical placement; changing it never rewrites existing objects.

Label keys must be nonempty and start with a lowercase/international letter;
keys/values allow lowercase/international letters, numeric characters, `_` and
`-`, at most 63 Unicode characters. Values may be empty. The final merged map
has at most 64 keys, including retained labels. Invalid deletion keys are also
rejected. The existing 64 KiB metadata body cap and bounded query parser apply.

Recognized unsupported fields are acl, autoclass, billing, cors,
customPlacementConfig, defaultObjectAcl, encryption, hierarchicalNamespace,
iamConfiguration, ipFilter, lifecycle, logging, objectRetention,
retentionPolicy, rpo, satisfiesPZI, satisfiesPZS, softDeletePolicy and website.
The insert-only empty-lifecycle SDK exception does not apply to PATCH.
Location changes are also unsupported; an unchanged location is accepted.
Output-only id, name, kind, selfLink, projectNumber, generation, metageneration,
etag, timeCreated, updated, locationType, owner, softDeleteTime and hardDeleteTime
are ignored even if changed or null. They cannot rename or move a bucket.
Unknown body fields follow existing insert handling and are ignored. No unknown
field is persisted or treated as supported behavior.

The `versioning` object likewise ignores unknown children while applying only
the optional boolean `enabled`. Empty labels values remain distinct from null
removal. REGIONAL and RAPID need unsupported region/zone placement; accepting
their names would imply settings that neither profile can enforce. The class
set is shared with insert rather than duplicated in the adapter.

Strict authentication is resolved before routing; query syntax precedes bucket
permission checks; permission precedes body parsing; inside the transaction,
authorization precedes conditions, and conditions precede final-map validation.
Recognized unsupported body/query inputs are rejected before any mutation;
their 501 can precede a stale condition. ACL queries never apply ACLs and hence
do not claim setIamPolicy enforcement; implementing them requires the extra
storage.buckets.setIamPolicy check. Full projection has no ACL/owner properties
and currently needs no getIamPolicy grant, as on other bucket routes.

## Reference profile differences

The pinned reference PATCH decodes only versioning/defaultEventBasedHold,
resets omitted booleans to false, ignores labels/class and does not enforce
conditions. Both local profiles implement merge semantics, effective settings,
atomic conditions and strict permissions. Native persistence retains history
and holds regardless of reference backend limitations.
The reference memory backend supports both settings and returns 404 for a
missing bucket; its filesystem backend returns 500 for versioning=true and
for a missing bucket. Both local profiles support durable versioning and use
the ordinary authorized missing-bucket 404 rule. The source also accepts
unmodeled protection/query inputs without enforcing them; local 501 rejection
is an intentional safety and compatibility boundary rather than false success.

Legacy creation and untouched resources keep their existing fixed defaults.
After a successful PATCH, legacy responses preserve US-CENTRAL1/region,
projectNumber zero and JSON newline framing while exposing stored class,
labels, actual metageneration, updated and etag. This explicit extension over
reference constants keeps subsequent GET/list/SDK conditional updates usable.
The same rendering applies after restart; it introduces no second metadata
state. Unsupported protection settings remain fail-closed in both profiles.

## Verification boundary

Verification on the contract check date, using the committed Rust 1.99.1 pin:

| Command | Result |
| --- | --- |
| `cargo +1.99.1 test --locked bucket_patch` | PASS: all 11 new PATCH tests, including both HTTP profiles |
| `cargo +1.99.1 test --workspace --locked` | PASS: all workspace suites, zero failures or ignored tests |
| `cargo +1.99.1 fmt --all --check` | PASS |
| `cargo +1.99.1 clippy --workspace --all-targets --locked -- -D warnings` | PASS |
| `cargo +1.99.1 xtask dependencies check` | PASS: compiler, direct libraries, SDK and pinned container inputs CURRENT |

The executable HTTP cases are in `tests/bucket_http.rs`; the injected-clock and
rollback case is in `tests/catalog.rs`; the stale-authorization case is in
`src/storage/catalog.rs`. Route/Allow coverage in `src/json/tests.rs` also now
recognizes PATCH while PUT remains unavailable.

Official SDK and pinned reference execution are separately owned integration
checks. They are not inferred from the Rust cases. No schema migration,
dependency update, container build or real-cloud differential run belongs to
this implementation. The feature ledger, matrices and user guides are updated
by the final integration task, not by this contract document.
