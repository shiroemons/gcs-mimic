# gRPC over TLS verification

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: DESIGN-104, following the [contract](../slices/grpc-tls.md). Verified on 2026-11-18 on macOS (Apple Silicon) with the pinned Rust toolchain, at the source that adds `grpc_scheme`. Local paths in the commands are replaced by placeholders such as `<fixtures>`.

## Commands and results

| Command | Result |
| --- | --- |
| `env -u RUSTUP_TOOLCHAIN cargo test --locked --offline --lib -- grpc:: tls:: task:: server:: config::` | PASS, 532 tests |
| `env -u RUSTUP_TOOLCHAIN bash scripts/with-test-tmpdir.sh bash scripts/test-rust.sh` | PASS: 16 test binaries and the doctests, among them 2453 library tests and 130 in `it_process` |
| `env -u RUSTUP_TOOLCHAIN cargo test --locked --offline --test it_process` | PASS, 130 tests, including the 14 in `grpc_tls` and the two in `grpc_foundation` |
| `env -u RUSTUP_TOOLCHAIN cargo test --locked --offline --test it_admin grpc`, `--test it_json grpc`, `--test it_xml_auth grpc` | PASS, the one gRPC test of each binary, which find the gRPC listener by its `h2c` entry |
| `env -u RUSTUP_TOOLCHAIN cargo fmt --all --check` | PASS |
| `env -u RUSTUP_TOOLCHAIN cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS |
| `env -u RUSTUP_TOOLCHAIN cargo xtask lint` | PASS: no new production violation; test wall-clock calls within the baseline, with none added |
| `GCSM_GO_TEST_RUN='^TestGRPCTLS' bash scripts/with-test-tmpdir.sh bash scripts/sdk-go.sh` | PASS: `go vet` and `TestGRPCTLS`, `TestGRPCTLSStrict` and `TestGRPCTLSUntrustedCA` with Go 1.27.4 and `cloud.google.com/go/storage` v1.71.0, against servers with `--scheme http` and the development certificate |
| `GCSM_SDK_SCHEME=https GCSM_GO_TEST_RUN='^TestGRPCTLS' bash scripts/with-test-tmpdir.sh bash scripts/sdk-go.sh` | PASS, the same three tests against servers with `--scheme https` and the development certificate |
| `GCSM_SDK_SCHEME=https GCS_MIMIC_TLS_CERT=<fixtures>/server.pem GCS_MIMIC_TLS_KEY=<fixtures>/server.key GCSM_CA_CERT=<fixtures>/ca.pem GCSM_GO_TEST_RUN='^TestGRPCTLS' bash scripts/with-test-tmpdir.sh bash scripts/sdk-go.sh` | PASS, the same three tests against servers with a certificate from `scripts/tls-fixtures.sh`, so the gRPC listener presents the supplied identity |
| `GCSM_GO_TEST_RUN='GRPC' bash scripts/with-test-tmpdir.sh bash scripts/sdk-go.sh` | PASS, 25 Go gRPC tests: every existing plaintext gRPC test through `STORAGE_EMULATOR_HOST_GRPC` still passes against the default `grpc_scheme=auto` listener, beside the three TLS tests |

Before the implementation, the new process tests failed for the expected reasons: `--grpc-scheme` was an unknown setting, a TLS client on the gRPC port read h2c frames (`InvalidContentType`), and a silent connection received the server's SETTINGS frame. With hyper's `TokioExecutor` restored for one run, `grpc_panicking_handler_is_reported_and_spares_other_streams_and_connections` failed waiting for the panic report, while the reset of the panicking stream and the answer to its neighbor already held. Before the TLS diagnostics were narrowed, `a_grpc_listener_beside_a_unix_socket_leaves_tls_settings_unused` failed because only `unix_socket` reported an error, so startup would have read the files, and `a_mismatched_key_fails_with_the_tls_cause` failed because the repair for a rejected pair named schemes.

## What the tests establish

| Test | Behavior |
| --- | --- |
| `config::tests::grpc_scheme_follows_precedence_and_requires_grpc_listen` | Default `auto`; JSON `h2c`, environment `tls` and CLI `auto` win in that order; `""`, `TLS`, `https` and `h2` are rejected from each source with the list of values; `grpc_scheme` without `grpc_listen` names `grpc_listen`; an invalid `grpc_listen` is the only error reported |
| `config::tests::tls_settings_are_accepted_when_only_the_grpc_listener_uses_them` | With `scheme=http`, `tls_cert`, `tls_key` and `tls_handshake_timeout_ms` are accepted beside `grpc_listen` under `auto` or `tls` and refused, three errors naming the gRPC alternative, without one or under `h2c`; one TLS path alone still fails |
| `config::unix_tests::a_unix_socket_serves_plain_http_under_the_default_scheme` | `grpc_scheme` beside `unix_socket` fails because it requires `grpc_listen` |
| `config::unix_tests::a_grpc_listener_beside_a_unix_socket_leaves_tls_settings_unused` | With `unix_socket`, `grpc_listen` and both TLS files, under `scheme=auto` and `scheme=http`, both TLS settings are refused with "plain HTTP only", so startup never reads the files, and `unix_socket` names the `grpc_listen` conflict |
| `config::help::tests::*` | The help text, whose digest changed, the native binary table and `llms.txt` list `--grpc-scheme` |
| `tls::tests::the_http_acceptor_offers_only_http1_and_the_grpc_acceptor_only_h2` | Under `auto`/`auto` both listeners detect; under `https`/`tls` with a supplied identity both are TLS-only; the HTTP acceptor offers exactly `http/1.1` and the gRPC acceptor exactly `h2` in both |
| `tls::tests::the_grpc_listener_alone_loads_the_identity` | A plain HTTP server loads no identity; a gRPC listener under `auto` or `tls` reads the TLS files (a missing file is `NotFound` on `tls_cert`), under `h2c` they are refused as unused |
| `tls::tests::a_mismatched_key_fails_with_the_tls_cause` | A key that does not match the certificate fails as `Rejected` on `tls_cert` with a `rustls::Error` as `source()`, and its repair asks for a matching key instead of naming schemes |
| `server::handshake::tests::detection_on_the_grpc_listener_selects_tls_for_a_handshake_record` | The gRPC listener's security selects TLS for `0x16` and h2c for the HTTP/2 preface |
| `grpc::tests::transport::grpc_tls_handshake_bytes_never_pass_for_the_preface` | A TLS client that completes the handshake but sends no preface is closed at the preface deadline, not kept under the 60 s keep-alive |
| `grpc::tests::transport::grpc_tls_preface_counts_decrypted_bytes` | A preface sent over TLS moves the connection past the preface deadline: no GOAWAY, keep-alive PINGs, the connection stays open |
| `grpc::tests::transport::grpc_panicking_handler_is_reported_and_spares_other_streams_and_connections` | A panicking stream gets RST_STREAM while its neighbor gets HEADERS; the panic is reported as `grpc-stream`; two later streams and a new connection to the same listener are answered |
| `task::tests::the_hyper_executor_reports_a_panic_and_keeps_spawning` | The executor reports a panic under its name and spawns the next task |
| `grpc_auto_serves_h2c_and_tls_on_one_port` | With `--scheme http` and a gRPC listener, the ready event has `grpc_scheme: "auto"` and the `h2c` entry; with the CA from `gcs-mimic tls ca`, a TLS 1.3 client with SNI `localhost` negotiates `h2` and creates a bucket (`grpc-status` 0) that JSON then returns with 200; TLS 1.2 and TLS 1.3 clients and an h2c client read it |
| `grpc_tls_provided_certificate_and_client_trust_failures` | With a fixture certificate and `grpc_scheme=tls` (entry `h2`), an untrusted CA fails with `UnknownIssuer`, a wrong name with `NotValidForName`, then clients by name and by IP create buckets; with the expired certificate a client fails with `Expired` and an h2c call still succeeds; with the wrong-host certificate `localhost` fails and `wrong.invalid` succeeds |
| `grpc_scheme_tls_refuses_h2c_clients` | Under `tls` an h2c call fails and a TLS call returns `NOT_FOUND` (5) for an absent bucket |
| `grpc_scheme_h2c_refuses_tls_clients_and_keeps_its_ready_event` | Under `h2c` the `listeners` array is exactly the HTTP and `h2c` entries, the only new member is `grpc_scheme`, a TLS handshake fails and an h2c call works |
| `grpc_tls_alpn_refuses_http1_only_clients_and_accepts_clients_without_alpn` | `http/1.1` alone receives the `no_application_protocol` alert; a client without ALPN negotiates none and its HTTP/2 call is answered |
| `a_grpc_tls_client_on_the_http_port_fails_and_https_keeps_working` | An `h2` client on the HTTP port receives `no_application_protocol`; an `http/1.1` client then lists buckets with 200 |
| `grpc_tls_carries_strict_bearer_tokens` | In strict mode `local-admin-token` creates buckets over h2c and over TLS, JSON returns the TLS one, and an anonymous TLS call gets `UNAUTHENTICATED` (16) |
| `grpc_detection_closes_a_silent_connection_within_one_header_read_timeout` | With `header_read_timeout_ms=1500` a silent connection receives no byte, is open after 1 s and closed by 2.7 s |
| `grpc_preface_deadline_counts_from_accept_not_from_the_first_byte` | A client that sends part of the preface 1.4 s after accept gets SETTINGS and is closed between 3.3 s and 4.8 s after accept: the 2 s preface deadline from accept plus the 2 s grace, not 5.4 s |
| `grpc_stalled_client_hello_closes_at_the_handshake_deadline_from_accept` | With `header_read_timeout_ms=4000` and `tls_handshake_timeout_ms=1500`, a connection is open after 1.1 s; a `0x16` sent then is closed within 1.2 s |
| `grpc_handshake_holds_the_only_connection_slot_until_it_ends` | With `max_connections=1` a stalled handshake keeps the next TLS client waiting for 250 ms; once the 600 ms deadline closes it, that client completes and is answered |
| `grpc_sigterm_during_a_tls_handshake_exits_promptly` | With a 60 s handshake deadline and a stalled handshake, SIGTERM stops the server within the 10 s the harness allows and closes the connection |
| `grpc_sigterm_with_an_open_tls_stream_sends_goaway_within_the_grace` | With an open WriteObject stream over TLS (confirmed by a PING acknowledgement), SIGTERM sends GOAWAY and the connection ends within 2.5 s; the server exits successfully and prints `stopped` |
| `grpc_sigterm_default_deadlines_close_incomplete_handshake` | Updated: the client sends part of the preface before reading SETTINGS, because a silent client gets none under `auto`; the drain still ends the incomplete handshake |
| `grpc_tls_invalid_identity_fails_before_readiness` | `--scheme http --grpc-scheme tls` with empty PEM files fails naming `tls_cert`, prints no ready event and creates no data directory; with `--grpc-scheme h2c` the same files fail as unused |
| `TestGRPCTLS` | The official Go client over TLS writes, reads and lists objects, runs a bidi read and an appendable write, and JSON sees the results |
| `TestGRPCTLSStrict` | A static token source authenticates over TLS; a client without credentials gets `UNAUTHENTICATED` |
| `TestGRPCTLSUntrustedCA` | Trusting an unrelated CA fails the call with `UNAVAILABLE` and a certificate error |

## Not established

- Java, C++ and Python clients over TLS; the [client setup](../slices/grpc-tls.md#client-setup) marks them unverified. The Rust setup is read from the pinned sources, not run.
- The Go client on Compute Engine, where `GOOGLE_CLOUD_DISABLE_DIRECT_PATH=true` matters; the tests set it everywhere.
- Windows, where the process tests that use `kill` and `scripts/tls-fixtures.sh` do not run.
