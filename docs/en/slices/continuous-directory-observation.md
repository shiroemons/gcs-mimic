# Continuous directory observation

Scope: continuous external create/edit observation within DESIGN-062,
DESIGN-072 and DESIGN-074. The related runtime now connects deletion approval,
conflict inspection/resolution and explicit strict management authority.
Parent feature status remains governed by the feature ledger.

## Behavior and ownership

An enabled sync directory now observes native notifications and sends every
notification through the existing complete bounded ingestion rescan. Linux uses
inotify directory watches; macOS uses one FSEvents stream for the root, including
per-file events so an in-place write is observable. Atomic file replacement,
new nested directories, renames, attribute changes and disappearance all cause
reconciliation. Disappearance alone does not authorize deleting API objects.

The existing supervised sync worker owns observation. It waits on native queue
readiness, catalog changes, explicit rescan or shutdown; no periodic scan or
idle polling timer is introduced. New watches are registered before scanning,
and the old queue stays alive until its replacement is complete. Content events
reuse the current watch set; structural changes rebuild Linux registrations.
A notification
that arrives during registration or reconciliation causes another pass. Linux
queue overflow and coalesced names follow the same full-rescan path rather than
attempting a partial replay of unreliable event names.

Registration traverses descriptors with no-follow operations on the bounded
file workers. A private DirectoryWatch retains a RootLease clone, closes all
watch descriptors before releasing it, and never exposes those descriptors.
Marker/root validation surrounds file jobs and rescans and precedes ingestion
publication. The existing generation/incarnation conditions, streaming hashes,
metadata inheritance and equal-hash suppression remain shared.

Linux admits at most 1022 watched directories and retains one inotify descriptor
per set; registration closes each temporary directory descriptor immediately.
macOS FSEvents uses a two-descriptor notification pipe rather than a descriptor
for each file, and requires a local filesystem. Traversal processes 256-entry
pages and caps depth and inspected entries instead of retaining a whole tree.
The operating system's actual descriptor limit still applies; the application
does not raise it. Exceeding capacity, unsupported watch setup or a permanently
unwatched subtree fails visibly and keeps readiness off.

Permission loss during re-registration preserves the old watches and retries
twice with exponential backoff. Persistent failure pauses intake and lowers
readiness while retaining the root and control queue. An authorized explicit
rescan after repair must complete registration and reconciliation before
restoring readiness. Shutdown interrupts backoff and releases the root.
Unsupported setup and capacity errors never produce a partially watched
healthy tree. The permission regression test repairs the tree at an explicit
retry barrier rather than racing a wall-clock timeout.

## Runtime controls and safety

`GET /_internal/sync` exposes the active catalog/root, runtime session, durable
findings revision, baseline, combined conflict/workspace quota and pending
deletion revision. The writeback summary omits the former always-null `journal_sequence`
and `journal_epoch`; the wait response's `journal_fence` carries its actual fence.
Revision-bound `view=findings` and `view=deletions` use keyset pages of at most
256 entries and 256 KiB. A terminal page has a null cursor;
mutation, foreign sessions and restart reject stale cursors with 409. Root
source findings cover unmappable entries without inventing a bucket/object or
returning host paths. A repaired complete scan clears that finding.

While the observer rebuilds deletion membership, status retains its last complete
deletion set instead of exposing an intermediate Ready state. The published value
is cloned under a short lock; readers never wait for the scan. Every read still
checks current management authority and the journal fence. Revision-bound pages
and approval use the current database, so a stale published revision cannot
authorize deletion. Safety findings, retained-workspace observations and their
revisions always refresh from the current transaction. Completion or failure
releases the previous deletion snapshot.

