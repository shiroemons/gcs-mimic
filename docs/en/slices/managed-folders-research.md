# Managed folders research

Status: research for a future slice. Nothing here is implemented, and the [feature ledger](../../../compatibility/features.json) still owns status. Features: `JSON-MANAGEDFOLDERS-001` to `JSON-MANAGEDFOLDERS-008`.

Checked on 2026-11-09 against commit `554c7b04`; line numbers refer to that commit. The research read code and official documents. It did not run the code or contact Cloud Storage.

## Summary

- Seven methods are documented: delete, get, getIamPolicy, insert, list, setIamPolicy and testIamPermissions. `update` is documented only as a Rapid Cache PATCH, so its GA status is unresolved under `SPEC-006`.
- Object authorization composes additively across project, bucket and ancestor managed folders, and `check_access` can centralize it.
- One table and a scoped `BucketAccess` are enough. Medium to large slice, in three parts: CRUD and policy; effect on object operations; listing and bucket interaction.

## Google contract

- Resource: `kind`, `id` (bucket and folder name), `selfLink`, `name`, `bucket`, `createTime`, `updateTime` (last change to "metadata or IAM policy"), `metageneration`. `rapidCacheConfig` appears only in Discovery and a how-to. There is no generation ("Managed folders don't have a generation number").
- Name: 1 to 1024 UTF-8 bytes, trailing `/` required, at most 15 `/`, no CR or LF, not `.` or `..`, not starting with `.well-known/acme-challenge/`. A child can be created without a parent. The count is unlimited.
- Trailing slash: JSON examples send `example-dir` and responses show `example_dir/example_dir2/`; the proto says "leading and trailing `/` are allowed". Whether the server adds it is unresolved; the examples suggest it does.
- Uniform bucket-level access is required to create and manage them. Disabling it requires that "You have deleted any managed folders". The XML API cannot manage them.
- Delete returns an empty body. Empty means "no objects or managed folders that it applies to". `allowNonEmpty=true` also requires `setIamPolicy`, and a same-named simulated folder remains.
- `ifMetagenerationMatch` and `ifMetagenerationNotMatch` apply to get, delete and update. `optionsRequestedPolicyVersion` is at least 1, and at least 3 with Conditions. The `resourceId` is `projects/_/buckets/B/managedFolders/NAME`.
- List returns items by name, with no `nextPageToken` on the last page. The reference says `maxResults`, while Discovery, gcloud 591.0.0 (`pageSize=1000`) and Go API v0.311.0 say `pageSize`; the proto caps at 1000. Treat `pageSize` as canonical and accept `maxResults` as an alias. Sending both is unresolved.
- `testIamPermissions` requires no permission.
- Errors: status-code pages do not cover managed folders. `allUsers` under public access prevention is 412 (documented). List with uniform access off is 412 (gcloud suppresses `PreconditionFailedError`, and `xtask/src/differential/bucket.rs:188` records it, with no document). Duplicate insert, non-empty delete, uniform-off insert and get with NotMatch (304 or 412) are unresolved.
- `update`: present in Discovery 20261103 (identical to the pinned 20261013), a how-to PATCH, iam-json `storage.managedfolders.update`, gcloud's ensure task and proto `UpdateManagedFolder` (only `rapid_cache_config`). Absent from the resource method table, with `managedFolder/update` returning 404. Release notes record Preview on 2023-10-26 and Console on 2024-04-03, with no GA entry.

