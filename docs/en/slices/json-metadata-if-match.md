# JSON object metadata If-Match

## Contract and limits

Object metadata PUT and PATCH, including POST with X-HTTP-Method-Override: PATCH,
accept If-Match as a local SDK compatibility extension. This scope does not claim
Google service write parity. Google's JSON header reference explicitly documents
If-Match for GET; no real-GCS write differential test was run.

The pinned .NET Storage client sends an object's returned ETag on its ordinary
UpdateObjectAsync request. The observed If-Match value is the same valid quoted
strong tag as the uploaded object's JSON etag. The emulator's existing metadata
and media GET validators use that same representation identity. The SDK's ETag,
compression and precondition behavior remain enabled.

If-Match succeeds for a matching strong tag, a matching member of a list, or a
sole wildcard when the selected object exists. Repeated field instances form
one list. Weak tags do not satisfy strong comparison. Stale or nonmatching tags
return 412 conditionNotMet; malformed values return local 400 invalidArgument.
Parsing checks every list member, so a matching prefix cannot hide malformed
trailing input. Wildcard/list mixtures and repeated wildcards are invalid.

Existing authorization, generation selection, query preconditions and customer
key validation remain in force. The catalog calls the existing validation
callback inside its immediate mutation transaction, before ACL or metadata
changes. That callback compares the selected current record's ETag and retains
the existing read-only-field validation. A concurrent metadata change therefore
cannot pass an outdated tag. Missing resources retain 404, and strict permission
denials precede conditional evaluation.

If-None-Match remains unsupported for metadata writes. Standalone date conditions
and If-Range reject explicitly. If-Match takes precedence over
If-Unmodified-Since as in the existing RFC comparison rules; this does not add
date comparison. Other write routes retain their existing conditional-header
rejections. The implementation adds no schema, catalog API, dependency or
unbounded body buffering.

The feature ledger and SPEC-METADATA-IF-MATCH gap own status and provider limits.
The broader operation rows remain partial.

## Sources

Checked on 2026-10-30:

- [Google JSON headers](https://docs.cloud.google.com/storage/docs/json_api/v1/parameters),
  retrieved HTML SHA256
  a9ce4895e7a184aec46797845da5a1336af67a1662eea312db145f0981eb6161.
  Its If-Match section documents GET support, not metadata PUT/PATCH parity.
- [Official Storage client update](https://github.com/googleapis/google-cloud-dotnet/blob/7db3d26977d60289be1a57fbaf3e7c3e26424e24/apis/Google.Cloud.Storage.V1/Google.Cloud.Storage.V1/StorageClientImpl.UpdateObject.cs),
  Google.Cloud.Storage.V1 5.0.0, commit
  7db3d26977d60289be1a57fbaf3e7c3e26424e24, SHA256
  63adb10b218f9e72a8114cbd57a0a6de291d70c0c4213277bb0d35030719e61d.
  It passes the object directly to the generated update request and invokes
  ModifyRequest only for non-null options. The observed null-options request
  has no generation query conditions.
- [Official common request ETag behavior](https://github.com/googleapis/google-api-dotnet-client/blob/dd7f8ffe5a9aa3b9b07174dc03faf0f3289e7e24/Src/Support/Google.Apis/Requests/ClientServiceRequest.cs),
  Google.Apis 1.76.0, commit dd7f8ffe5a9aa3b9b07174dc03faf0f3289e7e24,
  SHA256 bc6a47e8e92e0519ebec281cdc36ba3e7296243306495497dc9d9df266525dbd.
  AddETag reads the request body's ETag; GetDefaultETagAction selects IfMatch
  for PUT, POST, PATCH and DELETE. Actual Storage 5.0.0 with Gax.Rest 4.13.1
  emitted the returned quoted tag on a gzip-compressed metadata PUT.
- [RFC 9110 strong comparison](https://www.rfc-editor.org/rfc/rfc9110.html#section-13.1.1),
  reused by the existing shared parser. Provider-specific syntax/error ordering
  outside this declared local scope remains unverified.

## Acceptance

The four tests in tests/http_metadata_etag.rs cover a gzip JSON full-resource
PUT, fresh/stale tags, combined query conditions, POST override, retained media
bytes, wildcard/list/repeated-field comparison, weak and malformed tags,
unsupported conditional headers, strict denials and missing resources.
Two concurrent updates with the same tag produce exactly one success and one
412, with one metageneration increment and the same object generation.

All four dedicated tests pass. The existing object metadata six-case and
read/range sixteen-case suites also pass after preserving every rejection
outside the newly supported metadata If-Match subset. Both shared RFC parser
tests pass. Unfiltered cargo check and targeted Clippy pass. Actual SDK and
final combined distribution checks are recorded separately by the integration
owner; passing these wire cases does not establish the complete SDK or Google
service contract.
