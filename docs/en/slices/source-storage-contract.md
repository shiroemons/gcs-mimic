# Source storage and import contract

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

Contract checked: **2026-10-24**. Scope: DESIGN-062, DESIGN-072, DESIGN-073 and
AUDIT-014; delivery gate `source-storage-contract` and migration milestones
`seed-format-adapter` and `filesystem-state-import`. This slice freezes a local
conversion contract. It adds no importer, configuration setting or Google API.
Implementation status remains in `compatibility/features.json`.

## Acceptance cases

The following cases are defined before adapter implementation. All runtime
cases remain **PENDING**. Existing native seed/publication evidence is retained
and does not establish source import support.

| Case | Feature | Required behavior and negative/boundary checks | Owner |
| --- | --- | --- | --- |
| source-seed-profile | DESIGN-062 | Distinguish seed input from durable storage; retain empty buckets, literal names, binary/zero-byte bodies; explicit extension MIME uses pinned inputs and Go extension/lowercase rules; native default stays octet-stream; count and skip only regular OS metadata files at every depth, traverse `._assets/` including its children | import-seed-adapter |
| source-seed-safety | DESIGN-062 | Preserve all other native seed cases; reject unreadable/invalid/symlink/special inputs instead of source-style silent skipping; exclude only the generated seed ACL; no implicit replay or reseed; shutdown during publication drains side effects, removes temporary files and releases locks | import-seed-adapter |
| seed-content-type-config | DESIGN-062 | Reject empty/unknown values; CLI overrides environment, then config file, then octet-stream default; verify both MIME profiles and durable-import isolation | import-seed-adapter |
| source-filesystem-reader | DESIGN-072 | Read escaped bucket names, empty buckets, bucket sidecars and root-wide xattr/sidecar mode without writes; reject ambiguous/missing/orphan/oversize metadata, duplicate decoded names and unsupported fields; test OS-specific errno, typed errors, single-call xattr reads using only SourceMetadataBuffer, exact-cap acceptance, cap-plus-one rejection and Windows source/xattr configuration rejection | import-dry-run |
| source-integrity-report | DESIGN-072 | Report missing/truncated/changed bodies, invalid JSON/base64/times, checksum mismatches, unrepresentable names and directory-only entries; distinguish metadata-only inspection from streamed hash verification; bounded memory and diagnostics; property-test valid bucket escape round trips and panic-free arbitrary timestamp parsing | import-dry-run |
| source-name-scope | DESIGN-072 | Fixtures for upstream writes named `a//b`, `a/./b`, `a/../b` and `x/../../other/y` report normalized disk names and unverifiable original names; detect observable collisions/escapes, declare Windows separator rules, never invent lost names | import-dry-run |
| source-preserved-identity | DESIGN-073 | Preserve recoverable disk names, bytes, headers, metadata and generations; default blocks sub-millisecond object times; explicit ms conversion reports originals/counts and is lossy; report generated ETag/metageneration/bucket time; verify old generation reads/conditions, allocator bounds and unsupported ACL/history/retention blocks | import-filesystem with shared-imported-identity |
| source-import-recovery | DESIGN-073 | Per-element SAVEPOINT publication commits successful elements with journal state, closes batches at count/time limits without rolling back successes and stops on disk-full without a retry loop; element rejection is terminal, resumable progress is a contiguous prefix; resume only the same export after interruption/cancellation/Busy/disk-full or inode/ctime-only change; compare inode/ctime within each execution, stop on drift and revalidate drifted committed elements on resume; test crashes before/after recording that stop; cancel the caller while commit runs on the SQLite execution thread, drop its handle and retain the blob/readable committed body; reclaim definitely uncommitted, unreferenced excluded/unattempted/cancelled staging blobs before command exit; verify ambiguous commit, visible cleanup failures off Tokio workers, bounded handles, manifest equality, abandonment and shutdown cleanup; see Delivery boundaries for unresolved requests | import-filesystem with shared-import-journal and shared-publication-modes |
| source-import-authorization | DESIGN-073 | Require AdminGrant in both auth profiles; no imported IAM/public grants; strict post-cutover reads deny anonymous/ungranted principals and allow configured authorized principals; bucket default hold is retained without adding holds to imported objects | import-filesystem with shared-admin-interface and shared-publication-modes |
| source-cutover | AUDIT-014 | Empty separate destination only; unfinished import blocks serve/readiness; completion atomically enables serve and disables seed replay; changed source or foreign destination cannot resume; backup/rollback leaves source intact | import-filesystem |
| source-memory-boundary | AUDIT-014 | Memory state is a separate live-export format with active/archive membership, bucket settings and all generations; never infer it from a seed tree or claim a live-only listing is lossless | go-adapter-live-export, outside this slice |
| catalog-upgrade-policy | AUDIT-014 | Transactional upgrades preserve generations, permissions and resumable sessions; newer schemas fail without reset; document backup/downgrade behavior; each later schema slice adds its migration cases; see Delivery boundaries for the assignment request | Candidate: first catalog-schema-version-changing slice; final gate replacement-validation |

## Pinned evidence and interpretation

The [source manifest](../../../compatibility/upstream/source-storage-contract.json)
records the inspected git blobs and MIME content digests. The reference is
fake-gcs-server **v1.56.1**, commit
`896ece51e49eed0946089f3089f6343855c0d106`, not its moving main branch.
Review covered `fakestorage/server.go`, `fakestorage/object.go`, `main.go`,
`Dockerfile` and the backend files named in that manifest.
The [existing storage investigation](../fake-gcs-server-storage.md) supplies
earlier execution evidence; this slice only reads upstream source.

The onboarding findings' seed and backend descriptions still match that
revision. Two details matter for conversion: Linux/macOS bucket creation time
comes from inode **change time**, not birth time, and unset `Deleted` and
`CustomTime` may be serialized as Go's year-0001 zero time. The reader must
recognize that sentinel rather than report an ordinary live object as deleted.
Supplied nonempty checksums are retained by the upstream implementation even
though a source comment says they are overwritten; the executable branches
govern this contract.

