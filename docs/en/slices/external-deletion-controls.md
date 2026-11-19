# External deletion and live controls

<!-- markdownlint-configure-file {"MD013": false} -->

This local slice extends DESIGN-072 and DESIGN-074. Their parent statuses remain
PARTIAL until combined acceptance is complete. The runtime now connects
revision-bound findings, finite waits, conflict resolution, native recovery and
strict startup. See [runtime observation](continuous-directory-observation.md),
[conflict storage](sync-conflict-recovery.md) and [CLI usage](sync-cli.md).

## Deletion inventory

A complete, error-free scan observes every entry, or at most a named `max_sync_entries`, in bounded pages.
Only representable tracked live generations that still match their bucket
incarnation contribute to the root-wide baseline. Ignored findings, stale API
generations and incomplete/unobserved state cannot become deletion candidates.
An incomplete scan invalidates its draft without deleting API data.

What holds back the baseline and every inferred deletion is decided by the side
of the problem. An entry in the tree that no object can take (an unsafe entry, a
name that maps to no object, an unreadable path, a path collision, or a file in
the directory of a bucket deleted through the API) may be where a missing file
went, so it stops the baseline and the inventory until it is gone. So do an
interrupted pass, deferred or conflicting files, and files no object accounts
for yet, such as the new name of a renamed file. The mirror
keeps the first 256 such paths and the count of the rest in memory; the control
status does not list them yet. A regular file directly in the root, such as a
`README.md` or a `.gitkeep`, belongs to no bucket: it is counted as ignored,
logged once as `sync_skipped` while it stays, and holds nothing back, as
seeding skips one. An ignored operating-system or editor name, such as
`.DS_Store`, skips only a regular file, anywhere in the tree; a symlink or
special file so named is an unsafe entry, and a directory so named is traversed.
Ingestion and reconciliation classify every entry through one function, so
they cannot disagree about which entries hold anything back. An
API object the tree cannot hold, such as a name the file system rejects or one
deeper than `max_sync_depth`, is a mapping finding. It has no file that could
look deleted, so it neither keeps the baseline open nor stops the deletion of
other files. A complete baseline also spares later passes the full
reconciliation that ran on each pass while it stayed open.

