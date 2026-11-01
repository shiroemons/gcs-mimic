<!-- markdownlint-configure-file {"MD013": false} -->

# Differential comparison report

- Mode: replay of a recorded reference
- Generated: 2026-11-01 (recording of 2026-11-01)
- Requests sent to the live reference service (when recorded): 781
- HTTP 429 responses retried by the live run: 0
- Progress: 186 differing fields remain, down from 2989 in the baseline measurement of 2026-11-01.

## Summary

| Case | Result | Differing fields |
| --- | --- | --- |
| bkt-acl-crud | differs | 16 |
| bkt-create-errors | match | 0 |
| bkt-create-full | match | 0 |
| bkt-create-lifecycle-array | match | 0 |
| bkt-create-predefined-acl-ubla | match | 0 |
| bkt-create-unknown-field | match | 0 |
| bkt-create-unknown-query | match | 0 |
| bkt-iam-pap-ubla | match | 0 |
| bkt-notifications | differs | 12 |
| bkt-patch-errors | match | 0 |
| bkt-patch-put-edges | differs | 2 |
| bkt-project-resources | match | 0 |
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
| grpc-object-lifecycle | differs | 4 |
| grpc-object-list | differs | 6 |
| grpc-object-preconditions | differs | 2 |
| grpc-read-mask | differs | 2 |
| obj-compose | differs | 3 |
| obj-copy | differs | 6 |
| obj-download-gzip | differs | 8 |
| obj-download-range | differs | 14 |
| obj-download-validators | differs | 9 |
| obj-errors | differs | 2 |
| obj-get-fields | differs | 6 |
| obj-holds-customtime | differs | 20 |
| obj-list | differs | 2 |
| obj-metadata-write | differs | 4 |
| obj-move | differs | 3 |
| obj-resumable | differs | 38 |
| obj-retention-disabled-bucket | match | 0 |
| obj-retention-unlocked | match | 0 |
| obj-rewrite | differs | 4 |
| obj-upload-checksums | differs | 1 |
| obj-upload-content-type | differs | 8 |
| obj-upload-multipart | differs | 10 |
| obj-versioning | differs | 2 |
| object-delete | match | 0 |
| object-get-download | differs | 1 |
| object-media-upload | match | 0 |
| object-missing | differs | 1 |
| object-patch | match | 0 |
| object-preconditions | match | 0 |
| xml-copy | match | 0 |
| xml-errors | match | 0 |
| xml-list-bucket | match | 0 |
| xml-metadata | match | 0 |
| xml-object-crud | match | 0 |
| xml-preconditions | match | 0 |
| xml-put-edges | match | 0 |
| xml-range | match | 0 |

## Details

### bkt-acl-crud

Bucket, default object and object access control lists on a fresh bucket with uniform access disabled: list, get, patch, update, delete and insert of the project-viewers entity (never a public entity), plus invalid role, invalid entity and missing entry errors.

- step `bucket-acl-list` (`GET /storage/v1/b/<new_bucket>/acl`)
  - header `etag`: reference `<etag:bare>`, mimic `(absent)`
- step `bucket-acl-delete` (`DELETE /storage/v1/b/<new_bucket>/acl/project-viewers-<project-number>`)
  - header `etag`: reference `<etag:bare>`, mimic `(absent)`
- step `bucket-acl-list-after` (`GET /storage/v1/b/<new_bucket>/acl`)
  - header `etag`: reference `<etag:bare>`, mimic `(absent)`
- step `default-acl-list` (`GET /storage/v1/b/<new_bucket>/defaultObjectAcl`)
  - header `etag`: reference `<etag:bare>`, mimic `(absent)`
- step `default-acl-delete` (`DELETE /storage/v1/b/<new_bucket>/defaultObjectAcl/project-viewers-<project-number>`)
  - header `etag`: reference `<etag:bare>`, mimic `(absent)`
- step `object-acl-list` (`GET /storage/v1/b/<new_bucket>/o/bkt-acl-crud-1/acl`)
  - header `etag`: reference `<etag:bare>`, mimic `(absent)`
  - body `$.items[3]`: reference `{"bucket":"<new_bucket>","email":"<email>","entity":"user-<email>","etag":"<etag:str>","generation":"<generation:str:16>","id":"<new_bucket>/bkt-acl-crud-1/<...`, mimic `(absent)`
- step `object-acl-delete` (`DELETE /storage/v1/b/<new_bucket>/o/bkt-acl-crud-1/acl/project-viewers-<project-number>`)
  - header `etag`: reference `<etag:bare>`, mimic `(absent)`
- step `object-acl-list-after` (`GET /storage/v1/b/<new_bucket>/o/bkt-acl-crud-1/acl`)
  - header `etag`: reference `<etag:bare>`, mimic `(absent)`
  - body `$.items[2].email`: reference `"<email>"`, mimic `(absent)`
  - body `$.items[2].entity`: reference `"user-<email>"`, mimic `"project-viewers-<project-number>"`
  - body `$.items[2].id`: reference `"<new_bucket>/bkt-acl-crud-1/<gen>/user-<email>"`, mimic `"<new_bucket>/bkt-acl-crud-1/<gen>/project-viewers-<project-number>"`
  - body `$.items[2].projectTeam`: reference `(absent)`, mimic `{"projectNumber":"<project-number:str>","team":"viewers"}`
  - body `$.items[2].role`: reference `"OWNER"`, mimic `"READER"`
  - body `$.items[2].selfLink`: reference `"<www-host>/storage/v1/b/<new_bucket>/o/bkt-acl-crud-1/acl/user-<email>"`, mimic `"<host>/storage/v1/b/<new_bucket>/o/bkt-acl-crud-1/acl/project-viewers-<project-number>"`
  - body `$.items[3]`: reference `{"bucket":"<new_bucket>","entity":"project-viewers-<project-number>","etag":"<etag:str>","generation":"<generation:str:16>","id":"<new_bucket>/bkt-acl-crud-1...`, mimic `(absent)`

