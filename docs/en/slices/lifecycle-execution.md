# Lifecycle Delete execution

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: `BEHAVIOR-LIFECYCLE-002`, `BEHAVIOR-LIFECYCLE-010` and the lifecycle-time subset of `BEHAVIOR-OBJECT-003`. The feature ledger owns status. This slice executes stored `Delete` rules through the shared pure evaluator, with deterministic local scheduling. Storage class transitions and incomplete multipart upload aborts remain separate slices.

## Acceptance contract

- Unversioned matching objects are deleted through the ordinary retirement path. A bucket's soft-delete policy preserves deleted versions until its existing hard-delete deadline.
- A matching live version in a versioned bucket becomes noncurrent. A later pass removes a matching noncurrent generation. Each version is visited at most once per pass; soft-deleted generations, unfinalized appendable objects and in-progress uploads are excluded.
- Temporary and event-based holds block deletion. Bucket and object retention block it until the later expiry. Releasing protection is a write that invalidates the schedule. Restore starts age from the restored generation's new creation time.
- Selection, current rules, bucket incarnation, generation, newer-version count and protection are checked inside the immediate page transaction. Rules are decoded and validated once per page. Overwrites, rule removal, hold changes and bucket recreation cannot turn an old candidate into an unchecked delete. A version failure or failed commit rolls back the whole page; earlier committed pages remain applied and retryable.
- Retirement, synchronization journal, notification outbox and pending blob deletion commit together. Live-to-noncurrent emits `OBJECT_ARCHIVE`; removal emits `OBJECT_DELETE`, including soft deletion under the existing local notification contract. Failed commits roll back and remain retryable. Process-abort and power-loss snapshots must restart idempotently.
- A corrupt stored rule is quarantined for its bucket with a diagnostic; it does not fail the pass, drop readiness or schedule error retries. Other buckets' counts remain available through the admin route. Transient I/O or locking failures still retry after one second and affect readiness through the existing maintenance health checks.
- Selection holds at most sixteen bucket names and one page of 128 version keys. One page runs on the bounded catalog writer and commits once, avoiding a durable commit per deletion. Request work can run between pages. At most 1024 actions apply per pass; reaching that limit returns the current injected time for continuation.
- The version scan uses reverse `(bucket, name, generation)` primary-key order, without catalog-sized materialization or sorting. The newer-version count is recomputed under each mutation transaction and excludes soft-deleted versions.
- Without rules, ordinary writes add one atomic load and the thread-local write-scope bookkeeping, with no lifecycle query or wake. Rules are marked at the three SQL write sites and rearm centrally when the connection finishes the write. Marking outside that scope fails a debug assertion. Startup reconstructs armed/dirty state from stored rules; no persisted timer can become stale.
- A clean schedule does not scan before its deadline. The dirty bit is catalog-wide: while any rules exist, writes in any bucket can cause rescans of all rule-bearing buckets at one-second intervals, including buckets that have no Delete rules. Dirty writes coalesce until one second after the prior scan, unless a due deadline or a capped continuation is earlier. With no writes or due work there is no polling timer. A forced pass bypasses coalescing. The returned deadline uses the injected clock, including retention expiry.
- Storage class changes remain unapplied. Until that action is implemented, a rule set containing any `SetStorageClass` defers Delete rules with `matchesStorageClass`; otherwise a skipped transition could cause deletion that Cloud Storage would avoid. Other Delete rules still execute. The Delete-only evaluator produces deadlines only for executable Delete rules, so ignored transitions do not wake maintenance.

## Operator controls and upgrade behavior

Without directory synchronization, stored Delete rules execute by default, including rules stored by earlier releases. Upgrading can therefore delete existing objects as soon as their stored rules match. With `sync_dir`, execution defaults to disabled so stored rules do not delete host files unexpectedly. Set `--lifecycle-execution enabled`, `GCS_MIMIC_LIFECYCLE_EXECUTION=enabled` or JSON `"lifecycle_execution":"enabled"` to opt in: Delete then propagates through the ordinary synchronization journal and also removes synchronized host files. An explicit `disabled` preserves store-only behavior in either mode. Omission is distinct from either explicit choice. CLI > environment > JSON > default precedence applies; invalid values require `enabled` or `disabled`.

The ready event reports the effective string `lifecycle_execution`. Startup emits one `lifecycle_sync_disabled` diagnostic when stored rules are held inactive by the synchronization default, with the option needed to enable them and its host-file effect. Disabled execution preserves rule round trips and also disables the forced run route.

`POST /_internal/lifecycle/run` with an empty body runs one lifecycle pass and returns the effective `enabled` boolean plus `scanned`, `deleted`, `archived` and `blocked` counts. A disabled pass answers `enabled:false` with zero counts, distinct from an enabled empty pass. It uses the existing startup opt-in, instance administrator, Host and browser-origin guards. Other methods return 405; bodies and query parameters return 400. There is no time travel. A scheduled maintenance wake follows the pass to reclaim journaled bodies and arrange future work.

