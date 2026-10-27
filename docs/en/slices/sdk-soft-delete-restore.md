# Soft delete and object restoration

Scope: BEHAVIOR-BUCKET-015, BEHAVIOR-OBJECT-030, BEHAVIOR-OBJECT-031 and
JSON-OBJECTS-011. Bucket soft deletion and JSON-BUCKETS-010 remain NOT_STARTED.
All four scoped rows remain PARTIAL because the exclusions below are PENDING.

## Acceptance contract

- SOFT-DELETE-CONFIG: insert/PATCH/PUT accept integral retention seconds, 0 or
  604800 through 7776000; get/list return retentionDurationSeconds/effectiveTime.
  Invalid values return 400 without mutation; null/empty means unspecified.
- SOFT-DELETE-DEFAULT: omitted policy disables retention locally, preserving
  existing delete behavior and avoiding unexpected disk retention. Omission and
  retentionDurationSeconds=0 both allow softDeleted=true listing (200, empty
  unless retained generations remain); missing soft-deleted generations return
  404 notFound on metadata GET and restore.
- SOFT-DELETE-DELETE: shared JSON/XML delete preserves bytes and immutable
  softDeleteTime/hardDeleteTime after hold/retention checks; normal reads hide it.
- SOFT-DELETE-OVERWRITE: shared publication retains overwritten destinations
  for upload/copy/rewrite/compose/move; noncurrent deletion is retained.
- SOFT-DELETE-LIST-GET: softDeleted=true lists only retained generations using
  existing filters/cursors; versions=true conflicts. Metadata GET requires a
  generation and forbids alt=media. Normal generation reads hide retained data.
- SOFT-DELETE-RESTORE: POST restore creates a new live generation and resets
  metageneration to 1, with new timestamps and cleared deletion times; original
  remains. Conditions
  and create/delete/restore permissions are checked atomically against live
  state.
  copySourceAcl=true and restoreToken are unsupported; false is accepted.
- SOFT-DELETE-EXPIRY: injected-clock deadlines hide expired generations; delete,
  soft-deleted list and restore reclaim at most 100 expired rows per bucket call.
  No polling, timer or startup scan is added.
- SOFT-DELETE-BUCKET-DELETE: a bucket containing only soft-deleted generations can
  be deleted permanently together with those generations.
- SOFT-DELETE-AUTH: strict bucket update, object list/get and restore/create plus
  conditional overwrite delete use existing transaction authorization.
- SOFT-DELETE-PERSIST: additive catalog schema 26 preserves policy and generations.
- SOFT-DELETE-SDK: bounded Go and Ruby default high-level configure/delete/list/
  metadata/restore/read journeys.
- PENDING: bucket restore/soft deletion, initialization-file softDeletePolicy,
  HNS tokens, ACL copying, Autoclass, lifecycle execution, provider propagation,
  exhaustive error precedence and real-cloud differential evidence.

## Sources and local choices

