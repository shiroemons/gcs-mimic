# Directory synchronization contract

<!-- markdownlint-configure-file {"MD013": false} -->

Contract updated: **2026-10-25**. Scope: DESIGN-062, DESIGN-072 and DESIGN-074
remain PARTIAL; DESIGN-076 remains IMPLEMENTED. The initial-export increment
accepts the settings below, activates the root before bind, and exports live
objects once after ready. Continuous bidirectional synchronization is PENDING.

## Local-development scope

This table selects the practical local-development subset for subsequent
slices; broader safety targets remain explicit PENDING work. The initial-export
increment changes only configuration, activation and the export side.

| Acceptance case | Local-development subset | Deferred scope (PENDING) |
| --- | --- | --- |
| sync-configuration | Only root selection and the boolean deletion guard are configurable; verify precedence, boolean parsing, removed-key rejection, overlap and CLI-only reinitialization. | The fixed settle/deletion policy requires ingestion. |
| sync-initial-activation | Export all owners, empty buckets and immutable live bodies; count and preserve divergent/external-only files. | Initial ingestion and conflict transfer. |
| sync-name-mapping | Exact lexical mapping, actual directory spelling, host component limits, prefix/type collisions and no-follow checks. | Exhaustive case/Unicode alias groups and non-Linux platform conflict guarantees. |
| sync-lexical-name-mapping | Existing exact round-trip/property coverage remains implemented. | Runtime concerns belong to sync-name-mapping. |
| sync-bidirectional-mutations | Preserve API state and external bytes during the initial export. | Continuous API create/overwrite/delete/copy/compose and external mutations. |
| sync-api-writeback | One post-ready paged all-owner export using descriptor copies, no-replace publication and durable receipts; ordinary API/external races defer the affected object without stranding unpublished work. | Journal replay/coalescing/gap recovery, replacements and disk-full retries. |
| sync-metadata-and-loops | Export never mutates API metadata or generations. | Ingestion metadata inheritance, MIME and loop suppression. |
| sync-conflict-preservation | Leave divergent files untouched; ambiguous publication retains its pending intent/workspace. | Conflict transfer/registration, ordinary quota recovery and 1 TiB sparse-body cases. |
| sync-deletion-approval | Infer no external deletion. | Candidate inspection, threshold effects and approval. |
| sync-management-authorization | Configuration is a trusted local operator action. | Sync controls and detailed original-owner strict authorization. |
| sync-status-and-wait | Emit initial exported/identical/conflicting/external_only/ignored/unrepresentable counts. | Control API/CLI, paged status, readiness summary and waits. |
| sync-bounded-observation | Dedicated OS orchestration/file workers and at-most-256-entry catalog/directory pages. | Watcher events, stat-first rescans and bind-mount coverage. |
| sync-marker-recovery | Missing/foreign/malformed markers fail before bind; exact-ID missing-marker reinitialization. | Exhaustive process-kill/host-power-loss boundaries. |
| sync-writeback-recovery | Shared receipt and pending-state protection is reused; no destructive replacement is attempted. | Automated recovery, disk-full retries, every-instruction kill coverage, sparse limits and non-Linux conflict guarantees. |
| sync-shutdown-lock | Stop between objects, drain the active operation before catalog close, and retain root ownership through that drain; SIGTERM/restart and listener-error ordering are tested. | Exhaustive scan/publication/conflict cancellation and lock-boundary coverage. |

Initial export does not choose a winner by overwriting an existing target.
Filesystem aliases detected by exact directory spelling or no-replace failure
leave existing bytes intact. A case/Unicode alias discovered after a preceding
catalog member was exported can leave that first file in place; classifying all
members before any export is PENDING. Legacy buckets rejected by the native
typed bucket API are counted as unrepresentable and skipped; per-object durable
findings for those legacy buckets require a later shared typed interface.
Directories are capped at 10,000 inspected entries, pages at 256, traversal at
128 levels and individual components at 255 bytes. Exceeding an inventory bound
stops reconciliation without inferring deletion. No catalog-sized in-memory
inventory is built. Actual Linux runtime and non-Linux conflict guarantees are
PENDING; the current native verification runs on macOS without claiming parity.

Unfinished write-back stops new exports. Until recovery controls are delivered,
preserve/back up the working tree and reserved workspace, correct the mount or
permissions, and select a fresh unassociated root if recovery is required. No
pending body is automatically deleted. A changed journal fence, divergent file,
external-only file or unrepresentable entry prevents initial baseline completion.

Ordinary shutdown first stops accepting connections and drains HTTP requests,
then signals the mirror and waits for its current object to finish before closing
the catalog. The mirror checks shutdown before the next prepare and never drops
an armed operation to satisfy a network deadline. Background drain has no deadline,
matching the existing catalog-owned filesystem drain contract.

The source generation is opened before preparing an intent. A disappeared or
changed generation is deferred, and `deferred` counts also prevent baseline
completion. API copies are staged under durable Prepared intent before arm;
staging does not touch the target. Private copy provenance permits abandonment
only when the copy was never installed and its workspace inode/body remain
unchanged, or when an identical-file observation made no copy. Existing external
targets are never unlinked. Installed copies cannot be abandoned. If API state
changes after installation, an additional durable check can mark that obsolete
operation Complete without a synchronized fingerprint or journal progress;
the installed file remains available for the next reconciliation. Changed or
displaced external bodies still require protected recovery, not cancellation.
External deletion/edit/touch between rename and acknowledgement can make both recovery paths refuse completion, retaining an Armed/Durable root stop; the remedy belongs to sync-conflicts (PENDING).
An obsolete Complete counts as `deferred` in that pass and leaves baseline incomplete; the next reconciliation classifies retained output as Conflicting, or `external_only` after API deletion.

### Initial-export verification

Verified on 2026-10-25, macOS ARM64, Rust 1.99.1 against the two-setting configuration.
The startup-only marker diagnostic also explains accidental Git/copy transport,
`.gitignore`, preserving the old tree, and selecting the intended `data_dir`.

| Command | Result |
| --- | --- |
| `cargo test --locked --offline --test sync_mirror --test sync_configuration` | PASS: 10 tests; `sync_settings_precedence_defaults_boolean_and_startup_only_reinitialization`, `sync_removed_settings_and_guard_without_root_fail_with_diagnostics` and `sync_overlaps_aliases_empty_and_symlink_roots_are_rejected` cover both keys, all removed-key input paths, boolean errors and root validation; the 7 mirror tests preserve pagination, all owners, bodies, findings, marker diagnostics and SIGTERM/restart. |
| `cargo test --locked --offline --lib -- writeback_abandon export_concurrent_api listener_failure_drains` | PASS: 4 tests; deterministic API overwrite/delete before open/arm and after publication, external same-name creation and identical-file editing leave no permanent unpublished stop; installed/modified bodies reject abandonment, obsolete completion requires changed API state, and listener failure drains before catalog close. |
| `just check` | PASS on final source: format, workspace clippy/tests/doc tests, compatibility/generated views and shell checks. |
| `cargo xtask compatibility render` / `check` | PASS: English/Japanese views remain byte-identical because parent statuses/fidelity remain unchanged. Scoped passing cases and their evidence are recorded in the ledger. |
| `npx --yes markdownlint-cli2 docs/en/slices/directory-synchronization.md` | PASS; the existing MD013 exception permits long contract tables. |

At initial-export slice start, dependency freshness confirmed Rust 1.99.1 and all direct Rust dependencies
CURRENT; the overall check stopped with exit 2 on the Go proxy's new-major probe:
`curl: (28) Operation timed out after 30003 milliseconds with 0 bytes received`
for `https://proxy.golang.org/google.golang.org/api/v2/@v/list`.
The full freshness check is therefore not a PASS. No dependency/lockfile changed.

No SDK, container, full-guide or benchmark execution was performed: Google HTTP
behavior, container inputs and runnable guides are unchanged. Linux runtime,
exhaustive aliases/stale-finding cleanup, interruption recovery and the deferred
items in Local-development scope remain PENDING. No performance claim is made.

