# Response wire format observed on Cloud Storage

Observed **2026-10-31** with curl requests sent to `storage.googleapis.com`
from an opt-in test project, using a private, uniquely named bucket with
uniform bucket-level access in `asia-northeast1` that was deleted afterwards.
The project, project number, bucket and principals are not recorded; the
bucket appears as `<bucket>` below. Only per-request infrastructure headers
(`date`, `server`, `alt-svc`, `x-guploader-uploadid`) were removed from the
recording. The same requests were replayed against gcs-mimic.

Objects: `plain.txt`, the 16 bytes `plain text body\n` (MD5
`3S6ENAmCNQC429h/U5OY5Q==`), and `gz.txt`, a gzip member. Gzip transcoding,
JSON media HEAD, the `contentEncoding` upload parameter and `Vary` headers
are recorded in [media read observations](media-read-provider-observations.md)
and are not part of this document.

## Entity tags

| Response | Recorded value |
| --- | --- |
| JSON object metadata and media `ETag` header | `CK+S+qOG5JcDEAE=`, unquoted; equal to the resource `etag` |
| JSON bucket `ETag` header and `etag` | `CAE=` |
| XML object GET/HEAD `ETag` | `"dd2e843409823500b8dbd87f539398e5"` (quoted MD5 hex) |
| XML list `<ETag>` | `"dd2e843409823500b8dbd87f539398e5"`, with literal quote characters |

`CK+S+qOG5JcDEAE=` decodes to `08 af92faa386e49703 10 01`: protobuf field 1
is the generation `1793442589870383` as a varint and field 2 the
metageneration `1`. `CAE=` decodes to `08 01`, the metageneration alone. The
same encoding was already used for ACL entry etags.

### gcs-mimic behavior

| Observation | Local behavior | Test |
| --- | --- | --- |
| JSON ETag encoding | Object and bucket tags use the recorded encoding in header and body; ACL entries, objects, buckets and gRPC messages share one encoder; XML keeps the quoted MD5 and quotes the JSON tag for composite and encrypted objects (that XML form is unrecorded) | `wire_parity_http::json_etags_encode_generation_and_metageneration_like_cloud_storage`, `json::object_tests::obj_render_001_exact_resource_and_encoded_generation_links`, `json::tests::bkt_id_002_etag_encodes_only_the_metageneration` |
| SDKs echo the unquoted tag | A bare or quoted `If-Match`/`If-None-Match` member names the same JSON tag; members with whitespace or stray quotes, and a wildcard inside a list, still return 400 | `http_conditions::tests::unquoted_json_validators_match_bare_and_quoted_members`, `etag::json_etag_read_syntax_and_missing_resources`, `metadata_if_match_reuses_strong_lists_wildcards_and_complete_syntax_validation` |
| Recreated bucket | The tag encodes only the metageneration, so a recreated bucket at the same metageneration repeats it. Observed 2026-10-31: deleting and recreating a bucket under the same name returned `CAE=` again (new generation, metageneration 1); a GET of the new incarnation with the old tag answered 200 for `If-Match` and 304 (no body) for `If-None-Match` | `etag::json_etag_read_buckets_and_metadata_changes_in_both_profiles` |

## Cache headers

| Response | `cache-control` | `pragma` |
| --- | --- | --- |
| JSON object metadata, object list, bucket metadata | `private, max-age=0, must-revalidate, no-transform` | absent |
| JSON media 200 and 206 | `no-cache, no-store, max-age=0, must-revalidate` | `no-cache` |
| JSON `objects.insert` (`uploadType=media`) | `no-cache, no-store, max-age=0, must-revalidate` | `no-cache` |
| JSON error (404 metadata) | `no-cache, no-store, max-age=0, must-revalidate` | `no-cache` |
| JSON media 404 and 416 | `private, max-age=0` | absent |
| XML private object GET/HEAD (including 206), XML list, XML 404 | `private, max-age=0` | absent |

Only `uploadType=media` inserts were recorded as writes. Patch, update, copy,
compose, rewrite, move, restore, delete, bucket writes and resumable or
multipart uploads were not observed.

### gcs-mimic behavior

