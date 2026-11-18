# Provider probes for open compatibility gaps

Recorded on **2026-11-13**. This evidence answers questions that open entries in
the [gap ledger](../../../compatibility/gaps.json) asked of Cloud Storage, and
records what gcs-mimic does after the follow-up changes. It covers only the
request shapes listed below; it does not establish unrestricted Cloud Storage
API parity. Later recordings, on 2026-11-17 and 2026-11-18, are described in the last sections.

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

## Fourth recording: derived-write options

Recorded on **2026-11-17**. Aligning copy, rewrite and compose with the
2026-11-13 [unsupported-option probes](unsupported-option-probes.md) left three
questions those probes did not ask: a destination retention in a bucket with
object retention, `destinationPredefinedAcl` under uniform access, and what an
objects.patch `predefinedAcl` does to the ACL and under uniform access. New
steps in three existing cases asked them, with the project and access token
again supplied only through environment variables:

```sh
cargo xtask differential run --cases <directory-with-the-three-cases> \
  --record <partial-reference>
cargo xtask differential merge compatibility/differential/reference/gcs.json \
  <partial-reference> --out compatibility/differential/reference/gcs.json
```

- Requests sent to Cloud Storage: **76** for the three cases, then **29** for
  `bkt-gaps-settings` alone, with no HTTP 429. In the first run uniform access
  left the project owners without object read or update permission, and Cloud
  Storage answered the copy, rewrite, compose and PATCH with 403 before it
  looked at the ACL. The case now first gives the project owners
  `roles/storage.objectAdmin` on its bucket, and the second run's recording of
  it replaced the first. One more run of that case sent its requests but wrote
  no recording, and stopped before its report printed a count: the harness
  learned the project number when it created the case bucket but did not hide
  it in later messages, so its leak check refused the file. The harness now
  hides such a number in every later step.
- Resources: the harness's shared bucket, one bucket of its own and one bucket
  created with `enableObjectRetention`, in us-central1 with soft delete off and
  objects of a few bytes. Each retention was Unlocked and was removed with
  `overrideUnlockedRetention` before its object was deleted; no retention was
  locked.
- Cleanup: every bucket was deleted and confirmed 404, each run's final listing
  found nothing left, and a listing of the project afterwards showed only its
  three buckets that predate the harness.

The reference now holds **130** cases and **2112** requests.

| Case | Question | Cloud Storage answer | gcs-mimic |
| --- | --- | --- | --- |
| `obj-retention-unlocked` | An Unlocked retention on a copy, rewrite and compose destination in a bucket with object retention | 200, and each destination holds the retention | Matched since aligned (was 400 `badRequest`) |
| `bkt-gaps-settings` | `destinationPredefinedAcl=private` on a copy, rewrite and compose under uniform access | 400 `invalid` `Cannot insert legacy ACL for an object when uniform bucket-level access is enabled.` and a documentation link, as for an upload | Matched since aligned (was 400 `invalidArgument` with guidance) |
| `bkt-gaps-settings` | objects.patch with `predefinedAcl=private` under uniform access | 400 `invalid` `Cannot update access control for an object when uniform bucket-level access is enabled.` and a documentation link | Matched since aligned (was 400 `badRequest`, `predefinedAcl` unsupported on a PATCH) |
| `obj-unsupported-inputs` | The ACL after objects.patch with `predefinedAcl=private` | The owner's OWNER grant alone | Matched since aligned (the PATCH was refused and the ACL kept its project grants). The PATCH compared without `metageneration` until gcs-mimic stored the empty key ([fifth recording](#fifth-recording-empty-custom-metadata-key)): the refused empty-key PATCH before it had left the count one behind |

The 2026-11-13 steps of `obj-unsupported-derived` for `temporaryHold`,
`customTime`, `destinationPredefinedAcl` and a retention in a bucket without
object retention now match as well, and their markers were removed.

## Fifth recording: empty custom metadata key

Recorded on **2026-11-17**. The 2026-11-13
[unsupported-option probes](unsupported-option-probes.md) showed that Cloud
Storage stores an empty custom metadata key from a JSON multipart insert and an
objects.patch. They did not show how the key reads back over the XML API and
Storage v2, whether those protocols accept it on a write, or what a Storage v2
`metadata` update mask does with it. The new case `obj-gaps-empty-metadata-key`
asked, with the project and access token again supplied only through
environment variables:

```sh
cargo xtask differential run --cases <directory-with-the-case> \
  --record <partial-reference>
cargo xtask differential merge compatibility/differential/reference/gcs.json \
  <partial-reference> --add-new --out compatibility/differential/reference/gcs.json
```

- Requests sent to Cloud Storage: **22**, **23** and **25** in three runs of
  the case, with no HTTP 429. The first run showed that an UpdateObject with the
  `metadata` mask kept a stored key the request did not name. The second added
  an update with another key, to tell a merge from special handling of the
  empty key, and the third added the `metadata.` path and an empty map. Only the
  third run's recording was merged.
- Resources: the harness's shared bucket and four objects of one byte.
- Cleanup: every object and bucket was deleted and confirmed 404, and each
  run's final listing found nothing left.

The reference now holds **131** cases and **2137** requests.

| Step | Request | Cloud Storage answer | gcs-mimic |
| --- | --- | --- | --- |
| `json-insert-empty-key` | JSON multipart insert with metadata `{"":"v","k":"w"}` | 200, both keys stored | Matched since aligned (was 400 `badRequest`) |
| `xml-get-empty-key` | XML GET of that object | 200 with `x-goog-meta-: v` and `x-goog-meta-k: w` | Matched since aligned |
| `grpc-get-empty-key` | Storage v2 GetObject with read mask `name,metadata` | Metadata `{"":"v","k":"w"}` | Matched since aligned |
| `json-copy-empty-key` | JSON copy whose body sets metadata `{"":"c"}` | 200, the destination holds `{"":"c"}` alone | Matched since aligned (was 400 `badRequest`) |
| `json-patch-remove-empty-key` | objects.patch with `{"metadata":{"":null}}` | 200, the key is removed and no metadata remains | Matched since aligned (was 400 `badRequest`) |
| `grpc-update-empty-key` | UpdateObject, mask `metadata`, metadata `{"":"g"}` | `{"":"g","k":"w"}`: the stored `k` stays | Matched since aligned (was UNIMPLEMENTED) |
| `grpc-update-metadata-mask-other-key` | UpdateObject, mask `metadata`, metadata `{"n":"1"}` | `{"":"g","k":"w","n":"1"}`: the mask merges | Matched since aligned (gcs-mimic replaced the map) |
| `grpc-update-empty-key-path` | UpdateObject, mask `metadata.`, metadata `{"":"d"}` | `{"":"d","k":"w","n":"1"}` | Matched since aligned (the path was UNIMPLEMENTED) |
| `grpc-update-metadata-mask-empty-map` | UpdateObject, mask `metadata`, no metadata | The map is cleared | Matched since aligned |
| `grpc-write-empty-key` | WriteObject with metadata `{"":"v"}` | The object holds `{"":"v"}` | Matched since aligned (was UNIMPLEMENTED) |
| `xml-put-empty-key` | XML PUT with a bare `x-goog-meta-: v` header | 200 | Matched since aligned (was 400 `InvalidArgument` `Custom metadata requires a key.`) |
| `json-get-xml-put` | JSON GET of that object | Metadata `{"":"v"}` | Matched since aligned |

