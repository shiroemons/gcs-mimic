# gRPC over TLS on the gRPC listener

<!-- markdownlint-configure-file {"MD013": false} -->

Status: **`IMPLEMENTED`** as DESIGN-104 in the feature ledger. Every acceptance case below passes; the [verification record](../evidence/grpc-tls.md) lists the commands. It extends the [local TLS contract](local-tls.md) (DESIGN-065), the [one-port scheme](single-port-scheme.md) (DESIGN-102) and the [Storage v2 gRPC foundation](grpc-v2-foundation.md) (BEHAVIOR-RPC-005).

## Problem

Until this slice the separate `grpc_listen` listener served plaintext HTTP/2 (h2c) only, and the TLS listener offered ALPN `http/1.1` only. grpc-go (1.67 and later), gRPC C-core, grpc-java and tonic refuse a TLS server that does not negotiate ALPN `h2`, so no gRPC client could use TLS anywhere. Official clients also attach call credentials only to secure channels: on a plaintext channel they drop them or refuse to send them. TLS on the gRPC listener is therefore what lets a strict-profile test pass a bearer token through an official client's own credential plumbing instead of hand-written metadata.

## Decisions

The project owner decided these before implementation.

| Question | Decision |
| --- | --- |
| Port | gRPC over TLS reuses the dedicated `grpc_listen` port. gRPC is not multiplexed onto the HTTP listener, and no tonic transport or TLS feature and no dependency is added; tokio-rustls, already a dependency, is enough. |
| Setting | A new `grpc_scheme` with the values `auto`, `h2c` and `tls`, default `auto`: CLI `--grpc-scheme`, environment `GCS_MIMIC_GRPC_SCHEME`, JSON key `grpc_scheme`. It requires `grpc_listen`. |
| `auto` | Each connection's first byte selects the protocol, exactly as `scheme=auto` does on the HTTP listener: `0x16`, a TLS handshake record, selects TLS; anything else, such as the `P` that starts the HTTP/2 preface, selects h2c. |
| Identity | The gRPC listener uses the HTTPS identity: `tls_cert` and `tls_key`, or the built-in development certificate. It loads that identity even when the HTTP `scheme` is `http`. |
| HTTP listeners | They keep offering ALPN `http/1.1` only and never advertise `h2`, because they speak HTTP/1 only. |

## Settings

| Setting | Default | Accepted | Effect |
| --- | --- | --- | --- |
| `grpc_scheme` | `auto` | `auto`, `h2c`, `tls`; requires `grpc_listen` | `auto` serves h2c and gRPC over TLS on the gRPC port; `h2c` refuses TLS clients; `tls` refuses plaintext clients |
| `tls_cert`, `tls_key` | The built-in development identity | PEM paths, at most 1 MiB each, supplied together | The one identity every TLS listener presents, HTTPS and gRPC alike |
| `tls_handshake_timeout_ms` | 5000 | Unsigned integer, 0 disables | Bounds HTTPS and gRPC TLS handshakes alike, counted from accept |

| Configuration | Result |
| --- | --- |
| `grpc_listen` set, nothing else | h2c and gRPC over TLS with the development certificate on the gRPC port |
| `grpc_scheme` without `grpc_listen` | Startup error: `grpc_scheme requires grpc_listen` |
| `grpc_scheme` with `unix_socket` | Startup error, since a Unix socket excludes every TCP listener and so `grpc_listen` |
| TLS settings with `unix_socket` and `grpc_listen` | Startup error naming the TLS settings as unused and the listener conflict; the files are not read |
| `scheme=http`, TLS files, `grpc_listen` with `grpc_scheme` `auto` or `tls` | Accepted: the gRPC listener uses the files, the HTTP listener stays plain |
| `scheme=http`, TLS files, no gRPC listener or `grpc_scheme=h2c` | Startup error, as before: no listener would use the files |
| Unreadable, malformed or mismatched TLS files that the gRPC listener uses | Startup error naming `tls_cert` or `tls_key`, before the catalog opens and before the ready event |

## Behavior

### Detection, handshake and limits

The gRPC listener's connections go through the same path as the HTTP listener's, in one helper the two share:

