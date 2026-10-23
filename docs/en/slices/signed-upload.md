# V4 RSA signed upload

<!-- markdownlint-configure-file {"MD013": false} -->

Contract checked **2026-10-23**. This slice joins the
[signed-download verifier](signed-download.md) with [bounded XML PUT](xml-put-object.md).
It covers path-style V4 RSA query GET/HEAD/PUT in the strict local profile.
The ledger owns implementation status; the [combined evidence](../evidence/signed-urls.md)
records the final integrated gate.

## Contract sources

- [Canonical requests](https://docs.cloud.google.com/storage/docs/authentication/canonical-requests)
- [Signatures](https://docs.cloud.google.com/storage/docs/authentication/signatures)
- [Manual V4 signing](https://docs.cloud.google.com/storage/docs/access-control/signing-urls-manually)
- [Signed URLs](https://docs.cloud.google.com/storage/docs/access-control/signed-urls)
- [XML PUT](https://docs.cloud.google.com/storage/docs/xml-api/put-object-upload)
- [XML headers](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers)
- [XML errors](https://docs.cloud.google.com/storage/docs/xml-api/reference-status)

Existing Discovery/protobuf pins describe the shared resource model. These
XML/signing pages define the wire contract; their digests are in the evidence.

## Acceptance cases

| Scope | Required result |
| --- | --- |
| Query authentication | RSA, credential scope, required fields, actual Host including port, encoded path/query, method, declared headers and injected-clock time bounds before body polling; no credential fallback |
| Signer authorization | Existing project-plus-bucket checks: create for PUT, additionally delete on live overwrite; advisory check before body and authoritative check at publication; viewer/role-less denial, creator new object, bucket grant/revocation and admin overwrite |
| Canonical payload | Always UNSIGNED-PAYLOAD for query signatures, including optional content-sha256; reject unsupported Authorization-header signatures in strict mode |
| Optional SHA256 | One lowercase 64-hex digest or UNSIGNED-PAYLOAD; verify declared digest incrementally through the bounded bridge, signed or unsigned; no End marker/publication after mismatch |
| Signed headers | Missing/changed Content-Type, Content-MD5, metadata or generation match fails; standard unsigned extras retain XML semantics; used x-goog/x-amz headers must be signed except optional content-sha256 |
| Method/resource | PUT cannot GET/HEAD/DELETE or target another bucket/object/query; GET cannot PUT; Host changes fail without proxy/advertised-origin rewriting |
| Preservation | Signature/expiry/permission denial before body; digest/condition failures preserve live bytes, metadata and generation; two admitted PUTs with the same generation match yield one 200 and one 412 |
| CORS | Stored XML bucket rules wrap signed PUT preflight and actual success/error; CORS grants no permission; browser execution is the following slice |
| Helpers/SDKs | Native/container url sign --method PUT with headers works; pinned Go SignedURL PUT with ContentType and Ruby File#signed_url GET/PUT pass under recorded endpoint adaptations |

## Local rules and pending scope

SPEC-SIGNED-PAYLOAD separates the documented query payload rule from SDK
substitution when an optional digest is supplied. The server/helper follow
UNSIGNED-PAYLOAD and add streamed integrity checking for a declared digest.
Malformed strict signed declarations return 400 InvalidAuthentication; malformed
permissive query declarations return 400 InvalidDigest; a completed body mismatch
returns 400 BadDigest. Provider optional-header interactions and exact error
mapping remain unverified, so DESIGN-037 stays PARTIAL. A signed digest header
is bound; an unsigned digest can be changed by a URL holder and checks integrity.

Go storage v1.70.0 `storage.go:871-883` substitutes a supplied digest into its
payload line. The unmodified digest-bound PUT is explicitly tested to fail
403 SignatureDoesNotMatch for both matching and changed bodies, preserving the
previous resource. Omit SDK content-sha256/SHA256 signing options; use the local
helper to bind a digest. This source conflict requires opted-in real-service
differential evidence before changing the declared rule; no second canonical
payload form is accepted.

Unsigned standard headers such as Content-Type and Cache-Control keep their XML
semantics; sign them to constrain their values. An unsigned required extension
header returns 400 InvalidAuthentication. Missing/changed signed values return
403 SignatureDoesNotMatch after canonical whitespace normalization. Duplicate
fixed/custom headers still follow XML PUT's ExcessHeaderValues rule even when
their canonical signature is valid. Unsupported XML extensions fail explicitly.

The bridge has two copied 64 KiB slots and one fixed SHA256 context when requested.
It never buffers the complete body or publishes before integrity validation.
Existing admission, size/time limits, disk staging and transaction/recovery apply.
Pre-EOF cancellation prevents publication; a post-EOF commit may finish after
response cancellation. No new RSS/cgroup, power-loss or fsync-boundary claim is made.

SPEC-GO-SIGNER-PORT records canonical Host port omission: unmodified default-port
URLs pass through a guarded local dialer; independent explicit-port download
signatures are separate evidence. SPEC-RUBY-SIGNER-ENDPOINT records Ruby's fixed
default canonical Host and its supported bucket-bound option with an explicit
bucket/name File path, preserving final host:port without URL rewriting.

Virtual-host routing, response overrides, provider error/duplicate parity, PAP,
HMAC, Authorization-header V4, V2, POST policy, signed resumable, actual browsers
and Rails retain their pending rows.
