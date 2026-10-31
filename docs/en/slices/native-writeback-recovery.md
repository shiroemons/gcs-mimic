# Native write-back restart recovery

<!-- markdownlint-configure-file {"MD013": false} -->

This storage slice supports DESIGN-072 and DESIGN-074. It adds bounded restart recovery beneath the runtime mirror; actor startup, strict control authorization, CLI/HTTP projection and final distribution checks belong to their owning slices. The feature ledger remains authoritative and the broad synchronization rows remain partial.

## Recovery contract

`Writeback::recover_pending` owns the existing write-back execution permit and root/catalog leases until its file and SQL transitions finish. It scans pending operations by ordinal in keyset pages of at most 256 entries. It uses existing bounded file workers and short transactions with static cached statements; it never collects the entire pending catalog or object body.

Prepared operations have not armed target replacement. Only their opaque private API-copy entry may be removed, including an incomplete or empty copy. Recovery verifies the original root and workspace directory identities, removes only that owned entry, synchronizes the workspace and root, and then cancels the checked intent and releases its reservation. Unrelated workspace entries and current targets remain untouched.

Armed or durable operations with no workspace body are cancelled only after verifying and synchronizing that absence. The current target is left for ordinary full reconciliation. A surviving body becomes protected under its original owner, bucket incarnation and root. Actual size is charged in full, including empty files and displaced bodies beyond the configured quota; an over-limit body is never evicted. Existing protected operations remain protected. Only registered conflict transfer or an explicitly confirmed operator discard can release them, through the existing conflict backend.

Recovery attempts the existing root/workspace/quota resume validation only after every pending operation has resolved. File, parent-sync or SQL failures retain the intent and stop until retry; a committed cancellation cannot release its charge twice after a lost response. Neither recovery nor its cancellation checks the current API generation to retarget retained data or alter API objects.

## Reusing a captured path

`RootLease::capture_path` can reuse a registered conflict copy when the catalog, root, logical target, bucket incarnation, original owner and full source observation still match. Both the opened source and owned read-only copy are checked with bounded incremental hashes, size, device, inode and nanosecond timestamps. Missing or changed copies and changed external files do not reuse a proof. Only the ephemeral proof binds to the current expected API generation; the retained record keeps its original observed generation and owner.

This avoids retaining the same external file repeatedly when an API generation changes just after capture and the runtime safely defers replacement. Direct `capture_conflict` calls still create independently requested retained copies. No schema, cache, dependency or broader deduplication service is added.

## Verification boundary

The focused tests cover real same-directory restarts, prepared short/zero/full copies, armed/durable absence and surviving bodies, exact quota and over-quota boundaries, a 1 TiB plus one-byte sparse body without reading it in full, unrelated workspace entries, current external targets, repeated recovery and exact quota release. A 257-operation test crosses the 256-entry page boundary. API change, object deletion and cross-project bucket recreation preserve the original retained owner, generation and incarnation. Unexpected unarmed removal bodies become protected, and workspace replacement blocks recovery without removing the original bytes. Live-export tests restart at prepare, copied, armed, installed, durable-state and acknowledged boundaries.

Recovery cancellation injects failures before and after unlink, parent synchronization and SQL completion. Paired catalog and actual working-tree Snapshot/PowerLoss seams reopen the captured catalog, verify API record/body/checksums, reactivate the original root and run recovery through quota release. A protected-state checkpoint separately retains bytes and full quota through restart, then resumes only after an exact, explicitly confirmed discard. The snapshots restore copied file contents into the original root/workspace directories, preserving their actual directory identities; production identity checks are unchanged. Existing live-export seams cover copying, file synchronization, installation and acknowledgement.

On 2026-10-31, the following native storage checks passed:

| Check | Result |
| --- | --- |
| `cargo check --locked --offline` and `cargo check --locked --offline --tests` | PASS; new recovery and capture test modules are connected |
| `cargo test --locked --offline --lib storage::catalog::sync_state::writeback -- --nocapture` | 32 passed, including 12 new recovery tests; no failures or skips |
| `cargo test --locked --offline --lib storage::catalog::sync_state::conflicts -- --nocapture` | 22 passed, including the prior 19 and 3 path-reuse tests; no failures or skips |
| Scoped `rustfmt --edition 2024 --config skip_children=true --check` and `git diff --check` | PASS |
| Cached markdownlint-cli2 0.24.0, markdownlint 0.42.0 | One file linted; zero reported issues |

Combined runtime checks remain the source owner's responsibility. After integration, the mirror unit group passed all 19 cases and `cargo xtask lint` reported zero production violations. Parent disappearance now treats descriptor-open `Io(NotFound)` as a path deferral, temporary permission repair uses an explicit retry barrier, and API-priority replacement verifies the original external bytes remain in the registered conflict store with their owner and observed generation. The separate race tests still preserve later edits. Final combined platform and distribution evidence is recorded by the runtime owners.

The seam model covers only instrumented operations, not every instruction or physical host power loss. Native tests do not establish Linux checked-exchange races, Windows filesystem behavior, final SDK/container behavior, or performance improvement. Sparse oversized files are tested by metadata inspection rather than snapshot materialization. The final runtime owners run heavy combined checks after source freeze.

## Sources

Recovery controls are emulator-local safety behavior. Google's [request preconditions](https://docs.cloud.google.com/storage/docs/request-preconditions) and [object holds](https://docs.cloud.google.com/storage/docs/object-holds), checked on 2026-10-31, remain the publication contract. The existing Discovery/protobuf pins in [directory synchronization](directory-synchronization.md#sources-and-verification-boundary) are unchanged. The comparison baseline remains fake-gcs-server v1.56.1, commit `896ece51e49eed0946089f3089f6343855c0d106`.
