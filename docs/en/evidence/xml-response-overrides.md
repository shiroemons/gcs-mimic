# XML response override verification

<!-- markdownlint-configure-file {"MD013": false} -->

Checked **2026-10-23**. Code commit:
`d8b75981c35d2ab19c0966e8450f666a722c47ea`.
The [slice contract](../slices/xml-response-overrides.md) defines the supported
query parameters, validation, duplicate rules, helper and remaining boundaries.

## Executed checks

| Command | Result | Artifact |
| --- | --- | --- |
| `cargo test --locked --offline --test auth_http response_overrides -- --nocapture` | PASS, both new wire tests; also included in the full suite below | [Rust output](xml-response-overrides-checks.txt) |
| `cargo test --locked --offline --test sign_url_cli` | PASS, three CLI tests; also included in the full suite below | [Rust output](xml-response-overrides-checks.txt) |
| `just check` | PASS, 628 Rust tests, zero failed/ignored; fmt, Clippy, ledger and shell/publication-policy checks | [Rust output](xml-response-overrides-checks.txt) |
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
stored byte headers, custom metadata and configured CORS exposure. Changing
either signed override returns 403, including after a public grant. JSON reads
prove that both current and retained generation metadata remain unchanged.

`signing::overrides::xml_response_overrides_reject_injection_duplicates_and_unknown_names`
checks both override names with CRLF injection, separate CR/LF, NUL, DEL and
another invalid control, across signed/bearer GET/HEAD. Failures are 400
InvalidArgument with no injected header and bodyless HEAD. Raw duplicate names
and percent-encoded aliases reject with local unsigned InvalidArgument or strict
signed InvalidAuthentication. Undocumented response-* names and other unsupported
queries return 501 for both authorized bearer and valid signed requests.

The three `sign_url_cli` tests exercise repeated literal query options, reserved
character/Unicode/percent encoding, independent authentication of the resulting
URL, malformed/duplicate options and reserved signing field rejection. Existing
query sorting, path/header binding, malformed-field and other negative tests
remain in the full suite.

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
| [GET Object](https://docs.cloud.google.com/storage/docs/xml-api/get-object-download) | `f6e28a1cba774bbdd1d31ad4aef44a579bf6ab49cf8f22cf25c7027b90797c80` |
| [XML headers and query parameters](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers) | `9e5c413d52313c0a9216d9a4c5490c757ac62eb9ef1a8eb36bfc84952a8e9f09` |
| [Canonical requests](https://docs.cloud.google.com/storage/docs/authentication/canonical-requests) | `ab67caa21e0b202d412bd6c83f360711a0149160c08070234f96fe569741e7aa` |

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
extension beyond the documented authenticated GET wording, HeaderValue-only
validation and undocumented query rejection. Their exact provider behavior
remains unresolved. No status/fidelity promotion conceals these choices.

Container builds and guide walkthroughs are reserved for final integration.
No real-cloud, browser/Rails, new RSS/cgroup, crash-boundary or performance
claim is made. This change adds no body buffering or persistent state mutation.
The next integration step combines virtual-host routing and the existing
signed-browser transfer work with these query semantics.
