# Bucket metadata PUT

<!-- markdownlint-configure-file {"MD013": false} -->

Contract checked on **2026-10-27**. Scope: JSON-BUCKETS-013,
`PUT /storage/v1/b/{bucket}`, in native and legacy-emulator profiles.
PUT preserves omitted modeled settings, including labels, default storage class,
versioning, default event-based hold, CORS, PAP, UBLA, lifecycle, retention and
soft-delete policies. Explicit values retain their existing validation and effects.
This local compatibility choice supports Ruby Storage 1.62.1 update blocks;
it has not been verified against real GCS (SPEC-BUCKET-PUT).

## Sources and reconciliation

- [UPDATE](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/update),
  [partial updates](https://docs.cloud.google.com/storage/docs/json_api#patch),
  [Bucket resource](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets),
  [label rules](https://docs.cloud.google.com/storage/docs/tags-and-labels),
  [storage classes](https://docs.cloud.google.com/storage/docs/storage-classes).
- [Discovery](https://storage.googleapis.com/$discovery/rest?version=v1), historically
  fetched on 2026-10-23: revision `20261013`, SHA-256
  `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`.
- Supplementary [Storage v2 proto](https://github.com/googleapis/googleapis/blob/14a161a98ef045be80c12ab69d924504a756f109/google/storage/v2/storage.proto),
  SHA-256 `a0df6132967b97a89be248f1f24d277ee8df8f8d885db1d4af6249469ff0c1cf`,
  `UpdateBucketRequest.update_mask`: `*` is equivalent to JSON update and can
  reset newly introduced fields. This establishes replacement intent, not
  legacy JSON omitted-field defaults or null behavior.
- Comparison only: fake-gcs-server v1.56.1, commit
  `896ece51e49eed0946089f3089f6343855c0d106`,
  [router](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/server.go).
- Ruby Storage `1.62.1`, `lib/google/cloud/storage/bucket.rb`, SHA-256
  `53d1c79babadbfd457182ffb7280fa73ec9ff86952c5d0bc99f5f3da50ba3bd6`:
  `update`/`update_gapi!` construct a resource from changed attributes only and
  call `service.update_bucket`; direct setters call `patch_gapi!` instead.
  The gem version/checksum is pinned in `sdk-tests/ruby/Gemfile.lock`.

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
SPEC-BUCKET-PUT records omitted-field preservation,
null scalars, acl wording, no-effect revisions and error precedence as open
provider questions. SPEC-017 authentication wire questions also apply.
No provider bucket is contacted or authorized by this contract.

## Frozen behavior and executable acceptance cases

| Test ID | Case | Expected behavior |
| --- | --- | --- |
| `bucket_put_replaces_omitted_fields_and_preserves_resources` | Update a populated bucket; compare PATCH merge; GET/list/restart | Omitted modeled fields survive; supplied labels replace the map; identity, IAM, objects and bytes survive; one revision and changed etag |
| `bucket_put_empty_null_and_invalid_inputs` | Empty object/map, null map, versioning:{}, empty wire body, top-level null/array, invalid JSON/types, oversized body | Empty object preserves settings; null/empty labels clear; versioning:{} disables; empty wire body/null/array/invalid input 400 and body over 64 KiB 413, without mutation |
| `bucket_put_ruby_update_block_preserves_unrelated_settings` | Ruby update block changes only PAP on a bucket with versioning and labels; equivalent generated Go PUT | PAP changes while versioning and labels survive reload; no optional workaround arguments |
| `bucket_put_omitted_settings_survive_partial_iam_updates` | Empty PUT and PAP-only PUT on a bucket with all modeled settings, in both profiles; restart | Labels, class, versioning, holds, CORS, UBLA, lifecycle, retention and soft-delete policy survive; only explicit PAP changes |
| `bucket_put_label_boundaries_and_classes` | 64/65 keys; Unicode and 63/64 characters; classes | Shared insert/PATCH label and class validation; PUT labels replace the whole map; null label values invalid |
| `bucket_put_unsupported_and_output_inputs_preserve_state` | Unsupported fields, omitted acl versus acl:[], defaultObjectAcl, output-only/unknown fields, versioning:{"enabled":true,"future":1}, lifecycle | Unsupported 400 badRequest without mutation; omitted acl accepted as local policy; acl:[] ignored; lifecycle empty rules clear; unknown versioning children ignored while enabled applies |
| `bucket_put_conditions_and_query_contract` | Match/not-match/both; signed int64; malformed/duplicate; projection/ACL/billing queries; stale condition with invalid label/unsupported field; malformed query with denied access and invalid body | Atomic valid failures 412; HTTP invalid label/unsupported field precedes stale conditions with 400; query syntax precedes permission, which precedes body parsing; malformed/invalid projection 400; unsupported queries 400 badRequest; no 304 |
| `bucket_put_conditioned_race_has_one_winner` | Concurrent matching replacements | Exactly one 200, others 412 and one revision increment |
| `bucket_put_strict_permissions_and_policy_preservation` | Admin/viewer/other project/anonymous/invalid token; bucket policy | Require update with existing 401/403/404 ordering; policy bindings and etag unchanged |
| `bucket_put_rechecks_revocation_and_bucket_recreation` | Previously authorized grant revoked; bucket recreated under another owner; valid and invalid replacements | Transaction rechecks authority before stale conditions or invalid-label validation, 403 without replacement |
| `bucket_put_uses_clock_and_rolls_back_invalid_replacement` | Injected clock, invalid map/class with and without stale conditions, revision overflow | Direct catalog calls check stale conditions before label/class validation with 412; millisecond clock-derived updated; all failed replacements roll back |
| `bucket_put_settings_control_future_object_effects` | Enable, omit and explicitly disable versioning and default hold | Omission preserves future effects; explicit disabling preserves history and existing holds |
| `bucket_put_legacy_rendering_and_restart` | Profile rendering, restart and native carry-over | Fixed legacy location/project/framing retained; actual mutable revisions/settings exposed after PUT |
| `bucket_put_full_resource_round_trip_preserves_unchanged_fields` | GET body returned with label edit in each profile | Accept rendered unchanged location; preserve other fields except revision, updated and etag |
| `bucket_put_location_comparison_is_case_insensitive` | Same location in mixed case; relocation; legacy location:"US" | Accept unchanged rendered location ignoring ASCII case; relocation and legacy location:"US" return 400 badRequest without mutation |
| `bucket_put_null_settings_follow_profile_defaults` | Null scalar/boolean settings | Native null booleans 400; legacy null booleans false; null/empty storageClass 400 in both |
| `bucket_put_missing_bucket_validates_body_before_lookup` | Missing bucket and invalid/valid body, including 65 valid labels with or without an ifMetagenerationMatch condition | Permissive invalid body 400 before lookup/conditions; valid body 404; strict permission check precedes parsing |
| `bucket_put_rejects_get_only_generation_query_parameter` | generation and softDeleted=false queries on PUT versus GET | PUT 400 invalidParameter; GET retains generation 400 badRequest and softDeleted=false 200; state unchanged |

Every successful PUT, including an identical resource or empty update,
increments metageneration once and derives updated from the injected
clock without artificial advancement. Same-millisecond timestamps may repeat.
Authorization, conditions, replacement validation and write share PATCH's short
immediate SQLite transaction. No object enumeration, blob read or schema
migration is required. Authorization precedes conditions, which precede
catalog validation. Strict authentication is resolved before routing; query
syntax precedes bucket permission checks, and permission precedes body parsing.
Adapter unsupported/invalid input precedes conditions: a stale condition with
an invalid label or unsupported field returns HTTP 400.
Direct catalog calls with stale conditions and invalid labels return 412.
These local error/revision choices are not established provider parity.

## Supported and rejected fields

Omitted labels preserve the map; explicit null/empty labels clear it, and a
supplied map replaces all keys. Preservation on omission is a local choice
motivated by Ruby's update block, not an established provider guarantee.
Values must be strings, including empty strings; per-key null is invalid as
on insert, rather than PATCH key deletion. Shared label rules allow at most
64 keys, each starting with a lowercase/international letter, and keys/values
of at most 63 Unicode characters containing lowercase/international letters,
numbers, underscore or hyphen. Omitted storageClass preserves the current class.
The resource/Discovery STANDARD default applies only when a bucket is created.
Supported classes are STANDARD, NEARLINE, COLDLINE, ARCHIVE, MULTI_REGIONAL
and DURABLE_REDUCED_AVAILABILITY; REGIONAL/RAPID return 400 badRequest, invalid/null/empty
class 400. The implementation shares validation tables with insert/PATCH.

Omitted versioning and defaultEventBasedHold preserve current settings.
Explicit versioning:{} still disables versioning, as does omitted enabled inside
a supplied versioning object. Legacy null versioning, enabled or
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
the rendered location ignoring ASCII case; changed location is 400 badRequest. Native
compares stored placement in the transaction. Legacy accepts its rendered
US-CENTRAL1 only, never using it to rewrite stored placement. Legacy location:"US"
returns 400 badRequest even when US is stored.

Unsupported fields follow the shared PATCH list, which includes insert-only,
immutable or reserved fields absent from update's writable list. Their 400 badRequest
rejection is a fail-closed local choice, not a provider error claim:
acl, autoclass, billing,
customPlacementConfig, defaultObjectAcl, encryption, hierarchicalNamespace,
ipFilter, logging, objectRetention, rpo, satisfiesPZI, satisfiesPZS and website.
Only acl:[] is ignored; defaultObjectAcl:[] remains unsupported. Null/empty
unsupported settings otherwise retain their existing rejection.
CORS, PAP/UBLA, lifecycle, retention and soft delete use the existing PATCH
parsers and shared catalog transaction; omitted settings are preserved.
Explicit null/empty values retain each field's prior contract: null/empty CORS
and lifecycle clear configuration; null/empty retention clears an unlocked
policy; null/empty soft delete and UBLA are absent, while explicit zero/false
disables them. PAP requires inherited/enforced and retains its IAM checks.
See the respective SDK slice contracts for scoped effects and limitations;
lifecycle configuration is stored without executing actions.
The resource marks location, customPlacementConfig, hierarchicalNamespace and
name writable for insert; the proto marks location immutable; Discovery
reserves satisfiesPZI/PZS for future use. Unchanged-location acceptance and
ignoring name even when different from the path are also local choices.
Output-only id, kind, selfLink, projectNumber, generation, metageneration,
etag, timeCreated, updated, locationType, owner, softDeleteTime and hardDeleteTime
are ignored, as are name and unknown body/versioning children. None is persisted as
new supported behavior. Query predefinedAcl, predefinedDefaultObjectAcl and
userProject return 400 badRequest; invalid projection and GET-only generation/softDeleted
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
permissions with durable versioning/holds; unsupported settings return 400 badRequest.
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

The [historical integration evidence](../evidence/bucket-metadata-put.md) records
the original omission-reset policy's Rust/Go, reference, container and guide runs.
Those heavy runs do not establish the revised omission-preserving behavior.
HTTP cases are in tests/bucket_http.rs; clock/rollback cases in src/storage/catalog_tests.rs;
transaction-time authorization cases in src/storage/catalog.rs.
No real-cloud differential run or complete-method compatibility claim belongs
to this slice.

## SDK omission-preservation verification

On 2026-10-27, Ruby 4.0.9 / Storage 1.62.1 and Go 1.27.3 / Storage v1.70.0 /
generated API v0.309.0 reproduced lost versioning/labels before the fix.
`just check-sdk` over HTTP passes Go vet and the complete Go/Ruby journeys,
including the new update block in permissive and strict modes. The existing Go
V2 signer test skips HTTP because its URLs require HTTPS; this slice changes
no signing behavior. `BUNDLE_FROZEN=true bundle check` passes with the pinned Ruby.
`cargo xtask dependencies check` reports every input CURRENT.
`RUST_TEST_THREADS=8 just check` passes with 1,058 Rust tests, zero failures or
ignored tests, formatting, host/cross-target clippy, compatibility and shell checks.
This includes unchanged full-resource round trips, atomic conditions, permission
checks, future object effects and explicit null/empty validation. Empty PUT on a
locked retention policy succeeds without removing it; explicit null still fails.
Both generated feature matrices were rendered from the ledger; broad status and
fidelity remain PARTIAL / LOCAL_SIMULATION, with no matrix diff required.
The shared catalog transaction now preserves omitted values, without reading
objects or changing startup, schema, dependencies or configuration.
Containers, Docker guide walkthroughs, reference execution, benchmarks and real-cloud
tests were not run for this revision. Provider parity remains PENDING.

## Documentation and native guide verification

On 2026-10-27, `cargo build --locked --offline` passes. The unchanged shell block
under [Replace bucket labels and default storage class](../getting-started.md#replace-bucket-labels-and-default-storage-class)
was extracted with `awk` and executed once with `bash -eu` against an isolated
native debug binary in HTTP/permissive mode. Its prerequisite was a fresh
demo-bucket with EU location and the team:local label. The block completed in
under one second with exit 0: all three `jq -e` assertions returned true,
the stale condition returned 412 `conditionNotMet`, and `cmp` confirmed the
subsequent GET matched the preceding resource. PUT-GUIDE is PASS for this native
block only; Docker and restart verification remain historical.

PUT-REFERENCE is PENDING: PUT-ASSERT was changed for omission preservation and
the shared unsupported response, but the comparison has not been rerun.

`RUST_TEST_THREADS=8 just check` passes after these documentation/ledger changes:
1,058 Rust tests, zero failures or ignored tests, formatting, host/cross-target
clippy, ledger/generated-view and shell checks. The retention lock/permission/
restart and PUT omission/null/input regressions pass. No source, SDK test or
comparison script was changed, and the SDK suites were not rerun.
English/Japanese matrix regeneration and consistency checks pass with no
generated diff. Markdownlint passes; Japanese textlint retains one stock-boundary
false positive for the required feature ID JSON-BUCKETS-013.