| Observation | Local behavior | Test |
| --- | --- | --- |
| Metadata and list reads | Bucket and object get and list send the resource value without `pragma`; IAM policy reads keep `no-cache` because they were not recorded | `wire_parity_http::cache_headers_follow_each_recorded_response_kind`, `etag::json_etag_read_object_metadata_media_and_range` |
| Media 200 and 206 | `cache-control` is the no-store value; public objects keep their stored or `public, max-age=3600` value. The media `pragma` belongs to the media-header slice | `wire_parity_http::cache_headers_follow_each_recorded_response_kind`, `json::object_tests::obj_render_002_media_headers_match_selected_generation_and_hashes` |
| Insert and JSON errors | No-store value plus `pragma: no-cache` on object inserts (media and multipart, the same method) and on every JSON error body; a 304 stays bodyless | `wire_parity_http::object_inserts_and_json_write_errors_forbid_every_cache` |
| Media 404 and 416 | `private, max-age=0` without `pragma`; other media errors keep the JSON error headers | `wire_parity_http::cache_headers_follow_each_recorded_response_kind` |
| XML | Lists and error documents send `private, max-age=0`; private objects already did and a stored directive still wins | `wire_parity_http::cache_headers_follow_each_recorded_response_kind`, `xml_cache_control_respects_metadata_and_distinguishes_public_from_authenticated` |

## Error bodies

| Request | Recorded response |
| --- | --- |
| JSON metadata GET of a missing object | `application/json; charset=UTF-8`; fields `code`, `message`, `errors[{message, domain, reason}]`; message `No such object: <bucket>/missing.txt`, reason `notFound` |
| JSON media GET of a missing object | `text/html; charset=UTF-8`, body `No such object: <bucket>/missing.txt` |
| JSON media HEAD with an unsatisfiable range | 416, `text/html; charset=UTF-8`, `content-range: bytes */<size>`, `cache-control: private, max-age=0` (HEAD, so no body was seen) |
| XML GET of a missing object | `<?xml version='1.0' encoding='UTF-8'?><Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message><Details>No such object: <bucket>/missing.txt</Details></Error>` |

A follow-up probe recorded how object names are written into these bodies: the
JSON media 404 HTML-escapes the name (`&` to `&amp;`, `<` to `&lt;`, `>` to
`&gt;`, `"` to `&quot;`, `'` to `&#39;`) and sends no `x-content-type-options`
or content-security-policy header, while the XML `Details` escapes only `&`,
`<` and `>` and leaves quotes literal.

### gcs-mimic behavior

| Observation | Local behavior | Test |
| --- | --- | --- |
| Missing object messages | JSON get, patch and delete of a missing object say `No such object: <bucket>/<object>`; copy, compose and rewrite sources keep the generic `Object not found` because the request path names a different object | `wire_parity_http::missing_object_errors_name_the_object_like_cloud_storage` |
| Media 404 | `text/html; charset=UTF-8` with the escaped message and `private, max-age=0`; the name is escaped because the response is HTML and the name is client-controlled | `wire_parity_http::missing_object_errors_name_the_object_like_cloud_storage`, `wire_parity_http::missing_object_names_are_escaped_for_the_document_they_appear_in` |
| Media 416 | `text/html; charset=UTF-8`, `content-range: bytes */<size>` and `private, max-age=0`; the GET body text (the local range message) was not recorded | `wire_parity_http::unsatisfiable_media_ranges_answer_with_html_text` |
| XML NoSuchKey | Fixed `Message` plus a `Details` element naming the object, escaped as XML character data | `wire_parity_http::missing_object_names_are_escaped_for_the_document_they_appear_in` |

## JSON bodies

Every recorded JSON body (object, list, bucket, error) is pretty-printed with
two-space indentation and `"name": value` separators, and `content-length`
matches the body. Fields arrive in a fixed order:

- Object: `kind`, `id`, `selfLink`, `mediaLink`, `name`, `bucket`,
  `generation`, `metageneration`, `contentType`, `storageClass`, `size`,
  `md5Hash`, `contentEncoding` (gzip object only), `crc32c`, `etag`,
  `timeCreated`, `updated`, `timeStorageClassUpdated`, `timeFinalized`.