Checked 2026-10-27: official [soft delete overview](https://docs.cloud.google.com/storage/docs/soft-delete),
[bucket resource](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets),
[object list](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/list),
[object get](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/get),
[restore](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/restore),
and [JSON errors](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes).
Discovery revision 20261020 SHA256
`eeb0831df501d131c9497534b2c3d130ea40f9fa215195115d011cb64e1a42bc`.
The bucket reference says less than 90 days while the overview says 7–90 days;
the local inclusive upper boundary follows the requested range and overview.
effectiveTime changes on increases; policy changes do not change existing object
deadlines. Null/empty behavior lacks an explicit provider guarantee and is a
local normalization. Unlike GCS's seven-day default, local new buckets disable
soft delete. Deleting a bucket does not simulate bucket soft deletion.
The official list and restore references require a soft delete policy and
document 400 errors when it is absent. Locally omission and explicit zero are
equivalent: listing and generation lookup depend on retained objects, not on
whether a policy is stored. This is a deliberate local deviation.
These differences and immediate policy application are recorded in
SPEC-SOFT-DELETE. Existing hold and retention protections remain enforced.

The shared retirement helper applies policy to every destination publication and
explicit generation deletion. Object Versioning still makes a replaced live
generation noncurrent; deleting that selected noncurrent generation soft-deletes
it. Restore checks the existing live object's four conditions in its publication
transaction. The source remains retained until its original hardDeleteTime;
restoration after policy disabling still works until the source expires.
The source and overwritten destination must meet the existing retention rules.

Restore allocates an opaque blob ID and links immutable source bytes. A durable
pending-deletion entry precedes linking; publication, generation assignment and
removal of that entry commit atomically. Existing recovery handles uncommitted
links, and cleanup rechecks catalog references. There is no body-sized buffer or
copy, and no new startup scan or maintenance task. Expiry uses indexed,
bucket-scoped batches of at most 100 rows; filters hide every expired row even
when an earlier batch leaves physical cleanup for another call.

Ruby's pinned generated client repeats query parameters in a form POST body.
The adapter accepts an empty body or a form map exactly equal to the validated
URL query; mismatched/additional parameters and non-form bodies return 400 before
mutation. This reuses the retention-lock accommodation and is a local transport
choice, not a provider body contract. XML DELETE now joins the same catalog path
for bearer/permissive calls, optional generation and existing generation-match
headers; signed DELETE and other XML options remain unsupported.

## Verification

Initial implementation: `a5807b8`; verified 2026-10-27 with Rust 1.99.1 and unchanged
dependencies/lockfiles. `cargo xtask dependencies check`: PASS, every input CURRENT.

- Both default SDK configuration journeys failed with unsupported-policy 400
  before implementation.
- `cargo check --workspace --all-targets --locked --offline`: PASS before tests.
- `cargo test --locked --offline --lib soft_delete`: PASS, seven cases covering
  configuration/defaults/permissions (omitted and zero policy both list with
  200 and return 404 on get/restore of missing generations, without retaining
  newly deleted objects), all scoped overwrite paths, selected
  noncurrent deletion, filters/pages, expiry/holds/retention, bucket deletion,
  restore conditions/collisions/rollback/restart, concurrent create-if-absent,
  schema 25-to-26 migration and the 100-row cleanup bound with rollback.
- Existing catalog suite: 24 PASS; JSON object suite: 14 PASS; object-list SQL
  tests: two PASS. Unsupported-route and XML DELETE permission/condition/body
  tests also PASS. Bucket integration tests verify omitted/null/empty defaults,
  malformed-policy rejection and unchanged state in both compatibility profiles.
- `cargo test --locked --offline --test object_list_http
  object_list_invalid_unsupported_and_missing_bucket_errors`: PASS. The default
  bucket allows an empty softDeleted listing; malformed/unsupported queries,
  versions=true conflicts, request bodies and missing-bucket errors remain checked.
- `BUNDLE_FROZEN=true bundle check`: PASS with the pinned Ruby selected.
- `just check-sdk`: PASS once on final source over HTTP, including Go vet/test,
  Ruby origin self-check/migration and permissive/strict journeys. Go 1.27.3 /
  Storage v1.70.0 / API v0.309.0 and Ruby 4.0.9 / Bundler 4.0.23 /
  Storage 1.62.1 /
  generated Storage client 0.70.0. TestObjectSoftDeleteRestore has a 30-second
  context; ruby-soft-delete-restore uses ordinary high-level calls. The existing
  Go V2 signed-transfer case skips HTTP because its URLs require HTTPS.
- Final `RUST_TEST_THREADS=8 just check`: PASS, including formatting, native and
  cross-target clippy, the complete Rust suite, 591-row compatibility validation
  and offline shell/container-publication contract checks.
- Markdown lint: four changed handwritten documents PASS with zero issues.

No real-cloud request, benchmark, performance measurement, HTTPS suite, container
build or full-guide run was performed; these are outside this slice.
