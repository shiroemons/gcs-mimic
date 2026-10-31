# External deletion and live controls

<!-- markdownlint-configure-file {"MD013": false} -->

This local slice extends DESIGN-072 and DESIGN-074. Their parent statuses remain
PARTIAL until combined acceptance is complete. The runtime now connects
revision-bound findings, finite waits, conflict resolution, native recovery and
strict startup. See [runtime observation](continuous-directory-observation.md),
[conflict storage](sync-conflict-recovery.md) and [CLI usage](sync-cli.md).

## Deletion inventory

A complete, error-free scan observes at most 10,000 entries in bounded pages.
Only representable tracked live generations that still match their bucket
incarnation contribute to the root-wide baseline. Ignored findings, stale API
generations and incomplete/unobserved state cannot become deletion candidates.
An incomplete scan invalidates its draft without deleting API data.

Schema 40 persists one fixed root-bound deletion revision and its members.
Candidates are paged in batches of at most 256. The denominator, count, captured
journal fence and thresholds stay fixed across pages. The guard stops deletion
when enabled, the candidate count is at least 100, and the fraction is strictly
greater than one half. A disabled guard changes only that threshold decision.

Observed tree changes, root/marker failures, API journal changes and restart
invalidate unused revisions. Approval consumes exactly one revision, and replay
or stale inspection returns 409. Each actual delete rechecks no-follow absence,
root identity, membership, generation and incarnation. Member completion and the
expected execution fence commit in the same transaction as the shared delete.
An injected SQL failure rolls both back. Concurrent filesystem creation remains
possible between the final absence check and catalog commit; it is never an
authorization to unlink the newly created file.

Deleting a bucket directory preserves the bucket and recreates an empty
directory. Versioned deletion retains the selected generation as history.
The shared deletion path enforces holds and retention. A protected deletion
restores the immutable API bytes where safe without changing their metadata,
protection, generation or notification state. Its private read-copy mode checks
the selected winner, hashes, owner and incarnation, and installs only into an
absent target. A competing file stays untouched. Ordinary write-back arm and
acknowledgement retain their existing hold checks.

## Available controls

Controls use the live server outside Google API namespaces. They use a bounded
single-slot command queue and at most 32 active observers, fail immediately with
503 when busy/closed, cap JSON inputs at 64 KiB and responses at 256 KiB, and
reject unknown fields, methods and query keys.

| Route | Current subset |
| --- | --- |
| `GET /_internal/sync` | Readiness-aware state, authentication profile and fixed deletion revision/count/fence. |
| `GET /_internal/sync?view=deletions&revision=...` | Fixed members with original owner, incarnation and generation; opaque bound cursors; no filesystem paths. Long names shorten a page to respect the byte cap. |
| `GET /_internal/sync?view=findings&revision=...` | Root source stops, mapping findings, protected paths, retained conflicts and pending recovery with exact target/revision and remedy; runtime-session-bound cursors. |
| `GET /_internal/sync/conflicts` | Original-owner retained revisions, metadata and combined quota with bounded listing and revision-bound continuation. |
| `POST /_internal/sync/rescan` | Empty JSON object; serialized complete observation with operation ID and reconciliation result. It grants no deletion override. |
| `POST /_internal/sync/approve` | Exact `deletion_set` revision, one-shot acceptance and count. Observer disconnect does not cancel the accepted operation. |
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