gcs-mimic now stores an empty key on every write that carries custom metadata
and merges a Storage v2 `metadata` mask into the stored map, clearing it when
the request names no key. The pinned Go SDK 1.71.0 sends that bare path only
with an empty map, to delete every key, and `metadata.KEY` paths otherwise. The
two `obj-unsupported-inputs` markers for the empty key were removed, and that
case's objects.patch `predefinedAcl` step compares its `metageneration` again.
An empty key on a JSON PUT, a resumable initiation, a rewrite or compose
destination, a Storage v2 ComposeObject or RewriteObject destination and the
legacy Storage v1 adapter is inferred from the recorded writes that share its
parsing. An XML POST policy form still refuses a bare `x-goog-meta-` field, and
a Storage v2 bucket `labels` mask still replaces the labels; no recording covers
either.

## Sixth recording: Storage v2 request checksums on derived writes

Recorded on **2026-11-17**. The 2026-11-13 case `grpc-unsupported-options`
showed that Cloud Storage refuses a wrong `object_checksums` CRC32C on
ComposeObject and RewriteObject with INVALID_ARGUMENT, where gcs-mimic rejected
the field as UNIMPLEMENTED. It did not show what a matching checksum does, how
an MD5 is treated, or whether a refused compose leaves an object. The new case
`grpc-gaps-derived-checksums` asked, against a source holding `hello` (CRC32C
2591144780, MD5 `XUFAKrxLKna5cZ2REBfFkg==`), with the project and access token
again supplied only through environment variables:

```sh
cargo xtask differential run --cases <directory-with-the-case> \
  --record <partial-reference>
cargo xtask differential merge compatibility/differential/reference/gcs.json \
  <partial-reference> --add-new --out compatibility/differential/reference/gcs.json
```

- Requests sent to Cloud Storage: **27** in each of two runs, with no HTTP 429.
  In the first run the four RewriteObject steps sent an
  `x-goog-request-params` naming only `bucket`, and Cloud Storage refused them
  at routing with INVALID_ARGUMENT before reading the checksums. The case was
  corrected to name `source_bucket` and `bucket`, as the other rewrite cases do,
  and run again; only the second run's recording was merged.
- Resources: the harness's shared bucket and five-byte objects.
- Cleanup: every object and bucket was deleted and confirmed 404, and each
  run's final listing found nothing left.

The reference now holds **132** cases and **2164** requests.

| Step | Request | Cloud Storage answer | gcs-mimic |
| --- | --- | --- | --- |
| `compose-matching-crc32c` | ComposeObject, CRC32C 2591144780 | Completed, `componentCount` 1 | Matched since aligned (was UNIMPLEMENTED) |
| `compose-matching-md5` | ComposeObject, the source's MD5 | INVALID_ARGUMENT `Invalid argument.` | Matched since aligned (was UNIMPLEMENTED) |
| `compose-wrong-md5` | ComposeObject, MD5 of zeros | INVALID_ARGUMENT `Invalid argument.` | Matched since aligned (was UNIMPLEMENTED) |
| `compose-wrong-crc32c` | ComposeObject, CRC32C 1 | INVALID_ARGUMENT `The CRC32C you specified did not match what we computed.` | Matched since aligned (was UNIMPLEMENTED) |
| `get-after-wrong-crc32c` | GetObject of that destination | NOT_FOUND: nothing was published | Matched |
| `rewrite-matching-crc32c` | RewriteObject, CRC32C 2591144780 | Completed, `done` | Matched since aligned (was UNIMPLEMENTED), apart from `resource.contentType` |
| `rewrite-matching-md5` | RewriteObject, the source's MD5 | Completed, `done` | Matched since aligned (was UNIMPLEMENTED), apart from `resource.contentType` |
| `rewrite-wrong-md5` | RewriteObject, MD5 of zeros | INVALID_ARGUMENT `The MD5 you specified in Content-MD5 or x-goog-hash did not match what we computed.` | Matched since aligned (was UNIMPLEMENTED) |
| `rewrite-short-md5` | RewriteObject, a 3-byte MD5 | INVALID_ARGUMENT `Provided value (AAAA) is not a base64-encoded 128-bit MD5 hash.` | Matched since aligned (was UNIMPLEMENTED) |

Every refusal carried a `google.rpc.BadRequest` whose one field violation
repeats the message, and no refused request wrote a destination. The JSON API
rewrite ignores a supplied `md5Hash` (recorded 2026-11-13), so the MD5 check is
specific to Storage v2.

gcs-mimic now verifies `object_checksums` on both RPCs. ComposeObject checks a
declared CRC32C against the CRC32C combined from its sources before any byte is
read and refuses any MD5, which a composite object does not have. RewriteObject
checks the MD5's length, then the declared CRC32C and MD5 against the source's
stored hashes before a session starts, and binds them into the rewrite token
after `destinationPredefinedAcl` (`objectChecksums.crc32c`,
`objectChecksums.md5Hash`), so a continuation that repeats other checksums is
refused as a token mismatch. The two `grpc-unsupported-options` markers for the
wrong checksum were removed, and `kms_key` and `destination_kms_key` stay
UNIMPLEMENTED. A request with both a wrong CRC32C and a wrong MD5 reports the
CRC32C first, following the upload order. A rewrite whose source shows no MD5,
such as a composite object, refuses a declared MD5 with `Invalid argument.` as
a compose does. Neither is recorded.

A derived write now keeps the digests a client declared apart from the digests
recorded for the bytes it copies. Every copy, rewrite and compose, over JSON, XML
and Storage v2, checks that the copied bytes reproduce the source's recorded
digests, and a failure there is reported as damaged stored data (JSON 503
`backendError`, Storage v2 UNAVAILABLE) rather than as a checksum the client
specified. Only a declared digest fails as the client's error: on a JSON copy,
a JSON rewrite's CRC32C and a resumable upload's final check, the message is
the recorded multipart upload wording,
`Provided CRC32C "…" doesn't match calculated CRC32C "…".`, which replaces the
generic `The supplied checksum does not match the uploaded data`; Cloud Storage's
wording for those paths is not recorded.

One difference outside the checksums remains open. Cloud Storage answered the
two completed RewriteObject calls without `resource.contentType`, although the
source had `text/plain`, as it did for the `grpc-csek` rewrites and the
`rewrite-custom-time` step; gcs-mimic copies the source's value. Rewrites in
`grpc-unsupported-options` of a source updated through a wildcard mask did carry
`contentType`. The two new rewrite steps compare without it; no recording yet
explains when a Storage v2 rewrite drops it.

## Seventh recording: legacy storage classes and bucket location

Recorded on **2026-11-17**. The 2026-11-13 case `bkt-unsupported-settings`
showed that Cloud Storage stores the legacy `REGIONAL` class on a regional
bucket, in a lifecycle action and on an object, where gcs-mimic refused it as
needing a region or zone. It did not show where `REGIONAL` is refused, what
`MULTI_REGIONAL` does in a region, or how the XML API and Storage v2 answer.
Four new cases asked, with the project and access token again supplied only
through environment variables:

```sh
cargo xtask differential run --filter bkt-gaps-legacy-class- \
  --record <partial-reference>
cargo xtask differential merge compatibility/differential/reference/gcs.json \
  <partial-reference> --add-new --out compatibility/differential/reference/gcs.json
```

- Requests sent to Cloud Storage: **91** in one run, with no HTTP 429.
- Resources: the harness's shared bucket; its own us-central1 bucket for
  `bkt-gaps-legacy-class-region`; and the buckets the cases created, one in
  us-central1 with the `REGIONAL` class and one in `US`. Every other creation
  was refused. Objects held one byte, in classes priced as Standard storage
  without a minimum storage duration.
- Cleanup: every object and bucket was deleted and confirmed 404, and the
  final listing found nothing left.

The reference now holds **136** cases and **2255** requests.

Every refusal of a class the bucket location cannot hold carried the same text,
`Project <project-number> may not create storageClass <CLASS> buckets with
locationConstraint <LOCATION>.`, called *the conflict* below. Over JSON it was
400 `invalid`; over XML it was 400 `InvalidLocationConstraintStorageClassCombination`
with the Message `The combination of location and storage class specified is
not supported for your project. In particular, regional locations do not
support Multi-Regional Storage, and multi-regional locations do not support
Regional Storage.` and the conflict as Details; over Storage v2 it was
INVALID_ARGUMENT with a `google.rpc.BadRequest` repeating it.

In the harness's regional bucket (`bkt-gaps-legacy-class-region`):

| Step | Request | Cloud Storage answer | gcs-mimic |
| --- | --- | --- | --- |
| `patch-class-multi-regional` | PATCH `storageClass` `MULTI_REGIONAL` | The conflict for `US-CENTRAL1` | Matched since aligned (was 200) |
| `patch-class-dra` | PATCH `DURABLE_REDUCED_AVAILABILITY` | 200 | Matched |
| `patch-class-regional`, `get-bucket-class` | PATCH `REGIONAL`, then GET | 200, `REGIONAL`, `locationType` `region` | Matched since aligned (was 400 `badRequest`) |
| `upload-default-class` | Multipart insert naming no class | 200, `REGIONAL` from the bucket default | Matched since aligned |
| `upload-class-multi-regional` | Multipart insert with `MULTI_REGIONAL` | The conflict, the JSON envelope sent as `text/html` | Matched since aligned (was 200) |
| `xml-put-class-regional`, `xml-head-class-regional` | XML PUT with `x-goog-storage-class: REGIONAL`, then HEAD | 200; HEAD shows `x-goog-storage-class: REGIONAL` | Matched since aligned (was 400 `InvalidArgument`) |
| `xml-put-class-multi-regional` | XML PUT with `MULTI_REGIONAL` | The XML conflict | Matched since aligned (was 200) |
| `rewrite-to-regional` | JSON rewrite of a `STANDARD` object to `REGIONAL` | Done, `REGIONAL` | Matched since aligned (was 400 `badRequest`) |
| `rewrite-to-multi-regional` | JSON rewrite to `MULTI_REGIONAL` | The conflict | Matched since aligned (was done) |
| `rewrite-regional-to-standard` | JSON rewrite of a `REGIONAL` object to `STANDARD` | Done, `STANDARD` | Matched since aligned |
| `compose-default-class` | Compose of a `STANDARD` and a `REGIONAL` object, no class named | 400 `invalid` `Cannot compose object with destination storage class STANDARD from source components that have different storage classes (REGIONAL).` | Matched since aligned (the message was `Compose sources must use the same storage class`) |
| `lifecycle-set-class-multi-regional` | SetStorageClass `MULTI_REGIONAL` from `DURABLE_REDUCED_AVAILABILITY` | The conflict | Matched since aligned (gcs-mimic stored the rule, inert) |
| `lifecycle-set-class-regional-from-dra` | SetStorageClass `REGIONAL` from `DURABLE_REDUCED_AVAILABILITY` | 200, stored | Matched since aligned (was 400 `badRequest`) |
| `lifecycle-match-regional` | SetStorageClass `NEARLINE` for `matchesStorageClass` `REGIONAL` | 200, stored | Matched |
| `lifecycle-match-multi-regional` | Delete for `matchesStorageClass` `MULTI_REGIONAL` | The conflict | Matched since aligned (gcs-mimic used its own wording) |
| `grpc-write-class-regional` | WriteObject with `storage_class` `REGIONAL` | Completed, `REGIONAL` | Matched since aligned (was UNIMPLEMENTED) |
| `grpc-write-class-multi-regional` | WriteObject with `MULTI_REGIONAL` | The Storage v2 conflict | Matched since aligned (was completed) |
| `grpc-get-object-class`, `grpc-get-bucket-class` | GetObject and GetBucket with read masks | `REGIONAL` | Matched since aligned |
| `grpc-update-bucket-class-multi-regional` | UpdateBucket, mask `storage_class`, `MULTI_REGIONAL` | The Storage v2 conflict | Matched since aligned (was completed) |
| `grpc-update-bucket-class-regional`, `grpc-get-bucket-class-after-update` | UpdateBucket to `REGIONAL`, then GetBucket | Completed, `REGIONAL` | Matched since aligned (was UNIMPLEMENTED), apart from fields named below |
| `grpc-rewrite-class-regional` | RewriteObject with destination `storage_class` `REGIONAL` | Done, `REGIONAL` | Matched since aligned (was UNIMPLEMENTED) |
| `grpc-rewrite-class-multi-regional` | RewriteObject to `MULTI_REGIONAL` | The Storage v2 conflict | Matched since aligned (was done) |

In a `US` bucket the case created (`bkt-gaps-legacy-class-multi-region`):

| Step | Request | Cloud Storage answer | gcs-mimic |
| --- | --- | --- | --- |
| `create-us-regional` | Bucket insert in `US` with `REGIONAL` | The conflict for `US` | Matched since aligned (was 400 `badRequest`) |
| `patch-class-regional` | PATCH `REGIONAL` | The conflict | Matched since aligned (was 400 `badRequest`) |
| `upload-class-regional` | Multipart insert with `REGIONAL` | The conflict, sent as `text/html` | Matched since aligned (was 400 `badRequest`) |
| `xml-put-class-regional` | XML PUT with `REGIONAL` | The XML conflict | Matched since aligned (was 400 `InvalidArgument`) |
| `rewrite-to-regional` | JSON rewrite to `REGIONAL` | The conflict | Matched since aligned (was 400 `badRequest`) |
| `compose-class-regional` | Compose of a `STANDARD` object naming `REGIONAL` | 400 `invalid` `Cannot compose object with destination storage class REGIONAL from source components that have different storage classes (STANDARD).`, not the conflict | Matched since aligned (was 400 `badRequest`) |
| `lifecycle-set-class-regional`, `lifecycle-match-regional` | SetStorageClass `REGIONAL`; Delete for `matchesStorageClass` `REGIONAL` | The conflict | Matched since aligned |
| `lifecycle-set-class-multi-regional` | SetStorageClass `MULTI_REGIONAL` from `DURABLE_REDUCED_AVAILABILITY` | 200, stored | Matched |
| `grpc-write-class-regional`, `grpc-update-bucket-class-regional`, `grpc-rewrite-class-regional` | WriteObject, UpdateBucket and RewriteObject naming `REGIONAL` | The Storage v2 conflict | Matched since aligned (was UNIMPLEMENTED) |
| `patch-class-multi-regional`, `upload-default-class` | PATCH `MULTI_REGIONAL`, then a multipart insert naming no class | 200; the object is `MULTI_REGIONAL` | Matched |

