# Directory synchronization contract

Sync HTTP controls share the management browser/Host guard: page-initiated
requests return 403 and unapproved public DNS hosts return 421. Their existing
all-owner project authorization remains independent of fixture administration.
See [HTTP request boundaries](http-adapter-boundaries.md).

<!-- markdownlint-configure-file {"MD013": false} -->

Contract updated: **2026-10-31**. Scope: DESIGN-062, DESIGN-072 and DESIGN-074
remain PARTIAL; DESIGN-076 remains IMPLEMENTED. Root activation precedes bind;
all-owner export, external create/edit ingestion and continuous API journal replay
run after ready. The [external deletion and controls slice](external-deletion-controls.md)
adds fixed-revision deletion approval, protected restoration and live
status/deletions/rescan/approve routes. The current runtime also connects
revision-bound findings, strict project-owner management authorization, finite
waits and registered conflict resolution; see
[continuous observation and runtime controls](continuous-directory-observation.md).
The [conflict backend](sync-conflict-recovery.md) and
[native restart recovery](native-writeback-recovery.md) describe owned-state
boundaries. Protected workspace transfer is integrated and exercised through the
live CLI. The final combined platform/SDK checks remain open; component test
success alone does not complete the parent features.

## Local-development scope

The table and chronological implementation sections below record the earlier
initial-export increments. Their PENDING annotations describe those checkpoints;
the current component documents above and the feature ledger determine current
availability. The later acceptance sections remain the safety contract.

| Acceptance case | Local-development subset | Deferred scope (PENDING) |
| --- | --- | --- |
| sync-configuration | Root selection and the boolean deletion guard are configurable; precedence, boolean parsing, removed keys, overlap and CLI-only reinitialization are checked. One-second settle is fixed. Strict startup requires the local registry. | Exhaustive platform/container coverage. |
| sync-initial-activation | Export all owners, empty buckets and immutable live bodies; ingest external-only files/default buckets, preserve/count divergent files and finish a complete baseline. Resume interrupted ingestion from atomic fingerprints without duplicate generations or deletion. | Conflict transfer and exhaustive crash boundaries. |
| sync-name-mapping | Exact lexical mapping, actual directory spelling, host component limits, prefix/type collisions and no-follow checks. | Exhaustive case/Unicode alias groups and non-Linux platform conflict guarantees. |
| sync-lexical-name-mapping | Existing exact round-trip/property coverage remains implemented. | Runtime concerns belong to sync-name-mapping. |
| sync-bidirectional-mutations | API create/overwrite/delete/copy/compose, external create/edit and rename's create side across buckets, binary/empty bodies, and fingerprint-checked updates/deletes that preserve externally edited files. | Bucket-directory deletion on API bucket deletion: the directory is kept. External deletion and rename's delete side are implemented behind the approval guard. |
| sync-api-writeback | Post-ready all-owner export followed by fenced journal replay; coalesce bounded pages against current live state, replace tracked files only, and reconcile journal retention/epoch gaps through the same bounded keyset scans. | Conflict transfer and ingestion. |
| sync-metadata-and-loops | Conditional ingestion inherits commit-time metadata/PATCH, applies seed MIME/default holds to new files, adopts equal live hashes without generations, and suppresses recorded output and sync-ingest journal echoes. | Exhaustive watcher/platform races. |
| sync-conflict-preservation | Leave divergent files untouched; ambiguous publication retains its pending intent/workspace. | Conflict transfer/registration, ordinary quota recovery and 1 TiB sparse-body cases. |
| sync-deletion-approval | Complete bounded scans freeze root-wide candidates and denominator; the fixed 100-object/strictly-over-50-percent guard, exact paged inspection, one-shot approval and same-transaction generation/membership checks preserve API winners. Protected deletion restores bytes without removing holds. | Complete protected-target findings and automatic retained-work recovery. |
| sync-management-authorization | Startup configuration is a trusted local operator action; current controls require the complete original-owner project-admin union in strict mode, rechecked at native execution. | Final combined platform/SDK evidence. |
| sync-status-and-wait | Runtime summary, revision-bound findings/deletion pages, remedies, CLI and finite scan/journal-fence waits are connected. | Final combined platform/SDK evidence. |
| sync-bounded-observation | Dedicated OS workers, at-most-256-entry catalog/directory/journal pages, coalescing and gap recovery; internal rescan compares fingerprints before staging and hashes only changed/racy files. Native Linux/macOS notifications also trigger rescans without idle polling; see continuous-directory-observation. | Linux runtime, exhaustive overflow injection and bind-mount coverage. |
| sync-marker-recovery | Missing/foreign/malformed markers fail before bind; exact-ID missing-marker reinitialization. | Exhaustive process-kill/host-power-loss boundaries. |
| sync-writeback-recovery | Durable pending intents, checked exchange/removal and synchronized fingerprints; pending-work-only retries at 1, 2, 4, 8, 16, then 30 seconds preserve source descriptors and API/external bodies. | Automated interrupted-operation recovery, every-instruction kill coverage, sparse limits and non-Linux conflict guarantees. |
| sync-shutdown-lock | Stop between objects, finish active side effects before catalog close, and retain root ownership; live API-write/SIGTERM/restart convergence and initial-export/listener-error drain are tested. | Exhaustive scan/publication/conflict cancellation and lock-boundary coverage. |

