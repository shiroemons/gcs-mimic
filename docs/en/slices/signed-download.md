# V4 RSA signed downloads

<!-- markdownlint-configure-file {"MD013": false} -->

The integrated [signed-upload contract](signed-upload.md) adds PUT and supersedes
the initial optional content-sha256 substitution below: query payload is always
UNSIGNED-PAYLOAD, with supplied digests checked against the streamed body. See
SPEC-SIGNED-PAYLOAD and the [combined evidence](../evidence/signed-urls.md).
The later [response override](xml-response-overrides.md),
[virtual-host](xml-virtual-host.md) and [PAP](public-access-prevention.md)
contracts supersede the initial unsupported/pending statements below for those
declared scopes. Their combined gate is recorded in the same final evidence set;
other formats, provider edge parity and actual signed browsers remain pending.

Frozen local scope, checked against official sources on 2026-10-23: strict path-style XML GET/HEAD authenticates V4 RSA query signatures as registered local principals. Shared catalog authorization evaluates project and bucket `storage.objects.get` grants inside the existing read transaction. Public grants never rescue a failed signature. No Google IAM, ADC or metadata-server request is made.

## Keys and profiles

An identity may add `"public_keys": ["-----BEGIN PUBLIC KEY-----\n...\n-----END PUBLIC KEY-----\n"]`. These are inline RSA SubjectPublicKeyInfo PEM strings, at most eight per identity, 4096 bytes each, within the existing 64 KiB registry limit. Moduli must be 2048–8192 bits and odd; the positive odd exponent must be 3 through 2^33−1. Only the RSA encryption OID and minimal DER are accepted. Mixed PEM blocks, private keys, malformed DER and unsupported keys fail before readiness. `tokens` remains required but may be empty for an identity with keys. Multiple keys support local rotation; the verifier tries only that signer's bounded set.

The default permissive profile continues to ignore credentials and bypass permissions. The XML operation parser recognizes V4 credential fields, so permissive signed reads work without keys and without cryptographic guarantees. Strict XML authentication validates the original request before using that parsed operation; strict JSON routes reject query credentials. V2, HMAC, AWS aliases and Authorization-header signing remain unsupported. The deliberately public `fixtures/signing/test-only-*.pem` keys are only for isolated tests.

## Canonicalization and validity

The method, original encoded path, complete query except `X-Goog-Signature`, and sorted signed headers bind the signature. Existing percent escapes retain their spelling; reserved literal bytes are encoded, without collapsing slash or dot segments. Query parsing validates UTF-8/percent escapes, decodes form spaces, sorts decoded names by code point and then RFC 3986 encodes names/values. Duplicate decoded query names are rejected under the existing bounded local parser (SPEC-SIGNING-EDGES). Any of the six V4 fields, including Date/Expires/SignedHeaders alone, requires complete valid authentication in strict mode; partial fields never become anonymous under a public grant. The operation parser recognizes credential field names, while authentication validates the original URI before any parsed operation is used.

`GOOG4-RSA-SHA256` uses RSASSA-PKCS1-v1_5 SHA-256 through ring 0.17.14. Credential scope is `<identity>/<YYYYMMDD>/<location>/storage/goog4_request`; location must be nonempty and scope date must equal the UTC `X-Goog-Date` day. All six signing parameters are required exactly once. Signature hex must be nonempty, even-length and at most 2048 characters. `X-Goog-Expires` is decimal 1–604800 seconds.

The actual single nonempty Host header, including a non-default port, must be signed. Forwarded headers and configured advertised endpoints never rewrite it. Signed header names are lowercase, sorted and unique; sequential ASCII whitespace collapses, edge whitespace disappears, and repeated values join in wire order with commas. All supplied `x-goog-*`/`x-amz-*` headers must be signed except content-sha256. Query signatures default to `UNSIGNED-PAYLOAD`; when `x-goog-content-sha256` is present its single validated value supplies the canonical payload, matching the pinned Go signer. The canonical-request guide describes query URLs as `UNSIGNED-PAYLOAD`, while the SDK explicitly substitutes this header: SPEC-SIGNING-EDGES retains that source ambiguity. Empty GET/HEAD bodies accept only `UNSIGNED-PAYLOAD` or the SHA-256 of empty bytes. XML still rejects unsupported header semantics.

The injected clock accepts from active time minus the documented 900-second skew through strictly before active time plus expires. The exact expiry instant fails. This follows the official signatures active-datetime rule rather than inventing a zero-skew interval. Range may be unsigned; generation, bucket, object, host, signed headers and method remain signature-bound. GET and HEAD require independently signed methods. Queries outside the supported response overrides return 400 InvalidArgument, even with valid signatures. PAP is not modeled in this original slice; its later implementation is documented separately.