Native notifications also coalesce through a one-second quiet interval with a
ten-second cap. Further events restart quiet without starving control requests;
rescan queues until quiet or its cap, while status, approval and resolution
remain available. Approval and resolution return busy if the bounded control queue
is full. A capped scan starts another quiet interval and retains waiting
barriers; it does not establish quiet for wait or history reset. Wait reports
SourceChanging only if its own deadline arrives
while still settling. Schema 50 retains automatic
deletion history across scans and restart, so the absolute and percentage guard
cannot be reset by splitting one operation into smaller scans. During an operation
longer than the quiet cap, earlier deletions below the cumulative threshold may
already have committed; approval cannot undo them. A complete zero-candidate scan
after established quiet resets history, whether automatic or explicit. A capped,
interrupted or incomplete scan, or one with remaining candidates, cannot reset it.
The actor drains queued native notifications before starting and after completing
the scan. Reset requires the same event generation at both boundaries and no
pending notification; events read at either boundary remain queued for observation.
Reset failures retain history and log their cause while preserving the completed
wait or rescan response and the observer's running state.
The scan-continuity regression pauses after a zero-candidate scan, deletes the
remaining 60 of 120 original files, and waits for native queue readiness before
allowing reset. It retains the first 60 automatic deletions and guards the next
60 candidates. Removing the completion check fails deterministically with zero
retained deletions instead of 60. Separate cases cover a queued event with a
stale quiet flag, an unverified explicit rescan and reset failure with its cause
and both control responses intact.
See the
[deletion contract](directory-synchronization.md#external-deletion-and-approval).

Native macOS regression evidence covers a 50-object root deleting and recreating
20 objects six times, with a restart after the third cycle. Automatic native
scans clear history only after a complete zero-candidate quiet observation;
the test fails when the automatic reset is disabled. Separate tests retain
12/88 split-deletion approval across restart, preserve history for a capped
zero-candidate scan, and keep pending candidates from resetting history.
The racy-clean regression counts staging calls directly and requires zero, then
changes the body and requires one staging call as a positive control.
The single-request HTTP/CLI tests contain no SourceChanging retry loop.
The live-actor control test queues wait while settling and requires a stale
approval response within 500 ms while retaining the wait's reply.
Queued controls remain owned through observer failure and recovery. A queued
rescan can recover retained work instead of losing its response sender when a
background scan pauses; a retried worker reestablishes quiet for pending waits.

The live actor accepts bounded rescan, approval, wait and resolution commands.
Wait claims catalog activity before authorization's database wait. Authorization
and bounded request-body preparation share the configured catalog-wait budget;
after authorized input is parsed, the requested timeout bounds observation.
Permission checks still precede body validation. A request-scoped guard releases
activity on success, invalid input, preparation timeout or cancellation.
Descriptor exhaustion diagnostics include the current soft/hard descriptor limits
and a `ulimit -n` remedy without changing the process limits at startup.
Wait captures a source scan and finite journal fence; later unrelated API writes
do not extend it. Its explicit deadline is independent of connection inactivity
timeouts. A deadline interrupts settling and stops between bounded scan or
export steps. It does not mark an incomplete scan complete or approve deletion.
A waiting observer's timeout or disconnection does not cancel owned
file or catalog work. Persistent I/O, capacity and recovery stops keep inspection
and explicit disposal available. No idle retry loop runs while paused.

Retryable failures before displacement may retry twice. Once an installation
has displaced a body, a failure protects and fully charges any surviving
workspace entry, then stops that operation. Its old handle cannot perform
another namespace transition. Inspect and preserve the exact retained body,
then request an explicit rescan to use verified recovery/transfer and a fresh
plan. Shutdown releases ownership without requiring an unbounded cleanup retry.

`GET /_internal/sync/conflicts` lists retained revisions and quota.
`POST /_internal/sync/resolve` conditionally restores or explicitly discards an
exact registered conflict. Recovery targets require the inspected recovery
revision and opaque target identity plus `backup_verified` or
`discard_without_backup`; recovery restore is rejected. Fields of the other
target form must be absent, including null fields. Accepted operation IDs bind
the complete payload and durable result, so retry cannot publish twice or
change a previously selected action. See [the storage contract](sync-conflict-recovery.md).

Findings refresh protected workspace observations in bounded metadata-only
pages. Changed size/identity invalidates the findings and recovery revisions;
unverified hashes stay null. This does not hash a sparse oversized file or
pretend that a backup was verified by the server.

Strict startup requires the normal local identity registry. Every management
read and side effect requires explicit project-level `roles/storage.admin`
for the seed project and complete retained-owner union. Current buckets,
findings, pending deletion members, workspace operations, conflicts and
unreclaimed resolution intents participate. A bucket's disappearance never
changes the original retained owner. Reads check inside their snapshot;
publication and approval recheck inside the mutation transaction. Local
filesystem observation runs under the trusted startup configuration, and does
not grant the caller Google API access or import IAM permissions.

API-priority write-back first captures a divergent external body into the owned
immutable conflict store, then uses the exact captured proof for checked
replacement/removal, including an untracked conflict during initial activation.
A later edit cannot be erased with an older proof. A successful exact
acknowledgement clears only the resolved mapping finding; retained conflicts
remain inspectable until explicitly resolved. The
read-only export path preserves API holds and metadata; ordinary publication
and deletion keep their existing hold/retention checks. Missing parents defer
that path and allow unrelated buckets to progress.

## Acceptance and evidence

Native macOS tests cover nested in-place writes, atomic replacement, new
directories, delete notifications, symlink non-traversal, capacity refusal and
no readiness event during a quiet interval. The live ingestion test creates,
edits, replaces and renames files without requesting a manual rescan, verifies
new object generations, and preserves API state after filesystem disappearance.
The existing permissions test must still recover without advancing the export
cursor while permissions are denied; a separate test interrupts watch retry
through shutdown and verifies root release.

Native macOS acceptance includes 19 mirror cases and 11 HTTP control cases,
including strict authorization, finite waits and recovery projection. Six
import/sync integration targets passed all 48 tests. The final targeted
regression command passed four cases: post-displacement failure and repeated
old-handle rejection, the eight export failure phases with explicit recovery,
owner-union changes before recovery, and typed 403 mapping. The deleted-bucket
repair/rescan regression also passed on the original standard thread stack.
No stack-limit or streaming-buffer increase was used.

`cargo check --locked --offline --tests`,
`cargo clippy --locked --offline --lib --tests -- -D warnings`, scoped formatting
and Markdown lint passed. Whole-workspace, SDK and container verification belong
to the combined final integration gate. Linux runtime, Docker bind mounts,
native Windows support, exhaustive overflow injection and physical host
power-loss recovery remain unverified by this slice.
The code handles a reported overflow by rebuilding and scanning; that does not
prove every filesystem will deliver every notification.

No new dependency was added. The adapter uses the pinned libc and Tokio graph.
References: Apple's [FSEvents reference](https://developer.apple.com/documentation/coreservices/file_system_events)
and the Linux [inotify manual](https://man7.org/linux/man-pages/man7/inotify.7.html).
No resource benchmark or performance improvement is claimed.