`compatibility/gaps.json` was reconciled: no open gap targets these four rows
or defines a Google import API. Existing Google API gaps remain open. This
contract describes local conversion of reference-emulator data, not Google
`EXACT` fidelity or complete fake-gcs-server parity.

## Three distinct source formats

| Format | Input and identity | Metadata and empty buckets | Lifecycle |
| --- | --- | --- | --- |
| Seed directory | `seed_dir/BUCKET/OBJECT`; bucket and relative object components are literal UTF-8, joined with `/`; no percent decoding | No persisted object attributes; every top-level bucket directory exists even when empty | Native one-time initialization; source startup/reseed replay is a separate legacy behavior |
| Durable filesystem | Snapshot of `-filesystem-root`; bucket directory component is Go `url.PathEscape(bucket)`; decode once with `PathUnescape`; recover normalized disk object paths without URL-unescaping, not original API names | Root-wide xattr or sidecar object attributes; root sibling `<escaped-bucket>.bucketMetadata`; retain empty bucket directories | Stopped writer or immutable snapshot; separate empty destination; checkpointed conversion |
| Memory live export | In-process `storageMemory.buckets` with `Bucket`, `activeObjects` and `archivedObjects`; each object contains `ObjectAttrs` and bytes | Empty buckets and active/archive membership are explicit; archived generations cannot be reconstructed from a seed tree | Requires a consistent Go-adapter export before process exit; no durable root or implemented export codec |

Never auto-detect these formats from a directory's shape. Startup seed uses the
seed profile; the planned `gcs-mimic import fake-gcs-server --source DIR
--data-dir NEW --metadata-mode xattr|sidecar --source-platform unix|windows
[--timestamp-precision exact|ms] [--dry-run]` command reads durable
filesystem state. Its dry-run
report identifies the source format/revision and metadata mode. A future Go
export gets its own versioned envelope and reader; this slice freezes its
required information, not an invented upstream on-disk serialization.

Reject malformed percent escapes, duplicate decoded bucket names, names outside
the existing bucket/object validators and unsafe path mappings. Decode bucket
names only after preserving the raw component; a decoded `/` never becomes a
host path. Object `%2F` remains those three literal characters. Reject symlinks,
special files, non-UTF-8 entries, file/directory collisions and names whose
representation is ambiguous on the host. The later sync mapping contract owns
the reusable name/path rules; import owns the upstream bucket escape codec.

Upstream uses `filepath.Join(root, escapedBucket, objectName)` and does not
persist `Name`. Cleaning collapses `a//b` and `a/./b` to `a/b`, and `a/../b`
to `b`; `x/../../other/y` can place a body under another bucket. Overwritten
aliases and the original bucket/name cannot be reconstructed or reliably
detected from the resulting tree. Every dry-run/import report must therefore
declare `names_recovered_from_normalized_disk_paths` and
`original_names_verified: false`, even when no collision is observable.
Preservation covers recoverable disk names only; no full original-name lossless
claim is possible. The reader blocks observable collisions/unsafe mappings.
Record the source path platform explicitly with required
`--source-platform unix|windows`; Windows treats
both slash and backslash as separators, Unix treats backslash literally.
Convert actual Windows separators to logical `/` only under that declared
profile; block host-ambiguous mappings rather than guessing the source platform.

### Seed profile and MIME decision

The future setting is **`seed_content_type`**, with values **`octet-stream`**
(default) and **`extension`**. Its matching CLI/environment names are
`--seed-content-type` and `GCS_MIMIC_SEED_CONTENT_TYPE`. Empty/unknown values are
configuration errors. This slice does not make those settings available.
Use the existing CLI > environment > config file > default precedence.
The setting only controls seed input. Durable imports preserve `ContentType`
and must not infer a replacement from the filename.

`extension` uses one repository-pinned table, assembled in this order:

1. Go **go1.27.3** `src/mime/type.go`'s built-in map.
2. Alpine **v3.24**, `mailcap` **2.1.54-r0**, `etc/mime.types`, in file order;
   later assignments replace earlier extension entries, as in Go's loader.
3. The pinned source `main.go` additions: `.yaml` and `.yml` are
   `application/x-yaml`.

The Go version is a reproducible profile choice for the source Dockerfile's
floating `golang:1.27` builder, not proof of the compiler used for a published
reference image. Likewise its unversioned `apk add mailcap` is replaced with
the exact package bytes in the manifest. No equivalence with every historical
image or a host's MIME database is claimed.

