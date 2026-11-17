# Provider probes for open compatibility gaps

Recorded on **2026-11-13**. This evidence answers questions that open entries in
the [gap ledger](../../../compatibility/gaps.json) asked of Cloud Storage, and
records what gcs-mimic does after the follow-up changes. It covers only the
request shapes listed below; it does not establish unrestricted Cloud Storage
API parity. A third recording, on 2026-11-17, is described in the last section.

## Live recording

An opted-in differential run recorded 23 new cases against real Cloud Storage
in the project `<project>`, with the project and access token supplied only
through environment variables:

```sh
cargo xtask differential run --record <partial-reference>
```

- Requests sent to Cloud Storage: **329**, with **0** HTTP 429 responses.
- Regions: disposable buckets in us-central1, us-east1
  (`bkt-location-mixed-case`) and us-west1 (`bkt-location-us-west1`).
- Cleanup: every object and bucket the run created was deleted, and the run's
  cleanup check confirmed that nothing was left.
- Cost: the buckets held only empty or few-byte objects in free-tier US regions,
  with soft delete turned off. The run locked no retention policy and used no
  CMEK key, Pub/Sub topic or other billable resource. No Autoclass bucket held an
  object, so no Autoclass fee or transition applied.

The partial recording was merged into the committed
[reference](../../../compatibility/differential/reference/gcs.json), which now
holds **98 cases** and **1380 requests** across the recordings of 2026-11-01,
2026-11-02, 2026-11-04, 2026-11-09 and 2026-11-13. The case definitions are in
[compatibility/differential/cases](../../../compatibility/differential/cases).
Each case's recorded responses can be read with
`jq '.cases[] | select(.id == "<case>")' compatibility/differential/reference/gcs.json`.

## Cases and outcomes

"Changed" means gcs-mimic was changed after the recording to give the recorded
answer. "Matched" means it already did. "Inferred" marks behavior that shares
code with a recorded step but was not itself recorded.

