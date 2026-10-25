# Shared synchronization export interfaces

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: DESIGN-072 and DESIGN-074, both remain PARTIAL. Runtime mirror,
configuration, watching, conflict registration and controls remain pending.

## Acceptance contract

- Copy from immutable opened blobs into the tracked workspace, then replace
  through source and destination directory descriptors. Preserve displaced
  entries at their workspace names, including identity conflicts. Sync both
  parents before returning success; private write-back receipts remain the only
  route to committing synchronized state/progress after durability checks.
- Create intermediate directories relative to the leased root with no-follow
  opens, rejecting absolute paths, dot segments, symlinks and special entries.
  Sync newly created directories and their parents. Unsupported exchange
  platforms fail explicitly; do not silently fall back to a racy overwrite.
- Persist root/catalog-bound findings with bucket incarnation, object name,
  selected generation and typed mapping reason. Registration checks the live
  generation and is idempotent. Use indexed keyset pages of at most 256 entries
  and conditional removal that cannot erase a newer finding after recreation.
- Migrate schema 20 to 21 atomically, preserving API, seed, root and write-back
  state. Failed migration rolls back and can retry. Findings survive restart
  without an object-sized in-memory inventory.

## Decisions and boundaries

Findings are durable because a future status consumer must retain names/reasons
across restart without re-exporting or collecting an entire catalog. They are
observations, not permission grants or proof that the current live object still
has the recorded reason. Explicit reconciliation removes stale observations;
bucket deletion does not silently retarget records to a new incarnation.

Use `RootLease::record_finding(binding, name, generation, reason)` for an observed
live object, `findings_page(after_ordinal, limit)` to inspect this root, and
`remove_finding(finding)` after deletion, rename or successful reconciliation.
Registration replaces the same root/bucket/name observation without changing
its ordinal. Removal compares catalog/root, ordinal, bucket incarnation,
generation and reason, so an old observation cannot erase a newer one. Ordinals
are never reused. Pages are live observations, not fixed-revision status views;
a full reconciliation must restart from zero to observe updates before a cursor.
All six existing mapping reasons round-trip, including `Ignored`; mapping remains
the adapter's decision and ordinary API names are never changed to fit disk paths.

Within `Writeback::file_job`, call `fsutil::clone_or_copy_into` using the opened
`ObjectReader::into_file().await` descriptor and the pending workspace name.
Open/create the target parent with `fsutil::directory_path_at(root, relative,
create)`, then use `fsutil::durable_replace_between(workspace, pending_name,
parent, target_name, expected_identity)`. The existing `durable_replace` keeps
its same-directory behavior. Linux uses `RENAME_NOREPLACE`/`RENAME_EXCHANGE`;
macOS uses `RENAME_EXCL`/`RENAME_SWAP`. Cross-directory replacement on Windows
and exchange on unsupported Unix platforms return Unsupported. The working-tree
root adapter already requires supported Unix directory ownership.

An occupied workspace after exchange is retained by the existing inspect/
Protected protocol and cannot receive a successful completion receipt. Runtime
conflict transfer, retained-body cleanup and recovery remain pending; this API
does not implement or bypass them. Ancestor substitution must still be detected
by the future mirror orchestration, in addition to no-follow traversal.

Directory descriptors pin selected inodes. Callers must use the root lease and
descriptor-relative traversal, revalidate the selected ancestors before/after
publication and reconcile external races. Namespace changes and SQLite commits
remain separate durability domains; an error after rename may already have
installed bytes. Keep both names and the pending intent for recovery.

Schema 21 is forward-only. Back up the entire stopped data directory before
upgrading; restore the complete backup with its matching executable to roll back.
Executables supporting at most schema 20 reject schema 21. Do not edit
`user_version` to downgrade. No Google API contract or open Google gap changes.

## Verification

On 2026-10-25, macOS ARM64, Rust 1.99.1, the following passed 45 tests:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
RUST_TEST_THREADS=4 cargo test --locked --offline --lib -- \
  export sync_findings writeback sync_state sync_root sync_observation
```

New behavioral coverage:

- `export_directory_walk_rejects_redirects_and_unsafe_components`
- `export_replace_between_directories_retains_displaced_entries`
- `writeback_export_cross_directory_publication_precedes_state_and_retains_displacement`
- `writeback_export_parent_sync_failure_cannot_commit_state`
- `sync_findings_idempotent_pages_conditional_removal_and_restart`
- `sync_findings_reject_foreign_bindings_and_stale_incarnations`
- `sync_findings_migration_rolls_back_and_preserves_catalog_root_and_writeback`

The tests exercise no-replace and exchange with the same component name across
different directories, retained original/conflicting bytes, receipt-before-ack
state ordering, and failure of each parent fsync after installation. Failed
parent sync leaves Armed intent, an I/O stop and no synchronized state; explicit
durability revalidation then permits completion. These are injected local
failures, not host-power-loss evidence. Findings cover all six mapping reasons,
258-entry page boundaries, duplicate registration, stale/foreign bindings,
cross-project bucket recreation, conditional deletion and restart. Migration
tests exercise a conflicting table, actual SQLite FULL, transactional DDL
rollback and successful open/retry with the existing catalog/root/write-back
registration retained. Existing API/seed/sync migration regression fixtures
advance to schema 21 without weakening future-schema rejection.

Target clippy with warnings denied passed. `cargo xtask dependencies check`
exited 0 with all pinned inputs CURRENT; no dependency or lockfile changed.
English/Japanese matrices are generated from the ledger; parent rows remain
PARTIAL and only the new scoped acceptance cases are PASS.

On corrected source `e9e8fc7`, the command below exited 101 after 448 library
tests, 9 binary tests and preceding integration suites passed. TLS finished
with 8 passes and one failure in the unchanged
`tls_streaming_progress_and_upload_deadlines_preserve_existing_object`:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
RUST_TEST_THREADS=4 just check
```

```text
tests/tls.rs:997:5:
assertion failed: response.starts_with(b"HTTP/1.1 200")
test result: FAILED. 8 passed; 1 failed
```

Without source or timeout changes, that test passed once in isolation:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
cargo test --locked --offline --test tls \
  tls_streaming_progress_and_upload_deadlines_preserve_existing_object -- \
  --exact --test-threads=1
```

The remaining URL CLI tests (7), xtask tests (36), workspace doc tests (4),
compatibility/generated-view check and remaining shell checks passed separately.
Markdownlint 0.24.0 reported no issues in the four changed slice documents;
MD013 is disabled here to preserve exact long test identifiers. Generated
English/Japanese matrices remained byte-identical. The full `just check`
invocation is recorded as failed, not as a clean pass; the TLS concurrency/time
condition remains a verification limitation outside these shared export APIs.

No SDK, container, full-guide or benchmark execution was performed: HTTP,
container inputs and user guides are unchanged. Linux/Windows runtime,
host power loss, mirror integration and conflict recovery remain unverified
or unimplemented. Next consumer: paged sync-mirror export after readiness.
