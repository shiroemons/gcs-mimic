# Bucket retention policies through official SDKs

Scope: JSON-BUCKETS-007, BEHAVIOR-OBJECT-036 and the retention metadata subset
of bucket insert/get/list/patch/update. Both main rows remain PARTIAL for provider
edge details and excluded interactions.

## Acceptance contract

- RETENTION-CONFIG: insert, PATCH and PUT set/change a period; PATCH/PUT null removes
  an unlocked policy. PUT omission preserves it. Return effectiveTime and isLocked.
  Accept empty defaults.
- RETENTION-RANGE: require integral seconds greater than zero and less than
  3,155,760,000; reject invalid input without changing state.
- RETENTION-METADATA: return the start of the period plus the current period as
  retentionExpirationTime, including objects predating the policy. The period
  starts at creation, or at the last release of an event-based hold; while that
  hold is held the field is omitted. Storage v2 `retention_expire_time`, legacy
  v1 `retention_expiration_time` and the 403 message report the same deadline.
- RETENTION-PROTECT: reject delete, upload overwrite, copy/rewrite/compose
  destination overwrite, move source and compose deleteSourceObjects with
  403 retentionPolicyNotMet. Check atomically in shared JSON/XML mutation paths.
- RETENTION-CLOCK: at expiry, those operations succeed using the injected clock.
- RETENTION-LOCK: require ifMetagenerationMatch; reject mismatch, absent policy,
  locked-policy removal and shortening; allow extension and preserve lock.
  Removal/shortening returns 400 badRequestException with a locked-policy cause
  and a valid extension example; IAM denial remains 403 forbidden.
  Locking a bucket without a policy returns 400 badRequest with setup guidance.
- RETENTION-HOLDS: either temporary/event-based hold or retention blocks mutation.
  Releasing a held event-based hold restarts the bucket retention period from
  the release: JSON PATCH `eventBasedHold:false`, a JSON PUT that omits the
  hold, Storage v2 `UpdateObject` and legacy v1 updates. Setting a hold,
  releasing one that was not held and temporary holds leave the period alone.
  Lifecycle Delete waits for the restarted deadline.
- RETENTION-AUTH: settings and locking require storage.buckets.update in
  strict mode.
- RETENTION-PERSIST: schema 25 adds bucket policy columns; policies survive restart.
  Schema 57 stores the release time of an event-based hold; rows from earlier
  schemas have none and keep counting from creation, even if already released.
- RETENTION-SDK: Go BucketAttrs/LockRetentionPolicy and Ruby retention_period=/
  lock_retention_policy! configure, lock and reject retained deletion locally.
- PENDING: initialization-file policies, XML policy APIs, object retention,
  and lifecycle and soft-delete interactions.

## Sources and local choices

Checked 2026-10-27: official [bucket resource](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets),
[lock method](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/lockRetentionPolicy)
and [Bucket Lock guide](https://docs.cloud.google.com/storage/docs/bucket-lock),
plus the [JSON errors](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes).
Discovery revision 20261020 has SHA256
`eeb0831df501d131c9497534b2c3d130ea40f9fa215195115d011cb64e1a42bc`.
The method requires storage.buckets.update. The resource establishes the period
range, and the guide establishes 403 retentionPolicyNotMet for retained objects.
For locked bucket policy removal/shortening, the guide and JSON error reference
specifically establish 400 badRequestException. Discovery's generic
PERMISSION_DENIED conflicts with those HTTP sources; the specific JSON sources
take precedence, recorded in SPEC-RETENTION-POLICY.
Null/empty normalization, exact message bytes and already-locked error details
need provider verification; SPEC-RETENTION-POLICY records those local choices.
Ruby's pinned generated client repeats ifMetagenerationMatch in its form body
on POST. Only an identical query/form value is accepted; non-form bodies and
different or additional parameters are rejected before mutation. This is a scoped
SDK transport accommodation, not a general form-body API.
`effectiveTime` is the time of the request that set the period; restating the
same period keeps it. Cloud Storage reported a time about 60 seconds after the
request in one observation (2026-10-31) but enforced the policy at once, so
gcs-mimic keeps the request time as a scoped difference
(SPEC-RESPONSE-WIRE-REMAINING).
No new configuration, dependency, polling or startup object scan is required.
The event-based hold rules follow the pinned googleapis storage v1 and v2
protos: retention duration "begins from the moment event-based hold
transitioned from true to false", and the expiration time "is not provided for
objects with an active event-based hold, since retention expiration is unknown
until the hold is removed". The JSON Objects resource page (fetched 2026-11-18)
states neither, and no recording covers them. A PUT that omits a held hold
restarting the period, and a restated `false` not restarting it, are inferences
recorded in SPEC-EVENT-HOLD-WIRE.

## Verification

- Before implementation, both ordinary SDK retention configuration journeys
  failed with 400 badRequest for unsupported retentionPolicy.
- `cargo check --all-targets --locked --offline`: PASS before tests.
- `cargo test --locked --offline --lib storage::resumable_http_tests::retention`:
  PASS, four wire tests covering configuration, range, lock, permissions,
  restart, all scoped mutation paths, XML PUT, injected-clock expiry and holds.
- `cargo test --locked --offline --lib storage::catalog_tests::retention_schema_migration_preserves_existing_buckets`:
  PASS, additive schema 24-to-25 migration preserves bucket metadata.
- Event-based hold release (2026-11-18):
  `bash scripts/with-test-tmpdir.sh cargo test --locked --lib -- storage::resumable_http_tests::retention storage::catalog::object_hold storage::catalog::lifecycle::tests::releasing_an_event_based_hold grpc::v2::update_copy::event_hold_release grpc::v1::tests::legacy_updates_releasing`:
  PASS, 13 tests. They cover the JSON PATCH and PUT releases, the omitted
  deadline while held, Storage v2 and legacy v1 updates, the schema 56-to-57
  migration and restart, and Lifecycle Delete waiting for the restarted
  deadline.
- `just check-sdk`: PASS, full Go vet/test and Ruby self-check, migration,
  permissive/strict journeys and signatures. Go Storage v1.70.0 / API v0.309.0
  with Go 1.27.3; Ruby Storage 1.62.1 with Ruby 4.0.9 / Bundler 4.0.23.
- `just check-containers`: PASS for linux/amd64 and linux/arm64, including build,
  Compose host/peer HTTP access, CRUD, persistence, reset, signals, read-only
  bucket initialization and seed journeys. These are existing container/guide
  smoke checks, not additional retention-specific container assertions.
  They preceded the locked-policy error refinement; that response-only change
  was rechecked with the full SDK and Rust suites, without another image build.
- Markdownlint: PASS for the seven changed handwritten documents.
- `cargo xtask dependencies check`: PASS; fixed compiler, direct libraries,
  SDK toolchains/modules/gems and container inputs report CURRENT. No dependency
  version, lockfile or configuration key changed.
- `RUST_TEST_THREADS=8 just check`: PASS, 1,034 Rust tests, zero failures or
  ignored tests; formatting, native/cross-target clippy, ledger/generated views
  and offline shell checks pass. SQL-work limits remain unchanged.

No real-cloud operation, benchmark or performance measurement is performed.
