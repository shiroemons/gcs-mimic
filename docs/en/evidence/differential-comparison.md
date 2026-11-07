<!-- markdownlint-configure-file {"MD013": false} -->

# Differential comparison report

- Mode: replay of a recorded reference
- Generated: 2026-11-07 (recordings of 2026-11-01 and 2026-11-02 and 2026-11-04)
- Requests sent to the live reference service (when recorded): 855
- HTTP 429 responses retried by the live run: 0

## Summary

| Case | Result | Differing fields |
| --- | --- | --- |
| bkt-acl-crud | match | 0 |
| bkt-create-errors | match | 0 |
| bkt-create-full | match | 0 |
| bkt-create-lifecycle-array | match | 0 |
| bkt-create-predefined-acl-ubla | match | 0 |
| bkt-create-unknown-field | match | 0 |
| bkt-create-unknown-query | match | 0 |
| bkt-iam-pap-ubla | match | 0 |
| bkt-notifications | match | 0 |
| bkt-patch-errors | match | 0 |
| bkt-patch-put-edges | match | 0 |
| bkt-project-resources | match | 0 |
| bkt-soft-delete-any-retention | match | 0 |
| bucket-create-invalid | match | 0 |
| bucket-get-missing | match | 0 |
| bucket-get | match | 0 |
| bucket-lifecycle | match | 0 |
| bucket-list | match | 0 |
| bucket-patch-labels | match | 0 |
| bucket-preconditions | match | 0 |
| grpc-bucket-get | match | 0 |
| grpc-bucket-list | match | 0 |
| grpc-errors | match | 0 |
| grpc-object-lifecycle | match | 0 |
| grpc-object-list | match | 0 |
| grpc-object-preconditions | match | 0 |
| grpc-read-mask | match | 0 |
| obj-compose | match | 0 |
| obj-copy | match | 0 |
| obj-download-gzip | match | 0 |
| obj-download-range | match | 0 |
| obj-download-validators | match | 0 |
| obj-errors | match | 0 |
| obj-get-fields | match | 0 |
| obj-holds-customtime | match | 0 |
| obj-list | match | 0 |
| obj-metadata-write | match | 0 |
| obj-move | match | 0 |
| obj-resumable | match | 0 |
| obj-retention-disabled-bucket | match | 0 |
| obj-retention-unlocked | match | 0 |
| obj-rewrite | match | 0 |
| obj-upload-checksums | match | 0 |
| obj-upload-content-type | match | 0 |
| obj-upload-multipart | match | 0 |
| obj-versioning | match | 0 |
| object-delete | match | 0 |
| object-get-download | match | 0 |
| object-media-upload | match | 0 |
| object-missing | match | 0 |
| object-patch | match | 0 |
| object-preconditions | match | 0 |
| xml-content-length-range | match | 0 |
| xml-copy | match | 0 |
| xml-errors | match | 0 |
| xml-list-bucket | match | 0 |
| xml-metadata | match | 0 |
| xml-object-crud | match | 0 |
| xml-preconditions | match | 0 |
| xml-put-edges | match | 0 |
| xml-range | match | 0 |

## Details

No differences or errors apart from any local choices listed below.

### bkt-acl-crud

Bucket, default object and object access control lists on a fresh bucket with uniform access disabled: list, get, patch, update, delete and insert of the project-viewers entity (never a public entity), plus invalid role and invalid entity errors. The three *-get-missing steps request project-owners-1, a project-team entity that names another project, so they record the 400 `Project "1" was not found.` answer rather than a missing entry in the same project; object-acl-get-missing-object records the missing object.

### bkt-create-errors

Bucket creation inputs that must be rejected: unknown location, storage class, retention and soft delete values, short name, missing project and malformed JSON. Probes that may succeed on one side are separate cases (bkt-create-*) so no later step meets a bucket an earlier one created.

- step `soft-delete-below-minimum` (`POST /storage/v1/b?project=<project>`)
  - local choice: gcs-mimic accepts any retention from 1 second, which Cloud Storage refuses; see docs/en/releases/0.12.0.md.

### bkt-create-full

