# Shared synchronization write-back

Scope: DESIGN-062, DESIGN-072, DESIGN-074 and AUDIT-014. Parent rows remain
PARTIAL; UX-002 seed remains unchanged. This slice supplies typed storage APIs,
not target replacement, mirror/watch startup, conflict registration or HTTP
recovery/discard controls.

The later [shared export interfaces](shared-sync-export-interfaces.md) add
descriptor-relative workspace-to-target replacement, both-parent fsync and
durable findings in schema 21. They preserve the receipt/protection protocol
below; mirror orchestration and conflict transfer remain pending.

## Acceptance contract

- Schema 20 persists catalog/root-bound pending operations, original bucket
  owner/incarnation, logical name/path, selected generation/hashes, opaque
  workspace entry, revision/phase and journal fence. It binds the reserved
  workspace's real device/inode too. Pages cap at 256; bucket
  deletion/recreation never cascades pending recovery state away.
- Prepare and arm an intent durably before destructive file work. Phases are
  Prepared, Armed, Protected, Durable and Complete; stale tokens and invalid
  root/owner/incarnation/generation/path transitions fail closed. Deletion
  observations require a complete baseline and absent API winner.
- Produce an unforgeable durable receipt only after checking actual output or
  absence, hashes/identity, file sync and all affected parents on an owned OS
  worker. Ack atomically commits sync state, contiguous requested journal
  progress and completion, rechecking the API winner and holds. Failed/ambiguous
  commits retain recoverable intent and never overwrite API state.
- Workspace entries remain protected until resolved. Discovery accounts actual
  retained size without clipping, including a sparse hard-limit-plus-one file;
  byte/entry limits and I/O failures stop the root durably. No shared completion,
  restart or cleanup API can release a Protected body. Conflict registration and
  original-owner-authorized discard remain later runtime work.
- Restart reports unresolved operations and stops new destructive intake until
  recovery/revalidation. Test phase boundaries, ENOSPC, cancellation/shutdown,
  stale/foreign tokens, owner deletion, page boundaries and migration
  rollback/retry with existing API, seed and sync data retained.
- Transfer the existing ObjectReader descriptor without reopening a storage path
  or creating a hard link. Copy through existing dedicated workers and fsutil;
  test zero/binary bytes, overwrite/delete, unchanged source offset and owned
  descriptor/root lifetime through cancellation and drain.

## Decisions and boundaries

Filesystem work runs outside SQLite transactions. Private receipts prove only
the scoped file/parent checks, not an atomic filesystem/catalog snapshot. The
adapter still reconciles later external edits and completes the initial merge.
Shared storage never unlinks protected workspace entries or blindly reverses a
replacement. Retained recovery records keep original owner identity after bucket
deletion. Pending and terminal history are bounded; parent statuses remain
PARTIAL while mirror/conflict/authorization runtime work is unimplemented.

### Adapter sequence

Obtain `RootLease::writeback(Limits)` after normal activation. Limits allow
1..=1 TiB and 1..=1,000,000 pending entries per root. A page contains at most
256 records; terminal metadata retains at most the recent 65,536 operation
ordinals, with bounded pruning and no filesystem cleanup. Aggregate byte charges
use 128-bit integers so unexpected actual sizes cannot overflow or be clipped.

1. `prepare` captures the current bucket owner/incarnation, selected generation
   and hashes, validated logical target, a journal fence and an opaque workspace
   name. It reserves the observed target size, without reading its body into
   memory. An interrupted or ambiguous prepare is recovered through `page/get`;
   an active logical target cannot acquire another intent.
2. `arm` checks the current API winner/hold and commits Armed plus a root recovery
   stop before destructive work. Only one Armed/Protected/Durable operation may
   exist per root. The adapter uses `Writeback::file_job(armed_token, job)` for
   owned root/workspace descriptors, with current token/API/stop validation;
   this slice does not perform replacement, exchange or conflict transfer.
3. `inspect` discovers the reserved workspace entry, syncs it and its parents
   where possible, and records its actual size as Protected. The reservation is
   replaced by that actual charge, not added twice. A missing previously Protected
   entry is an I/O stop. A SQLite FULL error can prevent recording the newly
   observed size: the earlier durable Armed intent and recovery stop remain,
   the body is untouched, and inspection must retry after space is available.