| Case | Gap | Cloud Storage answer | gcs-mimic |
| --- | --- | --- | --- |
| `obj-undeclared-query-writes` | SPEC-JSON-UNKNOWN-QUERY-OBJECTS | An undeclared query parameter is ignored on objects.insert (media and multipart), get, media download, patch, update, copy, rewrite, compose and the object ACL list | Changed; both profiles ignore it on those methods. Unrecorded methods (objects.move, objects.restore, other ACL methods, bucket IAM and others) keep 400 invalidParameter |
| `obj-fields-selectors` | SPEC-FIELDS-EDGES | On objects: `fields=%20` gives `{}`, spaces around names are ignored, `metadata()` gives `{}`, `metadata/` is 400 `Invalid field selection metadata/`, `items/name` on one object is 400, `fields` is ignored with `alt=media`; list selectors with wildcards work | Changed for whitespace, `metadata()`, the message and `alt=media`; the rest matched. Non-object resources keep the strict grammar (inferred scope) |
| `obj-collection-validators` | SPEC-JSON-ETAG-COLLECTIONS | Object listing ignores `If-None-Match: *` and a bogus `If-Match`; object and bucket listings send no ETag; the object ACL listing sends one | Changed; object and bucket listings ignore both headers (bucket listing inferred) |
| `obj-mutation-not-match` | SPEC-008 | Not-match naming the current value is an empty 304 on object PATCH, PUT, DELETE, media upload and copy destination; a failing match wins with 412; a missing object is 404; a parse error wins over a failing condition; compose ignores the undeclared `ifGenerationNotMatch` | Changed. Rewrite, move, resumable finalization and restore destinations answer 304 by shared code (inferred). gRPC keeps FAILED_PRECONDITION |
| `bkt-put-not-match` | SPEC-008, SPEC-RESPONSE-WIRE-REMAINING | Bucket PUT, like PATCH, answers 304 for `ifMetagenerationNotMatch` naming the current value, and not-match wins over a failing `ifMetagenerationMatch` | Changed |
| `obj-metadata-output-only` | SPEC-015 | A body `generation` selects the generation (404 when missing); PUT ignores `size` and `md5Hash`; a body name or bucket that disagrees with the path is 400 with the parameter-agreement message; PATCH stores empty `contentLanguage`, `contentDisposition`, `contentEncoding` and `cacheControl` | Changed. Ignoring `id`, `selfLink` and `timeCreated` is inferred: they were sent only in the request that answered 404 |
| `obj-derived-inputs` | SPEC-DERIVED-METADATA, SPEC-REWRITE-EDGES | Whitespace body, `{"metadata":null}` and `{"metadata":{}}` inherit source metadata; `{"contentType":""}` replaces it; the path names the destination; unknown fields and a supplied size are ignored, and rewrite ignores `md5Hash`; a resource with `generation` but no name is 400 `You must specify an object name.`; a missing source is 404 before a failing destination condition | Changed for the blank and empty-map bodies, size, rewrite `md5Hash` and the name error; the rest matched |
| `obj-compose-inputs` | SPEC-COMPOSE-EDGES | Duplicate and self sources compose; no first-source contentType inheritance; unknown fields and a wrong destination crc32c are ignored; the path names the destination; output-only generation and size are ignored; a destination class that differs from the components is 400 | Changed for the destination name, generation, size and class check; the rest matched. Accepting a class equal to the components' is inferred |
| `obj-multipart-mime-edges` | SPEC-016 | Empty or null metadata contentType uses the media part's type; a base64 media part is decoded; an empty media part and LF-only lines are accepted; a non-JSON final part of three is 400 invalidPayloadTypes; a body without its closing delimiter is a text/plain 400 `Missing end boundary in multipart body.` | Changed for base64 (decoded while streaming, bounded) and the two errors; the rest matched |
| `obj-upload-hash-edges` | SPEC-010, SPEC-016 | Conflicting, repeated, upper-case and wrong-length X-Goog-Hash values and a conflicting md5 next to Content-MD5 are ignored on media uploads; the header is ignored on multipart; a wrong metadata crc32c is 400 | Matched |
| `obj-range-head` | SPEC-RANGE-FALLBACK | HEAD ranges answer 206 or 416 with Content-Range; a nonzero suffix of an empty object is 206 with `bytes 0-0/0`; the download endpoint serves ranges and the past Expires | Changed for the empty-object suffix and the download endpoint Expires; the rest matched |
| `obj-gzip-double` | SPEC-015 | An `application/gzip` object stored with `contentEncoding: gzip` is always served gunzipped, also with `Accept-Encoding: gzip`, with a Range and on XML GET | Changed |
| `obj-resumable-edges` | SPEC-RSM-EDGES, SPEC-RSM-DISCOVERY-PATH | A chunk past its declared total is 400 `Failed to parse Content-Range header.` and every later request gets the same, with no object; a cancelled session's status query is 499; the Discovery `/resumable/upload` path initiates a session | Changed for the overrun; the rest matched |
| `bkt-config-edges` | SPEC-SOFT-DELETE, SPEC-RETENTION-POLICY, SPEC-LIFECYCLE-CONFIG, SPEC-UBLA-EDGES | Soft delete accepts 7776000 s and refuses 7776001 s; retention 0, negative and above 100 years are refused; lifecycle drops an unknown condition field and refuses an empty condition, an unknown action and an abort rule with another condition; conflicting uniform access aliases enable access when the alias changes the stored setting | Changed for the soft delete ceiling, the lifecycle messages and unknown fields, and the alias rule; retention matched |
| `bkt-location-errors` | SPEC-REGIONAL-LOCATIONS | A padded, underscored, empty or zone location is 400 `The specified location constraint is not valid.` | Changed for the empty string; the rest matched |
| `bkt-location-mixed-case` | SPEC-REGIONAL-LOCATIONS, SPEC-RESPONSE-WIRE-REMAINING | `Us-East1` becomes US-EAST1, locationType region, satisfiesPZI true | Matched |
| `bkt-location-us-west1` | SPEC-RESPONSE-WIRE-REMAINING | US-WEST1 with satisfiesPZI true | Matched |
| `bkt-iam-policy-shape` | SPEC-IAM-WIRE, SPEC-TEST-PERMISSIONS | Bindings of one role merge at the first position with sorted, de-duplicated members; unpadded and non-zero-trailing-bit etags decode and answer 412; `!!!` is 400 `Invalid value for ByteString: !!!`; invalid permissions are 400 before a missing bucket's 404 | Changed for policy normalization and etag decoding; testIamPermissions matched |
| `bkt-autoclass-toggle` | none (Autoclass row) | Enabling with a label change bumps metageneration once; an invalid terminal class and a non-STANDARD class on an Autoclass bucket are refused with Cloud Storage's texts | Changed for the two refusal texts; the rest matched |
| `grpc-composite-checksum-mask` | SPEC-GRPC-CHECKSUM-SUBPATH | GetObject and ListObjects return an empty checksums message for a composite with an MD5-only mask; ReadObject without `checksummed_data.content` in its mask is INVALID_ARGUMENT with ErrorInfo `GRPC_READ_MASK_MISSING_CHECKSUMMED_DATA` | Changed for ReadObject; the rest matched |
| `grpc-empty-iam-update` | SPEC-GRPC-EMPTY-IAM-UPDATE | UpdateBucket with mask `iam_config` and an empty message advances the metageneration and resets public access prevention to inherited | Changed for public access prevention |
| `grpc-iam-config-partial-update` | SPEC-GRPC-EMPTY-IAM-UPDATE | Recorded later the same day (14 requests, no 429, cleanup confirmed) because the Go client sends one setting under the parent mask: a UBLA-only `iam_config` enables UBLA and resets enforced public access prevention to inherited; a PAP-only `iam_config` keeps the enabled UBLA | Changed: an absent UBLA message is now kept instead of disabled; the Go `TestGRPCUpdateCopy` expectation was corrected to the recorded reset |
| `xml-response-override-edges` | SPEC-XML-RESPONSE-OVERRIDES | An empty override is 400 InvalidArgument naming the parameter; a padded content type is trimmed; a non-ASCII disposition passes through; `response-cache-control`, `response-expires` and unknown `response-*` are ignored; HEAD applies the override | Changed |
| `obj-list-edges` | SPEC-LIST-PAGING, SPEC-LIST-RESOURCE-SHAPE, SPEC-LIST-GLOB-EDGES | A page token continues after its name under another prefix, delimiter or versions flag; multi-character and multibyte delimiters work; empty delimited listings omit empty fields; glob syntax errors and a non-slash delimiter have provider texts; a trailing backslash lists nothing | Changed for positional tokens, the trailing backslash and the glob texts; the rest matched. XML continuation tokens share the change but were not recorded |

