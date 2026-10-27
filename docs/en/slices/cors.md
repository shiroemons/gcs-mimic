# JSON and bucket-configured XML CORS

<!-- markdownlint-configure-file {"MD013": false} -->

The selected scope is JSON browser bucket/object operations and the existing
path-style XML object GET/HEAD surface, in native and legacy-emulator profiles.
CORS changes browser response visibility; the shared authorization and object
mutation rules still apply. Signed URLs, XML writes, XML bucket `?cors`, browser
login/cookie identity and Rails direct uploads remain separate deliveries.

## Official inputs

Checked on 2026-10-23:

- [Cloud Storage CORS](https://docs.cloud.google.com/storage/docs/cross-origin): JSON defaults are independent of bucket rules; XML uses bucket rules; credentials and empty-request-method behavior differ.
- [Bucket resource](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets): `cors` fields, signed 32-bit max age and method wildcard.
- [XML bucket metadata](https://docs.cloud.google.com/storage/docs/xml-api/put-bucket-metadata): the first matching origin/method rule determines the response.
- [JSON HTTP headers](https://docs.cloud.google.com/storage/docs/json_api/v1/parameters) and [resumable uploads](https://docs.cloud.google.com/storage/docs/performing-resumable-uploads): common headers, Origin, Location and Range.
- [CORS configuration](https://docs.cloud.google.com/storage/docs/cors-configurations): origin wildcards, including subdomains, and array clearing.

Discovery revision `20261013`, SHA-256
`1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`,
matches the current official input. Storage v2 protobuf blob
`f56e4f3e0a0795ae8dc45f322477ee14b96d5291`, SHA-256
`a0df6132967b97a89be248f1f24d277ee8df8f8d885db1d4af6249469ff0c1cf`,
defines the corresponding CORS fields; protobuf transport is not implemented.
The manifest's former SHA-256 was corrected after raw/blob content and Git blob
identity verification. These input pins describe source content, not a cloud test.

## JSON defaults

On supported `/storage/v1/b`, `/upload/storage/v1/b` and
`/download/storage/v1/b` namespaces, a single nonempty Origin receives its own
value as Allow-Origin. Bucket CORS never restricts these endpoints.
Ordinary responses, including authorization errors, stale conditions, empty
DELETE and 304 responses, include Allow-Credentials true. OPTIONS bypasses
authentication and mutations, advertises DELETE/GET/HEAD/PATCH/POST/PUT,
echoes syntactically valid requested header names and uses max-age 3600.
A missing/empty request method returns 404 with Allow-Credentials true.
No Origin, duplicate/invalid Origins, internal probes and unrelated namespaces
do not receive a global allow-all response. Malformed requested header names
return 400 without a CORS grant. TRACE is absent from the advertised methods.

The supported exposed common response headers are Cache-Control,
Content-Disposition, Content-Encoding, Content-Language, Content-Length,
Content-Range, Content-Type, Date, ETag, Expires, Last-Modified, Location and Range.
This fixed subset supports cached reads and browser resumable uploads.
The provider's complete common-header list is not published; exact list parity
and undocumented malformed-input wire details remain research boundaries in
SPEC-CORS-EDGES. No custom `x-goog-*` header exposure is implied for JSON.
Preflight does not advertise Allow-Credentials; credential-mode `include`
with a preflight is outside this declared browser workflow. Simple credentialed
JSON reads are covered. Local bearer requests use the ordinary fetch credential
mode, without browser cookie authentication.

## Bucket configuration and XML reads

JSON create/GET/list/PATCH/PUT persist the ordered `cors` array. Each rule has
string arrays `origin`, `method`, `responseHeader` and optional nonnegative
signed-32-bit `maxAgeSeconds`. Omitted max age evaluates to 3600.
PATCH omission preserves rules; an array replaces them and `[]`/null clears
them. PUT omission preserves the rules under the local choice in SPEC-BUCKET-PUT,
without real-GCS verification. Unknown fields retain existing bucket
adapter behavior. Configuration is bounded by the existing 64 KiB metadata
body/catalog limits. Invalid types, unsafe header names/values and invalid age
fail before mutation. Updates use the existing atomic metageneration condition
and `storage.buckets.update` checks, with one revision per successful update.

The first rule matching the request Origin and method decides XML CORS.
Whole-origin `*` and scheme-specific `*.domain` origin patterns are supported;
domain, scheme and port boundaries are preserved. Documented HTTP port 80 and
HTTPS port 443 match their omitted equivalents; custom schemes infer no default
port. Custom source schemes and opaque origins are covered separately in the
[mobile-origin research](cors-mobile-origins.md). Header matching ignores case.
On preflight, every requested header must be allowed by that one rule; later
rules cannot repair a denial. Nonmatching/malformed preflight returns 200
without Allow-Origin. A match includes reflected Origin, configured methods,
requested headers and effective max age. Actual GET/HEAD exposes configured
response headers, including on an authorization failure. XML never includes
Allow-Credentials. Rules survive restart and clear/replacement takes effect on
new uncached requests. Browser caches can retain previous grants until expiry.

The resource schema permits method `*` and OPTIONS; storing either does not
implement those actual operations. An allowed PUT preflight still leads to
the existing unsupported XML PUT response. Preflight is not an authorization
or method-implementation claim. Requests use explicit configured origins;
forwarded headers do not affect resumable Locations or permissions.

## Acceptance and integrated gate

Wire cases cover JSON defaults, errors/validators, no bucket dependency,
strict denial after preflight, XML allow/deny/header/credentials behavior,
configuration replacement/clearing, validation, conditions, concurrent updates
and restart/migration. Actual Chromium over trusted local HTTPS verifies JSON
bucket/object CRUD, media/multipart/resumable upload and exposed ETag/Location,
plus XML GET/HEAD, preflight, denied origins/headers/methods, strict permissions,
revocation and persisted configuration after restart. Pinned Go/Ruby exercise
the basic API method scope; final native/container journeys and regressions
close the delivery gate. Execution results belong in the final evidence and
feature ledger; this contract alone is not a passing claim.