The [Go built-in source](https://github.com/golang/go/blob/2aae1000ea1d4b28923b96db28acf4e3b99d6aff/src/mime/type.go)
has SHA-256
`e3d0b689c0f303a88c14844d4f7ac961030abe8d1e721f8889961ee1c2e277c8`.
The [Alpine package](https://dl-cdn.alpinelinux.org/alpine/v3.24/main/aarch64/mailcap-2.1.54-r0.apk)
has SHA-256
`d48e5f43c52905e344d6fc31c3fba4d1d3abd55594d94af2347e63fd13a9b528`;
its extracted `etc/mime.types` has SHA-256
`6d89583307fd2941a32039b9d8f392195c6e9a2fc89c681e693a06a714b3a23d`.
The manifest also pins Go's loader and the YAML additions. The adapter owner
must retain the generated table with provenance and applicable Go BSD notices;
Alpine's package declares the MIME data Public Domain. No MIME crate is needed.
The APK CDN may retire old revisions. The manifest also pins the aports APKBUILD
and its upstream tarball/SHA-512, with an Alpine distfiles mirror. Verify the
tarball's MIME member against the same SHA-256 as the APK member; do not replace
the selected data with a newer package. `import-seed-adapter` must add the final
generated table's path and SHA-256 to this manifest when it materializes it.

Extract extensions exactly as Go `filepath.Ext`: the suffix starting at the
last dot in the final path component, including a leading dot or a lone final
dot. Rust `Path::extension()` is not equivalent. Use exact-case lookup before
Go `strings.ToLower` fallback: ASCII lowercase for ASCII input, Unicode simple
lowercase for non-ASCII input (including U+212A KELVIN SIGN to `k`), using the
pinned Go release's Unicode tables. Do not use full case folding or expanding
Rust lowercase mappings. The adapter's fixtures must compare these lookup keys
with pinned Go behavior. Use Go's media-type formatting and `charset=utf-8`
default for text types.
Do not sniff bytes or consult the OS registry, globs, locale or `/etc/mime.types`
at runtime. No matching extension returns `application/octet-stream`, an
explicit local fallback rather than a claim that source `TypeByExtension`
returns that value (it returns an empty string).

| Filename | `octet-stream` | `extension` |
| --- | --- | --- |
| `note.txt` (extension `.txt`), `NOTE.TXT` (extension `.TXT`) | `application/octet-stream` | `text/plain; charset=utf-8` |
| `.yaml` (extension `.yaml`) | `application/octet-stream` | `application/x-yaml` |
| `archive.tar.gz` (extension `.gz`) | `application/octet-stream` | `application/gzip` |
| `foo.` (extension `.`) | `application/octet-stream` | `application/octet-stream` |
| `style.css` | `application/octet-stream` | `text/css; charset=utf-8` |
| `data.json` | `application/octet-stream` | `application/json` |
| `config.yaml`, `config.yml` | `application/octet-stream` | `application/x-yaml` |
| `payload`, `payload.unknown-extension` | `application/octet-stream` | `application/octet-stream` |

Seed creates empty buckets and imports binary and zero-byte regular files.
It computes hashes and assigns native generations/times through shared
publication; filesystem stat times do not become seed metadata. The source's
fixed `projectOwner-test-project: OWNER` ACL is generated code, not information
present in a seed file. Excluding it does not waive the block on **stored** ACLs
in durable or memory data and never grants anonymous or project access.

The adapter will skip only **regular files** named `.DS_Store` or `._*` at every
depth using the common OS metadata predicate plus an entry-type check, and
report `skipped_os_metadata` in the ready seed outcome. Directories such as
`._assets/` remain ordinary input, including their children; symlinks and
special files still fail. This qualifies the plan's name-only skip wording
because silently skipping a directory would discard real input recursively.
Other hidden entries remain ordinary input. Other top-level regular files,
unreadable buckets and invalid entries still fail native startup; legacy silent
skipping is outside this slice. Existing catalogs keep already-imported OS metadata
objects because seed is one-time. The seed-adapter owner must update
`slices/seed.md`, its affected test/evidence, UX-002 and release notes together;
none of that behavior changes in this documentation slice. Explicit reseed is
DESIGN-068, outside this contract.

### Durable metadata selection and xattr decision

Upstream selects one metadata handler for the entire root at construction.
A successful root read or a missing-attribute error selects `user.metadata`;
unsupported/other root errors select `<object>.metadata`. Windows selects
sidecars. This capability inference is unsafe after copying to another
filesystem: an xattr-capable destination can contain a sidecar snapshot.

The planned import therefore requires **`--metadata-mode xattr|sidecar`** for
durable filesystem input and records it in the report/fingerprint. There is no
per-file fallback, write probe or automatic downgrade on read errors. This is
an explicit read-only adapter rule, preserving the plan's root-wide format
distinction while avoiding host-dependent misclassification.

| Selected mode | Object attributes | Classification and failures |
| --- | --- | --- |
| `xattr` | Read `user.metadata` on the opened regular body file | `.metadata` suffix alone does not identify a sidecar; such a file is an object requiring its own xattr. Missing attribute or unsupported filesystem blocks import; never substitute a neighboring JSON file |
| `sidecar` | Read the neighboring `<object>.metadata` JSON | Exclude sidecars from bodies, require each body/sidecar pair, and report orphan sidecars and suffix collisions; an independent object ending in `.metadata` is ambiguous and blocks rather than being silently dropped |

Nonempty `user.metadata` on a body in sidecar mode is mixed-format evidence and
blocks import. On a platform unable to inspect xattrs, the report states that
absence was not verified; it cannot certify a mixed-format check or lossless
cross-platform snapshot solely from sidecars. Never change modes to hide that
limitation. Root `<escaped-bucket>.bucketMetadata` files are excluded from bucket
discovery and matched exactly to their directory; orphan files are reported.
Unlike seed input, a durable body with valid stored metadata is an object even
when its name matches `.DS_Store` or `._*`; do not silently discard it. Report
unpaired OS metadata as skipped detritus only when it cannot represent a stored
object in the selected mode, and count it explicitly. AppleDouble files do not
substitute for a lost `user.metadata` attribute.

**Read method:** use the already-approved `libc` dependency, not a new xattr
crate or a shell subprocess. Whichever of `import-dry-run` and
`shared-publication-io` lands first creates `src/fsutil.rs`, makes `libc` a
direct dependency and updates the dependency ledger following the investigation's
section 7; the later slice extends the same module. `import-dry-run` owns only
the safe, bounded descriptor-based `read_source_metadata(file, buffer)` interface.
`fsutil` provides `SourceMetadataBuffer` with a private
`Box<[u8; SOURCE_METADATA_CAP + 1]>` field and constant
`SOURCE_METADATA_CAP = 65_536`. The function accepts only
`&mut SourceMetadataBuffer`, never an arbitrary byte slice, and returns
`Option<&[u8]>` borrowing that buffer, with typed `SourceMetadataError::Unsupported`,
`TooLarge` and `Io(io::Error)` failures. `None` represents the missing-attribute
result for `user.metadata`; import makes it a blocking finding, never proof of
permission or format fallback. `Unsupported` and `TooLarge` are blocking findings;
`Io` preserves the original error. Metadata/stat changes detected by the importer
map to its own typed `ImportFailure::SourceChanged`, classified by the observed
changed fields as specified below. `import-filesystem` maps
`AdminError::SourceChanged` from `stage_from_file` to that same importer failure;
the read-only reader and `import-dry-run` do not depend on Shared state's errors.
Never classify retryability by matching error text.
This is an interface request, not code added by this slice. Publication
copy/replace/stat operations remain owned by `shared-publication-io`.
`import-dry-run` can start without Shared state, but merges only after
`repo-lints`. Unsafe stays only in this file, following repo-lints' scoped
exception procedure:
module-level `#[allow(unsafe_code)]` with the platform/ABI rationale, safe public
wrappers, and a safety justification at each unsafe block. Do not relax crate
or workspace lint levels.

| Platform/error | Reader result |
| --- | --- |
| Linux `ENODATA` | `Ok(None)` (attribute absent) |
| macOS `ENOATTR` | `Ok(None)`; compare `ENOATTR` (93), not `ENODATA` (96) |
| Linux/macOS `ENOTSUP` or `EOPNOTSUPP` | `Err(Unsupported)`; compare both constants (macOS values 45 and 102 differ); never `None` |
| `EACCES` / `EPERM`, `EBADF`, other errno | `Err(Io(original_error))`; never `None`; macOS `ENODATA` is not an absent-attribute alias |
| `ERANGE`, OS `E2BIG`, or returned length over 64 KiB | `Err(TooLarge)`; no retry or allocation beyond the cap-plus-one buffer |
| Successful zero-length attribute | `Ok(Some(empty))`; parser reports invalid metadata, not absence |
| `--source-platform windows` with `--metadata-mode xattr` | Configuration error on every host: Windows source data uses sidecars |
| Windows host with Unix-source xattr selection | `Err(Unsupported)`; sidecars remain available |

Linux uses four-argument `fgetxattr`; macOS uses its six-argument form with
position/options zero, always for the literal `user.metadata` key. Descriptor
reads bind attributes to the same inode used for body reading. See the
[Linux interface](https://man7.org/linux/man-pages/man2/fgetxattr.2.html) and
[Apple interface](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/fgetxattr.2.html).
Make one `fgetxattr` call into the reusable **64 KiB + 1 byte** buffer, with no
length query. Accept lengths through 64 KiB; a larger result, `ERANGE` or `E2BIG`
is `TooLarge`. There is no size-query/read race or reader `Changed` variant.
Sidecar and bucket JSON use the same cap with a cap-plus-one overflow check.
Windows supports
sidecar input; xattr selection fails as specified above. Linux/macOS execution
and ABI tests belong to the reader/shared-I/O slices. No additional dependency is
selected or added, so no unapproved xattr dependency is required.

### Durable field mapping and blocking information

JSON names below are the exact exported Go struct names. Reject duplicate keys,
unknown fields, wrong types and malformed encodings in this pinned local format;
that policy does not redefine unknown-field handling for Google APIs. Absence
of an optional zero-valued field is allowed; required identity/time fields
cannot be silently regenerated. Preserve absent versus empty values whenever
their observable meaning differs, or report an unsupported representation.

| Source | Target or report |
| --- | --- |
| Bucket directory / decoded name | Validated `BucketRecord.name`; collisions block; preserve empty buckets |
| `BucketAttrs.DefaultEventBasedHold` | `BucketRecord.default_event_based_hold`; preserve the bucket setting without synthesizing a per-object hold absent from source `ObjectAttrs`. If shared imported publication cannot prevent ordinary upload-default hold application, block that bucket |
| `BucketAttrs.VersioningEnabled` | `BucketRecord.versioning_enabled`; true is inconsistent with this filesystem backend and blocks; memory exports may retain true |
| `BucketAttrs.ACL` | Nonempty stored ACL blocks until shared permission semantics can preserve it; never treat it as the generated seed ACL |
| Bucket creation time from source stat | Generated destination time; report observed source value/provenance only. Exclude it from preservation, precision blockers and source fingerprints on every platform; inode change time is unstable on child updates/chmod and cannot prove creation time |
| Fields not stored for buckets | Explicit import project (same validation/default as `seed_project`), native location/class, metageneration 1 and other native defaults; report these as generated values, never as source-restored IAM/CORS/labels |
| `BucketName`, `Name`, `Size` | Excluded from persisted `ObjectAttrs` JSON by `json:"-"`; derive names from layout and size from body stat/stream; do not trust injected JSON replacements |
| `StorageClass` | `ObjectRecord.storage_class` if supported without semantic loss; source creates `STANDARD` by default; unknown/unsupported values block |
| `ContentType` | `content_type`, preserving stored value; no filename inference |
| `ContentEncoding`, `ContentDisposition`, `ContentLanguage`, `CacheControl` | Corresponding object fields; validate safe wire representation; preserve empty/absent semantics or block |
| `Metadata` | String-to-string `metadata`, bounded by parser limits; no conversion of arbitrary JSON values to strings |
| `Crc32c`, `Md5Hash` | Strict base64, exactly 4 big-endian CRC32C bytes and 16 MD5 bytes; compare both with one streamed hash pass; mismatch or missing required checksum blocks verified import |
| `Generation` | Positive signed 64-bit source generation, in microseconds in normal source writes; shared-imported-identity must retain it and reserve future allocator values above imported identities without overflow |
| `Etag` | Generated native ETag, including after metadata patch; retain the upstream value in reports only, even if custom. No imported ETag column, JSON/XML rendering override or source If-Match compatibility is requested |
| `Created`, `Updated` | `time_created`, `updated`; strict source timestamp parsing; reject missing/invalid/overflowing values. Default `--timestamp-precision exact` blocks sub-millisecond values; explicit `--timestamp-precision ms` permits reported conversion to catalog milliseconds |
| `Deleted` | Empty or parsed Go zero time means unset; any other value blocks this live-filesystem import; memory archived membership is separate |
| `CustomTime` | Empty or parsed Go zero time means unset; any other value blocks until its semantics are supported |
| `ACL` | Every nonempty persisted ACL blocks, including a seed-looking constant once stored in a runtime snapshot |
| `RetentionMode`, `RetentionRetainUntil` | Empty/zero values are unset; an active retention value blocks until enforceable; malformed/inconsistent pairs block |
| Metageneration / history | Not in persisted `ObjectAttrs`; generated metageneration 1 is reported. Filesystem has only live objects; it cannot prove preservation of earlier lost versions |

Source object timestamps use layout `2006-01-02T15:04:05.999999Z07:00`.
Accept its variable fractional precision and zone offset; compare instants,
not strings. Validate full input and retain the original representation in
findings. A nonzero digit below millisecond precision is a blocking precision
finding for object `Created`/`Updated` by default. This keeps a precision-lossless
path, although the source's usual microsecond clock makes most API-written
objects ineligible today. The explicit `--timestamp-precision ms` option uses
the existing catalog's Unix-millisecond conversion (floor toward negative
infinity, including pre-epoch instants), never nearest rounding. It reports
`rounded_objects` and `rounded_timestamp_fields`, plus source relative name,
field, original string and converted value for every affected field in both
dry-run and final import reports. Use bounded report pages/spooling with exact
totals; a truncated summary must link the complete detail report. Even zero
roundings in this mode do not authorize a lossless claim. Record the selected
precision mode in the journal/options fingerprint. Bucket stat times are
report-only observations and never enter this check or fingerprint.

These are reasoned refinements of the plan's design requirements: its blanket
custom-ETag block is replaced with generated-value reporting because native
ETags depend on generation/metageneration and are not source MD5 strings;
its precision block remains the default, with only the explicit lossy mode
added to make real snapshots usable. Generated ETags and unstable bucket ctime
are excluded from the preserved-identity scope. Increasing catalog persistence
to microseconds and deciding JSON/XML timestamp rendering still requires an
owner decision; candidate owners are `shared-imported-identity` for a deliberate
schema/rendering migration or a separate timestamp-precision slice before it.
Neither is implemented or silently required by this contract.

A missing bucket sidecar has upstream zero-value semantics, so report defaults
and retain the empty bucket. Malformed bucket JSON blocks. Directories below a
bucket do not prove a trailing-slash object exists: upstream creates only the
directory and discards its body/attributes. Report directory-only candidates
without manufacturing zero-byte objects. Body absence, stat/hash disagreement,
or a sidecar/xattr that cannot be paired is corruption, not an empty object.

Unsupported ACL, nonzero `CustomTime`/`Deleted`, retention, history or object
timestamp precision blocks by default. Only the explicit ms mode waives the
precision block; it cannot waive any other finding. Reports list generated
ETags, bucket times, metagenerations and every omitted field/generation; they
must distinguish the declared preservation subset from full original-state
losslessness. A filesystem snapshot is at most the source's current live state;
memory histories need the separate export.

### Memory export boundary

The pinned memory backend has no files to scan. Its snapshot must capture
`Bucket` (`Name`, `VersioningEnabled`, `TimeCreated`, `DefaultEventBasedHold`,
`ACL`) plus **all** active and archived `ObjectAttrs`/bodies under a consistent
writer barrier. Preserve bucket identity, empty buckets, original generation,
active/archive membership and every metadata field above. A bucket can have
archives without any active objects. Trailing-slash names are real memory
objects and must not be reconstructed using filesystem directory rules.

The Go-adapter slice owns export serialization/versioning, consistency and
history preservation tests. Import must reject memory-export input until that
codec and required shared history/permission/precision support exist. A JSON
API live-object listing, a seed copy or an empty filesystem root is not a full
memory export. No memory exporter is implemented or claimed here.

## Read-only inspection, publication and recovery

Input must be admin-controlled and immutable for the entire inspection/import,
with the source writer stopped or a consistent snapshot supplied. The importer
never writes source files, xattrs or permissions. Read-only mounting is strongly
preferred; ordinary reads may affect filesystem access time, so byte/metadata
integrity is distinct from an atime guarantee. Check source/destination overlap
after canonicalization in both directions; source, catalog and sync roots
must remain separate. Reject a destination symlink or preexisting content.

The reader produces typed bucket/object records, a bounded body reader and a
JSON report. Record format/revision, metadata mode, generated defaults,
preservation scope, entry counts, skipped OS metadata and findings by source
relative path/field/reason/fix. Report paths never need the operator's absolute
root. Each finding offers a concrete remedy; report all findings within a
bounded page and an explicit truncated/count summary when the cap is reached.
An incomplete inspection never authorizes publication or a lossless claim.

Dry-run defaults to metadata inspection, with explicit `--verify-bodies` for
streamed checksum verification. Metadata-only output says `bodies_verified:
false`; matching stat size alone is not body verification. Dry-run creates no
catalog, journal or source-side marker and does not bind listeners. Actual
import always verifies bodies before publication; it cannot reuse a
metadata-only report as checksum evidence.

Use the planned common directory iterator, overlap checks and pure streaming
hasher from `import-dry-run`. Keep native seed's 10000-entry/32-level limits
until its own adapter changes them with evidence; source import likewise uses
explicit bounded traversal, 64 KiB attribute records, 64 KiB body buffers and
pages of at most 256 entries over a bounded two-page channel. Exhausting a
limit is a diagnostic, never success or silent truncation. `max_object_bytes`
applies inclusively; zero-byte bodies remain valid. Do not retain an entire
catalog, body or findings list in memory.

Walk, body reads and hashing run synchronously on dedicated bounded OS workers,
not Tokio's shared blocking pool. Actual import uses shared
`stage_from_file` followed by `publish_objects`, with one hash pass, incremental
stored checksum comparison and source-change checks on the same opened file.
`stage_from_file` is the staging-only part of `publish_from_file`, requested
from `shared-publication-modes` below; it never commits a catalog record. Follow
shared-publication-modes' immutable snapshot/clone-or-stream rules; do not
copy mutable source with `copy_file_range` after hashing. No direct SQLite
writes, alternate hash loop, network upload bridge or private blob path is an
adapter interface. Shared state owns authorization, short atomic conditions,
identity reservation and the durable commit protocol.

Import is a local administrative action in either authentication profile;
the future shared `AdminGrant` interface is required. Importing a project/name
never installs an IAM grant or makes an object public. Strict post-cutover
reads still require the configured local principal and permissions.

Before publishing, preflight every resource's metadata and stat only and reject
every blocking finding. Preflight never hashes bodies. Actual import verifies
each body's stored digests only inside `stage_from_file` while staging the
bytes that will be published, before committing that resource; do not run a
second preflight body pass. A separate `--dry-run --verify-bodies` invocation
is optional diagnostic work and is not reused as import verification.
Start only in a new empty destination through `Catalog::open`; an existing
matching unfinished import is the sole resume exception. The shared import
journal stores a format-versioned export fingerprint, selected options,
metadata/stat identity, body digests as verified, and progress outside
`staging/` and `blobs/`. Hash the ordered manifest incrementally; do not store
all records in memory. Within one execution, compare preflight, publication and
final-scan fingerprints including each body's inode/file ID and nanosecond ctime,
as well as ordered raw/decoded paths, source platform, selected metadata bytes,
body size, nanosecond mtime and selected options. An inode replacement or ctime
change must stop publication even when size and mtime were restored. Only across
a process resume boundary are inode/file ID and ctime treated as separate drift
observations rather than export identity. The journal must retain those
observations per committed element, readable in bounded pages, so revalidation
can select only changed elements. Exclude directory ctime and bucket stat
creation observations; keep these only in the report. Define a
separate digest summary for verified body hashes, not a body-dependent preflight
fingerprint. Metadata-only dry-run fingerprints do not certify bytes.

The shared startup guard must reject `serve`/readiness for an unfinished
destination. Stage and verify bodies outside SQL transactions in bounded
batches; `stage_from_file` returns an opaque verified staged handle accepted by
`publish_objects`. The handle protects its blob as in-flight publication and
never exposes a private blob path to the adapter. Unresolved handles, including
unattempted work carried between calls, require a bounded budget; the unresolved
budget and staging/publication overlap are handed off in Delivery boundaries.
Use create-if-absent imported publication with the plan's **per-element
SAVEPOINTs**: one rejected element must not roll back other successful elements.
The adapter-state hook commits only successful elements' object/bucket records,
verified digests, inode/ctime observations and journal state in **the same
transaction**.
Never commit progress for a rejected or unattempted element. Reaching the count
or transaction-time limit closes and commits the current bounded batch, without
rolling back successes; resume from the first unattempted element.

The journal's resumable cursor/ordered-manifest prefix covers only a contiguous
committed range. Element-specific exclusions such as `PreconditionFailed`,
generation conflicts or hold violations are terminal for this importer; none
is a retryable hole. Busy is an admission timeout before the transaction;
DiskFull stops staging or fails the whole transaction, rather than providing
an element-specific retry result.
If A and C succeed while B fails a precondition, retain A/C and atomically record
the terminal failure; refuse resume and never advance a resumable cursor across
B. There is no hole-filling cursor or per-position retry mechanism. Batches
without an element rejection advance the contiguous prefix with their records;
a crash before commit replays that batch, and a crash after commit skips its
committed prefix. This preserves the plan's per-element failure isolation and
removes the publication/checkpoint crash gap.
On an ambiguous commit result, query the journal before retrying. Do not infer
completion from files or treat arbitrary AlreadyExists/PreconditionFailed as
successful import. Resume first repeats metadata/stat preflight and requires
the original fingerprint/options/destination identity, then uses the
transactionally recorded committed prefix. Any destination record outside
that prefix or disagreement with recorded identities blocks.

Classify excluded elements using the failure table below; finish the current
transaction's successful elements, persist the terminal failure state, then stop
this command. Shared publication has two cleanup invariants. Preserve blobs
whose outcome is unresolved or might be committed: cancelling the caller while
commit runs on the SQLite execution thread and dropping the handle must leave
the blob intact and the body readable if committed. An ambiguous commit result
followed by handle drop likewise leaves the blob for startup recovery.
Reclaim definitely uncommitted blobs within the command after checking they
are unreferenced, including staged-only handles dropped before any publication
attempt, excluded elements and cancellation during staging. Those blobs must
not remain after successful command cleanup; startup recovery alone does not
satisfy this case. Deletion requires a resolved outcome and an unreferenced check
under the plan's section 5 publication protection rules; keep that protection
until reclamation finishes. Cleanup failures must remain visible, and cleanup
must cause neither synchronous I/O nor SQLite waiting on Tokio workers. These
are requested invariants, not a Drop/discard/queue design; Shared state owns
those mechanisms. DiskFull or Busy never starts an automatic retry loop or rolls
back independent successes; a whole-transaction failure advances no progress
and returns the typed failure once. Failed cleanup uses shared recovery, not
repeated staging of the same body on a full disk.

On resume, inode/file ID or ctime-only differences produce
`stat_identity_drift`, not a terminal fingerprint mismatch. Before publishing
anything else, page through journaled per-element inode/ctime observations and
stream only drifted committed bodies against their journaled digests, using the
common hasher and bounded workers. Unchanged committed bodies are not reread.
Continue only if paths, sizes, mtime, selected metadata and every revalidated
digest still match; otherwise use the terminal failure rules below. Accept new
inode/ctime observations only after that element passes revalidation; they form
the baseline for comparisons within the resumed execution. Persistence of that
new baseline remains unresolved; see Delivery boundaries. Chmod-only changes,
reattaching identical xattrs, and an inode-only replacement preserving bytes,
mtime and metadata must pass this revalidation. Uncommitted bodies still pass
through `stage_from_file` once. The coordinator approved this limited exception
on 2026-10-24 to section 8's no-reread performance requirement: cross-process
stat-only drift can describe the same export, so reread only affected committed
elements rather than discard valid progress or reread all committed bytes.
Ordinary import still reads each body once while staging. Shared state owns
the journal's internal representation and update mechanism.

During publication recompute the ordered metadata/stat fingerprint, including
inode/ctime and revalidated committed-prefix records on resume, and compare it
with that execution's preflight before completion. Repeat a final
metadata/stat-only scan and require the same fingerprint to detect changes to
records visited earlier. No completion marker may commit on either mismatch
or without a complete successful prefix. Apart from the
explicit resume drift revalidation, do not reread bodies for these comparisons;
immutability remains an operator prerequisite and stat is not proof against
adversarial writers restoring timestamps. A late checksum mismatch or blocker
can leave earlier committed batches, but they remain unservable.

| Failure | Journal state / resume | Required remedy in report |
| --- | --- | --- |
| DiskFull, cancellation/interruption, Busy | Retryable; retain marker and committed cursor; resume only identical source/options | Free destination space, finish draining or release contention, then rerun the same import |
| inode/file ID or ctime-only change within an execution | Retryable stop; retain marker and committed cursor, publish no changed element and never complete; repeat metadata/stat preflight on resume and follow the drift row | Keep source immutable, then rerun the same import; test crashes before and after recording the stop yield the same drift revalidation |
| inode/file ID or ctime-only drift across process resume | Diagnostic `stat_identity_drift`; compare paged journal observations and revalidate drifted committed bodies against journaled digests before continuing | Keep source immutable; fix unreadable files before retrying; size/mtime/metadata/body mismatch follows the terminal rows |
| ImportFailure::SourceChanged or fingerprint mismatch involving paths, size, mtime, selected metadata or verified body digests | Terminal; refuse resume, retain marker and diagnostic | Stop writers, abandon the destination, obtain a fresh immutable snapshot and import into a new empty destination |
| Corrupt, checksum mismatch, blocking metadata/name/precision finding discovered during publication | Terminal; refuse resume, retain marker and diagnostic | Correct/re-export the source, abandon the destination and restart from a fresh snapshot in an empty destination |
| Foreign destination/options | Refuse binding/resume; never overwrite unrelated state | Select a new empty destination; do not reset or delete an unrelated catalog |
| Element exclusion: PreconditionFailed, generation conflict or hold violation | Terminal; retain independent successes and record failure atomically; no hole-filling resume | Resolve the conflict or unsupported hold semantics, then start from an immutable snapshot in a new empty destination |
| Other I/O/publication error | Stop with original error; no automatic retry classification | Resolve the error; resume only after the shared journal can verify the original binding/committed state, otherwise abandon |

`SourceChanged` alone does not establish terminality: retain the changed-field
evidence and use the table. Inode/ctime-only changes stop the execution and use
the already approved resume revalidation exception; a crash before persisting
that stop yields the same classification. Changes to export identity or verified
bytes still violate the immutable-export prerequisite and remain terminal.
This aligns stat-only interruptions with section 5's interrupt/resume behavior
without extending the approved no-reread exception.

Keep the marker on every failed started import. Terminal states never become
retryable merely by rerunning the command. Abandonment is an explicit operator
procedure, not an automatic cleanup command: stop all users of that destination,
confirm from the report that it is the disposable incomplete import and that
the source/backup is intact, then delete only that destination directory (or
its dedicated disposable volume) and create a new empty destination. Never
remove only the marker to enable serving partial data. This first adapter
does not add an abandon command or a reverse converter.

The shared publisher owns cleanup/recovery and cancellation-safe permits;
shutdown waits for in-flight side effects, cleans temporary files and releases
locks. Normal recovery may reclaim unreferenced staging files, so none is a
resume authority. After all fingerprint, count and verification gates pass,
completion atomically marks both import and seed complete before the startup
guard allows serve. Adding `seed_dir` afterward cannot replay migrated data.

Source backup/snapshot remains the rollback authority. Do not cut over clients
until final reports, counts, checksum/generation reads, restart and permission
checks pass. Rollback keeps the source snapshot untouched and treats the new
destination as a separate artifact; it is not a reverse converter or an
implicit catalog downgrade.

## Delivery boundaries

This slice changes contract, acceptance decomposition and source pins only.
It creates none of the common runtime code components. The common-components
table remains authoritative with the documented fsutil first-arrival rule
above. Shared interfaces required before filesystem import are:

| Owner | Required interface and acceptance |
| --- | --- |
| import-dry-run / shared-publication-io, whichever lands first | Create `src/fsutil.rs`, make approved libc direct and update the dependency ledger per investigation section 7; the later slice extends it. import-dry-run owns SourceMetadataBuffer and the single-call xattr reader accepting only that type, with Linux/macOS ABI, missing/unsupported/permission/exact-cap/cap-plus-one tests; shared-publication-io owns copy/replace/stat. import-dry-run starts without Shared state but merges after repo-lints, with its scoped unsafe exception |
| shared-admin-interface | AdminGrant in both profiles; no policy/public-access side effects from source project/name; shutdown drains work and releases locks |
| shared-publication-modes | Request staging-only verified handles (`stage_from_file`), accepted by `publish_objects`, without exposing private paths. Preserve unresolved/possibly committed blobs across caller cancellation during SQLite commit and handle drop; committed bodies remain readable. Reclaim definitely uncommitted, unreferenced excluded/unattempted/cancelled staging blobs before command exit, under section 5 publication protection. Failures stay visible and cleanup never blocks Tokio workers on synchronous I/O or SQLite. Preserve per-element SAVEPOINTs and atomic successful-element journal updates; test both cleanup invariants, terminal element exclusion retaining independent successes, count/time closure and disk-full stopping. Retain bucket default hold without synthesizing imported object holds; later uploads inherit the default. Shared state chooses mechanisms; see handoffs below for unresolved budget |
| shared-imported-identity | Explicit source generation, verified hashes and selected millisecond timestamps; advance allocator safely, reject collisions; ETags and metageneration stay native generated values, including after patch |
| shared-import-journal | Request committed digests and per-element inode/ctime observations with binding, typed failure and contiguous resumable progress in the publication transaction; observations are readable in bounded pages. Test terminal element rejection without hole-filling resume, revalidation of drifted committed elements, retryable inode/ctime-only stops with crashes before/after recording them, commit crash boundaries and atomic final import/seed completion. Shared state chooses internal representation; baseline persistence is unresolved below |
| shared-contract | Request assignment of initial catalog-upgrade-policy migration/backup/downgrade tests for AUDIT-014 to the first catalog-schema-version-changing slice before that increase; later schema slices add their own migration cases; replacement-validation retains the final migration gate. This is an assignment request, not an Import-imposed schema-change gate |

The owning feature rows' `known_deviations` are authoritative for Shared state
request status and unresolved handoffs; this section explains their scope.
Filesystem publication still requires accepted shared interfaces.
`import-dry-run` may proceed independently. No Shared state ledger entry or
implementation is changed here.

### Known limitations and handoffs

- Acceptance evidence, a single canonical receiving record, who verifies it and
  who updates the Import ledger before `import-filesystem` are unresolved.
  Candidate owners: coordinator and `shared-contract`. The requests have not
  been accepted; repeated prose is not acceptance evidence. DESIGN-073 records
  this limitation rather than inventing an acceptance protocol here.
- The catalog-upgrade-policy assignment is requested before the first schema
  increase; this Import slice imposes no gate on Shared state schema changes.
  Acceptance and any gate belong to the receiving slice. Candidate owners:
  coordinator, `shared-contract` and the first schema-changing slice.
  AUDIT-014 records the unresolved assignment.
- The former one-call unresolved-handle bound is stricter than section 5's
  sum of publication-route limits, including management slots plus one
  `publish_objects` batch. It could prevent staging the next batch during
  commit; no performance effect has been measured. Budget, permitted overlap
  and benchmark coverage remain unresolved, rather than fixing a new limit
  here. Candidate owners: `shared-contract` and `shared-publication-modes`.
  DESIGN-073 records this limitation.
- Revalidated inode/ctime observations form the current execution's baseline;
  whether they are persisted, by whom, and with what bounded journal operation
  remains unresolved. Without persistence, repeated resumes may reread every
  committed body after filesystem relocation. The existing approval covers
  revalidation of affected committed elements, not an agreed persistence
  mechanism or a measured repeated-resume budget. Candidate owners:
  `shared-contract`, `shared-import-journal` and `import-filesystem`.
  DESIGN-073 records this limitation; this slice selects neither policy.

- `import-dry-run`: read-only decoders, bounded report, source fingerprints,
  hash/overlap/walk helpers and the fsutil read; start without Shared state and
  merge after repo-lints; ensure libc direct-dependency/ledger work is complete,
  whether this slice or shared-publication-io lands first; fixtures for both
  metadata modes, normalized names, precision and corrupt input.
- `import-seed-adapter`: pinned generated MIME table and option, common OS
  regular-file metadata skip/count, local-file publication and native seed regression
  evidence; update the seed contract and release note together.
- `import-filesystem`: follows the dependencies and handoffs above, including
  shared-publication-modes,
  shared-imported-identity and shared-import-journal; validates recovery,
  serve/seed guards, unchanged source and preserved client identities.
- `go-adapter-live-export`: separate memory exporter/history work. No claim of
  complete source inventory, live directory synchronization or legacy reseed.

DESIGN-062, DESIGN-072 and DESIGN-073 remain PARTIAL; AUDIT-014 remains
NOT_STARTED. Their new import cases are PENDING. Existing API/seed evidence and
remaining upgrade/resource/durability cases remain intact. No performance code
changed, so no benchmark was run and no speed/resource result is claimed;
reader/publication implementations must use the shared-contract benchmark
profile once that contract exists.

## Verification

Checks executed on 2026-10-24:

| Check | Result |
| --- | --- |
| `git hash-object` for the 14 extracted reference files, compared with the pinned GitHub tree | PASS: every blob matched |
| Go MIME blob/tree comparison and SHA-256 for both Go files, mailcap package/member and source YAML additions | PASS: pinned bytes matched |
| `bash scripts/verify-source-storage-contract.sh` | PASS: 14 reference blobs, Go MIME blobs/SHA-256, YAML source SHA-256, APK bytes/SHA-256/member and aports tarball SHA-512/member; upstream source URL returned HTTP 404, pinned Alpine mirror passed |
| `bash scripts/verify-source-storage-contract.sh --self-test` | PASS: known digests accepted, altered bytes rejected by Git blob, SHA-256 and SHA-512 checks; fetch/fallback/HTTP 404/410 branches are outside this self-test |
| `npx --yes markdownlint-cli2 docs/en/slices/source-storage-contract.md` | PASS: no issues, cli2 0.24.0 / markdownlint 0.42.0 |
| `cargo xtask compatibility validate` | PASS: 591 feature rows, 401 parameter locations, 357 schema fields |
| `cargo xtask compatibility render` then `cargo xtask compatibility check` | PASS: both generated matrices are current; no output diff because statuses/titles/fidelity are unchanged |
| `jq` comparison against `main`, excluding the four assigned feature rows, three assigned delivery entries and Go milestone AUDIT-014 membership | PASS: all other ledger content is unchanged; `go-adapter-live-export.feature_ids` adds only AUDIT-014 across the complete slice |
| `git rebase main` with repo-lints and config-contract present | PASS: no conflicts; regenerated matrices have no diff and unrelated ledger rows match main |
| `shellcheck scripts/verify-source-storage-contract.sh` | PASS: no issues |
| `just check` after rebase | PASS: fmt, warning-free clippy, workspace tests, compatibility check and shell/release-script checks; one final execution with local listener permission |
| `git diff --check` and local-path scan of the assigned files | PASS: no whitespace errors or local absolute paths |
| `cargo xtask dependencies check` | FAIL, exit 1: existing distroless runtime digest has an available update; all other reported inputs are CURRENT |

No runtime import acceptance case is promoted by these documentation checks.
The source-verification script requires Bash, curl, jq, Git, shasum, tar, grep
and standard POSIX utilities. It is an explicit networked maintenance command,
not part of offline default tests. It fails on changed bytes; if the APK endpoint
returns 404/410 it reports the unavailable APK check separately and verifies
the pinned source route instead. Other APK transport/HTTP failures fail the run.
Run it again when refreshing source pins; `--self-test` needs no network.
The unresolved dependency follow-up is outside this documentation slice; no
compiler, library or container pin was changed. The following output line is
retained verbatim; its digest-bearing text is exempt from the line-length rule.

<!-- markdownlint-disable MD013 -->

```text
docker distroless/cc-debian13:nonroot: UPDATE_AVAILABLE current=sha256:83b9b50586253821cf63ffcec87d3d94265a093ad30da232039af962dd8f4edf latest=sha256:3ee00291cf386ec52a552e6a858d555debd74776eb4bfa53fa3df2cd6677515e
```

<!-- markdownlint-enable MD013 -->

No container build, SDK execution, real-cloud test, upstream runtime probe,
cross-platform xattr execution or benchmark was performed in this slice.
