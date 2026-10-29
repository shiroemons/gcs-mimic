# Object retention (Object Retention Lock)

Scope: BEHAVIOR-OBJECT-034, the `enableObjectRetention` subset of buckets.insert
and the `retention` subset of objects insert/get/list/patch/update over the JSON
API. The row stays PARTIAL for the pending cases listed below.

## Acceptance contract

- OBJRET-BUCKET-ENABLE: `POST /storage/v1/b?enableObjectRetention=true` stores
  the feature and returns `objectRetention: {"mode":"Enabled"}` from insert,
  get and list. It cannot be enabled later or disabled. A request body may echo
  `objectRetention` only when it matches: `{"mode":"Enabled"}` on insert needs
  the query parameter, PATCH/PUT accept `null`, `{}` or the stored value, and any
  other value returns 400 invalidArgument without changing state.
- OBJRET-CONFIG: multipart and resumable uploads and PATCH/PUT set
  `retention {mode, retainUntilTime}`; get, list and the upload response read it
  back. `mode` is exactly `Unlocked` or `Locked`; `retainUntilTime` is RFC 3339
  and stored with microsecond precision. `null`, `{}` and all-null fields remove
  the configuration (Go sends `null`, Ruby sends `{}`). PUT without `retention`
  keeps it.
- OBJRET-TRANSITIONS: an Unlocked configuration can be extended freely; locking,
  shortening or removing it needs `overrideUnlockedRetention=true`. A Locked
  configuration only accepts an equal or later `retainUntilTime` in Locked mode,
  even with the override and after it expires.
- OBJRET-PROTECT: until `retainUntilTime`, deleting the object (with or without
  `generation`), soft deleting it and replacing it by upload return
  403 `retentionPolicyNotMet`. Metadata edits remain allowed. In a versioned
  bucket a retained live version may become noncurrent through overwrite or
  live delete, but the noncurrent generation cannot be deleted. The injected
  clock ends protection exactly at `retainUntilTime`.
- OBJRET-VALIDATION: buckets without the feature reject any configuration with
  400 invalidArgument; removing an absent configuration is a no-op. Setting
  retention on an object under an event-based hold, or together with
  `eventBasedHold:true`, fails. `retainUntilTime` may be at most 3,155,760,000
  seconds after the current time. Copy, rewrite and compose reject `retention`
  in their destination metadata with 400 badRequest and never inherit it.
- OBJRET-INTERACTIONS: holds and object retention protect independently; a hold
  is reported first. With a bucket retention policy both must be satisfied, and
  `retentionExpirationTime` reports the later deadline. Retained objects keep a
  bucket non-empty.
- OBJRET-AUTH: in the strict profile, bucket insert with the flag also needs
  `storage.buckets.enableObjectRetention`; insert/patch/update bodies with
  `retention` also need `storage.objects.setRetention`, and
  `overrideUnlockedRetention=true` with a retention change also needs
  `storage.objects.overrideUnlockedRetention`.
- OBJRET-PERSIST: schema 29 adds `buckets.object_retention_enabled` and
  `objects.retention_mode`/`retain_until_time`; existing buckets migrate
  disabled and existing objects without retention. Values survive restart and
  pending resumable sessions keep the requested configuration.
- OBJRET-SDK: the pinned Go `SetObjectRetention`/`Writer.Retention` and Ruby
  `create_bucket(enable_object_retention: true)`/`File#retention=` journeys in
  the SDK scorecard store retention and get 403 on delete.
- PENDING: gRPC input (CreateBucket `enable_object_retention`, WriteObject and
  UpdateObject `retention`) stays an explicit unsupported error; gRPC output
  reports `Bucket.object_retention` and `Object.retention`. Copy/rewrite/compose
  destination retention, XML headers and modes (EXT-XML-016/027), lifecycle
  interaction, initialization-file buckets, the project lien and provider-exact
  error reasons and messages remain pending.

## Sources and local choices

Checked 2026-10-29: the [Object Retention Lock overview](https://docs.cloud.google.com/storage/docs/object-lock),
[using Object Retention Lock](https://docs.cloud.google.com/storage/docs/using-object-lock),
the [objects resource](https://docs.cloud.google.com/storage/docs/json_api/v1/objects),
[objects.insert](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/insert),
[objects.patch](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/patch),
[objects.copy](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/copy)
and [buckets.insert](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/insert),
plus the pinned Discovery extracts in `compatibility/upstream` (revision
20261013). The guides establish the mode rules, the override parameter and
permission, the 100-year cap, the event-based-hold conflict, that a retained
live version can still become noncurrent, that both object and bucket retention
apply, and that copies do not inherit retention.

The sources do not specify the status or reason for invalid transitions, a
bucket without the feature or the event-hold conflict; locally they return
400 invalidArgument with corrective guidance. Retained deletes reuse the
documented 403 `retentionPolicyNotMet` reason with an object-retention message.
A `retainUntilTime` in the past is accepted. The override permission is checked
whenever the flag accompanies a retention change, before the catalog knows
whether the change needs it. SPEC-OBJECT-RETENTION records these choices.

## Verification

- Before implementation, both scorecard `object.retention` journeys failed with
  400 badRequest for unsupported `enableObjectRetention`.
- `cargo test --locked --offline --lib object_retention`: PASS, nine tests
  covering transition rules, JSON parsing, bucket enablement, the wire contract
  with the injected clock, strict permissions, resumable sessions, versioning,
  soft delete, holds, bucket policy and the schema 28-to-29 migration.
- `bash sdk-scorecard/run.sh --out "$TMPDIR/scorecard-retention" gcs-mimic`:
  `object.retention` passes for Go Storage v1.70.0 and Ruby Storage 1.62.1;
  every case that passed in `sdk-scorecard/results/2026-10-29` still passes.
  Results stay outside the repository.
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked
  -- -D warnings` and `cargo xtask compatibility check`: PASS.
- `bash scripts/test-rust.sh`: PASS, 46 test binaries and doctests, including
  573 library tests, with zero failures or ignored tests.

Existing tests that asserted the old unsupported responses for
`enableObjectRetention`, `objectRetention`, `retention` and
`overrideUnlockedRetention` now assert the implemented contract or use another
unsupported field; catalog version assertions move to schema 29.

No real-cloud operation, benchmark or performance measurement is performed.
