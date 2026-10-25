# Shared import journal

## Scope and acceptance

This is a local administrative storage contract under DESIGN-072 and
`onboarding-state-contract`, not a Google API. No new upstream protocol or
dependency is introduced. Source discovery, digest verification, publication
and source cutover remain the Import adapter's responsibility.

- Persist one import binding (source identity digest, stable export fingerprint,
  expected element count), a contiguous committed cursor and an active marker.
  Resume only the identical binding; refuse a second or completed import.
- Bound each progress transaction and observation page to 256 elements. Store
  each element's stable fingerprint, inode/file ID and nanosecond ctime without
  retaining a manifest or bodies in memory. Reject gaps, stale cursors and
  out-of-range counts. Expose a crate-owned transaction hook for publication.
- Replace revalidated observations with compare-and-swap after the caller has
  verified the recorded body digest; preserve cursor and export fingerprint.
  A failed transaction retains the previous observations.
- Refuse serve before PAP, initialization, seed or bind while the marker exists.
  Errors close the catalog through existing startup ownership.
- Complete only the matching import at its expected count; marker removal and
  startup_seed completion share one transaction and roll back together.
- Migrate schema 15 to 16 atomically, preserving prior data and seed state;
  test DDL rollback/retry, stopped snapshots and newer-version rejection.

## Decisions and boundaries

The cursor is an integer ordinal in the adapter's deterministic export order,
bounded by the expected count and SQLite's signed integer range. Fingerprints
are 32-byte SHA-256 values supplied by the trusted adapter. The binding includes
the source identity separately so a matching export at a foreign source cannot
silently resume. The journal keeps its completed record for audit and refuses
reuse; it has no marker-only deletion or abandonment operation.

Observation inode is an optional u64 and ctime an optional signed i128 count of
nanoseconds. Fixed-width binary encodings retain their full ranges. A stable
element fingerprint covers path, metadata, size, mtime and body digests; only
inode/ctime observations can be replaced by revalidation. The storage layer
cannot prove the caller read bytes, so verification and terminal/retryable error
classification stay in Import. An observation replacement never advances progress.

The journal does not hold SQLite while reading bodies. Catalog administrative
admission and owned blocking jobs provide bounded waits and shutdown drain.
The publication hook is delivered here; wiring it into shared-publication-modes
and import-filesystem is a later integration, not evidence of atomic publication
already being connected.

Schema 16 is an on-disk upgrade. Back up the entire stopped data directory,
including its catalog and referenced blobs, before upgrading. Older binaries
must reject schema 16; there is no in-place downgrade or marker-only reset.
Restore a complete pre-upgrade backup into a separate empty destination using
the matching older executable. The snapshot tests below verify schema retention,
not a complete older-executable restoration. See the
[shared state contract](onboarding-state-contract.md#accepted-import-requests).

## Verification

Verified on 2026-10-25 with Rust 1.99.1 and the locked dependencies:

| Command | Result |
| --- | --- |
| `cargo xtask dependencies check` | PASS; all inputs CURRENT, no dependency changes. |
| `cargo test --lib import_journal --locked --offline` | PASS; 8 tests including the process-exit helper. |
| `cargo test --lib catalog_tests --locked --offline` | PASS; 21 existing catalog tests. |
| `cargo test --lib journal_schema_upgrade --locked --offline` | PASS; existing object/policy/active-session migration regression. |
| `cargo test --bin gcs-mimic unfinished_import --locked --offline` | PASS with localhost permission. |
| `cargo clippy --lib --bin gcs-mimic --tests --locked --offline -- -D warnings` | PASS. |
| `just check` | PASS on source `2d8e189`: formatting, workspace Clippy/tests/doc-tests, ledger/matrices and shell/release checks. |

English and Japanese matrices were regenerated from the ledger and remain
byte-identical because the parent feature status stays PARTIAL. The dependency
freshness check ran at 2026-10-25T04:07:09Z against official upstream sources.

`import_journal_binding_pages_prefix_and_reopen_are_bounded` verifies source,
fingerprint and count mismatches, stale/gapped progress, 256/257 boundaries,
full-range inode/ctime encoding, unfinished reopen, completion and completed
reuse rejection. `import_journal_revalidation_is_atomic_without_advancing_progress`
verifies immutable element identity, stale observation rejection, injected SQL
failure rollback, repeated updates and restart.
`import_journal_observation_crashes_preserve_committed_baselines` exits child
processes without destructors before and after observation commit; reopening
preserves the respective old/new baseline without advancing the cursor.
`import_journal_progress_hook_rolls_back_with_its_owning_transaction` verifies
the hook cannot escape an owning transaction rollback.

`import_journal_completion_and_seed_rollback_together` injects failure after
the marker update and verifies both marker and seed rollback, retry, empty
imports and occupied-destination refusal.
`unfinished_import_blocks_startup_before_policy_seed_and_bind` combines an
unfinished marker with invalid PAP configuration and a missing seed directory,
verifies the import diagnostic wins, closes/reopens the catalog, and verifies
completion skips seed and permits bind.

`import_journal_migration_rolls_back_and_retries_without_seed_changes` verifies
partial-DDL collision rollback and retry. The stopped schema-15 snapshot in
`import_journal_upgrade_preserves_native_state_and_stopped_snapshot` remains
readable as schema 15 while the migrated catalog validates as schema 16 and
retains bucket/seed values; existing migration regression retains object,
policy and active session identity. Future-schema rejection also preserves
database/WAL bytes in the existing catalog tests.

SDK, container and guide runs are not required because HTTP behavior, container
inputs and guides are unchanged. No performance measurement is required for
this correctness slice. Host power loss, full older-executable directory
restoration and actual Import digest-gated publication/resume remain unverified.
DESIGN-072 stays PARTIAL; the journal storage case passes while the broader
Import observation integration case remains PENDING.
