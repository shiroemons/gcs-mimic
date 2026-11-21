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
   requires the tracked generation and an absent API winner. It does not wait
   for a complete baseline, which gates external deletions: a removal pending
   on it would itself keep the baseline from completing.
2. A copy step streams or clones an already opened immutable API descriptor into
   the private workspace, verifies its hashes and syncs the file and workspace.
   An identical observation needs no object-body handle or workspace copy.
   A retry empties the copy a failed attempt left and refills it in place, and a
   filesystem that cannot clone receives the copy in the entry created for the
   clone. Neither removes the entry and creates its name again: Docker Desktop
   shared folders can report a new inode number for a recreated name. A copy
   that another name also links is never emptied; that retry stops as Protected.
   A macOS clone keeps its source's mode, so the copy of a read-only blob cannot
   be opened for writing again. Only that retry removes the copy and clones the
   blob again, which keeps the mode the installed file gets. Docker Desktop's
   renumbering is what Linux containers see of host folders, not what a process
   on the macOS host sees. Linux creates each copy itself with mode 0o600.
3. Installation commits Armed before changing the namespace. It compares the
   private copy's complete stat identity with the staged one. When only the inode
   number differs, the same handle rehashes the copy, and matching hashes record
   the new identity for the rest of the operation. Any other difference, or
   different bytes, stops the operation as Protected and logs
   `sync_writeback_copy_changed` with the changed fields. When a step fails before
   this check, the operation still holds the identity recorded at staging. The
   check that decides whether the failure displaced the target applies the same
   rule, so the private copy under a new number stays an ordinary retryable
   failure, and any other body in its entry still stops the root as displaced.
   Re-reading the identity after hashing must again differ only in the inode
   number. A tracked body at the target is checked by size, mtime and hashes
   before the exchange. The exchange compares nothing: what it moved into the
   private entry is checked again by the same rule, which ignores the inode
   number and ctime, and unlinked only when it still holds the tracked body. A
   permission change or a new inode number between the check and the exchange
   thus completes the export, as restart recovery's rule does after a crash at
   that point; other bytes, or an entry that is not a regular file, stay
   protected. Descriptor-relative
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
A file that differs from its fingerprint only in the inode number is hashed the
same way, because Docker Desktop shared folders can report a new number for an
unchanged file; equal hashes record the observed number. When the API generation
moved on or the object was deleted before that acknowledgement, equal hashes
still show no external edit, whether the fingerprint was racy or named another
inode number. The pass then handles the file like an unchanged one: it neither
reports a conflict nor counts the file as deferred, and the export or removal of
the API change replaces it. Counting it as deferred would keep `sync wait`
reporting a changing source, and a finding that holds that export back would
leave the wait to time out instead of succeeding and counting the finding in
its answer.

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
obtained writeback handles. Callers wait for their turn and receive Busy past
`catalog_wait_timeout_ms`. After admission, owned tasks retain the execution gate,
workspace, root and catalog leases until file and SQL jobs drain, even if the
calling future is cancelled. SQLite transactions never wait for file I/O.
Administrative API reads remain available while a writeback job drains.

Restart cancels unarmed private API copies. Armed/Durable operations inspect
their owned workspace names. An Armed entry that no other name links and that
holds exactly the operation's API copy or the tracked body its rename displaced
or removed is unlinked and the operation cancelled; other bodies become
Protected. Recovery never changes a live target. Unlink, directory sync and
completion are separate durable boundaries, validated through process-crash and
`Fault::PowerLoss` snapshots.

Workspace replacement or disappearance persists an identity/recovery stop.
Other open failures retain their I/O cause and leave those flags unchanged;
descriptor pressure can therefore retry without requiring manual reinitialization.
Per-operation entry/target failure bits cannot be cleared by another operation's
successful completion. A full SQLite filesystem can prevent accounting updates;
the preceding intent and bytes remain until recovery succeeds after repair.

## Verification boundaries

Production-path tests cover unpublished cancellation, installed external edits,
0-byte/binary copies, failed acknowledgement transactions, stale API generations,
handles that a root switch forgot, removal before a complete baseline, workspace replacement and cancelled
jobs across separate handles. Recovery and conflict-transfer tests cover sparse
quota overflow, original-owner retention, multiple keyset pages, failed writes
and syncs, restart, and paired crash/power-loss snapshots. Schema tests retain
negative rollback, SQLite FULL and retry cases.

Copy-step tests run without Docker. A test seam records a staged inode number
that later observations do not repeat, which reproduces the Docker Desktop
report. With the seam, installation and obsolete cancellation complete when the
bytes are unchanged. A failure injected before installation confirms such a copy
is retried and installs it instead of stopping the root, while the original
target moved into the private entry is still reported as displaced. A copy
whose bytes changed stays Protected at both checks,
alone or together with other stat changes. A retry after a failed sync and a
failed write keeps the private copy's inode, and so does a sync that fails after
a refill. Crash and power-loss snapshots
taken while it refills the copy keep only synced bytes and recover to Ready.
A retry stops as Protected without emptying a copy that another name links. A
copy of a read-only blob is cloned again on macOS and refilled in place on
Linux, and the installed file keeps the mode a first copy gets.
With cloning disabled, the copy uses the entry created for the clone, and no
unlink runs. A test holds installation after its check of a tracked target and
before the exchange: a permission change or a same-bytes replacement under a new
inode number there completes the export, while other bytes of the same size and
mtime, or a symlink, stay in the private entry and stop the root.

Ingest and export tests record a tracked fingerprint under another inode number.
Unchanged bytes are hashed without staging and are not reported as a conflict
while a newer API generation awaits export, and that export replaces them.
Different bytes with the same size and mtime are still ingested, and the export
still reports them as a conflict. Wait tests record the fingerprint as stable,
under another inode number or racy: a wait over a newer API generation exports
it and is ready, one over an API deletion removes the file, and one whose export
a collision holds back reports that finding instead of timing out.

These are cooperative fault-model tests, not physical host-power-loss evidence.
`durable_rehash_rejects_same_inode_edits_with_restored_mtime` fails when private
copy hashes are reused at Durable and passes with installed-body verification.
Installation renames the private entry by name after confirming the copy
through an open handle, so a file put in that entry in between would be
installed. That window stays open; the Durable hash keeps such a file out of
tracking, which `durable_rehash_never_tracks_a_file_installed_in_place_of_the_copy`
checks with a same-size file in the target's place.
Only the acknowledgement's extra body hash is removed; checksum verification
at Durable remains mandatory.
See [continuous observation](continuous-directory-observation.md) and
[conflict recovery](sync-conflict-recovery.md) for management and platform scope.
