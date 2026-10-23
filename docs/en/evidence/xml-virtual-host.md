# Virtual-host XML verification

<!-- markdownlint-configure-file {"MD013": false} -->

Verified **2026-10-23**, native macOS ARM64, Rust **1.99.1**, edition **2024**.
Implementation and tests: `1e2758fe3da7bf61070b403ae04a31c9db568ed5` and
`15aeea6c5bd518c464e4a1b6291c33768cd3a611`.
The [contract](../slices/xml-virtual-host.md) freezes the supported scope and
SPEC-XML-VIRTUAL-HOST records the deliberate dotted-bucket local extension.
DESIGN-048 and DESIGN-033 are IMPLEMENTED for their declared endpoint/canonical
scope. The broader XML methods and DESIGN-055 retain their existing PARTIAL status.

## Executed checks

| Command | Result | Evidence |
| --- | --- | --- |
| `just check` | PASS: 636 Rust tests, zero failed/ignored; fmt, all-target Clippy with warnings denied, compatibility and shell checks | [Raw checks](xml-virtual-host-checks.txt) |
| `cargo +1.99.1 test --locked --offline --test auth_http virtual_host -- --nocapture` | PASS: all five virtual-host wire tests; also included in the complete suite | Same test names in raw checks |
| `cargo +1.99.1 test --locked --offline --test tls virtual_host_https -- --nocapture` | PASS: supplied-SAN signed GET/PUT and embedded-certificate hostname rejection; also included in the complete suite | Same test name in raw checks |
| `GCS_MIMIC_GO_TEST_RUN='TestV4RSA' bash scripts/sdk-go.sh` | PASS: Go vet, signed-download/upload regression and virtual/bucket-bound SDK signing | [SDK output](xml-virtual-host-sdk.txt) |
| `GCS_MIMIC_SDK_SCHEME=https GCS_MIMIC_TLS_CERT=<cert> GCS_MIMIC_TLS_KEY=<key> GCS_MIMIC_CA_CERT=<ca> GCS_MIMIC_GO_TEST_RUN='TestV4RSA' bash scripts/sdk-go.sh` | PASS: same positive/negative official signer cases over verified HTTPS with a supplied wildcard SAN certificate | [HTTPS SDK output](xml-virtual-host-sdk-https.txt) |
| `cargo +1.99.1 xtask dependencies check` | PASS: all 36 upstream inputs CURRENT; no dependency/lockfile changes | [Freshness output](xml-virtual-host-dependencies.txt) |
| Compatibility render/validate/check and Markdown lint | PASS after the ledger update; both generated matrices use the same ledger | [Documentation output](xml-virtual-host-docs.txt) |

Rust wire tests launch isolated HTTP/TLS listeners with local fixture principals;
no default test contacts Google Cloud or a metadata server. Expected negative TLS,
body-timeout and crash-recovery diagnostics in the raw suite are not failures.
Raw output redacts only the local checkout prefix, with a notice on its first line.

## Wire coverage

| Test | Verified behavior |
| --- | --- |
| `config::tests::virtual_host_domains_are_opt_in_bounded_and_follow_precedence` | Disabled default; file/environment/CLI precedence; explicit empty override; case folding/longest suffix order; invalid forms, duplicates and excess domains rejected; advertised API virtual-host collision rejected with valid base-host alternatives |
| `url::tests::virtual_host_urls_preserve_final_authority_and_raw_object_path` | Dotted bucket and non-default port; lowercase final host, UTF-8/repeated slashes/percent encoding; virtual JSON-looking object path; invalid IP/prefixed/port bases and underscore-bucket DNS URLs rejected |
| `virtual_host::virtual_host_reads_puts_raw_paths_and_strict_permissions_match_path_style` | Dotted bucket; GET/HEAD/PUT byte/metadata/hash/generation equivalence, Unicode/reserved/encoded-dot/leading/repeated slashes; range; selected/missing generation; generation/metageneration-conditioned PUT with unchanged failed generation; malformed paths; strict role checks; bucket root 501 |
| `virtual_host::virtual_host_is_opt_in_and_never_uses_forwarded_hosts_or_json_dispatch` | Disabled/nonconfigured/lookalike/base/trailing-dot/IPv6 hosts remain path-style; uppercase Host routing; most-specific domain; ignored forwarded headers, fixed advertised selfLink; JSON/admin paths cannot dispatch those APIs on a bucket host |
| `virtual_host::virtual_host_absolute_target_authority_must_match_host_before_dispatch` | Matching absolute authority succeeds; either direction of conflicting Host/target rejects before GET/HEAD/preflight dispatch; HEAD is bodyless; signed conflict is SignatureDoesNotMatch |
| `virtual_host::virtual_host_signatures_bind_host_port_raw_path_and_addressing_style` | Strict signed GET/HEAD/PUT; viewer write denial; changed bucket/port/path rejection; both directions of style transfer fail; encoded/Unicode/repeated/leading paths round-trip |
| `virtual_host::virtual_host_public_access_and_xml_cors_use_bucket_rules` | Anonymous denial, public read, preflight including JSON-looking object paths, actual/error headers, signed PUT and tampered-signature rejection even under a public grant |
| `signing_cli_virtual_host_url_binds_final_bucket_host_and_port` and `prints_virtual_host_url_with_dotted_bucket_and_explicit_port` | Offline CLI style, final host/port and encoded path, independent verifier success, changed Host rejection and invalid style |
| `virtual_host_https_requires_a_matching_supplied_certificate` | Disposable supplied certificate with exact dotted-host SAN; trusted final-host signed PUT/GET through curl connect-to without URL/Host/SNI rewriting; embedded certificate fails with curl hostname-verification status 60 |
| `TestV4RSAVirtualHostedURLs` | Official Go default-port VirtualHostedStyle GET/HEAD/PUT, BucketBoundHostname GET, Unicode/repeated-slash names and query-tamper error; unmodified explicit-port URL fails as recorded in SPEC-GO-SIGNER-PORT |

