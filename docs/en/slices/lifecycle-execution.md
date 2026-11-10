# Lifecycle Delete execution

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: `BEHAVIOR-LIFECYCLE-002`, `BEHAVIOR-LIFECYCLE-010` and the lifecycle-time subset of `BEHAVIOR-OBJECT-003`. The feature ledger owns status. This slice executes stored `Delete` rules through the shared pure evaluator, with deterministic local scheduling. Storage class transitions and incomplete multipart upload aborts remain separate slices.

End-to-end Delete tests also establish the executed subset of `BEHAVIOR-LIFECYCLE-001` and `BEHAVIOR-LIFECYCLE-005` through `BEHAVIOR-LIFECYCLE-009`. These rows remain PARTIAL: pure evaluation evidence alone does not establish action application, and their other actions and provider timing remain outside this local model.

## Acceptance contract

- Unversioned matching objects are deleted through the ordinary retirement path. A bucket's soft-delete policy preserves deleted versions until its existing hard-delete deadline.
- A matching live version in a versioned bucket becomes noncurrent. A later pass removes a matching noncurrent generation. Each version is visited at most once per pass; soft-deleted generations, unfinalized appendable objects and in-progress uploads are excluded.
- Temporary and event-based holds block deletion. Bucket and object retention block it until the later expiry. Releasing protection is a write that invalidates the schedule. Restore starts age from the restored generation's new creation time.
- Selection, current rules, bucket incarnation, generation, newer-version count and protection are checked inside the immediate page transaction. Rules are decoded and validated once per bucket, reused across pages while their stored JSON is unchanged, and replaced under the transaction if it changes. Overwrites, rule removal, hold changes and bucket recreation cannot turn an old candidate into an unchecked delete. A mutation failure or failed commit rolls back the whole page; earlier committed pages remain applied and retryable.
- Retirement, synchronization journal, notification outbox and pending blob deletion commit together. Live-to-noncurrent emits `OBJECT_ARCHIVE`; removal emits `OBJECT_DELETE`, including soft deletion under the existing local notification contract. Failed commits roll back and remain retryable. Process-abort and power-loss snapshots must restart idempotently.
- A corrupt stored rule is quarantined for its bucket. Deterministic version restoration errors discovered before mutation skip only that version and increment `skipped`; later versions and pages still run. These cases do not fail the pass or drop readiness. Each quarantine emits a diagnostic once per stored rules revision or version metageneration during a process lifetime; repairing a rule through the API or repairing version metadata resumes execution. Deduplication uses a SQLite TEMP table, spills to disk on the disk backend and shares the memory backend's existing TEMP quota. Successful repairs and obsolete identities remove their history. Diagnostics are emitted after page commit so a rollback cannot produce repeated unrecorded warnings. Transient I/O or locking failures and all mutation failures still roll back, retry after one second and affect readiness through the existing maintenance health checks.
- Selection holds at most sixteen bucket names and one page of 128 version keys. One page runs on the bounded catalog writer and commits once, avoiding a durable commit per deletion. Request work can run between pages. At most 1024 actions apply per pass; reaching that limit returns the current injected time for continuation.
- The version scan uses reverse `(bucket, name, generation)` primary-key order, without catalog-sized materialization or sorting. The newer-version count is recomputed under each mutation transaction and excludes soft-deleted versions.
- Without rules, ordinary writes add one atomic load and the thread-local write-scope bookkeeping, with no lifecycle query or wake. Rules are marked at the three SQL write sites and rearm centrally when the connection finishes the write. Marking outside that scope fails a debug assertion. Startup reconstructs armed/dirty state from stored rules; no persisted timer can become stale.
- A clean schedule does not scan before its deadline. The dirty bit is catalog-wide: while any rules exist, writes in any bucket can cause rescans of all rule-bearing buckets at one-second intervals, including buckets that have no Delete rules. Dirty writes coalesce until one second after the prior scan, unless a due deadline or a capped continuation is earlier. With no writes or due work there is no polling timer. A forced pass bypasses coalescing. The returned deadline uses the injected clock, including retention expiry.
- Storage class changes remain unapplied. Until that action is implemented, every Delete rule containing `matchesStorageClass` is deferred, even if no transition rule currently exists or ever existed. Earlier unapplied transitions cannot be inferred from the current configuration: replacing a transition with a class-dependent Delete must preserve the object. Other Delete rules still execute. The Delete-only evaluator produces deadlines only for executable Delete rules, so deferred rules do not wake maintenance.

## Operator controls and upgrade behavior

