# JSON object metadata If-Match

## Contract and limits

Object metadata PUT and PATCH, including POST with X-HTTP-Method-Override: PATCH,
accept If-Match and do not evaluate it. Cloud Storage does the same: in the
opted-in recording of 2026-11-17 (case `obj-gaps-json`,
[provider probes](../evidence/provider-differential-gap-probes.md)) a PATCH
naming the current entity tag, a PATCH naming a superseded one and a PUT naming
`"bogus"` each answered 200, applied its change and advanced the metageneration.
Until that recording the emulator compared the tag and answered a stale one with
412, a protection the provider does not give; a test that relied on it would
pass locally and lose an update on Cloud Storage.

The pinned .NET Storage client sends an object's returned ETag on its ordinary
UpdateObjectAsync request, so its update keeps succeeding. Every other value,
including a weak, empty or malformed one, is ignored the same way; only
well-formed tags were recorded, and ignoring the others follows from not
evaluating the header.

Only the query conditions (`ifGenerationMatch`, `ifMetagenerationMatch` and
their NotMatch forms) guard a concurrent update; they are evaluated inside the
catalog's immediate mutation transaction. Authorization, generation selection
and customer key validation are unchanged. Missing resources keep 404, and strict
permission denials keep 401 or 403.

If-None-Match, standalone date conditions and If-Range on these writes, and
If-Match on other write routes, keep their 400: no recording shows what Cloud
Storage does with them there. A date condition sent together with If-Match is
accepted as before, because If-Match takes precedence over If-Unmodified-Since.

The feature ledger and the resolved SPEC-METADATA-IF-MATCH gap own the status.
The broader operation rows remain partial.

## Sources

Checked on 2026-10-30:

- [Google JSON headers](https://docs.cloud.google.com/storage/docs/json_api/v1/parameters),
  retrieved HTML SHA256
  a9ce4895e7a184aec46797845da5a1336af67a1662eea312db145f0981eb6161.
  Its If-Match section documents GET support, not metadata PUT/PATCH behavior;
  the 2026-11-17 recording settles that behavior.
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

The four tests in tests/it_protocol/http_metadata_etag.rs cover a gzip JSON
full-resource PUT with the returned tag, PATCH with a superseded and a bogus tag
(both applied), a failing `ifMetagenerationMatch` that still answers 412 with
the current tag, POST override, retained media bytes, weak, empty, list,
repeated and malformed values (all ignored), the conditional headers that stay
rejected, strict denials and missing resources. Two concurrent updates with the
same tag both apply; two with the same `ifMetagenerationMatch` produce exactly
one success and one 412, keeping the object generation.

The replay of case `obj-gaps-json` matches the recording for these steps with
no declared local choice.
