# XML response header overrides

<!-- markdownlint-configure-file {"MD013": false} -->

Contract checked **2026-10-23**. Features: DESIGN-035, EXT-XML-012 and
EXT-XML-017. The feature ledger remains authoritative; these rows stay PARTIAL
for their existing provider-exactness and broader method cases. This slice
completes the declared local response-override behavior without narrowing them.

## Official contract and local scope

The [GET Object reference](https://docs.cloud.google.com/storage/docs/xml-api/get-object-download)
links to the [XML header and query reference](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers).
That reference lists exactly two response override parameters:

| Parameter | Response header |
| --- | --- |
| `response-content-type` | `Content-Type` |
| `response-content-disposition` | `Content-Disposition` |

Their documented values are URL-encoded headers, and the reference describes
authenticated GET requests. This emulator also applies them to authorized
anonymous/public and permissive reads and to HEAD, as an explicit local extension
recorded in SPEC-XML-RESPONSE-OVERRIDES. Strict bearer and V4 RSA signed requests
still require the selected object's effective read permission. Query options
never grant access or change stored metadata.

The bounded XML parser decodes names and values once. Each override replaces
only its response header after metadata is built from the same selected record
as the bytes. Omitted overrides retain stored metadata and normal defaults.
Whole reads, retained-generation selection, range reads and HEAD share this
behavior. HEAD retains the full representation length and sends no body;
satisfiable Range adds Content-Range under the existing HEAD rule. GET ranges
retain 206 and the selected byte count. ETag, whole-object hashes, generation,
metageneration, stored length/encoding, other metadata and Cache-Control are
unchanged. Existing bucket CORS rules determine exposed headers; an override
does not add an exposure or bypass authorization.

No object body buffering, storage schema, transactional or canonical-path change
is introduced. The existing 16 KiB query limit, bounded streaming and immutable
selected-generation reader remain in effect.

## Validation and failure behavior

Values must be representable as an HTTP HeaderValue. CR, LF, NUL, DEL and other
invalid controls return XML 400 `InvalidArgument`; HEAD suppresses the XML body.
Empty values, HTAB and valid UTF-8 bytes are accepted by this local validation.
This does not validate the full MIME or Content-Disposition grammar. Prefer
standards-compliant media types and disposition syntax, including encoded
filename parameters where needed. Exact provider grammar, empty/non-ASCII
handling and error precedence remain SPEC-XML-RESPONSE-OVERRIDES.

The official XML inventory does not list S3's `response-cache-control`,
`response-content-language`, `response-content-encoding` or `response-expires`.
These and other unknown `response-*` names return 501 `NotImplemented`, as do
other unsupported query parameters. PUT continues to reject response overrides.
The provider's unknown-name behavior is not established by omission from an
inventory, so the rejection remains an explicit local rule.

## Canonical query and duplicate names

The [canonical-request contract](https://docs.cloud.google.com/storage/docs/authentication/canonical-requests)
includes every subsequent query parameter except the signature and specifies
lexicographical name order by code point. The existing implementation decodes
names, sorts them, then encodes names and values. Overrides therefore participate
in the signature automatically. Changing either override on a valid signed
GET/HEAD returns 403 `SignatureDoesNotMatch`, including on a public object.
Signature authentication precedes operation validation and never falls back to
an anonymous grant.

The official text does not define repeated-name ordering or how ambiguous
encoded aliases should behave. The local parser retains rejection of any
duplicate decoded name, including `response-content-type` together with
`response%2Dcontent-type`. Unsigned requests return 400 `InvalidArgument`;
strict signed requests return 400 `InvalidAuthentication`. No first/last-value
selection or repeated-value signing is implemented. SPEC-SIGNING-EDGES remains
open. The compatibility policy forbids shrinking an existing row merely to
remove deviations: DESIGN-035's original provider-exactness requirement therefore
remains pending even though the local override and duplicate-rejection tests pass.

## Signing helper and SDKs

Repeat `--query NAME=VALUE` to add literal, unencoded query names and values:

```sh
gcs-mimic url sign --base-url https://localhost:4443 --bucket example-bucket \
  --object report.csv --method GET --expires 15m \
  --signer local-viewer@example.invalid \
  --private-key fixtures/signing/test-only-private.pem \
  --query 'response-content-disposition=attachment; filename="report.csv"' \
  --query 'response-content-type=text/csv'
```

The helper encodes reserved bytes before signing. It rejects missing `=`, empty
names, duplicate names and attempts to supply reserved signing parameters.
The first `=` separates the name from the value; subsequent equals signs are
part of the value. Existing URL signing still binds the final host and port.

Pinned Go storage v1.70.0 `storage.SignedURL` uses `QueryParameters`; GET and HEAD
pass unchanged at a default-port authority through the existing guarded dialer.
SPEC-GO-SIGNER-PORT still covers non-default port signing. Pinned Ruby storage
1.62.1 `File#signed_url(query: ...)` passes with its existing bucket-bound final
host:port and explicit bucket/name path adaptation under
SPEC-RUBY-SIGNER-ENDPOINT. Neither SDK's generated URL or Host is rewritten after
signing. Trusted HTTPS, header values, exact bytes, tampering and metadata
preservation have [executed evidence](../evidence/xml-response-overrides.md).

## Acceptance and remaining work

Wire tests cover both override headers on whole/range GET and HEAD, private
bearer and V4 signer access, anonymous public access, retained-generation
selection, matching cache/hash/ETag metadata, CORS exposure, value mutation,
CR/LF/control rejection, duplicate encoded aliases, unsupported query rejection
and unchanged current/retained metadata. CLI tests verify encoding and signing
with repeated query options. Official Go/Ruby signer cases exercise their
documented query options with existing endpoint adaptations.

No real Google differential requests, container builds, guide walkthrough,
browser/Rails execution or new resource measurements run in this slice.
Provider duplicate/ambiguous-name behavior and override validation/public/HEAD
exactness remain explicit gaps. The coordinator owns final integrated container
and guide verification; broader XML features and other signing formats retain
their existing pending cases.
