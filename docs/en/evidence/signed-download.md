# V4 RSA signed-download verification

<!-- markdownlint-configure-file {"MD013": false} -->

Verified on 2026-10-23 for the [frozen contract](../slices/signed-download.md).
Code and tests are committed as `e2431fe623494cbfbf788248a08e6efdf998f7af`, with
canonicalization and incomplete-credential fixes in `4729d5a3657394eaa9edd0da116f377424c3ac9a`.
The implementation adds public-key registry validation, strict V4 query authentication,
existing project/bucket read permission checks and a local final-endpoint signing helper.
No storage schema, body buffering or Google service integration is added.

## Executed checks

| Command | Result | Artifact |
| --- | --- | --- |
| `just check-dependencies` at slice start and after ring promotion | PASS, 35 then 36 inputs CURRENT | [Dependency output](signed-download-dependencies.txt) |
| `cargo test --workspace --locked --offline` | PASS, 609 tests, zero failed/ignored | [Rust output](signed-download-tests.txt) |
| `cargo fmt --all --check` | PASS | [Checks](signed-download-checks.txt) |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS | [Checks](signed-download-checks.txt) |
| `cargo xtask compatibility render`, `validate`, `check` | PASS, regenerated English/Japanese matrices; 591 feature rows validated | [Checks](signed-download-checks.txt) |
| `just check-sdk` | PASS, Go vet and all 44 top-level tests over HTTP | [Go HTTP output](signed-download-sdk-go.txt) |
| `GCS_MIMIC_SDK_SCHEME=https just check-sdk` | PASS, Go vet and all 44 top-level tests over trusted HTTPS | [Go HTTPS output](signed-download-sdk-go-https.txt) |

The exact `just check` wrapper was not executed: automatic approval review rejected it
because its shell workflow references container smoke commands. The required Rust,
formatting, Clippy and ledger checks were run individually instead. Container builds,
container smoke runs, full guide walkthroughs and multi-architecture work were excluded
as requested. This evidence does not claim a pre-push or container gate.

## Behavior coverage

- Nine `auth::signature::tests::*` tests cover deterministic expiry/skew boundaries,
  the seven-day limit, calendar/scope validation, every signing field, missing/duplicate
  parameters, malformed hex, method/resource/query/Host/port/header changes, raw encoded
  paths, decoded-name query sorting with a full expected canonical string, the published
  canonical header example, default-port omission, conflicting absolute authority, valid
  declared-digest tampering and key validation.
- Five `signing::*` HTTP tests cover private GET/HEAD/range and selected generation,
  project and bucket-only grants, cross-bucket denial/revocation, all tamper families,
  unknown signer, early/expired errors, no fallback through public grants, JSON rejection,
  Unicode/slash/dot-like names, repeated headers, unsupported response overrides,
  startup failure without a ready event, all incomplete V4 field subsets under public
  grants and explicit permissive credential bypass.
- `signed_private_download_uses_the_https_listener_host_and_port` verifies private
  download through the trusted built-in HTTPS listener and rejects a changed actual Host.
- Three `signing_cli_*` tests verify final-host CLI output, encoded objects, repeated
  headers, GET/HEAD/PUT helper methods, duration suffixes/bounds, option validation,
  timestamps, public/private PEM misuse, missing key and English help.
- Existing `xml::xml_signatures_and_unsupported_headers_never_fall_back_to_public_access`
  passes unchanged. PAP remains unsupported with its existing status and rejection.

The signer uses ring 0.17.14 already present in the TLS graph. Promoting that package
to a direct dependency changes no resolved version. It is `Apache-2.0 AND ISC`,
1,502,610 compressed bytes, MSRV 1.66.0, non-yanked and the newest stable release
reported by the official Cargo index. Publisher repository metadata is not archived
and reports a 2026-07-23 push. See the [dependency policy](../dependency-policy.md)
and [snapshot](../../../compatibility/dependency-status.json). The package size is
not a binary-size measurement.

## Independent pinned Go verification