Without directory synchronization, stored Delete rules execute by default, including rules stored by earlier releases. Upgrading can therefore delete existing objects as soon as their stored rules match. With `sync_dir`, execution defaults to disabled so stored rules do not delete host files unexpectedly. Set `--lifecycle-execution enabled`, `GCS_MIMIC_LIFECYCLE_EXECUTION=enabled` or JSON `"lifecycle_execution":"enabled"` to opt in: Delete then propagates through the ordinary synchronization journal and also removes synchronized host files. An explicit `disabled` preserves store-only behavior in either mode. Omission is distinct from either explicit choice. CLI > environment > JSON > default precedence applies; invalid values require `enabled` or `disabled`.

The ready event reports the effective string `lifecycle_execution`. An unconfigured catalog never executes lifecycle actions; startup applies the effective configuration before maintenance starts. A process emits one `lifecycle_sync_disabled` diagnostic when the synchronization default disables stored rules, whether rules exist at startup or are first added later, with the option needed to enable them and its host-file effect. Disabled execution preserves rule round trips and also disables the forced run route.

`POST /_internal/lifecycle/run` with an empty body runs one lifecycle pass and returns the effective `enabled` boolean plus `scanned`, `deleted`, `archived`, `blocked` and `skipped` counts. A disabled pass answers `enabled:false` with zero counts, distinct from an enabled empty pass. It uses the existing startup opt-in, instance administrator, Host and browser-origin guards. Other methods return 405; bodies and query parameters return 400. There is no time travel. A scheduled maintenance wake follows the pass to reclaim journaled bodies and arrange future work.

## Sources and supported subset

