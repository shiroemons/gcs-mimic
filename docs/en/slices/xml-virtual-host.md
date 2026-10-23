# Virtual-hosted XML object endpoints

<!-- markdownlint-configure-file {"MD013": false} -->

Contract checked **2026-10-23**. This slice implements DESIGN-048 and completes
DESIGN-033's host/path cases for the existing XML GET/HEAD/PUT subset. The feature
ledger owns status; [evidence](../evidence/xml-virtual-host.md) records executed checks.
Other XML operation fields retain their existing PARTIAL rows.

## Official contract and local extension

[Request endpoints](https://docs.cloud.google.com/storage/docs/request-endpoints)
and the [XML API overview](https://docs.cloud.google.com/storage/docs/xml-api/overview)
describe `bucket.storage.googleapis.com/object` and
`storage.googleapis.com/bucket/object` as XML endpoints. JSON API endpoints are
documented separately; bucket hosts therefore dispatch only XML object requests.
[Canonical requests](https://docs.cloud.google.com/storage/docs/authentication/canonical-requests)
bind `/object` and the bucket host for virtual addressing, versus `/bucket/object`
and the base host for path addressing. A non-default port remains part of Host.

The endpoint guide explicitly excludes domain-named buckets from virtual-host
addressing. The requested dotted-bucket support is a deliberate local extension,
recorded as SPEC-XML-VIRTUAL-HOST, rather than a claim of Google parity for those
names. DNS, CNAME ownership, Google certificates and hosted website behavior are
not simulated. Ordinary supported XML semantics and signing remain shared.

## Configuration and URL helpers

Virtual-host routing defaults **disabled**. Enable an explicit comma-separated
list with one of these equivalent settings, using the existing precedence
CLI > environment > JSON configuration > default:

```sh
gcs-mimic serve --scheme http --xml-virtual-host-domains storage.example.invalid
GCS_MIMIC_XML_VIRTUAL_HOST_DOMAINS=storage.example.invalid gcs-mimic serve --scheme http
```

```json
{"xml_virtual_host_domains":"storage.example.invalid,another.example.invalid"}
```

The list accepts at most 32 distinct ASCII DNS names, each at most 253 bytes and
63 bytes per label. Surrounding entry whitespace is trimmed and names are
lowercased. IP addresses, schemes, ports, wildcards, empty entries, duplicate
names and trailing dots are rejected. An empty setting explicitly disables it.
Overlapping suffixes use the longest match; a configured base host itself remains
path-style. A boundary dot is required, so lookalike suffixes do not match.

Only `<bucket>.<configured-domain>[:port]` in the actual Host header selects a
virtual bucket. Ports do not select buckets, but remain signature-bound. Bucket
validation uses the selected compatibility profile. Host matching is
case-insensitive; signature validation retains the actual signed Host value.
An advertised `api_base_url` that would select a virtual bucket is rejected at
startup with a diagnostic; use an API host outside the configured suffixes, or
explicitly configure that API host itself as a base domain. Trailing-dot FQDN
Host values and IPv6 literals remain path-style; they are not normalized into
bucket hosts. A conflicting absolute-form target authority and Host involving a
virtual host is rejected before dispatch/CORS: unsigned/bearer requests return
XML 400 InvalidArgument, signed requests return 403 SignatureDoesNotMatch. This
explicit local conflict rule also preserves bodyless HEAD errors.
Forwarded/X-Forwarded headers are never trusted. This list selects XML routing;
it does not implement a general allowed-host security policy or grant access.
DESIGN-055's fixed advertised API base, listener and explicit URL helper bases
remain separate; no API selfLink or resumable Location changes.

Both offline helpers accept `--style path|virtual-hosted`, defaulting to `path`:

```sh
gcs-mimic url public --base-url http://storage.example.invalid:4443 \
  --bucket example-bucket --object 'photos/a b.jpg' --style virtual-hosted
gcs-mimic url sign --base-url http://storage.example.invalid:4443 \
  --bucket example-bucket --object 'photos/a b.jpg' --method GET --expires 15m \
  --signer local-viewer@example.invalid --private-key fixtures/signing/test-only-private.pem \
  --style virtual-hosted
```

The base must be a DNS origin without a path prefix. Its host is lowercased before
URL construction/signing. Buckets containing underscores remain valid shared
bucket names but cannot be used by the DNS-only virtual URL helper; it reports
a bucket-specific error. The final URL is
`http://example-bucket.storage.example.invalid:4443/photos/a%20b.jpg` before
signing query parameters. No URL/Host rewriting occurs after signing. Existing
encoding, dot-segment protection, key bounds and default-port omission apply.
DNS or a client-side mapping must direct this host to the listener; configuration
does not create DNS records. Building a public URL never grants IAM permission.

## Routing, authorization and bounded transfers

The complete raw path after the first slash is the object name on a virtual host.
Split/route before one decoding pass; preserve encoded separators, literal
percent signs, Unicode, repeated and leading/trailing slashes. The verifier uses
the original encoded URI, not a synthetic path with the bucket inserted.
Path-style and virtual-host signatures are not transferable, even when both
addresses select the same object. Changed bucket host, port, path, method or
query fails strict signature verification without public-access fallback.

Virtual `/storage/v1/...` and `/_internal/...` paths are ordinary XML object names;
they cannot dispatch JSON APIs or emulator probes. On path-style hosts, existing
reserved namespaces and JSON/admin behavior remain unchanged. `/` on a bucket
host returns the existing XML 501 NotImplemented policy after authentication;
listing and other bucket operations remain outside scope.

The same storage operations enforce strict bearer/signer project-plus-bucket
permissions, public grants, selected generations, ranges, checksums and atomic
PUT conditions. CORS uses the routed bucket for preflight and actual responses,
including denials; it never grants permission. Upload buffering, deadlines,
admission, publication and recovery are unchanged. No payload/catalog collection
or new dependency is introduced.

## TLS and official Go signer

The embedded development certificate covers localhost, gcs and loopback IPs; it
does **not** cover bucket subdomains. HTTPS virtual hosts require a supplied
certificate/key and client trust, with SANs matching the final bucket hostname.
A wildcard such as `*.storage.example.invalid` covers a simple bucket prefix but
not `dotted.bucket.storage.example.invalid`; use its exact SAN or an appropriate
deeper wildcard. Do not disable certificate verification or rewrite Host/SNI.

Pinned Go storage v1.70.0 `SignedURL` works unchanged with
`Style: storage.VirtualHostedStyle()` and `Hostname: "storage.example.invalid"`;
`storage.BucketBoundHostname("<bucket>.storage.example.invalid")` selects the
same object-only path. The guarded test dialer accepts only that bucket authority
and maps it directly to the isolated listener, with proxies and redirects off.
SPEC-GO-SIGNER-PORT still applies: default-port authorities pass; explicit
non-default-port URLs fail SignatureDoesNotMatch because the SDK drops the port
only from its canonical Host. The server/helper retain exact actual Host/port.
The Go HTTPS default-certificate branch asserts SAN rejection. Positive Go
HTTPS transfers require a supplied certificate including
`*.storage.example.invalid` for its randomly named single-label test bucket,
plus the configured API listener hostname/IP SAN and client CA trust. Both HTTP
and supplied-SAN HTTPS positive Go transfers pass; the Rust TLS test separately
verifies an exact SAN for a dotted bucket.

## Acceptance inventory

| Area | Required behavior |
| --- | --- |
| Configuration | Default disabled; CLI/environment/file precedence, empty override, bounded validated domains, case-insensitive/longest-suffix selection, advertised API host conflict rejected |
| Routing | Dotted bucket, encoded/Unicode/repeated/leading paths, GET/HEAD/PUT metadata/bytes equivalence, ranges, generation selectors/atomic conditions and malformed-path XML errors |
| Isolation | Nonconfigured/lookalike/base/trailing-dot/IPv6 hosts remain path-style; conflicting absolute authorities rejected; forwarded headers ignored; advertised links unchanged; virtual JSON/admin paths and OPTIONS use XML; bucket root unsupported |
| Permissions/CORS | Anonymous/roleless/viewer denial, creator create versus overwrite, public read and signed PUT, preflight/actual/error headers without access fallback |
| Signatures | Actual host/port/raw object path, both directions of addressing-style transfer rejection, tamper failure, private signed GET/HEAD/PUT and encoded paths |
| Helpers/SDK | Public and signed virtual style; final host verified, official Go virtual and bucket-bound signers, default-port success and known explicit-port failure |
| TLS | Supplied exact dotted-host SAN succeeds for signed GET/PUT; embedded certificate fails hostname validation |

No container builds, guide walkthrough, real-cloud request, browser execution,
resource benchmark, new crash-boundary test or publication is part of this slice.