### bkt-notifications

Notification configurations on a bucket of its own without a Pub/Sub topic: empty list, creation naming a topic that does not exist, invalid payload formats, and get and delete of an id that does not exist. The topic is never created, so no notification is expected to persist.

- step `insert-missing-topic` (`POST /storage/v1/b/<bucket>/notificationConfigs`)
  - status: reference `400`, mimic `200`
  - header `etag`: reference `(absent)`, mimic `<etag:bare>`
  - body `$.error`: reference `{"code":400,"errors":[{"domain":"global","message":"Cloud Pub/Sub topic '//pubsub.googleapis.com/projects/<project>/topics/gcs-mimic-diff-no-such-topic' not ...`, mimic `(absent)`
  - body `$.etag`: reference `(absent)`, mimic `"<etag:str>"`
  - body `$.id`: reference `(absent)`, mimic `"1"`
  - body `$.kind`: reference `(absent)`, mimic `"storage#notification"`
  - body `$.payload_format`: reference `(absent)`, mimic `"JSON_API_V1"`
  - body `$.selfLink`: reference `(absent)`, mimic `"<host>/storage/v1/b/<bucket>/notificationConfigs/1"`
  - body `$.topic`: reference `(absent)`, mimic `"//pubsub.googleapis.com/projects/<project>/topics/gcs-mimic-diff-no-such-topic"`
- step `insert-invalid-event-type` (`POST /storage/v1/b/<bucket>/notificationConfigs`)
  - body `$.error.errors[0].message`: reference `"Cloud Pub/Sub topic '//pubsub.googleapis.com/projects/<project>/topics/gcs-mimic-diff-no-such-topic' not found, or user 'service-<project-number>@gs-project...`, mimic `"Invalid event type \"NO_SUCH_EVENT\"; use OBJECT_FINALIZE, OBJECT_METADATA_UPDATE, OBJECT_DELETE or OBJECT_ARCHIVE"`
  - body `$.error.message`: reference `"Cloud Pub/Sub topic '//pubsub.googleapis.com/projects/<project>/topics/gcs-mimic-diff-no-such-topic' not found, or user 'service-<project-number>@gs-project...`, mimic `"Invalid event type \"NO_SUCH_EVENT\"; use OBJECT_FINALIZE, OBJECT_METADATA_UPDATE, OBJECT_DELETE or OBJECT_ARCHIVE"`
- step `list-still-empty` (`GET /storage/v1/b/<bucket>/notificationConfigs`)
  - body `$.items`: reference `(absent)`, mimic `[{"etag":"<etag:str>","id":"1","kind":"storage#notification","payload_format":"JSON_API_V1","selfLink":"<host>/storage/v1/b/<bucket>/notificationConfigs/1","...`

### bkt-patch-put-edges

PATCH and PUT edge inputs on a fresh multi-region bucket created with a lowercase location: empty body, unknown and output-only fields, null and empty values per field, retention and soft delete set and cleared (never locked), projection=full, fields, predefined ACLs (non-public), and PUT omission semantics.

- step `invalid-predefined-acl` (`PATCH /storage/v1/b/<new_bucket>?predefinedAcl=notACannedAcl`)
  - body `$.error.errors[0].location`: reference `"predefinedAcl"`, mimic `(absent)`
  - body `$.error.errors[0].locationType`: reference `"parameter"`, mimic `(absent)`

### grpc-object-lifecycle

gRPC WriteObject, GetObject, ReadObject (whole and ranged), UpdateObject, DeleteObject and the not-found statuses afterwards.

- step `write` (`GRPC google.storage.v2.Storage/WriteObject`)
  - body `$.resource.acl[3]`: reference `{"email":"<email>","entity":"user-<email>","etag":"<etag:str>","id":"<bucket>/grpc-object-lifecycle-1/<generation:16>/user-<email>","role":"OWNER"}`, mimic `(absent)`
  - body `$.resource.owner.entity`: reference `"user-<email>"`, mimic `"project-owners-<project-number>"`
- step `update` (`GRPC google.storage.v2.Storage/UpdateObject`)
  - body `$.acl[3]`: reference `{"email":"<email>","entity":"user-<email>","etag":"<etag:str>","id":"<bucket>/grpc-object-lifecycle-1/<gen>/user-<email>","role":"OWNER"}`, mimic `(absent)`
  - body `$.owner.entity`: reference `"user-<email>"`, mimic `"project-owners-<project-number>"`

### grpc-object-list

gRPC ListObjects with prefix, delimiter, read mask, paging by page_token and versions.

- step `write-a` (`GRPC google.storage.v2.Storage/WriteObject`)
  - body `$.resource.acl[3]`: reference `{"email":"<email>","entity":"user-<email>","etag":"<etag:str>","id":"<bucket>/grpc-object-list-1/a/<generation:16>/user-<email>","role":"OWNER"}`, mimic `(absent)`
  - body `$.resource.owner.entity`: reference `"user-<email>"`, mimic `"project-owners-<project-number>"`