Rechecked on 2026-11-10: the official [lifecycle guide](https://docs.cloud.google.com/storage/docs/lifecycle), [versioning](https://docs.cloud.google.com/storage/docs/object-versioning), [soft delete](https://docs.cloud.google.com/storage/docs/soft-delete), [holds](https://docs.cloud.google.com/storage/docs/object-holds), [bucket retention](https://docs.cloud.google.com/storage/docs/bucket-lock), and [object retention](https://docs.cloud.google.com/storage/docs/object-lock). The [research](lifecycle-execution-research.md) records the Discovery revision 20261103 and SHA-256 `30b498129041aceedca2e60678443e2671531cf0c08b3488c6934c212a2cb272` used for the evaluator contract. No real Google Cloud requests were made.

Google documents asynchronous execution without timing guarantees and configuration propagation that can take 24 hours. This implementation simulates scheduling locally; it does not simulate propagation or billable infrastructure. Conditions are provided by the evaluator's separately verified supported subset. Configuration remains available through the existing JSON and gRPC adapters. XML and initialization-file lifecycle configuration, hierarchical folders, storage class changes and multipart abort execution are outside this slice.

| Executed condition subset | End-to-end evidence |
| --- | --- |
| Age zero, positive age, strict `createdBefore` and a microsecond before each deadline | `creation_age_zero_and_strict_creation_dates_drive_exact_delete_deadlines` |
| Unset/future Custom-Time, `daysSinceCustomTime`, strict `customTimeBefore` and the exact metadata deadline | `custom_time_conditions_require_metadata_and_delete_at_its_exact_deadline` |
| Live/noncurrent state, inclusive newer counts across pages, `daysSinceNoncurrentTime` and `noncurrentTimeBefore` from the archive instant | `only_matching_noncurrent_generations_are_removed_with_newer_versions_across_pages`, `noncurrent_time_conditions_use_the_archive_instant_and_preserve_live_versions` |
| Case-sensitive ORed prefix/suffix lists, AND across conditions, strict data-byte size thresholds, zero/equality and exclusion of metadata bytes | `name_lists_and_strict_size_conditions_use_published_names_and_data_bytes` |
| Unconditional deferral of class-dependent Delete after replacing a transition rule; independent Delete still executes | `replacing_an_unapplied_transition_with_a_class_dependent_delete_preserves_the_object`, `pending_storage_transitions_defer_class_dependent_deletion_but_not_other_delete_rules` |

The evaluator's overflow, leap-date and full transition-priority tests remain separate evidence; this table does not claim those as catalog executions. The ledger records the achieved time/execution behavior as `LOCAL_SIMULATION`, retaining pending cases and the `EXACT` target.

Conservative local choices stay visible in `SPEC-LIFECYCLE-EXECUTION`: lifecycle waits for object retention even before archiving a live version; newer counts exclude soft-deleted versions; soft deletion emits the ordinary local delete event. Provider parity for these interactions is unverified. The existing bucket-retention limitation also remains: releasing an event-based hold does not restart its retention age. No schema migration or new durable filesystem step is introduced.

Location-dependent storage class transitions, including suppressing DRA to MULTI_REGIONAL for a regional bucket, belong to the later storage class execution slice. No storage class transition is applied here.

Source inspection of `sync::ingest::Ingest::ingest`, `sync::mirror::Reconciler::tracked_deletions` and the live write-back path establishes a restart limit: if lifecycle deletes an object while synchronization is absent, a later synchronization run still removes its previously tracked host file. Divergent tracked bytes go through the existing retained-conflict handling. A file in a previously untracked synchronization root instead enters ingestion and can recreate the absent object. The lifecycle option controls execution in the current process; it does not cancel earlier catalog deletions. This cross-startup scenario is source-inspected, not an additional end-to-end test, and synchronization code is unchanged.

The initializer rejects lifecycle input and filesystem import constructs buckets without rules. Public administrative publications can carry a `BucketRecord` with rules; their shared lower SQL functions carry the same marker as API publication, including ensure, import, seed and initialization transactions. A harmless extra arm after a rolled-back SQL write is permitted; missing a committed rule is not.

Upgrade execution is verified by same-version reopen of stored rules. The released-data fixture has no rule-bearing bucket, so execution of rules from an actual earlier-release fixture remains unverified. A running-maintenance test exercises write wakeup after advancing the injected deadline; physical timer expiry is reused from the existing maintenance scheduler rather than timed separately here.

## Verification

The implementation tests live in `src/storage/catalog/lifecycle/tests.rs`; wire guard checks live in `tests/it_admin/admin_http.rs`, and configuration precedence checks in `src/config/storage.rs`. They target the fixed evaluator interface, with no substitute evaluator.

Verified on 2026-11-10 (UTC):

| Command | Result |
| --- | --- |
| `cargo check --locked --offline --tests` | Passed |
| `cargo check --workspace --locked --offline --tests` | Passed after evaluator code and integration-branch merges, including xtask and grpc-codegen |
| `cargo test --locked --offline --lib storage::catalog::lifecycle::tests -- --nocapture` | 37 passed, 0 failed, including synchronization, unconditional storage-class/Delete deferral, isolated malformed versions, deduplicated diagnostics and condition boundaries |
| `cargo test --locked --offline --lib config::storage::tests::lifecycle_execution_defaults_on_and_disable_obeys_native_precedence` | 1 passed, 0 failed |
| `cargo test --locked --offline --lib server::admin::tests::config_uses_the_instance_principal_instead_of_project_roles` | 1 passed, 0 failed; both config and lifecycle routes enforce the instance administrator |
| `cargo test --locked --offline --test it_admin lifecycle_run_uses_admin_guards_and_refuses_time_travel_or_invalid_methods` | 1 passed, 0 failed |
| `cargo test --locked --offline --lib lifecycle -- --nocapture` | 110 passed after evaluator code merge, including all 37 sweep tests and rearming paths |
| `cargo test --locked --offline --lib bucket` | 226 passed after evaluator code merge |
| `cargo test --locked --offline --test it_json bucket` | 103 passed after evaluator code merge |
| `cargo test --locked --offline` | 3039 passed after merging the final evaluator and the integration branch: 1957 library tests, 32 binary tests, all integration suites and documentation tests; 0 failed, 1 existing ignored local Pub/Sub opt-in test |
| `cargo xtask lint` | 0 new production violations, 0 ratcheted findings |
| `cargo xtask dependencies check --scope all` | Every compiler, direct dependency, SDK and container input current at the checked snapshot; no dependency changes |
| `cargo xtask compatibility check` | 607 feature rows validated; generated English/Japanese matrices match |

Markdown lint covers this contract, release note, option listings, roadmap and both configuration companions with the existing repository rules.

Source inspection of `Catalog::run`, `wait_exclusive` and `Admin::reset_until` confirms that lifecycle page transactions and reset share the same execution permit. A page already running can delay reset admission, but cannot overlap its exclusive mutation; lifecycle retains no catalog execution permit between pages. The existing short-wait reset fixture creates no lifecycle rules, so its scheduled lifecycle pass returns before any SQL work. Its load-sensitive upload timeout is not evidence that lifecycle holds a write lock during reset. The full test run includes that unchanged fixture.

The version-scan `EXPLAIN QUERY PLAN` result on the catalog schema is:

```text
SEARCH objects USING PRIMARY KEY (bucket=? AND (name,generation)<(?,?))
SEARCH objects USING PRIMARY KEY (bucket=?)
```

`the_version_scan_seeks_the_primary_key_without_a_temporary_sort` asserts primary-key search and absence of a temporary B-tree or full object scan. SDK harnesses, containers and guide walkthroughs are run separately by integration and are not claimed here.