## Configuration reservation

These flat JSON keys reserve matching CLI/environment names: replace `_` with
`-` and prefix `--` for CLI, uppercase and prefix `GCS_MIMIC_` for environment.
Use CLI > environment > file > default. Selected values follow the existing
configuration parser; malformed source types are diagnosed even when overridden.
The deletion guard accepts only `true` or `false`.
Collect diagnostics by original source/key with a concrete fix before mutation.

| JSON key | Default | Validation and effect |
| --- | --- | --- |
| `sync_dir` | Unset: disabled | Nonempty existing writable directory, not a symlink. This is the only required setting to enable sync. Canonicalize and reject equality or ancestor/descendant overlap with `data_dir` or `seed_dir`, including aliases. Reserve and revalidate the root identity rather than following a replaced root. |
| `sync_delete_guard` | `true` | `false` disables only the count/percentage guard. Marker, complete-scan, incarnation and generation protections remain mandatory. |

Local development rarely needs tuning, so the internal policy is fixed at a
1000 ms quiet deadline, a deletion guard for at least 100 objects and strictly
more than 50%, and retained/in-progress conflict limits of 1 GiB and 1000 entries.
Zero-byte conflicts still consume an entry; no automatic eviction is permitted.
Settling is a stability heuristic: stat before/after staging and retry changed
input, rather than claiming an atomic snapshot of an active writer.
This owner decision replaces section 7's provisional tuning recommendations
to keep the local-development configuration small.