4. With no retained entry, `durable` validates installed bytes through a fixed
   64 KiB buffer or verifies removal, syncs the file and all affected parents,
   then records Durable and returns a private `DurableReceipt`. Revalidation can
   recover that operation's transient I/O stop; it cannot release Protected bodies
   or another operation's failed validation.
5. `acknowledge` rechecks the file identity, workspace absence, receipt revision,
   stored fingerprint and current original-owner API conditions. Its transaction
   records the synchronized generation/fingerprint, optional journal progress and
   Complete together. A progress fence must be current, at least the operation's
   captured fence, and have no other pending operation on that root. Complete
   earlier operations without progress and advance only with the final one.

`checkpoint` handles metadata-only or empty-baseline progress: require a complete
baseline, no pending operations, a Ready root and a valid journal fence. The
adapter remains responsible for preparing every required file operation before
claiming that fence. Listing/waiting remains a finite-fence, rescan-aware protocol.
`resume` clears a stop only after all pending operations have completed and no
protected or over-limit charges remain. Restart marks every root with unfinished
work as requiring recovery; new `prepare` is blocked. Existing Prepared intents
may be armed to finish that recovery after validation, but cannot bypass another
active operation, Protected body, I/O stop or exceeded quota.

Durability checks and their SQLite updates are separate boundaries. A failure
before the state transaction leaves the preceding phase; a failed transaction
rolls back progress and fingerprints; an ambiguous completion is resolved by
`get`, not a second destructive operation. Root and catalog leases remain owned
by queued SQL/file jobs and detached inspect/durable/ack tasks when callers cancel.
No filesystem work runs inside a SQLite transaction.

One catalog-owned execution semaphore serializes write-back handle registration,
file jobs and state/progress transitions across all roots and independently
obtained handles. Existing administrative admission bounds waiting requests to
32, with its 30-second Busy deadline, shutdown signal and cancellable waiting.
After admission an owned task keeps the semaphore and leases until every real
file/SQL job drains, even if its caller cancels. A file job therefore cannot add
an entry after another handle has already acknowledged Complete/Ready. SQLite's
execution permit is not held while awaiting files or this write-back gate;
ordinary API reads remain available. Callbacks must finish all their work before
returning and must not let directory descriptors escape into later work.

Workspace registration persists device/inode, and every file job retains that
opened directory descriptor and checks its name/identity before and after work.
A directory swap cannot redirect entry reads/writes to a same-named replacement.
Mismatch persists an I/O/recovery stop, without completing the intent or releasing
charges; reopening still rejects the replacement. A known missing workspace is
not recreated. Restore the original directory identity before inspection and
recovery; this slice has no workspace identity reset or discard control.

I/O stops belong to each operation revision, with independent entry/target
failure bits. Entry inspection can clear only that operation's entry failure;
successful target durability validation clears its target failure. Both survive
restart and aggregate into the root stop. One operation's success never clears
another's failures. The separate root flag records workspace identity failure
and clears only after the selected workspace and parent durability revalidate.

`ObjectReader::into_file` transfers its already opened immutable-generation
descriptor. It preserves the descriptor's current offset; existing
`fsutil::clone_or_copy_into` copies the complete source without changing that
offset. Move the descriptor into the armed write-back's dedicated file job.
Storage paths and hard links are not part of this handoff.

## Verification

On 2026-10-25, macOS ARM64, Rust 1.99.1, the following passed 14 tests:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
RUST_TEST_THREADS=4 cargo test --locked --offline --lib writeback
```

- `writeback_durable_receipts_atomically_ack_fingerprints_progress_and_completion`
- `writeback_pending_pages_survive_bucket_deletion_recreation_and_restart`
- `writeback_protected_sparse_body_exceeds_quota_without_cleanup_or_false_ack`
- `writeback_rejects_foreign_stale_hold_and_changed_api_winners`
- `writeback_failed_ack_rolls_back_progress_and_fingerprint_then_retries`
- `writeback_progress_cannot_skip_pending_work_and_protection_stops_other_operations`
- `writeback_removal_requires_complete_baseline_and_absent_unchanged_api_winner`
- `writeback_sqlite_enospc_keeps_displaced_bytes_and_retry_captures_actual_size`
- `writeback_schema_migration_rolls_back_retries_and_preserves_api_seed_root_and_sync`
- `writeback_restart_recovers_each_file_and_catalog_boundary_without_false_progress`
- `writeback_owned_descriptors_preserve_zero_binary_bytes_offsets_and_drain`
- `writeback_workspace_replacement_keeps_owned_bytes_and_blocks_completion_after_restart`
- `writeback_other_operation_success_cannot_clear_failed_entry_or_target_validation`
- `writeback_cancelled_job_serializes_separate_handles_until_protected_body_is_recorded`

The related `cargo test --locked --offline --lib sync_state` command initially
passed 28 tests. The broader target command below passed 57 after the safety
corrections; `cargo test --locked --offline --test seed` passed all 15 tests.
`cargo clippy --locked --offline --lib --tests -- -D warnings` passed.

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
RUST_TEST_THREADS=4 cargo test --locked --offline --lib -- \
  writeback sync_root sync_state sync_observation import_publication \
  import_journal change_journal
```