- step `write-b-x` (`GRPC google.storage.v2.Storage/WriteObject`)
  - body `$.resource.acl[3]`: reference `{"email":"<email>","entity":"user-<email>","etag":"<etag:str>","id":"<bucket>/grpc-object-list-1/b/x/<generation:16>/user-<email>","role":"OWNER"}`, mimic `(absent)`
  - body `$.resource.owner.entity`: reference `"user-<email>"`, mimic `"project-owners-<project-number>"`
- step `write-b-y` (`GRPC google.storage.v2.Storage/WriteObject`)
  - body `$.resource.acl[3]`: reference `{"email":"<email>","entity":"user-<email>","etag":"<etag:str>","id":"<bucket>/grpc-object-list-1/b/y/<generation:16>/user-<email>","role":"OWNER"}`, mimic `(absent)`
  - body `$.resource.owner.entity`: reference `"user-<email>"`, mimic `"project-owners-<project-number>"`

### grpc-object-preconditions

gRPC generation and metageneration preconditions on WriteObject, GetObject, UpdateObject and DeleteObject, and their FAILED_PRECONDITION statuses.

- step `write-if-absent` (`GRPC google.storage.v2.Storage/WriteObject`)
  - body `$.resource.acl[3]`: reference `{"email":"<email>","entity":"user-<email>","etag":"<etag:str>","id":"<bucket>/grpc-object-preconditions-1/<generation:16>/user-<email>","role":"OWNER"}`, mimic `(absent)`
  - body `$.resource.owner.entity`: reference `"user-<email>"`, mimic `"project-owners-<project-number>"`

### grpc-read-mask

gRPC read masks that name checksum sub-paths: GetObject, ReadObject metadata and ListObjects with checksums.crc32c, checksums.md5_hash and the whole message.

- step `write` (`GRPC google.storage.v2.Storage/WriteObject`)
  - body `$.resource.acl[3]`: reference `{"email":"<email>","entity":"user-<email>","etag":"<etag:str>","id":"<bucket>/grpc-read-mask-1/<generation:16>/user-<email>","role":"OWNER"}`, mimic `(absent)`
  - body `$.resource.owner.entity`: reference `"user-<email>"`, mimic `"project-owners-<project-number>"`

### obj-compose

Compose: destination metadata, absent destination, empty and missing sources, source generation and object preconditions, a composite as source, componentCount and deleteSourceObjects. Checks SPEC-COMPOSE-EDGES.