Sources: [managed folders](https://docs.cloud.google.com/storage/docs/managed-folders), [create and manage](https://docs.cloud.google.com/storage/docs/creating-managing-managed-folders), [JSON resource](https://docs.cloud.google.com/storage/docs/json_api/v1/managedFolder), [quotas](https://docs.cloud.google.com/storage/quotas), [uniform access](https://docs.cloud.google.com/storage/docs/uniform-bucket-level-access), [release notes](https://docs.cloud.google.com/storage/docs/release-notes), [Discovery](https://storage.googleapis.com/$discovery/rest?version=v1), googleapis master `bf54c10` `storage_control.proto`.

## IAM effect

- A folder policy applies to objects whose names start with the folder path, and nested folders add up ("apply additively throughout the resource hierarchy"). Deny policies are unavailable; Conditions are available with version 3.
- A managed folder is a real resource, unlike a plain prefix, and it can coexist with a same-named object. Deleting it removes the boundary; the exact behavior afterward is unresolved.
- Listing works with `objectViewer` on the folder and `prefix=folder/`. Documents also direct `includeFoldersAsPrefixes=true` and `delimiter=/`. Whether a list without a prefix over partly inaccessible folders is 403 or partial is unresolved; the proposal evaluates only bucket permission.
- `storage.managedFolders.*` in camelCase matches iam-roles, iam-permissions, the proto, Discovery and `src/auth/roles.rs`. Lowercase `storage.managedfolders.*` is a documentation spelling variant. Handling of lowercase in `testPermissions` is unresolved (`src/json/iam.rs:88` rejects it). iam-permissions has no `update`.
- Unresolved: whether a parent grant covers a child's `storage.managedFolders.*`, where `create` is evaluated (the proto says "on the bucket"), roles beyond `storage.objects.*`, whether a policy update raises metageneration (proposed: yes), whether `includeFoldersAsPrefixes` needs `storage.managedFolders.list` (proposed: no).
- Permissive mode continues to skip the check. Strict mode evaluates registered principals through folder policies and explicitly rejects Conditions and unsupported principals. `allUsers` counts as public; `allAuthenticatedUsers` is not anonymous public access.

Sources: [IAM](https://docs.cloud.google.com/storage/docs/access-control/iam), [IAM for managed folders](https://docs.cloud.google.com/storage/docs/access-control/using-iam-for-managed-folders), [roles](https://docs.cloud.google.com/storage/docs/access-control/iam-roles), [permissions](https://docs.cloud.google.com/storage/docs/access-control/iam-permissions), [iam-json](https://docs.cloud.google.com/storage/docs/access-control/iam-json), [listing](https://docs.cloud.google.com/storage/docs/listing-objects), [making data public](https://docs.cloud.google.com/storage/docs/access-control/making-data-public), [public access prevention](https://docs.cloud.google.com/storage/docs/public-access-prevention).

## Code touchpoints

- Routing: remove everything but update from `src/server/unsupported.rs:18-26`, add method IDs at `:74`, and route before the unsupported check at `src/server/routing.rs:406`. New modules: `src/json/managed_folders.rs`, `src/server/managed_folders.rs` and `src/storage/catalog/managed_folders.rs`, following the notifications modules. Extend `src/json/page_token.rs`.
- Permissions: adding 7 to `src/auth/mod.rs:107` makes 35, past `assert!(ALL.len() <= 32)` at `:138`, so `bit()` at `:72` and `PermissionSet` at `:141` become u64. Add `ManagedFolder` to `Resource` in `src/auth/denied.rs` and update the expectation at `tests/it_xml_auth/auth_http/iam.rs:102`.
- Policy: `validate_role` (`src/auth/policy_input.rs:59`) rejects only Project, so it needs a grant level (bucket-only legacy and admin roles are 400 on folders). Parameterize `resourceId` at `src/json/iam.rs:221`, extend validation at `src/storage/catalog/policies.rs:69-70`, and share `policy_revision`.
- Authorization: add a scope to `BucketAccess::Policy` (`src/bucket.rs:681`), evaluate in `check_access` (`policies.rs:180`), let `bucket_access` (`:252`) take the scope, and add folder `allUsers` to `publicly_readable` (`src/storage/catalog/objects.rs:384`).
- Pass object names through `src/server.rs:1643` and `src/server/context.rs:143` from `src/server/objects.rs:51,256,291,332-333`, `rewrite.rs:74,78`, `objects/derived.rs:52`, `xml_read.rs:37`, `src/xml/put.rs:444`, `post.rs:634`, `copy.rs:150,156`, `resumable.rs:122`, `multipart.rs:217`, `src/grpc/v2/mod.rs:113`, `src/grpc/v1/mod.rs:270`, `src/grpc/v2/update_copy.rs:638`, and listing at `src/storage/catalog/objects/list.rs:79`.
- Query cost: strict mode adds one SELECT only when bucket access is refused, and permissive adds none. The check shares the `authorized_bucket` transaction, so there is no time-of-check gap. `read_cache.rs:210` applies only to permissive. Writes invalidate `publicly_readable` (`connection.rs:114`).
- Bucket interaction: add folders to the non-empty check at `buckets.rs:124` (unresolved for JSON delete). Soft-delete restore does not return managed folders ("You can't recover managed folders"). Refuse disabling uniform access with folders present at `bucket_update.rs:121,230`, code unresolved. Add a delete at `admin/reset.rs:208-209` and use `ON DELETE CASCADE` like `notifications.rs:20`.
- Listing: merge into `list_page_within` (`list.rs:416`), shared by gRPC; update `update_copy.rs:1724`.
- gRPC: leave Storage Control CRUD to a later slice; `update_copy.rs:41` and `src/grpc/v2/mod.rs:61,160` stay UNIMPLEMENTED, along with `EXT-CTRL-003/005/010/016/025` and `EXT-CTRL-031`. Catalog functions can be shared later.

## Design

```sql
CREATE TABLE managed_folders (
  bucket TEXT NOT NULL COLLATE BINARY REFERENCES buckets(name) ON DELETE CASCADE,
  name TEXT NOT NULL COLLATE BINARY,
  metageneration INTEGER NOT NULL CHECK(metageneration > 0),
  time_created INTEGER NOT NULL,
  updated INTEGER NOT NULL,
  bindings TEXT NOT NULL CHECK(length(CAST(bindings AS BLOB)) <= 1048576),
  revision INTEGER NOT NULL CHECK(revision > 0),
  PRIMARY KEY(bucket, name)
) WITHOUT ROWID;
```

- Metageneration starts at 1 and increases on IAM changes. The etag uses `policy_etag(revision)`. A new folder has empty bindings; the provider default is unresolved.
- `ManagedFolderName` (`TryFrom<&str>`) reuses `crate::object::name::validate`, adds the trailing `/` and caps at 15 `/`. Handling of `//` and a leading `/` is unresolved (proposed: accept).
- `enum AccessScope { Bucket, Object(Box<str>), Prefix(Box<str>) }`. Passing a folder name as `Object` serves `storage.managedFolders.*`. `check_access` matches exhaustively, and compose switches scope per source with `for_object`.
- Ancestors: enumerate up to 15 `/`-terminated prefixes and run the static `SELECT bindings FROM managed_folders WHERE bucket = ?1 AND name IN (?2,…,?16)` through `prepare_cached`, binding NULL for unused slots. It only seeks the primary key. One read serves both the requested permission and `OBJECTS_DELETE`.
- Order: project, bucket, bucket ACL, folder. Uniform access keeps folders away from object ACLs, so `object_granted` is unchanged.
- Add typed `CatalogError` variants rather than strings and use `unavailable` from `src/storage/failure.rs`.

## Acceptance cases

- Success: `MF-01` insert fields, metageneration 1; `02` trailing slash added; `03` child without parent; `04` `%2F` get; `05` list order, prefix, `pageSize`, token, last page; `06` new policy and `fields=bindings,etag`; `07` full replace, stale etag 412, `updateTime`; `08` testIamPermissions needs no permission and returns held ones; `09` empty delete 204, then 404.
- Failure and bounds: `MF-10` uniform off insert and list 412; `11` duplicate 409, code to confirm; `12` invalid names 400; `13` non-empty delete fails without `allowNonEmpty`, succeeds with it, objects remain; `14` missing `setIamPolicy` 403; `15` condition mismatch; `16` missing folder or bucket 404; `17` bucket-only roles, Conditions and 1501 principals 400; `18` `allUsers` under public access prevention 412, existing grants ineffective; `19` update explicitly 400.
- Strict permissions: `MF-20` per-method permissions; `21` `a/` viewer reads `a/o` and not `b/o`; `22` object admin limited to scope; `23` nested additive; `24` every path: JSON get, media, patch, delete, copy, rewrite, compose, move, restore, resumable, XML GET, PUT, DELETE, multipart, signed URLs, gRPC; `25` list with `prefix=a/` succeeds, none or `b/` 403; `26` `allUsers` anonymous get with public Cache-Control, `allAuthenticatedUsers` not anonymous; `27` deleting the folder removes the boundary, permissive unchecked; `28` policy changes affect public reads immediately.
- Interactions: `MF-30` `includeFoldersAsPrefixes` with empty folders across pages; `31` disabling uniform access; `32` bucket delete with folders and restore; `33` `gcloud storage rm -r gs://B` succeeds (update the note in `examples/gcloud/README.md`); `34` restart, memory backend and migration; `35` concurrent insert and atomic metageneration condition; `36` object `a/` and folder `a/` coexist.
- Clients: gcloud 591 `managed-folders create`, `list`, `describe`, `add-iam-policy-binding`, `get-iam-policy` and `rm -r`; Go `raw.NewService` with `option.WithEndpoint(endpoint+"/storage/v1/")`. gcloud does not send `allowNonEmpty`. Go Storage Control needs a later slice, and its REST client has an empty managed-folder path in v1.70.0.

## Ledger and gaps

- `JSON-MANAGEDFOLDERS-001/002/004/005/007`: `IMPLEMENTED` with evidence. `003/006`: `PARTIAL` without Conditions. `008` stays `NOT_STARTED` and accepting `rapidCacheConfig` would be a false success.
- `BEHAVIOR-OBJECT-020` managed-folder portion, `EXT-BEH-007` to `PARTIAL`, `sources.md`, `docs/en/slices/json-object-list.md:31`, `grpc-v2-sdk-parity.md:48`, generated matrices.
- Reconcile first: `SPEC-006`, `SPEC-IAM-WIRE`, `SPEC-UBLA-EDGES`, `SPEC-BUCKET-SOFT-DELETE`. Add gaps for unresolved items.

## Overlap and order

`src/server/unsupported.rs`, `src/storage/catalog.rs:149`, `schema.rs`, the u64 permission change in `src/auth/mod.rs`, the ledger, `objects/list.rs`, `bucket_update.rs`, `buckets.rs` and object-operation authorization also change in folders and Cloud KMS work. Implement managed folders before hierarchical folders, which must handle their required managed-folder pairing and linked delete and rename.
