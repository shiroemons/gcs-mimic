# Shared synchronization observation

Scope: DESIGN-062, DESIGN-072 and DESIGN-074. Parent rows remain PARTIAL;
UX-002 seed behavior is unchanged. This slice provides storage APIs only; it
does not configure or start synchronization, change HTTP/CLI or write files.

## Acceptance contract

- `Admin::list_all_buckets(after, limit)` returns all owner projects and empty
  buckets, complete stored bucket records and their incarnations. Continue after
  the last returned name with a binary keyset; pages allow 1..=256 entries and
  share the existing expanded-metadata budget and singleton/lookahead rules.
  Raw string cursors allow stored legacy names without revalidating them as new
  native bucket names. Test project boundaries, metadata limits and recreation.
- `Catalog::wait_for_changes(after: JournalFence)` returns the current fence and
  `rescan_required`. Reuse read_changes cursor validity: disabled journal, gaps,
  future sequence or a different revision return immediately requiring rescan.
  Changed sequence/revision wakes all registered waiters. Journal pages retain
  their finite captured fence; waiting does not make listing an atomic snapshot.
- Register before rechecking state, without a commit/register lost wakeup. Test
  commits before/during/after registration and all object, bucket and IAM mutation
  paths, epoch changes, restart, multiple waiters and saturated API admission.
- Wait without admin/read/execution permits. Cancellation changes no durable
  state; shutdown returns AdminError::Closed. Rollback and unrelated writes may
  signal a recheck but never return a false change or create a polling loop.

## Decisions and boundaries

Reuse the existing Notify primitive and the three shared catalog execution
boundaries. Notify after an operation changed SQLite's total-change counter,
including possible rollback/irrelevant-write signals. Waiters always re-read
the durable journal fence before returning; reads do not signal themselves.
No polling, extra connection, dependency or schema migration is needed.
The original cursor remains fixed while retrying an effect-free wait.

`BucketPage.items` contains `BucketEntry { bucket, incarnation }`; owner is the
stored `bucket.project`, and continuation is `bucket.name` of the last entry.
The count is an upper bound, not a promise to fill a page: the 256 KiB expanded
metadata budget often cuts ordinary pages earlier. One oversized bucket still
advances alone, with at most one lookahead row and no JSON expansion of that row.
Missing incarnation state fails the page rather than hiding a bucket.

`ChangeObservation { fence, rescan_required }` describes the selected catalog,
using exactly the same cursor validity as `read_changes`. A disabled journal or
invalid cursor returns immediately; an unchanged enabled journal waits without
timers. Read snapshots retain the existing bounded administrative admission and
its Busy/Closed errors; all permits are released before the notification wait.
Shutdown notification is registered before the snapshot too. Notifications are
hints, not the durable event feed: only committed sequence/revision changes or
the existing rescan conditions can complete the wait.

Capture a fence before paged current-state listing and replay changes from that
fence afterward, honoring rescan indications. A delete/recreate at or before a
list cursor is found through the journal/new reconciliation, with its new owner
and incarnation. Neither listing nor waiting claims a whole-catalog snapshot.
No Google method or Discovery field changes, and no open Google API gap is
resolved by these local administrative primitives. Schema remains 19.

## Verification

On 2026-10-25, macOS ARM64, Rust 1.99.1, the command below passed seven tests:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
RUST_TEST_THREADS=4 cargo test --locked --offline --lib sync_observation
```

- `sync_observation_all_owner_pages_include_empty_buckets_and_legacy_cursors`
- `sync_observation_owner_inventory_recreation_preserves_keyset_and_incarnation`
- `sync_observation_owner_pages_share_metadata_budget_and_bounded_lookahead`
- `sync_observation_commits_before_during_and_after_registration_are_not_lost`
- `sync_observation_every_committed_mutation_wakes_all_waiters`
- `sync_observation_invalid_gap_disabled_epoch_and_restart_cursors_require_rescan`
- `sync_observation_cancel_rollback_false_wake_and_shutdown_hold_no_permits`

The related target command passed 14 tests, including existing origin-bearing
journal transactions, real retention eviction, finite page fences, atomic
migration/restart preservation, saturated API admission and metadata budgets:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
RUST_TEST_THREADS=4 cargo test --locked --offline --lib -- \
  sync_observation change_journal admin_admission_is_independent \
  bucket_list_metadata_budget
```

Deterministic barriers place commits before registration, after registration and
after snapshot selection. Three waiters observe each of the seven mutation kinds.
False/rollback signals retain the original cursor without repeated idle queries;
cancel/shutdown release waiters without owning catalog permits or mutating state.
All-owner pagination traverses 302 empty fixtures across owner boundaries with
strictly increasing names, while preserving oversized-row and malformed-lookahead
behavior. Missing incarnations fail closed.

`cargo xtask dependencies check` exited 0 with every pinned input CURRENT;
no dependency or compiler was changed. The final command below exited 0, covering
format, clippy with warnings denied, workspace/doc tests, compatibility/generated
views and shell checks. All 15 native seed tests passed, retaining UX-002.

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
RUST_TEST_THREADS=4 just check
```

Markdownlint 0.24.0 reported no issues in the two changed slice documents.
English and Japanese feature matrices were regenerated and remained identical
because parent implementation status and fidelity did not change.

SDK, container, full guide and benchmark execution were not performed because
HTTP, container inputs and user guides are unchanged. Linux/Windows runtime and
mirror/startup integration remain unverified or unimplemented in this slice.
