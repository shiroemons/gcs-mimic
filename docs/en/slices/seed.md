# Seed then ready startup

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

Contract checked: **2026-10-25**. Scope: UX-002, an emulator-specific project
requirement. The directory layout follows fake-gcs-server's fixture ergonomics;
it is not a Google API. The authoritative status remains in
`compatibility/features.json`. Historical native behavior is recorded in the
[native evidence](../evidence/seed.md); the completed Linux and Compose cases
are recorded in the [platform evidence](../evidence/seed-platform.md).

## Sources checked

All sources below were checked on 2026-10-22:

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

Existing compatibility gaps were reviewed. None defines a Google seeding API.
The existing bucket/media/metadata gaps remain open; local fixture import does
not resolve or broaden those API claims.

## Acceptance cases

Test identifiers below are the exact integration test function names in
`tests/seed.rs`, unless a different source is specified.

| ID | Acceptance behavior | Test identifier | Result |
| --- | --- | --- | --- |
| seed-config | CLI > environment > JSON > defaults; reject empty paths and invalid projects | `seed_configuration_precedence_and_validation` | PASS |
| seed-layout | Create buckets, empty buckets and nested objects, including Unicode, spaces, binary bytes and hidden files; list/download with size, MD5, CRC32C and explicit Content-Type | `seeded_http_metadata_bytes_and_strict_administrative_import` | PASS |
| seed-auth | Both profiles seed administratively; strict API reads still require the configured local token | `seeded_http_metadata_bytes_and_strict_administrative_import` | PASS |
| seed-project | Honor a nondefault owning project and accept an empty seed root | `explicit_project_and_empty_seed_directory_are_supported` | PASS |
| seed-once | Restart preserves overwritten bytes, new objects and metadata; deleting every bucket does not replay fixtures; fresh data reseeds | `restart_preserves_mutations_and_deleted_buckets_and_new_data_reseeds` | PASS |
| seed-disabled | First startup without seed_dir permanently disables automatic import in that catalog | `disabled_startup_never_replays_fixtures_later` | PASS |
| seed-retry | Failed partial import emits no ready event and releases ownership; retry preserves existing bytes/generation, imports missing files and rejects a changed bucket project | `interrupted_import_retries_without_overwrite_and_releases_lock` | PASS |
| seed-invalid | Reject top-level files, invalid bucket/object names, missing/overlapping roots and oversize files; accept zero bytes and the inclusive file-size cap | `invalid_fixtures_fail_before_ready_and_size_limit_is_inclusive` | PASS |
| seed-walk | Accept 32 nested directories below a bucket; reject deeper trees and more than 10000 entries | `fixture_depth_and_entry_count_are_bounded` | PASS |
| seed-links | Reject a symlink root, symlink entry and Unix socket without readiness | `symlinks_and_special_files_fail_closed` | PASS on macOS and Linux ARM64; Unix-only test |
| seed-utf8 | Reject non-UTF-8 filesystem names on a filesystem that permits their creation | `non_utf8_fixture_names_fail_closed` | PASS on Linux ARM64; macOS rejects fixture creation |
| seed-migration | Populated v4 catalog migrates to v5 without replay or overwrite; existing v1/v3 migration coverage remains green | `populated_v4_catalog_migrates_without_seeding`; `src/storage/object_tests.rs::v1_migration_is_transactional_and_current_schema_reopens_idempotently`; `src/storage/object_tests.rs::metadata_update_uses_clock_preserves_blob_and_migrates_previous_catalog` | PASS |
| seed-readiness | After recovery and the seeding event, blocked publication prevents any listener/probe success; successful import precedes health and readiness | `probes_cannot_succeed_while_seed_publication_is_blocked` | PASS |
| seed-compose | Read-only fixture mount; up --wait; stop/start, recreate and all-bucket deletion preserve mutations; deliberate new volume reseeds | `container-seed-lifecycle`: `scripts/smoke-containers.sh` seeded section | PASS on Linux ARM64/native and AMD64/emulated |
| seed-extension | Explicit pinned MIME, literal percent names, unchanged source bytes/mtime/permissions, existing OS metadata object identity and bytes retained after restart | `extension_mime_is_explicit_and_existing_os_metadata_objects_survive_restart` | PASS on macOS ARM64 |

## Supported subset and local rules