Each tracked path is looked at on its own without following links. A path that
tells nothing about its file (deeper than `max_sync_depth`, a name the file
system refuses, a non-directory above it, a differently spelled entry, an
unreadable path, or a directory or special file in the file's place) keeps its
own object out of the set, and the inventory goes on with the other paths. Each
complete inventory logs such paths in one `sync_deletion_unobservable` warning:
their count and the first ten, each with its reason and remedy. A directory or special file that
replaced a tracked file also stops that one path as `EntryReplaced`, in exports
and inventories alike, until the path is empty or holds a regular file again;
the root keeps running. An untracked one stays a mapping finding. The directory
of a bucket deleted through the API holds nothing back while it is empty or
holds only regular files with ignored names, which the same classification
decides, and is left in place; a directory or symlink with such a name holds it
back like any other entry.

With the baseline complete, a tracked file that is missing while its API
generation is still the one last synchronized was deleted from the tree, also
when that happened while the server was stopped or after it was killed.
Reconciliation leaves it to the next inventory instead of writing it back, and
the delete guard below is what stops a large deletion. It does so only for a
file the inventory looks at: an object with a finding, such as one kept out of
the tree while `max_sync_depth` was lower, is no candidate, so its missing file
is written back once the cause is gone, which clears the finding. One catalog
condition decides both. Every path that records a tracking row syncs the file
and each directory above it up to the root first: ingestion, including a file
whose bytes it only confirms, and write-back, including a file the user wrote
that an export only observes as identical. The row takes the proof of those
syncs, so a power loss never keeps a tracking row whose file it lost. A failed
sync records nothing; ingestion counts the file as deferred, or as unreadable
when the failure is one that waiting does not clear, such as a missing
permission, and retries on the next scan. Restoring the synchronization
directory to an earlier state, for example from a backup, makes the files
ingested after that state count as deleted; the delete guard stops a mass
deletion.

Schema 40 persists one fixed root-bound deletion revision and its members.
Candidates are paged in batches of at most 256. The denominator, count, captured
journal fence and thresholds stay fixed across pages. The guard stops deletion
when enabled, the candidate count is at least 100, and the fraction is strictly
greater than one half. It also stops it, whatever the counts, when a bucket's
whole directory is missing while the tree tracks files in it: a mount that did
not come up, or a directory moved away, looks the same as deleting it, and
putting it back brings every file back. Nothing recreates such a directory, at
startup or later, so the set waits for approval
(`sync_bucket_directory_missing` names the bucket): reconciliation leaves it
missing, and every export into the bucket, such as that of an object written
through the API while the server ran or was stopped, waits as deferred. Such an
export holds back neither the baseline nor the inventory, which offers the set
to approve, and while it waits, a wait and the status report the pending
deletion approval rather than a changing source. Approving the set deletes the files and then runs the waiting
exports; putting the directory back runs them on the next pass. A disabled
guard changes only these decisions.

A new set that waits for approval logs `sync_deletion_approval_required` once,
with its revision, its counts and the commands that inspect and approve it; an
inventory that confirms it under the same revision does not log it again, and
a restart, which never confirms an earlier set, logs the new one. Deletions that
run log `sync_deletions_applied` with their count and their first ten paths in
set order. The first pass of a run, after a start, a retry or a resumed rescan,
logs them as a warning, since it deletes what was deleted from the tree while
the mirror was not watching, such as while the server was stopped; later passes
and an approved set log them at the info level. The rescan result and the
`sync_ingested` and `sync_reconciled` events do not count deletions.

Observed tree changes, root/marker failures, API journal changes and restart
invalidate unused revisions. An API or bucket change during an inventory makes
it stale: it stops, logs `sync_deletion_inventory_superseded` at the info
level, and the next pass inventories again. A change that writes the
tree brings that pass through the native events of its own writes, which the
mirror does not filter out; a change to object metadata alone writes nothing, so
the missing files wait for the next change to the directory, a rescan or a wait.
Invalidating a set that is already invalid changes
nothing, so it starts no new observation revision. An inventory that finds
exactly the members of the root's unapproved set again, at the same journal
fence and guard setting, confirms that set under its revision instead of
replacing it, also after an observed change invalidated it. It never confirms an
approved set or one from before a restart. Approval consumes exactly one
revision, and replay or stale inspection returns 409. Each actual delete rechecks no-follow absence,
root identity, membership, generation and incarnation. Member completion and the
expected execution fence commit in the same transaction as the shared delete.
An injected SQL failure rolls both back. Concurrent filesystem creation remains
possible between the final absence check and catalog commit; it is never an
authorization to unlink the newly created file.

Deleting a bucket directory deletes its objects once the set is approved, as
above; the bucket stays and an empty directory is recreated. Versioned deletion retains
the selected generation as history.
The shared deletion path enforces holds and retention. A protected deletion
restores the immutable API bytes where safe without changing their metadata,
protection, generation or notification state. Its private read-copy mode checks
the selected winner, hashes, owner and incarnation, and installs only into an
absent target. A competing file stays untouched. Ordinary write-back arm and
acknowledgement retain their existing hold checks.

## Available controls

Controls use the live server outside Google API namespaces. They use a bounded
single-slot command queue behind which callers wait for their turn,
answer 503 once the mirror is closed or the wait gives up, cap JSON inputs at
64 KiB and responses at 256 KiB, and reject unknown fields, methods and query keys.

| Route | Current subset |
| --- | --- |
| `GET /_internal/sync` | `state` from a closed set that follows the mirror's own state (`Starting`, `Retrying` or `Stopped` while the mirror is so, its recorded pause reason or `Paused` while it is paused; never the server's readiness), `mirror_state` with the ready answer's `sync.state`, authentication profile and fixed deletion revision/count/fence. The [synchronization contract](directory-synchronization.md#stops-findings-and-remedies) lists the values. |
| `GET /_internal/sync?view=deletions&revision=...` | Fixed members with original owner, incarnation and generation; opaque bound cursors; no filesystem paths. Long names shorten a page to respect the byte cap. |
| `GET /_internal/sync?view=findings&revision=...` | Root source stops, mapping findings, protected paths, retained conflicts and pending recovery with exact target/revision and remedy; runtime-session-bound cursors. |
| `GET /_internal/sync/conflicts` | Original-owner retained revisions, metadata and combined quota with bounded listing and revision-bound continuation. |
| `POST /_internal/sync/rescan` | Empty JSON object; serialized complete observation with operation ID and reconciliation result. It grants no deletion override. A rescan that fails and leaves synchronization paused answers 409 with the `paused_reason`, the cause and the commands that resume it. |
| `POST /_internal/sync/approve` | Exact `deletion_set` revision, one-shot acceptance and count. Observer disconnect does not cancel the accepted operation. While synchronization is paused it answers 409 and asks for `gcs-mimic sync rescan` first. |
| `POST /_internal/sync/wait` | Empty object or explicit `timeout_ms`; captured source scan and finite journal fence, completed/timeout/stop result without cancelling owned side effects. |
| `POST /_internal/sync/resolve` | Exact registered-conflict restore/discard, or exact recovery identity with an explicit backup/disposal confirmation; durable operation-ID retries. |

