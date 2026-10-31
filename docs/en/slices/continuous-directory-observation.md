# Continuous directory observation

Scope: continuous external create/edit observation within DESIGN-062,
DESIGN-072 and DESIGN-074. The related runtime now connects deletion approval,
conflict inspection/resolution and explicit strict management authority.
Parent feature status remains governed by the feature ledger.

## Behavior and ownership

An enabled sync directory now observes native notifications and sends every
notification through the existing complete bounded ingestion rescan. Linux uses
inotify directory watches; macOS uses kqueue vnode watches for directories and
regular files so an in-place write is observable. Atomic file replacement,
new nested directories, renames, attribute changes and disappearance all cause
reconciliation. Disappearance alone does not authorize deleting API objects.

The existing supervised sync worker owns observation. It waits on native queue
readiness, catalog changes, explicit rescan or shutdown; no periodic scan or
idle polling timer is introduced. New watches are registered before scanning,
and the old queue stays alive until its replacement is complete. A notification
that arrives during registration or reconciliation causes another pass. Linux
queue overflow and coalesced names follow the same full-rescan path rather than
attempting a partial replay of unreliable event names.

Registration traverses descriptors with no-follow operations on the bounded
file workers. A private DirectoryWatch retains a RootLease clone, closes all
watch descriptors before releasing it, and never exposes those descriptors.
Marker/root validation surrounds file jobs and rescans and precedes ingestion
publication. The existing generation/incarnation conditions, streaming hashes,
metadata inheritance and equal-hash suppression remain shared.

Each watch set admits at most 1022 entries: directories on Linux and both
directories and regular files on macOS. Traversal reuses those descriptors
instead of cloning a separate directory stack. Even while old and new sets
overlap, watchers use at most 2048 descriptors, including queues and a candidate.
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
deletion revision. Revision-bound `view=findings` and `view=deletions` use keyset
pages of at most 256 entries and 256 KiB. A terminal page has a null cursor;
mutation, foreign sessions and restart reject stale cursors with 409. Root
source findings cover unmappable entries without inventing a bucket/object or
returning host paths. A repaired complete scan clears that finding.

The live actor accepts bounded rescan, approval, wait and resolution commands.
Wait captures a source scan and finite journal fence; later unrelated API writes
do not extend it. Its explicit deadline is independent of connection inactivity
timeouts. A waiting observer's timeout or disconnection does not cancel owned
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
References: Apple's [kqueue manual](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/kqueue.2.html)
and the Linux [inotify manual](https://man7.org/linux/man-pages/man7/inotify.7.html).
No resource benchmark or performance improvement is claimed.