- Object list: `kind`, `nextPageToken`, `items`.
- Bucket: `kind`, `selfLink`, `id`, `name`, `projectNumber`, `generation`,
  `metageneration`, `location`, `storageClass`, `etag`, `timeCreated`,
  `updated`, `softDeletePolicy`, `iamConfiguration` (`bucketPolicyOnly`,
  `uniformBucketLevelAccess`, `publicAccessPrevention`), `locationType`,
  `satisfiesPZI`.

The bucket `generation` was `1793442588560288904`, a 19-digit nanosecond-scale
instant about half a second before `timeCreated`. `softDeletePolicy` carried
the default 604800 seconds with `effectiveTime` equal to `timeCreated`;
`satisfiesPZI` was `true`.

### gcs-mimic behavior

| Observation | Local behavior | Test |
| --- | --- | --- |
| Pretty JSON in recorded order | Every JSON API body, including errors, is pretty unless `prettyPrint=false` (or `False`), which selects compact output; `prettyPrint=true` is accepted and a non-boolean value is still 400 | `wire_parity_http::json_bodies_are_pretty_printed_in_recorded_field_order`, `json::format::tests::prettyprint_false_selects_compact_output_and_everything_else_pretty` |
| Legacy emulator profile | Stays compact, as fake-gcs-server writes it | `wire_parity_http::legacy_profile_keeps_compact_bodies` |
| Batch | Each part renders in the layout its own, inherited or overridden, `prettyPrint` selects, not the envelope's | `wire_parity_http::batch_parts_choose_their_own_format` |
| Object and list fields | Recorded order; fields the recording did not contain follow it in an unverified order | `wire_parity_http::json_bodies_are_pretty_printed_in_recorded_field_order` |
| Bucket fields | Recorded order with `generation` after `projectNumber`; the value is the catalog's stored bucket version, not a creation-time derivation. The legacy profile omits it. A bucket list was not recorded and uses the object list layout | `wire_parity_http::bucket_resources_carry_recorded_fields_in_recorded_order` |

The local bucket `generation` is the catalog's stored value, which is
microsecond-scale (16 digits) rather than the recorded 19-digit
nanosecond-scale value, and the default `softDeletePolicy` and `satisfiesPZI`
stay absent; see `SPEC-RESPONSE-WIRE-REMAINING`.

## satisfiesPZI and satisfiesPZS in write bodies

