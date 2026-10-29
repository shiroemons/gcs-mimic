# JSON access control lists

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: JSON-BUCKETACCESSCONTROLS-001..006, JSON-DEFAULTOBJECTACCESSCONTROLS-001..006,
JSON-OBJECTACCESSCONTROLS-001..006, the ACL part of BEHAVIOR-COMMON-005,
DESIGN-018 and DESIGN-020. Contract checked on 2026-10-29; the feature ledger
owns status. Every row stays PARTIAL: ACL entries are stored, rendered and
managed, but they do not grant access (DESIGN-017).

## Acceptance contract

- `GET|POST b/{bucket}/acl`, `GET|PATCH|PUT|DELETE b/{bucket}/acl/{entity}` and the
  same shapes under `defaultObjectAcl` and `o/{object}/acl` list, get, insert,
  patch, update and delete one entity. Object routes accept `generation`; the
  default object ACL list accepts bucket `ifMetagenerationMatch` (412) and
  `ifMetagenerationNotMatch` (304).
- A bucket, default object ACL or object generation without stored entries has
  the `projectPrivate` canned ACL of the owning project:
  `project-owners-N OWNER`, `project-editors-N OWNER`, `project-viewers-N READER`.
  `N` is the bucket's rendered `projectNumber`: a numeric local project ID, or `0`.
- Entities follow the pinned Discovery forms: `user-`/`group-` email or ID,
  `domain-`, `project-{owners,editors,viewers}-ID`, `allUsers`,
  `allAuthenticatedUsers`. Roles are `OWNER`, `WRITER`, `READER` for bucket ACLs
  and `OWNER`, `READER` for default object and object ACLs.
- Insert, update and patch upsert the entity: Go `ACL().Set` sends update for a
  new entity and Ruby `add_reader` sends insert. Patch without `role` keeps an
  existing role and fails with 400 `required` for a new entity. A body entity
  that differs from the path entity fails with 400.
- Each change advances the containing bucket or object metageneration and
  `updated`, inside the same transaction, and journals the change. At most 100
  entries are stored per resource; the 101st new entity fails with 400 `invalid`.
- Resources use the Discovery fields: `kind`, `entity`, `role`, `etag` and the
  derived `email`, `entityId`, `domain` or `projectTeam`. Bucket entries add
  `id` (`bucket/entity`), `selfLink` and `bucket`. Object entries add `id`
  (`bucket/object/generation/entity`), `selfLink`, `bucket`, `object` and
  `generation`. Default object entries describe future objects and name no
  resource. `etag` encodes generation (objects) and metageneration as the
  service's protobuf-style base64, for example `CAE=` for metageneration 1.
- New object generations copy a stored default object ACL in the publishing
  transaction (media, multipart, resumable, copy, compose, rewrite, move,
  restore, XML PUT and administrative publication). Later default changes never
  rewrite existing objects; a replacement generation receives the current
  default, not the replaced object's ACL. ACL rows are deleted with their bucket
  or object generation.
- `projection=full`, including the PUT default, adds bucket `acl`,
  `defaultObjectAcl` and `owner` (`project-owners-N`) and object `acl` to bucket
  get/list/insert/patch/update and object get/list/insert/patch/update/copy/
  compose/move/restore responses. Strict callers without
  `storage.buckets.getIamPolicy` or `storage.objects.getIamPolicy` receive the
  resource without ACL fields.
- Bucket PATCH/PUT `acl` and `defaultObjectAcl`, and object PATCH/PUT `acl`,
  replace the entire supplied array. An empty array clears the ACL; omission
  preserves it for both methods. Output-only fields on entries are ignored.
  Null, non-array input, invalid entities/roles, duplicate entities and arrays
  over 100 entries fail without mutation. Bucket roles include `WRITER`;
  default object and object roles do not.