Bucket creation alone (`bkt-gaps-legacy-class-create-region` and
`bkt-gaps-legacy-class-create-multi-regional`): `REGIONAL` in `nam4` gave the
conflict for `NAM4`, `REGIONAL` in `us-central1` was created with
`locationType` `region`, and `MULTI_REGIONAL` in `us-central1` gave the
conflict for `US-CENTRAL1`. gcs-mimic matches since aligned; it refused both
`REGIONAL` creations as unsupported and created the `MULTI_REGIONAL` bucket.

gcs-mimic now stores `REGIONAL` and checks every requested class against the
bucket's location type in the catalog: on bucket insert, PATCH, PUT and Storage
v2 UpdateBucket, on lifecycle rules (actions and matched classes), on every
object publication, when an appendable object is created, when a rewrite
session starts, before any byte is copied, and when a resumable upload is
initiated. A lifecycle transition from `DURABLE_REDUCED_AVAILABILITY` to
`REGIONAL` runs in a region bucket, and `REGIONAL` objects move to `NEARLINE`,
`COLDLINE` and `ARCHIVE` at their age deadline, as the documented table allows.
A rule an earlier release stored against a location, such as a regional
`MULTI_REGIONAL` target, stays inert. A compose of components with different
classes now uses the recorded message. The three `REGIONAL` markers in
`bkt-unsupported-settings` were removed. `RAPID` stays unsupported, with the
message `storageClass RAPID requires a zonal bucket`.

The conflict names the number the bucket's `projectNumber` reports; how a named
project gets that number is described under the eighth recording.

Inferred, not recorded: `MULTI_REGIONAL` in a dual-region is accepted, as the
documentation says; resumable initiation refuses a class the location cannot
hold, with the envelope sent as `text/html` like the multipart insert, rather
than at finalization; a requested class must fit the location even in an
Autoclass bucket, which stores `STANDARD`; an XML copy that inherits a source
class the destination cannot hold is refused; an XML bucket creation naming such
a class gets the XML conflict; several differing compose classes are separated
by a comma and a space.

The full Storage v2 UpdateBucket response of `grpc-update-bucket-class-regional`
also differed outside the classes: gcs-mimic omitted `entityAlt` from bucket
and default object ACL entries, wrote `createTime` and `updateTime` with six
fractional digits where Cloud Storage gives three, and included
`softDeletePolicy.effectiveTime` for a bucket whose soft delete is off. All
three are now aligned, so this step and the Java default mask step of
`grpc-bucket-unset-settings-mask`, which ignored the same fields, compare the
whole response.

A replay of the merged reference with `--fail-on-diff --fail-on-note` on
2026-11-17 found all **136** cases `match`, with no differing fields, notes or
errors. The same source passed the workspace tests (3914 passed, 0 failed and
1 ignored across 19 test binaries), the production lint gate with 0 new
violations, `bash scripts/check-docs.sh` and `cargo xtask compatibility check`.

## Eighth recording: project numbers as input

Recorded on **2026-11-17**. Until then gcs-mimic rendered the number of every
named project as `0` (`projectNumber`, `projects/0`, `project-owners-0`,
`service-0@...`), refused its own `projects/0` as Storage v2 input, and the
differential normalizer needed three rules to hide that `0`. Whether Cloud
Storage takes a project number wherever it takes a project ID was not
recorded. One new case asked, with the project and access token again supplied
only through environment variables:

```sh
cargo xtask differential run --filter prj-number-inputs --record <partial-reference>
cargo xtask differential merge compatibility/differential/reference/gcs.json \
  <partial-reference> --add-new --out compatibility/differential/reference/gcs.json
```

- Requests sent to Cloud Storage: **24** in one run, with no HTTP 429.
- Resources: the case's own us-central1 bucket and the bucket the case created
  by project number, both empty.
- Cleanup: both buckets were deleted and confirmed 404, and the final listing
  found nothing left.

The reference now holds **137** cases and **2279** requests.

The case reads the case bucket's `projectNumber` into a variable and sends it
back (`prj-number-inputs`):

| Step | Request | Cloud Storage answer | gcs-mimic |
| --- | --- | --- | --- |
| `json-list-by-number` | `buckets.list` with `project=<number>` | 200, the case bucket | Matched since aligned (an empty listing) |
| `json-insert-by-number`, `json-list-inserted-by-id` | `buckets.insert` with `project=<number>`, then `buckets.list` with the project ID | 200; the new bucket lists under the ID | Matched since aligned (the bucket landed in a project named by the digits) |
| `grpc-list-by-number` | Storage v2 ListBuckets with `parent` `projects/<number>` | The case bucket, `project` `projects/<number>` | Matched since aligned (an empty listing) |
| `grpc-list-zero` | ListBuckets with `parent` `projects/0` | INVALID_ARGUMENT `Project id: 0 is invalid or not found` with a `google.rpc.BadRequest` repeating it | Matched since aligned (gcs-mimic used its own wording) |
| `service-account-by-number` | `projects/<number>/serviceAccount` | 200, `service-<number>@gs-project-accounts.iam.gserviceaccount.com` | Matched |
| `hmac-get-missing-by-number` | `projects/<number>/hmacKeys/<missing>` | 404 `Access ID not found in project.` | Matched |

gcs-mimic still has no project registry, but a project's number is now a pure
function of its ID: a numeric ID is that number, and any other ID
renders `100000000000 + (SHA-256(ID) mod 900000000000)`, the twelve-digit
number floci-gcp derives. Every rendering uses it: `projectNumber`, Storage v2
`Bucket.project`, project-team ACL entities and their IAM mapping, the service
agent address and the class-location refusal. A request may name its project
by that number in the JSON `project` parameter and project path, the XML
`x-goog-project-id` header and the Storage v2 and legacy v1 project fields; the
number is turned back into the ID before authorization, from the projects the
identity file names or that own a bucket, a soft-deleted bucket or an HMAC key.
Bucket and HMAC key page tokens bind the number, so a listing continues under
either spelling. Opening an older catalog renames the stored `-0` project teams
of a named project's ACLs. The normalizer's three rules for `0` were removed;
the emulator's number is hidden the way a recorded number is.

Inferred, not recorded: the XML `x-goog-project-id` header, the legacy v1
fields, HMAC key creation and listing, and soft-deleted bucket listings take a
number as the recorded routes do; a page token issued for one spelling of a
project continues under the other. Not modeled: a number no known project
derives from names a project of its own, where Cloud Storage would answer that
the project is invalid or not found.

A replay of the merged reference with `--fail-on-diff --fail-on-note` on
2026-11-17 found all **137** cases `match`, with no differing fields, notes or
errors. The same source passed the workspace tests (3923 passed, 0 failed and
1 ignored across 19 test binaries), the production lint gate with 0 new
violations, `bash scripts/check-docs.sh` and `cargo xtask compatibility check`.