For an absolute-form request target, its authority must exactly match the actual canonical Host; a conflict fails with `SignatureDoesNotMatch`. Origin-form targets use the actual Host directly. This is an explicit local conflict rule, retained in SPEC-SIGNING-EDGES pending provider-specific authority precedence evidence.

## XML errors

| Condition | Status/code |
| --- | --- |
| Missing, malformed, duplicate signing fields or unsupported algorithm; mixed Authorization | 400 InvalidAuthentication |
| Unknown signer or no registered RSA key | 403 InvalidAccessKeyId |
| Cryptographic mismatch or missing signed header | 403 SignatureDoesNotMatch |
| Too early beyond the documented skew | 403 RequestTimeTooSkewed |
| At or after expiration | 400 ExpiredToken |
| Valid signer without effective read permission | 403 AccessDenied |
| Valid signature with unsupported operation/query/header | 400 InvalidArgument |

HEAD keeps the normal XML status and headers with no body. The documented status catalog supports these names/statuses, but precise messages, precedence and query-specific early/expiry mappings remain local choices in SPEC-SIGNING-EDGES; this is not a claim of complete provider wire parity.

## Local helper

```sh
gcs-mimic url sign --base-url https://localhost:4443 --bucket example-bucket \
  --object 'photos/a b.jpg' --method GET --expires 15m \
  --signer local-viewer@example.invalid --private-key fixtures/signing/test-only-private.pem
```

The helper omits explicit default HTTP port 80 and HTTPS port 443 before constructing and signing the final URL, matching browser/curl Host behavior. Non-default ports and optional proxy paths are retained exactly; no host is rewritten after signing. GET, HEAD and PUT are accepted for future XML-write reuse; signing PUT does not implement a PUT server route. `--expires` accepts integer seconds or an `s`, `m`, `h`, `d` suffix. Repeat `--header NAME:VALUE` to sign required headers, including repeated names; Host is derived and cannot be overridden. `--timestamp YYYYMMDDTHHMMSSZ` supplies deterministic UTC time. RSA PKCS#8/PKCS#1 private PEM input is bounded to 16 KiB and used only by this command. It reads no server configuration, contacts no network and changes no permission.

## Pinned Go SDK endpoint limitation

Storage v1.70.0 `storage.go:854` builds canonical Host with `URL.Hostname()`, which removes a non-default port even when `SignedURLOptions.Hostname` includes it. Unmodified SDK URLs therefore fail with 403 `SignatureDoesNotMatch` when sent with the actual host:port. The emulator deliberately retains exact Host verification. Use `gcs-mimic url sign` for explicit local ports, or expose a default-port endpoint. SPEC-GO-SIGNER-PORT records the upstream limitation; no upstream code or manifest is patched here.

The Go test proves an unmodified `storage.SignedURL` succeeds at a portless authority. A guarded client dialer maps only that authority to the isolated ephemeral listener, so no privileged listener, external request or Host rewrite is needed. A separate test asserts failure of unmodified SDK URLs at non-default ports. Its independent Go canonical request is checked against the SDK `SignBytes` message, then corrects only the Host port and signs with Go's standard RSA implementation. The successful corrected host:port case is an independent cryptographic cross-check, not a claim that unmodified SDK signing supports such ports. Both workflows run over HTTP and trusted HTTPS.

## Acceptance inventory

- Verify private GET/HEAD, signed ranges, live generation selection, project grants, bucket-only grants and revoked/absent permissions.
- Reject mutations of method, bucket, object, generation, query add/remove/change, host, port, signed headers and signature; never fall back through public grants.
- Verify exact expiration boundaries, 900-second early skew boundary, invalid duration/date/scope and unknown identities.
- Exercise Unicode, encoded reserved bytes, percent spelling, slash/dot-like keys, sorted/encoded queries, duplicate rejection and repeated/whitespace headers.
- Fail malformed or private registry keys before readiness; exercise helper output/options and key bounds.
- Cross-check independently with pinned Go `storage.SignedURL` v1.70.0, including trusted HTTP/HTTPS host:port and tamper rejection; retain existing Rust regressions.

Virtual-host routing, signed XML writes, signed browser/Rails flows, cloud differential execution and resource benchmarking remain separate work. Broader rows retain their pending cases.

## Sources

- [Canonical requests](https://docs.cloud.google.com/storage/docs/authentication/canonical-requests), updated 2026-10-21.
- [Signatures and active datetime](https://docs.cloud.google.com/storage/docs/authentication/signatures).
- [Manual V4 signing](https://docs.cloud.google.com/storage/docs/access-control/signing-urls-manually).
- [Signed URLs](https://docs.cloud.google.com/storage/docs/access-control/signed-urls).
- [XML headers](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers) and [XML status codes](https://docs.cloud.google.com/storage/docs/xml-api/reference-status).
- [Pinned Go signer](https://github.com/googleapis/google-cloud-go/blob/storage/v1.70.0/storage/storage.go).