Create a bucket with an uppercase location, a non-default storage class and every common configuration block; read it back with projection=full and field selectors, then clear retention and soft delete and delete it. Checks location normalization, input echo, default ACL output and header parity.

### bkt-create-lifecycle-array

Bucket creation with lifecycle as an array instead of an object (SPEC-LIFECYCLE-ARRAY). The bucket is deleted afterwards in case a side accepts it.

### bkt-create-predefined-acl-ubla

Bucket creation with predefinedAcl together with uniform bucket-level access, which must be rejected. The bucket is deleted afterwards in case a side accepts it.

### bkt-create-unknown-field

Bucket creation with a body field that does not exist. The bucket is deleted afterwards because the request may succeed.

### bkt-create-unknown-query

Bucket creation with an undeclared query parameter. The bucket is deleted afterwards in case a side accepts it.

### bkt-iam-pap-ubla

Bucket IAM policy read, set and testPermissions (project principals only, never allUsers or allAuthenticatedUsers), then uniform bucket-level access and public access prevention toggles with their invalid inputs and aliases. Public access prevention stays enforced throughout (the harness refuses any relaxation), so only the enforce toggle and invalid values are measured.

### bkt-notifications

Notification configurations on a bucket of its own without a Pub/Sub topic: empty list, creation naming a topic that does not exist, invalid payload formats, and get and delete of an id that does not exist. The topic is never created, so no notification is expected to persist.

### bkt-patch-errors

Bucket PATCH and PUT inputs that must be rejected, and the same operations on a missing bucket. The case has a bucket of its own, because a PUT that succeeds replaces omitted fields with defaults.

### bkt-patch-put-edges

PATCH and PUT edge inputs on a fresh multi-region bucket created with a lowercase location: empty body, unknown and output-only fields, null and empty values per field, retention and soft delete set and cleared (never locked), projection=full, fields, predefined ACLs (non-public), and PUT omission semantics.

### bkt-project-resources

Project-level resources that need no setup: the project service account, and HMAC key lookups for keys that do not exist. No HMAC key is created, so nothing needs cleaning up. The service account email carries the project number and is not compared.

### bkt-soft-delete-any-retention

A bucket PATCH with a soft delete retention below the Cloud Storage minimum, on a bucket of its own because gcs-mimic applies it: the deliberate difference must not reach other steps. bkt-create-errors holds the same value on bucket creation.

- step `patch-soft-delete-below-minimum` (`PATCH /storage/v1/b/<bucket>`)
  - local choice: gcs-mimic accepts any retention from 1 second, which Cloud Storage refuses; see docs/en/releases/0.12.0.md.

### bucket-create-invalid

Create a bucket with an invalid name; no bucket results.

### bucket-get-missing

Get a bucket that does not exist and check the error body shape.

### bucket-get

Get a bucket, with and without a fields selector.

### bucket-lifecycle

Create a bucket, create it again, get it, delete it twice and get it after deletion.

### bucket-list

List buckets of the project filtered by the harness bucket name prefix.

### bucket-patch-labels

Patch bucket labels, remove one with null, and read the bucket back.

### bucket-preconditions

Metageneration preconditions on bucket get and patch: 412 on mismatch, 304 on not-match with the current value.

### grpc-bucket-get

gRPC GetBucket: a stable subset of the resource, a read mask, a missing bucket, an invalid name and a failed metageneration precondition.

### grpc-bucket-list

gRPC ListBuckets by parent project: prefix filter, page size, read mask, an empty result and a malformed parent.

### grpc-errors

gRPC statuses and messages for missing buckets and objects and malformed requests across the object RPCs.

### grpc-object-lifecycle

gRPC WriteObject, GetObject, ReadObject (whole and ranged), UpdateObject, DeleteObject and the not-found statuses afterwards.

### grpc-object-list

gRPC ListObjects with prefix, delimiter, read mask, paging by page_token and versions.

### grpc-object-preconditions

gRPC generation and metageneration preconditions on WriteObject, GetObject, UpdateObject and DeleteObject, and their FAILED_PRECONDITION statuses.

### grpc-read-mask

gRPC read masks that name checksum sub-paths: GetObject, ReadObject metadata and ListObjects with checksums.crc32c, checksums.md5_hash and the whole message.

