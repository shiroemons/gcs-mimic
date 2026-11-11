# One port for HTTP and HTTPS, and the `scheme` default

<!-- markdownlint-configure-file {"MD013": false} -->

Status: **design, not implemented.** Nothing here is supported behavior until the feature ledger records passing acceptance cases. This is the design that candidate 3 of the [public contract review](../public-contract-review.md) left pending for the minor release after `0.18.0`. It extends the [local TLS contract](local-tls.md) (DESIGN-065); the implementation slice adds its own ledger row before any code changes.

## Problem

The default `scheme` is `https`, yet every published recipe sets `GCS_MIMIC_SCHEME: http`, and a plain HTTP client that reaches the default port sees only a failed TLS handshake. Changing the default to `http` would instead break every client that relies on HTTPS today, and fake-gcs-server users arrive with `https://` URLs while SDKs given `STORAGE_EMULATOR_HOST` send plain HTTP.

## Decisions

The project owner decided these on 2026-11-11.

| Question | Decision |
| --- | --- |
| Default | A new value, `scheme=auto`, is the default. Its listener accepts HTTP and HTTPS connections on the same port. |
| `http`, `https` | Unchanged: each accepts only its own protocol. `https` is how to refuse plain HTTP. |
| Profiles | The default is `auto` under every `auth_mode` and `compat_profile`. No other setting changes it. |
| TLS identity | `auto` without `tls_cert` and `tls_key` uses the bundled development certificate, as `https` does. With both paths it uses them. |
| TLS paths without `scheme` | `tls_cert` and `tls_key` select the certificate only. They do not make the listener HTTPS-only; `scheme=https` does. |
| `scheme=both` | Kept unchanged: the main listener accepts only HTTPS and `listen_http` only HTTP. It remains the exact translation of fake-gcs-server's two-port command line. |
| `unix_socket` | `auto` with `unix_socket` serves plain HTTP, so `unix_socket` alone is a valid configuration. An explicit `https` or `both` is still refused. |
| fake-gcs-server `--public-host` without `--scheme` | Still translates to an `https://` public base, because fake-gcs-server's own default scheme is `https`. |
| Ready event | Under `auto` the top-level `scheme` reports `auto` and `listeners[].scheme` reports `http`, so a URL built from a listener entry works without a CA. |

