# Shared synchronization write-back

Scope: DESIGN-062, DESIGN-072, DESIGN-074 and AUDIT-014. Parent statuses remain
governed by the feature ledger. The runtime uses `begin_live_export`,
`advance_live_export` and `recover_pending`; tests exercise these same entry points.

## Durable export contract

`RootLease::writeback(Limits)` binds an opened workspace to its real device/inode.
Limits admit 1 byte through 1 TiB and 1 through 1,000,000 retained entries.
Recovery reads keyset pages of at most 256 operations and uses 128-bit aggregate
byte charges. Pending rows preserve the original owner, bucket incarnation,
generation, logical path, journal fence and opaque workspace name across bucket
deletion or recreation.

1. Begin records a Prepared intent and its reservation. The selected generation,
   tracked fingerprint, binding, authority and quota must still match. Removal
   requires a complete baseline and an absent API winner.
2. A copy step streams or clones an already opened immutable API descriptor into
   the private workspace, verifies its hashes and syncs the file and workspace.
   An identical observation needs no object-body handle or workspace copy.
3. Installation commits Armed before changing the namespace. Descriptor-relative
   traversal checks actual spelling, rejects aliases and symlinks, and checks the
   selected ancestors again. Installation syncs both affected parents. Reading or
   inspecting an existing parent never creates directories or calls fsync.
   Installation retries sync existing ancestors too: an earlier failed sync can
   leave a newly created ancestor visible without making its name durable.
   Consequently installation can sync two descriptors per ancestor even when
   they already exist; skipping these syncs requires durable provenance across
   failed retries. This cost has not been measured. Existing-only reads never sync.
4. The durable step hashes the installed target and records Durable. A writer
   can preserve its inode/size and restore mtime, while rename itself changes
   ctime, so the private copy's old hashes cannot prove the installed bytes.
   The acknowledgement compares the complete recorded stat
   identity, syncs the affected parents, atomically updates synchronization state
   and deletes the completed intent. A failed transaction changes neither tracking
   nor the pending intent. Completion does not maintain a separate journal cursor.

Fingerprint timestamps use the injected catalog clock. A stat observation within
two seconds of the latest mtime/ctime remains racy. Ingest hashes a matching racy
file in place before considering staging; equal hashes acknowledge the same
generation without copying it back to storage. A later stable observation clears
the racy flag. This interval conservatively covers filesystem timestamp granularity.

## Conflict, cancellation and recovery

Before installation, a changed API generation makes the export obsolete and a
new external target rejects the copy. Cancellation removes only the opaque
unpublished API copy and releases its reservation, preserving the external tree.
An installed or displaced body cannot use that cancellation path. Actual retained
workspace bytes are protected and charged in full, even beyond quota or for
sparse files. Protected bodies block new destructive work until explicit recovery
or resolution; no quota eviction or automatic rollback discards them.

API-priority replacement first captures a divergent external body in the owned
conflict store and uses its exact proof for the checked exchange. A later edit
cannot be erased with an older proof. Conflict registration and recovery retain
the original project, incarnation and generation, including after recreation.

A catalog-owned execution semaphore serializes transitions across independently
obtained writeback handles. Admission is bounded by the existing 32 waiters and
30-second Busy deadline. After admission, owned tasks retain the execution gate,
workspace, root and catalog leases until file and SQL jobs drain, even if the
calling future is cancelled. SQLite transactions never wait for file I/O.
Administrative API reads remain available while a writeback job drains.

Restart cancels only unarmed private API copies. Armed/Durable operations inspect
their owned workspace names; unexpected bodies become Protected. Recovery never
changes a live target. Unlink, directory sync and completion are separate durable
boundaries, validated through process-crash and `Fault::PowerLoss` snapshots.

Workspace replacement or disappearance persists an identity/recovery stop.
Other open failures retain their I/O cause and leave those flags unchanged;
descriptor pressure can therefore retry without requiring manual reinitialization.
Per-operation entry/target failure bits cannot be cleared by another operation's
successful completion. A full SQLite filesystem can prevent accounting updates;
the preceding intent and bytes remain until recovery succeeds after repair.

## Verification boundaries

Production-path tests cover unpublished cancellation, installed external edits,
0-byte/binary copies, failed acknowledgement transactions, stale API generations,
foreign handles, baseline-gated removal, workspace replacement and cancelled
jobs across separate handles. Recovery and conflict-transfer tests cover sparse
quota overflow, original-owner retention, multiple keyset pages, failed writes
and syncs, restart, and paired crash/power-loss snapshots. Schema tests retain
negative rollback, SQLite FULL and retry cases.

These are cooperative fault-model tests, not physical host-power-loss evidence.
`durable_rehash_rejects_same_inode_edits_with_restored_mtime` fails when private
copy hashes are reused at Durable and passes with installed-body verification.
Only the acknowledgement's extra body hash is removed; checksum verification
at Durable remains mandatory.
See [continuous observation](continuous-directory-observation.md) and
[conflict recovery](sync-conflict-recovery.md) for management and platform scope.