## Ninth recording: full projection on resumable uploads and rewrites

Recorded on **2026-11-17**. The Go SDK's Writer (a resumable upload) and Copier
(a rewrite) send `projection=full` on every call, and gcs-mimic rendered the
full projection's `acl` and `owner` on every object write except those two.
Whether Cloud Storage adds them there was not recorded. One new case asked, with
the project and access token again supplied only through environment variables:

```sh
cargo xtask differential run --filter obj-full-projection-writes --record <partial-reference>
cargo xtask differential merge compatibility/differential/reference/gcs.json \
  <partial-reference> --add-new --out <merged-reference>
```

- Requests sent to Cloud Storage: **16** in one run, with no HTTP 429.
- Resources: the run's shared bucket, which held the uploaded object and its
  rewritten copy.
- Cleanup: the bucket was emptied (2 object versions), deleted and confirmed
  404, and the final listing found nothing left.

The reference now holds **138** cases and **2295** requests.

| Step | Request | Cloud Storage answer | gcs-mimic before |
| --- | --- | --- | --- |
| `complete` | The final chunk of a resumable upload started with `projection=full` | 200 with the object's `acl` and `owner` | No `acl` or `owner` |
| `status-after-completion` | `Content-Range: bytes */5` on the completed session | 200 with the same `acl` and `owner` | No `acl` or `owner` |
| `rewrite-full` | A rewrite with `projection=full` | 200; the `resource` carries `acl` and `owner` | `owner` only, as without the projection |
| `get-rewritten-full` | `objects.get` of the rewrite's destination with `projection=full` | 200 with `acl` and `owner` | Matched |

A resumable session now stores its completed resource without ACL fields or
field selection, and each answer of the completed session adds the object's
`acl` and `owner` as they are then before applying the initiation's `fields`.
A rewrite with `projection=full` adds them to the resource of its finished
result the same way. Both reuse the path every other object response takes, so
uniform bucket-level access and a strict caller without
`storage.objects.getIamPolicy` still leave the fields out. Requests on a session
URI carry no credentials, so for a resumable session that caller is the
initiator, whose access the session stores.

Inferred, not recorded: a DELETE refused because the session completed answers
the same way; after an ACL change, an answer of the completed session lists the
current entries while keeping the stored resource's metageneration and the entry
etags it implies; a session completed before this change whose field selection
dropped `bucket`, `name`, `generation` or `metageneration` answers without ACL
fields.

A replay of the merged reference with `--fail-on-diff --fail-on-note` on
2026-11-17 found all **138** cases `match`, with no differing fields, notes or
errors. The same source passed the workspace tests (3930 passed, 0 failed and
1 ignored across 19 test binaries), the production lint gate with 0 new
violations, `bash scripts/check-docs.sh` and `cargo xtask compatibility check`.

## Tenth recording: a superseded read sent back as a metadata update

Recorded on **2026-11-17**. Since object metadata writes ignore If-Match (case
`obj-gaps-json`), the .NET client's UpdateObject made from a superseded read,
which sends the read object whole with no query condition, reached the
emulator's check of output-only body fields and failed with 400 on the stale
`etag`; the SDK scorecard's .NET case still expected 412. What Cloud Storage
does with such a body was not recorded. One new case asked, with the project
and access token again supplied only through environment variables:

```sh
cargo xtask differential run --filter obj-stale-body-update --record <partial-reference>
cargo xtask differential merge compatibility/differential/reference/gcs.json \
  <partial-reference> --add-new --out <merged-reference>
```

- Requests sent to Cloud Storage: **15** in one run, with no HTTP 429.
- Resources: the run's shared bucket, which held the one object the case wrote.
- Cleanup: the bucket was emptied (1 object version), deleted and confirmed 404,
  and the final listing found nothing left.

The reference now holds **139** cases and **2310** requests.

| Step | Request | Cloud Storage answer | gcs-mimic before |
| --- | --- | --- | --- |
| `put-stale-read` | After a PATCH, a PUT repeating the first read whole (stale `etag`, `metageneration` `1` and `updated`) with that `etag` as If-Match | 200; the metadata is replaced and the metageneration advances to 3 | 400, `etag` refused as an unsupported setting |
| `patch-stale-etag` | A PATCH whose body names the superseded `etag` | 200; applied, metageneration 4 | 400 |
| `get-after` | `objects.get` | The PATCH's metadata at metageneration 4 | The first PATCH's metadata at metageneration 2 |

Metadata PATCH and PUT now ignore a body `etag`, `metageneration` and `updated`
like the other output-only fields; a body `generation` still selects a
generation. Only a query condition guards a concurrent update. The .NET
scorecard case now expects the stale update to apply, as it does on Cloud
Storage.

Inferred, not recorded: malformed or `null` values of these three fields are
ignored the same way, as for `size` and `md5Hash`.

## Eleventh recording: open gap probes

Recorded on **2026-11-17** in two live runs, with the project and access token
again supplied only through environment variables. The first run recorded seven
new cases whose IDs contain `-probe-`; the second recorded one more case for the
methods the first left unrecorded:

```sh
cargo xtask differential run --filter -probe- --record <partial-reference>
cargo xtask differential run --filter bkt-probe-undeclared-query-writes --record <partial-reference>
cargo xtask differential merge compatibility/differential/reference/gcs.json \
  <partial-reference> --add-new --out <merged-reference>
```

- Requests sent to Cloud Storage: **173** and **30**, with no HTTP 429.
- Resources: the shared bucket of each run, three buckets of the cases' own and
  one bucket created by a case with a 7-day soft delete policy. Every bucket was
  emptied, deleted and confirmed 404; no retention lock, HMAC key, Pub/Sub topic
  or IAM member other than the default legacy bindings was created.

The reference now holds **147** cases and **2513** requests.

| Case | Gap | Cloud Storage answer | gcs-mimic before |
| --- | --- | --- | --- |
| `obj-probe-noncurrent-delete-conditions` | SPEC-VERSION-DELETE-CONDITIONS | Every DELETE condition compares with the selected noncurrent generation, with and without a live generation: a failing match is 412, a failing not-match an empty 304; with one holding and one failing, the failing one decides | Matched |
| `bkt-probe-collection-validators` | SPEC-JSON-ETAG-COLLECTIONS | The bucket ACL, default object ACL and object ACL listings answer `If-None-Match: *` with 304 and `If-Match: "bogus"` with 412; the notification and HMAC key listings send no ETag and ignore both | 200 on the ACL listings; 400 on the notification listing |
| `obj-probe-undeclared-query-move-restore`, `obj-probe-undeclared-query-acls`, `bkt-probe-undeclared-query-resources`, `bkt-probe-undeclared-query-writes` | SPEC-JSON-UNKNOWN-QUERY-OBJECTS | `gcsmimicUnknown=1` is ignored on objects.move and restore, every object, bucket and default object ACL method, bucket IAM get, set and test, notification list, get, insert and delete, HMAC key list, get, update and delete, the service account and the managed folder list; a successful restore sends no ETag header | 400 `Unknown query parameter for this method` |
| `obj-probe-resumable-alias-session` | SPEC-RSM-DISCOVERY-PATH | A session started on `/resumable/upload/storage/v1` keeps that path in its Location, and its chunk, status and final requests answer like the upload session URI | Location under `/upload`; PUT on the alias routed to the XML API (400) |
| `obj-probe-upload-hashes` | SPEC-010 | A wrong Content-MD5 alone is ignored; X-Goog-Hash on a resumable initiation is ignored; on the final request a wrong value is 400 with the `Provided … doesn't match calculated …` message and covers the whole object | Verified the initiation header (400 at completion) |