- Resource ACL replacement requires both the metadata `update` permission and
  the resource's `setIamPolicy` permission. As an existing local accommodation,
  a caller without `setIamPolicy` but with `getIamPolicy` can echo the current
  entity/role set in any order; the comparison and metadata update share the
  transaction, and the ACL stays unchanged. Changed arrays still require `setIamPolicy`.
  An update-only caller receives the same 403 for unchanged and changed arrays;
  no comparison exposes the stored ACL. `getIamPolicy` still controls response
  projection independently.
- ACL arrays and other supplied metadata commit in one catalog transaction,
  with one metageneration increment and journal entry. Object generation and
  generation/metageneration conditions select and constrain that same update.
  Invalid metadata, permissions, preconditions, UBLA or PAP roll back all changes.
  A combined bucket policy/ACL update checks its resulting UBLA/PAP settings.
  PUT retains its existing metadata-map/content replacement behavior; ACL
  omission does not reset stored entries.
- With uniform bucket-level access enabled every ACL method returns 400 `invalid`
  and full projection omits `acl`/`defaultObjectAcl` (bucket `owner` remains).
  Stored entries survive and reappear when uniform access is disabled.
- With public access prevention in effect, adding `allUsers` or
  `allAuthenticatedUsers` fails with 412 `conditionNotMet`, as IAM policy
  submissions do.
- Strict mode requires `storage.buckets.getIamPolicy`/`setIamPolicy` for bucket
  and default object ACLs and `storage.objects.getIamPolicy`/`setIamPolicy` for
  object ACLs; anonymous callers without a grant receive 401.
- Schema 33 adds `acl_bucket_entries` and `acl_object_entries`; existing buckets
  and objects keep the virtual `projectPrivate` ACL.

## Sources and local choices

- [Access control lists](https://docs.cloud.google.com/storage/docs/access-control/lists)
- [BucketAccessControls](https://docs.cloud.google.com/storage/docs/json_api/v1/bucketAccessControls),
  [DefaultObjectAccessControls](https://docs.cloud.google.com/storage/docs/json_api/v1/defaultObjectAccessControls)
  and [ObjectAccessControls](https://docs.cloud.google.com/storage/docs/json_api/v1/objectAccessControls)
- [Uniform bucket-level access](https://docs.cloud.google.com/storage/docs/uniform-bucket-level-access)
- [Public access prevention](https://docs.cloud.google.com/storage/docs/public-access-prevention)
- [Bucket PATCH](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/patch),
  [Object PATCH](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/patch)
  and [JSON PATCH semantics](https://docs.cloud.google.com/storage/docs/json_api#patch)
  (checked 2026-10-29): arrays replace, metadata update and ACL set permissions
  are both required, and ACL read permission governs the response.
- Pinned Discovery schemas and methods in `compatibility/upstream/json-schemas.json`
  and `compatibility/upstream/json-methods.json` (revision `20261020`).

Local choices without provider evidence: upsert semantics of insert/update/patch
for a new entity (derived from Go and Ruby SDK usage), error reasons and
messages for uniform access, missing entities and the entry limit, the project
number `0` for non-numeric local project IDs, accepting any syntactically valid
email without account lookup and keeping entity case as sent, omitting
`id`/`selfLink` from default object entries, omitting ACL fields instead of
failing when a strict caller lacks ACL read permission, rejecting duplicate
entities, and null ACL rejection. Resource ACL replacements use 400
`invalidArgument` under UBLA (legacy bucket profile: `invalid`); dedicated ACL
routes retain 400 `invalid`. Exact mixed-error precedence is unverified.

## Limitations

- ACL entries never grant or deny access; authorization remains IAM-only
  (DESIGN-017 stays NOT_STARTED).
- The uploader's `user-` OWNER entry and the object `owner` field are not
  modeled.
- `predefinedAcl`, `predefinedDefaultObjectAcl` and `destinationPredefinedAcl`
  keep their existing responses (DESIGN-019); `acl` in insert/copy/rewrite
  bodies remain unsupported.
- Resumable-upload completion and rewrite responses do not render ACL fields;
  restore and move do not offer `copySourceAcl` and use the default object ACL.
- `userProject` keeps the shared unsupported response.
