# Seed then ready startup

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

Contract checked: **2026-10-29**. Scope: UX-002, an emulator-specific project
requirement. The directory layout and the replay-on-every-start lifecycle follow
fake-gcs-server's `-data` fixture ergonomics; this is not a Google API. The
authoritative status remains in `compatibility/features.json`. Historical
one-time-import behavior is recorded in the [native evidence](../evidence/seed.md)
and the [platform evidence](../evidence/seed-platform.md); those records describe
the earlier contract and are kept unchanged.

## Sources checked

The Google sources below were checked on 2026-10-22; the fake-gcs-server
lifecycle comparison was rechecked on 2026-10-29:

- [Google object naming](https://docs.cloud.google.com/storage/docs/objects#naming):
  the existing flat-namespace validator governs seeded object names.
- [Google bucket naming](https://docs.cloud.google.com/storage/docs/buckets#naming):
  seeded buckets use the existing bucket validator.
- [Bucket insertion](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/insert):
  the existing insertion parser supplies project validation and bucket defaults.
- [Object resource](https://docs.cloud.google.com/storage/docs/json_api/v1/objects):
  the existing publisher computes size, MD5 and CRC32C; HTTP metadata exposes them.
- [Discovery v1](https://storage.googleapis.com/$discovery/rest?version=v1):
  revision `20261013`, SHA-256
  `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`.
  The fetched bytes match the retained source manifest. No API route/schema is
  added by this slice, and no RPC behavior changes.
- fake-gcs-server v1.56.1 README:
  a usability reference for bucket-directory/file layout only. Its raw README
  SHA-256 is
  `562a420488e91854d23eb1e627a6953621727dd81f8e44d37a09151035a05b66`.
- fake-gcs-server seed lifecycle, as recorded in
  [the fake-gcs-server storage comparison](../fake-gcs-server-storage.md#seed-replay-and-explicit-reseed):
  startup reads the seed directory every time, overwrites objects whose names are
  present in the tree, keeps unrelated API-created objects and skips top-level
  regular files.

Existing compatibility gaps were reviewed. None defines a Google seeding API.
The existing bucket/media/metadata gaps remain open; local fixture replay does
not resolve or broaden those API claims.

## Acceptance cases

Test identifiers below are the exact integration test function names in
`tests/seed.rs`, unless a different source is specified.

| ID | Acceptance behavior | Test identifier | Result |
| --- | --- | --- | --- |
| seed-config | CLI > environment > JSON > defaults for `seed_dir`, `default_seed_dir`, `seed_project` and `seed_content_type` (default `extension`); reject empty paths, unknown MIME modes and invalid projects | `seed_configuration_precedence_and_validation` | PASS |
| seed-layout | Create buckets, empty buckets and nested objects, including Unicode, spaces, binary bytes and hidden files; list/download with size, MD5, CRC32C and default extension Content-Type | `seeded_http_metadata_bytes_and_strict_administrative_import` | PASS |
| seed-auth | Both profiles seed administratively; strict API reads still require the configured local token | `seeded_http_metadata_bytes_and_strict_administrative_import` | PASS |
| seed-project | Honor a nondefault owning project and accept an empty seed root | `explicit_project_and_empty_seed_directory_are_supported` | PASS |
| seed-replay | Restart restores fixture bytes and seed-default metadata over an API overwrite and over API metadata edits, keeps the generation of an object that already matches, recreates an API-deleted fixture object and a deleted fixture bucket, keeps API-only objects and never deletes an object whose fixture file was removed | `restart_replays_fixtures_over_api_changes_and_keeps_unrelated_objects` | PASS |
| seed-replace | An edited fixture file publishes a new live generation; a versioned bucket keeps the previous generation as noncurrent, an unversioned bucket drops it | `edited_fixture_replaces_live_generation_and_versioning_keeps_noncurrent` | PASS |
| seed-blocked | A differing held fixture fails the entire replay without readiness or any namespace changes; an identical held object is unchanged; releasing changed holds permits the complete retry | `held_seed_changes_roll_back_the_entire_tree_while_identical_held_objects_are_unchanged` | See [atomic replay](atomic-seed-reseed.md) |
| seed-content-type | `.txt` maps to `text/plain; charset=utf-8` by default; `seed_content_type=octet-stream` restores `application/octet-stream`; switching modes replaces only objects whose mapping changes and a repeated replay is unchanged | `default_content_type_is_extension_and_octet_stream_is_opt_in` | PASS |
| seed-default-dir | `default_seed_dir` seeds an existing directory when `seed_dir` is unset; a missing path, a regular file, and a path overlapping `data_dir` in either direction are ignored and report `disabled`; an explicit `seed_dir` wins | `default_seed_dir_imports_only_an_existing_disjoint_directory` | PASS |
| seed-disabled | Startups without a seed root report `disabled` and replay nothing; adding `seed_dir` later imports the fixtures | `seed_dir_added_after_disabled_startup_imports_and_removal_disables_again` | PASS |
| seed-top-level-file | A regular file directly under the seed root is skipped with a `seed_skipped` warning and startup reaches readiness | `top_level_files_are_skipped_with_a_warning` | PASS |
| seed-retry | A failed replay emits no ready event, publishes no partial bucket/object and releases ownership; a repaired tree succeeds and project mismatch remains rejected | `interrupted_import_retries_without_overwrite_and_releases_lock` | See [atomic replay](atomic-seed-reseed.md) |
| seed-invalid | Reject invalid bucket/object names, missing/overlapping configured roots and oversize files; accept zero bytes and the inclusive file-size cap | `invalid_fixtures_fail_before_ready_and_size_limit_is_inclusive` | PASS |
| seed-walk | Accept 32 nested directories below a bucket; reject deeper trees and more than 10000 entries | `fixture_depth_and_entry_count_are_bounded` | PASS |
| seed-links | Reject a symlink root, symlink entry and Unix socket without readiness | `symlinks_and_special_files_fail_closed` | PASS on macOS ARM64; Unix-only test |
| seed-utf8 | Reject non-UTF-8 filesystem names on a filesystem that permits their creation | `non_utf8_fixture_names_fail_closed` | PASS on Linux ARM64 before replay (test unchanged); macOS rejects fixture creation |
| seed-migration | Populated v4 catalog migrates to the current schema, then replay restores the fixture at its name and keeps migrated API-only objects; existing v1/v3 migration coverage remains green | `populated_v4_catalog_migrates_and_replays_seed`; `src/storage/object_tests.rs::v1_migration_is_transactional_and_current_schema_reopens_idempotently`; `src/storage/object_tests.rs::metadata_update_uses_clock_preserves_blob_and_migrates_previous_catalog` | PASS |
| seed-readiness | After recovery and the seeding event, blocked publication prevents any listener/probe success; successful import precedes health and readiness | `probes_cannot_succeed_while_seed_publication_is_blocked` | PASS |
| seed-extension | Pinned extension MIME, literal percent names, unchanged source bytes/mtime/permissions; an API object at an OS metadata name survives replay | `extension_mime_is_explicit_and_existing_os_metadata_objects_survive_restart` | PASS on macOS ARM64 |
| seed-compose | Read-only fixture mount; up --wait; replay over API changes across stop/start and recreate; deleted fixture buckets reappear; fresh volume seeds | `container-seed-lifecycle`: `scripts/smoke-containers.sh` seeded section | PENDING final-source execution: the script already asserts replay; the new atomic publication contract still needs its final combined gate |

`tests/import_filesystem.rs::filesystem_import_preserves_source_identity_and_skips_later_seed`
also verifies that replaying an empty fixture root over an imported catalog leaves
every imported object and generation untouched, and that a missing configured
root fails instead of being skipped.

## Supported subset and local rules

Seed uses the typed
[shared administrative interface](onboarding-state-contract.md#administrative-interface-acceptance),
with a Seed origin and explicit local authority in both authentication profiles.
API grants remain unchanged. Bucket creation returns the actual stored record
without overwriting it. Its two management upload slots and bounded 30-second
admission waits are independent of the API's catalog and connection limits.
Accepted publication and cancellation cleanup retain their permits until
completion, and catalog close drains them before releasing directory ownership.
Local files use shared file staging and one whole-tree publication transaction,
with dedicated file workers, immutable staging checksums and source-change
detection. The
[seed adapter contract](import-seed-adapter.md) records the source profile.

### Configuration

| JSON setting | CLI | Environment | Default |
| --- | --- | --- | --- |
| `seed_dir` | `--seed-dir PATH` | `GCS_MIMIC_SEED_DIR` | Absent: seeding disabled unless `default_seed_dir` applies |
| `default_seed_dir` | `--default-seed-dir PATH` | `GCS_MIMIC_DEFAULT_SEED_DIR` | Absent; the container image sets `/data` |
| `seed_project` | `--seed-project ID` | `GCS_MIMIC_SEED_PROJECT` | `local-project` |
| `seed_content_type` | `--seed-content-type MODE` | `GCS_MIMIC_SEED_CONTENT_TYPE` | `extension` |

These are flat settings with the existing precedence. An explicitly empty path,
including an empty `GCS_MIMIC_SEED_DIR` or `GCS_MIMIC_DEFAULT_SEED_DIR`, fails
configuration validation. Projects use the same nonempty identifier,
256-byte maximum and whitespace/control-character rejection as bucket insertion.
There is no project registry or Google project lookup. `seed_content_type` accepts
only `extension` and `octet-stream`; empty or unknown values fail validation.
The same setting also selects the content type of files ingested from a
synchronized directory.

`default_seed_dir` is a fallback for images that want fake-gcs-server's
`-data /data` behavior without another variable. At `serve` startup it becomes the
seed root only when `seed_dir` is unset, the path exists as a directory (not a
symlink), and it does not overlap `data_dir` or `sync_dir` in either direction.
Otherwise it is ignored silently and the ready event reports `disabled`. If the
path cannot be inspected, startup reports a warning diagnostic with key
`default_seed_dir` and does not seed from it. A configured `seed_dir` is never
ignored: a missing or overlapping explicit root fails startup.

### Directory mapping

Every top-level directory must have a valid bucket name; it becomes a bucket.
Empty bucket directories create empty buckets. A top-level regular file is
skipped with a `seed_skipped` warning event on stderr whose `path` is the file
name and whose `reason` is `top-level entries must be bucket directories`.
It is not counted in the ready event. Each regular file beneath a bucket becomes
an object named by its relative components joined with `/`. Empty nested
directories produce no object. Directory prefixes also undergo object-name
validation to bound names before traversal. Files are processed sequentially in
sorted depth-first order, independent of directory enumeration order on the
current platform.

Regular files named `.DS_Store` or starting with `._` are skipped at every depth,
including the root, and counted in `skipped_os_metadata`. Matching directories,
such as `._assets/`, are traversed normally; other hidden files such as `.gitkeep`
remain objects inside a bucket. Symlinks (including the root and
metadata-looking names), special files, non-UTF-8 names and invalid bucket/object
names fail startup with a diagnostic.

By default each seeded file gets the content type from the pinned extension
table: the last dot in the final component, exact-case then pinned Go
simple-lowercase lookup, and `application/octet-stream` for unknown suffixes.
`.txt` therefore becomes `text/plain; charset=utf-8` and hidden `.yaml` files
`application/x-yaml`. `seed_content_type=octet-stream` writes
`application/octet-stream` for every file.
The [source contract](source-storage-contract.md#seed-profile-and-mime-decision)
pins the exact inputs and rules. Filesystem metadata does not define Google
metadata. Bucket defaults are the existing `US` multi-region and
`STANDARD` class. Seed timestamps use the injected clock. No IAM/ACL grant follows
from import. Seeding bypasses API authorization as a local administrative action
in both profiles; subsequent API access follows the selected profile.

The canonical seed and data directories must not overlap in either direction.
The source remains trusted, stable, read-only administrative input for the entire
operation. The importer never writes to it. Compose enforces a read-only mount.
Unix source opens now pin the root and use no-follow descriptor traversal for
every ancestor and leaf; deterministic concurrent symlink swaps fail closed.
Windows retains its prior trusted-source requirement. Native seeding does not
create a filesystem-wide source snapshot; see [atomic replay](atomic-seed-reseed.md).

### Replay on every startup

Every startup with a seed root replays the whole tree before the listener binds.
For each bucket directory the bucket is ensured: a missing bucket is created and
counted; an existing bucket keeps its settings but must belong to
`seed_project`, otherwise startup fails. For each fixture file:

- **No live object**: the transaction publishes the fixture and it is
  counted in `objects`. This includes an object that was deleted through the API.
- **Live object that is exactly what seeding would write**: same size, MD5 and
  CRC32C, the seed content type, no content encoding, disposition, language,
  cache control or custom time, and no custom metadata. Nothing is written, no
  generation is minted and the object is counted in `unchanged`. Holds and
  storage class are not compared. The unchanged check uses the verified staged bytes.
- **Any other live object**: the file is published as a new live generation
  with authoritative selection inside the whole-tree transaction and seed-default
  metadata and counted in `replaced`. API overwrites and API metadata edits at
  fixture names are therefore reverted. A versioned bucket keeps the previous
  generation as noncurrent; an unversioned bucket drops it. A catalog seeded
  with the previous `octet-stream` default is replaced once with the extension
  content type.
- **Replacement rejected by an event-based or temporary hold or an unmet
  retention period**: the entire replay rolls back. Startup fails without
  readiness and explains how to release protection and retry. No other selected
  bucket or object changes. Successful results retain `blocked: 0`.

Objects that have no fixture file are never touched, and removing a fixture
file never deletes an object. Deleting a seeded bucket through the API does not
survive the next replay: the bucket and the current fixture files reappear.
An identical object keeps its generation instead of being rewritten.
The opt-in [reseed endpoint](atomic-seed-reseed.md) also replays the configured
source; disk merges while RAM reseed replaces its namespace.

### Durable initialization and recovery

Catalog v5 added a singleton `startup_seed` table. It now records only that the
catalog finished its first startup; it no longer suppresses replay. Fresh schema
creation, its generation allocator and that state commit in one SQLite
transaction, and existing valid catalogs migrate as already initialized. Replay
still runs on migrated catalogs. Unreleased v2, foreign schemas and future
versions remain rejected.

Complete-tree staging uses the existing media durability/recovery protocol,
opaque blob identifiers and incremental checksums. One SQL transaction publishes
all bucket/object references and completion state after validation; no transaction
spans a body transfer. A failure before commit preserves the old namespace.
Staging/orphan cleanup checks committed references and ordinary recovery removes
unreferenced remnants. Cancellation and ambiguous commits retain ownership until
their outcome is resolved; see [atomic replay](atomic-seed-reseed.md).

A startup without any seed root replays nothing and reports `disabled`; API
changes made since the last seeded startup survive it. Adding the seed root back
replays the fixtures on that startup.

### Readiness and outcomes

The order is configuration/registry validation, default seed root resolution,
catalog open/recovery, seed replay, listener bind, ready event, then request
handling. A configured replay first emits `{"event":"seeding"}` to stderr. It
signals work starting and is not a readiness event.

The stdout ready event adds a `seed` object:

- `{"outcome":"disabled"}` when no seed root applies.
- `{"outcome":"imported","buckets":1,"objects":1,"replaced":0,"unchanged":0,"blocked":0,"skipped_os_metadata":0}`
  after a successful replay. Counts describe this startup, not catalog totals:
  `buckets` and `objects` are created entries, `replaced`, `unchanged` and
  `blocked` follow the rules above; successful replay always reports zero
  blocked objects.

A failure prints a diagnostic, explicitly closes the catalog, releases its lock
and exits nonzero without a ready event or bound listener. If catalog close
itself fails while handling a seed failure, the close error replaces the
original seed diagnostic; successful lock release is then unproven.

### Resource bounds

Replay is sequential. A dedicated traversal thread sends entries over a two-slot
channel using the shared bounded, sorted Walker. File publication clones
to immutable staging where supported or copies with fixed buffers, hashing the
staged bytes once on the shared administrative file workers. Retained metadata is
capped at 8 MiB and buckets at 1,000; one batch lease bounds whole-tree ownership.
`max_object_bytes` applies to every fixture file on every replay, with a
metadata check before reading and the publisher's streamed cap at staging; a
fixture larger than a later, smaller cap fails startup even when the live object
matches. Disk seeding never collects the body in memory; RAM seeding retains
bounded chunks under the configured shared byte budget.

Traversal counts all files and directories, including hidden entries and bucket
directories, with a total cap of 10000 entries and 32 directory levels below each
bucket. Sorted directory-entry vectors and the pending stack share that global
entry budget; directory iterators are closed before descent. Logical names are
bounded to 1024 UTF-8 bytes. These limits bound traversal memory and open handles
without a general directory-walking dependency. They are local usability limits,
not Google bucket/object limits. Filesystem/SQLite operation duration and overall
seed duration have no additional wall-clock bound. Replaying an unchanged tree
reads every fixture body once per startup.

## Getting-started snippet for integration

Keep the existing base journey unchanged. Use a distinct Compose project for the
seeded journey.

```sh
export COMPOSE_PROJECT_NAME=gcs-mimic-seed-demo
export GCS_MIMIC_PORT=0
docker compose -f compose.yaml -f compose.seed.yaml up -d --wait
seed_address="$(docker compose -f compose.yaml -f compose.seed.yaml port gcs 4443)"
curl --fail "http://$seed_address/storage/v1/b?project=local-project"
curl --fail "http://$seed_address/storage/v1/b/local-bucket/o"
curl --fail "http://$seed_address/storage/v1/b/local-bucket/o/hello.txt?alt=media"
docker compose -f compose.yaml -f compose.seed.yaml stop
```

The final download is the checked-in `fixtures/local-bucket/hello.txt`, containing
`Hello from the emulator.` followed by a newline. Its MIME type is
`text/plain; charset=utf-8`. Keep both file arguments on subsequent stop/start
and recreate commands; each start replays the fixtures over API changes at the
same names. Strict mode additionally uses `-f compose.strict.yaml` and API
requests use `Authorization: Bearer local-admin-token`.

## Deviations and limitations

All seed configuration, directory rules, MIME profiles, replay policy and caps
are declared local project rules. They make no Google fixture API or full
fake-gcs-server flag/metadata compatibility claim. No additional upstream
ambiguity was found that needs a new Google compatibility gap.

Process-abort and modeled power-loss snapshots at four publication/commit
boundaries now reopen as complete old/new namespaces; see [atomic replay](atomic-seed-reseed.md).
Physical power loss and individual instruction windows remain unverified. Existing
publication crash-recovery tests remain green. The seeded container lifecycle in
`scripts/smoke-containers.sh` still asserts the earlier one-time import and has
not been rerun, and the image's `default_seed_dir=/data` path is not verified in
a container. Native AMD64, Linux and Windows execution of the replay tests remain
unverified by this evidence. Source mutation races are excluded by the stable
read-only input contract.

## Verification

| Command | Result |
| --- | --- |
| `RUST_TEST_THREADS=4 cargo test --locked --offline --test seed` | PASS on macOS ARM64: 21 passed, 0 failed (`non_utf8_fixture_names_fail_closed` is Linux-only and not compiled) |
| `RUST_TEST_THREADS=4 cargo test --locked --offline --test import_filesystem --test foundation` | PASS: 12 and 27 passed, 0 failed |
| `cargo test --locked --offline --lib -- mime::tests v1_migration_is_transactional_and_current_schema_reopens_idempotently metadata_update_uses_clock_preserves_blob_and_migrates_previous_catalog` | PASS: 3 passed |
| `cargo xtask compatibility check` | PASS |

## Ledger completion

**UX-002 is PARTIAL** with achieved fidelity `PROJECT`: every native acceptance
case above passes, while `seed-compose` is PENDING until the container smoke
script asserts replay and is rerun. **DESIGN-062** remains PARTIAL for its
separate manifest-metadata and synchronization scope; **DESIGN-068** (test admin
commands such as reset/reseed) remains NOT_STARTED because restart replay adds no
admin endpoint.
