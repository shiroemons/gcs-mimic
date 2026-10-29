# fake-gcs-server storage behavior

<!-- markdownlint-configure-file {"MD013": false} -->

Reviewed on **2026-10-24** against fake-gcs-server `v1.56.1`, commit
`896ece51e49eed0946089f3089f6343855c0d106`. Source inspection and a targeted
macOS ARM64 run establish the behavior below. The
[probe results](evidence/fake-gcs-server-filesystem.txt) record the execution scope.
This is not a full replacement test or a cross-platform durability assessment.

## Two directories with different jobs

`-data` is fixture input. The Docker entrypoint passes `-data /data`.
`-filesystem-root` is runtime persistence, defaulting to `/storage` with the CLI's
default filesystem backend. API changes go to the persistence directory.
Mounting only `/data` does not preserve new API-created objects when replacing
the container.

Both trees can look like this, but they are separate copies:

```text
<seed-or-storage-root>/
  first-bucket/
    nested/
      seed.txt
  second-bucket/
```

The seed reader treats top-level directories as buckets and relative file paths
as object names. It also creates empty buckets. It reads fixture bytes into
memory, calculates hashes and derives Content-Type from the filename extension.
The native gcs-mimic seed reader also derives Content-Type from the extension
through its own pinned table.

Source: [CLI configuration](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/internal/config/config.go),
[Docker entrypoint](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/Dockerfile),
[seed and reseed implementation](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/server.go).

## Filesystem storage and metadata

The filesystem backend writes object bodies to real files below the bucket
directory. Bucket directory names use URL path escaping; object names such as
`nested/seed.txt` become nested paths in the inspected implementation and probe.
The introductory source comment describes escaped names more broadly; the actual
`CreateObject` and `getObject` path construction is the relevant behavior.

Metadata is stored separately from the body:

- Object attributes use the `user.metadata` extended attribute when supported.
- Otherwise, the backend uses a neighboring `<object>.metadata` JSON file.
- Bucket attributes use `<bucket>.bucketMetadata` beside the bucket directory.

The metadata includes content headers, custom metadata, ACLs, generation and
checksums. Copying body files without their metadata does not create an equivalent
storage snapshot. The macOS probe exercised the xattr branch; the sidecar branch
was inspected but not executed.

An API upload holds the backend mutex and buffers the complete body while hashing,
then writes the body and its metadata. Unix body replacement uses `renameio`.
Body and metadata publication are separate operations. These observations do not
establish power-loss durability or a measured performance ranking.

The filesystem backend rejects versioning; the separate memory backend keeps
active and archived objects in process. The CLI defaults to filesystem storage,
whereas Go `Options` without `StorageRoot` selects memory storage.

Source: [filesystem backend](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/internal/backend/fs.go),
[xattrs](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/internal/backend/metadata_xattr.go),
[sidecars](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/internal/backend/metadata_file.go),
[Unix writes](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/internal/backend/writefile_unix.go),
[memory backend](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/internal/backend/memory.go).

## External edits do not reconcile metadata

The inspected filesystem path reads files and metadata on each operation;
it has no directory watcher that rebuilds object attributes.

| Action in the macOS xattr probe | Observed result |
| --- | --- |
| Edit a file under the seed directory | API bytes remain unchanged |
| Upload through the API | A named file appears under runtime storage |
| Overwrite an existing runtime file in place | GET returns new bytes and size |
| Inspect metadata after that overwrite | Hashes and generation remain unchanged |
| Replace that file with a new inode lacking xattrs | Object GET returns 404 |
| Add a plain runtime file without object metadata | GET and bucket object listing return 404 |
| Remove an existing runtime file | GET returns 404 immediately |

The in-place edit deliberately preserved the file's xattrs. Editors can use
different save strategies; the atomic replacement probe models one that loses
them. A visible body change is therefore not evidence of a consistent external
editing workflow. Sidecar behavior and Windows/Linux editor behavior were not
executed.

## Seed replay and explicit reseed

Startup calls the seed reader each time a seed directory is configured.
`POST` or `PUT /_internal/reseed` explicitly runs it again. No seed watcher was
found in the inspected path.

For filesystem storage, the probe confirmed that reseed overwrites an object
whose name is present in the seed tree while retaining unrelated API-created
objects. Restart with the same seed also overwrites an API mutation at that name.
Source inspection shows that memory reseed replaces the in-memory backend, so it
has a different lifecycle; that branch was not executed in this investigation.

gcs-mimic also applies its seed directory on every
startup, and its image reads `/data` by default. Like fake-gcs-server, it
replaces an object that differs from its seed file in bytes or metadata,
including API metadata edits, with a new live generation. Unlike
fake-gcs-server, an object exactly matching what seeding would write keeps its
generation. Objects without a seed file are retained, and removing a seed
file deletes nothing. There is no reseed route or watcher. 0.2.1 and earlier
imported seed files once into a fresh catalog; see the
[0.3.0 release notes](releases/0.3.0.md).

## Multiple buckets and CORS

Both products support multiple buckets. This does not imply that their settings
have the same effect.

fake-gcs-server installs one global CORS middleware with wildcard origins,
credentials, a common method/header allowlist and exposed `Location`.
`-cors-headers` extends the global allowed request headers. In the probe, a JSON
bucket PATCH containing `cors` succeeded but did not store that field; preflight
from a different origin received the same wildcard rule on both buckets.

gcs-mimic persists CORS on each bucket through JSON insert/PATCH/PUT. Its XML
GET/HEAD/PUT and preflight use that bucket's rules; JSON requests use independent
defaults. This separation follows the
[Google CORS contract](https://docs.cloud.google.com/storage/docs/cross-origin).
The XML bucket CORS configuration subresource itself remains unimplemented.
Bucket IAM, labels and supported versioning settings are also separate per bucket;
advanced settings retain their feature-ledger limitations.

Source: [global middleware and routes](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/server.go),
[bucket request fields](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/bucket.go),
[gcs-mimic CORS contract](slices/cors.md).

## Required gcs-mimic follow-up

The agreed usability requirement is **bidirectional synchronization**: users can
inspect and edit a configured bucket/object directory, and API changes appear
in that same directory. Current gcs-mimic storage does not provide this.
Seed import and a one-time migration converter do not satisfy it.
The planning requirement is recorded in
`compatibility/features.json` under `delivery_plan.migration.filesystem_sync_contract`.

Keeping SQLite is compatible with this requirement. A possible design keeps
SQLite as the metadata/index catalog and treats the visible directory as a
synchronized working tree. External changes enter the existing streaming,
hashing, generation and publication path; committed internal blobs stay immutable.
Updating only a SQLite size or pathname would leave inconsistent object state.
This is a design candidate, not an implemented watcher or finalized layout.

Before implementation, settle and verify:

- API/external create, overwrite, delete and rename behavior in both directions.
- Editor temporary files, incomplete writes and atomic replacement.
- Conflicts between an API write and an external edit, with visible failure or
  recovery instead of silent data loss.
- Event deduplication, self-generated write events, overflow and bounded rescan
  after restart or a missed-event signal.
- Name mapping for file/directory collisions, platform restrictions and symlinks.
- Metadata ownership, checksum/ETag/generation updates and preservation of history,
  bucket rules and permissions.
- Consistent restart/recovery across visible files, immutable blobs and SQLite.

The conflict policy, synchronization delay and unusual-name representation remain
open. Safe external-edit reconciliation goes beyond what the inspected
fake-gcs-server filesystem path guarantees. No new runtime option is available yet.