Migration tests exercise name collisions, real SQLite FULL, explicit transaction
rollback and open retry, preserving catalog/root identity, API object metadata,
seed completion and synchronized generation. Completion failure injection leaves
the old fingerprint/progress intact and retries. Restart fixtures cover Prepared,
Armed, copied workspace, installed target, Durable and Complete boundaries.
These are deterministic local boundary tests, not host-power-loss evidence or an
implementation of the future target exchange/conflict algorithm.

Workspace replacement is tested during an owned descriptor copy, after the
swap and after restart. The original bytes/intent survive until the original
directory is restored and the actual retained size is recorded as Protected.
A separate regression keeps another Prepared operation's failed entry validation
stopped through successful output validation and restart. A target failure also
survives a subsequent entry failure/recheck until full output validation passes.
The sparse Protected-body test deletes/recreates the original bucket and verifies
its original owner/incarnation, exact retained size, body and stop survive restart.
A separate-handle barrier leaves an owned file job blocked before entry creation
and cancels its caller. Durable/ack/resume and new handle acquisition remain
queued; cancellation releases their waiting slots and ordinary API reads succeed.
Only after the job drains can inspection capture the late body as Protected,
retaining its original owner, 19-byte charge and stop across restart.

The first `just check` stopped at two existing seed tests: the blocked-publication
probe observed a diagnostics event before seeding, and the TERM/drain test timed
out waiting for seeding. The new unconditional recovery UPDATE acquired a writer
lock even with no pending work. Open now checks for unfinished operations first
and writes only when needed. Both tests and the full 15-test seed suite pass
without changing their negative assertions or timeouts.

`cargo xtask dependencies check` exited 0 with every checked compiler/dependency
input CURRENT. No dependency or lockfile changed. On the corrected source the
following command passed the complete check before the final cancellation
serialization correction:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
RUST_TEST_THREADS=4 just check
```

The first check on the final serialized source exited 101: format/clippy passed,
then the library stage reported 440 passes and one failure in the unchanged
`storage::resumable_http_tests::failed_finalization_and_cancel_cleanup_retry_before_session_deadline`.
Its cleanup wait failed at `src/storage/resumable_http_tests.rs:1865` with
`assertion failed: tokio::time::Instant::now() < deadline`. The exact isolated
rerun passed one test in 2.17 seconds; conditions and timeouts were not changed.
The cause remains unconfirmed. Remaining workspace/integration/doc/shell stages
were not reached by that attempt. The coordinator authorized one additional full
check after other self-managed tests completed. On identical source and settings
it exited 0, including all 441 library tests, workspace/integration/doc tests,
all 15 seed tests, compatibility/generated views and shell checks. No test,
condition or timeout changed between attempts. The 14 write-back and 57 shared
target tests also pass independently.

Markdownlint 0.24.0 passed all three changed slice documents. English and Japanese
matrices were regenerated and remained unchanged because parent status/fidelity
did not change. Standalone `cargo xtask compatibility check` passed 591 feature
rows. Independent review approved the final implementation with no critical or
major findings; its limited confirmation also passed all 14 write-back tests.

No benchmark, SDK, container or full guide execution was performed because HTTP,
container inputs and user guides are unchanged. Linux/Windows runtime, physical
disk/filesystem power-loss behavior, mirror orchestration, conflict registration
and authorized recovery/discard remain outside this shared API slice.
