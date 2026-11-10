# Lifecycle configuration through official SDKs

Scope: BEHAVIOR-LIFECYCLE-001 and the lifecycle subset of JSON-BUCKETS-005,
JSON-BUCKETS-008 and JSON-BUCKETS-013. Stored Delete rules now execute under the
[lifecycle execution contract](lifecycle-execution.md); other actions remain
metadata only. The evaluator also covers BEHAVIOR-LIFECYCLE-005 through
BEHAVIOR-LIFECYCLE-009. These behavior rows remain PARTIAL until their remaining
cases pass.

## Acceptance contract

- LIFECYCLE-INSERT-STORE: validate and store rules; insert/get/list return them.
- LIFECYCLE-PATCH-REPLACE: replace the entire rule list and advance
  metageneration once.
- LIFECYCLE-PATCH-CLEAR: null, an empty object, a null rule list or an empty
  rule list clears
  configuration; omit lifecycle from subsequent responses.
- LIFECYCLE-UPDATE-REPLACE: PUT replaces supplied lifecycle; an omitted
  lifecycle is cleared in the native profile (as recorded from Cloud Storage)
  and preserved in the legacy-emulator profile, a local choice under
  SPEC-BUCKET-PUT.
- LIFECYCLE-VALIDATE-ACTION: accept Delete, SetStorageClass with a supported
  bucket storage class, and AbortIncompleteMultipartUpload; reject invalid
  actions with guidance.
- LIFECYCLE-VALIDATE-CONDITION: validate the thirteen scoped condition fields,
  dates, nonnegative int32 values, booleans, string lists, nonempty conditions
  and official limits.
  AbortIncompleteMultipartUpload permits age, matchesPrefix and matchesSuffix
  only.
- LIFECYCLE-PERSIST-RESTART: preserve rules across restart and migrate schema
  23 to 24.
- LIFECYCLE-SDK-GO: ordinary Create/Update/Attrs round trips and empty Lifecycle
  clearing.
- LIFECYCLE-SDK-RUBY: create-block rules, lifecycle-block replacement and clearing.
- PENDING: storage class changes and multipart abort execution, XML
  lifecycle, initialization-file lifecycle and matchesPattern.

## Sources and choices

