# Bucket metadata PUT

<!-- markdownlint-configure-file {"MD013": false} -->

Contract checked on **2026-10-23**. Scope: JSON-BUCKETS-013,
`PUT /storage/v1/b/{bucket}`, in native and legacy-emulator profiles.
Replacement covers exactly labels, default storage class, versioning and
default event-based hold. Broader bucket settings remain separate work.

## Sources and reconciliation

- [UPDATE](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/update),
  [updates](https://docs.cloud.google.com/storage/docs/json_api#updates),
  [Bucket resource](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets),
  [label rules](https://docs.cloud.google.com/storage/docs/tags-and-labels),
  [storage classes](https://docs.cloud.google.com/storage/docs/storage-classes).
- [Discovery](https://storage.googleapis.com/$discovery/rest?version=v1), fetched
  on the check date: revision `20261013`, SHA-256
  `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`.
- Supplementary [Storage v2 proto](https://github.com/googleapis/googleapis/blob/14a161a98ef045be80c12ab69d924504a756f109/google/storage/v2/storage.proto),
  `UpdateBucketRequest.update_mask`: `*` is equivalent to JSON update and can
  reset newly introduced fields. This establishes replacement intent, not
  legacy JSON omitted-field defaults or null behavior.
- Comparison only: fake-gcs-server v1.56.1, commit
  `896ece51e49eed0946089f3089f6343855c0d106`,
  [updateBucket](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/bucket.go).

The method requires storage.buckets.update and warns that omitted metadata
may change. The overview describes entire-metadata replacement. Neither
establishes defaults for each omitted JSON field. The method calls acl[]
required but says an empty list is ignored; Discovery requires a Bucket body
without making acl a required schema property. This slice accepts omitted or
empty acl without changing permissions and rejects every other acl value.
Projection defaults to full for PUT; both projections expose the same modeled
subset without ACL/owner fields. SPEC-BUCKET-PUT records omitted defaults,
null scalars, acl wording, no-effect revisions and error precedence as open
provider questions. SPEC-017 authentication wire questions also apply.
No provider bucket is contacted or authorized by this contract.

## Frozen behavior and executable acceptance cases

| Test ID | Case | Expected behavior |
| --- | --- | --- |
| `bucket_put_replaces_omitted_fields_and_preserves_resources` | Replace a populated bucket; compare PATCH merge; GET/list/restart | Omitted modeled fields reset, identity, IAM, objects and bytes survive; one revision and changed etag |
| `bucket_put_empty_null_and_invalid_inputs` | Empty object/map, null map, invalid JSON/types, oversized body | Empty object replaces with defaults; null labels clear; invalid input 400 and body over 64 KiB 413, without mutation |
| `bucket_put_label_boundaries_and_classes` | 64/65 keys; Unicode and 63/64 characters; classes | Shared insert/PATCH label and class validation; PUT labels replace the whole map; null label values invalid |
| `bucket_put_unsupported_and_output_inputs_preserve_state` | Unsupported fields, acl, defaultObjectAcl, output-only and unknown fields, lifecycle | Unsupported 501 without mutation; omitted/empty acl and exact lifecycle:{"rule":[]} accepted; output-only/unknown ignored |
| `bucket_put_conditions_and_query_contract` | Match/not-match/both; signed int64; malformed/duplicate; projection/ACL/billing queries | Atomic valid failures 412; malformed/invalid projection 400; unsupported queries 501; no 304 |
| `bucket_put_conditioned_race_has_one_winner` | Concurrent matching replacements | Exactly one 200, others 412 and one revision increment |
| `bucket_put_strict_permissions_and_policy_preservation` | Admin/viewer/other project/anonymous/invalid token; bucket policy | Require update with existing 401/403/404 ordering; policy bindings and etag unchanged |
| `bucket_put_rechecks_revocation_and_bucket_recreation` | Previously authorized grant revoked; bucket recreated under another owner | Transaction rechecks authority before stale conditions or validation, 403 without replacement |
| `bucket_put_uses_clock_and_rolls_back_invalid_replacement` | Injected clock, invalid map/class and revision overflow | Millisecond clock-derived updated; all failed replacements roll back |
| `bucket_put_settings_control_future_object_effects` | Enable and omit/disable versioning and default hold | Future effects follow replacement; history and existing holds survive disabling |
| `bucket_put_legacy_rendering_and_restart` | Profile rendering, restart and native carry-over | Fixed legacy location/project/framing retained; actual mutable revisions/settings exposed after PUT |
| `bucket_put_full_resource_round_trip_preserves_unchanged_fields` | GET body returned with label edit in each profile | Accept rendered unchanged location; preserve other fields except revision, updated and etag |
| `bucket_put_location_comparison_is_case_insensitive` | Same location in mixed case; relocation | Accept unchanged rendered location ignoring ASCII case; relocation 501 without mutation |
| `bucket_put_null_settings_follow_profile_defaults` | Null scalar/boolean settings | Native null booleans 400; legacy null booleans false; null/empty storageClass 400 in both |
| `bucket_put_missing_bucket_validates_body_before_lookup` | Missing bucket and invalid/valid body | Permissive invalid body 400 before lookup; valid body 404; strict permission check precedes parsing |
| `bucket_put_rejects_get_only_generation_query_parameter` | generation query on PUT versus GET | PUT 400 invalidParameter; GET retains 501; state unchanged |

Every successful PUT, including an identical resource or empty replacement of
defaults, increments metageneration once and derives updated from the injected
clock without artificial advancement. Same-millisecond timestamps may repeat.
Authorization, conditions, replacement validation and write share PATCH's short
immediate SQLite transaction. No object enumeration, blob read or schema
migration is required. Authorization precedes conditions, which precede
catalog validation; adapter unsupported/invalid input may precede conditions.
These local error/revision choices are not established provider parity.

## Supported and rejected fields

Omitted or null labels clear the map; a supplied map replaces all keys.
Values must be strings, including empty strings; per-key null is invalid as
on insert, rather than PATCH key deletion. Shared label rules allow at most
64 keys, each starting with a lowercase/international letter, and keys/values
of at most 63 Unicode characters containing lowercase/international letters,
numbers, underscore or hyphen. Omitted storageClass becomes STANDARD.
Supported classes are STANDARD, NEARLINE, COLDLINE, ARCHIVE, MULTI_REGIONAL
and DURABLE_REDUCED_AVAILABILITY; REGIONAL/RAPID return 501, invalid/null/empty
class 400. The implementation shares validation tables with insert/PATCH.

Omitted versioning, omitted versioning.enabled and omitted defaultEventBasedHold
become false. Legacy null versioning, enabled or defaultEventBasedHold also
become false, consistently with insert/PATCH; native rejects these nulls with
400. This is an explicit local policy, not a conclusion inferred from PATCH.
Disabling defaults never releases an existing event hold or deletes history.
Settings apply immediately to future publication, unlike possible provider
propagation delays. Class replacement never rewrites existing objects.

Identity, name/id/project/projectNumber/timeCreated, placement, objects,
history/holds and IAM policy bindings/etag are preserved. Location may match
the rendered location ignoring ASCII case; changed location is 501. Native
compares stored placement in the transaction. Legacy accepts its rendered
US-CENTRAL1 only, never using it to rewrite stored placement.

Unsupported fields are the shared PATCH list: acl, autoclass, billing, cors,
customPlacementConfig, defaultObjectAcl, encryption, hierarchicalNamespace,
iamConfiguration, ipFilter, lifecycle, logging, objectRetention,
retentionPolicy, rpo, satisfiesPZI, satisfiesPZS, softDeletePolicy and website.
Only acl:[] and exactly lifecycle:{"rule":[]} are exceptions; defaultObjectAcl:[]
remains unsupported. Null/empty unsupported settings otherwise return 501.
Output-only id, name, kind, selfLink, projectNumber, generation, metageneration,
etag, timeCreated, updated, locationType, owner, softDeleteTime and hardDeleteTime
are ignored, as are unknown body/versioning children. None is persisted as
new supported behavior. Query predefinedAcl, predefinedDefaultObjectAcl and
userProject return 501; invalid projection and GET-only generation return 400.
The existing bounded query parser and 64 KiB metadata body cap apply.

## Reference profile differences

The pinned reference shares PUT/PATCH decoding for versioning/default holds,
resets omitted booleans, ignores labels/class and conditions, and returns fixed
revision fields. Both local profiles implement actual replacement of the
declared subset, atomic conditions and strict permissions, with durable
versioning/holds. Unsupported settings fail closed instead of false success.
Reference filesystem versioning/missing-bucket limitations are not reproduced.

After replacement, legacy retains US-CENTRAL1/region, projectNumber zero and
newline framing while exposing stored labels/class, actual metageneration,
updated and etag consistently on PUT/GET/list and restart. Untouched
metageneration-1 native buckets reopened in legacy can hide class/labels;
their legacy GET is not a lossless replacement source. This existing rendering
boundary and case-sensitive native insert location remain outside this slice.
Same-profile full-resource round trips are covered explicitly.

## Verification boundary

The acceptance table is frozen before implementation. Execution results will
be recorded here after running the committed Rust 1.99.1 toolchain.

| Command | Result |
| --- | --- |
| `cargo +1.99.1 test --locked --test bucket_http bucket_put` | Pending |
| `cargo +1.99.1 test --locked --test bucket_http bucket_patch` | Pending |
| `cargo +1.99.1 test --workspace --locked` | Pending |
| `cargo +1.99.1 fmt --all --check` | Pending |
| `cargo +1.99.1 clippy --workspace --all-targets --locked -- -D warnings` | Pending |

HTTP cases are in tests/bucket_http.rs; clock/rollback cases in tests/catalog.rs;
transaction-time authorization cases in src/storage/catalog.rs. Official SDK,
reference execution, final container/guide checks and ledger/evidence updates
are separately owned integration work. No real-cloud differential run,
dependency change or complete-method compatibility claim belongs to this slice.
