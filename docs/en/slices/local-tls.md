# Local HTTPS/TLS contract

<!-- markdownlint-configure-file {"MD013": false} -->

This delivery scope belongs to DESIGN-065. DESIGN-055 gains HTTPS link evidence;
its proxy-derived origins and signing remain separate pending work.

## Contract and sources

Checked on 2026-10-23:

- [Google request endpoints](https://docs.cloud.google.com/storage/docs/request-endpoints): JSON uses HTTPS endpoints; XML supports HTTP and HTTPS. The emulator retains explicitly selected local HTTP for development.
- [Rustls 0.23.46](https://docs.rs/rustls/0.23.46/rustls/) supplies TLS 1.2/1.3, certificate/key matching and peer verification.
- [Tokio Rustls 0.26.6](https://docs.rs/tokio-rustls/0.26.6/tokio_rustls/) supplies bounded async transport integration. TLS writes require flush/shutdown.
- Discovery and protobuf revisions remain pinned in the existing upstream inventory. TLS adds no Google API request field, response field, IAM permission or storage transition; these schemas do not define local certificate configuration.

## Acceptance

| Case | Required behavior |
| --- | --- |
| Mode/configuration | HTTPS default or explicit HTTP only; CLI > environment > JSON > defaults. HTTPS without custom TLS paths uses the bundled development certificate/key. A custom override requires both PEM chain and private key; supplying only one path fails instead of falling back. HTTP rejects TLS settings. Invalid mode and empty/unreadable/malformed/mismatched/oversized custom input fail before readiness. |
| Transport/trust | Trusted development CA and matching DNS/IP SAN work with TLS 1.2 and 1.3. Unknown CA, wrong hostname, expired leaf and plaintext on a TLS listener fail. No verification bypass. |
| Resource/lifecycle | Handshakes consume the existing bounded connection slots, have an independent finite timeout, release admission after failure and respond to shutdown. Existing bounded body streaming, request deadlines and draining remain effective. |
| URLs | Listener scheme/address, fixed advertised API base and explicitly supplied public URL base are independent. The default API base follows the chosen scheme; explicit overrides retain proxy use. JSON links and resumable Location use the configured HTTPS base, ignoring untrusted Host/Forwarded. URL construction grants no permission. |
| Clients | Native binary and non-root read-only container probes, bucket/object round trip, resumable upload, restart and host/peer SANs pass with explicit CA trust. Official pinned Go/Ruby clients and a real browser verify the certificate and hostname. |
| Regression | HTTP still works. Strict anonymous denial and authorized HTTPS operations preserve shared IAM behavior. |
| Development fallback | A default HTTPS startup needs no certificate files. `gcs-mimic tls ca` exports only its public development CA. HTTPS healthcheck trusts that CA by default; `--ca-cert PATH` selects only the explicitly supplied trust roots. Custom unknown CA and wrong SAN still fail. |

The TLS handshake timeout is separate from HTTP header/body deadlines and is
inside `max_connections`. Certificates are loaded before catalog recovery,
seed import, listener bind and readiness in the CLI. Keys are loaded once and
are never emitted in diagnostics. PEM inputs are capped at 1 MiB each.

## Bundled development certificate

The binary includes a fixed development leaf certificate, its private key and
the public issuing CA. `gcs-mimic tls ca` prints the CA PEM without reading
runtime configuration, accessing the data directory or making a network
request. Clients can trust this exported root for local tests while retaining
CA, validity and hostname verification. HTTPS healthcheck uses the bundled CA
when `--ca-cert` is omitted; supplying that option replaces the default roots.
The P-256 leaf contains SANs for `localhost`, `gcs`, `127.0.0.1` and `::1`,
and is valid from 2026-10-30 05:10:27 UTC through 2036-10-26 05:10:27 UTC;
the CA expires on 2036-10-27 05:10:27 UTC.
The bundled credentials must be replaced before expiry; no automatic renewal
or clock-validation bypass is provided.

The chain satisfies RFC 5280 strict verification (Python 3.13+
`VERIFY_X509_STRICT`, `openssl verify -x509_strict`), so clients keep their
default checks. Strict verifiers require the leaf's Authority Key Identifier
and the CA's Subject Key Identifier; a self-signed root may omit its own
Authority Key Identifier, but the bundled root includes it so every certificate
in the chain names its issuing key:

| Certificate | Properties |
| --- | --- |
| CA | Self-signed P-256 named curve; critical `basicConstraints` `CA:TRUE`; critical `keyUsage` `keyCertSign, cRLSign`; Subject Key Identifier; Authority Key Identifier equal to it |
| Leaf | P-256 named curve; critical `basicConstraints` `CA:FALSE`; critical `keyUsage` `digitalSignature`; `extendedKeyUsage` `serverAuth`; Subject Key Identifier; Authority Key Identifier equal to the CA's Subject Key Identifier; the SANs above |

`bash scripts/tls-development-cert.sh EMPTY_DIRECTORY` regenerates the CA, leaf
and leaf key with these extensions, checks the result with
`openssl verify -x509_strict -purpose sslserver` and deletes the CA private key.
Copy the three PEM files into `src/tls` and update the validity dates here and
in the [local HTTPS guide](../local-https.md). Every regeneration creates a new
CA, so clients must export it again with `gcs-mimic tls ca`. Unit tests in
`src/tls/tests.rs` parse the embedded chain and fail if a regeneration drops
any of these properties.

The bundled leaf key is deliberately public development material. Trusting this
CA does not establish a private server identity; keep it scoped to the local
test client. The CA private key is not shipped. For another hostname or private
key material, supply both `--tls-cert` and `--tls-key` and configure client
trust for their issuer. The fallback never replaces a malformed supplied pair
or fills in one missing path.

## Exclusions

Public certificate issuance, ACME, system trust-store changes, mTLS, hot reload,
simultaneous HTTP/HTTPS listeners, HTTP/2, HTTP/3, CORS and signed URLs are outside this
scope. Certificates are bundled development material or an operator-supplied
override; validity and SAN checks belong to clients. The fixture generator
produces short-lived test material for independent-CA and expiry cases.
TLS protects transport and does not change permissive/strict authorization.

Actual GCS supports HTTP/1.1, HTTP/2 and HTTP/3 according to the request-endpoints
reference. [Google HTTP guidelines](https://docs.cloud.google.com/apis/docs/http)
describe HTTP/2 over TCP and HTTP/3 over QUIC/UDP. The emulator currently offers
HTTP/1.1 over TLS/TCP only; HTTPS does not imply HTTP/2 or HTTP/3 support.
[GCS gRPC](https://docs.cloud.google.com/storage/docs/enable-grpc-api) is a
separate Protocol Buffers API used by supported C++, Go and Java clients and
other documented integrations. [gRPC over HTTP/2](https://github.com/grpc/grpc/blob/master/doc/PROTOCOL-HTTP2.md)
does not become available merely by adding TLS or REST HTTP/2 transport.

## Reference implementation

Source-reviewed on 2026-10-23: fake-gcs-server v1.56.1, commit
`896ece51e49eed0946089f3089f6343855c0d106`. This describes that comparison
implementation; Google documentation remains the API contract.

- [`main.go`, lines 27–53](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/main.go#L27-L53) wraps the TCP listener with Go `tls.NewListener`. When both certificate paths are supplied, `tls.LoadX509KeyPair` loads the certificate/key. Otherwise, it obtains the default test certificate from a temporary `httptest` TLS server, including when only one path is supplied. Its TLS configuration advertises `h2` through ALPN.
- [`main.go`, lines 67–99](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/main.go#L67-L99) creates separate HTTP and HTTPS listeners for `scheme=both`, sharing the same backend and handler. The scoped gcs-mimic implementation supports one selected listener, a bundled development fallback and an optional matching certificate/key override. It rejects a one-sided override rather than silently selecting the fallback.
- [`internal/config/config.go`, lines 107–142](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/internal/config/config.go#L107-L142) defaults to HTTPS, accepts `http`, `https` and `both`, and provides `cert-location` / `private-key-location` with corresponding `FAKE_GCS_*` environment variables. The default primary port is 4443; the additional HTTP port is 8000.
- [`internal/config/config.go`, lines 191–197](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/internal/config/config.go#L191-L197) derives an unset external URL from the listener settings, selecting HTTPS for `both`. [`fakestorage/server.go`, lines 474–500](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/server.go#L474-L500) gives an explicit external URL precedence and constructs its public URL independently from the configured scheme and public host.

The SHA-256 of the complete pinned `main.go` is
`9d742f32505386a344cdddc0b740b73be1eb96b6d3ed905878c15200db7c0fb6`;
`internal/config/config.go` is
`5517104974a0834c033cd0404562b77cc6673b4d14d295c5178757dbd8687a67`.
These are source observations, not executed TLS comparison results or claims
that clients trust the default certificate.
