# Lifecycle configuration through official SDKs

Scope: BEHAVIOR-LIFECYCLE-001 and the lifecycle subset of JSON-BUCKETS-005,
JSON-BUCKETS-008 and JSON-BUCKETS-013. Configuration is metadata only; actions
are never executed. The behavior row remains PARTIAL.

## Acceptance contract

- LIFECYCLE-INSERT-STORE: validate and store rules; insert/get/list return them.
- LIFECYCLE-PATCH-REPLACE: replace the entire rule list and advance
  metageneration once.
- LIFECYCLE-PATCH-CLEAR: null, an empty object, a null rule list or an empty
  rule list clears
  configuration; omit lifecycle from subsequent responses.
- LIFECYCLE-UPDATE-REPLACE: PUT replaces supplied lifecycle and clears omitted
  lifecycle.
- LIFECYCLE-VALIDATE-ACTION: accept Delete, SetStorageClass with a supported
  bucket storage class, and AbortIncompleteMultipartUpload; reject invalid
  actions with guidance.
- LIFECYCLE-VALIDATE-CONDITION: validate the eleven scoped condition fields,
  dates, nonnegative int32 values, booleans, string lists, nonempty conditions
  and official limits.
  AbortIncompleteMultipartUpload permits age, matchesPrefix and matchesSuffix
  only.
- LIFECYCLE-PERSIST-RESTART: preserve rules across restart and migrate schema
  23 to 24.
- LIFECYCLE-SDK-GO: ordinary Create/Update/Attrs round trips and empty Lifecycle
  clearing.
- LIFECYCLE-SDK-RUBY: create-block rules, lifecycle-block replacement and clearing.
- PENDING: action execution with an injected clock and maintenance scheduler, XML
  lifecycle, initialization-file lifecycle, matchesPattern and size conditions.

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
matchesPrefix, matchesSuffix, noncurrentTimeBefore and numNewerVersions.
Delete and AbortIncompleteMultipartUpload omit action.storageClass;
SetStorageClass requires it. Valid but unsupported classes, including REGIONAL
and RAPID for actions and RAPID for matchesStorageClass, use the common
unsupported response with the indexed field path. Invalid class names include
a supported example in the error. Errors return 400 invalidArgument in native mode
and 400 invalid in legacy-emulator mode, with valid examples. Recognized
unsupported conditions retain the common 400 badRequest response.

Schema 24 adds JSON rules to the bucket row in a short transactional migration.
List pagination charges lifecycle metadata against the existing bounded page
budget. No startup scan, object-body processing, background task or new setting
is added.
The initializer and XML lifecycle remain unsupported.

## Verification

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