Two steps did not test what their names say. In `obj-mutation-not-match`, the
compose step before `rewrite-source-not-match-current` created a new
generation, so that step named a superseded source generation and its 200 says
nothing about a source not-match. In `obj-resumable-edges`, the session had
already failed when `empty-finalize-with-total` and
`chunk-after-completion-other-total` ran, so they show only the failed session's
400.

The resulting gap states are in the [gap ledger](../../../compatibility/gaps.json):
SPEC-GRPC-CHECKSUM-SUBPATH and SPEC-GRPC-EMPTY-IAM-UPDATE are resolved; the
other gaps above stay open with the remaining questions they name.

## Second recording: buckets, HMAC keys and notifications

A second opted-in run on 2026-11-13 recorded three new cases, with the project
and access token again supplied only through environment variables:

```sh
cargo xtask differential run --filter bkt-edges- --record <partial-reference>
cargo xtask differential merge compatibility/differential/reference/gcs.json \
  <partial-reference> --add-new --out <merged-reference>
```

- Requests sent to Cloud Storage: **40**, with **0** HTTP 429 responses.
- Resources: the harness's shared bucket and one bucket of its own, both empty
  in us-central1 with soft delete off. No object, HMAC key, Pub/Sub topic or
  notification configuration was created; the harness refuses to create an HMAC
  key because the response carries its secret.
- Cleanup: both buckets were deleted and confirmed 404, and the run's final
  listing found nothing left.
- Cost: only metadata requests on empty free-tier buckets.

| Case | Gap | Cloud Storage answer | gcs-mimic |
| --- | --- | --- | --- |
| `bkt-edges-fields` | SPEC-FIELDS-EDGES | On a bucket, the bucket list and the IAM policy: `fields=%20` gives `{}`; spaces around names and an empty segment are ignored; `labels()` gives `{}`; `na me` is 400 invalidParameter `Invalid field selection na me` and `*/name` is 400 invalidParameter `Leading and nested wildcards are unsupported in field selection: */name`, both at `fields`; `labels/` and `items/name` on one bucket are invalid selections | Changed: every resource now uses the lenient grammar, and the two rejections use the recorded reason and wording on every resource (objects inferred). An empty `fields` value selects nothing on every resource (non-object resources inferred) |
| `bkt-edges-hmac-keys` | SPEC-HMAC-WIRE, SPEC-SDK-UNSUPPORTED | An empty listing is the kind alone without `items`; `maxResults=-1` is 400 invalidParameter `Invalid unsigned integer value: '-1'.`; a body without `state` is 400 invalid `Must specify resource.state.` and `DELETED` is 400 invalid `Cannot set state to 'DELETED'.`, both before the key is looked up; `userProject` is served like an absent parameter | Changed for the empty listing, the maxResults text and the two state errors. `userProject` was first kept rejected as a declared `local_choice`, then ignored on every API the same day (SPEC-SDK-UNSUPPORTED), and the `local_choice` markers were removed |
| `bkt-edges-notification-ids` | SPEC-NOTIFICATION-WIRE, SPEC-SDK-UNSUPPORTED | A non-numeric (`abc`) or zero-padded (`01`) ID is 404 notFound on get and delete; `userProject` is served like an absent parameter | Matched for the IDs. `userProject` was first kept rejected as a declared `local_choice`, then ignored on every API the same day, and the markers were removed |

