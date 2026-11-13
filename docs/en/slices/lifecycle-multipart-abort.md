# Lifecycle multipart abort execution

The feature ledger owns status for `BEHAVIOR-LIFECYCLE-004` and the multipart-abort acceptance cases of `BEHAVIOR-LIFECYCLE-001` and `BEHAVIOR-LIFECYCLE-008`. Stored `AbortIncompleteMultipartUpload` rules now execute for XML multipart uploads in the shared lifecycle pass. `BEHAVIOR-LIFECYCLE-004` and `008` target and achieve `LOCAL_SIMULATION` and remain `PARTIAL`: provider propagation, execution timing and the age-zero midnight interpretation have not been checked against Google Cloud.

## Contract and sources

Checked on 2026-11-11 against the official [lifecycle action](https://docs.cloud.google.com/storage/docs/lifecycle#abort-incomplete-multipart-uploads), [age condition](https://docs.cloud.google.com/storage/docs/lifecycle#age), [name conditions](https://docs.cloud.google.com/storage/docs/lifecycle#matchesprefix-suffix) and [notification event types](https://docs.cloud.google.com/storage/docs/pubsub-notifications#event_types). Abort applies to XML multipart uploads, removes associated parts, permits only `age`, `matchesPrefix` and `matchesSuffix`, and measures age from initiation. Conditions within a rule combine with AND; alternative rules and entries within a name list combine with OR. Name comparisons are case-sensitive. Aborting an upload emits no object notification.

The existing [research contract](lifecycle-execution-research.md) records Discovery revision 20261103, SHA-256 `30b498129041aceedca2e60678443e2671531cf0c08b3488c6934c212a2cb272`, and the pinned Storage v2 protobuf revision and digest. The discovery baseline and existing rule validation are unchanged. This slice extends the local execution evidence referenced by `SPEC-LIFECYCLE-CONFIG` and `SPEC-LIFECYCLE-EXECUTION` without resolving their independent provider questions.

The guide explicitly uses initiation for multipart age, but its age-zero sentence refers to an object. It does not settle multipart initiation exactly at midnight. This implementation reuses the object evaluator's day helpers and chooses the next UTC midnight strictly after initiation, including waiting a full day for initiation exactly at midnight. This is an unverified local decision. Positive ages use initiation plus whole 24-hour days. Overflow uses the same checked arithmetic as object evaluation and yields no match or lifecycle deadline. Upload evaluation accepts only age and name conditions; an exhaustive condition pattern also rejects object-only conditions independently of stored-rule validation.

## Execution and recovery

The lifecycle pass visits upload pages before object actions in each selected bucket and before the existing multipart expiry/reclamation sweep. Each page rechecks the bucket incarnation and current validated rules in its immediate transaction. A bucket without abort rules performs no upload scan. The upload query holds at most 128 `(name, id, created)` rows and seeks the existing `xml_multipart_uploads_list` index. Whole-upload leases stay held through the page commit, so part writes and completion cannot observe reclaimed parts.

Rule-bearing buckets currently use separate validation jobs and rule caches for upload and object selection. This adds one initial rule decode for the upload sweep even when the bucket has no abort rule; it does not scan uploads in that case.

Each page retires up to 128 uploads in one transaction and holds their whole-upload leases until commit. Uploads with many parts can therefore delay other catalog requests for that page. An abort error rolls back its page and stops object actions, including Delete and SetStorageClass, for the same bucket in that pass; other buckets continue and earlier committed pages remain applied. Individual corrupt upload rows are not quarantined. This is a known limit; ordinary startup validates upload rows before serving requests.

An upload held by a part or completion lease increments `skipped` and remains available. A single in-memory flag records that some upload was skipped. Client multipart lease release consumes that flag and marks the lifecycle schedule dirty; the ordinary one-second write-coalescing gate then allows another pass. Maintenance leases never rearm lifecycle. A release without a skipped abort leaves the schedule clean. With all matching uploads Busy, lifecycle returns no immediate deadline; ordinary multipart expiry still supplies its seven-day deadline. Repeated maintenance passes before a write, release or deadline perform no lifecycle rescan. The flag is catalog-wide: an unrelated multipart lease release can consume it and cause one extra scan, but a later scan seeing the still-Busy upload rearms it.

Abort shares the strict 1024-action pass budget with Delete, versioned archive and SetStorageClass. The admin outcome includes `aborted` and `transitioned`; `scanned` includes visited uploads and object versions. Reaching the cap returns the current injected time. Existing uploads' future match instants participate in the lifecycle deadline. Successful mutations can also return the existing one-second coalescing deadline for one follow-up scan.

Retirement calls the existing multipart `retire` path: it journals every part blob and deletes the upload with cascading part removal in the same SQLite transaction. The following multipart sweep reclaims journaled disk staging files or memory blobs. Upload-part, list-parts, completion and abort subsequently return XML 404 `NoSuchUpload`. Abort does not publish an object, touch existing object versions or resumable sessions, or write object notifications or synchronization events. Delete and SetStorageClass rules do not select uploads.

No schema, blob write, rename, unlink or fsync operation is added. The existing disk deletion-journal path unlinks staging files, synchronizes their directory, then forgets the journal rows; failures remain retryable. Existing memory reclamation checks references before removing the blob. Tests verify bytes disappear on both backends, commit rollback preserves parts, and process-crash/power-loss snapshots at the second page recover without resurrecting uploads or leaking parts. Injected reclaim, directory-sync and journal-forget failures retain the deletion row until retry.

`lifecycle_execution=disabled` disables abort along with other lifecycle actions. A configured synchronization directory defaults execution to disabled unless explicitly enabled. Enabling abort has no host-file effect because incomplete multipart parts were never synchronized objects. The ordinary seven-day multipart expiry remains active independently of lifecycle configuration. Both memory and disk follow these existing controls; synchronization configuration and explicit enable are exercised with the memory catalog, while disk cleanup and restart use the disk catalog.

## Acceptance evidence

All execution cases use the real catalog lifecycle pass and manual clock. XML missing-upload responses use the actual multipart adapter with the same catalog; this is adapter-level wire verification, without opening a listener.

| Behavior | Passing test |
| --- | --- |
| Initiation-age deadline, the preceding microsecond, exact next deadline | `multipart_abort_age_uses_initiation_and_exact_shared_deadlines` |
| Age zero before/at the next midnight, including midnight initiation | `multipart_abort_zero_age_waits_for_next_midnight_including_midnight_initiation` |
| Shared day-overflow behavior | `multipart_abort_overflow_never_matches_or_schedules` |
| Prefix/suffix AND, list/rule OR, case sensitivity, object and resumable exclusion | `multipart_abort_name_conditions_and_multiple_rules_preserve_other_resources` |
| Name-only prefix or suffix rules match without age | `multipart_abort_name_only_rules_match_without_an_age_condition` |
| Busy part/completion leases, release-driven wake and no polling | `multipart_abort_busy_leases_rearm_on_release_without_polling` |
| No extra dirty write for a lease release without a skipped abort | `multipart_lease_release_without_a_skipped_abort_does_not_dirty_the_schedule` |
| Whole-upload release alone rearms a skipped abort; unrelated release after successful abort stays clean | `multipart_abort_whole_lease_release_alone_rearms_the_next_pass`, `multipart_abort_success_does_not_rearm_on_an_unrelated_lease_release` |
| Object-only conditions never match even without rule validation | `multipart_abort_evaluation_rejects_object_only_conditions_without_validation` |
| Completion wins its lease or the aborted ID fails before completion | `multipart_abort_completion_lease_wins_or_missing_id_prevents_completion` |
| Disk/memory part reclamation, no notification, all four XML missing-ID operations | `multipart_abort_reclaims_parts_without_notifications_on_disk_and_memory` |
| Disabled execution, synchronization default, explicit enable and admin serialization | `multipart_abort_disabled_and_sync_defaults_preserve_uploads_until_enabled` |
| Multiple same-name pages and shared action cap with object Delete | `multipart_abort_keyset_pages_share_the_action_budget_with_object_deletion` |
| Abort, transition, deletion and archive in one capped pass; continuation preserves the applied class and archived versions | `multipart_aborts_transitions_deletes_and_archives_share_one_strict_action_limit` |
| Indexed query plan with no full scan or temporary sort | `multipart_abort_scan_seeks_the_existing_list_index_without_sorting` |
| No upload scan for other actions, rule quarantine and API repair | `multipart_abort_does_not_scan_uploads_for_other_actions_or_quarantined_rules` |
| Bucket deletion/recreation between passes excludes the old incarnation | `multipart_abort_rechecks_bucket_incarnation_after_recreation` |
| Failed page commit preserves upload, parts and journal state | `multipart_abort_commit_failure_rolls_back_and_retries_without_losing_parts` |
| Restart midway through multiple pages; process-crash and power-loss snapshots | `multipart_abort_restart_mid_pass_and_power_loss_recover_the_deletion_journal` |
| Reclaim, directory-sync and journal-forget failures retry | `multipart_abort_reclamation_failures_leave_a_retryable_journal` |

The tested query plan is:

```text
SEARCH xml_multipart_uploads USING INDEX xml_multipart_uploads_list (bucket=? AND (name,id)>(?,?))
```

Verified on 2026-11-11:

| Command | Result |
| --- | --- |
| `cargo check --locked --offline --tests` | Passed |
| `cargo test --locked --offline --lib storage::catalog::lifecycle::abort -- --nocapture` | 21 passed, 0 failed |
| `cargo test --locked --offline --lib lifecycle --quiet -- --test-threads=4` | 195 passed, 0 failed |
| `cargo test --locked --offline --test it_admin lifecycle -- --nocapture` | 2 passed, 0 failed |
| `cargo xtask lint` | 0 new production violations, 0 ratcheted findings |
| `cargo fmt --all --check` | Passed |
| `cargo test --locked --offline --no-fail-fast -- --test-threads=4` | 3124 passed, 0 failed; 1 existing ignored local Pub/Sub opt-in test |
| `cargo xtask compatibility render` | 607 feature rows validated; regenerated English and Japanese matrices |
| `cargo xtask compatibility check` | 607 feature rows validated; matrices and test IDs consistent |
| `just check-markdown` | 384 Markdown files and 2 generated views; 0 issues |

SDK harnesses, container builds, guide walkthroughs, resource measurements and real-provider calls were not run for this slice. No new dependency is needed; dependency pins and final integrated SDK/container verification belong to the concurrent integration work. The next lifecycle scope is provider evidence for the stated midnight and timing questions, or the remaining action interactions recorded in the ledger.