Observed **2026-10-31** with curl against the JSON API, using throwaway
buckets in `asia-northeast1` that were deleted afterwards (each `buckets.get`
then returned 404). Bucket names, the project and the project number are not
recorded. Both fields are described only as `Reserved for future use.` by
[live JSON Discovery](https://storage.googleapis.com/$discovery/rest?version=v1)
(`schemas.Bucket.properties`, revision `20261026`, SHA-256
`91588de46b456618d09d202a6595ba1e4d1752658251fce0aa61e7f36781adff`); the
[Storage v2 proto](https://github.com/googleapis/googleapis/blob/d73dfb3c3be52245625b951b9c0bcf65163c549f/google/storage/v2/storage.proto)
(googleapis `d73dfb3c3be52245625b951b9c0bcf65163c549f`, SHA-256
`a0df6132967b97a89be248f1f24d277ee8df8f8d885db1d4af6249469ff0c1cf`) has no
`satisfies_pzi` field and marks `satisfies_pzs` `Optional. Reserved for future
use.` Neither source says what a write does, so the behavior below was observed.

Every response was `HTTP/2 200` with `content-type: application/json;
charset=UTF-8`, `vary: Origin` and `vary: X-Origin`, and an
`x-goog-gcs-base-ts` header whose value changes per request. Writes carried
`cache-control: no-cache, no-store, max-age=0, must-revalidate`,
`expires: Mon, 01 Jan 1990 00:00:00 GMT` and `pragma: no-cache`; reads carried
`cache-control: private, max-age=0, must-revalidate, no-transform` and no
`pragma`.

| Request | Body member | Status | `satisfiesPZI` in the response | `satisfiesPZS` in the response | `metageneration` / `etag` after |
| --- | --- | --- | --- | --- | --- |
| `buckets.insert` | `satisfiesPZI: true` | 200 | `true` | absent | `1` / `CAE=` |
| `buckets.insert` | `satisfiesPZI: false` | 200 | `true` | absent | `1` / `CAE=` |
| `buckets.get` after each insert | none | 200 | `true` | absent | `1` / `CAE=` |
| `buckets.patch` | `satisfiesPZI: true` | 200 | `true` | absent | `1` / `CAE=` (unchanged) |
| `buckets.patch` | `satisfiesPZI: false` | 200 | `true` | absent | `1` / `CAE=` (unchanged) |
| `buckets.update` (PUT) | `satisfiesPZI: true` | 200 | `true` | absent | `2` / `CAI=` |
| `buckets.update` (PUT) | `satisfiesPZI: false` | 200 | `true` | absent | `3` / `CAM=` |
| `buckets.get` after the PUTs | none | 200 | `true` | absent | `3` / `CAM=` |
| `buckets.insert` | `satisfiesPZS: true` | 200 | `true` | absent | `1` / `CAE=` |
| `buckets.insert` | `satisfiesPZS: false` | 200 | `true` | absent | `1` / `CAE=` |
| `buckets.patch` | `satisfiesPZS: true` | 200 | `true` | absent | `1` / `CAE=` (unchanged) |
| `buckets.patch` | `satisfiesPZS: false` | 200 | `true` | absent | `1` / `CAE=` (unchanged) |
| `buckets.update` (PUT) | `satisfiesPZS: true` | 200 | `true` | absent | `2` / `CAI=` |
| `buckets.update` (PUT) | `satisfiesPZS: false` | 200 | `true` | absent | `3` / `CAM=` |
| `buckets.get` after the PUTs | none | 200 | `true` | absent | `3` / `CAM=` |

The PUT bodies carried only the one member, and the PUT still succeeded. The
PATCH responses used the default `projection=full` for the first group (they
added `acl`, `defaultObjectAcl` and `owner` to the 818-byte resource, 3118
bytes in all) and `fields=` for the second group.

A PATCH whose body held no writable setting did not advance the resource, and
the `updated` time stayed as it was. Observed with `fields=metageneration,updated`
on a bucket at metageneration 3:

| PATCH body | Status | `metageneration` after |
| --- | --- | --- |
| `{}` | 200 | `3` (unchanged) |
| `{"labels":{}}` | 200 | `4` |
| `{"versioning":{}}` | 200 | `4` (unchanged) |
| `{"labels":{"missing":null}}` | 200 | `5` |
| `{"labels":{"a":"b"}}`, sent twice | 200, 200 | `6`, then `7` |

### gcs-mimic behavior

| Observation | Local behavior | Test |
| --- | --- | --- |
| `satisfiesPZI` and `satisfiesPZS`, true and false, are accepted in insert, PATCH and PUT bodies and ignored | Both members are ignored on all three methods and in the legacy profile too; the native profile reports `satisfiesPZI: true` and never emits `satisfiesPZS`. The reply to a write and a following get agree. The gRPC adapter has no `satisfies_pzi`, and its `satisfies_pzs` is never read | `bucket_http::satisfies_pzi_and_pzs_are_ignored_on_insert_patch_and_put_whatever_their_value`, `json::tests::satisfies_pzi_and_pzs_in_a_write_body_are_ignored_whatever_their_value` |
| A PATCH without a writable setting changes nothing | The catalog returns the stored bucket without advancing the metageneration or `updated`; `labels:{}` and a null label still count as a write. A PUT always advances them | `bucket_http::satisfies_pzi_and_pzs_are_ignored_on_insert_patch_and_put_whatever_their_value`, `bucket_http::bucket_patch_empty_null_and_invalid_inputs` |
| `autoclass:{}` is a no-op, `autoclass:{"enabled":false}` is a write | `autoclass:{}` counts as no writable setting; `enabled:false` advances the metageneration, etag and `updated` | `bucket_http::empty_patch_evaluates_metageneration_preconditions_and_keeps_the_metageneration` |
| Preconditions of an empty PATCH | A mismatching `ifMetagenerationMatch` is 412; an `ifMetagenerationNotMatch` naming the current value is an empty 304; otherwise 200 unchanged. The native profile words the 412 and names `If-Match` as observed (PUT and non-empty PATCH share the wording, unobserved) | same test |

A second session (2026-10-31, `us-central1` buckets with
`softDeletePolicy.retentionDurationSeconds: 0`) covered `autoclass` and the
metageneration preconditions of an empty PATCH. All responses were 200 unless
noted; write responses carried the usual no-store headers.

| PATCH body (bucket at metageneration 1, then 2) | Status | Result |
| --- | --- | --- |
| `{"autoclass":{}}` | 200 | metageneration 1, etag `CAE=` and `updated` unchanged, no `autoclass` member |
| `{"autoclass":{"enabled":false}}` | 200 | metageneration 2, etag `CAI=`, `updated` advanced, still no `autoclass` member (also on a following GET) |
| `{}` with `ifMetagenerationMatch=1` (current 2) | 412 | reason `conditionNotMet`, message `At least one of the pre-conditions you specified did not hold.`, `locationType` `header`, `location` `If-Match` |
| `{}` with `ifMetagenerationMatch=2` | 200 | metageneration 2, `updated` unchanged |
| `{}` with `ifMetagenerationNotMatch=2` | 304 | empty body, `content-length: 0`, `content-type: application/json` |
| `{}` with `ifMetagenerationNotMatch=1` | 200 | metageneration 2 unchanged |
| `{}` | 200 | metageneration 2 unchanged |

The earlier recording showed `satisfiesPZI: true` only for `asia-northeast1`;
the rows above are that location too. `us-central1` was observed later (see below).
Ledger row: `BUCKET-SATISFIES-PZI`.

## Object retention time precision

Observed **2026-10-31** with curl against the JSON API, using throwaway
buckets in `us-central1` (STANDARD, created with `enableObjectRetention=true`
and `softDeletePolicy.retentionDurationSeconds: 0`, no bucket retention
policy), one 2-byte object, and `retention.mode` `Unlocked` in the first
session. The retention was removed with `overrideUnlockedRetention=true`
before the object and then the bucket were deleted; `buckets.get` then
returned 404.

Responses were `HTTP/2 200` unless noted, with the write and read cache
headers listed above. A PATCH response carried the resource etag with the
object metageneration incremented (`CLm/tNLR5JcDEAI=` after the first PATCH).
`fields=retention` was used on reads and writes.

| Request `retainUntilTime` (now + 10 minutes) | Status | Value read back (PATCH reply and GET) |
| --- | --- | --- |
| `…:00.123456789Z` | 200 | `…:00.123Z` |
| then `…:00.123999999Z` | 200 | `…:00.123Z` |
| then `…:00.123000001Z` | 403 | not changed |

The 403 body was `error.code` 403, reason `forbidden`, message `The unlocked
object retention cannot be removed and its retention period cannot be
shortened without overriding unlocked retention intent and permission.` In a
second bucket the same calls with `…:00.123456789Z` stored first gave:

| Next request | Status | Meaning |
| --- | --- | --- |
| `…:00.123456788Z` (1 ns earlier) | 403 | shortening |
| `…:00.123456789Z` (same) | 200 | accepted, `…:00.123Z` |
| `…:00.123456790Z` (1 ns later) | 200 | accepted, `…:00.123Z` |
| `…:00.123Z` (the displayed value) | 403 | shortening |

Cloud Storage therefore compared times at nanosecond precision while the
response showed the millisecond truncation (not rounding: `.123999999` is
displayed as `.123`). Writing back the displayed time of an Unlocked
retention counted as a shortening.

A second session (a Locked retention is irreversible, so it used a deadline
about two minutes ahead and waited it out) covered gRPC, Locked retention and
retained deletes. Messages, all `403` with reason `forbidden`:

- U: `The unlocked object retention cannot be removed and its retention period cannot be shortened without overriding unlocked retention intent and permission.`
- L: `The locked object retention cannot be removed. Its retention mode cannot be changed and its retention period cannot be shortened.`

| Request on a retained object | Without `overrideUnlockedRetention` | With `overrideUnlockedRetention=true` |
| --- | --- | --- |
| Unlocked to Locked, same time | 403 U | 200 (now Locked) |
| Locked, same time again | 200, metageneration advances | 200, metageneration advances |
| Locked, displayed millisecond value (456789 ns shorter) | 403 L | 403 L |
| Locked, 30 s shorter | 403 L | 403 L |
| Locked, `retention: null` | 403 L | 403 L |
| Locked, mode back to Unlocked | 403 L | 403 L |
| Locked, 1 ns later | 200 | 200 |
| `objects.delete` on a Locked or Unlocked retained object | 403 `retentionPolicyNotMet` | 403 `retentionPolicyNotMet` (the override is ignored on delete) |

The delete denial read `Object '<bucket>/<name>' is subject to bucket's
retention policy or object retention and cannot be deleted or overwritten
until 2026-10-31T10:03:59.123456793-07:00` for a retain-until time of
17:03:59.123456793Z: US Pacific time (daylight offset `-07:00` on that date),
the fraction at full nanosecond precision. An Unlocked object printed
`…T10:00:00.123456789-07:00` for 17:00:00.123456789Z.

Over gRPC (`GetObject`, googleapis `ff8fd20f3840386e12f4f69750eff6dc99f6e1b7`)
an Unlocked object set with `…00.123456789Z` returned
`retention.retain_until_time` with seconds and nanos `123456789`, and
`retention_expire_time` at millisecond precision (`…00.123Z`). `GetObject`
and a JSON `fields=retentionExpireTime` differ: the JSON selector is rejected
with 400, reason `invalidParameter`, message `Invalid field selection
retentionExpireTime` (the rejected name only), `locationType` `parameter`,
`location` `fields`.

### gcs-mimic behavior

| Observation | Local behavior | Test |
| --- | --- | --- |
| Display is the millisecond truncation | `retainUntilTime` is rendered truncated to milliseconds | `json::object_retention::tests::retain_until_displays_milliseconds_and_keeps_nanoseconds` |
| Comparison below the displayed millisecond | The time is kept to the nanosecond in memory, in the catalog (schema 47 column `retain_until_sub_micro_nanos`, 0 for rows written earlier, next to the microsecond `retain_until_time`) and in resumable session metadata (`retainUntilSubMicroNanos`, absent when zero). One nanosecond earlier and the displayed millisecond are refused, the same time and a later one are accepted, also after a restart | `retention_roundtrip_http::unlocked_retention_compares_below_the_displayed_millisecond`, `storage::catalog::objects::retention::tests::columns_split_a_time_into_microseconds_and_a_remainder`, `storage::catalog::objects::retention::tests::sub_micro_column_migrates_microsecond_rows_and_checks_its_range`, `storage::catalog::objects::retention::tests::session_value_keeps_the_remainder_and_reads_older_sessions` |
| The displayed time is a shortening | Unlocked needs `overrideUnlockedRetention=true`; Locked refuses it whatever the override | `object::retention::tests::echoing_the_displayed_millisecond_time_of_an_unlocked_value_is_a_shortening`, `object::retention::tests::echoing_the_displayed_millisecond_time_of_a_locked_value_is_refused_even_with_the_override` |
| Statuses and messages | Unlocked shortening, removal and locking without the override: 403 `forbidden` with message U (gRPC `PERMISSION_DENIED`). Locked shortening, removal and mode change, with or without the override: 403 `forbidden` with message L. The same time again is 200 | `retention_roundtrip_http::unlocked_retention_compares_below_the_displayed_millisecond`, `retention_roundtrip_http::locked_retention_refuses_every_reduction_with_403_and_accepts_equal_or_later`, `retention_roundtrip_http::locking_an_unlocked_retention_needs_the_override`, `storage::resumable_http_tests::object_retention::object_retention_mode_transitions_and_injected_clock_expiry`, `grpc::v2::update_copy::tests::grpc_update_object_retention` |
| Retained delete | 403 `retentionPolicyNotMet` naming the object and the retain-until time in US Pacific time to the nanosecond (trailing zeros of the fraction trimmed); objects.delete accepts and ignores `overrideUnlockedRetention`. The offset follows the US daylight-saving rule (second Sunday of March to first Sunday of November, 02:00 local) because Google's frontends use `America/Los_Angeles`; both `-07:00` and `-08:00` were observed | `retention_roundtrip_http::deleting_a_retained_object_names_it_and_ignores_the_override`, `object::retention::tests::pacific_time_follows_daylight_saving_and_trims_the_fraction` |
| gRPC | `retention.retain_until_time` keeps nanoseconds; `retention_expire_time` is truncated to milliseconds | `grpc::v2::update_copy::tests::grpc_retention_keeps_nanoseconds_and_expire_time_is_milliseconds` |
| JSON `fields=retentionExpireTime` | 400 `invalidParameter`, message naming the rejected field only, `locationType` `parameter`, `location` `fields` | `retention_roundtrip_http::fields_selection_rejects_the_grpc_retention_expire_time_name`, `json::fields::tests::fields_reject_malformed_unknown_and_scalar_children` |

Ledger row: `OBJECT-TIME-READ-BACK`.

## Bucket retention policy, bucket preconditions and gRPC retention

Observed **2026-11-01** with curl against the JSON API and grpcurl against
the gRPC API (googleapis `storage.proto`), using two throwaway buckets in
`us-central1` (STANDARD, `softDeletePolicy` 0) that were deleted afterwards
(each `buckets.get` then returned 404). One had
`enableObjectRetention=true`; the other had an unlocked bucket retention
policy of 90 seconds. The project, project number, bucket names and user
are not recorded. All errors are JSON `error` bodies with a `global` domain.

Retained deletes name the retain-until time in US Pacific time. The message
template is `Object '<bucket>/<name>' is subject to bucket's retention policy
or object retention and cannot be deleted or overwritten until <time>`:

| Object retention `retainUntilTime` set | Time in the message |
| --- | --- |
| `2026-11-03T00:00:00.123456789Z` | `2026-11-02T16:00:00.123456789-08:00` |
| `2026-11-03T00:00:00.12Z` | `2026-11-02T16:00:00.12-08:00` |
| `2026-11-03T00:00:00Z` | `2026-11-02T16:00:00-08:00` |

The winter offset is `-08:00`, the nanosecond fraction is kept in full,
trailing zeros are trimmed and a zero fraction is omitted. A bucket
retention policy of 90 seconds (the `PATCH` response `effectiveTime` was
about 60 seconds after the request, not the request time) on an object
created at `19:17:01.407263Z` gave `retentionExpirationTime`
`19:18:31.407Z`. The delete and an overwrite (media upload of the same name)
both returned 403 `retentionPolicyNotMet` with the identical message ending
`until 2026-10-31T12:18:31.407263-07:00`: the creation time plus the period
at microsecond precision, with the summer offset. `PATCH`
`retentionPolicy: null` removed the policy (200) although `effectiveTime` was
still in the future, and the object was then deleted (204).

Failed bucket preconditions (metageneration 1; the bucket stayed unchanged):

| Request | Status | Result |
| --- | --- | --- |
| `PUT` with `ifMetagenerationMatch=99` | 412 | reason `conditionNotMet`, message `At least one of the pre-conditions you specified did not hold.`, `locationType` `header`, `location` `If-Match` |
| `PATCH {"labels":{"k":"v"}}` with `ifMetagenerationMatch=99` | 412 | identical |
| `PATCH {"labels":{"k":"v"}}` with `ifMetagenerationNotMatch=1` (the current value) | 304 | empty body, `content-length: 0` |
| `PATCH {}` with `ifMetagenerationNotMatch=1` | 304 | same |

Removing (`retention: null`) or shortening an Unlocked object retention
without `overrideUnlockedRetention` returned 403 `forbidden` with the same
message U as above. Over gRPC:

- `UpdateObject` (mask `retention`) that shortened or removed an Unlocked
  retention without the override returned `PERMISSION_DENIED` with message U;
  with `override_unlocked_retention` the removal succeeded and the object had
  no `retention` or `retention_expire_time`.
- `DeleteObject` of the retained object returned `PERMISSION_DENIED` with the
  JSON message, microsecond time included.
- `GetObject` under the bucket retention policy returned
  `retention_expire_time` `…:31.407Z` for a creation time of `…:01.407263Z`
  (milliseconds) and no object-level `retention`.

`satisfiesPZI` was `true` in `us-central1` (`locationType` `region`,
`location` `US-CENTRAL1`), as in `asia-northeast1`.

### gcs-mimic behavior

| Observation | Local behavior | Test |
| --- | --- | --- |
| Winter offset, trailing zeros, zero fraction | `-08:00` outside daylight saving; the fraction is trimmed and omitted when zero | `object::retention::tests::pacific_time_follows_daylight_saving_and_trims_the_fraction`, `retention_roundtrip_http::deleting_a_retained_object_names_it_and_ignores_the_override` |
| Bucket policy denial | The same 403 `retentionPolicyNotMet` and message as object retention, with the creation time plus the period to the microsecond; delete and overwrite alike. `retentionPolicy: null` lifts it | `retention_roundtrip_http::bucket_retention_policy_denies_delete_and_overwrite_with_the_object_retention_wording` |
| Writing PATCH with `ifMetagenerationNotMatch` at the current value | 304, empty body (a PUT was not observed with it and stays 412) | `bucket_http::writing_patch_and_put_evaluate_metageneration_preconditions` |
| PUT and writing PATCH with a failed `ifMetagenerationMatch` | 412 `conditionNotMet`, location `If-Match`, native profile | `bucket_http::writing_patch_and_put_evaluate_metageneration_preconditions` |
| Unlocked removal and shortening without the override | 403 `forbidden`, message U | `retention_roundtrip_http::unlocked_removal_and_shortening_share_one_refusal_without_the_override` |
| gRPC `UpdateObject`, `DeleteObject`, bucket policy `retention_expire_time` | `PERMISSION_DENIED` with the JSON message; `retention_expire_time` truncated to milliseconds | `grpc::v2::update_copy::tests::grpc_update_object_retention`, `grpc::v2::update_copy::tests::grpc_bucket_policy_expire_time_is_milliseconds` |
| `satisfiesPZI` in another location | `true` in every location | `bucket_http::satisfies_pzi_is_true_in_every_location` |

Difference kept: gcs-mimic sets a bucket retention policy's `effectiveTime`
to the request time, not about 60 seconds later. Ledger rows:
`OBJECT-TIME-READ-BACK`, `BUCKET-SATISFIES-PZI`.

## XML listing

`GET /<bucket>?max-keys=5` returned `application/xml; charset=UTF-8`,
`x-goog-metageneration: 1`, `cache-control: private, max-age=0` and:

```xml
<?xml version='1.0' encoding='UTF-8'?><ListBucketResult xmlns='http://doc.s3.amazonaws.com/2006-03-01'><Name><bucket></Name><Prefix></Prefix><Marker></Marker><MaxKeys>5</MaxKeys><IsTruncated>false</IsTruncated><Contents><Key>plain.txt</Key><Generation>1793442589870383</Generation><MetaGeneration>1</MetaGeneration><LastModified>2026-10-31T10:29:49.875Z</LastModified><ETag>"dd2e843409823500b8dbd87f539398e5"</ETag><Size>16</Size></Contents></ListBucketResult>
```

(one of the two recorded `Contents` elements shown). No `Delimiter` was sent
without a delimiter request, and `Contents` carried no `StorageClass`.

### gcs-mimic behavior

| Observation | Local behavior | Test |
| --- | --- | --- |
| Document layout | Single-quoted declaration and `xmlns`; `Name`, `Prefix`, request markers and `MaxKeys` before `IsTruncated`; `Delimiter` only when requested; no `StorageClass` in a version 1 listing (version 2 and versions listings keep it, unverified) | `wire_parity_http::xml_listing_matches_the_recorded_document` |
| Bucket metageneration | `x-goog-metageneration` carries the bucket's current value, read by the same catalog job as the page | `wire_parity_http::xml_listing_matches_the_recorded_document` |
| Declaration and quotes | Every XML document the server writes (lists, errors, copy, multipart, POST responses) uses the single-quoted declaration; only the listing and the error document were recorded. Character data escapes `&`, `<` and `>` and leaves quotes literal | `wire_parity_http::xml_documents_use_the_single_quoted_declaration`, `xml_bucket_list_encodes_once_and_keeps_object_namespace_collisions` |

Remaining differences are tracked as `SPEC-RESPONSE-WIRE-REMAINING` in
`compatibility/gaps.json`.
