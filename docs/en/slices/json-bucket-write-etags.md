# JSON bucket write and IAM policy entity-tag headers

## Contract and limits

The pinned .NET Storage client copies the fetched resource's JSON `etag` into
If-Match on writes: UpdateBucketAsync sends it on the bucket PUT and
SetBucketIamPolicyAsync on `PUT /storage/v1/b/{bucket}/iam`. Google's JSON header
reference documents If-Match and If-None-Match for GET only, so the write behavior
follows the Cloud Storage observations in
docs/en/evidence/bucket-write-preconditions.md.

Bucket PATCH and PUT accept If-Match and If-None-Match and do not evaluate them.
Cloud Storage returned 200 and applied the change for a stale If-Match and for
If-None-Match on both methods, so the emulator matches that observed behavior.
This is accepted-but-ignored by provider evidence, not an evaluated precondition:
use `ifMetagenerationMatch`, which stays atomic with the mutation, for
compare-and-set on bucket metadata. Because the headers are not evaluated, their
syntax is not checked either, and standalone date and If-Range headers keep their
existing ignored behavior on these writes. Bucket DELETE keeps rejecting both
headers with 400.

setIamPolicy evaluates If-Match against the current policy etag. A bare element
names the same opaque tag as the JSON `etag` field, which is unquoted base64 that
Google.Apis copies verbatim; a quoted element is compared with the HTTP ETag that
getIamPolicy returns. Strong comparison, lists, repeated field lines and a sole
wildcard follow the shared RFC 9110 parser. A stale, foreign or weak tag returns
412 conditionNotMet. The body `etag` remains a separate condition, so a stale
body etag fails with 412 even when the header is current, as observed. The catalog
runs both checks on the current policy inside the immediate write transaction,
before any ACL or binding change, so two writes carrying one tag have exactly one
winner. Malformed If-Match returns 400 invalidArgument.

Google.Apis also replaces a fetched resource's body `etag` with the response's
HTTP ETag, and getIamPolicy sends that tag quoted. The .NET client therefore
returns the policy with a quoted body etag. The body parser strips one pair of
surrounding quotes before decoding the base64 value, so that body names the same
policy etag. Empty, unbalanced, doubled or weak quoted values stay invalid. This
is a local SDK compatibility choice; the provider's handling of a quoted body
etag was not observed. If-None-Match, date
conditions and If-Range on setIamPolicy return 400 without a change, because
their provider behavior was not observed.

## Sources

Checked on 2026-10-31:

- [Google JSON headers](https://docs.cloud.google.com/storage/docs/json_api/v1/parameters):
  If-Match and If-None-Match apply to GET requests for all resources, including
  buckets, objects and ACLs. Writes are not documented.
- [Request preconditions](https://docs.cloud.google.com/storage/docs/request-preconditions):
  both headers are "Applicable for requests that retrieve data"; generation and
  metageneration preconditions are recommended over ETags.
- [buckets.setIamPolicy](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/setIamPolicy):
  the body `etag` is the "HTTP 1.1 Entity tag for the policy", optional.
- [Official common request ETag behavior](https://github.com/googleapis/google-api-dotnet-client/blob/dd7f8ffe5a9aa3b9b07174dc03faf0f3289e7e24/Src/Support/Google.Apis/Requests/ClientServiceRequest.cs),
  Google.Apis commit dd7f8ffe5a9aa3b9b07174dc03faf0f3289e7e24: AddETag adds the
  body's ETag with TryAddWithoutValidation, because some services return unquoted
  etags, and selects If-Match for PUT, POST, PATCH and DELETE.
- [Official response ETag behavior](https://github.com/googleapis/google-api-dotnet-client/blob/dd7f8ffe5a9aa3b9b07174dc03faf0f3289e7e24/Src/Support/Google.Apis/Services/BaseClientService.cs),
  same commit: DeserializeResponse sets the result's ETag to the response's
  `ETag` header tag when the header is present.
- docs/en/evidence/bucket-write-preconditions.md: the Cloud Storage observations.

## Acceptance

tests/bucket_if_match_http.rs covers the gzip full-resource PUT with the returned
etag, stale, weak, malformed and If-None-Match headers on PATCH and PUT applying
the change, atomic `ifMetagenerationMatch` failure, retained DELETE rejection and
a missing bucket. For setIamPolicy it covers bare, quoted and wildcard success,
stale, foreign-bucket and weak 412s with the policy unchanged, the body etag and
header combinations including the quoted HTTP ETag in both, 400 for malformed and
unsupported headers, and a two-writer race with exactly one winner. The unit test
`json::iam::tests::etag_accepts_one_pair_of_http_quotes_as_the_same_tag` covers
the quoted body etag forms. The updated
`etag::json_etag_read_unsupported_lists_dates_and_writes` in
tests/object_range_http/etag.rs records the bucket write headers as accepted.