Why `auto` can become the default in one step: existing HTTPS clients keep working on the same port with the same certificate, so the change adds accepted connections and removes none. The differences are listed under [Migration](#migration).

## Behavior

### Detecting the protocol

Both protocols make the client speak first, so the server waits for the first byte of a new connection without consuming it.

- `0x16`, a TLS handshake record, selects TLS. The handshake then runs exactly as on an `https` listener.
- Any other byte selects plain HTTP. HTTP/1 methods and the HTTP/2 preface start with an ASCII letter.
- A connection closed before its first byte ends without a diagnostic. TCP probes from Docker or an orchestrator therefore stay silent.

Detection applies only to the main TCP listener under `scheme=auto`. The trusted listener stays plain HTTP, the gRPC listener and the two `both` listeners are unchanged, and a Unix socket is never inspected.

### Time and connection limits

A detecting connection holds a connection slot from accept, like every other connection.

- The wait for the first byte ends at `header_read_timeout_ms`, measured from accept, which is the limit a silent plain HTTP connection has today.
- When the first byte selects TLS, the handshake deadline `tls_handshake_timeout_ms` is also measured from accept, so detection never extends it.
- Shutdown interrupts the wait.

A silent client therefore holds a slot no longer than the longer of the two limits, never their sum.

### Advertised addresses

Each request already carries the scheme of its own connection (`ConnectionScheme`), and without an explicit `api_base_url` the API base is derived from that scheme and the authority the request addressed. Under `auto` the detected protocol supplies the scheme, so:

- JSON links and the resumable-upload session URL use `https://` for a request that arrived over TLS and `http://` for one that arrived in plain HTTP.
- An explicit `api_base_url` is returned unchanged for both. Either spelling reaches the same port.

Where no connection supplies a scheme, `auto` uses `http`, which every client can follow without a CA:

- the fixed fallback API base, used when a request's authority cannot be advertised;
- the `listeners[].scheme` member of the ready event, so a URL built from that member and `listen` works. The top-level `scheme` member reports `auto`.

No response redirects between the protocols, and no response carries `Strict-Transport-Security`: a browser that remembered HTTPS for the host could no longer use plain HTTP on the same port.

### Healthcheck

Without `--url`, `gcs-mimic healthcheck` probes the main listener over plain HTTP when the configured scheme is `auto`. The fallback to PID 1's options is unchanged.

### Configuration rules

| Configuration | Result |
| --- | --- |
| Nothing set | `auto` with the development certificate |
| `tls_cert` and `tls_key` | `auto` with that certificate |
| Only one of `tls_cert`, `tls_key` under `auto`, `https` or `both` | Startup error, as today |
| `scheme=http` with a TLS setting | Startup error, as today |
| `listen_http` without `scheme=both` | Startup error, as today |
| `unix_socket` with `auto` or `http` | Plain HTTP on the socket |
| `unix_socket` with `https` or `both` | Startup error, as today |
| `unix_socket` with a TLS setting | Startup error: a Unix socket never serves TLS |

## Implementation notes

- `Scheme` gains `Auto` and it becomes the `#[default]`. `Scheme::primary()` and every `match` on `Scheme` are exhaustive, so the compiler lists the sites to decide: `healthcheck_url`, `effective_api_base_url`, `tls::load_server`, `check_tls`, `check_listeners`, `bind_primary`, the ready event and the endpoint snapshot.
- `Serving::tls: Option<TlsAcceptor>` in `src/server.rs` becomes a three-variant enum (plain, TLS, detect). `serve_http` peeks one byte on the TCP stream inside the existing `select!` with the stop signal, then continues on the existing TLS or plain path with the matching `ConnectionScheme`.
- The alias translation of `--public-host` keeps its own scheme rule (`config_input.rs` `public_scheme`) and must not follow `Scheme::primary()`.
- No dependency is added: `tokio::net::TcpStream::peek`, tokio-rustls and hyper are already in use.

## Acceptance cases

Decomposed here; the implementation slice records results in the ledger.

| Case | Required behavior |
| --- | --- |
| Default and values | Unset `scheme` is `auto` in permissive and strict mode and under both compatibility profiles; `auto`, `http`, `https` and `both` parse from CLI, environment and JSON with the usual precedence; an unknown value names the four values; the help digest pins the option line. |
| Both protocols on one port | With defaults, one port serves a bucket and object round trip over HTTP and over HTTPS with the development CA trusted, concurrently, with TLS 1.2 and 1.3. |
| Explicit modes unchanged | `https` refuses plain HTTP and `http` refuses TLS, with today's diagnostics; `both` serves HTTPS on `listen` and HTTP on `listen_http` only. |
| Custom identity | `auto` with `tls_cert` and `tls_key` presents that certificate and still serves plain HTTP; one path alone fails startup; `scheme=http` with a TLS path fails startup. |
| Session URL follows the connection | A resumable initiation over HTTP returns an `http://` session URL and over HTTPS an `https://` one, each with the addressed authority, and each upload completes; JSON `selfLink` and `mediaLink` follow the same rule; an explicit `api_base_url` is returned unchanged over both. |
| Signed URLs | A V4 signed URL verifies over both protocols on the same port in strict mode, including the `signed_host_port` rules for a Host without a port. |
| Silent and short connections | A connection that sends nothing is closed at `header_read_timeout_ms` and releases its slot; one that closes before its first byte emits no diagnostic; a TLS client that stalls after its first byte is closed at `tls_handshake_timeout_ms` measured from accept; with `max_connections` silent connections held, the next client is served once one times out. |
| Shutdown | Shutdown during the first-byte wait ends the connection without delaying the drain. |
| Listener roles | The trusted listener refuses TLS as today; the gRPC listener is unchanged; a Unix socket with the default scheme serves plain HTTP and with `https` fails startup. |
| Ready event and healthcheck | The ready event reports `scheme: "auto"` and `listeners[0].scheme: "http"`; `healthcheck` without `--url` succeeds against a default server and a server started with only PID 1 options. |
| fake-gcs-server flags | `--public-host HOST` without `--scheme` yields an `https://` public base and the `native_equivalent` diagnostic names it; `--scheme both --port-http N` behaves as in `0.18.0`. |
| Clients | The pinned Go and Ruby SDK suites pass against a default server without a scheme setting; a browser loads a public object over both protocols; the container image's `HEALTHCHECK` passes with defaults. |

## Migration

Most configurations need no change: clients that use HTTPS against the default, and configurations that set `--scheme http` or `--scheme https`, behave as before.

| If you | Then |
| --- | --- |
| Rely on plain HTTP being refused | Set `scheme=https`. |
| Pass `tls_cert` and `tls_key` without `scheme` and want HTTPS only | Set `scheme=https`; otherwise plain HTTP is now served too. |
| Read `scheme` in the ready event | Expect `auto`; `listeners[0].scheme` becomes `http`. |
| Use links produced without an explicit `api_base_url` and without a usable request authority | They change from `https://` to `http://`. Set `api_base_url` to fix them. |
| Set `scheme=http` only because of the old default | It can be removed; keeping it refuses HTTPS, as before. |
| Set `scheme=http` together with `unix_socket` | The scheme can be removed. |

Published recipes and the README drop `GCS_MIMIC_SCHEME: http` in the release that ships this change.

## Limits and open points

- The bundled development key is public, so HTTPS with it authenticates nothing; this is unchanged from `https` today.
- Detection reads one byte. A TLS client that sends an SSLv2-format hello is served as HTTP and fails there; rustls supports neither that format nor those protocol versions.
- Whether an `https`-only listener should answer a plain HTTP request with an explanatory `400` instead of a failed handshake is not part of this design.