`sync_settle_ms`, `sync_delete_min_objects`, `sync_delete_percent`,
`sync_conflict_max_bytes` and `sync_conflict_max_entries` are not settings.
Their CLI, environment and JSON spellings are rejected as unknown settings,
including when a higher-precedence valid setting is supplied.
Supplying `sync_delete_guard` without `sync_dir` is an error, with a fix to supply
the root or remove the guard setting. The guard alone never enables sync.
`seed_project` remains the existing default `local-project` and owns new external
bucket directories. Existing bucket owners and settings are preserved. New-file
MIME follows `seed_content_type` and its pinned table in the
[source storage contract](source-storage-contract.md#seed-profile-and-mime-decision),
defaulting to `application/octet-stream`; do not consult host MIME databases.
`sync-ingest` depends on the MIME component when the extension profile is selected;
until available, reject that combination rather than accepting an ineffective setting.

The startup-only **`--sync-reinitialize CATALOG_ID`** has no environment or file
equivalent. It requires `sync_dir` and the exact currently locked catalog ID,
is never persisted as configuration, and requests the safe reactivation below.
It is not a reset, deletion approval or permission to overwrite foreign markers.

## Acceptance cases

### Initial export delivery scope

The first mirror increment implements configuration, pre-bind root activation
and a single post-ready all-owner export. Before implementation, its acceptance
cases are: exact two-setting precedence, boolean parsing and removed-key rejection, startup-only reinitialization,
overlap rejection, fail-closed marker diagnostics, empty bucket creation,
paged binary/empty-body export from immutable descriptors through durable
write-back receipts, runtime mapping findings, and exact summary counts.
Existing divergent and external-only files remain untouched and keep the
baseline activating. A scan interrupted by API mutations cannot complete the
baseline. No external deletion is inferred.

Continuous journal consumption, conflict registration/transfer, disk-full retry,
sync control API/CLI and filesystem ingestion remain PENDING. The boolean guard
is validated now; fixed settle/delete behavior belongs to the pending ingestion
scope. Fixed conflict limits already constrain shared write-back reservations.

Each ID below is a row-local case in the named feature. These specify future
executable tests, not results obtained by inspecting this document.

| Case | Feature | Required positive, negative and boundary behavior | Owning slice |
| --- | --- | --- | --- |
| sync-configuration | DESIGN-062 | Only sync_dir and sync_delete_guard; defaults and CLI/environment/file precedence, strict boolean parsing, removed tuning keys rejected as unknown, guard without root, overlap/alias/root substitution rejection and startup-only reinitialization; internal values fixed for local development | sync-mirror, sync-ingest |
| sync-initial-activation | DESIGN-062 | Empty/nonempty catalogs, empty buckets, same/different/absent names, exact summary counts; interrupted activation resumes without replay/deletion; owner mismatch never changes ownership | sync-mirror, sync-ingest |
| sync-name-mapping | DESIGN-072 | Property-test name to path to identical name or explicit Unrepresentable; literal percent/backslash rules, dot/empty segments, ignored/reserved names, OS limits, case/Unicode aliases and file/directory collisions; no traversal, symlink or special-file following | sync-mirror, sync-ingest |
| sync-lexical-name-mapping | DESIGN-072 | Shared lexical name/path conversion rejects invalid, reserved, ignored and host-invalid components and preserves exact UTF-8 bytes on round trip. Property tests cover arbitrary names and paths. This is a candidate-path check; filesystem collisions, host length limits and descriptor-relative safety still require runtime validation. | sync-mirror |
| sync-bidirectional-mutations | DESIGN-072 | Multiple buckets; API create/overwrite/delete/copy/compose, external create/edit/delete/rename, independent bucket settings/history and zero-byte/binary bodies; deleted bucket directory returns without deleting the bucket | sync-mirror, sync-ingest |
| sync-api-writeback | DESIGN-072 | After ready, page all owners' buckets and live objects, mirror binary/empty bodies from opened immutable descriptors, consume fenced changes without idle polling, preserve conflicting external bytes and owner/settings/history, recreate empty bucket directories, and recover a gap through bounded reconciliation. Pending the shared interfaces below. | sync-mirror |
| sync-metadata-and-loops | DESIGN-072 | Atomic metadata inheritance including concurrent PATCH, new-file MIME/default holds, same-hash no new generation, live-hash recovery and origin/fingerprint loop suppression | sync-ingest |
| sync-conflict-preservation | DESIGN-072 | API priority, immutable conflict body/identity, exact/exceeded byte and entry caps, disk-full and zero-byte conflicts; a quota-exceeding last-check race after displacement retains the actual body in workspace with full quota accounting, stop/restart and operator recovery, never automatic cleanup/exchange-back; even a 1 TiB + 1 byte sparse body can be explicitly discarded before registration only with original-owner authority, exact recovery revision/identity and backup or no-backup discard confirmation; rejected/stale/unconfirmed requests retain bytes and quota, and only durable authorized completion permits root resumption; restore/discard cannot retarget or overwrite newer API state; cancellation/retry retains unresolved bytes and quota | sync-mirror, sync-control, sync-ingest |
| sync-deletion-approval | DESIGN-072 | Count below/at threshold and fraction equal/above threshold, disabled guard and zero denominator; inspect all pages of one fixed deletion revision and approve exactly that set; an intervening tree/API/root/incarnation change or restart makes old cursors and approval return 409; holds restore files and preserve history | sync-control, sync-ingest |
| sync-management-authorization | DESIGN-072 | Every control route and CLI operation in both profiles; strict missing/invalid/insufficient/project-incomplete identities denied before disclosure or mutation, including list/resolve 403 without the original conflict/recovery owner project's grant before/after its last bucket deletion, restart and cross-project same-name recreation; pre-registration recovery discard also rejects stale revision/identity or missing confirmation without changing retained bytes; recheck authority/target on execution and restart retries, only authorized confirmed completion can release the recovery stop; signed/public/bucket-only access does not authorize administration | sync-control |
| sync-status-and-wait | DESIGN-072 | All stop remedies; bounded fixed-revision status pages expose exact deletion candidates and Unrepresentable/ProtectedDeletion/recovery targets, fields and remedies; multi-page inspection matches approval, changed state rejects stale cursors/approval with 409; sanitized readiness, captured scan/journal fence, timeout/disconnect/shutdown without cancelling side effects; stopped work stays inspectable | sync-control |
| sync-bounded-observation | DESIGN-072 | Dedicated OS workers, bounded pages/path queue/diagnostics, fingerprint/racy hashing, journal coalescing/gap, duplicate/self/missed/overflow events, no idle polling, unchanged-file stat-only path and bind-mount behavior | sync-watcher, sync-ingest |
| sync-marker-recovery | DESIGN-074 | Missing/foreign/malformed marker and wrong mount stop before bind; correct-root recovery and safe reinitialization, interrupted marker/state persistence and restart cannot interpret unobserved files as deletions | sync-mirror, sync-recovery |
| sync-writeback-recovery | DESIGN-074 | Barrier-kill copy/sync/exchange/parent-sync/state windows; Linux displaced-inode and absent-target races, including exact/exceeded byte/entry quotas, a 1 TiB + 1 byte sparse displaced body and ENOSPC before conflict registration plus kill/restart while stranded in workspace; retain/protect/account bytes, stop and recover without automatic cleanup/exchange-back; authorized confirmed pre-registration discard survives kill before/after durable intent, unlink, parent sync and completion, with idempotent retries and no premature quota/stop release; rejected authority/revision/identity/confirmation retains the body; other-OS race diagnostics/limits and retry; opened blobs stay immutable/readable and catalog microseconds remain intact | sync-mirror, sync-recovery |
| sync-shutdown-lock | DESIGN-074 | Shutdown during scan, staging, publication, conflict resolution and write-back drains owned side effects, cleans safe temporary files and retains catalog/root ownership until completion; second server cannot acquire either active root | sync-recovery with Shared state |

DESIGN-076 keeps its existing cases and IMPLEMENTED status unchanged. Its lock
regression requirements above belong to the new pending synchronization scope.

The delivery gate's contract condition is addressed by Configuration through
Stops below; its mutation condition maps to `sync-initial-activation`,
`sync-bidirectional-mutations` and `sync-metadata-and-loops`; its observation/
recovery condition maps to the remaining safety, control and recovery cases.
The five unchanged `filesystem_sync_contract.acceptance_targets` map as follows:

| Target | Cases |
| --- | --- |
| Bidirectional mutations and independent settings | sync-initial-activation, sync-bidirectional-mutations, sync-metadata-and-loops |
| Editor/partial/self/duplicate/missed changes | sync-name-mapping, sync-metadata-and-loops, sync-bounded-observation |
| Conflicts, failures and retained history | sync-conflict-preservation, sync-deletion-approval, sync-management-authorization, sync-status-and-wait |
| Portable mapping, collisions and symlinks | sync-configuration, sync-name-mapping |
| Bounded rescan and restart/recovery | sync-bounded-observation, sync-marker-recovery, sync-writeback-recovery, sync-shutdown-lock |

## Name and path reversibility

`PathMapping` is either `File(relative_path)` or `Unrepresentable(reason)`.
Apply existing bucket/object validators first, then this mapping. Paths contain
one literal UTF-8 bucket component followed by object components split on `/`.
No percent decoding/encoding, separator cleaning, case folding, Unicode
normalization, escaping or truncation is allowed. `%2F` names a literal `%2F`
file component. `/` is the logical separator on every OS. A backslash is literal
on Unix and unrepresentable on Windows; it never acts as an implicit separator.
Do not change the API's wider accepted-name set to fit a filesystem.

| Reason | Cases and behavior |
| --- | --- |
| `InvalidName` | Invalid UTF-8 on disk, invalid API name, NUL, leading/trailing `/`, empty component, `.` or `..` component; report, do not clean into another name. |
| `HostRestriction` | Host-invalid characters, Windows device names/drive/UNC syntax, trailing spaces/dots where not reversible, component/path length limits; retain API state, report the exact restriction. |
| `Reserved` | Root marker `.gcs-mimic-sync.json`, workspace `.gcs-mimic-sync-work` and either reserved component at any object depth; reserve root names against legacy bucket names too. |
| `Ignored` | Regular files named `.DS_Store` or `4913`, beginning `._`, ending `.swp` or `~`. Use one shared predicate for ingestion and API write-back. Traverse ordinary matching directories such as `._assets/`; they are not ignored files. |
| `Collision` | `a` and `a/b`, aliases differing only by case or Unicode normalization on the actual filesystem, bucket aliases, or a conflicting directory/file type. Report all members, never choose one silently or delete either API object. |
| `UnsafeEntry` | Symlink, junction/reparse redirect, special file, or changed ancestor/root identity. Never follow it or infer deletion from unreadable/unsafe traversal. |

Reserved workspace subtrees alone are excluded as directories. An API regular
object whose final component matches an ignored-file rule is `Ignored`, even
if present from old seed input. A matching directory in a prefix is traversed;
its child may be representable. Two API names causing a file/directory collision
are both unrepresentable until the collision is removed. Detect aliases before
writing, including an existing external alias, and revoke any previously mapped
member without treating its disappearance as an external deletion. Existing
files in an ambiguous group remain untouched pending operator resolution.

For every `File(p)`, decoding that exact relative path must reproduce the same
bucket and object bytes. Filesystem normalization must preserve this property;
otherwise return Unrepresentable. Test the actual root's behavior using bounded
private workspace probes; do not assume all macOS or Windows volumes behave alike.
Perform descriptor-relative operations with no-follow ancestor checks. The root
and ancestors are trusted operator inputs but replacement must be detected;
canonicalization once at startup is insufficient against later substitution.
Unrepresentable API objects remain available through the API and are excluded
from deletion candidates. Unsupported external entries are findings, never imports.

## Activation, marker and startup

### Shared interface prerequisites for write-back

Shared state owns the delivered interfaces below. The initial exporter consumes
them without SQL or independent catalog identity/progress files.

| Interface shape | Required properties and acceptance handoff |
| --- | --- |
| `Admin::list_all_buckets(after, limit)` | Initial export consumes all-owner keyset pages of at most 256, including empty buckets and stored owner/incarnation. `seed_project` does not filter the inventory. Shared tests: [shared-sync-observation](shared-sync-observation.md). |
| `Catalog::wait_for_changes(after)` | Idle runtime ownership waits without catalog permits or polling, and releases the root when catalog shutdown wakes it. This wait does not export changes or advance synchronization progress; journal replay is PENDING. Shared finite-fence/race tests: [shared-sync-observation](shared-sync-observation.md). |
| Durable catalog identity and root association | Startup invokes `SyncState::activate_root` after seed and before bind. Reconciliation uses owned file jobs and calls `complete_baseline` only for an unchanged, resolved scan. Shared recovery contract: [shared-sync-root](shared-sync-root.md). |
| Write-back progress and pending-operation API | Initial export uses prepare_publish_copy/arm/durable/acknowledge/resume for each absent or identical target. Private provenance permits safe unpublished abandonment; obsolete installed copies complete without fingerprints/progress. It commits no journal progress and preserves protected operations. Replacement, conflict registration and recovery controls are PENDING. Shared receipt contract: [shared-sync-writeback](shared-sync-writeback.md). |
| `ObjectReader` descriptor transfer or duplication | Export transfers the opened immutable selected generation with `into_file` to the owned workspace copy job; no source path reopening or hard link. Shared descriptor tests: [shared-sync-writeback](shared-sync-writeback.md). |
| Workspace-to-target publication | Export uses descriptor-relative no-replace publication and shared file/parent durability checks before receipt acknowledgement. Ancestors are reopened and compared after installation. Exchange/conflict transfer is PENDING. Shared interfaces: [shared-sync-export-interfaces](shared-sync-export-interfaces.md). |
| Unrepresentable/ignored observations | Initial export registers generation/incarnation-bound findings and emits aggregate counts. Stale-finding reconciliation and status/control consumption remain PENDING. Shared finding APIs: [shared-sync-export-interfaces](shared-sync-export-interfaces.md). |

The baseline captures a journal fence before scanning bounded current-state pages.
This increment completes it only when the end fence is unchanged; replay remains
PENDING. The complete mirror will replay from that fence and handle rescan;
listing is not an atomic snapshot. Durable pending operations
are a prerequisite for replacement, deletion and conflict preservation. The
initial exporter uses these delivered interfaces. No polling or best-effort
marker substitute bypasses them.

### Marker and activation requirements

The [shared root implementation](shared-sync-root.md) supplies these storage
primitives on supported Unix filesystems. The initial-export increment connects
configuration, startup and `--sync-reinitialize` to them. Directory-inode locks prevent
lock-file replacement bypass; owned file jobs retain both root and catalog leases.
An interrupted marker creation can leave a partial marker: preserve/back up that
file and remove it explicitly before requesting missing-marker reinitialization.
The API never overwrites it. Host power loss and other-platform runtime behavior
are unverified; unsupported platforms reject activation.

The root marker is bounded to 4 KiB and contains version `1`, opaque `catalog_id`
and opaque `root_id`. Reject unknown versions/fields, malformed IDs, symlinks
and oversize input. Marker IDs are identities, not credentials. Shared state
must expose durable identity and association properties; this contract does not
choose their schema, allocation or transaction implementation.

Before bind, validate settings, root identity and marker association under
exclusive catalog ownership. Acquire exclusive sync-root ownership too: two
different catalogs must not write the same tree. Keep both ownerships through
all in-flight file jobs. Existing catalog recovery remains before bind.
Do no catalog-sized scan before ready. Startup ordering follows Shared state's
contract: PAP, bucket initialization and seed precede sync preflight, then
bind/ready, then paged initial reconciliation. Ready means API usable.

Never-used catalog/root pairs may create a marker and record an activating
association durably before reconciliation. Do not accept external deletions
until the complete initial baseline is committed. After interruption, an
activating association resumes the same merge, not a fresh empty-tree deletion.
First activation behaves as follows:

- Empty catalog: create default buckets from top-level directories, including
  empty directories; ingest valid files with create-if-absent conditions.
- Nonempty catalog: API bytes win. Export catalog-only objects; ingest external-only
  names with create-if-absent; identical bodies adopt the live generation without
  a new one; differing bodies at the same name are preserved as conflicts first.
- Existing buckets retain ownership, CORS, IAM, versioning, holds and metadata.
  A new external bucket uses `seed_project` and shared default-bucket validation.
  An ownership mismatch is reported, never reassigned. Report created, imported,
  exported, identical, conflicting, ignored and unrepresentable counts separately.

A previously associated root with a missing marker, or any foreign/malformed
marker, stops startup before bind. A wrong mount is never an empty baseline.
First repair the mount/root. For intentional reactivation, stop other writers,
back up the tree and use `--sync-reinitialize CATALOG_ID`: require a missing
marker, exact local catalog ID, unchanged selected root identity and exclusive
ownership. A foreign/malformed marker cannot be overwritten with this option;
move that tree aside intact and select a fresh unassociated directory instead.
Reactivate using the first-activation merge, preserve old conflict records and
forget no API data. Reset neither catalog nor seed marker; suppress all external
deletions until the new baseline completes. Record the new association and marker
crash-safely; a half-written/missing marker after a crash stops and explains the
same reactivation remedy. Do not change an unrelated root's ownership record.

## Ingestion, metadata and conflicts

SQLite owns all metadata; files contain current live bodies only, with no xattrs
or sidecars representing metadata. External rename is delete plus create, not a
metadata-preserving API rename. Use the shared management path in both auth
profiles as a trusted operator action; external file access is itself powerful
and is not authenticated as an API principal. Strict API reads still enforce IAM.

After settling, use `publish_from_file` and an atomic observed generation/bucket
incarnation condition. `InheritFromReplaced` inherits metadata in the committing
transaction, preserving a racing metadata PATCH; never read and resend stale
metadata. New objects use seed MIME rules and ordinary default-hold behavior.
`SkipIfSameHash` compares size, MD5 and CRC32C and allocates no generation when
identical. Changed-during-staging input retries after another quiet deadline.
Checksums always describe the staged immutable bytes. Catalog timestamps remain
microseconds under schema 14; filesystem nanosecond fingerprints are separate
observations, never catalog timestamps or imported object identities.

Compare divergent files with the current live hash before declaring a conflict.
If they already match, update only sync state: this is recovery after write-back
but before its state commit. Suppress self notifications only when path, size,
mtime, inode and hash match recorded output. Journal origin `sync-ingest` skips
body write-back, while still advancing state/cursors. Origin alone cannot mask
an independent later API mutation.

API mutations win over divergent external edits, including initial activation.
Preserve known divergent bodies first under opaque conflict IDs in dedicated
`data_dir/sync-conflicts`, never `staging/`, `blobs/` or ordinary working-tree
paths. The reserved workspace may temporarily retain a newly displaced body
only under the recovery boundary below; it is never an ingestible conflict file.
Store catalog/root identity, logical name, original owning project, observed
generation/incarnation, fingerprint, hashes and reason with it. Persist that owner
identity for pending recovery as well as registered conflicts; deleting the bucket
or recreating its name in another project cannot erase or retarget it. API deletion
can therefore win without destroying the external body. Retained version history
follows ordinary shared publication.

Reserve bytes and one entry before capturing; stream and enforce the byte cap
even if the source grows. Exact cap usage is allowed, overflow stops ingestion
and any write-back that would discard external bytes. Disk-full/sync failures
leave the source intact and expose a stop. Never evict old conflicts automatically.
If capture of an already observed divergence cannot complete, do not overwrite the
external file to finish mirroring. Persist conflict registration before replacing
that known divergent file. A last-check race can instead reveal divergence only
after exchange/removal has displaced the actual inode: the independent displaced-body
recovery boundary below applies, without claiming registration preceded that race.
Interrupted captures remain recoverable and quota-accounted, with no unbounded
orphan accumulation.

For registered conflicts, `resolve restore` means publish the retained body as an
external edit through the same shared conditional path, then mirror the resulting generation. It does
not write a surprise object at a caller-supplied path. Bind the request to the
conflict revision and currently observed live generation/incarnation; if either
changes, reject and require a new inspected resolution. Use ordinary holds and
metadata inheritance. Mark resolution complete and reclaim the preserved body
only after confirmed publication; ambiguity retains bytes. `resolve discard`
explicitly removes that one retained conflict revision, leaving the API winner.
An absent original bucket or a different current incarnation rejects restore with
409; never create or retarget a bucket from a retained logical name. Discard still
requires authority over the retained original owner. Both operations are idempotent
for their recorded operation ID and drain safely after caller disconnect;
cancellation cannot release ownership before side effects.

## External deletion and approval

Deleting a top-level directory does not delete its bucket or settings. Treat
its tracked files as deletion candidates, retain history under versioning and
recreate the empty bucket directory. A hold/retention rejection restores the
file and reports the shared failure; release the protection through the API
and delete the file again to retry. No sync operation bypasses holds.

After a complete, error-free root scan, let `N` be the number of representable,
successfully baselined live objects at the captured catalog fence, and `D` the
tracked names absent in that scan. Exclude ignored/unrepresentable/unobserved
names and stale incarnations. A candidate's live generation must still equal
its last successfully synchronized generation; a newer API generation is
write-back work, never a newly inferred deletion. An incomplete scan never generates deletions.
Guard when enabled and `D >= 100` and
`100 * D > 50 * N`, using checked wide arithmetic; `N=0`
has no deletions. Freeze this root-wide denominator and exact candidate set;
do not recompute the fraction per page or convert a marker failure into a scan.

The pending deletion set has an opaque revision tied to catalog/root IDs,
root identity, completed scan revision, journal fence, thresholds, candidate
names, bucket incarnations and live generations. Persist/page large sets;
never materialize the complete set in RAM. `approve` takes that revision,
not a boolean or wildcard, and grants one execution of that exact set only.
Any observed tree change, root/marker change, relevant API change or restart
invalidates unused approval and requires rescan and fresh approval. Replays and
foreign/stale revisions return 409 without broadening the set.

Inspect candidates with `GET /_internal/sync?view=deletions&revision=REVISION`,
following its cursors to completion. Every page belongs to that exact frozen set,
with the same denominator, count and fence; `approve` executes the whole inspected
revision, never only the last page or a recomputed current set. Any invalidation
also makes its remaining page requests return 409. Restart invalidates both the
inspection revision/cursors and unused approval, requiring fresh inspection.

Immediately before each delete, recheck absence, root/marker identity and
approval membership, then atomically check generation/incarnation with deletion.
Already completed members stay completed; a failure reports progress and stops
remaining members for reinspection. Never delete a newer API generation or a
recreated bucket by name alone. A filesystem create can still race the final
absence check and catalog commit: there is no atomic filesystem/SQLite snapshot.
Leave the recreated file untouched, detect it on notification/rescan and ingest
it conditionally; a new API generation wins. Approval cannot authorize unlinking
that new external file. Test this boundary explicitly, without claiming atomic
root-wide deletion. Disabling the percentage guard does not remove these checks.

## Write-back and recovery boundaries

The [shared write-back implementation](shared-sync-writeback.md) supplies durable
intent/protection, private durability receipts, progress acknowledgment and
descriptor handoff. The following replacement, exchange, conflict-transfer and
authorized-resolution sequence remains a runtime requirement. Deterministic
storage boundary tests do not establish power-loss safety for that future runtime.

Copy from the already opened `ObjectReader` descriptor into the same-volume
reserved workspace using Shared state's `fsutil` copy/replace operations.
Never reopen a blob by path or hard-link a mutable tree file to a blob. Required
order: copy/clone, file `sync_all`, destination recheck, replacement, parent
directory fsync, then sync-state commit. On Unix, sync every affected parent
when moving between workspace and target. A failed durability step cannot advance
state; preserve recoverable files and report retry. Use no-replace publication
for absent targets so a concurrently created file becomes a conflict.

For API deletion, move the actual target into a unique reserved workspace name
without clobbering another entry, then validate the moved file before cleanup.
Preserve divergent external bytes as a conflict; an already absent target is a
no-op. A file newly created at the target afterward is new external input and
must not be unlinked. Remove only empty directories, never recursively remove
untracked external files when an API bucket disappears. Apply the same parent
durability and state-after-file ordering to removal.

On Linux with supported `renameat2(RENAME_EXCHANGE)`, exchange existing target
and prepared output, then validate the actual displaced inode/fingerprint/hash
through its retained descriptor/name. Changed displaced content is a conflict
and must be saved before cleanup; API output remains the winner. Recheck the
installed output too: another external replacement after exchange is fresh input,
not proof that the API output is still installed. Never blindly exchange back
over another writer. Unsupported exchange on a Linux filesystem must be reported
and use the documented fallback limitations, not silently claim exchange safety.

### Displaced body awaiting preservation

Exchange, and the move used for API deletion, can discover a different or larger
external inode after the final target check. Ordinary conflict capacity checks
cannot prevent that race. Before displacement, require durable recoverability of
the operation identity, original owner project/incarnation and unique workspace
entry; afterward the actual displaced body is protected recovery input until
durably transferred and registered in conflict storage or explicitly resolved
through the authorized recovery discard below. Shared state owns the
intent/protection mechanism, including discovering an interrupted displacement.
Never automatically unlink, truncate, reuse or collect that entry as temporary/orphan
output, even if no conflict row was committed. Never blindly exchange/move it back over
the current target or another writer.

Failure to capture/register after displacement is a separate recovery boundary.
Keep the body at its reserved workspace entry if byte/entry quota or ENOSPC stops
transfer; sync the retained file and affected parents where possible. Failed
sync/registration remains an explicit unresolved failure, never successful sync
state. Account the entry and all actual retained bytes, including workspace
residency and in-progress reservations, without double-counting a transferred
body. Unexpected size can exceed the fixed quota: report actual usage and
over-limit state rather than clipping, deleting bytes or granting more intake.
Pause root-wide ingestion and destructive write-back on `ConflictCapacity` or
`IoFailure` before another such operation can accumulate stranded bodies. Status
and authorized recovery controls remain available. API service may continue;
mirror progress cannot be acknowledged while this recovery is incomplete.

Restart must rediscover and protect these entries before workspace cleanup or
new destructive sync work; keep the stop and quota charges even if the original
bucket no longer exists. Free disk space or resolve retained conflicts within
the fixed quota, then rescan to finish preservation
and register a conflict. Alternatively use the explicit recovery discard below;
registration is not a prerequisite for that operation. Repeated failure stays
stopped without spinning. If normal preservation cannot proceed, stop external
writers and the server, back up the
catalog, conflict store and working tree including its reserved workspace, then
copy the retained entry to an operator-selected location outside both roots.
The stopped recovery diagnostic must identify its recovery ID, logical/original
owner identity, exact relative workspace entry and size/hashes so the operator
can locate and verify the copy; HTTP status never returns the raw entry path.
Keep the original until preservation/registration succeeds or an authorized,
confirmed recovery discard durably completes. An offline copy alone does not
authorize automatic deletion or clearing recovery state. The owning runtime
slices must ship and test these recovery instructions with their actual workspace layout.

### Explicit discard before conflict registration

A displaced sparse file of **1099511627777 bytes (1 TiB + 1 byte)** cannot fit
even the maximum conflict quota. Registration-only recovery would leave a stop
with no legal remedy. Extend the existing `resolve` operation to discard one
unregistered `recovery_id`; do not increase the cap or turn quota pressure into
automatic cleanup. This makes the no-remedy prohibition concrete while retaining
the requirement to preserve bytes until the operator explicitly chooses disposal.
Restore still requires a registered conflict and follows its existing conditions.

Inspect the recovery finding first. It supplies `recovery_id`, `recovery_revision`
and opaque `target_identity` in addition to the original owner/incarnation and
retained-body observations. The identity binds catalog/root, original logical
target/owner/incarnation and the actual protected workspace entry, not a current
same-name bucket lookup or a caller-supplied path. The revision fixes that
identity, size/fingerprint, hashes when verified and recovery phase; any change
or restart invalidates an unused revision and requires fresh inspection. Changing
these returned fields also invalidates the findings revision/cursors as above.

Send `recovery_id`, `revision` (the exact `recovery_revision`), `target_identity`,
`action: "discard"`, `operation_id` and exactly one `confirmation` value:

- `backup_verified`: the operator has stopped external writers, made a durable
  copy outside both roots and verified its size and hashes against the retained
  body. This is an operator attestation, not server verification of a backup path.
- `discard_without_backup`: the operator has stopped external writers and
  explicitly accepts permanent loss of this exact body without a verified backup.
  A full disk or unverified hashes cannot silently select this option.

No default, generic force flag or copy alone constitutes confirmation. The CLI
requires `--recovery ID --revision REV --target-identity ID --action discard
--operation-id ID` plus exactly one of `--confirm-backup-verified ID` or
`--confirm-discard-without-backup ID`; that confirmation ID must equal the selected
recovery ID. It maps to the matching JSON enum. `conflict_id` and
`expected_generation` are absent in this form; mixed target forms, restore on a
recovery ID, or ambiguous confirmation are rejected without effects. Recovery
discard changes neither API generations nor the current working-tree target.

Apply the complete management authority union, including the durable original
owner's project-level `roles/storage.admin` grant in strict mode, before disclosure
or mutation. Recheck it, the exact revision and retained entry identity/observations
when executing; bucket deletion/recreation never retargets authority. Permissive
controls retain their explicitly unauthenticated operator profile; exact targeting
and confirmation remain mandatory. Missing/invalid credentials return 401;
insufficient original-owner authority returns 403. Control errors carry `code`:
missing/ambiguous confirmation is 400 `recovery_confirmation_required`, an
unsupported target/action shape is 400 `invalid_recovery_resolution`, a stale or
foreign recovery/revision is 409 `recovery_revision_mismatch`, changed identity or
body observations is 409 `recovery_target_changed`, and reuse of an operation ID
with a different payload is 409 `operation_id_reused`. These rejections leave
the body, quota and root stop intact; the remedy is reauthorization or reinspection
and a new explicit confirmation, never automatic retry with broadened conditions.

Before unlink, durably record acceptance of that exact operation, original owner,
target identity/revision and confirmation. If that cannot be committed (including
ENOSPC), return 503 `disk_full` or `io_failure` and retain all bytes and the stop.
Then unlink only the checked protected entry, sync its parent, and durably finish
the resolution and quota/state update. Keep the quota charge, original-owner
requirement and recovery stop until durable completion; 202 acknowledges acceptance,
not completion or root resumption. An unlink/parent-sync/completion failure remains
inspectable and stopped, even if authorized deletion already removed the body.
No failure may delete another entry or roll back over the current target.

After disconnect, kill or ambiguous commit, recover the durable operation before
cleanup. With no committed intent, preserve the entry and require a fresh request;
with an intent, recheck the current authority and still-present target before
continuing only that deletion. Revoked authority or changed identity stays stopped.
After the same authority check, an absent entry with that intent may finish
parent durability/state completion;
absence without an intent is corruption, not evidence of authorized disposal.
Identical `operation_id`/payload retries, including after restart or completion,
return that operation's recorded progress/result after current authorization
including its recorded original owner,
without repeating stale-revision admission or deleting another body. Unaccepted
requests after restart require a fresh revision. Shared state owns durable intent,
completion/idempotency and quota mechanisms; these are required properties, not
a prescribed schema or transaction implementation.

After registration or durable authorized discard resolves every displaced-body
recovery, rescan/revalidate root and marker, quota, installed output and all other
stops before resuming ingestion/destructive write-back. Reconcile the current API
winner; neither discard acceptance nor removal of one of several entries clears
the whole-root pause by itself.

Acceptance covers a final-check writer replacing the target with a quota-sized
or oversized body, byte/entry quota already full, ENOSPC during transfer,
file/parent sync and registration, and kill/restart in each stopped/displaced
state. Verify actual bytes, original identity, full quota charges, no automatic cleanup or
blind rollback, inspectable stops, operator retrieval and eventual resumption.
Include the hard-limit-plus-one sparse body, both confirmation choices and ENOSPC
before intent persistence; reject other-project, stale revision/identity and
missing-confirmation requests with byte-for-byte retention. Kill/restart before
and after intent, unlink, parent sync and completion must retain unresolved quota
and authority, converge idempotently and release the pause only after authorized
durable completion and successful root revalidation.

### Other replacement and restart boundaries

On other OS/filesystems, take a settled conflict-capable snapshot of the old
target, check identity/fingerprint immediately before replacement and verify
the installed output immediately afterward. Windows uses same-volume
`MoveFileExW` with write-through and no copy-across-volume fallback. A write
between the last old-target check/snapshot and replacement can be overwritten
without detection; a post-replacement edit is detected by the output fingerprint
or next scan. Matching/restored stat fields require racy hashing but cannot
recover already overwritten bytes. Status must advertise
`writeback_safety: checked_replace` and this limitation; Linux exchange advertises
`exchange`. Neither closes writes made later through a still-open displaced
descriptor. Stop the service and editors for a conflict-free cutover, then restart
and rescan. Do not claim no-loss concurrent-writer guarantees
for this fallback or power-loss namespace guarantees on Windows.

After crash, compare working bytes with the current live hash before ingesting;
recover pending conflicts and write-back intents, then reconcile in bounded
pages. Kill tests cover before/after file sync, exchange/replace, parent sync,
conflict registration and state commit, including disk-full and ambiguous commit.
Restart must never publish a damaged output as an external replacement. Shared
state chooses how intents/state hooks provide this property; this contract does
not prescribe tables or an internal transaction design.

## Management API and CLI

Reserve the following emulator-only routes outside Google API namespaces.
`gcs-mimic sync status|approve|conflicts|resolve|rescan|wait` calls them through
the live server and never opens the data directory. CLI accepts `--url` and
`--ca-cert`, verifies TLS using the shared connector and accepts a bearer token
from `GCS_MIMIC_SYNC_TOKEN` without printing it. It must not follow redirects
carrying credentials. These commands are future interfaces, not runnable today.

In strict mode **all** routes below, including reads/wait, require a registered
bearer identity with an explicit project-level `roles/storage.admin` binding
for the union of `seed_project`, every existing synchronized bucket's owning
project and every original owning project retained by unresolved conflicts or
recovery state (including displaced workspace bodies). Retain the original owner
and bucket incarnation across deletion of that project's last bucket, restart
and same-name recreation under a different project. Never derive retained-state
authority solely from the current bucket lookup; missing/corrupt retained owner
identity fails closed. Removing its final unresolved record removes that project's
retained-state requirement, subject to the other members of the union.
Bucket IAM grants, object roles, `allUsers`, `allAuthenticatedUsers`, signed URLs,
loopback origin and knowing a catalog ID confer no sync authority. Reject missing
or invalid credentials with 401 and insufficient authority with 403 before
parsing control bodies or disclosing names/conflicts. Recheck the authorized
project set/target revision when executing queued side effects; newly involved
projects cannot inherit an earlier approval. This is local administrative
authority, not a new Google IAM permission. Background sync uses the startup
operator's `AdminGrant` separately. Permissive mode explicitly allows controls
without authentication and must be identified as such in status.

| Method and path | Request and response |
| --- | --- |
| `GET /_internal/sync` | Default summary: state, stop counts, catalog/root IDs, applied/current journal sequences, pending-work count, scan revision, pending-deletion revision/count, findings revision, conflict quota usage (including workspace bytes/entries and over-limit state) and `writeback_safety`. `view=deletions` or `view=findings` selects the fixed-revision pages defined below. No object bodies or raw filesystem paths. |
| `GET /_internal/sync/conflicts` | `limit` default/max 256, opaque cursor bound to catalog/root and listing revision. Return IDs, logical names, original owner project/incarnation, revisions, generations, reason, size/hashes; no retained-body path. Stale cursor returns 409. |
| `POST /_internal/sync/approve` | JSON `deletion_set` revision; return accepted operation ID and exact count, or 409 for stale/foreign/replayed revision. |
| `POST /_internal/sync/resolve` | Registered conflict: JSON `conflict_id`, `revision`, `action` (`restore` or `discard`), `expected_generation` (zero means absent) and `operation_id`; generation/incarnation recheck as above. Unregistered recovery: exclusive `recovery_id`, `revision`, `target_identity`, `action: "discard"`, `confirmation` and `operation_id` as defined above; original-owner authority and explicit confirmation precede deletion. |
| `POST /_internal/sync/rescan` | Empty JSON object; return coalesced scan revision/operation ID. It requests observation, never approval or an override of a stop. |
| `POST /_internal/sync/wait` | JSON `timeout_ms`, default 10000, `1..=60000`; capture a requested stable scan plus journal fence and wait for that bounded barrier. Return barrier/revisions/counts or explicit timeout/stopped result. Later unrelated edits do not move the fence forever. |

CLI `approve --deletion-set ID`, `resolve --conflict ID --revision ID --action
restore|discard --expected-generation N --operation-id ID`, and `wait --timeout
10s` map exactly to these requests. `status --view deletions|findings --revision
ID --limit N --cursor ID` maps to the GET query; listing accepts the same page
limit/cursor flags. Recovery resolve uses the exclusive recovery form and explicit
confirmation flags above. Omit `--view` for the summary.
All control JSON is capped at 64 KiB, rejects unknown fields and wrong types;
GET bodies are rejected. Responses are capped at 256 KiB with paged detail.
Unknown method returns 405; unknown route/disabled sync returns 404; malformed
input 400, oversize 413, stale conditions 409, admission/closed service 503.
Accepted asynchronous mutations return 202 with operation ID; completed reads
and wait return 200, wait deadline 408 and an existing stop 409. The CLI exits
nonzero on errors, timeout or stopped work and prints actionable diagnostics.
At most 32 control requests/waits are active; excess returns 503. Wait uses
notifications/deadlines, not polling. Disconnect cancels only its observer.

Both status detail views require a nonempty `revision` from the summary:
`pending_deletion_revision` for `deletions`, `findings_revision` for `findings`.
Accept `limit` in `1..=256` (default 256) and an optional opaque `cursor`; reject
duplicate/unknown query keys, an unknown view, wrong types, or pagination keys
without a detail view with 400. A cursor binds the view, catalog/root identities,
fixed revision and next position; it cannot select another revision or filter.
Each response contains `catalog_id`, `root_id`, `view`, `revision`, `total_count`,
`items` and `next_cursor` (null on the final page), within the response byte cap.
Persist/page the fixed set in deterministic order without collecting all items.

- `deletions` also returns `scan_revision`, `journal_fence`, `baseline_count`,
  `candidate_count`, `guard_enabled`, `min_objects` and `percent`, identical on
  every page. Each item has `bucket`, `object`, `owner_project`,
  `bucket_incarnation` and `generation` for exactly one approved candidate.
  `total_count` equals `candidate_count`; traversing every page yields each
  candidate exactly once, matching the whole set targeted by `approve`.
- `findings` pages every current stop/finding target, including Unrepresentable,
  ProtectedDeletion and displaced-body recovery. Each item has `finding_id`,
  `reason`, `scope`, nullable `bucket`, `object`, `owner_project`,
  `bucket_incarnation` and `generation`, plus `detail_code` and actionable
  `remedy`. Root stops use null target fields. Recovery items additionally carry
  `recovery_id`, `recovery_revision`, `target_identity`, `phase`,
  `retained_location` (`workspace` or `conflict_store`),
  `retained_bytes`, `hashes` (null until verified) and `over_limit`; they expose
  no body or filesystem path. A findings revision fixes all returned fields.

An invalidated/foreign revision or mismatched/stale cursor returns 409 before
returning any items. Deletion invalidation follows the approval rules above;
a change to the findings set or any returned finding field invalidates its
revision. Restart/root reassociation invalidates both views and all cursors.
The operator must fetch the summary and reinspect pages after 409. Test multiple
pages of deletion, unrepresentable and protected targets, including a mutation
between pages: neither an old cursor nor the old deletion approval may succeed.
All summary/detail/conflict reads and resolution recheck the complete authority
union above. Test list and restore/discard 403 for a seed-project admin lacking
project B authority before/after B's last bucket deletion and restart, and
before/after recreating that name in another project. A fully authorized caller
still gets 409 when restore targets a different bucket incarnation.

`wait` forces a complete stable scan of preceding external edits and applies
API changes through its captured journal fence, including a post-scan fence;
completion requires no unresolved work/stops within that barrier. A writer that
never settles yields timeout. A successful wait is not a permanent quiescence
claim. Shutdown wakes waiters and drains accepted side effects before releasing
locks. CLI users can write a file, wait, then read through the API.

## Stops, findings and remedies

`SyncStop` is exhaustive for state/CLI/diagnostics. Distinguish a whole-root
pause, a blocked path and a nonblocking finding. Emit structured stderr events
on transitions, not on every repeated observation. Every diagnostic names the
logical target/key, scope and remedy; paginate details and aggregate counts.

| Reason | Scope and discovery | Complete recovery path |
| --- | --- | --- |
| `DeletionApproval` | External deletions paused; status/ready summary/event | Inspect exact revision, approve it, or restore files and rescan; changed candidates require fresh approval. |
| `MarkerMismatch` | Pre-bind failure, or whole-root pause if discovered later | Repair mount/root; for intentional missing-marker reactivation back up and use the one-shot option; preserve foreign/malformed trees and choose a fresh directory. |
| `Conflict` | Path finding after successful preservation; `conflicts`, status/event | Restore the retained revision with inspected expected generation, or explicitly discard it; reinspection resolves stale conditions. Other paths continue. |
| `ConflictCapacity` | Root ingestion and destructive write-back paused; status/ready/event and paged recovery findings | Resolve/discard registered conflicts within the fixed 1 GiB/1000-entry limit, then rescan. Before displacement the source stays intact; afterward protect/account its workspace entry even above quota. Complete preservation/registration or use original-owner-authorized, exact-revision/identity recovery discard with explicit backup/no-backup confirmation, then revalidate all root stops. Never automatically delete or blindly roll back the body to free space. |
| `Unrepresentable` | Path finding; paged status findings/event | Rename/copy-delete through API into a representable name, remove the collision/unsafe external entry without following it and rescan, or retain API-only access. Never silently normalize. |
| `ProtectedDeletion` | Path restored and blocked; paged status findings/event | Release hold/retention through a supported API, then delete the file again. If protection cannot be released, retain the restored file/API object. |
| `SourceChanging` | Path deferred until quiet deadline; status | Finish/stop the writer then rescan/wait. Continuously changing input times out wait without publishing partial bytes. |
| `IoFailure` | Affected path; whole-root pause for unavailable root, incomplete scan or failed displaced-body preservation; status/ready/event and paged recovery findings | Restore mount/permissions/free space, preserve source/conflicts and any displaced workspace body, then rescan under the displaced-body recovery rules; typed disk-full is not swallowed or tight-looped. |
| `ObservationGap` | Whole scan required; status/event | Rescan after journal gap/notification overflow; bounded recovery proceeds automatically when resources permit. Watch setup failure requires fixing the mount or approved platform support and restart; no silent periodic polling. |
| `StateCorrupt` | Root pause or pre-bind failure if preflight detects it | Stop service, retain catalog/tree/conflicts and restore a coherent stopped-data backup or repair through the owning recovery tooling; never reset state to turn missing files into deletions. |
| `Closed` | Shutdown; status while available, waiting requests wake | Restart after fixing the reported shutdown/storage error; reopen only after old jobs release ownership. |

`/_internal/ready` keeps its existing API-readiness HTTP status. Its optional
`sync` summary contains only enabled/state, aggregate pending/conflict/stop counts
and applied sequence, with no paths, names, catalog/root IDs or tokens. Detailed
strict diagnostics require management authorization. A synchronization stop
alone does not make the API unready; catalog health failures still can.

## Resources, ownership and remaining approval

Sync owns one dedicated scan/file worker with at most two queued pages of 256
entries and a 4096-distinct-path notification queue. Coalesce duplicate paths;
overflow sets one full-rescan-needed flag. Stream bodies with bounded buffers,
page catalog/state/diagnostics, and never collect/sort the entire catalog or a
large directory in memory. Run filesystem iteration/stat/hash/copy on dedicated
OS workers, not the shared Tokio blocking pool. Use shared wait-bounded admin
admission for reads/publication and short keyset transactions interleaved with
API work, never the API's immediate-failure admission/upload slots.

Compare size/mtime-ns/ctime-ns/inode before reading bodies; hash changed and racy
files. Unknown/coarse timestamp precision is conservatively racy. Skip staging
when a size comparison already decides an unchanged case. Read journal pages
by sequence, coalesce repeated object changes to the final state, and rescan
when history is unavailable. Use event wakeups and next due quiet/retry deadlines;
no timer exists when nothing is due. Retrying I/O uses bounded backoff and a
visible stop, never a hot loop.

| Shared owner | Required property and acceptance request; internal mechanism belongs to owner |
| --- | --- |
| shared-startup-phases | Bounded sync preflight before bind, reconciliation after ready, sanitized ready summary and cancellation-safe close on failure. |
| shared-admin-interface | Trusted AdminGrant, typed Busy/DiskFull/Closed/Precondition/Hold/SourceChanged failures, wait-bounded separate admission and default-bucket creation; close waits for all accepted admin jobs. |
| shared-publication-modes | Atomic generation/incarnation conditions, InheritFromReplaced plus SkipIfSameHash, descriptor-based staging with stable-byte hashes and source drift detection; crash/cancel cannot reclaim possibly committed bodies. |
| shared-publication-io | Descriptor copy/clone, no-replace and exchange/checked-replace outcomes identifying the actual displaced body, nanosecond stat identity, durability ordering and detectable unsupported operation; preserve displaced entries on error, with no mutable hard links or blob path exposure. |
| shared-change-journal | Paged monotonic origin-bearing changes committed with mutations, gap notification and stable fences covering API create/overwrite/metadata/delete/copy/compose and bucket changes. |
| shared-sync-state | Durable catalog/root association and activation state, indexed paged path/fingerprint/generation observations, atomic publication/deletion state hook and recoverable write-back intents; protect displaced bodies before registration across quota/ENOSPC/kill/restart, account workspace residency and retain original owner projects/incarnations after bucket deletion/recreation; provide fixed-revision paged inspection and reject stale incarnations while protecting lock lifetime. Before-registration discard requires exact recovery revision/identity, original-owner authority and explicit confirmation; retain authorized intent/completion for idempotent recovery, and release quota/stop only after parent durability and state completion. |
| import-dry-run | Reuse path-overlap validation, bounded traversal and the shared regular-file ignore predicate; sync-specific collision/incremental policy stays in sync. |

These are handoff requirements and pending tests, not an implementation of
Shared state or authority to choose its schema, permits, journal retention or
benchmark thresholds. The first name-mapping consumer implements the rules
above outside `storage`; source import retains its separate upstream bucket
escape codec. No runtime interface is added by this contract.

**File-notification dependency selection is deferred to `sync-watcher` and
requires owner approval.** Review current stable version, license, package/
binary-size impact, maintenance and native platform behavior before asking.
No library is selected or authorized here. Linux/macOS/Windows and Docker bind
mount notifications require their own evidence, including unavailable/missed
events and overflow fallback. Windows watcher support is pending; unsupported
watch setup must report failure, never false synchronized success. Reusing
`fsutil`, MIME and TLS components does not authorize a new dependency.

## Sources and verification boundary

This is an emulator extension, not a Google directory-sync API. Checked official
[object naming](https://docs.cloud.google.com/storage/docs/objects) and
[preconditions](https://docs.cloud.google.com/storage/docs/request-preconditions)
on 2026-10-24; their constraints do not make filesystem normalization safe.
The pinned Google baseline remains Discovery revision `20261013`, SHA-256
`1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`, and
Storage v2 commit `d73dfb3c3be52245625b951b9c0bcf65163c549f`, as recorded in
the [basic JSON contract](basic-json-contract.md). No protocol inventory changes.
The Linux [rename contract](https://man7.org/linux/man-pages/man2/rename.2.html)
and Microsoft's [MoveFileExW contract](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw)
were checked on the same date; platform implementation/tests remain pending.

Reconciled `compatibility/gaps.json`: no open gap directly targets these design
rows. Existing SPEC-008, SPEC-VERSION-DELETE-CONDITIONS and SPEC-EVENT-HOLD-WIRE
remain open; sync uses existing conditional live deletion/hold behavior and
claims no new provider wire parity. The onboarding findings' schema-13 note is
historical: the current catalog uses schema 15 and microseconds. No millisecond import
rounding or new identity-preservation mode is introduced.

This document changes no owner decision. It makes the specified non-exchange
race window explicit rather than claiming an unattainable cross-filesystem/SQL
atomic transaction. Runtime cases, process-kill/platform tests, official SDKs,
containers and notification behavior have not been executed for this contract.
Performance is **unmeasured**. Ordinary runtime slices require no benchmark or
measurement; a slice whose purpose is performance may record one short run at
about 1,000 entries as a fact, without a regression gate. Bounded pages and the
stat-first rescan remain design requirements enforced by tests and review. No
speed or memory improvement is claimed.

### Historical contract-only document checks

Executed on 2026-10-24 against this contract and its ledger changes:

| Check | Result |
| --- | --- |
| `npx --yes markdownlint-cli2 docs/en/slices/directory-synchronization.md` | PASS with the per-file MD013 exception for contract tables; stdin inspection also passed. A deliberately invalid heading control failed with MD022, confirming active linting. |
| `cargo xtask compatibility validate` | PASS: 591 feature rows, 401 parameter locations and 357 schema fields. |
| `cargo xtask compatibility render` then `cargo xtask compatibility check` | PASS; both generated language views are unchanged. |
| Baseline/current ledger comparison with `jq -e` | PASS: the 13 sync cases remain PENDING with no test IDs; only the existing conflict-preservation, management-authorization and write-back-recovery descriptions changed from the preceding contract. All other ledger values, including the previous fixed-revision inspection/approval requirements, DESIGN-062/072/074 PARTIAL and DESIGN-076 IMPLEMENTED, are unchanged. |
| `git diff --check` and local-path scan | PASS for changed content. The full tracked scan finds only the pre-existing path-search command in the onboarding plan, not a local path value. |

No runtime tests, `just check`, dependency freshness run, SDK suite, container
build or performance measurement was run for this document-only slice. The
coordinator owns full `just check` after local-main integration. No new passing
runtime evidence or feature-status promotion is recorded by these document checks.

### Historical lexical mapping implementation checks

Verified on 2026-10-25. `src/sync/mapping.rs` exposes the shared `PathMapping`
and explicit reasons, `name_to_path` and `path_to_name`. Both directions use
the shared validated `BucketName` and `ObjectName` types. The forward function
produces a **lexical candidate**, not permission to write a filesystem entry.
It applies native bucket/object validation, rejects empty/dot/NUL segments and
reserved components at every depth, and shares
`source_tree::is_ignored_regular_file` for final components. Matching prefix
directories remain eligible. Literal percent and Unicode bytes are unchanged;
Unix backslashes stay literal and Windows-invalid components are rejected.
The reverse function compares raw OS strings rather than normalized `Path`
equality, so an alternate spelling cannot be silently cleaned into another name.

`cargo test --locked --offline --lib sync::mapping` passed five tests, including
two property tests for arbitrary names/paths and the Windows restriction
predicate. The path strategy generates valid bucket/component pairs on every
iteration, checks both decoded identities and exact raw path spelling, and
rejects a dot-segment variation of each generated path.
`cargo test --locked --offline --test sync_mapping` passed two public
API tests: the exact 1024-byte name boundary/overflow and invalid Unix UTF-8.
The Windows predicate follows Microsoft's
[file naming conventions](https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file#naming-conventions),
checked on 2026-10-25; testing that pure predicate on Unix does not verify native
Windows path handling or actual filesystem representability.
`cargo xtask dependencies check` passed with Rust 1.99.1 and every checked input
current at Unix time 1792901168. No dependency or lockfile changed.

The lexical slice did not implement actual-root aliases, host component/path
limits, API file/directory collisions, no-follow root/ancestor checks,
notifications, activation, marker persistence or write-back/restart recovery.
The shared root, observation and write-back APIs now deliver the limited storage
prerequisites listed above. Runtime filesystem reconciliation, notifications,
configuration and startup remain pending; these APIs do not complete sync.
Replacing seed's duplicate path conversion with this shared mapper is deferred
to `import-seed-adapter`.
SDK/container/guide runs are unnecessary for this source-only utility and
contract change; no HTTP, container or runnable guide behavior changed.
No performance measurement was performed or result claimed.

The final source was checked once with `just check` and localhost-capable permissions. Formatting,
workspace clippy with warnings denied and the initial shell checks passed, but
the library test stage failed in an unchanged resumable timing test:

```text
storage::resumable_http_tests::final_hash_scan_uses_request_budget_instead_of_catalog_wait_budget
assertion `left == right` failed
  left: 503
 right: 200
test result: FAILED. 363 passed; 1 failed
```

The exact isolated rerun,
`cargo test --locked --offline --lib storage::resumable_http_tests::final_hash_scan_uses_request_budget_instead_of_catalog_wait_budget -- --exact`,
passed: one test, zero failures. Concurrent-load sensitivity is known to the
integration coordinator; the cause remains unconfirmed. No condition, test or
storage implementation was changed. Per the coordinator's instruction, the
full check was not repeated; its remaining integration/shell stages were not
reached on this final source. The coordinator owns the merged full check.

Standalone ledger/generated-view checks and Markdownlint passed with the existing
MD013 table exception. Both English and Japanese matrices were regenerated and
remain byte-identical because parent statuses are unchanged:
DESIGN-062/072/074 stay PARTIAL and DESIGN-076 stays IMPLEMENTED. Only the new
`sync-lexical-name-mapping` case passes; `sync-api-writeback` explicitly records
the pending runtime and its shared-interface dependencies.
