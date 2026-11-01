<!-- markdownlint-configure-file {"MD013": false} -->

# Differential comparison report

- Mode: replay of a recorded reference
- Generated: 2026-11-01 (recording of 2026-11-01)
- Requests sent to the live reference service (when recorded): 48

## Summary

| Case | Result | Differing fields |
| --- | --- | --- |
| bucket-create-invalid | differs | 4 |
| bucket-get-missing | differs | 3 |
| bucket-get | differs | 3 |
| bucket-lifecycle | differs | 20 |
| bucket-list | differs | 2 |
| bucket-patch-labels | differs | 18 |
| bucket-preconditions | differs | 10 |
| object-delete | differs | 6 |
| object-get-download | differs | 3 |
| object-media-upload | differs | 2 |
| object-missing | differs | 3 |
| object-patch | differs | 5 |
| object-preconditions | differs | 20 |

## Details

### bucket-create-invalid

Create a bucket with an invalid name; no bucket results.

- step `create-invalid-name` (`POST /storage/v1/b?project=<project>`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - body `$.error.errors[0].message`: reference `"Invalid bucket name: 'Invalid_Name'"`, mimic `"Invalid bucket name"`
  - body `$.error.errors[0].reason`: reference `"invalid"`, mimic `"invalidArgument"`
  - body `$.error.message`: reference `"Invalid bucket name: 'Invalid_Name'"`, mimic `"Invalid bucket name"`

### bucket-get-missing

Get a bucket that does not exist and check the error body shape.

- step `get-missing` (`GET /storage/v1/b/<missing_bucket>`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - body `$.error.errors[0].message`: reference `"The specified bucket does not exist."`, mimic `"Bucket not found"`
  - body `$.error.message`: reference `"The specified bucket does not exist."`, mimic `"Bucket not found"`

### bucket-get

Get a bucket, with and without a fields selector.

- step `get` (`GET /storage/v1/b/<bucket>`)
  - header `expires`: reference `<time>`, mimic `(absent)`
  - body `$.softDeletePolicy.effectiveTime`: reference `(absent)`, mimic `"<time>"`
- step `get-fields` (`GET /storage/v1/b/<bucket>?fields=name%2CstorageClass`)
  - header `expires`: reference `<time>`, mimic `(absent)`

### bucket-lifecycle

Create a bucket, create it again, get it, delete it twice and get it after deletion.

- step `create` (`POST /storage/v1/b?project=<project>`)
  - header `cache-control`: reference `no-cache, no-store, max-age=0, must-revalidate`, mimic `(absent)`
  - header `etag`: reference `<etag>`, mimic `(absent)`
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - header `pragma`: reference `no-cache`, mimic `(absent)`
  - body `$.softDeletePolicy.effectiveTime`: reference `(absent)`, mimic `"<time>"`
- step `create-again` (`POST /storage/v1/b?project=<project>`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - body `$.error.errors[0].message`: reference `"Your previous request to create the named bucket succeeded and you already own it."`, mimic `"Bucket already exists"`
  - body `$.error.message`: reference `"Your previous request to create the named bucket succeeded and you already own it."`, mimic `"Bucket already exists"`
- step `get` (`GET /storage/v1/b/<new_bucket>`)
  - header `expires`: reference `<time>`, mimic `(absent)`
  - body `$.softDeletePolicy.effectiveTime`: reference `(absent)`, mimic `"<time>"`
- step `delete` (`DELETE /storage/v1/b/<new_bucket>`)
  - header `cache-control`: reference `no-cache, no-store, max-age=0, must-revalidate`, mimic `(absent)`
  - header `content-type`: reference `application/json`, mimic `(absent)`
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - header `pragma`: reference `no-cache`, mimic `(absent)`
- step `delete-again` (`DELETE /storage/v1/b/<new_bucket>`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - body `$.error.errors[0].message`: reference `"The specified bucket does not exist."`, mimic `"Bucket not found"`
  - body `$.error.message`: reference `"The specified bucket does not exist."`, mimic `"Bucket not found"`
- step `get-after-delete` (`GET /storage/v1/b/<new_bucket>`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - body `$.error.errors[0].message`: reference `"The specified bucket does not exist."`, mimic `"Bucket not found"`
  - body `$.error.message`: reference `"The specified bucket does not exist."`, mimic `"Bucket not found"`

### bucket-list

List buckets of the project filtered by the harness bucket name prefix.

- step `list` (`GET /storage/v1/b?prefix=<bucket>&project=<project>`)
  - header `expires`: reference `<time>`, mimic `(absent)`
  - body `$.items[0].softDeletePolicy.effectiveTime`: reference `(absent)`, mimic `"<time>"`

### bucket-patch-labels

Patch bucket labels, remove one with null, and read the bucket back.

- step `add-label` (`PATCH /storage/v1/b/<bucket>`)
  - header `cache-control`: reference `no-cache, no-store, max-age=0, must-revalidate`, mimic `(absent)`
  - header `etag`: reference `<etag>`, mimic `(absent)`
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - header `pragma`: reference `no-cache`, mimic `(absent)`
  - body `$.acl`: reference `[{"bucket":"<bucket>","entity":"project-owners-<project-number>","etag":"<etag>","id":"<bucket>/project-owners-<project-number>","kind":"storage#bucketAccess...`, mimic `(absent)`
  - body `$.defaultObjectAcl`: reference `[{"entity":"project-owners-<project-number>","etag":"<etag>","kind":"storage#objectAccessControl","projectTeam":{"projectNumber":"<project-number>","team":"o...`, mimic `(absent)`
  - body `$.owner`: reference `{"entity":"project-owners-<project-number>"}`, mimic `(absent)`
  - body `$.softDeletePolicy.effectiveTime`: reference `(absent)`, mimic `"<time>"`
- step `remove-label` (`PATCH /storage/v1/b/<bucket>?ifMetagenerationMatch=2`)
  - header `cache-control`: reference `no-cache, no-store, max-age=0, must-revalidate`, mimic `(absent)`
  - header `etag`: reference `<etag>`, mimic `(absent)`
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - header `pragma`: reference `no-cache`, mimic `(absent)`
  - body `$.acl`: reference `[{"bucket":"<bucket>","entity":"project-owners-<project-number>","etag":"<etag>","id":"<bucket>/project-owners-<project-number>","kind":"storage#bucketAccess...`, mimic `(absent)`
  - body `$.defaultObjectAcl`: reference `[{"entity":"project-owners-<project-number>","etag":"<etag>","kind":"storage#objectAccessControl","projectTeam":{"projectNumber":"<project-number>","team":"o...`, mimic `(absent)`
  - body `$.owner`: reference `{"entity":"project-owners-<project-number>"}`, mimic `(absent)`
  - body `$.softDeletePolicy.effectiveTime`: reference `(absent)`, mimic `"<time>"`
- step `get` (`GET /storage/v1/b/<bucket>`)
  - header `expires`: reference `<time>`, mimic `(absent)`
  - body `$.softDeletePolicy.effectiveTime`: reference `(absent)`, mimic `"<time>"`

### bucket-preconditions

Metageneration preconditions on bucket get and patch: 412 on mismatch, 304 on not-match with the current value.

- step `get` (`GET /storage/v1/b/<bucket>?fields=name%2Cmetageneration`)
  - header `expires`: reference `<time>`, mimic `(absent)`
- step `patch-match-mismatch` (`PATCH /storage/v1/b/<bucket>?ifMetagenerationMatch=999`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `patch-not-match-current` (`PATCH /storage/v1/b/<bucket>?ifMetagenerationNotMatch=3`)
  - header `cache-control`: reference `no-cache, no-store, max-age=0, must-revalidate`, mimic `(absent)`
  - header `content-type`: reference `application/json`, mimic `(absent)`
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - header `pragma`: reference `no-cache`, mimic `(absent)`
- step `get-not-match-current` (`GET /storage/v1/b/<bucket>?ifMetagenerationNotMatch=3`)
  - header `cache-control`: reference `private, max-age=0, must-revalidate, no-transform`, mimic `(absent)`
  - header `content-type`: reference `application/json`, mimic `(absent)`
  - header `expires`: reference `<time>`, mimic `(absent)`
- step `get-match-current` (`GET /storage/v1/b/<bucket>?fields=name&ifMetagenerationMatch=3`)
  - header `expires`: reference `<time>`, mimic `(absent)`

### object-delete

Delete an object, read it afterwards and delete it again.

- step `delete` (`DELETE /storage/v1/b/<bucket>/o/object-delete-1`)
  - header `cache-control`: reference `no-cache, no-store, max-age=0, must-revalidate`, mimic `(absent)`
  - header `content-type`: reference `application/json`, mimic `(absent)`
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - header `pragma`: reference `no-cache`, mimic `(absent)`
- step `get-after-delete` (`GET /storage/v1/b/<bucket>/o/object-delete-1`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `delete-again` (`DELETE /storage/v1/b/<bucket>/o/object-delete-1`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`

### object-get-download

Read object metadata, a fields selection and the media download.

- step `get-metadata` (`GET /storage/v1/b/<bucket>/o/object-get-download-1`)
  - header `expires`: reference `<time>`, mimic `(absent)`
- step `get-fields` (`GET /storage/v1/b/<bucket>/o/object-get-download-1?fields=name%2Csize%2CcontentType`)
  - header `expires`: reference `<time>`, mimic `(absent)`
- step `download` (`GET /storage/v1/b/<bucket>/o/object-get-download-1?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`

### object-media-upload

Simple media upload and the resulting resource.

- step `upload` (`POST /upload/storage/v1/b/<bucket>/o?name=object-media-upload-1&uploadType=media`)
  - header `etag`: reference `<etag>`, mimic `(absent)`
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`

### object-missing

Metadata read, media download and delete of an object that does not exist.

- step `get-metadata` (`GET /storage/v1/b/<bucket>/o/object-missing-1`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `download` (`GET /storage/v1/b/<bucket>/o/object-missing-1?alt=media`)
  - header `expires`: reference `<time>`, mimic `(absent)`
- step `delete` (`DELETE /storage/v1/b/<bucket>/o/object-missing-1`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`

### object-patch

Patch content type and custom metadata, then read the object back.

- step `patch` (`PATCH /storage/v1/b/<bucket>/o/object-patch-1`)
  - header `cache-control`: reference `no-cache, no-store, max-age=0, must-revalidate`, mimic `(absent)`
  - header `etag`: reference `<etag>`, mimic `(absent)`
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - header `pragma`: reference `no-cache`, mimic `(absent)`
- step `get` (`GET /storage/v1/b/<bucket>/o/object-patch-1`)
  - header `expires`: reference `<time>`, mimic `(absent)`

### object-preconditions

Generation preconditions on upload, get and delete.

- step `upload-if-absent` (`POST /upload/storage/v1/b/<bucket>/o?ifGenerationMatch=0&name=object-preconditions-1&uploadType=media`)
  - header `etag`: reference `<etag>`, mimic `(absent)`
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `upload-if-absent-again` (`POST /upload/storage/v1/b/<bucket>/o?ifGenerationMatch=0&name=object-preconditions-1&uploadType=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - body `$.error.errors[0].location`: reference `"If-Match"`, mimic `(absent)`
  - body `$.error.errors[0].locationType`: reference `"header"`, mimic `(absent)`
  - body `$.error.errors[0].message`: reference `"At least one of the pre-conditions you specified did not hold."`, mimic `"Precondition failed"`
  - body `$.error.message`: reference `"At least one of the pre-conditions you specified did not hold."`, mimic `"Precondition failed"`
- step `get-match-mismatch` (`GET /storage/v1/b/<bucket>/o/object-preconditions-1?ifGenerationMatch=1`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - body `$.error.errors[0].location`: reference `"If-Match"`, mimic `(absent)`
  - body `$.error.errors[0].locationType`: reference `"header"`, mimic `(absent)`
  - body `$.error.errors[0].message`: reference `"At least one of the pre-conditions you specified did not hold."`, mimic `"Precondition failed"`
  - body `$.error.message`: reference `"At least one of the pre-conditions you specified did not hold."`, mimic `"Precondition failed"`
- step `get-not-match-current` (`GET /storage/v1/b/<bucket>/o/object-preconditions-1?ifGenerationNotMatch=<gen>`)
  - header `cache-control`: reference `private, max-age=0, must-revalidate, no-transform`, mimic `(absent)`
  - header `content-type`: reference `application/json`, mimic `(absent)`
  - header `expires`: reference `<time>`, mimic `(absent)`
- step `delete-match-mismatch` (`DELETE /storage/v1/b/<bucket>/o/object-preconditions-1?ifGenerationMatch=1`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - body `$.error.errors[0].location`: reference `"If-Match"`, mimic `(absent)`
  - body `$.error.errors[0].locationType`: reference `"header"`, mimic `(absent)`
  - body `$.error.errors[0].message`: reference `"At least one of the pre-conditions you specified did not hold."`, mimic `"Precondition failed"`
  - body `$.error.message`: reference `"At least one of the pre-conditions you specified did not hold."`, mimic `"Precondition failed"`

## Cleanup

- mimic shared bucket: emptied (4 object versions), deleted, GET confirmed 404
- mimic lifecycle bucket: already absent (404)
- reference (recorded live run, 48 requests): the shared bucket was emptied (4 object versions) and deleted, and a GET confirmed 404; the lifecycle bucket was already deleted by its case and answered 404.