Startup supports permissive and strict profiles. Strict mode requires the
normal local registry; missing registry configuration is still rejected.
The strict control authorization boundary requires a registered
bearer identity and an explicit project-level `roles/storage.admin` for the
seed project, current buckets and every original owner retained by unresolved
findings, pending write-back, conflicts, unreclaimed resolution operations and
deletion members. Signed queries, bucket IAM,
role combinations and the fixture-admin principal do not grant this authority.
Authorization precedes body parsing and is checked again inside each read
snapshot and the native mutation transaction, including accepted retries.

Finding upserts retain original owners per incarnation until the finding is
resolved. Deleting the last bucket or recreating its name under another project
does not substitute a new owner. Migration backfills only a proven matching
incarnation; unknown legacy owners remain null and fail closed. Upload identity
snapshots never restore explicit sync-administrator roles.

## Verification and limits

The native HTTP control test passes through the running server. Scoped library
tests cover 300 fixed candidates, multi-page long names within the byte cap,
stale cursors/approval, disabled/threshold boundary decisions, API interference,
recreated files, source symlinks and marker failure, protected restoration,
versioned history, transactional rollback and owner retention across restart.
Library tests also cover an object the tree cannot hold beside an external
deletion, an unsafe entry that stops deletions until it is removed, a tracked
file replaced by a directory, a lowered `max_sync_depth`, an unreadable tracked
path beside a deleted file, and the reuse rules of an identical inventory. They
hold an API write into a bucket whose directory is missing until approval, at
startup and while running, with repeated waits and the status reporting that
approval, and cut power after write-back observed a file whose
ingestion sync had failed. An integration test deletes files while the server
is stopped and after it is killed.
The ordinary hold-negative write-back test remains enabled and passes.

The read-copy tests pair a catalog crash snapshot with a tree snapshot at file
sync, copy durability, installed namespace durability and acknowledgement. Both
`Fault::Snapshot` and `Fault::PowerLoss` are exercised at all four boundaries.
Before acknowledgement, reopening preserves API bytes/metadata and stops
unfinished write-back; committed copies retain their full bytes. Failed writes
and syncs do not acknowledge progress, and a new external target is preserved.

These are cooperative snapshots of the seamed boundaries, not physical host
power-loss tests or every-instruction coverage inside the existing filesystem
helpers. Restart recovers owned pending work before new intake; retained bodies
remain charged until verified conflict transfer or an exact confirmed recovery
discard durably completes. The ephemeral read-copy capability alone never
authorizes deletion of an unknown retained body. Linux runtime, bind-mount and exhaustive kernel overflow coverage remain
final integration work. No benchmark or performance improvement is claimed.