gcs-mimic now gives each recorded answer and the replay of all eight cases
matches, except the two managed folder steps, which stay declared local
choices because gcs-mimic does not implement managed folders. The
fake-gcs-server profile keeps refusing an undeclared parameter on the methods
fake-gcs-server v1.56.1 does not serve.

## Twelfth recording: Storage v2 gRPC probes

Recorded on **2026-11-18** against real Cloud Storage over gRPC, with the project
and access token supplied only through environment variables, as for the earlier
recordings. Ten new cases, all named `grpc-probe-*`, asked the Storage v2 answers
that the ledger had kept as local choices: bucket and object conditions, read and
update masks, object ETags, routing metadata, write streams and resumable
sessions, and derived-write destinations. They hold **230** recorded steps; the
reference now holds **157** cases. The case definitions are in
[compatibility/differential/cases](../../../compatibility/differential/cases) and
the recorded answers in the committed
[reference](../../../compatibility/differential/reference/gcs.json). Opaque
per-request values (upload IDs, the trailing detail of an INTERNAL message) are
masked, and the four INTERNAL steps of `grpc-probe-resumable-sessions` compare
without their message.

gcs-mimic now gives each recorded answer below, and the offline replay matches,
except for the ten steps of these cases that a case file declares as `local_choice`: nested bucket
read mask paths (six steps of `grpc-probe-bucket-masks`), the `*` bucket update
mask (one step), a destination `acl` on ComposeObject and RewriteObject (two steps
of `grpc-probe-derived-writes`) and GetIamPolicy on an object resource (one step
of `grpc-probe-routing`). Cloud Storage refused BidiReadObject to the recording
project as not allowlisted (case `grpc-gaps-bidi-read`), so no BidiReadObject
answer was recorded.

| Case | What it showed | What changed |
| --- | --- | --- |
| `grpc-probe-bucket-conditions` | GetBucket, UpdateBucket and DeleteBucket compare whatever metageneration conditions arrive: a match of 0 or -1 answers FAILED_PRECONDITION `At least one of the pre-conditions you specified did not hold.`, a not-match of 0 or -1 holds, and a match with a not-match that both hold succeeds. DeleteBucket answers a missing bucket (NOT_FOUND) before a failing condition, a failing condition before `The bucket you tried to delete is not empty.`, and a not-match that alone fails with `The resource you requested has not been modified according to the condition you specified.`. ListObjects with an unknown read mask path answers INVALID_ARGUMENT `Invalid read_mask field: 'no_such_field'.` | UpdateBucket no longer refuses 0, negative values and both conditions with INVALID_ARGUMENT; DeleteBucket words a nonempty bucket and a lone failing not-match as recorded. LockBucketRetentionPolicy keeps its refusal of 0 and negative values, because the harness never locks a real bucket |
| `grpc-probe-bucket-masks` | GetBucket and ListBuckets serve nested members of a message (such as `soft_delete_policy.retention_duration`) and answer INVALID_ARGUMENT `There is no field for this resource named '<path>'.` for an unknown top-level path and for a path under a repeated field, a `labels` key or a member the message lacks. A negative ListBuckets `page_size` answers INVALID_ARGUMENT with reason `GRPC_PAGE_SIZE_MUST_BE_POSITIVE`, also beside an unknown path. UpdateBucket applied an update naming a superseded `bucket.etag` and one naming the current etag, accepted the output-only `create_time` and `update_time` paths, answered an unknown path INVALID_ARGUMENT, answered an empty and an absent `update_mask` INVALID_ARGUMENT with reason `GRPC_MISSING_UPDATE_MASK_FIELDS`, served `*`, rounded a retention duration of 5.5 s down to 5 s and refused a fractional soft delete duration with INVALID_ARGUMENT `Invalid argument.` and a `BadRequest` | UpdateBucket ignores the ETag and takes output-only paths as no-ops; unknown paths, empty masks, a negative page size and a fractional soft delete duration answer as recorded. Nested bucket read mask paths and the `*` bucket update mask stay refused with UNIMPLEMENTED (declared local choices) |
| `grpc-probe-object-conditions` | GetObject and ReadObject served a match with a not-match. DeleteObject, UpdateObject, WriteObject, ComposeObject (match of 0 only) and RewriteObject (destination and source) answered a metageneration match of 0 with FAILED_PRECONDITION and, apart from ComposeObject, a match beside a not-match naming the current value with FAILED_PRECONDITION `The resource you requested has not been modified…`. MoveObject compared a match of 0 but refused a match beside a not-match with INVALID_ARGUMENT and a `BadRequest` `Found conflicting object preconditions…`. RestoreObject in a bucket without soft delete always answered the `bucket soft delete policy must be set.` BadRequest. GetObject with `soft_deleted` in that bucket answered a `Soft delete policy is required…` BadRequest, and a `restore_token` answered INTERNAL | UpdateObject, ComposeObject, RewriteObject and RestoreObject compare conditions instead of refusing their shape; MoveObject refuses a pair (every pair by its shape; the thirteenth recording confirmed it for pairs of different values); GetObject serves `soft_deleted` and answers a `restore_token` with INTERNAL; a not-match naming the current value is worded as not modified on writes and on a rewrite source |
| `grpc-probe-object-masks` | GetObject, ReadObject and ListObjects select a member of a singular message (`owner.entity`) and name no field for an unknown name, a member the message lacks or a sub-field of `acl`; ListObjects words an unknown path `Invalid read_mask field`. UpdateObject applied an update naming a superseded `object.etag`, accepted output-only paths, answered an unknown path INVALID_ARGUMENT, answered an empty and an absent mask with the `GRPC_MISSING_UPDATE_MASK_FIELDS` answer above, and a `*` mask with only a content type replaced the editable metadata and kept the ACL. ReadObject returned the stored bytes of a gzip object without transcoding | Object read masks accept the recorded members; UpdateObject ignores the etag, accepts output-only paths and `*`, and answers the empty mask as recorded. The thirteenth recording then showed that timestamp parts and `contexts.custom` are selectable, and they are now served |
| `grpc-probe-object-precedence` | DeleteObject answered NOT_FOUND before a failing condition and a failing condition (FAILED_PRECONDITION) before a temporary hold, which alone is PERMISSION_DENIED. ReadObject answered a read mask that lacks `checksummed_data.content` and a malformed customer key before looking up the object, the read mask before a failing generation condition, and a failing generation condition before an offset past the end | The recorded order of these checks is kept; combinations nothing recorded stay local choices |
| `grpc-probe-derived-writes` | ComposeObject accepted a destination `storage_class` the sources have and refused another (NEARLINE) with INVALID_ARGUMENT and a `BadRequest`, accepted a destination `acl` and ignored service-set destination fields. RewriteObject refused a destination carrying a generation, metageneration, size and times with `You must specify an object name.`, accepted a destination `acl`, and a rewrite without a destination kept `contentType` `text/plain` in its response and in Storage v2 and JSON reads of the result | ComposeObject accepts the sources' class and ignores service-set fields; RewriteObject refuses service-set fields as recorded. A destination `acl` on either RPC stays refused with UNIMPLEMENTED (declared local choice) |
| `grpc-probe-routing` | GetObject, GetBucket, ListBuckets and a WriteObject stream sent without `x-goog-request-params` answered INVALID_ARGUMENT with reason `GRPC_INVALID_X_GOOG_REQUEST_PARAMS`; a routed bucket other than the request's, or a project alone, answered `GRPC_INVALID_X_GOOG_REQUEST_PARAMS_BUCKET` naming both IDs; a bare bucket ID answered `GRPC_INVALID_X_GOOG_REQUEST_PARAMS_INVALID_BUCKET_FORMAT`; an unknown key and a `routing_token` were ignored. GetIamPolicy on an object resource succeeded with a policy | The metadata is required, compared and refused as recorded on every RPC except QueryWriteStatus and CancelResumableWrite (the thirteenth recording confirmed it for every RPC it asked); unknown keys, `routing_token` and keys an RPC does not route are ignored. GetIamPolicy on an object resource stays refused (declared local choice) |
| `grpc-probe-write-streams` | A WriteObject stream answers at `finish_write` and ignores a later message, a repeated `write_object_spec` or `upload_id` and `object_checksums` on any message. Only whole 256 KiB granules persist before `finish_write`, so a nonfinal 3-byte message fails the stream and persists nothing. A first `write_offset` other than the persisted size answers OUT_OF_RANGE with reason `GRPC_UPLOAD_INVALID_START_OFFSET` (`..._NEW_UPLOAD` on a non-resumable write); a final size that misses `object_size` answers OUT_OF_RANGE with reason `GRPC_MISMATCHED_UPLOAD_SIZE` and terminates the session; a wrong data CRC32C answers INVALID_ARGUMENT before the write's conditions; a malformed customer key answers before a failing condition | WriteObject and BidiWriteObject follow these answers through one shared decoding of each stream message |
| `grpc-probe-bidi-write` | The same stream rules hold on BidiWriteObject; flush and `state_lookup` are served on a single-shot stream, reporting the persisted granules with the CRC32C alone | BidiWriteObject follows the recorded answers |
| `grpc-probe-resumable-sessions` | CancelResumableWrite on a finalized upload answers FAILED_PRECONDITION `Upload has already been completed.`; a cancelled upload answers QueryWriteStatus and CancelResumableWrite with FAILED_PRECONDITION and its reason, a write with INVALID_ARGUMENT, and cancelling it again succeeds. Text that is no upload ID answers INVALID_ARGUMENT `Invalid upload ID '<id>'.`; an ID that cannot be read answers INTERNAL, also under the bucket's resource name and with a routed bucket equal to the ID's | The upload ID RPCs answer from the ID and route no bucket. An ID of the issued shape whose session is missing or expired first answered NOT_FOUND; the thirteenth recording showed INTERNAL for such IDs, and gcs-mimic now answers INTERNAL |

