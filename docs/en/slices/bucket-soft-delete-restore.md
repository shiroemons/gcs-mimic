# Bucket soft delete and restoration

<!-- markdownlint-configure-file {"MD013": {"code_blocks": false}} -->

Scope: BEHAVIOR-BUCKET-017, JSON-BUCKETS-001 (delete), JSON-BUCKETS-002 (get),
JSON-BUCKETS-006 (list) and JSON-BUCKETS-010 (restore). Every scoped row is
PARTIAL because the PENDING list below is not empty. Object soft delete and
`objects.restore` keep their own contract in
[sdk-soft-delete-restore.md](sdk-soft-delete-restore.md).

## Contract

Checked 2026-10-31 against the official
[soft delete overview](https://docs.cloud.google.com/storage/docs/soft-delete),
[bucket resource](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets),
[buckets.get](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/get),
[buckets.list](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/list)
and [buckets.restore](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/restore).

- Deleting a bucket that has a soft delete policy makes it soft-deleted. A
  soft-deleted bucket cannot be read or modified; it can only be listed or
  restored. Its name is free, and a live bucket with the same name blocks
  restoration until that live bucket is removed.
- The resource has `generation` (int64 string, "the version of the bucket"),
  `softDeleteTime` and `hardDeleteTime`. `hardDeleteTime` is always equal to or
  later than the latest `hardDeleteTime` of any soft-deleted object in the bucket.
- `buckets.get`: `generation` selects a soft-deleted version and is required when
  `softDeleted=true`; `softDeleted` is required when `generation` is given.
  Permission: `storage.buckets.get`.
- `buckets.list`: `softDeleted=true` lists only soft-deleted versions as distinct
  results ordered by name and generation; the page token carries the generation.
  Permission: `storage.buckets.list`.
- `buckets.restore`: `POST b/{bucket}/restore?generation=G`, no request body,
  optional `projection`, returns the bucket. IAM policies and metadata are
  restored, managed folders are not, and `softDeleteTime`/`hardDeleteTime` are
  cleared. The JSON API restores only the empty bucket; objects are restored
  afterwards with `objects.restore`. Permission: `storage.buckets.restore` at the
  project level.

## Acceptance cases

- BUCKET-GENERATION: native insert/get/list/PATCH/PUT/restore render
  `generation`. New buckets draw a 19-digit nanosecond-scale value, as Cloud
  Storage returns (observed 2026-10-31), from a bucket-only allocator that yields
  `max(current time in nanoseconds, previous + 1)`; it is unique and increasing
  under a frozen clock, so a recreated name always gets a new generation. Object
  generations keep their separate microsecond allocator. Schema 45 backfilled
  existing buckets with their creation time in microseconds; schema 46 multiplies
  every live and soft-deleted bucket generation by 1000 (saturating at the largest
  representable multiple), moves the retained objects of soft-deleted versions to
  the matching internal key and seeds the allocator, so order, uniqueness and
  `restore?generation=` keep working. The legacy-emulator profile keeps its
  previous resource shape.
- BUCKET-SOFT-DELETE: JSON, XML and gRPC bucket deletion share one catalog
  transaction. When `softDeletePolicy.retentionDurationSeconds` is positive, the
  bucket row, IAM bindings and bucket/default object ACL entries move to the
  soft-deleted table with `softDeleteTime = now` and
  `hardDeleteTime = max(now + retention, latest retained object hardDeleteTime)`.
  The bucket incarnation, notification configurations and sync state are dropped
  as for permanent deletion. A bucket with no stored policy or a zero policy
  keeps permanent deletion; new native buckets get a seven-day policy unless the
  server default is 0 or the request says otherwise. Permission, metageneration
  conditions and the live-object emptiness check run first and are unchanged.
- BUCKET-SOFT-DELETE-OBJECTS: soft-deleted objects stay with their bucket version.
  They are re-keyed to an internal name that no bucket name can take, so a new
  live bucket with the same name never sees them. After the bucket is restored
  they list again with `softDeleted=true` and can be restored with
  `objects.restore` until their own `hardDeleteTime`.
- BUCKET-SOFT-DELETED-GET: `GET b/{bucket}?softDeleted=true&generation=G`
  returns the version with `generation`, `softDeleteTime` and `hardDeleteTime`.
  An unknown or expired version returns 404 notFound. `softDeleted=true` without
  `generation`, or `generation` without `softDeleted=true`, returns 400 required.
  Metageneration conditions combined with `softDeleted=true` return the shared
  unsupported 400.
- BUCKET-SOFT-DELETED-LIST: `GET b?project=P&softDeleted=true` lists only
  unexpired soft-deleted versions owned by project P, ordered by name and
  generation, with `prefix`, `maxResults` and a page token that carries the name
  and generation. Live buckets are never included.
- BUCKET-RESTORE: `POST b/{bucket}/restore?generation=G` restores the version in
  one transaction: the bucket row with its original generation, metageneration,
  timestamps and settings, the IAM bindings (with a new policy revision), the
  bucket/default object ACL entries, a new incarnation and the retained objects.
  The response is the live bucket without deletion times. A missing `generation`
  returns 400 required, an unknown or expired version 404 notFound, a live bucket
  with the same name 409 conflict, and a request body 400. All failures leave the
  catalog unchanged.
- BUCKET-SOFT-DELETE-AUTH: in strict mode, soft-deleted get, list and restore
  require `storage.buckets.get`, `storage.buckets.list` and
  `storage.buckets.restore` on the soft-deleted version's project. The deleted
  bucket's own IAM policy is not consulted. Anonymous callers get 401, and callers
  without the permission on that project get 403. Permissive mode checks nothing.
- BUCKET-SOFT-DELETE-EXPIRY: reads compare `hardDeleteTime` with the injected
  clock, so an expired version disappears immediately. The catalog maintenance
  scheduler includes the earliest `hardDeleteTime` in its next deadline and then
  permanently removes expired versions with their retained objects. It removes at
  most 16 versions per transaction, and blobs are reclaimed through the pending
  deletion journal. A pass reads the earliest `hardDeleteTime` first and takes
  the write lock only when a version is due, so a pass that a request wakes does
  not wait on another writer. No polling loop or timer runs while no version is
  due.
- BUCKET-SOFT-DELETE-PERSIST: additive schema 45 keeps live and soft-deleted
  state across restart; schema 46 rescales the generations as described above
  (`schema_46_scales_bucket_generations_and_retained_keys_to_nanoseconds`).
  Admin reset clears soft-deleted versions.
- BUCKET-SOFT-DELETE-SDK: with the pinned .NET SDK 5.0.0, create a bucket with a
  seven-day policy, upload an object, read the bucket generation, delete the
  object and the bucket, get it with `SoftDeleted`/`Generation`, call
  `RestoreBucketAsync`, restore the object with `RestoreObjectAsync` and read its
  bytes.

## Local choices

- A live bucket with the same name makes restore return 409 conflict; the
  official page states that restoration is blocked but does not name the status.
- The legacy-emulator profile does not render `generation`, matching the reference
  emulator's bucket shape.
- Soft-deleted bucket state changes only catalog rows: no blob is copied, linked
  or renamed, and there is no new file publish, rename or fsync step. Atomicity
  therefore rests on the single SQLite transaction, which is verified with
  injected statement failures and a restart.

## PENDING

gRPC soft-deleted bucket reads and restore, retaining notification
configurations, managed folders, `userProject`, HNS restore tokens, soft-deleted
listing across projects, XML API access to soft-deleted buckets, exact provider
error reasons and precedence, and real-cloud differential evidence.

New native buckets receive the seven-day default policy; see SOFT-DELETE-DEFAULT
in [the object contract](sdk-soft-delete-restore.md).