## Sources and supported subset

Rechecked on 2026-11-10: the official [lifecycle guide](https://docs.cloud.google.com/storage/docs/lifecycle), [versioning](https://docs.cloud.google.com/storage/docs/object-versioning), [soft delete](https://docs.cloud.google.com/storage/docs/soft-delete), [holds](https://docs.cloud.google.com/storage/docs/object-holds), [bucket retention](https://docs.cloud.google.com/storage/docs/bucket-lock), and [object retention](https://docs.cloud.google.com/storage/docs/object-lock). The [research](lifecycle-execution-research.md) records the Discovery revision 20261103 and SHA-256 `30b498129041aceedca2e60678443e2671531cf0c08b3488c6934c212a2cb272` used for the evaluator contract. No real Google Cloud requests were made.

Google documents asynchronous execution without timing guarantees and configuration propagation that can take 24 hours. This implementation simulates scheduling locally; it does not simulate propagation or billable infrastructure. Conditions are provided by the evaluator's separately verified supported subset. Configuration remains available through the existing JSON and gRPC adapters. XML and initialization-file lifecycle configuration, hierarchical folders, storage class changes and multipart abort execution are outside this slice.

Conservative local choices stay visible in `SPEC-LIFECYCLE-EXECUTION`: lifecycle waits for object retention even before archiving a live version; newer counts exclude soft-deleted versions; soft deletion emits the ordinary local delete event. Provider parity for these interactions is unverified. The existing bucket-retention limitation also remains: releasing an event-based hold does not restart its retention age. No schema migration or new durable filesystem step is introduced.

Location-dependent storage class transitions, including suppressing DRA to MULTI_REGIONAL for a regional bucket, belong to the later storage class execution slice. No storage class transition is applied here.

The initializer rejects lifecycle input and filesystem import constructs buckets without rules. Public administrative publications can carry a `BucketRecord` with rules; their shared lower SQL functions carry the same marker as API publication, including ensure, import, seed and initialization transactions. A harmless extra arm after a rolled-back SQL write is permitted; missing a committed rule is not.

Upgrade execution is verified by same-version reopen of stored rules. The released-data fixture has no rule-bearing bucket, so execution of rules from an actual earlier-release fixture remains unverified. A running-maintenance test exercises write wakeup after advancing the injected deadline; physical timer expiry is reused from the existing maintenance scheduler rather than timed separately here.

## Verification

The implementation tests live in `src/storage/catalog/lifecycle/tests.rs`; wire guard checks live in `tests/it_admin/admin_http.rs`, and configuration precedence checks in `src/config/storage.rs`. They target the fixed evaluator interface, with no substitute evaluator.

Verified on 2026-11-10 (UTC):

| Command | Result |
| --- | --- |
| `cargo check --locked --offline --tests` | Passed |
| `cargo test --locked --offline --lib storage::catalog::lifecycle::tests -- --nocapture` | 28 passed, 0 failed, including synchronization and storage-class/Delete interaction safety |
| `cargo test --locked --offline --lib config::storage::tests::lifecycle_execution_defaults_on_and_disable_obeys_native_precedence` | 1 passed, 0 failed |
| `cargo test --locked --offline --lib server::admin::tests::config_uses_the_instance_principal_instead_of_project_roles` | 1 passed, 0 failed; both config and lifecycle routes enforce the instance administrator |
| `cargo test --locked --offline --test it_admin lifecycle_run_uses_admin_guards_and_refuses_time_travel_or_invalid_methods` | 1 passed, 0 failed |
| `cargo test --locked --offline` | 3024 passed: 1941 library tests, 32 binary tests, all integration suites and documentation tests; 0 failed, 1 existing ignored local Pub/Sub opt-in test |
| `cargo xtask lint` | 0 new production violations, 0 ratcheted findings |
| `cargo xtask dependencies check --scope all` | Every compiler, direct dependency, SDK and container input current at the checked snapshot; no dependency changes |
| `cargo xtask compatibility check` | 607 feature rows validated; generated English/Japanese matrices match |

Markdown lint covers this contract, release note, option listings, roadmap and both configuration companions with the existing repository rules.

The version-scan `EXPLAIN QUERY PLAN` result on the catalog schema is:

```text
SEARCH objects USING PRIMARY KEY (bucket=? AND (name,generation)<(?,?))
SEARCH objects USING PRIMARY KEY (bucket=?)
```

`the_version_scan_seeks_the_primary_key_without_a_temporary_sort` asserts primary-key search and absence of a temporary B-tree or full object scan. SDK harnesses, containers and guide walkthroughs are run separately by integration and are not claimed here.
