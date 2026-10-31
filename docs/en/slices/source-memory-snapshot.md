# Quiesced memory snapshot format

This versioned local migration format complements the pinned source filesystem
reader. The exporter must hold an application writer barrier for the entire
capture; a live-only JSON API listing cannot establish complete memory state.
The source is fake-gcs-server v1.56.1 at
`896ece51e49eed0946089f3089f6343855c0d106`.

## Layout and limits

The snapshot directory contains `snapshot.jsonl` and `blobs/`. Object names are
manifest values only. Each body has an opaque, exactly 16-character lowercase
hexadecimal ordinal filename, beginning at `0000000000000001`; ordinals increase
by one across all object records. Never derive a path from an object name.

Every JSON Lines record ends with a newline. Each complete line is bounded to
128 KiB, while the serialized source attributes remain bounded to 64 KiB.
Traversal, manifest records and each object body use the import's existing
limits. Symlinks, special files, unknown fields, duplicate members, missing or
extra blobs, incomplete records and trailing data fail visibly.

The header has `kind: "snapshot"`, `version: 1`, the exact `source_revision`, a
32-character lowercase hexadecimal `capture_id`, `quiesced: true`,
`backend: "memory"` and `original_names_verified: true`. These declarations do
not themselves prove consistency: exporter tests must demonstrate that writers
cannot change membership or selected-generation bytes during capture.

Bucket rows have `kind: "bucket"`, `name`, exact-nanosecond RFC 3339 `created`,
`attributes` using the source `BucketAttrs` PascalCase fields, and a boolean
`enable_object_retention`. That flag is a derived target requirement because
the source does not store Google's enablement flag; reports identify it as
generated. It may only initialize the empty import target, never change the
protection settings of an unrelated existing bucket.

Schema 39 stores bucket times as floor microseconds plus a checked 0–999
nanosecond remainder. Both fields commit with the bucket and import journal.
JSON bucket resources and protobuf timestamps retain that precision, including
negative epoch instants. Native metadata updates retain the imported creation
time and clear the updated-time remainder to their existing microsecond clock.
Object timestamps retain their separate exact-microsecond validation; nonzero
sub-microsecond object values fail without truncation. Source empty optional
header strings mean unset and become absent native optional headers.

Object rows have `kind: "object"`, `bucket`, original `name`,
`membership: "live" | "archived"`, opaque `blob`, unsigned byte `size`, and
`attributes` using the existing source `ObjectAttrs` PascalCase schema.
`Generation`, `Created`, `Updated`, `Deleted`, classes, headers, checksums,
custom metadata, customTime, ACL and retention retain the source contract.
The archived membership and exact `Deleted` value are explicit; neither can be
inferred from generation ordering or the presence of a live version.

Buckets are in lexical order. A bucket row precedes its objects. Within each
bucket, object names are in lexical order; each name has archived generations
in ascending order, followed by at most one live generation. An archived-only
name and a bucket containing only archived objects are valid. Duplicate
generations or multiple live members fail before destination publication.
Trailing-slash memory names remain actual objects.

The last row has `kind: "complete"`, unsigned `buckets`, `generations`, `bytes`
totals and `manifest_sha256`. This digest covers every preceding exact line,
including its newline, and excludes the trailer. EOF must immediately follow
the trailer's newline. All counts and the digest must match before the importer
opens a new destination. Every imported body must match both source MD5 and
CRC32C through bounded streaming.

## Resume and cutover

Resume binds the canonical source identity, format revision, capture ID, exact
manifest and selected import limits. A changed export is a different import;
never repair an unfinished destination by substituting source files. Metadata,
membership, publication and journal progress share the catalog transaction.
An unfinished destination remains unservable.

Keep the old instance quiesced and retain its original storage or process
snapshot until import completion and selected-generation verification succeed.
Cut over clients only after the destination is complete. To roll back before
new destination writes, restore client endpoints to the still-preserved source;
after new writes, retain both states and reconcile explicitly. This importer
does not reverse-convert or silently overwrite the source.

Filesystem-normalized original names and history already discarded by the
source filesystem remain unrecoverable. A memory snapshot can preserve only
state that still exists behind the verified writer barrier. No format marker
turns a lossy source listing into a lossless backup.

## Implementation evidence

The separate [source exporter](../../../go/sourceexport/README.md) uses the
actual pinned source memory backend and an application writer barrier. Its
captured manifest is retained in `tests/fixtures/import/memory-snapshot.jsonl`;
the native CLI imported its two buckets, three generations and 28 body bytes
with both body checksums verified. A regression reads live generation 7,
archived generation 900 and archived-only generation 11, unusual original
names and selected body bytes after reopening the destination twice. The source
manifest remains byte-identical. Generated native ETags, metagenerations,
project defaults and nil-ACL defaults remain explicit transformations.

`cargo check --locked --offline --tests` passes. The scoped library invocation
below passes 27 tests, including interruption/resume/restart, bucket nanoseconds,
negative epoch, old-schema migration rollback, unchanged object precision,
retention enforcement, missing/extra blobs and manifest/attribute bounds.

```sh
cargo test --locked --offline --lib -- import:: \
  storage::catalog::bucket_timestamps:: \
  storage::catalog::timestamp_tests::
```

Exhaustive process-kill and host-power-loss coverage is not established by
these importer tests; SDK and container checks belong to final integration.
