# XML object read validators

Scope: EXT-XML-012 and EXT-XML-017 remain PARTIAL. Local GET and HEAD evaluate
If-Match, If-None-Match, If-Modified-Since and If-Unmodified-Since after
authorization, customer-key checks and selection of one immutable generation.
The same evaluator serves path-style and configured virtual-host objects,
including public objects and retained generations. JSON keeps its separately
observed validator precedence and date-header behavior.

Official inputs rechecked on 2026-11-04:

- [XML GET Object](https://docs.cloud.google.com/storage/docs/xml-api/get-object-download)
- [XML HEAD Object](https://docs.cloud.google.com/storage/docs/xml-api/head-object)
- [XML request headers](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers)
- [RFC 9110 evaluation order](https://www.rfc-editor.org/rfc/rfc9110.html#section-13.2.2)

The local evaluator first applies strong If-Match, otherwise If-Unmodified-Since;
a failure returns XML 412 PreconditionFailed. Then weak If-None-Match applies,
otherwise If-Modified-Since; a cache hit returns an empty 304. ETags override the
corresponding date condition. Modification dates compare at the second precision
used by Last-Modified. Invalid or repeated date headers are ignored. Malformed
ETag syntax returns local 400 InvalidArgument. Catalog generation/metageneration
conditions run first on the selected snapshot.

A 304 preserves ETag, Cache-Control, Last-Modified and any existing Expires/Vary
fields, including gzip negotiation. It has no body or object payload headers.
XML PUT, DELETE, bucket methods and listing still reject these HTTP conditions.
If-Range remains unsupported. Repeated Range fields are ignored by both JSON and
XML; one multi-range field keeps JSON 501 and XML 400. This is a documented local
choice, with range classification kept at each adapter's original evaluation point.

Mixed-condition error/header details and provider-specific XML validator
precedence remain unverified local RFC choices.

## Verification

Not run: SDK suites, container builds and provider differential tests.