The remaining local choices are listed under SPEC-GRPC-LOCAL-CHOICES in the
[gap ledger](../../../compatibility/gaps.json), each with the test that pins it.

## Thirteenth recording: Storage v2 answers that had been inferred

Recorded on **2026-11-18**, after the twelfth recording, against real Cloud
Storage over gRPC with the same environment-only credentials. Eight new cases,
all named `grpc-probe-inferred-*`, asked about every Storage v2 answer that
gcs-mimic had decided by inference: the questions the twelfth recording did not
reach and the answers its code marked as not recorded. They hold **202**
recorded steps. The case definitions are in
[compatibility/differential/cases](../../../compatibility/differential/cases)
and the recorded answers in the committed
[reference](../../../compatibility/differential/reference/gcs.json). Upload IDs
and the trailing detail of an INTERNAL message are masked.

gcs-mimic was changed to give each recorded answer below, and the offline
replay matches, except for one step the case file declares as `local_choice`:
`update-mask-location-type` of `grpc-probe-inferred-bucket-masks`. That step
sends an update whose mask names the output-only `location_type`. Cloud Storage
raised the bucket metageneration from 1 to 2 on it, which neither the no-op
masks before it (`etag`, `bucket_id` and `metageneration` left it at 1) nor the
steps after it explain, and gcs-mimic changes the metageneration only when an
update changes a setting. The case file records that reason.

