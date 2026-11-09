# Lifecycle action execution research

Status: research for a future slice. Nothing here is implemented, and the [feature ledger](../../../compatibility/features.json) still owns status. Features: `BEHAVIOR-LIFECYCLE-001` to `BEHAVIOR-LIFECYCLE-010`.

Checked on 2026-11-09 against commit `554c7b04`; line numbers refer to that commit. The research read code and official documents. It did not run the code, inspect SQL plans or contact Cloud Storage.

## Summary

- Execution fits as one more sweep in the existing maintenance pass. Delete reuses `retire_previous` and multipart abort reuses `retire_unheld`. Only storage class change needs a new in-place UPDATE, because rewrite creates a new generation.
- One migration is enough: `objects.time_storage_class_updated`. No new index is expected.
- Support `sizeAboveBytes` and `sizeBelowBytes`, which the current guide documents. Keep rejecting `matchesPattern`, which is Early Access.
- Large slice: about 900 to 1300 lines plus similar test volume.

## Google contract

Sources, with last-updated dates: [lifecycle](https://docs.cloud.google.com/storage/docs/lifecycle) (10-30), [lifecycle configurations](https://docs.cloud.google.com/storage/docs/lifecycle-configurations) (10-30), [metadata](https://docs.cloud.google.com/storage/docs/metadata) (10-30), [generations and preconditions](https://docs.cloud.google.com/storage/docs/generations-preconditions), [Pub/Sub notifications](https://docs.cloud.google.com/storage/docs/pubsub-notifications) (11-07), [soft delete](https://docs.cloud.google.com/storage/docs/soft-delete) (11-07), [object versioning](https://docs.cloud.google.com/storage/docs/object-versioning) (11-07), [object holds](https://docs.cloud.google.com/storage/docs/object-holds), [bucket lock](https://docs.cloud.google.com/storage/docs/bucket-lock), [object lock](https://docs.cloud.google.com/storage/docs/object-lock), [Autoclass](https://docs.cloud.google.com/storage/docs/autoclass), [Rapid buckets](https://docs.cloud.google.com/storage/docs/rapid/rapid-bucket), [Discovery](https://storage.googleapis.com/$discovery/rest?version=v1) revision 20261103, SHA-256 `30b498129041aceedca2e60678443e2671531cf0c08b3488c6934c212a2cb272`.

Rules:

- Conditions in one rule are ANDed. Multiple rules with the same action are ORed.
- When several actions match, only one runs. Delete beats SetStorageClass, then the SetStorageClass with the lowest storage price wins.
- A configuration change can take up to 24 hours to apply and execution is asynchronous with no timing guarantee. Objects whose upload has not completed are not subject to rules.

Actions:

- Delete soft-deletes by default. With versioning, a live object becomes noncurrent and a noncurrent one is deleted (soft-deleted when soft delete is on). A noncurrent version that also matches is deleted "after some time".
- Delete does not act during a hold or before retention expires, and it runs afterward if the conditions still match.
- SetStorageClass transitions: DRA to Nearline, Coldline, Archive, and Multi-Regional or Regional; Standard, Multi-Regional and Regional to Nearline, Coldline and Archive; Nearline to Coldline and Archive; Coldline to Archive. A transition outside the table is accepted but does not happen. A regional bucket cannot use Multi-Regional, and multi or dual-region buckets cannot use Regional.
- SetStorageClass does not rewrite, so generation is unchanged and `updated` changes (lifecycle and metadata pages), and `timeStorageClassUpdated` changes (Discovery). Metageneration +1 is inferred from "increases with each metadata update"; the lifecycle page does not state it.
- AbortIncompleteMultipartUpload applies only to XML multipart uploads, accepts only `age`, `matchesPrefix` and `matchesSuffix`, deletes parts, and measures age from the initiation time.

Condition evaluation:

| Condition | Rule |
| --- | --- |
| `age` | N > 0: now is at least creation + N days (10:00 becomes 10:00 ten days later in the documented example). N = 0: next UTC midnight after creation |
| `createdBefore` | creation is before 00:00Z of the date |
| `customTimeBefore` | Custom-Time date is before the date; false when unset |
| `daysSinceCustomTime` | now is at least Custom-Time + N days; false when unset |
| `daysSinceNoncurrentTime` | now is at least noncurrent time + N days; 0 means the moment it became noncurrent (Discovery) |
| `noncurrentTimeBefore` | noncurrent date is before the date; false for live objects |
| `isLive` | true for live, false for noncurrent; everything is live without versioning |
| `numNewerVersions` | newer versions, including live, are at least N; live has 0, newest noncurrent has 1 (0 with no live version) |
| `matchesPrefix`, `matchesSuffix` | any entry matches, case-sensitive, excluding the bucket name and leading `/` |
| `matchesStorageClass` | any entry matches |
| `sizeAboveBytes`, `sizeBelowBytes` | strict comparison of data bytes excluding metadata, limit 5 TiB |
| `matchesPattern` | absent from the guide; Discovery marks it Early Access and allowlist-only |

Interactions:

- Soft-deleted objects are outside lifecycle: rules neither change nor permanently delete them.
- Object retention prevents deletion until retain-until (object lock page).
- Autoclass forbids SetStorageClass and `matchesStorageClass`. `validate_bucket` in `src/bucket/autoclass.rs` already checks this.
- Zonal buckets do not support SetStorageClass.
- `OBJECT_DELETE` includes lifecycle deletion but not noncurrent transitions or aborted multipart uploads. `OBJECT_ARCHIVE` covers a live object becoming noncurrent under versioning.
- `OBJECT_METADATA_UPDATE` for SetStorageClass is inferred from "sent for metadata changes on existing objects"; no page states it directly.
- `x-goog-expiration` (XML only) is documented and stays out of this slice.

Unresolved by documents:

- Whether a retained live version can become noncurrent under versioning. The object lock page says both "can be made noncurrent" and "not deleted until expiry".
- Whether SetStorageClass runs while a hold or retention blocks a matching Delete.
- Whether `age: 0` for an object created exactly at UTC midnight waits for the next day, and whether multipart abort uses the same rule.
- Whether `numNewerVersions` counts soft-deleted versions.
- Whether soft deletion emits `OBJECT_DELETE`; the document says "permanently deleted".
- The JSON type of size conditions (Discovery says string/int64, the guide shows only values). Neither the pinned proto nor upstream master has a gRPC size field.
- The relative price order of the legacy DRA, Multi-Regional and Regional classes.

## Code touchpoints

- New modules: `src/bucket/lifecycle/evaluate.rs` for pure, clock-free evaluation and `src/storage/catalog/lifecycle.rs` for the sweep and application.
- Configuration, `src/bucket/lifecycle.rs`: add the size fields at lines 26-49 and to `CONDITIONS` at 194-206, remove size from the unsupported list at 255-262 (keep `matchesPattern`), and validate int64 and 5 TiB near 296-349 with `int64_setting` from `src/json/buckets.rs`. The check at line 267 already restricts abort conditions.
- Scheduler: register at `src/storage/catalog/resumable_maintenance.rs:288-307`, before "XML multipart" and soft-deleted, so one pass drains aborted parts and computes new hard-delete deadlines. The existing deletion journal reclaims blobs last.
- Delete: split the part of `delete_selected` after taking the immediate transaction (`src/storage/catalog/objects.rs:408-468`) into a shared in-transaction function, without `purge_expired` in the sweep. Live objects use `generation: None`; noncurrent objects use `Some`.
- `retire_previous` (`src/storage/catalog/objects/publication.rs:429-489`) handles notifications, the soft-delete branch and `pending_blob_deletions`. Use `BucketAccess::Permissive`.
- `delete_selected` applies `check_policy_and_holds` for noncurrent versions and `check_retention` for removal (lines 440-444). Lifecycle should always apply the full protection check, the conservative answer to the first unresolved question.
- Storage class: `update_object_metadata` (`objects.rs:195-303`) does not update the class, so add a new UPDATE and reuse `change_journal::object(.., ChangeKind::Metadata, ..)` (`change_journal.rs:184-202`) for notification and journal effects.
- `timeStorageClassUpdated`: `ObjectRecord` (`src/object.rs:72-109`), `object_columns!` (`objects.rs:45-54`), `read_object` (`objects.rs:703-754`), `src/json/objects.rs:866-869`, `src/grpc/v2/convert.rs:349` and `src/grpc/v1/convert.rs:159`, which all use `time_created` today. Restore (`soft_delete.rs:181-186`) sets it back to none.
- Multipart: reuse `retire` (line 5) and `retire_unheld` (line 262, skipping Busy) in `xml_multipart/maintenance.rs`. Add condition checks there and call `retire` only inside a transaction.
- Schema: `src/storage/catalog.rs:149`, `schema.rs:177` and `:354`, and the test downgrade support.
- Wakeup: `connection.rs:113`, the point every write passes through (`total_changes` delta), and state on `Inner` (`catalog.rs:171-235`).
- Rule changes originate from bucket insert in `buckets.rs`, `bucket_update.rs:395-398` and restore at `bucket_soft_delete.rs:104`. These are the only two `INSERT INTO buckets` sites.
- Automatically excluded: unfinalized appendable objects (separate table), resumable uploads, and soft-deleted buckets (rekeyed internally while the sweep starts from `buckets`).
- The memory backend shares the path; add one `open_memory_for_server` test with a manual clock. The notification outbox drops the oldest entries past 4096 events or 16 MiB (`notification_outbox.rs:123`).

## Design

Two-stage wakeup:

- Add an in-memory `LifecycleSchedule { armed, dirty, next, last_scan }` on `Inner`, starting dirty and never persisted.
- At `connection.rs:113`, when a write happens and the schedule is armed, notify `maintenance_wake` only on the rising edge of `dirty.swap(true)`. A deployment without rules pays one atomic read.
- At pass start, `!dirty && now < next` returns `next` without scanning. A dirty schedule within 1 second of the last scan (a local choice) returns `last_scan + 1s`. This avoids a full scan per request and any polling with no due work.
- Clear dirty before scanning. Writes during the scan, and the sweep's own changes, mark it dirty again, so a follow-up noncurrent delete reaches the next pass at most 1 second later.

Selection:

- `SELECT name, lifecycle, ... FROM buckets WHERE lifecycle != '[]' AND name > ?1 ORDER BY name LIMIT 16`.
- Objects per bucket in reverse primary-key order: `... WHERE bucket=?1 AND soft_delete_time IS NULL AND (name,generation)<(?2,?3) ORDER BY name DESC, generation DESC LIMIT 128`.
- Newest-first order lets a counter compute `numNewerVersions`, with the page boundary carried in the cursor. Only one page is in memory. Time conditions are checked in Rust rather than compiling up to 100 rules into SQL. Confirm with EXPLAIN at implementation time. Older generations added later by import are a known simplification.

Application and limits:

- Run one page per `run`, alternating with requests. Inside `sql::immediate`, re-read `(bucket,name,generation)`, the `lifecycle` column and the newer count, then re-evaluate before mutating, so the condition and mutation are atomic.
- Stop a pass after 1024 applied actions, including multipart aborts, and return `Some(now)` with dirty set.
- Next wakeup: each version's time conditions are already true, true at T, or false. A rule's due time is the maximum over its conditions, and Delete also takes the maximum with the retention expiry. Held objects have no time because releasing a hold is a write that marks dirty. The overall minimum becomes `next`, reported through `SweepOutcome::record`.
- Use `checked_*` arithmetic: int32-maximum days in microseconds overflow i64, which counts as not satisfied. Reuse `custom_time_key` (`src/object.rs:244`).
- Crash idempotence: row changes, outbox, journal and `pending_blob_deletions` commit together. Restart re-evaluates from dirty and converges. No new fsync path is added. Add `Step::LifecycleCommit` following `Step::ExpiryCommit`.
- Deterministic tests use a manual clock and the `cfg(test)` `maintain_resumable_sessions()`, asserting the returned deadline exactly. A forced pass used to apply configuration skips the 1-second gate.
- Add `POST /_internal/lifecycle/run` at `src/server/admin.rs:30-39` to run one pass and return counts for black-box tests and users. It offers no time travel.

Decision needed: stored Delete rules start acting right after an upgrade. The recommendation is to execute by default and state it in the release notes, with a disable flag only if needed.

## Acceptance cases

- Delete: `LC-DEL-001` unversioned; `002` soft-delete on; `003` live to noncurrent; `004` noncurrent by generation; `005` live, noncurrent then deleted across two passes; `006` restore updates creation time and restarts age.
- SetStorageClass: `LC-SC-001` Standard to Coldline with generation unchanged, metageneration +1, `updated`, `timeStorageClassUpdated` and etag changed; `002` every transition row; `003` an unlisted transition is unchanged; `004` new objects have `timeStorageClassUpdated` equal to `timeCreated`; `005` a NULL column reads as `timeCreated`; `006` JSON and gRPC v1/v2 output.
- Abort: `LC-ABORT-001` age, prefix and suffix with part reclamation; `002` a part write in progress is Busy and aborted on the next pass; `003` NoSuchUpload afterward; `004` no `OBJECT_DELETE`; `005` Delete rules do not affect multipart uploads.
- Conditions: `LC-COND-AGE-001` 1 microsecond around creation + N days; `002` age 0 at next UTC midnight; `003` int32-maximum age does not panic. `LC-COND-DATE-001` exactly 00:00Z (false) and just before; `LC-COND-CUSTOM-001` unset, future and past Custom-Time; `LC-COND-NC-001` zero days, live false. `LC-COND-NEWER-001` with and without a live version and N = 0; `LC-COND-LIVE-001` unversioned; `LC-COND-NAME-001` case and multiple values; `LC-COND-CLASS-001`. `LC-COND-SIZE-001` equal is false; `002` over 5 TiB is 400; `003` string and number; `004` gRPC cannot represent it; `LC-COND-PATTERN-001` stays 400.
- Precedence: `LC-PREC-001` Delete over SetStorageClass; `002` Nearline and Coldline choose Coldline; `003` same-action OR; `004` one false AND condition; `005` abort rules do not affect objects.
- Protection: `LC-PROT-001` holds block and release allows; `002` bucket retention blocks and sets `next`; `003` object retention; `004` a held Delete suppresses SetStorageClass; `005` soft-deleted objects are unchanged.
- Notifications: `LC-NOTIF-001` `OBJECT_DELETE`; `002` `OBJECT_ARCHIVE`; `003` `OBJECT_METADATA_UPDATE` with the new class; `004` abort sends none.
- Scheduling: `LC-SCHED-001` no rules, no scan; `002` exact future deadline with no scan before it; `003` PATCH marks dirty; `004` new put marks dirty; `005` early wake does not scan; `006` 1-second gate; `007` the cap returns `Some(now)`.
- Races: `LC-RACE-001` overwrite, rule removal or metadata change between scan and apply; `002` bucket deletion and recreation.
- Faults: `LC-FAULT-001` commit failure leaves state unchanged and the next pass succeeds; `002` crash and restart idempotence; `003` a broken rule does not stop other buckets; `004` pass failure sets `maintenance_error` and `RETRY_DELAY`; `005` restart recomputes `next`.
- Migration and backends: `LC-MIG-001` 52 to the new schema; `002` downgrade fixture; `LC-MEM-001` memory Delete and abort; `LC-ADMIN-001` route guard and methods.

## Ledger and gaps

- `BEHAVIOR-LIFECYCLE-002` to `009` keep target `EXACT`, because condition truth and state transitions are deterministic and documented. `010` carries delay and scheduling simulation as `LOCAL_SIMULATION`. Timing differences stay within "asynchronous, no guarantee".
- `001`: actual fidelity moves from `METADATA_ONLY` to `EXACT` and status stays `PARTIAL` (XML, initializer and provider details remain).
- `002`, `003`, `007`: `PARTIAL`, because unresolved provider questions remain and `IMPLEMENTED` allows no deviations.
- `004`, `005`, `006`, `008`, `009`: `IMPLEMENTED` once their cases pass (`009` JSON scope, gRPC recorded). `010`: `IMPLEMENTED` as `LOCAL_SIMULATION`.
- Also update the `BEHAVIOR-OBJECT-003` notes, size wording in `JSON-BUCKETS-005/008/013`, `sdk-lifecycle-config.md:33`, `roadmap.md:112`, `fake-gcs-server-migration.md:537` and the generated matrices.
- Reconcile first: `SPEC-LIFECYCLE-CONFIG` (Discovery update, size scope, action execution in `required_resolution`) and `SPEC-006` (`matchesPattern` remains Early Access).
- Resolve "lifecycle PENDING" in `SPEC-RETENTION-POLICY`, `SPEC-OBJECT-RETENTION` and `SPEC-SOFT-DELETE`. Releasing an event-based hold still does not restart retention, an existing gap Delete inherits.
- Add `SPEC-LIFECYCLE-EXECUTION` for local choices on the unresolved questions.

## Overlap and order

- Cloud KMS overlaps most: schema number, `object_columns!` and `row.get(N)` indexes in `read_object`, `publication.rs`, `src/json/objects.rs`, `src/grpc/v2/convert.rs`.
- Folders overlap moderately: `bucket_update.rs`, `src/json/buckets.rs`, object listing and schema. Empty-folder Delete depends on the folders table, and SetStorageClass refusal connects to zonal buckets.
- Managed folders overlap only on schema and ledger.
- Suggested order: schema and `timeStorageClassUpdated` with the pure evaluator; sweep, three actions, wakeup and fault tests; size conditions, admin route, SDK checks, ledger and docs. Fix schema numbering first if run beside Cloud KMS.
