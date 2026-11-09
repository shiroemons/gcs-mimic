# Hierarchical namespace, folders and zonal Rapid buckets research

Status: research for future slices. Nothing here is implemented, and the [feature ledger](../../../compatibility/features.json) still owns status. Features: `JSON-FOLDERS-001` to `JSON-FOLDERS-006`, `BEHAVIOR-BUCKET-018`, `BEHAVIOR-RAPID-001` and related rows.

Checked on 2026-11-09 against commit `554c7b04`; line numbers refer to that commit. The research read code and official documents. It did not run the code or contact Cloud Storage.

## Summary

- Hierarchical namespace is set only at creation, cannot change and requires uniform bucket-level access. Writes create parent folders implicitly and nothing removes them automatically.
- Rename needs catalog changes only, because blob IDs are independent of names. One immediate transaction can run range UPDATEs and insert a completed Operation row.
- A zonal JSON create uses region `location`, `customPlacementConfig.dataLocations=[zone]`, `storageClass=RAPID`, hierarchical namespace and uniform access. This is derived from the gcloud implementation; no REST example exists.
- Split: hierarchical namespace with JSON folders and operations (large), then Storage Control gRPC and REST (medium to large), and zonal Rapid (medium) after the first.

## Google contract

Hierarchical namespace ([overview](https://docs.cloud.google.com/storage/docs/hns-overview), [folders overview](https://docs.cloud.google.com/storage/docs/folders-overview), both 2026-11-03; [rename](https://docs.cloud.google.com/storage/docs/rename-hns-folders), 2026-11-07):

- Requires "a bucket must also enable uniform bucket-level access" and cannot change after creation. GA 2024-10-15.
- Unsupported: Bucket Lock, relocation with write downtime, cross-bucket replication, object holds, object ACLs, Object Retention Lock, Object Versioning. Soft delete is compatible.
- Upload, Rewrite and Compose create parents "as long as you have the necessary permissions". Empty folders are not removed automatically.
- Name mapping: `dir1/foo.txt` to `dir1/`, `dir1/` to `dir1/` (remaining an object), `/foo.txt` to `/`, `dir1//foo.txt` to `dir1/` and `dir1//`. Folders and objects are "distinct resources".
- Limits: folder name 512 bytes, depth 50 (15 with managed folders), object name 1024 bytes with folder and base parts at most 512.
- "You can only delete an empty folder with no folders or objects". A same-named managed folder is deleted with it. A bucket can be deleted with empty folders left.
- [Rename](https://docs.cloud.google.com/storage/docs/json_api/v1/folders/rename) needs `storage.folders.rename` on the source and `storage.folders.create` on the destination, with no body.
- The response is an Operation: `name`, `metadata` (`sourceFolderId`, `destinationFolderId`, `commonMetadata{createTime,endTime,updateTime,type:"rename-folder",requestedCancellation,progressPercent}`), `done`, and `error` or `response(Folder)`.
- Rename cannot be cancelled. A child over the name or depth limit fails it and restores the state. During rename only reads and listing are allowed, and proto `pending_rename_info` blocks writes on folders, managed folders and objects. Creation time is preserved, which affects lifecycle age, and generation preservation is assumed by the soft-delete example.
- Folder resource: `kind,id(bucket/name),selfLink,name,bucket,createTime,updateTime,metageneration`, plus `pendingRenameInfo.operationId` in Discovery.
- List: `delimiter` only `/`, `prefix` empty or ending in `/`, at most 1000 per page.
- `includeFoldersAsPrefixes` adds empty folders and managed folders to `prefixes[]`, requires `delimiter=/`, and default listing excludes empty folders.
- A rename can create same-named, same-generation soft-deleted objects, which then require `restoreToken`.
- gcloud parses Operation names as `projects/_/buckets/B/operations/ID`.
- Since 2026-08-26, Delete lifecycle rules also delete matching empty folders. `sizeBelowBytes` does not apply to empty folders.

Document conflicts:

- Rename condition names: Discovery and the gcloud generated client use `ifSourceMetagenerationMatch`, the reference uses `ifMetagenerationMatch` (`SPEC-002`). Accept both.
- `folders.list` page size: Discovery and gcloud use `pageSize`, the reference uses `maxResults`. Accept both.

Zonal and Rapid ([Rapid buckets](https://docs.cloud.google.com/storage/docs/rapid/rapid-bucket), 2026-11-02; [storage classes](https://docs.cloud.google.com/storage/docs/storage-classes), 2026-10-30; [create](https://docs.cloud.google.com/storage/docs/rapid/create-zonal-buckets)):

- GA 2026-03-10. RAPID is "only compatible with zonal buckets".
- Creation: `gcloud storage buckets create gs://B --location=us-east1 --placement=us-east1-b --default-storage-class=RAPID --enable-hierarchical-namespace --uniform-bucket-level-access`, with namespace and uniform access required.
- `locationType`: gcloud compares with `'zone'`; documents give no value.
- Writes: gRPC BidiWriteObject only (not WriteObject), XML except multipart, no JSON. Reads work on JSON, XML and gRPC. Objects are always RAPID and appendable, have no MD5, and can be overwritten, updated and renamed after finalization.
- Incompatible: Rapid Cache, Autoclass, Bucket Lock, composite objects, XML multipart, holds, SetStorageClass lifecycle, Object Retention Lock, relocation, resumable, rewrite, Requester Pays, object ACLs, CORS, CSEK, HMAC, Versioning, soft delete, cross-bucket replication, BigQuery. Default soft delete does not apply.

Unresolved by documents:

- Rename to an existing destination and whether the destination parent must exist.
- HTTP status of a non-empty folder delete.
- Whether implicit parents need `storage.folders.create`.
- Whether soft-deleted children move with a rename, the error for writes during rename, and whether rename emits notifications.
- JSON folder trailing-slash rules (gRPC requires it, JSON is silent, gcloud sends input as given) and `selfLink` encoding of `/`.
- `deleteRecursive` GA: present in Discovery, proto and the gcloud generated client, absent from the HTML index and release notes, unused by gcloud commands.
- Zonal error shapes, response `location`, appendability after XML PUT and JSON `objects.move`.

## Code touchpoints

- Bucket creation: `src/json/buckets.rs:17-20`, `:126-150`, `:259`, `:344`, `:622`, `:946-965`, `:1034`; `src/grpc/v2/buckets.rs:272-273`, `:456`; `src/xml/buckets/create.rs:113`; `src/bucket_init.rs:802-804`, `:830`.
- Types: `src/bucket.rs:59` (RAPID refusal), `:145` (`BucketRecord`, 22 struct literals).
- Catalog: `src/storage/catalog.rs:149`, `:151` (`bucket_columns!`); `schema.rs:354`; `bucket_update.rs:238-253` (refuse disabling uniform access).
- Bucket delete: `buckets.rs:123-144`, `bucket_soft_delete.rs:74`, `:93-95`, `:145`, `:289`, `admin/reset.rs:207` (its retained-table test catches missing tables).
- Parents: `objects/publication.rs:26` (add namespace to `BucketPublication`), `:279` (`publish_in_transaction` serves upload, resumable, derived, move, restore and admin), `appendable.rs:944` (separate path).
- Listing: `objects/list.rs:101` (`prefix_end`), `:485-520`; `src/json/object_list.rs:56`, `:106`; gRPC at `object_io/read.rs:214` shares `list_page`.
- Routing: `src/server/unsupported.rs:11-16`, `:27-29`; `src/server/routing.rs` before `:406` and near `:420`. New `src/json/folders.rs`, `src/server/folders.rs`, `src/storage/catalog/folders.rs`.
- Storage layout: add `hierarchicalNamespace` and `customPlacementConfig` at `src/json/buckets.rs:950-961`. gcloud reads this to detect hierarchical buckets.
- `src/auth/roles.rs` already defines folders and bucket operation permissions. `connection.rs:114` invalidates caches automatically.
- Rapid: `src/server/objects.rs:75-79`, `:146`, `src/server/resumable.rs`, `rewrite.rs`, `src/xml/mod.rs:689`, `src/grpc/v2/mod.rs:214-217`, `object_io/resumable.rs:57`, `update_copy.rs:279`, `:742`, default soft delete at `catalog/buckets.rs:27`.
- Storage Control is out of the first slice: `src/grpc/v2/mod.rs:61` only forwards three IAM RPCs, and `storage_control.proto` and `google.longrunning` need code generation. The Go SDK uses this client by default for folders, so it should follow the JSON slice.

## Design

- Types: `BucketNamespace { Flat, Hierarchical }`, `ZonalPlacement(zone)` and `FolderName`, parsed once at the boundary with trailing `/` normalization, the 512-byte and depth limits and an ancestor iterator.
- `BucketRecord` and `BucketPublication` carry the namespace. It never changes after creation, so there is no time-of-check gap.
- One migration adds `hierarchical_namespace` and `placement_zone` to `buckets` and `soft_deleted_buckets` (they share `bucket_columns!`), plus `folders(bucket,name,metageneration,time_created,updated, PK(bucket,name)) WITHOUT ROWID` and `bucket_operations(bucket,id,kind,done,metadata,response,error,created,updated, PK(bucket,id)) WITHOUT ROWID`.
- Implicit parents: in `publish_in_transaction`, only for hierarchical buckets, `INSERT OR IGNORE` ancestors, O(depth). Refuse over-limit depth or segment length. Require only `objects.create` at first and record the `folders.create` question as a deviation.
- Rename, one `sql::immediate` transaction like `object_move.rs`:
  1. Check source and destination permissions, namespace, source existence and metageneration.
  2. Refuse an existing destination with 409 (a local choice) and a destination under the source.
  3. Check child limits with static `EXISTS` SQL.
  4. UPDATE `folders`, `objects`, `appendable_objects` and `acl_object_entries` over `name >= ?src AND name < prefix_end(?src)`, with `objects` limited to `soft_delete_time IS NULL`. Leaving soft-deleted objects behind is a local choice.
  5. When `change_journal` is enabled, record Delete and Publish by keyset in batches of 256.
  6. INSERT a completed Operation row and commit.
- Live names cannot collide under the namespace invariant. The single generation allocator leaves only import as a primary-key collision, which rolls back as 409.
- A single transaction is acceptable at local scale of about a thousand objects. No fsync or rename step is added; test rollback on mid-transaction failure.
- `pendingRenameInfo` is never observed because rename completes synchronously. Record that as a deviation and keep `PARTIAL`.
- Operations: JSON `operations.get` and `list` use `storage.bucketOperations.*`, trim old completed rows in the same transaction, and `cancel` sets only `requestedCancellation`.
- Listing merges the next folder row and next object by name, one query per step. `maxResults` counts prefixes, and `includeFoldersAsPrefixes` applies only to hierarchical buckets.
- Rapid: `src/bucket/rapid.rs` holds `ZonalRestriction`, with one function taking a `WriteSurface` enum, called before reading a body. Appendable objects remain allowed in every bucket, because SDK probes use `Writer.Append` on ordinary buckets. `StorageClass::Rapid` is reachable only through zonal creation; `"RAPID"` on ordinary buckets stays refused.

## Acceptance cases

- Bucket: `BK-01` create and layout; `02` no uniform access 400; `03` versioning and retention refused, soft delete allowed; `04` changing namespace refused, same value no-op; `05` disabling uniform access refused; `06` restart and migration; `07` bucket with only empty folders deletes, soft-delete restore returns folders.
- Folder: `FOL-01` insert fields and slash normalization; `02` missing parent refused, `recursive=true` atomic; `03` duplicate 409, 512 bytes and depth 50 succeed, 513 and 51 fail; `04` flat bucket refused, missing bucket 404; `05` get and delete metageneration conditions; `06` non-empty delete refused for object, child and appendable; `07` only soft-deleted children allow delete; `08` list prefix, delimiter, offsets, `pageSize`, `maxResults`, 1000 cap, token.
- Objects: `OBJ-01` upload, resumable, XML PUT, compose, rewrite, copy, move, restore and appendable create parents; `02` name mapping table; `03` object delete leaves folders; `04` 1024-byte and 512-byte segment bounds.
- Listing: `LST-01` hierarchical `includeFoldersAsPrefixes`; `02` flat unchanged, page-boundary merge.
- Rename: `REN-01` descendants move with generation, creation time, CRC32C and bytes unchanged; `02` Operation shape, get and list; `03` both condition names, 412 unchanged; `04` limit overflow and injected failure leave state unchanged; `05` missing source 404, existing destination 409, under-source and same-name refused; `06` strict source and destination permissions; `07` concurrent rename, rename with upload and list, stale appendable flush fails; `08` change journal; `09` cancel only sets the flag.
- Zonal: `ZON-01` create, `locationType=zone`, layout; `02` missing namespace, uniform access, RAPID, bad zone, multiple locations, Autoclass, versioning and soft delete refused; `03` JSON insert and resumable, gRPC WriteObject and StartResumableWrite, compose, rewrite and XML multipart refused, XML PUT and BidiWrite allowed; `04` no MD5, RAPID, no soft-delete time, ACL, CORS, HMAC and CSEK refused; `05` default soft delete absent, ordinary appendable and RAPID refusal unchanged.
- Clients: gcloud 591 with the `CLOUDSDK_API_ENDPOINT_OVERRIDES_STORAGE` setup from `examples/gcloud/gcloud-example.sh`: `buckets create --enable-hierarchical-namespace`, `folders create --recursive`, `folders list`, `folders describe`, `mv` and `folders delete`. Go Storage Control follows that slice. Zonal `cp` switches to a gRPC Bidi client on `locationType=zone`; whether it can reach the emulator is unknown.

## Ledger and gaps

- JSON slice: `JSON-FOLDERS-001/003/004/005/006`, `BEHAVIOR-BUCKET-018`, `BEHAVIOR-FOLDER-001/002/003`, `EXT-BEH-006`, `BEHAVIOR-OBJECT-020`, `JSON-OPERATIONS-003/004`, and `002` limited to cancel scope. `JSON-FOLDERS-006` and `BEHAVIOR-FOLDER-003` stay `PARTIAL` while `pendingRenameInfo` is unobservable. `BEHAVIOR-OBJECT-032` stays `NOT_STARTED` (the schema key cannot express duplicates).
- Storage Control: `EXT-CTRL-002/004/008/015/018/024`, `EXT-CTRL-REST-001` to `006`, `EXT-LRO-001` to `005`, per `SPEC-007`.
- Zonal: `BEHAVIOR-RAPID-001`, notes on `002` to `005`, `docs/en/slices/grpc-v2-rapid-append.md`.
- Reconcile first: `SPEC-002`, `SPEC-006` (`deleteRecursive`, `JSON-FOLDERS-002`), `SPEC-007`, `SPEC-001`, `SPEC-GRPC-DERIVED-OWNER-VIEW`, `SPEC-SOFT-DELETE`, `SPEC-BUCKET-SOFT-DELETE`. Add gaps for `pageSize` and `maxResults`, zonal REST and `locationType`, existing-destination rename, non-empty delete and implicit permissions. Update `layout-no-namespace-or-placement` in `docs/en/slices/bucket-storage-layout.md`.

## Decisions needed

- `deleteRecursive`: its GA status is unresolved. The recommendation is `PARTIAL` with a deviation linked to `SPEC-006`; otherwise it keeps answering 400.
- Legacy profile: the recommendation is to enable hierarchical namespace only in the native profile first and keep the legacy rejection.

## Overlap

- Schema number and `bucket_columns!`: Cloud KMS, managed folders and lifecycle.
- Managed folders: `unsupported.rs`, `routing.rs`, linked delete, depth 15, `includeFoldersAsPrefixes`, and bucket emptiness at `buckets.rs:123`. Implement them first.
- Cloud KMS: `UNSUPPORTED_FIELDS`, `bucket_init.rs:802`, the 22 `BucketRecord` literals.
- Lifecycle: empty-folder Delete depends on the folders table, zonal SetStorageClass refusal connects to Rapid, and preserved creation time affects age.