Contract checked on 2026-10-27 against the official
[lifecycle guide](https://docs.cloud.google.com/storage/docs/lifecycle),
[JSON patch semantics](https://docs.cloud.google.com/storage/docs/json_api#patch),
[bucket resource](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets),
[limits](https://docs.cloud.google.com/storage/quotas), the
[100-rule limit](https://docs.cloud.google.com/storage/docs/xml-api/put-bucket-metadata#lifecycle)
and live Storage v1 Discovery.
Discovery revision 20261020 has SHA256
`eeb0831df501d131c9497534b2c3d130ea40f9fa215195115d011cb64e1a42bc`.
The JSON overview explicitly allows deleting lifecycle configuration with null.
Empty configurations and null rule lists normalize to absence. Provider edge cases
remain recorded in SPEC-LIFECYCLE-CONFIG. Legacy matchesStorageClass names must
agree with the bucket's location type. Storage class transitions themselves are
not checked, matching the guide: invalid transitions are accepted but never run.
Unknown resource/rule/action fields follow existing ignored-input behavior;
unknown condition names are invalid, as specified by the lifecycle guide.
Recognized out-of-scope conditions use the shared unsupported response and are
never silently dropped.
Ruby 1.62.1 serializes unused condition fields and action.storageClass as null,
and unused string lists as empty arrays. These defaults normalize to absence
before validation; a condition with no remaining fields is still invalid. This
local normalization requires provider verification under SPEC-LIFECYCLE-CONFIG.

The supported condition fields are age, createdBefore, customTimeBefore,
daysSinceCustomTime, daysSinceNoncurrentTime, isLive, matchesStorageClass,
matchesPrefix, matchesSuffix, noncurrentTimeBefore, numNewerVersions,
sizeAboveBytes and sizeBelowBytes.
Delete and AbortIncompleteMultipartUpload omit action.storageClass;
SetStorageClass requires it. Valid but unsupported classes, including REGIONAL
and RAPID for actions and RAPID for matchesStorageClass, use the common
unsupported response with the indexed field path. Invalid class names include
a supported example in the error. Errors return 400 invalidArgument in native mode
and 400 invalid in legacy-emulator mode, with valid examples. Recognized
unsupported conditions retain the common 400 badRequest response.

Schema 24 adds JSON rules to the bucket row in a short transactional migration.
List pagination charges lifecycle metadata against the existing bounded page
budget. The configuration slice adds no object-body processing; the subsequent
[execution slice](lifecycle-execution.md) adds scheduling and its disable setting.
The initializer and XML lifecycle remain unsupported.

## Size conditions and evaluator extension

Checked on 2026-11-10 against the current lifecycle guide and Discovery revision
20261103 (SHA-256
`30b498129041aceedca2e60678443e2671531cf0c08b3488c6934c212a2cb272`).
Size thresholds accept JSON integers or int64 decimal strings from 0 through
5 TiB. Stored values and JSON responses use canonical decimal strings, including
when input is a number or uses a plus sign or leading zeroes. Equality does not
satisfy either size condition. Invalid values preserve the existing configuration.

The pinned Storage v2 protobuf cannot represent size conditions. GetBucket and
ListBuckets return UNIMPLEMENTED when their read mask requests lifecycle, including
the default and wildcard masks; a mask excluding lifecycle succeeds. UpdateBucket
and LockBucketRetentionPolicy check response representability before mutation.
An update replacing or clearing lifecycle may remove the unsupported conditions.
The retention lock's mandatory metageneration match protects its precheck from
concurrent changes. Updates without a metageneration match can still race a JSON
write between the precheck and transaction; that check is not yet part of the
catalog transaction. No concurrency guarantee is claimed for this case.

The evaluator parses action and class types at the configuration boundary and
evaluates each rule once without per-version heap allocation. It selects Delete
before the cheapest supported storage-class transition and reports only deadlines
that could change that selection. Deadlines outside the catalog's signed int64
microsecond range never match or schedule. Calendar dates are parsed on the stack
when a rule is evaluated; caching their midnight is not part of this change.
Age zero follows the guide over the conflicting protobuf description; see the
[source conflict](lifecycle-execution-research.md#google-contract).
Location restrictions on legacy transitions belong to the sweep, which has the
bucket location. REGIONAL and RAPID action classes remain unsupported.

Until the sweep applies class transitions, its `evaluate_delete_only` entry point
never selects SetStorageClass. It also defers every Delete rule containing
matchesStorageClass and excludes its deadline, even if the bucket has never had
a transition rule. Current rules cannot reveal earlier unapplied transitions:
replacing an age-30 transition with an age-60 class-dependent Delete must not
delete an object merely because the transition was skipped. This temporary
restriction deliberately defers more deletes than necessary. Deletes without
class conditions remain enabled.
The full `evaluate` interface retains the complete selection contract.

## Verification

Verified for the evaluator extension on 2026-11-10:

- `cargo check --all-targets` passed. The evaluator still has unused-code warnings
  in this isolated change until the sweep calls it; no warnings were suppressed.
- `cargo test --lib lifecycle` passed 62 cases, including gRPC mask handling and
  mutation rejection before state changes.
- `cargo test --lib bucket::lifecycle` passed 31 cases after adding the delete-only
  entry point, including its three conservative-selection cases.
- `cargo test` passed 2,994 unit, integration and documentation cases; one existing
  test remained ignored and no cases failed.
- `cargo test --test it_json bucket_lifecycle_rules_round_trip_replace_clear_and_validation`
  and `cargo test --test it_json bucket_core_settings_round_trip` passed one case
  each in the native and legacy-emulator profiles.
- `cargo xtask lint` passed with zero new production violations or ratcheted
  findings; the existing reasoned suppression count stayed at one.
- `cargo xtask dependencies check --scope release` reported the checked release
  inputs CURRENT. The SDK and development inputs were outside that scope.

No SDK harness, container build, guide walk-through, real-cloud differential or
resource benchmark was run for this extension. The earlier SDK results below
cover the original configuration subset, not the new size fields.

Go Storage v1.70.0 / API v0.309.0 and Ruby google-cloud-storage 1.62.1 use
ordinary high-level calls, without lifecycle workarounds. Both SDK cases failed
on nonempty lifecycle creation before implementation.

Verified on 2026-10-27:

- `cargo check --workspace --all-targets --locked --offline` passed.
- Lifecycle wire cases pass in native and legacy-emulator profiles: round trips,
  PATCH replacement/clearing, PUT replacement/omission, metageneration conditions,
  invalid action/type/date/combination cases and 100/101-rule boundaries.
- Catalog schema 23 migration and restart preservation pass. Calendar leap-year
  validation and the 1000/1001 prefix/suffix boundary pass.
- `just check-sdk` passed the full HTTP Go suite, Ruby migration/default journey
  and strict journey, including TestBucketLifecycleRules and
  ruby-bucket-lifecycle-rules in both Ruby profiles.
- `cargo xtask dependencies check` reported all 39 inputs CURRENT; Rust 1.99.1,
  edition 2024. No dependency was added.
- `just check` passed formatting, clippy, cross-target checks, all workspace
  unit/wire/doc tests, generated-ledger verification and offline shell checks.
  Schema 22/23 migration, older catalog migrations and the timestamp fixture
  regression all pass in the complete suite.

Markdown lint reported zero issues in the changed user documentation. HTTPS SDK,
container/guide journeys, cloud differential checks and benchmarks are excluded
from this slice's verification scope.