- Under `auto` the server peeks at the first byte without consuming it. The wait ends at `header_read_timeout_ms` counted from accept; a connection that sends nothing is closed then, without a reply, and logs `grpc_handshake_timeout` at Info. A connection closed before its first byte logs nothing.
- The TLS handshake ends at `tls_handshake_timeout_ms` counted from accept, so detection never extends it. A failed handshake logs `tls_handshake_failed` with its cause and what a gRPC client needs: a trusted CA, a host name in the certificate and ALPN `h2` or none.
- Detection and the handshake run in the connection's own task, never in the accept loop. A connection holds a `max_connections` slot from accept, through detection and the handshake, and gives it up as soon as the handshake fails or times out.
- Shutdown ends the wait for the first byte and an unfinished handshake at once.

After the handshake the existing HTTP/2 driver runs on the decrypted stream, with every limit of the [foundation](grpc-v2-foundation.md):

- Only decrypted bytes count toward the 24-byte HTTP/2 client preface, so a ClientHello never passes for one.
- The preface deadline, `header_read_timeout_ms`, counts from accept for plaintext and from the end of the handshake for TLS. The caller passes that instant, so the deadline never restarts at the first byte.
- Keep-alive, stream inactivity, GOAWAY grace and SIGTERM draining are unchanged and apply to both protocols.

### ALPN

Each TLS listener offers exactly one protocol: the HTTP listeners `http/1.1`, the gRPC listener `h2`. rustls 0.23.46 then behaves as follows, checked in its server handshake source and pinned by the tests:

| Client offers | Result on the gRPC listener |
| --- | --- |
| `h2` | `h2` is selected |
| `http/1.1` only | The handshake fails with a `no_application_protocol` alert |
| No ALPN extension | The handshake completes without a protocol, and the HTTP/2 preface decides; an HTTP/2 client is served |

A gRPC client that dials the HTTP port over TLS offers only `h2`, so that handshake fails with the same alert, and HTTPS JSON on that port keeps working.

The configurations are built with rustls's ring provider named explicitly, through `builder_with_provider` and the safe default protocol versions (TLS 1.2 and 1.3). `ServerConfig::builder()` would panic at runtime if a future dependency enabled a second rustls provider feature.

### Ready event and diagnostics

- The gRPC entry of `listeners` reports `scheme: "h2c"` whenever plaintext is accepted (`auto` and `h2c`), the value existing consumers search for, and `scheme: "h2"` for `tls`.
- With a gRPC listener the ready event also carries `grpc_scheme`, the configured value. Without one the member is absent, so the ready event of a server without gRPC is unchanged.
- `grpc_connection_failed` and `grpc_handshake_timeout` name what the configured scheme accepts instead of asking for a plaintext client.

### Supervised stream tasks

hyper's HTTP/2 server runs each stream in a task of its own. Those tasks were spawned with `hyper_util`'s `TokioExecutor`, a raw `tokio::spawn` whose handle was dropped, so a panic in a gRPC handler went unreported. They now spawn through `crate::task::spawn_supervised`: a panic is reported as `background_task_panicked` with the task name `grpc-stream`; hyper resets only that stream, and the connection's other streams and the listener keep serving.

## Behavior changes

| Before | Now |
| --- | --- |
| The gRPC listener started HTTP/2 at accept and sent its SETTINGS frame to a client that had not spoken | Under the default `auto` the server waits for the client's first byte, so a silent client receives nothing. `grpc_scheme=h2c` keeps the earlier behavior |
| A silent connection got SETTINGS at once and was closed when the grace after the preface deadline ended, twice `header_read_timeout_ms` after accept | Under `auto` it is closed when the wait for its first byte ends, one `header_read_timeout_ms` after accept |
| `scheme=http` with `tls_cert`, `tls_key` or `tls_handshake_timeout_ms` always stopped startup | It stops startup only when no gRPC listener serves TLS either |
| The ready event had no `grpc_scheme` | It reports `grpc_scheme` with a gRPC listener; `listeners[].scheme` for gRPC is unchanged except under `tls` |
| A gRPC handler panic was silent | It is reported as `background_task_panicked` |

The process test `grpc_sigterm_default_deadlines_close_incomplete_handshake` used to read the server's SETTINGS without sending anything. It now sends part of the preface first, which selects h2c and keeps testing an incomplete HTTP/2 handshake.