## Replay and checks

The replay of the merged reference sends no Google request:

```sh
cargo xtask differential run \
  --reference compatibility/differential/reference/gcs.json \
  --fail-on-diff --fail-on-note
```

Result on **2026-11-13**: all **98** cases `match`, with no differing fields,
notes or errors. After the second recording the reference holds **102** cases
and **1434** requests, and a replay of the changed source on 2026-11-13 found
all 102 cases `match` with no differing fields, notes or errors. The comparison report in
[differential-comparison.md](differential-comparison.md) was not regenerated by
this run and still describes the 2026-11-07 replay.

The same source passed the workspace tests and the production lint gate on
2026-11-13:

```sh
cargo test --locked --workspace --no-fail-fast
cargo xtask lint
```

`cargo test` exited 0 with no failed test, and `cargo xtask lint` reported 0 new
production violations.

## Third recording: questions the ledger still held

Recorded on **2026-11-17**. Six new cases sent the questions that open ledger
entries still asked about implemented features, with the project and access
token again supplied only through environment variables:

```sh
cargo xtask differential run --filter -gaps- --record <partial-reference>
cargo xtask differential merge compatibility/differential/reference/gcs.json \
  <partial-reference> --add-new --out compatibility/differential/reference/gcs.json
```

- Requests sent to Cloud Storage: the first run sent **116** and could not be
  merged, because Cloud Storage refused a resumable initiation whose session a
  later step captured (that refusal was the answer the step asked for), so the
  rest of that case did not run. After the step stopped capturing, a second run
  of all six cases sent **119**. Neither run received an HTTP 429.
- Resources per run: the harness's shared bucket, one bucket of its own and one
  bucket a case created, all in us-central1 with soft delete off and objects of
  a few bytes. The customer-supplied keys are fixed test values. No CMEK key,
  Pub/Sub topic, HMAC key or retention lock was used.
- Cleanup: every bucket was deleted and confirmed 404, and the final listing of
  each run found nothing left.

The reference now holds **130** cases and **1962** requests. "Declared" below
means the step carries a `local_choice` whose reason names this recording and
the gap, so the replay stays green while the difference stays visible; a marker
is removed when gcs-mimic is aligned.

