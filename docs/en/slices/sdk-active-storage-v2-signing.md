# Legacy V2 RSA signed URLs

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: DESIGN-040 (PARTIAL) and DESIGN-084 (PARTIAL). No new configuration or dependencies.

This records the original RSA/SDK slice and its historical acceptance results.
The later [V2 HMAC/header slice](xml-v2-hmac-signatures.md) adds HMAC queries,
GOOG1/AWS headers, HEAD and virtual-host object resources. Its current supported
subset and remaining questions supersede the original exclusions below; the
parent feature still remains PARTIAL.

## Acceptance contract

| Case | Required behavior | Result |
| --- | --- | --- |
| V2-DETECT | Require exactly one nonempty GoogleAccessId, Expires and Signature; reject mixed V2/V4 credentials, missing fields and duplicates with XML 400. | PASS |
| V2-EXPIRY | Parse unsigned Unix seconds; strict expiry uses the injected clock and XML 400 ExpiredToken; malformed/negative values return 400. | PASS |
| V2-PERMISSIVE | Like V4, ignore signature credentials and expiry in permissive mode; retain V2 structural validation. | PASS |
| V2-STRICT | Verify RSA-SHA256 using registered public keys and the official verb/MD5/type/expiry/extension-header/resource string; reuse V4 signer permissions and failures. | PASS |
| V2-GET | Unchanged Go/Ruby path-style signed GET downloads exact private bytes; method, resource and signed-header changes fail. | PASS |
| V2-PUT | Unchanged signed PUT preserves bytes; changed/missing signed Content-Type or MD5 fails; existing streaming XML PUT validates body MD5 before publication. | PASS |
| AS-SIGNED-URL | Unchanged blob.url fetches bytes; retain the existing OAuth authorization-removal workaround classification. | PASS_WITH_WORKAROUND |
| AS-DIRECT-UPLOAD | Unchanged url_for_direct_upload and headers_for_direct_upload accept PUT and exact readback with that same workaround. | PASS_WITH_WORKAROUND |
| V2-HMAC | GOOG1/HMAC signing. | PENDING |
| V2-POST | Signed POST policies. | PENDING |
| V2-RESUMABLE | Signed resumable initiation. | PENDING |
| V2-VIRTUAL-HOST | Bucket-prefixed canonical resources for virtual-host endpoints. | PENDING |

The supported subset is path-style object GET/PUT. The received URL is never rewritten.
V2 does not cryptographically bind Host or ordinary response override queries; unlike V4,
its resource is the literal encoded path. Unsupported XML subresources remain rejected.
Virtual-host differences, HMAC, POST and resumable initiation remain outside this slice.
HEAD and generation selection are also explicitly rejected for V2; no supported
request is verified with a rewritten host, path or query. Unsupported HMAC identities
fail with the shared unknown-signer XML error and RSA registry guidance.
The existing public-key registry and XML authorization path apply without new settings.

## Sources

Checked 2026-10-27: [official V2 signing process](https://docs.cloud.google.com/storage/docs/access-control/signed-urls-v2)
(page updated 2026-10-23) and [XML errors](https://docs.cloud.google.com/storage/docs/xml-api/reference-status).
The retrieved V2 page SHA-256 is `81697542744cf91201634db04faaaaf1e3d6d574ec6f5313b0a8f07219564ff5`.
RSA-SHA256, literal encoded paths and unbound response overrides follow that contract;
extension headers are lowercase, sorted, whitespace-normalized and joined in wire order.
Pinned Go 1.27.3 / Storage 1.70.0 and Ruby 4.0.9 / Storage 1.62.1 are the SDK contracts.

## Verification

Verified 2026-10-27; implementation commit `36a8ed0`:

- Before implementation, both official Go HTTPS V2 GET and Ruby default V2 GET failed with XML 400 InvalidAuthentication. These failures were observed after adding the SDK assertions.
- `cargo check --workspace --all-targets --locked --offline`: PASS.
- `cargo test --lib --locked --offline auth::signature`: PASS, 11 tests, including injected expiry boundaries, missing/duplicate/mixed credentials, permissive behavior, literal encoded resources and extension header ordering/whitespace.
- `GCS_MIMIC_SDK_SCHEME=https just check-sdk`: PASS, complete pinned Go/Ruby suites; unchanged V2 RSA GET/PUT, unknown signer, viewer PUT denial, missing/changed Content-Type and MD5, method/path changes and body MD5 failure. Ruby verifies both omitted version and explicit `version: :v2`.
- `just check-active-storage`: exit 1, **0 PASS, 6 PASS_WITH_WORKAROUND, 1 FAIL**. Both signed cases succeed without URL modification; only AS-DEFAULT-AUTH fails because the guard blocks OAuth before network access. The failure reports `Google::Auth::AuthorizationError` and `request targets https://oauth2.googleapis.com:443`.
- `cargo xtask dependencies check`: PASS; all declared compiler, crate, SDK, gem and container inputs CURRENT.
- `just check` (one final execution): PASS, formatting, host/cross-target clippy, complete Rust/xtask tests, generated compatibility views and offline shell/publication checks.
- `just check-containers` (one final execution): PASS for linux/arm64 and linux/amd64 builds, health, media CRUD, persistence, initialization, seed, signal shutdown and cleanup.
- Existing `docs/en/getting-started.md` signed-transfer section, first two shell blocks (native journey): PASS, V4 private GET/PUT, tamper/expiry/header rejection, response overrides, virtual-host routing and PAP behavior. The source-Compose signed section and unrelated guide sections were not rerun.
- Markdown lint on the eight changed narrative documents: PASS, zero issues.

The Go V2 signer at Storage v1.70.0 `storage.go:940-987` always emits HTTPS,
including when `Insecure: true`; its test explicitly skips HTTP servers. The final
SDK run used HTTPS with verified development CA trust, without scheme rewriting.
Ruby V2 uses the configured endpoint without the V4 bucket-bound workaround.

DESIGN-040 remains PARTIAL for the named pending formats. DESIGN-084 remains
PARTIAL and its default signed acceptance remains PENDING while authentication
requires the existing workaround. SPEC-ACTIVE-STORAGE-V2 is resolved for this subset.
The later [local credential recipe](sdk-local-auth-softdelete-notes.md) verifies a
separate application-created service with preset token/expiry and unchanged V2
signed GET. Ordinary storage.yml authentication and its acceptance remain unchanged.
Real-cloud differential testing, Rails strict integration, browser CORS, actual
image processing and benchmarks were not run; no performance claim is made.