### obj-compose

Compose: destination metadata, absent destination, empty and missing sources, source generation and object preconditions, a composite as source, componentCount and deleteSourceObjects. Checks SPEC-COMPOSE-EDGES.

### obj-copy

Copy: metadata inheritance for an empty, null and empty-object body, metadata override, storage class change, copy onto itself, source and destination preconditions and missing source. Checks SPEC-DERIVED-METADATA.

### obj-download-gzip

An object stored with contentEncoding gzip, downloaded with and without Accept-Encoding gzip. The body is a valid gzip stream of the text 'hello hello hello', so the downloads measure real transcoding and range handling as well as headers.

### obj-download-range

Range downloads: closed, suffix, open-ended, clamped, unsatisfiable, malformed, multiple and non-bytes ranges, If-Range, and the same on an empty object. Checks SPEC-RANGE-FALLBACK and SPEC-RANGE-VALIDATORS.

### obj-download-validators

Download response headers and conditional request headers (If-Match, If-None-Match, If-Modified-Since, If-Unmodified-Since) on media downloads and metadata reads. Checks SPEC-JSON-ETAG-WIRE.

### obj-errors

Error bodies and headers of object requests: invalid parameters, bad alt, malformed JSON, missing bucket, unsupported methods and a trailing slash.

### obj-get-fields

Object metadata reads with projection values and many fields selections: nested, wildcard, repeated, invalid and unknown selectors. Checks SPEC-FIELDS-EDGES.

### obj-holds-customtime

Temporary and event-based holds block delete and overwrite; PUT omission, explicit null and false; customTime set, backdate, clear and PUT omission. Both holds are released and the object deleted at the end. Checks SPEC-OBJECT-HOLDS-CUSTOMTIME and SPEC-EVENT-HOLD-WIRE.

### obj-list

Object listing: prefix, delimiter, includeTrailingDelimiter, paging with maxResults and pageToken, zero and invalid maxResults, matchGlob (valid, empty, invalid), startOffset and endOffset, fields, versions and an unmatched prefix. Checks SPEC-LIST-PAGING, SPEC-LIST-GLOB-EDGES and SPEC-LIST-RESOURCE-SHAPE.

### obj-metadata-write

PATCH and PUT of object metadata: custom metadata, null removal of a key and of the whole map, null content fields, empty bodies, unknown fields, wrong types and metageneration preconditions. Checks SPEC-015.

### obj-move

Move (moveTo): the source disappears and the destination keeps content and metadata; missing source, move onto itself and generation preconditions.

### obj-resumable

