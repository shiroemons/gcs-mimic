# SDK scorecard transport and PHP resumable wire

<!-- markdownlint-configure-file {"MD013": false} -->

This slice changes the optional comparison harness, not the emulator's transport
or implementation statuses. Related rows remain owned by the feature ledger:
DESIGN-065, DESIGN-082, DESIGN-083, DESIGN-085, DESIGN-086, AUDIT-004 and
BEHAVIOR-OBJECT-005. Full SDK scores require the separately integrated server
changes and a new complete run; these transport checks do not establish 51/51.

## Shared comparison transport

All three products receive the same HTTPS origin through a Go standard-library
TLS frontend and a temporary development CA from `scripts/tls-fixtures.sh`.
Their HTTP backends bind only loopback; gRPC keeps its existing separate
plaintext listener. No runtime dependency or container image is added.
The certificate fixture includes subject and authority key identifiers required
by strict certificate verifiers.

- Go uses a CA-configured, exact-origin HTTP transport for SDK and signed calls.
- Ruby sets Faraday and Net::HTTP CA files, peer and hostname verification, with
  origin checks before either transport sends a request.
- Node uses `NODE_EXTRA_CA_CERTS` for native fetch and SDK HTTPS clients. A socket
  guard bounds teeny-request and gaxios connections to the selected loopback
  host and port; signed fetch requests also require the exact origin.
- PHP uses Guzzle's CA file and existing exact-origin request middleware.
  Docker gateway dialing preserves the URL, Host and certificate name.
- Python sets the guarded Requests session's CA file while keeping
  `trust_env=False`, no redirects and exact-origin checks.

The proxy preserves the original Host, escaped path, query, body, checksums,
content encoding and range headers. It streams requests and responses using
the standard reverse proxy; it does not collect object bodies. It normalizes
absolute response Locations only from the explicit backend or frontend origin
to the public HTTPS origin, and rejects external or network-path authorities.
Signed URLs themselves are sent unchanged, including Go and PHP V2 URLs whose
SDKs require HTTPS. Returned object JSON is not rewritten.

New result metadata records HTTPS, the standard-library frontend and the
temporary CA certificate SHA256. The scorer rejects differing transport inputs
across products while retaining historical result formats. Debug filters are
still refused by `run.sh`; no scored case is removed or added.

The Go and Ruby wrong-CRC32C cases now require an HTTP 400 checksum error and no
published object, matching the other probes' error classification instead of
crediting an unrelated upload failure. Go uses the same zero CRC32C declaration
as Ruby, Node and PHP. Python's public supplied-checksum API still requires its
larger resumable input; cross-SDK byte-for-byte equivalence is not claimed.

## PHP resumable checksum finding

The pinned packages are `google/cloud-storage` 2.6.1 at
`cf14f967fa45113e2a239fe9103bada8d7b92021`, `google/cloud-core` 1.74.0 at
`2ce0b5206407501dc3e6cf1536df3f8b4b30589f`, and Guzzle 8.3.1.
Official Packagist metadata checked on 2026-10-29 still selects these latest
stable Storage/Core versions. Official Go, npm, RubyGems and PyPI metadata also
confirmed the existing 1.27.3 toolchain, Storage 8.4.0, 1.62.1 and 3.17.1 pins.

`php/test-resumable-wire.php` sends the ordinary public SDK upload call over
real loopback HTTP. A 3 MiB plus 123 byte input with 256 KiB chunks produces
13 PUTs. Every PUT, including 12 non-final chunks, carries the whole-object
`md5=xMhmsWbl+K2oNrkTpUwPfQ==,crc32c=mQprBg==` header.

Core's `ResumableUploader::upload` removes `X-Goog-Hash` from a local copy of the
custom headers for non-final chunks, but leaves it in the request options.
`RequestWrapper::getRequestOptions` forwards those options to Guzzle, whose
`Client::applyOptions` reapplies the header. Neither vendor patches nor disabled
validation are used. PHP failure diagnostics retain HTTP codes and structured
SDK error metadata while redacting URLs that can contain resumable capabilities.

The [official validation contract](https://docs.cloud.google.com/storage/docs/data-validation#json-api-resumable-uploads)
specifies whole-object checksums on the final resumable request. It does not
establish provider acceptance of a repeated non-final checksum. Any server
accommodation therefore remains a separately declared local compatibility rule,
with final whole-object validation and conflicting declarations rejected.
The documentation HTML checked on 2026-10-29 has SHA256
`554f43d464f7022a4732dcdfdf078d794fa77ddd803eb79e39131355c17cb15e`.

## Executed lightweight checks

On 2026-10-29, without a Docker build or a full SDK scorecard run:

- `go build ./...`, `go test ./...` and `go vet ./...` in `sdk-scorecard/go`
  passed. Tests cover CA trust, wrong SAN, external origins, raw paths/queries,
  checksum/body preservation, 308, Range, gzip and external Location rejection.
- `node --test sdk-scorecard/node/local-http.test.mjs` passed for the SDK
  transports' loopback and external socket behavior.
- `bash sdk-scorecard/test-tls.sh` passed all 16 real-client combinations:
  trusted certificate, unrelated CA, wrong SAN and wrong SAN plus unrelated CA
  for Node, Python, Ruby and PHP, including their origin guards.
- `bash sdk-scorecard/test-score.sh` passed weighting, incomplete/mismatched
  evidence, transport mismatch and historical two-SDK scoring checks.
- PHP syntax and `php sdk-scorecard/php/test-resumable-wire.php` passed in the
  existing digest-pinned Composer image. Node, Python, Ruby and shell syntax
  checks also passed.

Integration owns the new result directory, feature/gap reconciliation and
canonical matrices. Existing historical results are unchanged.