The bounded XML reader/upload engine, atomic permissions/conditions, disk state,
generation selection and recovery are reused without mutation. Existing tests
for those interactions pass in the same complete suite; this slice introduces
no payload buffering, dependencies or new storage transitions.

## SDK transport and TLS boundary

Go **1.27.3**, storage **v1.70.0**, API **v0.308.0** are unchanged. The SDK test
maps only the exact generated bucket authority at its default or explicit
listener port to the validated loopback listener. The request URL and Host stay
unchanged; proxies/redirects are disabled, reads/timeouts/connections are bounded.
Both official URL styles omit the bucket from the URI. Existing explicit-port
defect assertions remain negative tests; no server canonicalization is relaxed.

The Go SDK transfer test passes over **HTTP and supplied-certificate HTTPS**.
The disposable certificate uses the existing fixture CA/leaf process with
`*.storage.example.invalid` added to its listener SANs, covering the randomly
named single-label test bucket. The Rust TLS test separately covers a dotted
bucket with its exact SAN. The embedded certificate is intentionally unchanged;
its hostname rejection passes in Rust. The Go HTTPS default-cert negative branch
was not separately executed in this worker.

## Official source pins

Fetched content was SHA-256 checked on **2026-10-23**. XML references define the
transport; existing Discovery revision **20261013** and protobuf source pins are
unchanged and describe shared resources rather than virtual-host routing.

| Source | SHA-256 |
| --- | --- |
| [Request endpoints](https://docs.cloud.google.com/storage/docs/request-endpoints) | `e30bad47e945d702a53d44987689c826720df0d31d1814af8c16070c78c3f096` |
| [Canonical requests](https://docs.cloud.google.com/storage/docs/authentication/canonical-requests) | `2f537a017e12a199b577d9b9bd0df5a6735ee1321a711ce86546b1b413865357` |
| [XML API overview](https://docs.cloud.google.com/storage/docs/xml-api/overview) | `347ff48b2cbc5607b1c765e6cd486713e6556ca41e37a66f5c2691ca47c1803f` |
| [GET Object](https://docs.cloud.google.com/storage/docs/xml-api/get-object-download) | `3ca70123d0ba3d50efdc135f502ba42e8a14030eb1da867dc9d1029f62d196f4` |
| [HEAD Object](https://docs.cloud.google.com/storage/docs/xml-api/head-object) | `32907528fd51ba6cb828e23b71dd5f919ce7b64be4b9303bceecfe5de26bd015` |
| [PUT Object](https://docs.cloud.google.com/storage/docs/xml-api/put-object-upload) | `1b78935de999fc9c878743a1b549c0166e45cbfb331624b022c7aef60418be8a` |
| [Go signer storage.go, storage/v1.70.0](https://github.com/googleapis/google-cloud-go/blob/storage/v1.70.0/storage/storage.go) | `fb15593b2ab6448ec3a622ad4ae91ad2972a9bb0e14ec34792c87d91bbb74a7d` |

## Remaining limits

The configured suffixes are a routing selection, not a general allowed-host or
trusted-proxy policy. Fixed advertised API URLs stay independent. DNS ownership,
CNAME routing, hosted websites and Google TLS provisioning are not simulated.
Dotted bucket virtual addressing is an explicit requested local extension over
the endpoint guide. JSON APIs and emulator probes are unavailable on bucket
hosts; bucket listing and advanced XML operation fields remain unsupported.

No container build, guide walkthrough, full Go/Ruby SDK suite, actual browser,
real-cloud differential request, benchmark, added crash-boundary/power-loss test,
push or publication was performed. The next integration gate combines this
host-routing slice with response query overrides and the browser work, then runs
the final container/guide checks once against the integrated source.