Resumable completion and replay, a small non-final chunk on an isolated session, status and cancellation on a fresh untouched session, and separate sessions for a mismatched total and malformed Content-Range. The 2026-11-02 live recording confirms that the 5-byte non-final chunk returns 400, cancellation and its subsequent status return a clientClosedRequest JSON envelope with 499, and a syntax error in Content-Range poisons its session so subsequent status also returns the same plain 400. start-precondition-fail answers 200 because the precondition is evaluated at completion. Contract: [Resumable upload guide](https://docs.cloud.google.com/storage/docs/performing-resumable-uploads). Checks SPEC-RSM-EDGES.

### obj-retention-disabled-bucket

Setting object retention on a bucket that has not enabled object retention. Checks SPEC-OBJECT-RETENTION.

### obj-retention-unlocked

Unlocked object retention in a disposable bucket created with enableObjectRetention: set, delete and shorten blocked, override, invalid mode and time, event-based hold conflict. Retention and holds are cleared before the object is deleted. Locked retention is never requested. Checks SPEC-OBJECT-RETENTION.

### obj-rewrite

Rewrite: a small object completes in one call, metadata body, storage class change, bad and unknown rewrite tokens, preconditions and missing source. Checks SPEC-REWRITE-EDGES and SPEC-DERIVED-METADATA.

### obj-upload-checksums

JSON media and multipart uploads with correct and wrong CRC32C and MD5 values (X-Goog-Hash, Content-MD5, metadata fields). Checks SPEC-010 and SPEC-016.

### obj-upload-content-type

Media upload without a Content-Type header, without a name, with contentEncoding in the query, an empty body, a name that needs percent-encoding and bad preconditions or uploadType values. Checks SPEC-011.

### obj-upload-multipart

Multipart upload: metadata fields, a content type that disagrees between the metadata and the media part (metadata-type-wins answers 400, so the object is never created and get-precedence answers 404; despite its name the step records the mismatch rejection, not a precedence), missing media part type, missing name, missing boundary, one part only and invalid metadata JSON. Checks SPEC-016 and SPEC-011.

### obj-versioning

With bucket versioning enabled for the duration of the case: generation-specific get and delete, combined generation and metageneration preconditions, noncurrent versions, listing with versions and copy from a source generation. Versioning is switched off again at the end. Checks SPEC-008 and SPEC-VERSION-DELETE-CONDITIONS.

### object-delete

Delete an object, read it afterwards and delete it again.

### object-get-download

Read object metadata, a fields selection and the media download.

### object-media-upload

Simple media upload and the resulting resource.

### object-missing

Metadata read, media download and delete of an object that does not exist.

### object-patch

Patch content type and custom metadata, then read the object back.

### object-preconditions

Generation preconditions on upload, get and delete.

### xml-content-length-range

XML PUT x-goog-content-length-range: inclusive bounds, the exact-size pair Rails Active Storage signs, bodies outside the range leaving no object, malformed values and the header on a copy.

### xml-copy

XML object copy through x-goog-copy-source: metadata kept and replaced, CopyObjectResult body, and the missing-source and failed-precondition errors.

### xml-errors

XML error bodies for a missing bucket and object, a PUT into a missing bucket, an unknown query parameter and an unknown x-goog header.

### xml-list-bucket

XML ListBucket with prefix, delimiter, max-keys, marker and versions; element order and truncation fields are compared.

### xml-metadata

x-goog-meta-* and the standard content headers stored by an XML PUT and returned by GET and HEAD, including a replacing PUT that drops them.

### xml-object-crud

XML API object PUT, GET, HEAD and DELETE with the default response headers, then the not-found errors after the delete.

### xml-preconditions

x-goog-if-generation-match and x-goog-if-metageneration-match on XML PUT and DELETE, with the XML PreconditionFailed error bodies.

### xml-put-edges

XML PUT checksum headers (valid and mismatched Content-MD5 and x-goog-hash), an empty body, and percent-encoded object names in paths and listings.

### xml-range

XML GET and HEAD with satisfiable, suffix, open-ended, multiple and unsatisfiable Range headers.

## Cleanup

```text
- mimic new bucket of bkt-acl-crud: already absent (404)
- mimic new bucket of bkt-create-errors: emptied (0 object versions), deleted, GET confirmed 404
- mimic new bucket of bkt-create-full: already absent (404)
- mimic new bucket of bkt-create-lifecycle-array: already absent (404)
- mimic new bucket of bkt-create-predefined-acl-ubla: already absent (404)
- mimic new bucket of bkt-create-unknown-field: already absent (404)
- mimic new bucket of bkt-create-unknown-query: already absent (404)
- mimic new bucket of bkt-iam-pap-ubla: already absent (404)
- mimic bucket of bkt-notifications: emptied (0 object versions), deleted, GET confirmed 404
- mimic bucket of bkt-patch-errors: emptied (0 object versions), deleted, GET confirmed 404
- mimic new bucket of bkt-patch-put-edges: already absent (404)
- mimic bucket of bkt-soft-delete-any-retention: emptied (0 object versions), deleted, GET confirmed 404
- mimic new bucket of bucket-lifecycle: already absent (404)
- mimic bucket of bucket-patch-labels: emptied (0 object versions), deleted, GET confirmed 404
- mimic bucket of bucket-preconditions: emptied (0 object versions), deleted, GET confirmed 404
- mimic new bucket of obj-retention-unlocked: already absent (404)
- mimic bucket of obj-versioning: emptied (1 object versions), deleted, GET confirmed 404
- mimic shared bucket: emptied (38 object versions), deleted, GET confirmed 404
- mimic sweep: 0 leftover bucket(s) found
```