Initial export does not choose a winner by overwriting an existing target.
Filesystem aliases detected by exact directory spelling or no-replace failure
leave existing bytes intact. A case/Unicode alias discovered after a preceding
catalog member was exported can leave that first file in place; classifying all
members before any export is PENDING. Legacy buckets rejected by the native
typed bucket API are counted as unrepresentable and skipped; per-object durable
findings for those legacy buckets require a later shared typed interface.
A tree has no entry or depth limit by default (`max_sync_entries` and `max_sync_depth` are 0),
so time grows with the tree. A named `max_sync_entries` (up to 2^64−1) also bounds one
directory, and a named `max_sync_depth` (up to 1024) counts levels below the root, the bucket
included. Pages stay at 256 and individual components at 255 bytes, or the root filesystem's
lower limit where the startup probe finds one. A scan, a watch and export
all accept exactly that depth. Without a limit a walk still stops at 1024 levels, which keeps its
descriptors bounded and is deeper than any object name can nest. Past 10,000 entries without a
limit, the process logs one `sync_tree_large` warning. Exceeding an inventory bound
stops reconciliation without inferring deletion. No catalog-sized in-memory
inventory is built. Linux runs the sync integration tests in the
`x86_64-unknown-linux-musl` CI lane and the container smoke's sync stage on
`linux/amd64` and `linux/arm64` ([container verification](#container-and-linux-verification)).
Linux exchange/displaced-inode races and non-Linux conflict guarantees remain PENDING.

Unfinished write-back stops new exports. Preserve the working tree and reserved
workspace, inspect current conflict/recovery findings, correct storage access
and resolve the exact retained state before an explicit rescan. No unknown
pending body is automatically deleted. A changed journal fence, divergent file,
unimported external-only file or tree entry that no object can take prevents
initial baseline completion. An API object the tree cannot hold is a finding and
does not ([deletion inventory](external-deletion-controls.md#deletion-inventory)).

Ordinary shutdown first stops accepting connections and drains HTTP requests,
then signals the mirror and waits for its current object to finish before closing
the catalog. The mirror checks shutdown before the next prepare and never drops
an armed operation to satisfy a network deadline. Background drain has no deadline,
matching the existing catalog-owned filesystem drain contract.

### Continuous API export

`sync-live-export` captures a journal cursor before the initial export, then
reads at most 256 committed changes per page. A bounded set selects each affected
bucket/object once and reads its current live generation; later pages observe
identical bytes without replacing the file. `sync-ingest` origins are skipped.
Retention-floor and epoch changes trigger the existing all-owner keyset scan,
including indexed synchronized-state pages to find API-deleted objects. The
cursor remains at the pre-scan fence so writes during the scan are replayed.
No debounce timer or idle polling is used. API bucket deletion leaves its
directory in place; deleting directories is PENDING.

Updates compare the tracked size, nanosecond mtime and stable MD5/CRC32C before
replacement; ctime must remain stable during verification. The inode number is
not compared, because Docker Desktop shared folders can report a new number for
an unchanged file. Checked
exchange retains the displaced body until
its identity and hashes are confirmed. API deletion first moves the checked
file into the reserved workspace, validates it again and only then unlinks it.
Directory sync precedes the fingerprint/state commit. A divergent target is
left intact, logged with a preservation remedy and counted as a conflict;
`export-conflict` findings record both overwrite and deleted-object cases.
Before catalog schema 58 these cases were recorded as `collision`, the reason a
name collision also uses; a stored row cannot tell the two apart, so schema 58
turns every such row into `unclassified-collision`. That finding stops `sync wait`
like an export conflict until the object is observed again: a name collision found
again replaces it with `collision`, and an edited file found again by ingestion,
which runs before the wait decides, replaces it with `export-conflict`.
If the API advances after installation, state records the generation and
fingerprint actually installed, allowing the next live generation to replace
those known bytes without treating the export as an external edit.

Additional storage methods implement API-authoritative deletion without changing
the existing baseline-complete requirement for other removal callers. No schema,
startup order, configuration key, control route or ingestion path changes.
The existing shared pending ledger reserves the operation before any file write.
Each transition owns the root/catalog lease until its file/SQL work settles.

Before displacement, retryable I/O failures log their cause and retry twice
with one pending operation. Persistent failure pauses intake with controls
available. A post-displacement failure protects and fully charges surviving
workspace bytes; old operation handles cannot continue destructive cleanup.
Inspection, verified conflict transfer or explicit confirmed discard and a new
rescan provide recovery. Backoff is interruptible by shutdown and no idle retry
loop runs while paused. Exhaustive instruction-level kill/physical power-loss
coverage and Linux/Windows runtime parity are not claimed.

Identical-body PUTs and metadata-only changes that find the file still holding
the live body are observations. They use the same retained live-export
operation for prepare, arm, durability and acknowledgement retries; they never
restart a partially completed export from prepare. Observations make no copy or
replacement. An external edit before observation durability or acknowledgement
cancels only that non-writing intent and preserves the file. A metadata-only
change that instead meets a divergent file, such as an edit that ingestion
deferred because of a hold, is exported like any other API change: the edit is
preserved as a conflict and the file returns to the API bytes (see
[Ingestion, metadata and conflicts](#ingestion-metadata-and-conflicts)).
A parent directory removed or moved after prepare also cancels the observation, leaves no pending
intent and lets other buckets progress; observations do not require that parent
to remain present for a replacement they never perform. File/ancestor
permission and transient I/O failures retry inspection without recording a
permanent mapping finding or advancing the journal cursor. Permission repair
may change ctime; matching size, mtime and stable MD5/CRC32C still prove the
tracked body before checked replacement. Symlinks, special files and type
collisions retain their structural findings.

Bucket disappearance or incarnation changes between binding, tracked-path lookup
and prepare are stale observations. Prepare rechecks the captured binding in its
short transaction before reserving an intent. The key is deferred for later
journal replay while other buckets in the current page continue; a same-name
bucket never inherits the old binding or source body.

### Live-export verification

Verified on 2026-10-25, macOS ARM64, Rust 1.99.1. Parent rows remain PARTIAL.

| Command | Result |
| --- | --- |
| `cargo test --locked --offline --lib --test sync_live_export --test sync_mirror -- live_export initial_export runtime_mapping export_concurrent_api` | PASS on source `9d402b4`: 15 tests (7 library, 4 live-export and 4 initial-export/mapping), including same-body PUT/metadata PATCH prepare/arm/durable/ack retry, file/parent permission repair, observation-only external edits, three bucket deletion/recreation boundaries and continued export to another bucket. Existing API operations, external-body preservation, coalescing, gaps, notification-only waiting, eight injected update/delete/conflict failures and SIGTERM/restart also pass. |
| `cargo clippy --locked --offline --lib --tests -- -D warnings` | PASS. |
| `cargo xtask dependencies check` | INCOMPLETE for this repair: Rust, direct Rust dependencies, Actions/container inputs, Go language/toolchain, Storage and OAuth modules CURRENT; exit 2 on the Go API new-major probe timeout. No dependencies or lockfiles changed. The original live-export slice's full check passed. |
| `just check` | PASS on final source `f96e678`, run once after the parent-disappearance correction: formatting, workspace clippy/tests/doc tests, ledger/generated-view validation and shell checks. The earlier source `9d402b4` also passed a separate run before that correction. |
| `cargo xtask compatibility render` / `check` | PASS; both language matrices were regenerated and remain unchanged because parent status/fidelity is unchanged. |
| `npx --yes markdownlint-cli2 docs/en/slices/directory-synchronization.md` | PASS with the existing MD013 exception for contract tables. |

SDK, container and full-guide executions are omitted because Google HTTP
contracts, container inputs and runnable guides are unchanged. No benchmark,
real disk-full fixture or performance claim is made.

The added regressions are
`live_export_retries_identical_observation_at_prepare_arm_durable_and_ack`,
`live_export_identical_observation_preserves_edits_before_durable_and_ack`,
`live_export_retries_file_and_parent_permissions_without_advancing_the_cursor`
and `live_export_bucket_incarnation_races_keep_other_buckets_progressing`.
`cargo test --locked --offline --lib -- live_export` also passes all 7 tests on
`f96e678`, including
`live_export_identical_parent_disappearance_cancels_and_keeps_other_buckets_progressing`
for parent removal/move after prepare, continued export to another bucket and
an empty pending ledger. The parent-disappearance regression failed before the
correction with `called Result::unwrap() on an Err value: Elapsed(())`.
The dependency probe stopped with `curl: (28) Operation timed out after 30006 milliseconds with 0 bytes received`
for `https://proxy.golang.org/google.golang.org/api/v2/@v/list`; this is not a full
freshness PASS. No SDK, container, guide or benchmark was rerun for this repair.

The source generation is opened before preparing an intent. A disappeared or
changed generation is deferred, and `deferred` counts also prevent baseline
completion. API copies are staged under durable Prepared intent before arm;
staging does not touch the target. Private copy provenance permits abandonment
only when the copy was never installed and its workspace stat identity and body
remain unchanged, or when an identical-file observation made no copy. A changed
inode number alone is accepted once the same handle rehashes the expected body,
because Docker Desktop shared folders can report a new number for an unchanged
file; installation applies the same check. Existing external
targets are never unlinked. Installed copies cannot be abandoned. The legacy
unpublished-copy interface can complete an obsolete installed API copy without
claiming a synchronized fingerprint or progress. Continuous live export instead
records the generation/fingerprint actually installed, so a later API generation
can replace those known bytes. Changed or displaced external bodies still require
protected recovery, not cancellation.
External deletion/edit/touch between rename and acknowledgement can make both recovery paths refuse completion, retaining an Armed/Durable root stop; the remedy belongs to sync-conflicts (PENDING).
An obsolete completion counts as `deferred` in that pass and leaves baseline incomplete; live journal replay or the restart scan reconciles the next API state.

### Initial-export verification

The validation-only suites now share the `it_admin` integration binary. Run
`cargo test --locked --offline --test it_admin sync_configuration` to select
one. The name-mapping cases moved into `cargo test --locked --offline --lib
sync::mapping` once the mapping took the root's probed naming rules.
Historical verification commands below retain their original binary names.

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
| `sync_dir` | Unset: disabled | Nonempty existing writable directory, not a symlink. This is the only required setting to enable sync. Canonicalize and reject equality or ancestor/descendant overlap with `data_dir` or `seed_dir`, including aliases. Reserve and revalidate the root identity rather than following a replaced root. A data directory synchronizes one directory: another one takes its place only when it holds this data directory's marker (merged) or no subdirectory (switched); see [marker and activation requirements](#marker-and-activation-requirements). |
| `sync_delete_guard` | `true` | `false` disables only the count/percentage guard. Marker, complete-scan, incarnation and generation protections remain mandatory. |
| `max_sync_entries` | `0` (unlimited) | `0..=2^64−1`; requires `sync_dir`. Files and directories in `sync_dir`, also the watches Linux may use. A scan keeps one page per directory level, so a larger tree takes longer but holds no more memory. |
| `max_sync_depth` | `0` (unlimited) | `0..=1024`; requires `sync_dir`. Directory levels below `sync_dir`, counting the bucket. Without a limit a walk stops at 1024. |

Local development rarely needs tuning, so the internal policy is fixed at a
1000 ms quiet deadline, a deletion guard for at least 100 objects and strictly
more than 50%, and retained/in-progress conflict limits of 1 TiB and 1,000,000 entries, the
most the schema accepts. Reopening a root replaces a budget that a catalog stored under the
former limits. Exporting over a tracked file reserves its whole size, so a tracked file larger than 1 TiB,
under the 5 TiB object limit, pauses its root with nothing to resolve.
Zero-byte conflicts still consume an entry; no automatic eviction is permitted.
Settling is a stability heuristic: stat before/after staging and retry changed
input, rather than claiming an atomic snapshot of an active writer.
This owner decision replaces section 7's provisional tuning recommendations
to keep the local-development configuration small.

`sync_settle_ms`, `sync_delete_min_objects`, `sync_delete_percent`,
`sync_conflict_max_bytes` and `sync_conflict_max_entries` are not settings.
Their CLI, environment and JSON spellings are rejected as unknown settings,
including when a higher-precedence valid setting is supplied.
Supplying `sync_delete_guard`, `max_sync_entries` or `max_sync_depth` without `sync_dir`
is an error, with a fix to supply the root or remove the setting. The guard alone never enables sync.
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

### External create/edit delivery scope

The ingestion increment accepts empty-catalog activation through `publish_objects`,
external-only files, new default buckets, binary/empty bodies and rename's create
side. A one-second stable observation precedes staging. Dedicated OS workers walk
bounded pages, stat unchanged fingerprints without reading bodies, and stage only
changed/racy files. Conditional publication inherits current metadata and skips
equal hashes; API body races win and divergent files remain untouched with a
durable `export-conflict` finding. Interrupted activation resumes from atomic sync state.
The runtime exposes an internal async rescan and native Linux/macOS observation;
startup scans after ready and there is no periodic scan. The
[continuous observation slice](continuous-directory-observation.md) records the
watch bounds, lease lifetime, retry and shutdown contract.
That historical ingestion increment preceded strict controls. Current strict
startup uses the normal local registry and complete project-owner authority;
see [current runtime controls](continuous-directory-observation.md).

External disappearance effects, deletion guards, control routes, conflict transfer
and exhaustive crash boundaries remain PENDING. SDK, container and guide journeys
are out of scope and are not run for this increment.

`Task::rescan` serializes requests through a one-entry channel and returns counts;
there is no HTTP/CLI endpoint or periodic scan. A depth-bounded cursor stores only
one name per ancestor and hands at most 256 observations to async code. The scan
stops at a named `max_sync_entries` or `max_sync_depth`, and otherwise at 1024 levels. A quiet interval is
applied only to changed/racy candidates in a page and wakes on shutdown; unchanged
files require no timer or body read. Descriptor-relative identity checks run before/after
shared staging; a changing or replaced file is deferred to the next requested
scan. Symlinks/special files are counted and logged without following them.

Each publication uses `InheritAndSkipIfSameHash`, an observed generation/absence
condition and catalog-incarnation-bound `SyncUpdate` in the same transaction.
Equal live size/MD5/CRC32C updates only the observed fingerprint. A racing API body
fails the condition; a racing metadata PATCH remains visible to inheritance.
Unfinished write-back blocks ingestion before scanning. Shutdown stops between
objects and drains the active publication, leaving atomic fingerprints for restart.
The existing all-owner export completes the baseline after the initial merge;
an unfinished baseline can also complete after an internal rescan.

Schema 27 adds `sync_buckets(root_id, bucket, incarnation)` because object sync
records are removed by ordinary API bucket deletion and cannot prevent accidental
recreation on restart. The new independent observations retain known bucket names
after API deletion. A directory with a known but absent API bucket is skipped,
counted as `blocked_buckets` and diagnosed with a remedy: remove/move the
directory or explicitly recreate the API bucket. Other buckets continue. While it
holds an entry other than an ignored name, it is also an entry no object can
take, which stops inferred deletions and counts once among
`unrepresentable_entries`; `blocked_buckets` breaks that count down, and the
entries below the directory are not counted. An empty one holds nothing back and
stays in place. Observing
the directory absent clears only that observation, permitting deliberate later
directory recreation; it never deletes a bucket or object and never runs `rmdir`.
Per-path read/access failures are logged with a relative path and a
permission/space remedy, leaving other candidates runnable. One that passes by
itself, such as an entry that went away or was busy while the pass read it, is
deferred to a later pass. One that waiting does not clear, such as a missing
permission, a full disk or a kind the mirror does not know, holds back the
baseline and deletions like an entry no object can take, until an operator
restores the entry.

### External create/edit verification

Native macOS ARM64 checks on 2026-10-27:

| Command | Result |
| --- | --- |
| `cargo check --workspace --all-targets --locked --offline` | PASS |
| `cargo test --locked --offline --lib -- sync::ingest sync::mirror` | PASS: 18 tests |
| `cargo test --locked --offline --lib -- sync::ingest sync::mirror schema_migration storage::catalog_tests sync_state change_journal import_journal import_publication` | PASS: 108 tests |
| `cargo xtask dependencies check` | PASS: compiler and all checked inputs CURRENT |
| `just check` | FAIL (exit 101): obsolete IAM schema expectation (`27 != 26`), after native/Windows/Linux clippy, 502 library tests and 9 startup tests PASS. Corrected migration expectations pass below; the whole command was not rerun after test-only corrections. |
| `cargo test --locked --offline --lib ingest_unchanged_files_skip_settle_and_shutdown_interrupts_changed_settle` | PASS: enters the pending quiet wait before notifying shutdown |
| `cargo test --locked --offline --test auth_http --test seed --test object_versioning_http --test bucket_http -- iam_migrates_v5_catalog populated_v4_catalog_migrates_without_seeding versioned_schema_migrations_preserve_objects bucket_core_migrates_v7_settings` | PASS: 4 migration tests |

The tests cover initial binary/empty import, default bucket ownership, runtime
rescan/baseline/restart, rename's create side and retained API objects after file
deletion, divergent/external-only summaries, ignored/unsafe entries, equal-hash
generation suppression, unchanged/racy fingerprints, seed MIME/default hold,
metadata PATCH/API-body publication races, replacement-save deferral, interrupted
activation, active-publication shutdown and empty staging after restart.
Additional cases retain API-deleted buckets across restart/rescan, allow deliberate
directory removal/recreation, continue past unreadable files, skip unchanged-file
settling, interrupt changed-file settling on shutdown, and migrate schema 26 to 27
without changing existing bucket metadata.
The quiet-wait test polls past its released checkpoint before sending shutdown,
and four historical migration fixtures expect schema 27. These final edits affect
tests only; remaining workspace integration suites were not rerun afterward.
SDK/container/guide journeys are not run because their surfaces are unchanged.
No benchmark, performance number, Linux/Windows runtime or host-power-loss result
is claimed. External deletion, conflict transfer/control, watcher events and
exhaustive interruption boundaries remain PENDING.
The catalog sync-path validator rejects backslash/colon. The lexical mapper now
returns `HostRestriction` for those components on every OS, so export records a
finding and continues and ingestion counts such files as unrepresentable instead
of stopping the root. The storage validator is unchanged.

Superseded on 2026-11-19 for colons: the catalog validator now rejects only
backslash and NUL, and a colon maps wherever the root's probed filesystem stores
it; see [Name and path reversibility](#name-and-path-reversibility).

### Container and Linux verification

Checks on 2026-10-28 against the source that skips backslash/colon names:

| Command | Result |
| --- | --- |
| `cargo test --locked --offline --lib sync::mapping` | PASS: 5 tests; backslash, colon and timestamp-style names map to `HostRestriction` in both directions. |
| `cargo test --locked --offline --test sync_live_export --test sync_mapping --test sync_mirror --test sync_configuration` | PASS: 17 tests, including `live_export_skips_colon_and_backslash_names_and_keeps_exporting`, which failed before the mapping change because the root stopped at the first colon name. |
| `bash scripts/smoke-containers.sh` | PASS on `linux/arm64` (native) and `linux/amd64` (emulated); see [evidence](../evidence/sync-dir-containers.txt). The sync stage runs a read-only, capability-free container with sibling data/sync directories in one volume and checks API create/overwrite/copy/delete export, a skipped colon name, external create ingestion after restart and exit code 0. |

The `x86_64-unknown-linux-musl` CI lane already ran `sync_configuration`,
`sync_live_export`, `sync_mapping` and `sync_mirror` on Linux before this change
(3, 4, 2 and 7 tests PASS on main at `a64dd5a`).

Manual Docker Desktop checks on macOS ARM64 also passed for a host bind mount,
the published `0.2.1` image and a non-default `--user` with data and sync
directories owned by that user. Exported entries use modes `0600`/`0700`, so a
Linux host user reads them only when the container runs with that user's UID.
A replaced data volume with the old directory fails before bind with
`ForeignMarker`. Ingested files become visible about one second after readiness.
External deletion, conflict transfer, watcher events, Linux exchange races and
host-power-loss behavior are not covered.

The colon checks above describe the source of that date. Since 2026-11-19 the
renamed `live_export_exports_colon_names_and_skips_backslash_names` and the
container smoke expect colon names to export and external colon files to be
ingested; only backslash names are skipped.

### Root naming probe verification

Checks on 2026-11-19, macOS ARM64 on APFS, against the source that probes the
root's naming rules (see [Name and path reversibility](#name-and-path-reversibility)):

| Command | Result |
| --- | --- |
| `cargo test --locked --offline --lib -- sync:: storage::catalog::sync_state storage::catalog::lifecycle server::admin::sync fsutil source_tree` | PASS: 434 tests, including the probe, mapping, parent-walk and catalog-path cases. |
| `cargo test --locked --offline --test it_sync -- sync_live_export` | PASS: 10 tests, including `live_export_exports_colon_names_and_skips_backslash_names`. |

Colon-refusing, case-folding and name-rewriting roots are injected
observations, since the host has none of them; the host probe itself reported
case folding, a stored colon and a 255-byte limit. A crash or power loss at each
of the probe's nine boundaries, followed by restart, recovery and a scan, leaves
nothing taken for data. Linux, Docker bind mounts and the container smoke were
not run for this change.

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

The `sync-live-export` increment accepts journal replay and coalescing, bounded
gap reconciliation, fingerprint-checked API create/overwrite/delete/copy/compose,
retained divergent files, pending-work-only I/O retry and object-boundary shutdown.
Its acceptance tests cover each operation, a journal gap, external edits during
overwrite and delete, injected write failure, notification-only idle waiting and
SIGTERM followed by restart reconciliation. Bucket deletion keeps directories.
External ingestion, conflict registration/transfer,
sync control API/CLI and filesystem ingestion remain PENDING. The boolean guard
is validated now; fixed settle/delete behavior belongs to the pending ingestion
scope. Fixed conflict limits already constrain shared write-back reservations.

Each ID below is a row-local case in the named feature. These specify future
executable tests, not results obtained by inspecting this document.

| Case | Feature | Required positive, negative and boundary behavior | Owning slice |
| --- | --- | --- | --- |
| sync-configuration | DESIGN-062 | Only sync_dir and sync_delete_guard; defaults and CLI/environment/file precedence, strict boolean parsing, removed tuning keys rejected as unknown, guard without root, overlap/alias/root substitution rejection and startup-only reinitialization; internal values fixed for local development | sync-mirror, sync-live-ingest |
| sync-initial-activation | DESIGN-062 | Empty/nonempty catalogs, empty buckets, same/different/absent names, exact summary counts; interrupted activation resumes without replay/deletion; owner mismatch never changes ownership | sync-mirror, sync-live-ingest |
| sync-name-mapping | DESIGN-072 | Property-test name to path to identical name or explicit Unrepresentable; literal percent/backslash rules, dot/empty segments, ignored/reserved names, OS limits, case/Unicode aliases and file/directory collisions; no traversal, symlink or special-file following | sync-mirror, sync-live-ingest |
| sync-lexical-name-mapping | DESIGN-072 | Shared lexical name/path conversion rejects invalid, reserved, ignored and host-invalid components and preserves exact UTF-8 bytes on round trip. Property tests cover arbitrary names and paths. This is a candidate-path check; filesystem collisions, host length limits and descriptor-relative safety still require runtime validation. | sync-mirror |
| sync-bidirectional-mutations | DESIGN-072 | Multiple buckets; API create/overwrite/delete/copy/compose, external create/edit/delete/rename, independent bucket settings/history and zero-byte/binary bodies; deleted bucket directory returns without deleting the bucket | sync-live-export, sync-live-ingest |
| sync-api-writeback | DESIGN-072 | After ready, page all owners' buckets and live objects, mirror binary/empty bodies from opened immutable descriptors, consume fenced changes without idle polling, preserve conflicting external bytes and owner/settings/history, recreate empty bucket directories, and recover a gap through bounded reconciliation. | sync-live-export |
| sync-metadata-and-loops | DESIGN-072 | Atomic metadata inheritance including concurrent PATCH, new-file MIME/default holds, same-hash no new generation, live-hash recovery and origin/fingerprint loop suppression | sync-live-ingest |
| sync-conflict-preservation | DESIGN-072 | API priority, immutable conflict body/identity, exact/exceeded byte and entry caps, disk-full and zero-byte conflicts; a quota-exceeding last-check race after displacement retains the actual body in workspace with full quota accounting, stop/restart and operator recovery, never automatic cleanup/exchange-back; even a 1 TiB + 1 byte sparse body can be explicitly discarded before registration only with original-owner authority, exact recovery revision/identity and backup or no-backup discard confirmation; rejected/stale/unconfirmed requests retain bytes and quota, and only durable authorized completion permits root resumption; restore/discard cannot retarget or overwrite newer API state; cancellation/retry retains unresolved bytes and quota | sync-live-export, sync-conflicts, sync-live-ingest |
| sync-deletion-approval | DESIGN-072 | Count below/at threshold and fraction equal/above threshold, disabled guard and zero denominator; inspect all pages of one fixed deletion revision and approve exactly that set; an intervening tree/API/root/incarnation change or restart makes old cursors and approval return 409; holds restore files and preserve history | sync-deletion-guard, sync-live-ingest |
| sync-management-authorization | DESIGN-072 | Every control route and CLI operation in both profiles; strict missing/invalid/insufficient/project-incomplete identities denied before disclosure or mutation, including list/resolve 403 without the original conflict/recovery owner project's grant before/after its last bucket deletion, restart and cross-project same-name recreation; pre-registration recovery discard also rejects stale revision/identity or missing confirmation without changing retained bytes; recheck authority/target on execution and restart retries, only authorized confirmed completion can release the recovery stop; signed/public/bucket-only access does not authorize administration | sync-conflicts |
| sync-status-and-wait | DESIGN-072 | All stop remedies; bounded fixed-revision status pages expose exact deletion candidates and Unrepresentable/ProtectedDeletion/recovery targets, fields and remedies; multi-page inspection matches approval, changed state rejects stale cursors/approval with 409; sanitized readiness, captured scan/journal fence, timeout/disconnect/shutdown without cancelling side effects; stopped work stays inspectable | sync-control-min |
| sync-bounded-observation | DESIGN-072 | Dedicated OS workers, bounded pages/path queue/diagnostics, fingerprint/racy hashing, journal coalescing/gap, duplicate/self/missed/overflow events, no idle polling, unchanged-file stat-only path and bind-mount behavior | sync-live-export, sync-watcher, sync-live-ingest |
| sync-marker-recovery | DESIGN-074 | Missing/foreign/malformed marker and wrong mount stop before bind; correct-root recovery and safe reinitialization, interrupted marker/state persistence and restart cannot interpret unobserved files as deletions | sync-mirror, sync-recovery |
| sync-writeback-recovery | DESIGN-074 | Barrier-kill copy/sync/exchange/parent-sync/state windows; Linux displaced-inode and absent-target races, including exact/exceeded byte/entry quotas, a 1 TiB + 1 byte sparse displaced body and ENOSPC before conflict registration plus kill/restart while stranded in workspace; retain/protect/account bytes, stop and recover without automatic cleanup/exchange-back; authorized confirmed pre-registration discard survives kill before/after durable intent, unlink, parent sync and completion, with idempotent retries and no premature quota/stop release; rejected authority/revision/identity/confirmation retains the body; other-OS race diagnostics/limits and retry; opened blobs stay immutable/readable and catalog microseconds remain intact | sync-live-export, sync-conflicts, sync-recovery |
| sync-shutdown-lock | DESIGN-074 | Shutdown during scan, staging, publication, conflict resolution and write-back drains owned side effects, cleans safe temporary files and retains catalog/root ownership until completion; second server cannot acquire either active root | sync-live-export, sync-recovery with Shared state |

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
file component. `/` is the logical separator on every OS. Backslash components
are unrepresentable on every OS because the catalog sync-path validator rejects
them; a backslash never acts as an implicit separator.
Do not change the API's wider accepted-name set to fit a filesystem.

A colon is mapped literally where the root's filesystem stores it. Each time the
mirror activates the root, after write-back recovery and before its first scan,
it probes the filesystem with fixed names in the fixed `naming-probe`
subdirectory of `.gcs-mimic-sync-work` and records one set of naming rules for
the root: whether `:` is stored as created, whether letter case folds, whether
Unicode normalization folds, and the component byte limit that
`fpathconf(_PC_NAME_MAX)` reports, capped at 255. An info-level
`sync_root_naming` log event reports them (`colon`, `case`, `normalization`,
`name_max`). Letter case folds when the lower-case spelling of a name the probe
created opens anything, and normalization folds when the decomposed (NFD)
spelling of a precomposed (NFC) name it created does: the probe unlinks those
spellings first and never creates them. Neither inode numbers, which Docker
Desktop shared folders can report anew for a name the probe creates again, nor a
clear casefold flag decide. A later probe of the same root
that observes other rules stops it as changed and logs `sync_root_naming_changed`
with the `recorded` and `observed` rules. No setting chooses or overrides these rules. Both mapping directions, parent walks,
target checks and deletion checks read that one value, and each rule is decided
on its own: a filesystem that refuses `:` makes only colon components
`HostRestriction`. A colon `HostRestriction` finding that an earlier start
recorded clears when a later start exports the object. The probe starts by unlinking its own fixed names, the folded spellings included, never
lists and deletes what it finds, and never removes its subdirectory; none of its
names has the 32-digit hexadecimal form of a write-back operation, so recovery
never takes a leftover for an interrupted operation.

A lookup below the root opens a name without following a symlink and confirms
that the entry it reached has that exact spelling; an entry reached under
another letter case or normalization is an alias. macOS reports the name of an
open file and a Linux directory reports its casefold flag; where neither
answers, the lookup lists the parent. The recorded rules only make that check
stricter, never looser: on a root that folds letter case or normalization, a
clear casefold flag is not believed, because a mount that folds without that
flag reports the same, and the parent is listed. On a root that folds neither,
each directory still answers for itself, and a directory whose casefold flag
query fails is listed: the probe measured one directory the mirror created, and
a mount that cannot report the flag can fold names in some directories and not
in others on one device, as a WSL2 `drvfs` mount with `case=dir` does for
directories created from Windows. A Docker Desktop bind mount from a macOS host
folds case and answers the flag query with `ENOTTY`, so its lookups list the
parent.

A listing is read in one pass over the directory and kept for the rest of one
pass of the mirror, such as one ingestion scan, one deletion inventory or one
export: up to 4 directories of at most 4096 names each, and nothing of the
files' contents. A kept listing answers only while the directory reports the
identity, size, modification time and change time it had before the listing
was read, so an entry created, removed or renamed in the directory during the
pass makes the next lookup read it again. A listing is kept only when the
directory's last change lies at least two seconds before the read, the coarsest
timestamp tick the mirror supports, so that a later change cannot leave the
timestamps as they were; a directory that just changed, or that holds more
names, is read for each lookup. The scan itself confirms no spelling of the
names it lists, since each comes from its directory's listing: ingestion opens
a changed file again under its exact spelling before it reads it, so an entry
renamed between the listing and that open is deferred as a changing source,
and the next scan takes it in under its new name.

A filesystem that stores a created name under another spelling, in letter case
or Unicode normalization, cannot keep the mapping reversible. The mirror pauses
with `paused_reason` `observer-failure` before its first scan, so nothing is
ingested from that tree. The `sync_paused` reason says that the directory cannot
be a synchronization root, that a rescan pauses again, and to set `sync_dir` to a
directory on a filesystem that keeps file names as created and restart. A probe
that cannot write its workspace pauses with `io-failure`. The Storage API stays
ready in both cases.

A file created outside the API with `:` in its name is ingested as a new object
with that literal name. macOS Finder displays a `:` in a file name as `/`, a
remnant of classic Mac OS paths; the name on disk, which the mirror reads and
writes, keeps the colon.

| Reason | Cases and behavior |
| --- | --- |
| `InvalidName` | Invalid UTF-8 on disk, invalid API name, NUL, leading/trailing `/`, empty component, `.` or `..` component; report, do not clean into another name. |
| `HostRestriction` | Backslash on every OS, a colon where the root's filesystem refuses or rewrites it, host-invalid characters, Windows device names/drive/UNC syntax, trailing spaces/dots where not reversible, components over the probed limit (at most 255 bytes) and path length limits; retain API state, report the exact restriction. |
| `Reserved` | Root marker `.gcs-mimic-sync.json`, staging marker `.gcs-mimic-sync.json.staging`, workspace `.gcs-mimic-sync-work` and any reserved component at any object depth; reserve root names against legacy bucket names too. |
| `Ignored` | Regular files named `.DS_Store` or `4913`, beginning `._`, ending `.swp` or `~`. Use one shared predicate for ingestion and API write-back. Traverse ordinary matching directories such as `._assets/`; they are not ignored files. |
| `Collision` | `a` and `a/b`, aliases differing only by case or Unicode normalization on the actual filesystem, bucket aliases, or a conflicting directory/file type. Report all members, never choose one silently or delete either API object. |
| `UnsafeEntry` | Symlink, junction/reparse redirect, special file, or changed ancestor/root identity. Never follow it or infer deletion from unreadable/unsafe traversal. |

Atomic marker publication additionally reserves `.gcs-mimic-sync.json.staging`
at every component depth, including below bucket prefixes. An object previously
mapped through that component is now `Unrepresentable(Reserved)`; its API bytes
remain stored and are not deleted by the mapper.

Reserved workspace subtrees alone are excluded as directories. An API regular
object whose final component matches an ignored-file rule is `Ignored`, even
if present from old seed input. A matching directory in a prefix is traversed;
its child may be representable. Two API names causing a file/directory collision
are both unrepresentable until the collision is removed. Detect aliases before
writing, including an existing external alias, and revoke any previously mapped
member without treating its disappearance as an external deletion. Existing
files in an ambiguous group remain untouched pending operator resolution.

An external directory at an exported object's target name gives way only when
it holds nothing but Finder's metadata files and nothing the catalog records
refers to it or to a path below it: no tracked file, path stop, unfinished
write-back operation or retained conflict. Finder's metadata files are
`.DS_Store` and `._.DS_Store`, the AppleDouble file that holds the former's
extended attributes on a volume that cannot store them. The export lists that
directory, never below it and at most eight names, and goes on only when every
name is a regular file named exactly one of those; any other entry, more names,
or a listing that fails keeps it. The export then removes that one directory
under its armed operation: it unlinks each metadata file relative to the
directory's descriptor without following a symlink, and only while the name
still holds the file the listing opened, and syncs the directory. It then
checks that the name still holds the directory the listing read, lists that
directory once more, and removes it with `rmdir` relative to the parent's
descriptor, which never follows a symlink and never removes a directory that
holds an entry. A name that holds another directory by then fails the attempt,
and the retry proves that one. It syncs the parent and installs the object in
its place. This is how an object `a` created through the API after `a/x` was
deleted there reaches the tree without operator action, even after Finder has
shown `a/`. A directory that someone else removed first (`ENOENT`) counts as
removed: the installing rename never replaces an entry, so a file or directory
that took the name meanwhile fails that attempt and keeps its contents.
Those two names are the only entries ever removed from such a directory;
nothing else in it is removed, no ancestor that the removal empties is removed,
and neither a bucket's own directory nor a reserved name is ever removed. The
set of removable names lives in one constant, and each is a name that scans
ignore, so removing one loses nothing the tree would publish. Other ignored
names, such as any other `._*` or an editor's swap file, may hold a user's data
and keep the directory. The listings, not `rmdir`, are what keep a directory of
AppleDouble files: on macOS, `rmdir` removes a directory that holds only `._*`
files of names it does not hold, and those files with it. A `._*` file created
between the second listing and `rmdir` can still be removed that way; no check
closes that window.
A directory that holds anything else, a `.DS_Store` that is a directory or a
symlink, a directory the catalog still refers to, and a removal the filesystem
refuses (`ENOTEMPTY`, `EEXIST`, `EBUSY`, `EACCES` or `EPERM`) all stay as they
are and leave a `directory-occupied` finding (catalog schema 60) for that object
alone, whose remedy asks to move the directory's entries out rather than to
rename the object; other objects continue. A metadata file already unlinked when
the removal finds another entry, such as one Finder wrote again, stays
unlinked; Finder recreates it. A crash or power loss between the unlinks and
the removal leaves the directory empty, and the next pass removes it. A
directory under another spelling stays and leaves a `collision` finding, as any
alias does. A symlink at the name is never followed or removed and leaves an
`unsafe-entry` finding. A directory where a tracked file was is no export
target: it stops that path as `EntryReplaced`. A `sync_directory_kept` warning
names the bucket, the object, the check that kept the directory (such as
`tracked`, `not_empty`, `unlisted` with its error and system error code, `busy` or
`permission_denied`) and the finding's remedy; an info `sync_directory_removed`
event names each directory the export removed and how many metadata files
went with it (`metadata_removed`).
Catalog references compare names
byte for byte, so a reference under another letter case on a root that folds
case does not keep the directory; the removal itself still refuses one that
holds an entry. On a root that the probe found to fold letter case or
normalization, the parent must also list the target's exact spelling before the
directory goes, as for any lookup there: a Docker Desktop bind mount cannot
report the spelling, and a Linux mount that folds case without the casefold flag
reports any spelling as exact; removing `photos/` for an object `Photos` would
put a file where the parent of a tracked `photos/a.jpg` was.

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
| Write-back and pending-operation API | The runtime and its tests use begin_live_export/advance_live_export/recover_pending for absent, identical, replaced or removed targets. Verified unpublished copies can be cancelled; modified or retained bodies are protected. Completion updates tracking and deletes its intent, without a separate journal cursor. Shared contract: [shared-sync-writeback](shared-sync-writeback.md). |
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
marker, stops startup before bind. A data directory synchronizes one directory,
because tracked files carry no root: ingestion takes a file that differs from its
tracked file while the object kept its generation for an edit, and a complete
baseline takes a missing tracked file for a deletion, so tracked files are only
ever compared with the tree they were recorded in. Startup decides in one place
how it takes the selected directory:

1. The registered root, at its path with its identity and marker, opens as it
   is.
2. Otherwise a directory with this catalog's marker, such as the previous
   directory mounted back or a root moved with `mv`, is merged under the
   marker's root, which keeps the marker.
3. Otherwise a directory with no subdirectory, no symlink and no special file,
   only files directly in it that a scan never makes objects of (`.DS_Store`,
   `.gitkeep`, `README.md`), gets a new root. This is looser than what live
   export removes, which is only `.DS_Store`, because startup removes nothing.
4. Otherwise a directory without a marker is refused with
   `ReplacementOccupied`, which names the registered paths.
5. Another catalog's marker is refused with `ForeignMarker`.

A catalog that an earlier release let register several roots keeps only the one
it opens. Every change, whether a merge, a new root or several roots reduced to
one, forgets in one transaction every registered root's rows (findings, path
stops, deletion sets, conflicts, recovery workspaces and write-back state) and
every tracked file, and registers the selected root with an incomplete baseline.
Its first pass then merges the tree as a first activation does: differing files
become conflicts, objects without files are exported, and nothing is deleted.
An object deleted through the API while another directory was synchronized comes
back from the merged directory's file. For a new root, startup lists the
directory once for the proof and stages its marker before the transaction, which
first rechecks that the registrations are the ones it replaces; when they are
not, it registers nothing and removes the staged marker. A crash before that
transaction commits keeps the previous registration, and the next start removes
the staged marker no root owns and switches again; after it, the next start
publishes the staged marker. The `sync_root_switched` warning names the reason,
both paths and what was forgotten, listing the first retained conflicts and
recovery workspaces with the paths of their bytes, and `sync status` reports the
path and the change made by the current process. A previous directory is never
opened, so its files and the bytes in its workspace stay, and no object changes.
`--sync-reinitialize` never switches. First repair the mount/root. For intentional reactivation, stop other writers,
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
mtime and ctime match recorded output, together with either the inode on a
stable observation or the hash. Journal origin `sync-ingest` skips
body write-back, while still advancing state/cursors. Origin alone cannot mask
an independent later API mutation.

API mutations win over divergent external edits, including metadata-only changes
and initial activation. Preserve known divergent bodies first under opaque conflict IDs in dedicated
`data_dir/sync-conflicts`, never `staging/`, `blobs/` or ordinary working-tree
paths. The reserved workspace may temporarily retain a newly displaced body
only under the recovery boundary below; it is never an ingestible conflict file.
Store catalog/root identity, logical name, original owning project, observed
generation/incarnation, fingerprint, hashes and reason with it. Persist that owner
identity for pending recovery as well as registered conflicts; deleting the bucket
or recreating its name in another project cannot erase or retarget it. API deletion
can therefore win without destroying the external body. Retained version history
follows ordinary shared publication.

A temporary or event-based hold, or a retention period that has not expired,
makes the publication of a local edit fail. The pass counts the edit as
`deferred`, reports `SourceChanging` and leaves the file as edited; a later pass,
started by a tree change, a rescan or a wait, tries it again. A metadata-only API change of a held object,
the release of the hold included, is an API mutation like any other: its export
preserves the edit as a conflict and returns the file to the API bytes, whose
generation does not change. It records no mapping finding, and the pass that
next observes the file clears the scan stop while the baseline stays complete.
Once the hold is released, `resolve restore` publishes the preserved edit.
Retention expiry writes nothing to the catalog and the observer runs no timer,
so an edit deferred by retention is ingested only on the next tree change,
rescan or wait after the retention expires; until then the status keeps
reporting `SourceChanging`.

Reserve bytes and one entry before capturing; stream and enforce the byte cap
even if the source grows. Exact cap usage is allowed, overflow stops ingestion
and any write-back that would discard external bytes. Disk-full/sync failures
leave the source intact and expose a stop. Never evict old conflicts automatically.
If capture of an already observed divergence cannot complete, do not overwrite the
external file to finish mirroring. When the file changed or went away during capture,
or changed again after it was preserved, the live observer runs that export again,
with a new capture, once the tree settles, unless a journal replay or an ingested
edit has moved the API or tracked generation since; a rescan runs it too.
Persist conflict registration before replacing
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
Let `A` be automatic deletions committed since the last safe history reset.
Guard when enabled and `A + D >= 100` and
`100 * (A + D) > 50 * (A + N)`, using wide arithmetic; `N=0`
has no deletions. Freeze this root-wide denominator and exact candidate set;
do not recompute the fraction per page or convert a marker failure into a scan.

Schema 50 persists `A` in the same transaction as each automatic deletion.
Existing catalogs initialize this new history to zero; earlier deleted names
cannot be reconstructed from the previous schema.
Approval resets it; a newly completed baseline or a complete observation scan
with zero candidates after established native quiet also resets it. Automatic
and explicit scans use that same condition. A scan triggered by the ten-second
cap without quiet, an incomplete scan or one with remaining candidates never
resets it. Restart and rescanning a guarded set never reset it. Approved-set
execution does not count as a new automatic deletion.
Reset also requires an unchanged native event generation and no pending event
between the scan's start and completion. Notifications read at either boundary
are retained for observation. A failed reset retains history, logs the original
cause and preserves the completed control response.
Public set counts still describe the current frozen members and baseline;
the threshold also includes the retained automatic history.

Native notifications wait for one second of quiet, restarting that interval
after another notification, with a ten-second total cap. Controls remain
available during this interval. Wait and rescan stay in a bounded actor queue;
status, approval and resolution remain available; approval and resolution return
busy if the bounded control queue is full. Wait retains its original
deadline and only reports SourceChanging if still settling at that deadline;
otherwise it completes observation in the same request. Rescan waits for quiet
or the cap before actually scanning; clients do not retry a false completion.
The cumulative guard protects split scans even when the cap expires. It cannot
undo automatic deletions below the threshold during a longer operation; the
quiet interval is a coalescing heuristic, not an atomic snapshot of a writer.
The former per-scan-only calculation could delete a 100-object batch as smaller
unguarded batches; this was already present before the observation changes.
Migration from 49 to 50 is forward-only: back up the stopped data directory
and restore it with the matching executable instead of editing user_version.

Native macOS validation on 2026-11-04 passed six focused cases covering split
12/88 deletion scans, restart and approval history, zero-candidate reset, migration
rollback/retry and older-reader rejection, split bulk deletion during quiet with
zero automatic deletions, bounded control replies, quiet-cap cancellation and
the deadline-bound HTTP SourceChanging response. The production HTTP control suite passed
21 cases; the native sync integration suite passed all 23, including the fixed
100-object three-page approval journey and an individual successful wait request
that exceeds its connection inactivity budget. No negative assertion was removed.

The pending deletion set has an opaque revision tied to catalog/root IDs,
root identity, completed scan revision, journal fence, thresholds, candidate
names, bucket incarnations and live generations. Persist/page large sets;
never materialize the complete set in RAM. `approve` takes that revision,
not a boolean or wildcard, and grants one execution of that exact set only.
For a set that does not wait for approval, any observed tree change, and for
every set a root/marker change, relevant API change or restart, invalidates
unused approval and requires rescan and fresh approval. Replays and
foreign/stale revisions return 409 without broadening the set. A later inventory
that finds exactly the members of an undecided set again confirms it under the
same revision instead of replacing it, also after an API write moved the journal;
an approved or rejected set, or one from before a restart, is never confirmed
again. An observed tree change leaves a set that waits for approval in place, and
approving or rejecting it first inventories the tree again, so the decision
applies to the members the tree shows then and a late notification about another
path does not make the named revision stale. A set that waits keeps waiting
until it is approved or rejected or every file it holds is back: a set that
replaces it, because some files came back, others went or the tree gained
files, waits whenever it holds one of those files, across a restart and an
upgrade too. A rejection is durable: the write-back of the rejected files
resumes at the next start, retry or rescan, and no inventory runs until it has
finished.

Inspect candidates with `GET /_internal/sync?view=deletions&revision=REVISION`,
following its cursors to completion. Every page belongs to that exact frozen set,
with the same denominator, count and fence, which only an inventory that confirms
the same members after an API write moves; `approve` executes the whole inspected
revision, never only the last page or a recomputed current set. Any invalidation
also makes its remaining page requests return 409 until an inventory confirms the
same members again. Restart invalidates both the
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
durability and state-after-file ordering to removal. Exporting a deletion opens
the target's directories without creating them: a missing directory means the
target is already gone, and creating it would put back a directory someone
removed. Removing an empty directory before a publication follows the same
ordering: the removal, its parent's sync, and only then the rename that installs
the object.

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
and authorized recovery controls remain available. The Storage API stays ready;
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
target identity/revision and confirmation. If that cannot be committed, retain all
bytes and the stop: a storage failure (including ENOSPC) returns 503 `disk_full` or
`io_failure`, busy or closing storage returns 503 `backendError` to retry the same
operation ID, and corrupt stored state returns 503 `backendError` to restore a valid
data directory. Then unlink only the checked protected entry, sync its parent, and durably finish
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
the live server and never opens the data directory. CLI accepts `--endpoint`
and `--ca-cert`, verifies TLS using the shared connector and accepts a bearer token
from `--token-file` or `GCSM_SYNC_TOKEN` without printing it. It must not follow
redirects carrying credentials. These commands are runnable; see
[the CLI reference](sync-cli.md).

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
| `GET /_internal/sync` | Default summary: authentication mode, state, session ID, findings revision, the observation summary (revision, baseline completeness, tracked objects, mapping findings with `blocking_findings` and `unmapped_findings`, the parts of them that stop a wait and the objects the tree cannot hold, pending write-back, path stops, unresolved conflicts, pause and scan-stop reasons, conflict quota usage including workspace bytes/entries and over-limit state), pending-deletion revision/count with the frozen deletion set and, while it waits for approval, `pending_deletion_remedy` with the commands that inspect, approve and reject it, `writeback_safety`, and whether conflict resolution and stable waits are available. `view=deletions` or `view=findings` selects the fixed-revision pages defined below. No object bodies or raw filesystem paths. |
| `GET /_internal/sync/conflicts` | `limit` default/max 256, opaque cursor bound to catalog/root and listing revision. Return IDs, logical names, original owner project/incarnation, revisions, generations, reason, size/hashes; no retained-body path. Stale cursor returns 409. |
| `POST /_internal/sync/approve` | JSON `deletion_set` revision; return accepted operation ID and exact count, or 409 for stale/foreign/replayed revision. |
| `POST /_internal/sync/reject` | The same request and answers as approval; it closes the set and writes its files back from their objects instead of deleting them. |
| `POST /_internal/sync/resolve` | Registered conflict: JSON `conflict_id`, `revision`, `action` (`restore` or `discard`), `expected_generation` (zero means absent) and `operation_id`; generation/incarnation recheck as above. Unregistered recovery: exclusive `recovery_id`, `revision`, `target_identity`, `action: "discard"`, `confirmation` and `operation_id` as defined above; original-owner authority and explicit confirmation precede deletion. |
| `POST /_internal/sync/rescan` | Empty JSON object; return coalesced scan revision/operation ID. It requests observation, never approval or an override of a stop. |
| `POST /_internal/sync/wait` | JSON `timeout_ms`, default 10000, `1..=60000`, and an optional boolean `require_no_findings`; capture a requested stable scan plus journal fence and wait for that bounded barrier. Return barrier/revisions/counts with `findings` (`unmapped` and the findings `revision` that lists them, null unless the barrier was observed), `paused_reason` for a pause, `source_entries` (their `total`, the first five `entries` the pass could not take in, each with `path`, U+FFFD for each sequence that is not UTF-8, and `reason`, and the `omitted` count) for `SourceUnrepresentable`, and the `remedy` for the reason, or an explicit timeout/stopped result. Later unrelated edits do not move the fence forever. |

CLI `approve --deletion-set ID` and `resolve --conflict ID --revision ID --action
restore|discard --expected-generation N --operation-id ID` map exactly to these
requests. `wait --timeout 10m --require-no-findings` sends wait requests of
at most a minute each until its own deadline, as described below. `status --view deletions|findings --revision
ID --limit N --cursor ID` maps to the GET query; without `--revision`, the CLI
reads the summary first and requests the page at the `findings_revision` or
`pending_deletion_revision` it reports, and a `--cursor` needs the revision of
the page that returned it. Listing accepts the same page
limit/cursor flags. Recovery resolve uses the exclusive recovery form and explicit
confirmation flags above. Omit `--view` for the summary.
All control JSON is capped at 64 KiB, rejects unknown fields and wrong types;
GET bodies are rejected. Responses are capped at 256 KiB with paged detail.
Unknown method returns 405; unknown route/disabled sync returns 404; malformed
input 400, oversize 413, stale conditions 409, closed service or a catalog wait past its budget 503.
Accepted asynchronous mutations return 202 with operation ID; completed reads
and wait return 200, wait deadline 408 and an existing stop 409. A rescan that
fails and leaves synchronization paused answers 409 with the `paused_reason`, the
cause and the remedy, and an approval while paused answers 409 and asks for a
rescan first; neither answers 503, which would invite a blind retry. The CLI exits
nonzero on errors, timeout or stopped work and prints actionable diagnostics; a
wait that ends for another reason than `Ready` names it, with the recorded
`paused_reason` for a pause, and the remedy with the connection options filled in.
Control requests have no count limit. A command waits for the mirror's one-command queue
within its own deadline: the requested `timeout_ms` for a wait, which still answers 408, and
`object_request_timeout_ms` for the others. The mirror stopping closes the queue and wakes them,
and a cancelled request gives up its place. Wait uses
notifications/deadlines, not polling. Disconnect cancels only its observer.

Both status detail views require a nonempty `revision` from the summary:
`pending_deletion_revision` for `deletions`, `findings_revision` for `findings`.
The findings revision, which a wait also returns, moves only when something the
findings view lists changes: a mapping finding, a pause or scan stop, a path
stop, a write-back operation or its quota, a conflict or a recovery workspace.
Since catalog schema 59 the baseline flag, the deletion sets and the tracked
files no longer move it, so a late directory notification that invalidates an
unchanged deletion set leaves a findings page readable. A findings page at a
revision that did change answers 409 and asks for the status again, which
reports the current `findings_revision`.
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
API changes through its captured journal fence, including a post-scan fence.
It succeeds once every change it can reflect is reflected; a pause ends it at
once, with `paused_reason` in the answer.

Objects whose names the tree cannot hold, such as a backslash, a component
longer than the host allows, a file and directory of one name or a case alias,
stay reachable through the API and do not stop a wait: a `Ready` answer counts
them in `findings.unmapped` with the findings `revision` that lists them. Names
the tree ignores, such as `.DS_Store`, are neither counted nor stop it. Deleting
such an object through the API stops it counting at once, even while the
mirror is paused and has not replayed the deletion; the findings view lists its
finding until the deletion is replayed or a scan removes it. An export conflict stays through the deletion,
and a name finding of a later generation in the same bucket incarnation does not
replace it, since the external edit it holds is still unresolved. With
`require_no_findings: true`, a `Ready` wait that counts any becomes
`FindingsPending`, with a remedy of its own that names how many remain and the
findings view that lists them, and nothing about conflicts; the control decides
it from the holdup the mirror's barrier ended on, which is the same either way.
Every answer other than `Ready`, `Stopped` and
`JournalChanged` carries the `remedy` for the holdup it ended on, with `--endpoint URL` for
the client to fill in and the findings revision or the deletion set revision the
answer read filled in already; a `Timeout` or `SourceChanging` remedy names what
the wait's last finished pass was still waiting for. A writer that never
settles yields timeout. A successful
wait is not a permanent quiescence claim. Shutdown wakes waiters and drains
accepted side effects before releasing locks. CLI users can write a file, wait,
then read through the API.

A server observes one wait request for at most a minute. `gcs-mimic sync wait`
therefore sends requests of at most a minute each, with the time left of its own
`--timeout`, which defaults to two minutes and takes up to an hour; the
healthcheck's start period, not the wait's default, covers the first pass. It asks again
only while the answer says the work is still in progress, `Timeout`,
`SourceChanging` or, after a one-second pause, `JournalChanged`, and reports each
repeat on standard error; a pause, source entries no object can take, findings,
an approval or a stop end it at once.
A `Ready` answer that counts unmapped objects exits 0 and prints one warning with
the ready-to-run `gcs-mimic sync status --view findings` command that lists them;
it prints nothing when the count is zero. The exit status tells the other ends
apart: 2 when someone must act (`Paused`, `SourceUnrepresentable`,
`DeletionApproval`, `FindingsPending`), 3 when the timeout passed while work was
in progress (`Timeout`, `SourceChanging`, `JournalChanged`), and 1 for anything
else, such as `Stopped` or an unreachable server.

## Stops, findings and remedies

Each kind of stop has its own exhaustive type; no single type covers them all:

- `PauseReason` pauses the whole root until a rescan succeeds and is stored
  with the observation as `paused_reason`: `ConflictCapacity`, `IoFailure`,
  `RecoveryRequired`, `SourceChanging`, `SourceUnrepresentable` and
  `observer-failure`, the last for any failure that names no other reason. The
  `sync_paused` event reports it as `paused_reason` with the remedy for that
  reason, and a root-stop finding as its `reason`, except that the finding
  shows `observer-failure` as `Paused`.
- A scan stop ends one scan without pausing: the root keeps running and the
  next scan tries again. It is stored apart from the pause as `scan_stop`,
  which only takes `SourceChanging` or `SourceUnrepresentable`, and is also
  listed as a root-stop finding. These two names describe the same source
  condition in either place; only `paused_reason` means synchronization paused.
- `writeback::Stop` is the write-back state in status: `Ready`,
  `RecoveryRequired`, `ProtectedBody`, `ConflictCapacity` or `IoFailure`.
- `WaitReason` says why `sync wait` ended: `Ready`, `Timeout`, `Stopped`,
  `Paused`, `JournalChanged`, `SourceChanging`, `SourceUnrepresentable`,
  `DeletionApproval` or `FindingsPending`.
- A path finding's `reason` is `Conflict`, `Unrepresentable`,
  `ProtectedDeletion`, `SourceChanging`, `IoFailure`, `EntryReplaced` or
  `RecoveryRequired`.
- Status `state` is one of `Starting`, `Retrying`, `Paused`, `Stopped`,
  `ConflictCapacity`, `IoFailure`, `RecoveryRequired`, `ProtectedBody`,
  `SourceChanging`, `SourceUnrepresentable`, `Observing`, `DeletionApproval`,
  `FindingsPending` or `Ready`. Status also reports `mirror_state`, the same
  value as `sync.state` in a ready answer, which decides `state` as follows.

| `mirror_state` | Status `state` |
| --- | --- |
| `starting` | `Starting`. A reason an earlier process recorded shows only in `observation.paused_reason`. |
| `retrying` | `Retrying`. |
| `paused` | `ConflictCapacity`, `IoFailure` or `RecoveryRequired` for that recorded reason; otherwise `Paused`, also for a pause the mirror failed to record. |
| `stopped` | `Stopped`. |
| `running` | The first that holds: a recorded pause, named as for `paused`; the write-back state other than `Ready`; the scan stop; `Observing` without a complete baseline; `DeletionApproval` while a guarded deletion set waits for an approval no operator has given; `FindingsPending` while findings that stop a wait (`blocking_findings`), path stops, pending write-back or unresolved conflicts remain; otherwise `Ready`, also while `unmapped_findings` counts objects the tree cannot hold. |

A wait that finished its own pass ends by the same judgment, in the same
order. Both take the working tree's side from what held back a pass, the same
answer that decides whether missing files become deletion candidates: each scan
records it when it ends, and status reads that record, while a wait reads it
from its own pass, which also counts the exports it replayed. The two can
differ only for the length of a pass. What waiting cannot change comes first:
tree entries no object can take, the non-empty directory of a bucket deleted
through the API, and entries a pass could not read for a reason waiting does
not clear end a wait at once with `SourceUnrepresentable`, since they hold back
every deletion too, and status shows `SourceUnrepresentable` for them; files
that wait for a conflict decision end it with `FindingsPending`. Files still
changing or racing an API write, files written after the pass took the tree in,
exports skipped for a busy file and a baseline still open are work in progress,
which the wait asks about again until its deadline. A wait that runs out of
time answers with its last finished pass and names what that pass was still
waiting for: how many files, and the first of them. Objects the tree cannot hold
never decide a wait. A deletion set ends a wait with `DeletionApproval` only
while it is guarded and unapproved, as in status; a granted set runs in the
pass that completes it.

Distinguish a whole-root pause, a blocked path and a nonblocking finding. Emit
structured stderr events on transitions, not on every repeated observation.
Every diagnostic names the logical target/key, scope and remedy; paginate
details and aggregate counts.

| Reason | Scope and discovery | Complete recovery path |
| --- | --- | --- |
| `Starting` | This process's first pass, until it finishes, through any retries; status, `sync.state` and the healthcheck's diagnostic | Nothing to repair: wait, and follow it with `gcs-mimic sync status`. A persistent failure pauses the root instead. |
| `Retrying` | A started mirror recovering from a transient failure, until the next run's reconciliation finishes, or a directory watch rebuild; status, `sync.state`, `sync_retrying` or `sync_watch_retry` event | Nothing to repair for a transient failure; restore read/search permissions for a watch rebuild, whose third failure pauses the root. |
| `Observing` | Running without a complete baseline; status | Wait for the scan, or run `gcs-mimic sync wait`. |
| `DeletionApproval` | External deletions paused; status/event | Inspect the exact revision, approve it, or restore the files and wait again; changed candidates require fresh approval. |
| `FindingsPending` | Running with findings, path stops, pending write-back or unresolved conflicts; status | List them with `gcs-mimic sync findings --revision` and the status's `findings_revision`, and follow each finding's remedy. Other paths continue. |
| `Conflict` | Path finding after successful preservation; `conflicts`, status/event. A mapping finding with `detail_code` `export-conflict` is an export that met an edited file and left it as it was; one with `unclassified-collision` was carried over from a `collision` recorded before schema 58 | Restore the retained revision with inspected expected generation, or explicitly discard it; reinspection resolves stale conditions. For an export conflict, preserve the edited file, then inspect conflicts or restore the previously exported bytes and wait again. For an unclassified collision, rescan or restart so the object is observed again, then follow the remedy of the finding that replaces it. Other paths continue. |
| `ConflictCapacity` | Root ingestion and destructive write-back paused; status/event and paged recovery findings | Resolve/discard registered conflicts within the fixed 1 TiB and 1,000,000-entry limits, then rescan. Before displacement the source stays intact; afterward protect/account its workspace entry even above quota. Complete preservation/registration or use original-owner-authorized, exact-revision/identity recovery discard with explicit backup/no-backup confirmation, then revalidate all root stops. Never automatically delete or blindly roll back the body to free space. |
| `RecoveryRequired` | Root pause, write-back state, or a path's pending write-back operation; status, `sync_paused` event and paged recovery findings | Preserve the workspace and current target, inspect each recovery finding with `gcs-mimic sync findings`, and settle it with `gcs-mimic sync resolve --recovery` as an exact confirmed discard, or as a registered conflict; then rescan. |
| `ProtectedBody` | Write-back state while a displaced body is protected in the workspace; status and paged recovery findings, whose `reason` is `RecoveryRequired` | As for `RecoveryRequired`; when it stops a run, the root pauses as `RecoveryRequired`. |
| `Unrepresentable` | Path finding; paged status findings/event. Its `detail_code` names the mapping reason, including `collision` for a file/directory collision or an aliased entry | Rename/copy-delete through API into a representable name, remove the collision/unsafe external entry without following it and wait again, or retain API-only access. Never silently normalize. |
| `ProtectedDeletion` | Path restored and blocked; paged status findings/event | Release hold/retention through a supported API, then delete the file again. If protection cannot be released, retain the restored file/API object. |
| `SourceChanging` | Path deferred until quiet deadline, or a scan stop; status, path or root-stop finding | Finish or stop the writer, then wait again. Continuously changing input times out wait without publishing partial bytes. A root paused for it shows `Paused` and needs a rescan. |
| `SourceUnrepresentable` | A scan stop for unsafe or unmappable source entries; status and a root-stop finding | Preserve the entries the findings list, rename them or move them out of the tree, then wait again. A root paused for it shows `Paused` and needs a rescan. |
| `IoFailure` | Affected path; whole-root pause for unavailable root, incomplete scan or failed displaced-body preservation; status/event and paged recovery findings | Restore mount/permissions/free space and preserve source/conflicts and any displaced workspace body. For an affected path, then wait again; a paused root needs a rescan under the displaced-body recovery rules; typed disk-full is not swallowed or tight-looped. |
| `Paused` | Root paused by `observer-failure`, `SourceChanging` or `SourceUnrepresentable`, or by a pause it failed to record; status with `observation.paused_reason`, `sync_paused` event and a root-stop finding | Correct the condition the event names without removing pending data, then run `gcs-mimic sync rescan`. A changed server setting, such as `max_sync_entries`, needs a restart instead. A failed rescan answers 409 with the reason and keeps the pause. |
| `Stopped` | Shutdown, or the mirror's thread ended; status while available, waiting requests wake and end with it | Restart after fixing the reported shutdown/storage error; reopen only after old jobs release ownership. |

A marker mismatch is a startup failure, reported through the root error before
bind; it has no stop reason of its own. A journal gap
or notification overflow is not a stop: the mirror recovers with a bounded full
rescan, and a wait it interrupts ends with `JournalChanged`. Catalog corruption
surfaces as the catalog's own open or I/O failure.

`/_internal/ready` and its alias `/_internal/healthcheck` keep their
API-readiness HTTP status. No synchronization stop makes the API unready;
catalog health failures and failing maintenance still can. While synchronization
is enabled, a ready answer adds `sync.state`, the mirror's state, read from
memory without a catalog query: `starting` until this process's first
reconciliation finishes, through any retries on the way, then `running`;
`retrying` from a transient failure of a mirror that has started until its next
run's reconciliation finishes, or while a directory watch rebuild retries;
`paused` until an explicit rescan succeeds, so a rescan that fails, even
transiently, answers 409 with its failure and leaves the mirror paused under the
reason it recorded; and `stopped` after shutdown or after the mirror's thread
ends unexpectedly, which also withdraws the sync controls. `running` says the
first pass has finished, not that a later change is applied, which
`gcs-mimic sync wait` waits for. Each change of state is logged once as a
`sync_state_changed` event with `from` and `to`. The summary carries no counts,
paths, names, catalog/root IDs or tokens; detailed strict diagnostics require
management authorization. Status reports the same value as `mirror_state`
beside its own `state`, as tabled above. The `sync` object is outside the
public contract until the directory synchronization feature is complete.

`gcs-mimic healthcheck`, which the container image runs, fails while a ready
answer reports `starting`. `starting` covers the whole first pass: ingesting the
tree, then exporting every existing object, so a Compose dependency on a healthy
container sees the tree reconciled. Every other state passes, because waiting
would not end a pause or a stop, and `gcs-mimic sync status` explains them. The
diagnostic for `starting` names that command with the probed origin. An answer
without `sync` passes as before, and a `sync` object the healthcheck cannot
read, as from another version, fails and says so. The image allows a 5-minute
start period, checked every 2 seconds, for the first pass over a tree near the
local-development scale.

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
files. A file whose inode number alone changed is hashed too, and the tracked
hashes keep it unchanged, because Docker Desktop shared folders can report a new
number for an unchanged file. Unknown/coarse timestamp precision is conservatively racy. Skip staging
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

**File-notification dependency selection is closed.** The
[native observer](continuous-directory-observation.md) calls inotify (Linux) and
FSEvents (macOS) through the existing libc dependency and adds no notification
crate. Docker bind mount notifications require their own evidence, including
unavailable/missed events and overflow fallback. Native Windows synchronization
is considered separately and is not part of this contract; synchronization is
rejected there at configuration, startup and in the HTTP controls (501), and
unsupported watch setup must report failure, never false synchronized success.
Reusing `fsutil`, MIME and TLS components does not authorize a new dependency.

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
