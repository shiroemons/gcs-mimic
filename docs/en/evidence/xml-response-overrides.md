# XML response override verification

<!-- markdownlint-configure-file {"MD013": false} -->

Checked **2026-10-23**. Code commit:
`80e3074801d9dd30865fe23bf054b999b951e996`.
The [slice contract](../slices/xml-response-overrides.md) defines the supported
query parameters, validation, duplicate rules, helper and remaining boundaries.

## Executed checks

| Command | Result | Artifact |
| --- | --- | --- |
| `cargo test --locked --offline --test auth_http response_overrides -- --nocapture` | PASS, four wire tests; also included in the full suite below | [Rust output](xml-response-overrides-checks.txt) |
| `cargo test --locked --offline --test sign_url_cli` | PASS, three CLI tests; also included in the full suite below | [Rust output](xml-response-overrides-checks.txt) |
| `just check` | PASS, 630 Rust tests, zero failed/ignored; fmt, Clippy, ledger and shell/publication-policy checks | [Rust output](xml-response-overrides-checks.txt) |
| `GCS_MIMIC_SDK_SCHEME=https bash scripts/sdk-go.sh --with-ruby` | PASS, Go vet and all 44 top-level tests including ResponseOverrides; Ruby origin guard, permissive/strict API and signed cases | [SDK output](xml-response-overrides-sdk.txt) |
| `cargo xtask dependencies check` | PASS, all 36 inputs CURRENT; no dependency or toolchain changes | [Freshness output](xml-response-overrides-dependencies.txt) |
| `cargo xtask compatibility render`, `validate`, `check`; Markdown lint | PASS, 591 rows and both generated views; four Markdown files | [Documentation output](xml-response-overrides-docs.txt) |

Ruby uses the provided Ruby 4.0.9 runtime and existing installed gem cache through
shell PATH/GEM_HOME/GEM_PATH settings. Those machine-specific paths are omitted
from reproducible repository commands. Bundler 4.0.23 and storage 1.62.1 remain
locked; no dependency installation or lockfile change was needed. Logs redact
only local path prefixes and declare that redaction on the first line.
Markdown lint disables MD013 for the existing long generated rows and uses
MD024 `siblings_only` because feature-kind sections repeat category headings.
All other rules remain enabled; the lint configuration is external to the repo.

## Verified behavior

`signing::overrides::xml_response_overrides_preserve_selected_metadata_ranges_and_cors`
checks full and range GET/HEAD, public/bearer/signed callers, retained generation
selection after a live overwrite, exact bytes/lengths, override values with
spaces/plus/quotes, Cache-Control, ETag, hashes, generation/metageneration,
stored byte headers, custom metadata and configured CORS exposure. Changing or
removing a signed override, or adding one to a URL signed without it, returns
403 SignatureDoesNotMatch, including after a public grant. JSON reads
prove that both current and retained generation metadata remain unchanged.

`signing::overrides::xml_response_overrides_reject_injection_duplicates_and_unknown_names`
checks both override names with CRLF injection, separate CR/LF, NUL, DEL and
another invalid control, empty values and leading/trailing spaces or HTAB,
across signed/bearer GET/HEAD. Failures are 400 InvalidArgument with no injected
header and bodyless HEAD. Raw non-ASCII values return 501 NotImplemented rather
than being classified as malformed; HEAD remains bodyless. Raw duplicate names
and percent-encoded aliases reject with local unsigned InvalidArgument or strict
signed InvalidAuthentication. Undocumented response-* names and other unsupported
queries return 501 for both authorized bearer and valid signed requests.

`signing::overrides::xml_response_overrides_from_cli_reach_the_server` signs
GET/HEAD URLs through the actual CLI and requests the strict server with those
unchanged URLs. Both override headers, RFC 5987 filename escapes, exact GET
bytes, bodyless HEAD and unchanged stored content type pass.

`signing::overrides::xml_response_overrides_are_absent_from_error_responses`
checks valid bearer and signed requests that fail with 404 NoSuchKey, 412
PreconditionFailed and 416 InvalidRange. XML error Content-Type remains intact
and Content-Disposition is absent; the signed condition header is bound to
the signature.

The three `sign_url_cli` tests exercise repeated literal query options, reserved
character/Unicode object-path/percent encoding, RFC 5987 ASCII disposition values,
independent authentication of the resulting URL, malformed/duplicate options and
reserved signing field rejection. Existing query sorting, path/header binding,
malformed-field and other negative tests remain in the full suite.

`TestV4RSASignedDownload/ResponseOverrides` uses unmodified official Go
`storage.SignedURL(QueryParameters: ...)` for a private object. GET/HEAD return
the exact two override headers and expected bodies; value tampering rejects.
The existing guarded default-port dialer preserves the signed authority without
privileged listeners or external requests. No SDK or server Host rule changes.

Ruby's strict signed case uses unmodified `File#signed_url(query: ...)`, checks
both response headers and private bytes, rejects value tampering and confirms
stored content type remains text/plain. The existing bucket-bound host:port and
explicit bucket/name File path compensate for the pinned endpoint behavior
before signing; no URL is rewritten. Both SDKs verify the development CA and
hostname, disable proxies/redirects and confine requests to isolated loopback
emulators. Go 1.27.3, storage v1.70.0 and API v0.308.0 remain pinned.

## Source pins

Official pages fetched on **2026-10-23**. SHA256 identifies the complete fetched
HTML, not vendored prose. Existing Discovery/protobuf pins in the
[source manifest](../../../compatibility/upstream/source-manifest.json) are
unchanged; the XML-specific references define this slice.

| Source | SHA256 |
| --- | --- |
| [GET Object](https://docs.cloud.google.com/storage/docs/xml-api/get-object-download) | `e4e68da48af6def3e8a4a53d8672c53acd548f130d5bd4c94ced8344308996eb` |
| [XML headers and query parameters](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers) | `e11827c917a480523193f558e89a207c4e0387018245bafcaa0a3e165c92a49d` |
| [Canonical requests](https://docs.cloud.google.com/storage/docs/authentication/canonical-requests) | `400ef61b3d9408d34c996acf33812f0ed1482d50e6ba976a9dd15281af70b70a` |

## Ledger decision and limits

DESIGN-035 adds passing override, wire, CLI and SDK cases. Its pre-existing
provider duplicate/ambiguous-name acceptance remains PENDING under
SPEC-SIGNING-EDGES, so the row remains PARTIAL. The official text defines name
ordering but no repeated decoded-name behavior. Marking the row IMPLEMENTED
would require removing an unresolved original requirement, prohibited by the
[completion policy](../compatibility-policy.md). No provider evidence is claimed.

EXT-XML-012 and EXT-XML-017 add passing response override cases and remove
overrides from their unsupported-input lists while keeping all other pending
scope. SPEC-XML-RESPONSE-OVERRIDES records the explicit anonymous/permissive/HEAD
extension beyond the documented authenticated GET wording, local empty/edge-whitespace
400 rejection, unrepresentable text-header/non-ASCII 501, incomplete header grammar
validation and undocumented query rejection. Their exact provider behavior
remains unresolved. No status/fidelity promotion conceals these choices.

Container builds and guide walkthroughs are reserved for final integration.
No real-cloud, browser/Rails, new RSS/cgroup, crash-boundary or performance
claim is made. This change adds no body buffering or persistent state mutation.
The next integration step combines virtual-host routing and the existing
signed-browser transfer work with these query semantics.