| Case | What it showed | What changed |
| --- | --- | --- |
| `grpc-probe-inferred-conditions` | A negative `if_generation_match` or `if_metageneration_match` of -1 is compared, not validated: WriteObject, UpdateObject, GetObject, ComposeObject, RewriteObject and DeleteObject answered FAILED_PRECONDITION `At least one of the pre-conditions you specified did not hold.`. An `if_generation_not_match` of -1 held on UpdateObject, GetObject and DeleteObject, and failed on a WriteObject of a new name and on a RewriteObject destination. MoveObject refused every match beside a not-match with INVALID_ARGUMENT `Found conflicting object preconditions. Mutually exclusive fields cannot be set simultaneously.`, for the destination and the source generation and metageneration pairs alike, whether the match failed or both conditions held | The negative-value refusals were removed: every Storage v2 object RPC compares the values. BidiReadObject does too, by inference from GetObject and ReadObject, because Cloud Storage refused BidiReadObject to the recording project. The legacy v1 service and LockBucketRetentionPolicy keep their own rules. MoveObject's refusal of a match beside a not-match is now recorded for pairs of different values as well as equal ones |
| `grpc-probe-inferred-destinations` | RewriteObject refused only a destination that carries a `generation` (INVALID_ARGUMENT `You must specify an object name.`); a destination with `metageneration`, `size`, `etag`, `create_time`, `update_time`, `checksums`, `owner` or `component_count` set alone was served. ComposeObject served a destination with any one of those fields or a `generation` | RewriteObject refuses only a destination `generation`; RewriteObject and ComposeObject ignore every other service-set destination field. That the fields this case did not set, the retention expiry among them, are ignored too is inferred from the proto |
| `grpc-probe-inferred-upload-ids` | An issued upload ID with one character changed, in the middle or at the end, answered INTERNAL (`We encountered an internal error. Please try again.` with a detail) on QueryWriteStatus, a resumed WriteObject and CancelResumableWrite, and the session the ID was derived from stayed usable | An upload ID that names no session the catalog knows answers INTERNAL, not NOT_FOUND. An expired session answers INTERNAL as well: the catalog cannot tell it from an ID that was never issued, and no expired ID was recorded |
| `grpc-probe-inferred-session-states` | After a finishing message whose size missed `object_size`, a resumed WriteObject (with and without `finish_write`), CancelResumableWrite and QueryWriteStatus after the cancel all answered FAILED_PRECONDITION `Upload has already been terminated.`. A write to a cancelled session answered INVALID_ARGUMENT `Upload has already been cancelled.`. On a non-resumable first message, a `write_offset` of 2 beside bytes past `object_size` answered OUT_OF_RANGE `A new upload request must start with write_offset 0, but received '2'.`. On a resumable message, a failing condition alone answered FAILED_PRECONDITION; beside it a wrong data CRC32C answered INVALID_ARGUMENT `Checksummed data message crc32c mismatch: ...`, a first offset past the persisted size answered OUT_OF_RANGE and bytes past `object_size` answered OUT_OF_RANGE, and the size answered before a wrong data CRC32C; a first offset past the persisted size also answered before a malformed `md5_hash`. A CRC32C declared in `object_checksums` by a stream that did not finish did not bind a later stream, and a stream that declared it and then finished with data that does not match answered INVALID_ARGUMENT `Object crc32c mismatch: server computed: '...', client provided: '...'.` | A write to a terminated session answers FAILED_PRECONDITION with reason `GRPC_UPLOAD_ALREADY_TERMINATED`. A resumable message is checked in the order offset, size, data CRC32C, declared object checksums, conditions, then the finished object, by one function the single-shot path shares. Object checksums declared on a stream hold for that stream and are verified at its finish, and the mismatch leaves the session open at its persisted size. Not recorded: a malformed `md5_hash` against the size, the data checksum and the conditions; the MD5 wording; the session state after a finishing message past `object_size` |
| `grpc-probe-inferred-granules` | After one whole 256 KiB granule without `finish_write`, QueryWriteStatus reported a persisted size of 262144. A resumed stream whose first offset was 262145 answered OUT_OF_RANGE (`Upload request started at offset '262145', which is past expected offset '262144'.`); a first offset of 0 or 131072 with one byte that matched the persisted content was accepted and left the persisted size at 262144, and a first offset equal to the persisted size finished the upload. A granule past an `object_size` of 262143 answered OUT_OF_RANGE and ended the session (QueryWriteStatus answered `Upload has already been terminated.`). A single-shot BidiWriteObject reported a persisted size of 0 for `state_lookup` after a granule and after a flush at one granule | A resumed stream may start at or below the persisted size and the bytes it repeats are skipped; they are not compared with the persisted ones, because the recording sent identical bytes. A message whose bytes pass `object_size` ends the session even without `finish_write`. A single-shot BidiWriteObject reports persisted size 0 for `flush` and `state_lookup` |
| `grpc-probe-inferred-routing` | RPCs sent without `x-goog-request-params` answered INVALID_ARGUMENT `An x-goog-request-params request metadata property must be provided for this request.` on all 16 asked: ListObjects, DeleteObject, UpdateObject, ComposeObject, RewriteObject, StartResumableWrite, GetIamPolicy, TestIamPermissions, BidiWriteObject, ReadObject, MoveObject, RestoreObject, UpdateBucket, DeleteBucket, QueryWriteStatus and CancelResumableWrite. A `bucket` key sent twice, with the same or different values, answered `The x-goog-request-params metadata has duplicate entries for the key 'bucket'.`; a `bucket` pair without a value answered the invalid bucket format; an empty value counted as no metadata; an unknown key with a malformed escape, a trailing `&` and a percent-encoded key beside the right bucket were served. ListBuckets answered a bare bucket ID beside the right project with the invalid bucket format, and another project with `The x-goog-request-params must specify the same project as the resource in the request...`. RewriteObject answered a routed source bucket other than the request's, `bucket` alone and `source_bucket` alone each with its own message. QueryWriteStatus and CancelResumableWrite answered a routed bucket other than the upload ID's with reason `GRPC_INVALID_X_GOOG_REQUEST_PARAMS_BUCKET` | Every call reads each routing key once, refuses a duplicated key and a malformed value, ignores unknown keys and compares only the keys its RPC routes. The metadata is required on every RPC except the two upload ID RPCs, which check the ID first and need the metadata for a readable ID. A resumed WriteObject or BidiWriteObject compares the ID's bucket with the routed one after authorization, which is inferred from those two RPCs. Not recorded: a malformed `source_bucket` or `appendable`, a duplicated key other than `bucket`, and the order of the comparison against authorization |
| `grpc-probe-inferred-bucket-masks` | UpdateBucket accepted the output-only `bucket_id`, `metageneration`, `location_type` and `owner`, the immutable `project` and `location`, the identifier `name` and `etag` set to the current etag in `update_mask`, as no-ops apart from the metageneration of `location_type` described above. A not-match equal to the current metageneration answered FAILED_PRECONDITION `The resource you requested has not been modified according to the condition you specified.` on GetBucket and on UpdateBucket. `versioning.no_such_field` and `iam_config.no_such_field` answered INVALID_ARGUMENT `There is no field for this resource named '<path>'.`, and `labels.` answered INVALID_ARGUMENT `Invalid user label:  with value . Reason: Invalid field "user_labels_list"; key is empty.` | UpdateBucket accepts these paths and changes nothing, refuses an unknown member under `versioning` and `iam_config` as naming no field, and refuses an empty label key with a `BadRequest`. A not-match that alone fails on GetBucket, UpdateBucket and DeleteBucket is worded as not modified through the shared Storage v2 conversion; the legacy v1 service keeps the failed-condition wording. Other bucket message fields keep UNIMPLEMENTED for an unknown member, which was not asked |
| `grpc-probe-inferred-object-masks` | GetObject answered `There is no field for this resource named '<path>'.` for a map key (`metadata.somekey`), a path under a scalar or a member (`name.x`, `owner.entity.x`), an empty path, a trailing dot and a leading dot, and served `create_time.seconds`, `retention.retain_until_time` and `contexts.custom`. ReadObject answered `The 'read_mask' field must include checksummed_data.content, ...` for `metadata.no_such_field` and `metadata.owner.entity`, and served `object_checksums.crc32c` beside the content. UpdateObject accepted `metageneration`, `size`, `delete_time`, `finalize_time`, `checksums`, `update_storage_class_time`, `owner`, `component_count`, `restore_token`, `soft_delete_time` and `hard_delete_time` (the fields the proto marks `OUTPUT_ONLY`), and `etag`, `generation`, `retention_expire_time`, `name`, `bucket`, `custom_time`, `content_encoding` and `acl`, as paths. It refused `storage_class` (NEARLINE) with `Client is not allowed to directly modify storage_class.` and `kms_key` (a key that does not exist) with `Cannot change the Cloud KMS key associated with a resource.`. A `*` mask beside an object that carried only a content type released a temporary hold and an event-based hold, and `*` beside `content_language` was served | Object read masks select `create_time.seconds` (the `.nanos` part and the other timestamps' parts follow by inference), `contexts.custom` and, on ReadObject, a single digest of `object_checksums`, and name no field for the recorded path shapes. UpdateObject accepts output-only, identifier and service-kept fields as no-ops, refuses `storage_class` and `kms_key` with a `BadRequest`, and treats `*` beside another path as a replacement without reading the other path (inferred). Not recorded: the parts of the retention timestamp, a path under the context map and the selection of `md5_hash` alone |

gcs-mimic gives every recorded answer above. The answers it gives where nothing
was recorded, and the declared differences of this and the twelfth recording
(eleven steps in all), are listed under SPEC-GRPC-LOCAL-CHOICES in the
[gap ledger](../../../compatibility/gaps.json), each with the test that pins it.