## Acceptance cases

| Case | Required behavior | Tests |
| --- | --- | --- |
| DESIGN-104-CONFIG | `grpc_scheme` defaults to `auto`; `auto`, `h2c` and `tls` parse from CLI, environment and JSON with the usual precedence; another value names the three; without `grpc_listen`, also beside `unix_socket`, it fails startup; `scheme=http` accepts TLS settings only while the gRPC listener serves TLS, and one TLS path alone still fails; beside `unix_socket` a `grpc_listen` gives TLS settings no use, so they are refused as unused and their files are never read; the help, native binary table and `llms.txt` list the option | `config::tests::grpc_scheme_follows_precedence_and_requires_grpc_listen`, `config::tests::tls_settings_are_accepted_when_only_the_grpc_listener_uses_them`, `config::unix_tests::a_unix_socket_serves_plain_http_under_the_default_scheme`, `config::unix_tests::a_grpc_listener_beside_a_unix_socket_leaves_tls_settings_unused`, `config::help::tests::help_output_keeps_its_exact_bytes`, `config::help::tests::the_native_binary_table_names_every_option_once_with_its_variable`, `config::help::tests::llms_txt_lists_every_setting_once` |
| DESIGN-104-IDENTITY | The identity is parsed once and builds one acceptor per ALPN choice; the HTTP acceptor offers exactly `http/1.1` and the gRPC acceptor exactly `h2`, with the development identity and with a supplied one; the identity loads only where a listener serves TLS; a mismatched key fails with rustls's error as the cause and a repair about matching the key to the certificate | `tls::tests::the_http_acceptor_offers_only_http1_and_the_grpc_acceptor_only_h2`, `tls::tests::the_grpc_listener_alone_loads_the_identity`, `tls::tests::a_mismatched_key_fails_with_the_tls_cause` |
| DESIGN-104-DETECTION | On the gRPC listener a first byte of `0x16` selects TLS and the preface selects h2c; with defaults one port serves h2c and TLS 1.2 and 1.3 with SNI `localhost` and the CA that `gcs-mimic tls ca` exports; a unary call returns `grpc-status` 0; a bucket created over gRPC and TLS is visible over JSON; the ready event reports `grpc_scheme: "auto"` and the gRPC entry `h2c` | `server::handshake::tests::detection_on_the_grpc_listener_selects_tls_for_a_handshake_record`, `grpc_auto_serves_h2c_and_tls_on_one_port` |
| DESIGN-104-EXPLICIT-MODES | `tls` refuses an h2c client and reports the gRPC entry as `h2`; `h2c` refuses a TLS client and keeps the gRPC entry of earlier releases, the only addition being `grpc_scheme` | `grpc_scheme_tls_refuses_h2c_clients`, `grpc_scheme_h2c_refuses_tls_clients_and_keeps_its_ready_event` |
| DESIGN-104-TRUST | A certificate from `scripts/tls-fixtures.sh` serves gRPC over TLS by name and by loopback IP; an untrusted CA, a wrong host name and an expired certificate fail on the client, and the server keeps serving the next client | `grpc_tls_provided_certificate_and_client_trust_failures` |
| DESIGN-104-ALPN | A client offering only `http/1.1` gets `no_application_protocol`; a client without ALPN completes the handshake without a protocol and is served over HTTP/2; a gRPC TLS client on the HTTP port fails the same way, and HTTPS JSON keeps working there | `grpc_tls_alpn_refuses_http1_only_clients_and_accepts_clients_without_alpn`, `a_grpc_tls_client_on_the_http_port_fails_and_https_keeps_working` |
| DESIGN-104-PREFACE | Under TLS only decrypted bytes count toward the preface: a client that sends none is closed at the preface deadline and one that sends it idles under keep-alive; the plaintext preface deadline counts from accept, not from the first byte | `grpc::tests::transport::grpc_tls_handshake_bytes_never_pass_for_the_preface`, `grpc::tests::transport::grpc_tls_preface_counts_decrypted_bytes`, `grpc_preface_deadline_counts_from_accept_not_from_the_first_byte` |
| DESIGN-104-STRICT | In strict mode a bearer token accepted over h2c is accepted over TLS, and an anonymous call over TLS is UNAUTHENTICATED; the official Go client's static token source authenticates over TLS | `grpc_tls_carries_strict_bearer_tokens`, `TestGRPCTLSStrict` |
| DESIGN-104-LIMITS | A silent connection is closed within one `header_read_timeout_ms`, not two; a ClientHello that stalls after its first byte is closed at `tls_handshake_timeout_ms` counted from accept; with `max_connections=1` a connection in its handshake holds the slot and frees it when the handshake ends | `grpc_detection_closes_a_silent_connection_within_one_header_read_timeout`, `grpc_stalled_client_hello_closes_at_the_handshake_deadline_from_accept`, `grpc_handshake_holds_the_only_connection_slot_until_it_ends` |
| DESIGN-104-SHUTDOWN | SIGTERM during a TLS handshake exits promptly; SIGTERM with an open TLS stream sends GOAWAY and closes within the grace; an incomplete plaintext HTTP/2 handshake is still drained | `grpc_sigterm_during_a_tls_handshake_exits_promptly`, `grpc_sigterm_with_an_open_tls_stream_sends_goaway_within_the_grace`, `grpc_sigterm_default_deadlines_close_incomplete_handshake` |
| DESIGN-104-STARTUP | An identity the gRPC listener would use but cannot load stops startup before the ready event and before the data directory exists, also with `scheme=http`; TLS files that no listener uses still stop startup | `grpc_tls_invalid_identity_fails_before_readiness` |
| DESIGN-104-SUPERVISED | hyper's stream tasks spawn through the supervised executor: a panicking handler is reported, its stream is reset, the connection's other streams are answered and the listener serves a new connection | `grpc::tests::transport::grpc_panicking_handler_is_reported_and_spares_other_streams_and_connections`, `task::tests::the_hyper_executor_reports_a_panic_and_keeps_spawning` |
| DESIGN-104-GO | The official Go client, configured without `STORAGE_EMULATOR_HOST_GRPC`, writes, reads and lists over TLS, runs a bidi read and an appendable write, and JSON sees the results; trusting an unrelated CA fails the handshake | `TestGRPCTLS`, `TestGRPCTLSUntrustedCA` |

