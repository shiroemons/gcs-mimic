# Bucket metadata PUT

<!-- markdownlint-configure-file {"MD013": false} -->

Contract checked on **2026-10-23**. Scope: JSON-BUCKETS-013,
`PUT /storage/v1/b/{bucket}`, in native and legacy-emulator profiles.
Replacement covers exactly labels, default storage class, versioning and
default event-based hold. Broader bucket settings remain separate work.

## Sources and reconciliation

- [UPDATE](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/update),
  [partial updates](https://docs.cloud.google.com/storage/docs/json_api#patch),
  [Bucket resource](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets),
  [label rules](https://docs.cloud.google.com/storage/docs/tags-and-labels),
  [storage classes](https://docs.cloud.google.com/storage/docs/storage-classes).
- [Discovery](https://storage.googleapis.com/$discovery/rest?version=v1), fetched
  on the check date: revision `20261013`, SHA-256
  `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`.
- Supplementary [Storage v2 proto](https://github.com/googleapis/googleapis/blob/14a161a98ef045be80c12ab69d924504a756f109/google/storage/v2/storage.proto),
  SHA-256 `a0df6132967b97a89be248f1f24d277ee8df8f8d885db1d4af6249469ff0c1cf`,
  `UpdateBucketRequest.update_mask`: `*` is equivalent to JSON update and can
  reset newly introduced fields. This establishes replacement intent, not
  legacy JSON omitted-field defaults or null behavior.
- Comparison only: fake-gcs-server v1.56.1, commit
  `896ece51e49eed0946089f3089f6343855c0d106`,
  [router](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/server.go).

The method requires storage.buckets.update and warns that omitted metadata
may change. The overview contrasts PATCH with entire-metadata replacement;
it does not directly describe PUT/update. Neither establishes defaults for
each omitted JSON field. Both the method page and Discovery mark acl[] required:
Discovery uses `annotations.required: [storage.buckets.update]`. The method
also says an empty list is ignored. Accepting omitted acl without changing
permissions is an explicit local choice for SDK full-resource round trips,
rather than a behavior established by those sources. Empty acl is ignored;
every other acl value is rejected.
Projection defaults to full for PUT; both projections expose the same modeled
subset without ACL/owner/ipFilter fields, so neither storage.buckets.getIamPolicy
nor storage.buckets.getIpFilter is required. Ignored acl:[] does not require
storage.buckets.setIamPolicy. SPEC-017 retains full-projection permission
questions; SPEC-BUCKET-PUT explicitly retains whether getIpFilter is required
for the modeled response and whether ignored acl:[] requires setIamPolicy.
SPEC-BUCKET-PUT records omitted defaults,
null scalars, acl wording, no-effect revisions and error precedence as open
provider questions. SPEC-017 authentication wire questions also apply.
No provider bucket is contacted or authorized by this contract.

## Frozen behavior and executable acceptance cases

| Test ID | Case | Expected behavior |
| --- | --- | --- |
| `bucket_put_replaces_omitted_fields_and_preserves_resources` | Replace a populated bucket; compare PATCH merge; GET/list/restart | Omitted modeled fields reset under local policy; identity, IAM, objects and bytes survive; one revision and changed etag |
| `bucket_put_empty_null_and_invalid_inputs` | Empty object/map, null map, versioning:{}, empty wire body, top-level null/array, invalid JSON/types, oversized body | Empty object replaces with local defaults; null labels clear; versioning:{} disables; empty wire body/null/array/invalid input 400 and body over 64 KiB 413, without mutation |
| `bucket_put_label_boundaries_and_classes` | 64/65 keys; Unicode and 63/64 characters; classes | Shared insert/PATCH label and class validation; PUT labels replace the whole map; null label values invalid |
| `bucket_put_unsupported_and_output_inputs_preserve_state` | Unsupported fields, omitted acl versus acl:[], defaultObjectAcl, output-only/unknown fields, versioning:{"enabled":true,"future":1}, lifecycle | Unsupported 501 without mutation; omitted acl accepted as local policy; acl:[] and exact lifecycle:{"rule":[]} ignored; unknown versioning children ignored while enabled applies |
| `bucket_put_conditions_and_query_contract` | Match/not-match/both; signed int64; malformed/duplicate; projection/ACL/billing queries; stale condition with invalid label/unsupported field; malformed query with denied access and invalid body | Atomic valid failures 412; HTTP invalid label precedes stale condition with 400; unsupported field precedes it with 501; query syntax precedes permission, which precedes body parsing; malformed/invalid projection 400; unsupported queries 501; no 304 |
| `bucket_put_conditioned_race_has_one_winner` | Concurrent matching replacements | Exactly one 200, others 412 and one revision increment |
| `bucket_put_strict_permissions_and_policy_preservation` | Admin/viewer/other project/anonymous/invalid token; bucket policy | Require update with existing 401/403/404 ordering; policy bindings and etag unchanged |
| `bucket_put_rechecks_revocation_and_bucket_recreation` | Previously authorized grant revoked; bucket recreated under another owner; valid and invalid replacements | Transaction rechecks authority before stale conditions or invalid-label validation, 403 without replacement |
| `bucket_put_uses_clock_and_rolls_back_invalid_replacement` | Injected clock, invalid map/class with and without stale conditions, revision overflow | Direct catalog calls check stale conditions before label/class validation with 412; millisecond clock-derived updated; all failed replacements roll back |
| `bucket_put_settings_control_future_object_effects` | Enable and omit/disable versioning and default hold | Future effects follow replacement; history and existing holds survive disabling |
| `bucket_put_legacy_rendering_and_restart` | Profile rendering, restart and native carry-over | Fixed legacy location/project/framing retained; actual mutable revisions/settings exposed after PUT |
| `bucket_put_full_resource_round_trip_preserves_unchanged_fields` | GET body returned with label edit in each profile | Accept rendered unchanged location; preserve other fields except revision, updated and etag |
| `bucket_put_location_comparison_is_case_insensitive` | Same location in mixed case; relocation; legacy location:"US" | Accept unchanged rendered location ignoring ASCII case; relocation and legacy location:"US" return 501 without mutation |
| `bucket_put_null_settings_follow_profile_defaults` | Null scalar/boolean settings | Native null booleans 400; legacy null booleans false; null/empty storageClass 400 in both |
| `bucket_put_missing_bucket_validates_body_before_lookup` | Missing bucket and invalid/valid body, including 65 valid labels with or without an ifMetagenerationMatch condition | Permissive invalid body 400 before lookup/conditions; valid body 404; strict permission check precedes parsing |
| `bucket_put_rejects_get_only_generation_query_parameter` | generation and softDeleted=false queries on PUT versus GET | PUT 400 invalidParameter; GET retains generation 501 and softDeleted=false 200; state unchanged |

Every successful PUT, including an identical resource or empty replacement of
defaults, increments metageneration once and derives updated from the injected
clock without artificial advancement. Same-millisecond timestamps may repeat.
Authorization, conditions, replacement validation and write share PATCH's short
immediate SQLite transaction. No object enumeration, blob read or schema
migration is required. Authorization precedes conditions, which precede
catalog validation. Strict authentication is resolved before routing; query
syntax precedes bucket permission checks, and permission precedes body parsing.
Adapter unsupported/invalid input precedes conditions: a stale condition with
an invalid label returns HTTP 400, or with an unsupported field returns 501.
Direct catalog calls with stale conditions and invalid labels return 412.
These local error/revision choices are not established provider parity.

## Supported and rejected fields

Omitted or null labels clear the map; a supplied map replaces all keys.
Clearing omitted/null labels is a local replacement policy; the sources do
not establish the update-time omission/null result.
Values must be strings, including empty strings; per-key null is invalid as
on insert, rather than PATCH key deletion. Shared label rules allow at most
64 keys, each starting with a lowercase/international letter, and keys/values
of at most 63 Unicode characters containing lowercase/international letters,
numbers, underscore or hyphen. Omitted storageClass becomes STANDARD as a
local choice reusing the insert default. The resource/Discovery default applies
only when a bucket is created; neither establishes that default for update.
Supported classes are STANDARD, NEARLINE, COLDLINE, ARCHIVE, MULTI_REGIONAL
and DURABLE_REDUCED_AVAILABILITY; REGIONAL/RAPID return 501, invalid/null/empty
class 400. The implementation shares validation tables with insert/PATCH.

Omitted versioning, omitted versioning.enabled and omitted defaultEventBasedHold
become false under local policy, including versioning:{}; the sources do not
establish these update-time defaults. Legacy null versioning, enabled or
defaultEventBasedHold also become false; native rejects these nulls with 400.
These null choices follow existing insert/PATCH local policy, rather than
being derived from the PUT sources.
Disabling defaults never releases an existing event hold or deletes history.
Settings apply immediately to future publication, unlike possible provider
propagation delays. Class replacement never rewrites existing objects.

Identity, name/id/project/projectNumber/timeCreated, placement, objects,
history/holds and IAM policy bindings/etag are preserved. Keeping the IAM policy
etag independent of bucket metadata revisions is a local choice: the resource's
updated description includes metadata or IAM policy changes, but does not
establish independent revisions. Location may match
the rendered location ignoring ASCII case; changed location is 501. Native
compares stored placement in the transaction. Legacy accepts its rendered
US-CENTRAL1 only, never using it to rewrite stored placement. Legacy location:"US"
returns 501 even when US is stored.

Unsupported fields follow the shared PATCH list, which includes insert-only,
immutable or reserved fields absent from update's writable list. Their 501
rejection is a fail-closed local choice, not a provider error claim:
acl, autoclass, billing, cors,
customPlacementConfig, defaultObjectAcl, encryption, hierarchicalNamespace,
iamConfiguration, ipFilter, lifecycle, logging, objectRetention,
retentionPolicy, rpo, satisfiesPZI, satisfiesPZS, softDeletePolicy and website.
Only acl:[] and exactly lifecycle:{"rule":[]} are exceptions; defaultObjectAcl:[]
remains unsupported. Null/empty unsupported settings otherwise return 501.
The resource marks location, customPlacementConfig, hierarchicalNamespace and
name writable for insert; the proto marks location immutable; Discovery
reserves satisfiesPZI/PZS for future use. Unchanged-location acceptance and
ignoring name even when different from the path are also local choices.
Output-only id, kind, selfLink, projectNumber, generation, metageneration,
etag, timeCreated, updated, locationType, owner, softDeleteTime and hardDeleteTime
are ignored, as are name and unknown body/versioning children. None is persisted as
new supported behavior. Query predefinedAcl, predefinedDefaultObjectAcl and
userProject return 501; invalid projection and GET-only generation/softDeleted
return 400 invalidParameter, including softDeleted=false.
The existing bounded query parser and 64 KiB metadata body cap apply.

## Reference profile differences

The pinned reference router registers no bucket PUT. Observed memory and
filesystem runs each return HTTP 404 with the JSON Not Found envelope for all
32 PUT cases, including malformed bodies, existing buckets, unsupported queries
and conditions. PUT leaves GET/list state and object bytes unchanged. Separately,
object upload on the filesystem backend resets defaultEventBasedHold to false
and changes bucket timestamps; the memory backend preserves them. These are
reference observations, not Google contract evidence. Both local profiles
implement replacement of the declared subset, atomic conditions and strict
permissions with durable versioning/holds; unsupported settings return 501.
The [reference comparison](../evidence/bucket-metadata-put-comparison.md)
tracks the separately owned final-source execution evidence.

After replacement, legacy retains US-CENTRAL1/region, projectNumber zero and
newline framing while exposing stored labels/class, actual metageneration,
updated and etag consistently on PUT/GET/list and restart. Untouched
metageneration-1 native buckets reopened in legacy can hide class/labels;
their legacy GET is not a lossless replacement source. This existing rendering
boundary and case-sensitive native insert location remain outside this slice.
Same-profile full-resource round trips are covered explicitly.

## Verification boundary

The [final integration evidence](../evidence/bucket-metadata-put.md) records
the verified source, pinned inputs, commands, results and raw logs for Rust,
official Go SDK, reference comparison, containers and native/Docker guide checks.
HTTP cases are in tests/bucket_http.rs; clock/rollback cases in tests/catalog.rs;
transaction-time authorization cases in src/storage/catalog.rs.
No real-cloud differential run or complete-method compatibility claim belongs
to this slice.