- step `download-composed` (`GET /storage/v1/b/<bucket>/o/obj-compose-2?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `compose-source-precondition-fail` (`POST /storage/v1/b/<bucket>/o/obj-compose-1-pre/compose`)
  - body `$.error.errors[0].location`: reference `"If-Match"`, mimic `(absent)`
  - body `$.error.errors[0].locationType`: reference `"header"`, mimic `(absent)`

### obj-copy

Copy: metadata inheritance for an empty, null and empty-object body, metadata override, storage class change, copy onto itself, source and destination preconditions and missing source. Checks SPEC-DERIVED-METADATA.

- step `copy-no-body` (`POST /storage/v1/b/<bucket>/o/obj-copy-1/copyTo/b/<bucket>/o/obj-copy-2`)
  - body `$.owner`: reference `{"entity":"user-<email>"}`, mimic `(absent)`
- step `copy-null-body` (`POST /storage/v1/b/<bucket>/o/obj-copy-1/copyTo/b/<bucket>/o/obj-copy-1-null`)
  - body `$.owner`: reference `{"entity":"user-<email>"}`, mimic `(absent)`
- step `copy-empty-object-body` (`POST /storage/v1/b/<bucket>/o/obj-copy-1/copyTo/b/<bucket>/o/obj-copy-1-empty`)
  - body `$.owner`: reference `{"entity":"user-<email>"}`, mimic `(absent)`
- step `copy-metadata-override` (`POST /storage/v1/b/<bucket>/o/obj-copy-1/copyTo/b/<bucket>/o/obj-copy-1-over`)
  - body `$.owner`: reference `{"entity":"user-<email>"}`, mimic `(absent)`
- step `copy-storage-class` (`POST /storage/v1/b/<bucket>/o/obj-copy-1/copyTo/b/<bucket>/o/obj-copy-1-cls`)
  - body `$.owner`: reference `{"entity":"user-<email>"}`, mimic `(absent)`
- step `copy-onto-itself` (`POST /storage/v1/b/<bucket>/o/obj-copy-1/copyTo/b/<bucket>/o/obj-copy-1`)
  - body `$.owner`: reference `{"entity":"user-<email>"}`, mimic `(absent)`

### obj-download-gzip

An object stored with contentEncoding gzip, downloaded with and without Accept-Encoding gzip. The body is a valid gzip stream of the text 'hello hello hello', so the downloads measure real transcoding and range handling as well as headers.

- step `download-accept-gzip` (`GET /storage/v1/b/<bucket>/o/obj-download-gzip-1?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - header `x-harness-body-framing`: reference `chunked`, mimic `content-length:<ok>`
- step `download-no-accept-encoding` (`GET /storage/v1/b/<bucket>/o/obj-download-gzip-1?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `range-accept-gzip` (`GET /storage/v1/b/<bucket>/o/obj-download-gzip-1?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - header `x-harness-body-framing`: reference `chunked`, mimic `content-length:<ok>`
- step `download-no-transform` (`GET /storage/v1/b/<bucket>/o/obj-download-gzip-1?alt=media`)
  - header `cache-control`: reference `no-cache, no-store, max-age=0, must-revalidate, no-transform`, mimic `no-cache, no-store, max-age=0, must-revalidate`
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - header `x-harness-body-framing`: reference `chunked`, mimic `content-length:<ok>`

### obj-download-range

Range downloads: closed, suffix, open-ended, clamped, unsatisfiable, malformed, multiple and non-bytes ranges, If-Range, and the same on an empty object. Checks SPEC-RANGE-FALLBACK and SPEC-RANGE-VALIDATORS.

- step `full` (`GET /storage/v1/b/<bucket>/o/obj-download-range-1?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `closed` (`GET /storage/v1/b/<bucket>/o/obj-download-range-1?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `suffix` (`GET /storage/v1/b/<bucket>/o/obj-download-range-1?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `open-ended` (`GET /storage/v1/b/<bucket>/o/obj-download-range-1?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `clamped` (`GET /storage/v1/b/<bucket>/o/obj-download-range-1?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `unsatisfiable` (`GET /storage/v1/b/<bucket>/o/obj-download-range-1?alt=media`)
  - header `expires`: reference `<time:http>`, mimic `(absent)`
- step `malformed` (`GET /storage/v1/b/<bucket>/o/obj-download-range-1?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `multiple` (`GET /storage/v1/b/<bucket>/o/obj-download-range-1?alt=media`)
  - header `cache-control`: reference `private, max-age=0`, mimic `(absent)`
  - header `expires`: reference `<time:http>`, mimic `(absent)`
- step `if-range-etag` (`GET /storage/v1/b/<bucket>/o/obj-download-range-1?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `if-range-stale` (`GET /storage/v1/b/<bucket>/o/obj-download-range-1?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `empty-full` (`GET /storage/v1/b/<bucket>/o/obj-download-range-2?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `empty-first-byte` (`GET /storage/v1/b/<bucket>/o/obj-download-range-2?alt=media`)
  - header `expires`: reference `<time:http>`, mimic `(absent)`
- step `empty-open-ended` (`GET /storage/v1/b/<bucket>/o/obj-download-range-2?alt=media`)
  - header `expires`: reference `<time:http>`, mimic `(absent)`

### obj-download-validators

Download response headers and conditional request headers (If-Match, If-None-Match, If-Modified-Since, If-Unmodified-Since) on media downloads and metadata reads. Checks SPEC-JSON-ETAG-WIRE.

- step `download` (`GET /storage/v1/b/<bucket>/o/obj-download-validators-1?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `media-if-none-match-current` (`GET /storage/v1/b/<bucket>/o/obj-download-validators-1?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `media-if-match-other` (`GET /storage/v1/b/<bucket>/o/obj-download-validators-1?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `media-if-modified-since-future` (`GET /storage/v1/b/<bucket>/o/obj-download-validators-1?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `media-if-unmodified-since-past` (`GET /storage/v1/b/<bucket>/o/obj-download-validators-1?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `media-if-none-match-and-match` (`GET /storage/v1/b/<bucket>/o/obj-download-validators-1?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `metadata-if-match-other` (`GET /storage/v1/b/<bucket>/o/obj-download-validators-1`)
  - header `etag`: reference `<etag:bare>`, mimic `(absent)`
  - body `$.error.errors[0].location`: reference `"If-Match"`, mimic `(absent)`
  - body `$.error.errors[0].locationType`: reference `"header"`, mimic `(absent)`

### obj-errors

Error bodies and headers of object requests: invalid parameters, bad alt, malformed JSON, missing bucket, unsupported methods and a trailing slash.

- step `post-object-resource` (`POST /storage/v1/b/<bucket>/o/obj-errors-1`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `get-media-trailing-slash` (`GET /storage/v1/b/<bucket>/o/obj-errors-1%2F?alt=media`)
  - header `expires`: reference `<time:http>`, mimic `(absent)`

### obj-get-fields

Object metadata reads with projection values and many fields selections: nested, wildcard, repeated, invalid and unknown selectors. Checks SPEC-FIELDS-EDGES.

- step `projection-full` (`GET /storage/v1/b/<bucket>/o/obj-get-fields-1?projection=full`)
  - body `$.acl[3]`: reference `{"bucket":"<bucket>","email":"<email>","entity":"user-<email>","etag":"<etag:str>","generation":"<generation:str:16>","id":"<bucket>/obj-get-fields-1/<genera...`, mimic `(absent)`
  - body `$.owner.entity`: reference `"user-<email>"`, mimic `"project-owners-<project-number>"`
- step `fields-acl-with-full` (`GET /storage/v1/b/<bucket>/o/obj-get-fields-1?fields=name%2Cacl%28entity%2Crole%29&projection=full`)
  - body `$.acl[3]`: reference `{"entity":"user-<email>","role":"OWNER"}`, mimic `(absent)`
- step `fields-owner` (`GET /storage/v1/b/<bucket>/o/obj-get-fields-1?fields=name%2Cowner&projection=full`)
  - body `$.owner.entity`: reference `"user-<email>"`, mimic `"project-owners-<project-number>"`
- step `patch-projection-full` (`PATCH /storage/v1/b/<bucket>/o/obj-get-fields-1?projection=full`)
  - body `$.acl[3]`: reference `{"bucket":"<bucket>","email":"<email>","entity":"user-<email>","etag":"<etag:str>","generation":"<generation:str:16>","id":"<bucket>/obj-get-fields-1/<genera...`, mimic `(absent)`
  - body `$.owner.entity`: reference `"user-<email>"`, mimic `"project-owners-<project-number>"`

### obj-holds-customtime

Temporary and event-based holds block delete and overwrite; PUT omission, explicit null and false; customTime set, backdate, clear and PUT omission. Both holds are released and the object deleted at the end. Checks SPEC-OBJECT-HOLDS-CUSTOMTIME and SPEC-EVENT-HOLD-WIRE.

- step `put-omits-hold` (`PUT /storage/v1/b/<bucket>/o/obj-holds-customtime-1`)
  - body `$.acl`: reference `(absent)`, mimic `[{"bucket":"<bucket>","entity":"project-owners-<project-number>","etag":"<etag:str>","generation":"<generation:str:16>","id":"<bucket>/obj-holds-customtime-1...`
  - body `$.owner`: reference `(absent)`, mimic `{"entity":"project-owners-<project-number>"}`
- step `put-null-hold` (`PUT /storage/v1/b/<bucket>/o/obj-holds-customtime-1`)
  - body `$.acl`: reference `(absent)`, mimic `[{"bucket":"<bucket>","entity":"project-owners-<project-number>","etag":"<etag:str>","generation":"<generation:str:16>","id":"<bucket>/obj-holds-customtime-1...`
  - body `$.owner`: reference `(absent)`, mimic `{"entity":"project-owners-<project-number>"}`
- step `release-temporary-hold` (`PATCH /storage/v1/b/<bucket>/o/obj-holds-customtime-1`)
  - body `$.temporaryHold`: reference `false`, mimic `(absent)`
- step `set-event-based-hold` (`PATCH /storage/v1/b/<bucket>/o/obj-holds-customtime-1`)
  - body `$.temporaryHold`: reference `false`, mimic `(absent)`
- step `release-event-hold-only` (`PATCH /storage/v1/b/<bucket>/o/obj-holds-customtime-1`)
  - body `$.eventBasedHold`: reference `false`, mimic `(absent)`
- step `hold-invalid-type` (`PATCH /storage/v1/b/<bucket>/o/obj-holds-customtime-1`)
  - body `$.eventBasedHold`: reference `false`, mimic `(absent)`
- step `release-all` (`PATCH /storage/v1/b/<bucket>/o/obj-holds-customtime-1`)
  - body `$.eventBasedHold`: reference `false`, mimic `(absent)`
  - body `$.temporaryHold`: reference `false`, mimic `(absent)`
- step `set-custom-time` (`PATCH /storage/v1/b/<bucket>/o/obj-holds-customtime-1`)
  - body `$.eventBasedHold`: reference `false`, mimic `(absent)`
  - body `$.temporaryHold`: reference `false`, mimic `(absent)`
- step `custom-time-null` (`PATCH /storage/v1/b/<bucket>/o/obj-holds-customtime-1`)
  - body `$.eventBasedHold`: reference `false`, mimic `(absent)`
  - body `$.temporaryHold`: reference `false`, mimic `(absent)`
- step `custom-time-later` (`PATCH /storage/v1/b/<bucket>/o/obj-holds-customtime-1`)
  - body `$.eventBasedHold`: reference `false`, mimic `(absent)`
  - body `$.temporaryHold`: reference `false`, mimic `(absent)`
- step `put-omits-custom-time` (`PUT /storage/v1/b/<bucket>/o/obj-holds-customtime-1`)
  - body `$.acl`: reference `(absent)`, mimic `[{"bucket":"<bucket>","entity":"project-owners-<project-number>","etag":"<etag:str>","generation":"<generation:str:16>","id":"<bucket>/obj-holds-customtime-1...`
  - body `$.owner`: reference `(absent)`, mimic `{"entity":"project-owners-<project-number>"}`
- step `put-with-custom-time` (`PUT /storage/v1/b/<bucket>/o/obj-holds-customtime-1`)
  - body `$.acl`: reference `(absent)`, mimic `[{"bucket":"<bucket>","entity":"project-owners-<project-number>","etag":"<etag:str>","generation":"<generation:str:16>","id":"<bucket>/obj-holds-customtime-1...`
  - body `$.owner`: reference `(absent)`, mimic `{"entity":"project-owners-<project-number>"}`

### obj-list

Object listing: prefix, delimiter, includeTrailingDelimiter, paging with maxResults and pageToken, zero and invalid maxResults, matchGlob (valid, empty, invalid), startOffset and endOffset, fields, versions and an unmatched prefix. Checks SPEC-LIST-PAGING, SPEC-LIST-GLOB-EDGES and SPEC-LIST-RESOURCE-SHAPE.

- step `list-projection-full` (`GET /storage/v1/b/<bucket>/o?prefix=obj-list-1%2Fa&projection=full`)
  - body `$.items[0].acl[3]`: reference `{"bucket":"<bucket>","email":"<email>","entity":"user-<email>","etag":"<etag:str>","generation":"<generation:str:16>","id":"<bucket>/obj-list-1/a.txt/<genera...`, mimic `(absent)`
  - body `$.items[0].owner.entity`: reference `"user-<email>"`, mimic `"project-owners-<project-number>"`

### obj-metadata-write

PATCH and PUT of object metadata: custom metadata, null removal of a key and of the whole map, null content fields, empty bodies, unknown fields, wrong types and metageneration preconditions. Checks SPEC-015.

- step `put-replace` (`PUT /storage/v1/b/<bucket>/o/obj-metadata-write-1`)
  - body `$.acl`: reference `(absent)`, mimic `[{"bucket":"<bucket>","entity":"project-owners-<project-number>","etag":"<etag:str>","generation":"<generation:str:16>","id":"<bucket>/obj-metadata-write-1/<...`
  - body `$.owner`: reference `(absent)`, mimic `{"entity":"project-owners-<project-number>"}`
- step `put-empty` (`PUT /storage/v1/b/<bucket>/o/obj-metadata-write-1`)
  - body `$.acl`: reference `(absent)`, mimic `[{"bucket":"<bucket>","entity":"project-owners-<project-number>","etag":"<etag:str>","generation":"<generation:str:16>","id":"<bucket>/obj-metadata-write-1/<...`
  - body `$.owner`: reference `(absent)`, mimic `{"entity":"project-owners-<project-number>"}`

### obj-move

Move (moveTo): the source disappears and the destination keeps content and metadata; missing source, move onto itself and generation preconditions.

- step `download-destination` (`GET /storage/v1/b/<bucket>/o/obj-move-2?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `move-missing-source` (`POST /storage/v1/b/<bucket>/o/obj-move-1/moveTo/o/obj-move-1-x`)
  - body `$.error.errors[0].message`: reference `"<email> does not have storage.objects.move access to the Google Cloud Storage object. Permission 'storage.objects.move' denied on resource '//<host>/project...`, mimic `"Caller does not have storage.objects.move access to the Google Cloud Storage object. Permission 'storage.objects.move' denied on resource '//<host>/projects...`
  - body `$.error.message`: reference `"<email> does not have storage.objects.move access to the Google Cloud Storage object. Permission 'storage.objects.move' denied on resource '//<host>/project...`, mimic `"Caller does not have storage.objects.move access to the Google Cloud Storage object. Permission 'storage.objects.move' denied on resource '//<host>/projects...`

### obj-resumable

Resumable upload: start, status query, single-request completion, query and resend after completion, a separate session with a small non-final chunk, cancel and query after cancel, bad Content-Range, mismatched total, an unknown session and start-time errors. Checks SPEC-RSM-EDGES.

- step `start` (`POST /upload/storage/v1/b/<bucket>/o?name=obj-resumable-1&uploadType=resumable`)
  - header `cache-control`: reference `no-cache, no-store, max-age=0, must-revalidate`, mimic `no-store`
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - header `pragma`: reference `no-cache`, mimic `(absent)`
- step `status-empty` (`PUT /upload/storage/v1/b/<bucket>/o?uploadType=resumable&upload_id=<up1>`)
  - header `cache-control`: reference `(absent)`, mimic `no-store`
  - header `vary`: reference `(absent)`, mimic `Origin, X-Origin`
- step `download-result` (`GET /storage/v1/b/<bucket>/o/obj-resumable-1?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `start-chunked` (`POST /upload/storage/v1/b/<bucket>/o?name=obj-resumable-2&uploadType=resumable`)
  - header `cache-control`: reference `no-cache, no-store, max-age=0, must-revalidate`, mimic `no-store`
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - header `pragma`: reference `no-cache`, mimic `(absent)`
- step `total-mismatch` (`PUT /upload/storage/v1/b/<bucket>/o?uploadType=resumable&upload_id=<up2>`)
  - header `vary`: reference `(absent)`, mimic `Origin, X-Origin`
- step `bad-content-range` (`PUT /upload/storage/v1/b/<bucket>/o?uploadType=resumable&upload_id=<up2>`)
  - header `vary`: reference `(absent)`, mimic `Origin, X-Origin`
- step `chunk-not-final` (`PUT /upload/storage/v1/b/<bucket>/o?uploadType=resumable&upload_id=<up2>`)
  - header `cache-control`: reference `(absent)`, mimic `no-cache, no-store, max-age=0, must-revalidate`
  - header `content-type`: reference `text/plain; charset=utf-8`, mimic `application/json; charset=UTF-8`
  - header `pragma`: reference `(absent)`, mimic `no-cache`
  - header `vary`: reference `(absent)`, mimic `Origin, X-Origin`
  - body `$`: reference `"Failed to parse Content-Range header."`, mimic `{"error":{"code":400,"errors":[{"domain":"global","message":"Nonfinal chunk must be a multiple of 256 KiB","reason":"invalidParameter"}],"message":"Nonfinal ...`
- step `status-after-chunk` (`PUT /upload/storage/v1/b/<bucket>/o?uploadType=resumable&upload_id=<up2>`)
  - mimic: expected status 400, got 308
  - status: reference `400`, mimic `308`
  - header `cache-control`: reference `(absent)`, mimic `no-store`
  - header `vary`: reference `(absent)`, mimic `Origin, X-Origin`
  - body `$`: reference `"Failed to parse Content-Range header."`, mimic `(absent)`
- step `cancel` (`DELETE /upload/storage/v1/b/<bucket>/o?uploadType=resumable&upload_id=<up2>`)
  - mimic: expected status 400, got 499
  - status: reference `400`, mimic `499`
  - header `cache-control`: reference `(absent)`, mimic `no-store`
  - header `content-type`: reference `text/plain; charset=utf-8`, mimic `(absent)`
  - header `vary`: reference `(absent)`, mimic `Origin, X-Origin`
  - body `$`: reference `"Failed to parse Content-Range header."`, mimic `(absent)`
- step `status-after-cancel` (`PUT /upload/storage/v1/b/<bucket>/o?uploadType=resumable&upload_id=<up2>`)
  - mimic: expected status 400, got 410
  - status: reference `400`, mimic `410`
  - header `cache-control`: reference `(absent)`, mimic `no-cache, no-store, max-age=0, must-revalidate`
  - header `content-type`: reference `text/plain; charset=utf-8`, mimic `application/json; charset=UTF-8`
  - header `pragma`: reference `(absent)`, mimic `no-cache`
  - header `vary`: reference `(absent)`, mimic `Origin, X-Origin`
  - body `$`: reference `"Failed to parse Content-Range header."`, mimic `{"error":{"code":410,"errors":[{"domain":"global","message":"Upload session is no longer active","reason":"gone"}],"message":"Upload session is no longer act...`
- step `unknown-session` (`PUT /upload/storage/v1/b/<bucket>/o?uploadType=resumable&upload_id=<upload-id>`)
  - header `vary`: reference `(absent)`, mimic `Origin, X-Origin`
- step `start-no-name` (`POST /upload/storage/v1/b/<bucket>/o?uploadType=resumable`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - header `x-content-type-options`: reference `(absent)`, mimic `nosniff`
  - body `$`: reference `"{\n  \"error\": {\n    \"code\": 400,\n    \"message\": \"You must specify an object name.\",\n    \"errors\": [\n      {\n        \"message\": \"You must s...`, mimic `"{\n  \"error\": {\n    \"code\": 400,\n    \"message\": \"You must specify an object name.\",\n    \"errors\": [\n      {\n        \"message\": \"You must s...`
- step `start-precondition-fail` (`POST /upload/storage/v1/b/<bucket>/o?ifGenerationMatch=0&name=obj-resumable-1&uploadType=resumable`)
  - header `cache-control`: reference `no-cache, no-store, max-age=0, must-revalidate`, mimic `no-store`
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - header `pragma`: reference `no-cache`, mimic `(absent)`

### obj-rewrite

Rewrite: a small object completes in one call, metadata body, storage class change, bad and unknown rewrite tokens, preconditions and missing source. Checks SPEC-REWRITE-EDGES and SPEC-DERIVED-METADATA.

- step `rewrite-no-body` (`POST /storage/v1/b/<bucket>/o/obj-rewrite-1/rewriteTo/b/<bucket>/o/obj-rewrite-2`)
  - body `$.resource.owner`: reference `{"entity":"user-<email>"}`, mimic `(absent)`
- step `rewrite-metadata-body` (`POST /storage/v1/b/<bucket>/o/obj-rewrite-1/rewriteTo/b/<bucket>/o/obj-rewrite-1-meta`)
  - body `$.resource.owner`: reference `{"entity":"user-<email>"}`, mimic `(absent)`
- step `rewrite-storage-class` (`POST /storage/v1/b/<bucket>/o/obj-rewrite-1/rewriteTo/b/<bucket>/o/obj-rewrite-1-cls`)
  - body `$.resource.owner`: reference `{"entity":"user-<email>"}`, mimic `(absent)`
- step `rewrite-onto-itself` (`POST /storage/v1/b/<bucket>/o/obj-rewrite-1/rewriteTo/b/<bucket>/o/obj-rewrite-1`)
  - body `$.resource.owner`: reference `{"entity":"user-<email>"}`, mimic `(absent)`

### obj-upload-checksums

JSON media and multipart uploads with correct and wrong CRC32C and MD5 values (X-Goog-Hash, Content-MD5, metadata fields). Checks SPEC-010 and SPEC-016.

- step `download-hash-headers` (`GET /storage/v1/b/<bucket>/o/obj-upload-checksums-1-both?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`

### obj-upload-content-type

Media upload without a Content-Type header, without a name, with contentEncoding in the query, an empty body, a name that needs percent-encoding and bad preconditions or uploadType values. Checks SPEC-011.

- step `no-name` (`POST /upload/storage/v1/b/<bucket>/o?uploadType=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - header `x-content-type-options`: reference `(absent)`, mimic `nosniff`
  - body `$`: reference `"{\n  \"error\": {\n    \"code\": 400,\n    \"message\": \"You must specify an object name.\",\n    \"errors\": [\n      {\n        \"message\": \"You must s...`, mimic `"{\n  \"error\": {\n    \"code\": 400,\n    \"message\": \"You must specify an object name.\",\n    \"errors\": [\n      {\n        \"message\": \"You must s...`
- step `download-encoded-name` (`GET /storage/v1/b/<bucket>/o/obj-upload-content-type-1%20sp%2F%C3%BC%3F%23%2B?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `invalid-precondition-value` (`POST /upload/storage/v1/b/<bucket>/o?ifGenerationMatch=abc&name=obj-upload-content-type-1-absent&uploadType=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - header `x-content-type-options`: reference `(absent)`, mimic `nosniff`
  - body `$`: reference `"{\n  \"error\": {\n    \"code\": 400,\n    \"message\": \"Invalid long value: 'abc'.\",\n    \"errors\": [\n      {\n        \"message\": \"Invalid long val...`, mimic `"{\n  \"error\": {\n    \"code\": 400,\n    \"message\": \"Invalid long value: 'abc'.\",\n    \"errors\": [\n      {\n        \"message\": \"Invalid long val...`
- step `unknown-upload-type` (`POST /upload/storage/v1/b/<bucket>/o?name=obj-upload-content-type-1-absent&uploadType=bogus`)
  - header `vary`: reference `(absent)`, mimic `Origin, X-Origin`

### obj-upload-multipart

Multipart upload: metadata fields, content type precedence, missing media part type, missing name, missing boundary, one part only and invalid metadata JSON. Checks SPEC-016 and SPEC-011.

- step `metadata-type-wins` (`POST /upload/storage/v1/b/<bucket>/o?uploadType=multipart`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - header `x-content-type-options`: reference `(absent)`, mimic `nosniff`
  - body `$`: reference `"{\n  \"error\": {\n    \"code\": 400,\n    \"message\": \"Content-Type specified in the upload (text/plain) does not match Content-Type specified in metadat...`, mimic `"{\n  \"error\": {\n    \"code\": 400,\n    \"message\": \"Content-Type specified in the upload (text/plain) does not match Content-Type specified in metadat...`
- step `no-name` (`POST /upload/storage/v1/b/<bucket>/o?uploadType=multipart`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - header `x-content-type-options`: reference `(absent)`, mimic `nosniff`
  - body `$`: reference `"{\n  \"error\": {\n    \"code\": 400,\n    \"message\": \"You must specify an object name.\",\n    \"errors\": [\n      {\n        \"message\": \"You must s...`, mimic `"{\n  \"error\": {\n    \"code\": 400,\n    \"message\": \"You must specify an object name.\",\n    \"errors\": [\n      {\n        \"message\": \"You must s...`
- step `missing-boundary` (`POST /upload/storage/v1/b/<bucket>/o?uploadType=multipart`)
  - header `vary`: reference `(absent)`, mimic `Origin, X-Origin`
- step `invalid-metadata-json` (`POST /upload/storage/v1/b/<bucket>/o?uploadType=multipart`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
  - header `x-content-type-options`: reference `(absent)`, mimic `nosniff`
  - body `$`: reference `"{\n  \"error\": {\n    \"code\": 400,\n    \"message\": \"Parse Error: Expected : between key:value pair.\\n{not json\\n     ^\",\n    \"errors\": [\n      ...`, mimic `"{\n  \"error\": {\n    \"code\": 400,\n    \"message\": \"Parse Error: Expected : between key:value pair.\\n{not json\\n     ^\",\n    \"errors\": [\n      ...`

### obj-versioning

With bucket versioning enabled for the duration of the case: generation-specific get and delete, combined generation and metageneration preconditions, noncurrent versions, listing with versions and copy from a source generation. Versioning is switched off again at the end. Checks SPEC-008 and SPEC-VERSION-DELETE-CONDITIONS.

- step `get-g1-media` (`GET /storage/v1/b/<bucket>/o/obj-versioning-1?alt=media&generation=<g1>`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`
- step `disable-versioning` (`PATCH /storage/v1/b/<bucket>?fields=name%2Cversioning`)
  - body `$.versioning`: reference `{"enabled":false}`, mimic `(absent)`

### object-get-download

Read object metadata, a fields selection and the media download.

- step `download` (`GET /storage/v1/b/<bucket>/o/object-get-download-1?alt=media`)
  - header `expires`: reference `Mon, 01 Jan 1990 00:00:00 GMT`, mimic `(absent)`

### object-missing

Metadata read, media download and delete of an object that does not exist.

- step `download` (`GET /storage/v1/b/<bucket>/o/object-missing-1?alt=media`)
  - header `expires`: reference `<time:http>`, mimic `(absent)`

## Cleanup

```text
- mimic new bucket of bkt-acl-crud: already absent (404)
- mimic new bucket of bkt-create-errors: already absent (404)
- mimic new bucket of bkt-create-full: already absent (404)
- mimic new bucket of bkt-create-lifecycle-array: already absent (404)
- mimic new bucket of bkt-create-predefined-acl-ubla: already absent (404)
- mimic new bucket of bkt-create-unknown-field: already absent (404)
- mimic new bucket of bkt-create-unknown-query: already absent (404)
- mimic new bucket of bkt-iam-pap-ubla: already absent (404)
- mimic bucket of bkt-notifications: emptied (0 object versions), deleted, GET confirmed 404
- mimic bucket of bkt-patch-errors: emptied (0 object versions), deleted, GET confirmed 404
- mimic new bucket of bkt-patch-put-edges: already absent (404)
- mimic new bucket of bucket-lifecycle: already absent (404)
- mimic bucket of bucket-patch-labels: emptied (0 object versions), deleted, GET confirmed 404
- mimic bucket of bucket-preconditions: emptied (0 object versions), deleted, GET confirmed 404
- mimic new bucket of obj-retention-unlocked: already absent (404)
- mimic bucket of obj-versioning: emptied (1 object versions), deleted, GET confirmed 404
- mimic shared bucket: emptied (29 object versions), deleted, GET confirmed 404
- mimic sweep: 0 leftover bucket(s) found
```