## Client setup

The examples trust the CA that signs the listener's certificate: the development CA from `gcs-mimic tls ca`, or the issuer of a supplied pair. Address the listener by a name its certificate covers; see [the development certificate's names](#development-certificate-names).

### Go

Verified with `cloud.google.com/go/storage` v1.71.0 by `sdk-tests/go/grpc_tls_test.go`.

- Do not set `STORAGE_EMULATOR_HOST_GRPC` for TLS. That variable makes the SDK dial in plaintext with `option.WithoutAuthentication()`, which also drops call credentials.
- Set `GOOGLE_CLOUD_DISABLE_DIRECT_PATH=true` on Compute Engine, and keep client metrics off: without the emulator variable the client otherwise tries DirectPath, whose detection may query the metadata server, and exports metrics to Cloud Monitoring. The test sets the variable everywhere.

```go
pem, err := os.ReadFile("ca.pem") // gcs-mimic tls ca > ca.pem
if err != nil {
    return err
}
roots := x509.NewCertPool()
roots.AppendCertsFromPEM(pem)
client, err := storage.NewGRPCClient(ctx,
    option.WithEndpoint("localhost:4444"),
    option.WithGRPCDialOption(grpc.WithTransportCredentials(
        credentials.NewTLS(&tls.Config{MinVersion: tls.VersionTLS12, RootCAs: roots}))),
    option.WithoutAuthentication(), // strict mode: option.WithTokenSource(...)
    storage.WithDisabledClientMetrics())
```

In strict mode `option.WithTokenSource(oauth2.StaticTokenSource(&oauth2.Token{AccessToken: "local-admin-token"}))` replaces `option.WithoutAuthentication()`; the SDK then sends `authorization: Bearer local-admin-token` over TLS. `storage.WithGRPCBidiReads()` adds bidi reads as usual.

### Rust

Checked against the sources of the versions `sdk-tests/rust` pins, not run against this listener: `google-cloud-storage` 1.22.0 builds its gRPC channel in `google-cloud-gax-internal` 0.7.22, which enables TLS exactly when the endpoint's scheme is `https`, with tonic 0.14.9's `tls-native-roots`; tonic offers ALPN `h2` and requires it. Native roots come from `rustls-native-certs` 0.8.5, which reads `SSL_CERT_FILE` when it is set.

```sh
SSL_CERT_FILE=ca.pem cargo run # with the endpoint below
```

```rust
let control = StorageControl::builder()
    .with_endpoint("https://localhost:4444")
    .with_credentials(anonymous::Builder::new().build())
    .build()
    .await?;
```

`SSL_CERT_FILE` replaces the system trust store for that process. An `http://` endpoint keeps plaintext h2c.

### Java

Unverified: not checked against the client source in this slice. `google-cloud-storage`'s gRPC transport is selected with `StorageOptions.grpc()`; an `https://` host selects TLS, as for Rust, and grpc-java uses the JVM's default trust manager, so a CA imported into a trust store passed with `-Djavax.net.ssl.trustStore` is expected to apply. Confirm with the client before relying on it.

```java
Storage storage = StorageOptions.grpc()
    .setHost("https://localhost:4444")
    .setCredentials(NoCredentials.getInstance())
    .build()
    .getService();
```

### C++

Unverified: not checked against the client source in this slice. `google-cloud-cpp`'s gRPC storage client is expected to accept TLS through its endpoint and gRPC channel credentials, for example `EndpointOption("localhost:4444")` with credentials built from `grpc::SslCredentials` whose `pem_root_certs` hold the CA, or gRPC C-core's `GRPC_DEFAULT_SSL_ROOTS_FILE_PATH=ca.pem` for its default roots. Confirm with the client before relying on it.

### Python

Unverified: not checked against the client source in this slice. A gRPC channel created with `grpc.ssl_channel_credentials(root_certificates=open("ca.pem", "rb").read())` for `localhost:4444`, or gRPC C-core's `GRPC_DEFAULT_SSL_ROOTS_FILE_PATH`, is expected to work with the generated Storage v2 client; how `google-cloud-storage` exposes its gRPC transport differs between releases. Confirm with the client before relying on it.

## Development certificate names

The built-in development leaf certificate names `localhost`, `gcs`, `127.0.0.1` and `::1`. A gRPC client takes the TLS server name from its target, so:

- Address the listener as `localhost:4444`, `127.0.0.1:4444` or `[::1]:4444` on the same host.
- In a Compose network, name the service `gcs` and address it as `gcs:4444`.
- For any other name, supply `tls_cert` and `tls_key` whose SANs include it, and trust their issuer. A client must never disable hostname or chain verification instead.

The development key is public, so TLS with it protects nothing outside isolated local development, as for HTTPS.

## Exclusions

- mTLS: the listener never requests or verifies client certificates.
- Certificate hot reload: the identity is loaded once at startup.
- ALTS, DirectPath and xDS: the listener is a plain TLS endpoint, and clients must not try DirectPath against it.
- HTTP and gRPC on one port: gRPC stays on `grpc_listen`, and the HTTP listeners keep ALPN `http/1.1` only.
- A gRPC trusted listener: strict-mode gRPC calls without a token stay anonymous.

## Sources

Checked on 2026-11-18:

- [rustls 0.23.46](https://docs.rs/rustls/0.23.46/rustls/): server ALPN selection in `server/hs.rs` (no extension completes without a protocol; no common protocol sends `no_application_protocol`) and `ServerConfig::builder_with_provider`.
- [tokio-rustls 0.26.6](https://docs.rs/tokio-rustls/0.26.6/tokio_rustls/): `TlsAcceptor` and the decrypted `server::TlsStream`.
- [hyper 1.12.0 `rt::Executor`](https://docs.rs/hyper/1.12.0/hyper/rt/trait.Executor.html), which the HTTP/2 server uses for stream tasks.
- [gRPC over HTTP/2](https://github.com/grpc/grpc/blob/master/doc/PROTOCOL-HTTP2.md) and [RFC 7301](https://www.rfc-editor.org/rfc/rfc7301) for ALPN.
- `cloud.google.com/go/storage` v1.71.0 `grpc_client.go`, `defaultGRPCOptions`: `STORAGE_EMULATOR_HOST_GRPC` selects an insecure channel without authentication; without it DirectPath is enabled.