| Case | Gap | Cloud Storage answer | gcs-mimic |
| --- | --- | --- | --- |
| `obj-gaps-json` | SPEC-METADATA-IF-MATCH | If-Match on an object metadata PATCH or PUT is ignored: the current etag, a superseded etag and `"bogus"` all answer 200 and apply the change | Matched since aligned (was 412 for a non-matching value) |
| `obj-gaps-json` | SPEC-SOFT-DELETE | On a bucket with soft delete off, a `softDeleted` listing, a `softDeleted` get and a restore are 400 invalid: `Soft delete policy is required to list soft-deleted versions`, `Soft delete policy is required to request a soft-deleted version.` and `bucket soft delete policy must be set.` | Matched since aligned (was an empty listing, 404 and 404) |
| `obj-gaps-json` | SPEC-LIST-GLOB-EDGES | The globs `[/-]`, `[!x]` and `?` never match a slash | Matched |
| `obj-gaps-json` | SPEC-RUBY-DEFAULTS | Compose with `deleteSourceObjects: null` or `destination: null` succeeds and keeps the source | Matched |
| `obj-gaps-json` | SPEC-015 | An empty `contentLanguage` is stored by a multipart upload and accepted by a resumable initiation; a multipart upload also stores empty `contentDisposition`, `contentEncoding` and `cacheControl`, and a copy keeps all four (follow-up run); a media HEAD of a gzip-stored object answers like the GET without a body | Matched since aligned (was 400 for the empty `contentLanguage`, and a copy dropped the empty fields). The HEAD matched; its body framing is ignored as in `obj-range-head` |
| `obj-gaps-uploads` | SPEC-RSM-EDGES | A wrong `Content-MD5` on the final chunk is ignored; DELETE on a completed session is 200 with the object; an initiation whose metadata `contentType` disagrees with `X-Upload-Content-Type` is a text/html 400 invalid | Matched for `Content-MD5`; the DELETE (was 410) and the disagreeing initiation (was 200) matched since aligned |
| `obj-gaps-uploads` | SPEC-RSM-AUTH-CONDITIONS | A chunk PUT ignores an unknown query parameter and `ifGenerationMatch` | Matched |
| `obj-gaps-uploads` | SPEC-016 | Media parts with transfer encoding 7bit, 8bit and quoted-printable are stored as sent, without decoding | Matched since aligned (was 400 invalidParameter) |
| `xml-gaps-objects` | SPEC-CROSS-PROTOCOL-EXCLUSIONS | A version 2 listing with `fetch-owner=true` is 200 with an Owner for each object | Differs: 400 InvalidArgument (declared) |
| `xml-gaps-objects` | SPEC-XML-PATH-ERROR | An invalid bucket segment is XML 400 InvalidBucketName; one with a percent-encoded slash is XML 404 NoSuchBucket | Matched since aligned (was the JSON 404 unknown-route error) |
| `xml-gaps-objects` | SPEC-XML-INPUTS | A non-ASCII metadata value is served as raw UTF-8; the keys `Dup` and `dup` become one `x-goog-meta-dup: 1, 2`; the XML ETag of a composite or customer-encrypted object is not its JSON etag, nor for the encrypted one the plaintext MD5; an XML HEAD of an encrypted object without its key is 400 | Matched for the non-ASCII value; the colliding keys (was 400) and the keyless HEAD (was 200) matched since aligned. Differs for both entity tags (200 with the JSON etag) and the headers and Details of the encrypted object's 412 (declared) |
| `bkt-gaps-settings` | SPEC-UBLA-EDGES | On a bucket with uniform access, the alias whose value changes the stored setting wins in both directions; an upload with `predefinedAcl=private` is 400 invalid | Matched for the aliases; the upload matched since aligned (was 400 invalidArgument with local guidance) |
| `bkt-gaps-settings` | SPEC-LIFECYCLE-CONFIG | A `matchesPattern` condition is 400 invalid `MatchesPattern is not available for this project <project-number>` | Differs in reason and message (declared) |
| `bkt-gaps-autoclass-off` | SPEC-RUBY-DEFAULTS | A bucket created with `autoclass.enabled` false carries no `autoclass` | Matched |
| `grpc-gaps-bidi-read` | SPEC-GRPC-PENDING-RANGES | PERMISSION_DENIED with reason `GRPC_BIDI_READ_OBJECT_NOT_ALLOWLISTED`: BidiReadObject is not available to the recording project | Differs: the RPC is served (declared). The pending-range question stays open |

Reading the reference for this run also showed ledger questions that earlier
cases had already answered, and the ledger now says so: a past
`retainUntilTime` is 400 (`obj-retention-unlocked`), a mismatching XML CRC32C is
400 CrcMismatch (`xml-put-edges`), testIamPermissions without `permissions` is
400 (`bkt-iam-pap-ubla`), `Range: bytes=abc` serves the whole object
(`obj-download-range`), and a private XML object's default Cache-Control is
`private, max-age=0` (`xml-object-crud`). gcs-mimic matches each in replay.

Questions this run did not send, and why:

- The harness refuses retention locks, HMAC key creation and public access, so
  those answers under SPEC-RETENTION-POLICY, SPEC-HMAC-WIRE and SPEC-PAP-EDGES
  stay unrecorded.
- It sends every request with the measuring account's token, so anonymous,
  invalid-credential and less-privileged callers (SPEC-017) stay unrecorded.
- It has no step that waits, so lifecycle and Autoclass execution, configuration
  propagation, hold-release timing and session expiry stay unrecorded.
- A soft-deleted bucket would outlive the run, so SPEC-BUCKET-SOFT-DELETE was
  not probed.
- Questions that need thousands of objects or members, concurrent writers or
  large copies were left out for cost.

A replay of the merged reference on 2026-11-17 with `--fail-on-diff
--fail-on-note` found all 130 cases `match`, the declared steps reported as
local choices. No gcs-mimic source changed in this run.

### Alignment the same day

gcs-mimic was then aligned with the answers the table marks "since aligned",
and each aligned step lost its `local_choice` marker. One follow-up run recorded
`obj-gaps-json` again with three more steps (empty content fields on a
multipart upload, the same PATCH, and a copy) and sent **45** requests; the
reference then held **130** cases and **2007** requests. A second follow-up run
sent **43** requests for an XML probe of empty `Cache-Control`,
`Content-Disposition`, `Content-Language` and `Content-Encoding` headers and was
not merged: the harness passes headers to curl, which drops a header whose value
is empty, so neither side received them and the question stays open. Neither
run received an HTTP 429, and both deleted every bucket they created.