Seed uses the typed
[shared administrative interface](onboarding-state-contract.md#administrative-interface-acceptance),
with a Seed origin and explicit local authority in both authentication profiles.
API grants remain unchanged. Bucket creation returns the actual stored record
without overwriting it; publication preserves the existing create-if-absent
condition and checksums. Its two management upload slots and bounded 30-second
admission waits are independent of the API's catalog and connection limits.
Accepted publication and cancellation cleanup retain their permits until
completion, and catalog close drains them before releasing directory ownership.
Local files use `Admin::publish_from_file`, with dedicated file workers,
immutable staging checksums and source-change detection. The
[seed adapter contract](import-seed-adapter.md) records current verification.

### Configuration

| JSON setting | CLI | Environment | Default |
| --- | --- | --- | --- |
| `seed_dir` | `--seed-dir PATH` | `GCS_MIMIC_SEED_DIR` | Absent: import disabled |
| `seed_project` | `--seed-project ID` | `GCS_MIMIC_SEED_PROJECT` | `local-project` |
| `seed_content_type` | `--seed-content-type MODE` | `GCS_MIMIC_SEED_CONTENT_TYPE` | `octet-stream` |

These are flat settings with the existing precedence. An explicitly empty path,
including an empty `GCS_MIMIC_SEED_DIR`, fails configuration validation.
Projects use the same nonempty identifier,
256-byte maximum and whitespace/control-character rejection as bucket insertion.
There is no project registry or Google project lookup. `seed_content_type` accepts
only `octet-stream` and `extension`; empty or unknown values fail validation.
The latter uses the repository's pinned Go/mailcap/YAML MIME table. There is no
runtime host MIME database lookup, body sniffing, sidecar format or new dependency.

### Directory mapping

Every top-level entry must be a bucket directory with a valid bucket name. Empty
bucket directories create empty buckets. Each regular file beneath a bucket
becomes an object named by its relative components joined with `/`. Empty nested
directories produce no object. Directory prefixes also undergo object-name
validation to bound names before traversal. Files are processed sequentially in
sorted depth-first order, independent of directory enumeration order on the
current platform.

Regular files named `.DS_Store` or starting with `._` are skipped at every depth,
including the root, and counted in `skipped_os_metadata`. Matching directories,
such as `._assets/`, are traversed normally; other hidden files such as `.gitkeep`
remain objects inside a bucket. Other top-level regular files fail.
Symlinks (including the root and metadata-looking names),
special files, non-UTF-8 names and invalid bucket/object names fail startup with
a diagnostic. Only the declared regular OS metadata files are skipped.

By default every newly imported file has explicit `application/octet-stream`
content type, including `.txt`. Explicit `extension` mode uses the last dot in
the final component, exact-case then pinned Go simple-lowercase lookup, and
`application/octet-stream` for unknown suffixes. Hidden `.yaml` files therefore
use `application/x-yaml`; text mappings include `charset=utf-8`.
The [source contract](source-storage-contract.md#seed-profile-and-mime-decision)
pins the exact inputs and rules. Filesystem metadata does not define Google
metadata. Bucket defaults are the existing `US` multi-region and
`STANDARD` class. Seed timestamps use the injected clock. No IAM/ACL grant follows
from import. Seeding bypasses API authorization as a local administrative action
in both profiles; subsequent API access follows the selected profile.

The canonical seed and data directories must not overlap in either direction.
The source is trusted, stable, read-only administrative input for the entire
startup operation. The importer never writes to it. Compose enforces a read-only
mount. Native use does not change permissions or snapshot the source; concurrent
source mutation, including replacing checked paths with symlinks, is outside this
contract and must be prevented by the operator.
In particular, the directory must be admin-controlled: the symlink check and
file open are separate operations, not protection against a concurrent attacker.

### Durable initialization and recovery

Catalog v5 adds a singleton `startup_seed` table. Fresh schema creation, its
generation allocator and the incomplete seed state commit in one SQLite
transaction. A crash before that transaction commits leaves an empty catalog;
a crash afterward leaves durable retryable state. Existing valid v1/v3/v4
catalogs migrate to v5 as already complete, even if they currently have no
buckets. Unreleased v2, foreign schemas and future versions remain rejected.

A configured import completes only after every file publication succeeds. The
final marker is a short catalog transaction after the last publication. Objects
use the existing media publication/recovery protocol, opaque blob identifiers,
incremental checksums and atomic `ifGenerationMatch=0` enforcement. No database
transaction spans a body transfer.

If startup fails or dies mid-import, published objects and buckets remain;
unpublished staging/orphan blobs follow ordinary catalog recovery. The marker
remains incomplete. On retry, existing bucket metadata is preserved, a different
owning project is an error, and existing live objects are skipped without opening
their source bodies or changing bytes, metadata or generation. Missing objects
are imported. Removed source entries are not reconciled or deleted. This is
convergent create-if-absent import, not a whole-directory atomic transaction.

Once complete, restarts do not inspect the seed path at all. This includes a
missing/changed seed source and a catalog where the user deleted every bucket.
A first startup **without** `seed_dir` commits the same completion marker before
binding: adding `seed_dir` later is ignored, even after all buckets are deleted.
Use a new empty data directory or a deliberate volume reset to seed again. Never
delete the marker or edit SQLite as a normal reset procedure.

The no-seed rule also applies after an interrupted partial import: starting once
without `seed_dir` marks that partial catalog complete. The ready event reports
`disabled`; a later configured startup reports `skipped` and does not resume the
remaining fixtures. There is no separate warning event for this transition.

### Readiness and outcomes

The order is configuration/registry validation, catalog open/recovery, seed
import, listener bind, ready event, then request handling. After recovery, an
eligible configured import emits `{"event":"seeding"}` to stderr. It signals
work starting and is not a readiness event.

The stdout ready event adds a `seed` object:

- `{"outcome":"disabled"}` for first startup with no seed setting.
- `{"outcome":"skipped"}` for an already initialized or migrated catalog.
- `{"outcome":"imported","buckets":1,"objects":1,"existing_objects":0,"skipped_os_metadata":0}`
  after a successful import; counts describe this startup, not catalog totals.

A failure prints a diagnostic, explicitly closes the catalog, releases its lock
and exits nonzero without a ready event or bound listener. A successful marker
can precede a later bind failure; retry then skips import because publication
already succeeded.
If catalog close itself fails while handling a seed failure, the close error
replaces the original seed diagnostic; successful lock release is then unproven.

### Resource bounds

Import is sequential. A dedicated traversal thread sends entries over a two-slot
channel using the shared bounded, sorted Walker. File publication clones to
immutable staging where supported or copies with fixed buffers, hashing the
staged bytes once on the shared administrative file workers.
`max_object_bytes` applies
per newly imported file, with a metadata check before reading and the publisher's
streamed cap at staging. Existing live objects are preserved regardless of a
later, smaller configured cap. No object body is collected in memory.

Traversal counts all files and directories, including hidden entries and bucket
directories, with a total cap of 10000 entries and 32 directory levels below each
bucket. Sorted directory-entry vectors and the pending stack share that global
entry budget; directory iterators are closed before descent. Logical names are
bounded to 1024 UTF-8 bytes. These limits bound traversal memory and open handles
without a general directory-walking dependency. They are local usability limits,
not Google bucket/object limits. Filesystem/SQLite operation duration and overall
seed duration have no additional wall-clock bound.

## Getting-started snippet for integration

Keep the existing base journey unchanged. Use a distinct Compose project and a
new volume for the seeded journey; a volume first started without seeding will
already be initialized.

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
`application/octet-stream`. Keep both file arguments on subsequent stop/start
and recreate commands. Strict mode additionally uses `-f compose.strict.yaml`
and API requests use `Authorization: Bearer local-admin-token`.

After explicitly warning that it deletes this demo project's emulator data, the
runnable guide can offer `docker compose -f compose.yaml -f compose.seed.yaml
down -v`, followed by `up -d --wait`. Ordinary shutdown uses `stop` or `down`
without `-v`. The container smoke script uses its own isolated project/volume.

## Deviations and limitations

All seed configuration, directory rules, MIME profiles, initialization policy
and caps are declared local project rules. They make no Google fixture API or
full fake-gcs-server flag/metadata compatibility claim. No additional upstream
ambiguity was found that needs a new Google compatibility gap.

Power-loss behavior and individual seed-marker/fsync crash instruction windows
were not tested. Mid-import failure/retry and normal persistence were tested;
existing publication crash-recovery tests remain green. The Linux ARM64 seed
suite and source-built ARM64/native plus AMD64/emulated Compose journeys pass.
Native AMD64 and Windows execution remain unverified by this evidence.
Source mutation races are excluded by the stable read-only input contract.

## Ledger completion

**UX-002 is IMPLEMENTED** with achieved fidelity `PROJECT`, the original 14
acceptance cases PASS and the Linux/Compose evidence recorded on 2026-10-22.
Its earlier
native checks and evidence remain attached. **UX-009 is IMPLEMENTED** for its
declared stop/reset journey. The broader **DESIGN-062** fixture requirement
remains PARTIAL because manifest metadata is unimplemented; **DESIGN-063** and
**UX-008** retain their separate durability/platform/permission limitations.

The [seed adapter](import-seed-adapter.md) supersedes the hidden-entry rule and
adds explicit extension MIME. Historical platform evidence remains attached;
the adapter's current native results are recorded separately. No new Google
compatibility gap or complete legacy-format compatibility claim is added.