`TestV4RSASignedDownload` uses Go 1.27.3, `cloud.google.com/go/storage` v1.70.0,
generated API v0.308.0 and the committed throwaway private key. It creates a private
object as the local admin and reads as the local viewer signer without bearer headers.
The complete HTTP and HTTPS suites use guarded origins, no proxy/redirect, bounded
read buffers and verified CA trust; no ADC, metadata server or Google Cloud request
is made.

Unmodified `storage.SignedURL` successfully downloads through a default-port authority.
A guarded client dialer maps only that authority to the local ephemeral listener,
retaining the portless Host without opening privileged ports. Tampering fails.

For explicit non-default ports the pinned SDK's `storage.go:854` uses `u.Hostname()`
and drops the port from canonical Host. The test asserts that its unmodified output
fails with 403 `SignatureDoesNotMatch`. A separate independent Go canonical request
matches the SDK `SignBytes` message with that same port omission, then corrects only
the final Host and signs through Go's standard RSA implementation. Corrected private
GET/HEAD/range, generation, required header, Unicode/repeated-slash paths and tamper
cases pass over HTTP and HTTPS. This is a cryptographic cross-check with an explicit
SDK workaround, not an unmodified non-default-port SDK compatibility claim.
Use `gcs-mimic url sign` or a default-port endpoint; see SPEC-GO-SIGNER-PORT.

## Status and remaining boundaries

DESIGN-030, DESIGN-032, DESIGN-034, DESIGN-036, DESIGN-038 and DESIGN-045 are
IMPLEMENTED for their original cryptographic, timing, path, header, restriction and
helper scopes. DESIGN-029 remains PARTIAL for signed write permissions;
DESIGN-033 remains PARTIAL for virtual-host routing; DESIGN-035 remains PARTIAL for
response overrides and duplicate-provider edge parity; DESIGN-044 remains PARTIAL
for exact provider wire errors. DESIGN-002 and EXT-XML-012/017 gain passing signed
read cases and retain broader pending cases and PARTIAL status. DESIGN-025 is unchanged.

SPEC-SIGNING-EDGES records exact error mappings/messages/precedence, duplicate/ambiguous
query behavior and the optional content-sha256 source discrepancy. Google documents
900-second early skew; no undocumented zero-skew rule is invented. Query signatures
default to `UNSIGNED-PAYLOAD`, with the pinned SDK's optional header substitution.
Virtual-host routing, signed writes/browser/Rails flows, resource benchmarks,
real-cloud differential tests and full provider parity were not run or implemented.
The next slice is XML PUT/signed upload, preserving this verifier and final-host rule.

## Frozen source provenance

The official canonical-request and signature HTML documents were retrieved on
2026-10-23. SHA-256 values pin the fetched bytes; SDK source is pinned by module
version and committed `go.sum`.

| Source | SHA-256 |
| --- | --- |
| [Canonical requests](https://docs.cloud.google.com/storage/docs/authentication/canonical-requests) | `6a7aeef582635879ce4275a3224964628906b0746fcb036865556a77ac9bbe25` |
| [Signatures](https://docs.cloud.google.com/storage/docs/authentication/signatures) | `f042dfbc35d6e383a81854f906d743eae4978de91d03e82839e45ccc10303bb0` |
| [Go storage.go at storage/v1.70.0](https://github.com/googleapis/google-cloud-go/blob/storage/v1.70.0/storage/storage.go) | `fb15593b2ab6448ec3a622ad4ae91ad2972a9bb0e14ec34792c87d91bbb74a7d` |
| `sdk-tests/go/go.mod` | `93a6051f221a20bf7586e846469c8012d0ea5e1d0512e9e9587772bb1d7d524e` |
| `sdk-tests/go/go.sum` | `1fe4e981f7239be32bec257908d18fea019b970e85ec86ffdcf559094d01fc38` |
| `fixtures/signing/test-only-public.pem` | `7e24f0c214d2a62d0213299adc4e33b5004ec0adcd83534782842ca355a0682a` |

The remaining official [manual signing](https://docs.cloud.google.com/storage/docs/access-control/signing-urls-manually),
[signed URLs](https://docs.cloud.google.com/storage/docs/access-control/signed-urls),
[XML headers](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers)
and [XML errors](https://docs.cloud.google.com/storage/docs/xml-api/reference-status)
were checked on 2026-10-23. Exact source questions stay in the gap ledger.
