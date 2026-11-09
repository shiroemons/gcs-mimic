# Upload predefined object ACLs

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: the `predefinedAcl` query parameter of JSON `objects.insert` uploads
(`uploadType=media`, `multipart` and `resumable`), part of JSON-OBJECTS-007.
Contract checked on 2026-10-31; the feature ledger owns status. The row stays
PARTIAL.

## Contract

Sources (checked 2026-10-31):
[objects.insert](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/insert),
[Access control lists: predefined ACLs](https://docs.cloud.google.com/storage/docs/access-control/lists#predefined-acl)
and [Public access prevention](https://docs.cloud.google.com/storage/docs/public-access-prevention).

| Value | Resulting object ACL |
| --- | --- |
| `private` | object owner `OWNER` |
| `projectPrivate` | owner `OWNER`, `project-owners-N` `OWNER`, `project-editors-N` `OWNER`, `project-viewers-N` `READER` |
| `bucketOwnerRead` | owner `OWNER`, `project-owners-N` `READER` |
| `bucketOwnerFullControl` | owner `OWNER`, `project-owners-N` `OWNER` |
| `authenticatedRead` | owner `OWNER`, `allAuthenticatedUsers` `READER` |
| `publicRead` | owner `OWNER`, `allUsers` `READER` |

- The canned ACL replaces the bucket's default object ACL for the new
  generation. The owner entity is the existing ownership model's entity: the
  strict-mode uploader's `user-` entity, or `project-owners-N` for permissive and
  anonymous uploads. `N` is the rendered project number, as for every other ACL.
- The documented insert permissions are `storage.objects.create` (plus
  `storage.objects.delete` for overwrites); `predefinedAcl` adds no
  `setIamPolicy` requirement.
- Any value on a bucket with uniform bucket-level access fails with 400.
- Public values (`publicRead`, `authenticatedRead`) under enforced public access
  prevention fail with 412 `conditionNotMet`; the other values succeed.
- A resumable initiation's value is stored with the session and applied at
  completion.
- `publicReadWrite` is bucket-only; it and any other value fail with 400
  `invalidParameter` before authorization or body reads.
- The ACL row commits in the publication transaction. Uniform access and public
  access prevention are rechecked there, so a bucket change between initiation
  or the advisory check and publication cannot be bypassed.

## Real Cloud Storage observations

An opted-in differential run against Cloud Storage (fine-grained bucket with
public access prevention enforced) matched the table above for `private`,
`projectPrivate`, `bucketOwnerRead` and `bucketOwnerFullControl`, with entities
relative to the uploader and the project number. `authenticatedRead` and
`publicRead` returned 412 "The member bindings allUsers and allAuthenticatedUsers
are not allowed since public access prevention is enforced." A resumable
initiation with `bucketOwnerRead` produced the same ACL at completion as a media
upload. On a bucket with uniform bucket-level access, every value returned 400
"Cannot insert legacy ACL for an object when uniform bucket-level access is
enabled." Project, bucket and principal names are omitted.

## Local choices

- The uniform-access 400 uses reason `invalidArgument`; its message begins with
  the observed sentence and adds the remedy. The 412 keeps the shared public
  access prevention message.
- The documentation states that anonymous users cannot specify a predefined ACL
  on upload; strict anonymous uploads with `predefinedAcl` fail with 400
  `invalidArgument` before the body is read. The provider status is unverified.
- Resumable completion responses still omit ACL fields, as before; GET with
  `projection=full` shows the applied ACL.

## Not supported

These keep their explicit 400 responses: object PATCH/PUT `predefinedAcl`,
`destinationPredefinedAcl` on copy, rewrite and compose, bucket `predefinedAcl`
and `predefinedDefaultObjectAcl`, and gRPC `predefined_acl` fields.

## XML API `x-goog-acl`

`x-goog-acl` applies the same canned entries on XML PUT, resumable initiation,
multipart initiation and copy, under either copy metadata directive. Bucket PUT
keeps its 400. The values are the XML names `private`, `project-private`,
`bucket-owner-read`, `bucket-owner-full-control`, `public-read` and
`authenticated-read`; the JSON spellings are invalid. The following answers were
recorded from Cloud Storage on 2026-11-09:

- An invalid value is 400 `InvalidArgument` with the message `Invalid argument.`
  and `Details` of `Invalid canned acl: VALUE`.
- Any value under uniform bucket-level access is the same 400 whose `Details`
  begins `Cannot insert legacy ACL for an object when uniform bucket-level
  access is enabled.` A write without the header succeeds.

Uniform access is checked before any body is read and again in the publishing
transaction, so a session opened before the bucket changed fails at completion
with the same answer. Public access prevention and the anonymous rule follow the
JSON API. On a multipart upload the header belongs to initiation; a part or
completion request carrying it is refused.

Tests: `xml_canned_acl_values_produce_the_recorded_entries_on_every_write`,
`xml_canned_acl_refuses_json_spellings_and_unknown_values_without_writing`,
`xml_canned_acl_under_uniform_access_answers_the_recorded_error` and
`xml_canned_acl_is_rechecked_at_publication_with_the_recorded_error` in
`tests/it_xml_auth/acl_permission_effects/xml_canned.rs`.

## Verification

Tests in `tests/acl_permission_effects/predefined_upload.rs`, run on the disk
and memory backends where noted:

- `media_upload_predefined_acls_replace_the_default_object_acl`: every value's
  ACL in the insert response and on GET, replacement of a stored default object
  ACL, effective anonymous/authenticated reads, and invalid values.
- `multipart_and_resumable_uploads_apply_predefined_acl_at_publication`:
  multipart, a resumable session completed after a default ACL change and a
  restart, and a uniform-access recheck at completion and on media upload.
- `predefined_acl_uploads_need_only_create_and_respect_access_settings`: a
  bucket `WRITER` without `setIamPolicy`, anonymous rejection, public access
  prevention at upload and at resumable completion, and an injected ACL write
  failure that publishes nothing.

Unit tests: `acl::tests::predefined_object_acls_round_trip_and_exclude_bucket_only_values`
and `json::object_tests::obj_route_001_media_upload_metadata_download_delete`.
The three integration tests failed before the change (400 `badRequest` on every
upload) and pass after it. `xml::uniform_access_metadata_and_predefined_acl_rejections`
keeps the uniform-access rejections and the remaining non-upload rejections.

The .NET SDK scorecard case `sdk.upload_predefined_acl`
(`UploadObjectOptions.PredefinedAcl = PublicRead`, a resumable upload, then
`projection=full` GET expecting `allUsers` `READER`) passed against a release
build of this change over HTTPS.
