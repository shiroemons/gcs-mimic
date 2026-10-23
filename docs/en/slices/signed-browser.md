# Signed browser transfers

<!-- markdownlint-configure-file {"MD013": false} -->

Contract checked **2026-10-23**. This slice combines the existing local HTTPS
listener, persisted bucket XML CORS and strict V4 RSA query GET/HEAD/PUT.
It extends the [CORS](cors.md), [signed download](signed-download.md) and
[signed upload](signed-upload.md) contracts. The feature ledger owns status;
this contract and hermetic wire results alone do not prove browser execution.

## Official inputs

The following official sources were re-read on 2026-10-23. SHA-256 values identify
the complete fetched HTML, without vendoring the prose. Existing resource-schema
pins remain in `compatibility/upstream/source-manifest.json`; no schema or
dependency version changes are needed for this interaction.

| Source | SHA-256 |
| --- | --- |
| [Cloud Storage CORS](https://docs.cloud.google.com/storage/docs/cross-origin) | `97e09c1d3b1f69dd58f19cfc385d7060398907c94cbb5954fc37a4f26ed25520` |
| [CORS configuration examples](https://docs.cloud.google.com/storage/docs/cors-configurations) | `be8fdc2d0e94186fa7fb685381cfdf6080c05d1dd41e75518a9ac800f555ad18` |
| [Signed URLs](https://docs.cloud.google.com/storage/docs/access-control/signed-urls) | `0e1be89e4585c48aa8e303f0bdfc70403d50b7bee7ed3573c509e4e16cc22ad1` |
| [Canonical requests](https://docs.cloud.google.com/storage/docs/authentication/canonical-requests) | `3afa8287503a512561cdf3865bc47e2a532401f351b908946836bc4ec98f1ba9` |
| [Manual V4 signing](https://docs.cloud.google.com/storage/docs/access-control/signing-urls-manually) | `b3703b49de4450c7dae0d4de474c3480b658d8e36b15c12edb6001fadf76035b` |
| [Signatures and time bounds](https://docs.cloud.google.com/storage/docs/authentication/signatures) | `174a17dfbf7c1108d92c04bffff8d8e9a2343d743e6fc420d55025a484209e01` |
| [XML API headers and query parameters](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers) | `e0640aa8df91f33f188aee013e4f76bc1b40189bbc40b17396ec96f6fe03787a` |
| [Fetch snapshot](https://fetch.spec.whatwg.org/commit-snapshots/59afe349913d56898c947576216f1c2579308be7/) | `5d80be13cdc78b652732acf22002c17a3be3691b7c3de2cb9c26c794aad2b881` |

Google documents separate JSON defaults and bucket-configured XML CORS,
origin/method/header matching, effective max age and no XML credential support.
The configuration examples include a signed-URL PUT fetch. V4 signing binds
the method, final Host, encoded path, query and declared headers; signed URLs
use XML endpoints. The XML header reference defines ETag, generation, checksums
and generation-match inputs used by these tests.

Fetch [CORS-preflight fetch](https://fetch.spec.whatwg.org/#cors-preflight-fetch)
clones the original URL list, retaining the signed query, and uses OPTIONS with
`Access-Control-Request-Method` and optional `Access-Control-Request-Headers`.
Preflight excludes browser credentials, but does not remove query parameters.
The [response-header rules](https://fetch.spec.whatwg.org/#http-access-control-expose-headers)
make exposure an actual-response property; matching CORS headers can also share
an HTTP error. Wire headers and browser-visible headers require separate checks.

## Request and response boundary

Both `/<bucket>/<object>` and opt-in `<bucket>.<configured-domain>/<object>`
routes are covered. Virtual-host wire tests connect only to the isolated loopback
listener while sending the signed Host unchanged, without external DNS.
HTTPS virtual hosts additionally need a certificate covering that final name.
GET and HEAD have separately signed methods. PUT can bind Content-Type and
generation-match headers. Browser-generated Origin is normally unsigned.
The signer helper must receive the externally visible host/path before signing;
neither the browser nor a proxy may rewrite them afterwards.

For a recognized XML object path and bucket, CORS selection depends on its path,
Host, Origin and method, independently of object-name/query validation or
signature validity. The same raw bucket selection is used by the actual route;
an object path segment resembling another bucket never overrides a virtual host.
OPTIONS with Origin reads bucket rules without authenticating, verifying the
signature, reading object media, creating an object or reserving a capability.
The original complete signed query, an expired/invalid signature and even a
duplicate signing parameter do not change this CORS decision. The subsequent
actual request still authenticates and parses its original, unchanged URI.
Unknown buckets and unrecognized paths have no bucket CORS grant to expose.
Malformed transport framing or conflicting absolute-form authority can reject
before this wrapper; their exact provider CORS behavior is outside this scope.

The first origin/method matching rule controls preflight. Every requested header
must match that rule, ignoring header-name case; a later rule cannot repair a
header denial. A single valid Origin and single valid request-method/header-list
are required. Method tokens retain case. Missing/empty method or invalid header
syntax denies the grant. OPTIONS itself need not appear in stored methods.
With no requested-header list, a matching method/origin still succeeds.

| Response | Matching origin and method | Disallowed origin or method |
| --- | --- | --- |
| OPTIONS | 200, empty body, reflected Allow-Origin, configured Allow-Methods, requested Allow-Headers when present, effective Max-Age | 200, empty body, no `Access-Control-*` grant |
| Actual GET/HEAD/PUT success | Operation status, reflected Allow-Origin and configured Expose-Headers | Same authorization/operation result, no `Access-Control-*` grant |
| Actual signature/permission or input/condition error | Original XML status and error, reflected Allow-Origin and configured Expose-Headers | Same XML error, no `Access-Control-*` grant |

Preflight also denies every allow header when any requested header is disallowed.
Actual responses use `Vary: Origin`; preflight uses
`Vary: Origin, Access-Control-Request-Method, Access-Control-Request-Headers`.
Variation remains on denied and no-Origin requests to recognized object routes.
`responseHeader` supplies actual Expose-Headers, including after a preflighted PUT;
listing a name does not create a response header that the operation lacks.
The wire fixture exposes Content-Type, ETag, X-Goog-Generation and
X-Goog-If-Generation-Match. It does not expose the signature query.
Actual responses do not advertise preflight Allow-Methods or Max-Age.
`maxAgeSeconds` defaults to 3600; wire/browser change tests use zero to avoid a
cached grant. Browsers may impose their own cache ceiling.

The explicit local error contract retains CORS on 403 SignatureDoesNotMatch
(including changed Host port), 403 AccessDenied, 400 ExpiredToken,
400 InvalidAuthentication, 400 InvalidDigest and 412 PreconditionFailed.
Unsigned private requests return 401 AuthenticationRequired with the same CORS
decision. HEAD errors remain bodyless. Failed writes retain bytes, generation
and complete metadata. These status mappings and error CORS details are local
tested decisions, not resolved provider-exactness claims.

XML never sends Allow-Credentials. Signed query authentication uses the normal
fetch credential mode or `credentials: omit`; `credentials: include` remains
unsupported even with a valid signature. CORS controls browser visibility and
preflight permission to attempt a request. A valid signed write sent directly
can commit despite a disallowed Origin; CORS is not server authorization.
Changing or clearing a rule after preflight changes the actual response headers,
but does not revoke the signer's IAM permissions or an already cached response.
The current bucket policy is still checked for actual access. PAP suppresses
public grants while preserving valid explicitly authorized signer GET/HEAD/PUT.
JSON routes keep their independent defaults, including after XML rules clear.
Stored rules and object state survive normal stop/start; a changed listener port
requires newly signed URLs, never rewriting an existing signature.

## Hermetic acceptance cases

All following tests live in `tests/signed_browser_http.rs`, run in the strict
local profile and exercise both path-style and virtual-host routing. They use
`tests/common` process/directory helpers and the existing public test RSA fixture.
No Google, ADC, metadata server, browser or external DNS is involved.

| Case ID | Acceptance | Wire test |
| --- | --- | --- |
| SIGNED-PREFLIGHT-QUERY | Full PUT signature query, malformed/duplicate/expired declarations and unknown/invalid operation-query inputs bypass authentication on OPTIONS; empty response; absent destination and unchanged existing object; actual URL remains reusable | `signed_put_preflight_ignores_signature_and_never_publishes` |
| SIGNED-PREFLIGHT-DENIAL | Allowed method/headers and case-insensitive names; denied origin/method/header, lowercase method, missing/empty method and malformed header list have no grant; max-age zero and preflight Vary | `signed_put_preflight_ignores_signature_and_never_publishes` |
| SIGNED-TRANSFER-HEADERS | Signed Content-Type GET/HEAD/PUT, exact bytes, bodyless HEAD, ETag/generation and configured exposure, reflected Origin and Vary, no credentials header | `signed_transfers_expose_bucket_headers_without_granting_credentials` |
| SIGNED-ORIGIN-DENIAL | Valid actual GET/HEAD/PUT still succeeds for a denied or absent Origin without Allow-Origin | `signed_transfers_expose_bucket_headers_without_granting_credentials` |
| SIGNED-ERROR-CORS | Tampered query, changed signed Host port, expired/duplicate/partial signing declaration, role-less signer and unsigned anonymous GET/HEAD/PUT retain status and allowed/denied-origin CORS decisions | `signed_failures_keep_cors_and_preserve_object` |
| SIGNED-ATOMIC-FAILURES | Every rejected request preserves metadata; 400 invalid digest and signed 412 generation-match failure preserve bytes/generation with allowed/denied-origin CORS | `signed_failures_keep_cors_and_preserve_object` |
| SIGNED-RULE-CHANGES | Replacing rules after a successful preflight suppresses the actual PUT grant without changing authorized publication; clearing suppresses signed GET visibility | `signed_rules_change_between_preflight_and_actual_and_survive_restart` |
| SIGNED-CORS-RESTART | Graceful stop/start preserves metadata, bytes, rules and subsequent signed GET/preflight behavior on both endpoint styles | `signed_rules_change_between_preflight_and_actual_and_survive_restart` |
| SIGNED-JSON-INDEPENDENCE | Authenticated JSON media remains readable with its own Allow-Credentials and reflected origin after XML CORS clearing | `signed_rules_change_between_preflight_and_actual_and_survive_restart` |
| SIGNED-PERMISSIONS | Matching preflight cannot authorize role-less signed or unsigned actual reads/writes; unchanged object after denials | `signed_preflight_never_grants_permission_and_pap_preserves_signer_access` |
| SIGNED-PAP | Under enforced PAP and a stored allUsers grant, explicit signer GET/HEAD/PUT retains matching CORS while public-only signer and anonymous access stay denied | `signed_preflight_never_grants_permission_and_pap_preserves_signer_access` |
| SIGNED-BUCKET-SELECTION | Two buckets with distinct rules select the virtual-host bucket before the path; path-style selects its own bucket for preflight and actual PUT/GET | `signed_cors_selects_the_host_bucket_before_the_path_bucket` |
| SIGNED-ENCODED-PATHS | Encoded slash/space object names preserve preflight and signed PUT/GET; tampered encoded names retain CORS on signature failure | `signed_encoded_paths_keep_preflight_transfer_and_error_cors` |
| SIGNED-INVALID-NAMES | Invalid object names receive metadata-only preflight and matching-origin CORS on actual 400 errors, including malformed percent paths; validation and object preservation remain enforced | `signed_invalid_object_names_keep_error_cors_without_bypassing_validation` |

Existing `auth_http` CORS tests retain first-rule, malformed/duplicate Origin,
default max-age, wildcard and basic permission cases. Existing signing tests
retain exact injected-clock boundaries, signed-header tampering and atomic
permission checks at publication. This slice does not replace or relax them.

## Actual-browser acceptance owned by the browser harness

`scripts/signed-browser.sh` and `scripts/signed-browser.html` own the following
checks over distinct, trusted local
HTTPS origins. They must execute in an actual browser; a Rust socket or curl
cannot establish that JavaScript sees a response or that a denied request was
blocked. The browser harness and integrated evidence are maintained separately.

| Case ID | Required actual-browser observation | Browser test IDs |
| --- | --- | --- |
| BROWSER-SIGNED-TRANSFERS | Actual signed PUT then independently signed GET/HEAD, unchanged final URL/Host/path, exact bytes, readable length and signed response overrides | `signed-browser-strict-initial`; GET also `signed-browser-strict-restart` |
| BROWSER-SIGNED-PREFLIGHT | Browser-generated OPTIONS permits PUT with signed Content-Type/metadata; denied method/header/origin blocks transfer and JSON checks confirm no publication or overwrite | `signed-browser-strict-initial` |
| BROWSER-SIGNED-EXPOSURE | Configured ETag/generation/hash are readable; non-exposed metageneration is hidden | `signed-browser-strict-initial`, `signed-browser-strict-restart` |
| BROWSER-SIGNED-ERRORS | Tampered/expired URL and changed signed-header failures are readable XML HTTP errors with unchanged bytes and metadata | `signed-browser-strict-initial` |
| BROWSER-SIGNED-PERMISSIONS | Successful preflight does not authorize a viewer whose upload grant was revoked, or anonymous private GET/PUT | `signed-browser-strict-initial` |
| BROWSER-SIGNED-RULES | Fresh requests observe replaced rules; independent JSON verifies unchanged state while XML is blocked; stored rules and signed download bytes work after restart | `signed-browser-strict-initial`, `signed-browser-strict-restart` |

Full-query preflight handling, malformed signing declarations, 412 CORS, PAP and
clearing are hermetic cases above; they are not additional claims about this
browser harness. Its network trace need not expose a signature to demonstrate
the automatic preflight. Existing basic CORS browser coverage separately checks
XML credential-mode include rejection; the signed harness adds no credential
support. Integrated browser execution and artifacts are recorded separately in
`docs/en/evidence/signed-browser.md` and `docs/en/evidence/signed-browser.txt`.

## Open source questions and remaining scope

`SPEC-CORS-EDGES` and `SPEC-SIGNING-EDGES` in `compatibility/gaps.json` remain
**OPEN**. This slice does not edit them. Proposed additions to their evidence are:

- The CORS guide advises against adding OPTIONS to configuration, while the
  configuration examples include it. The guide also spells the requested-method
  header in the plural in one table; Fetch specifies the singular. The local
  contract uses the singular and does not require OPTIONS in a rule. Obtain
  clarification before claiming the conflicting examples establish new semantics.
- The CORS page's exposure description mixes preflight and simple-request wording;
  Fetch places Expose-Headers on actual responses. The local rule applies exposure
  to actual PUT and errors too. Exact Google header presence, ordering and Vary
  on each signed 400/403/412 response, malformed queries and early authority
  failures still require authoritative clarification or opted-in cloud evidence.
- Preflight retains the complete query under Fetch. Ignoring invalid/expired or
  duplicate signature declarations for metadata-only OPTIONS is tested locally;
  provider malformed-query precedence is not established by the signing pages.
- Existing signing questions about duplicate/ambiguous query encoding, expiry
  mappings and mixed-failure precedence remain unresolved. Existing CORS questions
  about complete JSON exposure, mobile origins and malformed inputs also remain.

No real Google differential, real iOS/WebKit, cookie identity, Rails Active
Storage, signed resumable upload, POST policy, HMAC or other signing format is
added. Resource/durability guarantees remain those of the existing upload and
TLS implementations; no new benchmark or crash-boundary claim is made.
