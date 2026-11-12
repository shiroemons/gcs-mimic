# Single-port HTTP/HTTPS verification

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: DESIGN-102, following the [design](../slices/single-port-scheme.md). Verified on 2026-11-11 on macOS (Apple Silicon) with the pinned Rust toolchain, at the source that adds `scheme=auto`. Signed URLs over both protocols, concurrent use of both protocols and the connection-slot case were added on 2026-11-12 on the same platform. The row stays `PARTIAL` until the client cases below pass.

## Commands and results

| Command | Result |
| --- | --- |
| `cargo test --lib -- config:: config_input:: tls:: server::handshake` | PASS, 120 tests |
| `cargo test --bin gcs-mimic -- help_output_keeps_its_exact_bytes` | PASS |
| `bash scripts/with-test-tmpdir.sh cargo test --locked --offline --test it_process` | PASS, 102 tests |
| `bash scripts/with-test-tmpdir.sh bash scripts/test-rust.sh` | Every binary except `it_process` passed (library: 2058 tests). `it_process` then failed 12 tests that asserted the former `https` default; after updating them it passed as above. |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS |
| `cargo xtask lint` | PASS: no new production violation; test wall-clock calls within the baseline |
| `bash scripts/check-cross.sh` | PASS for `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu` and `x86_64-unknown-linux-musl`, with no new production Clippy violation. The first CI run of the feature failed this check on Windows, where the Unix-socket handshake was unused; it now compiles only on Unix. |
| `bash scripts/with-test-tmpdir.sh cargo test --locked --offline --test it_process -- tls::` (2026-11-12) | PASS, including the three tests added for signed URLs, slots and concurrency |

## What the tests establish

| Test | Behavior |
| --- | --- |
| `server::handshake::tests::a_tls_record_selects_tls_and_a_request_line_selects_plain_http` | A first byte of `0x16` selects TLS; an HTTP/1 request line selects plain HTTP |
| `server::handshake::tests::detection_leaves_the_first_byte_for_the_protocol` | Detection peeks: the HTTP/2 preface is still read in full afterwards |
| `server::handshake::tests::a_client_that_closes_first_is_closed_and_a_silent_one_times_out` | A connection closed before its first byte is `Closed`; one that sends nothing until the deadline is `Silent` |
| `server::handshake::tests::shutdown_ends_the_wait_for_the_first_byte` | The stop signal ends the wait |
| `server::handshake::tests::fixed_modes_never_wait_for_the_first_byte` | `Plain` and `Tls` listeners select at once even with a past deadline |
| `server::handshake::tests::a_unix_socket_serves_plain_http_under_detection` | On Unix, a socket under detection serves plain HTTP; the handshake selection that only Unix sockets use compiles only there, so Windows builds have no unused code |
| `config::tests::tls_configuration_precedence_and_validation` | The default is `auto`, also when only `tls_cert` and `tls_key` are supplied |
| `config::tests::api_origin_defaults_use_bound_addresses_and_preflight_hosts` | The fallback API base uses `http` under `auto` and `http`, `https` under `https` and `both` |
| `config::unix_tests::a_unix_socket_serves_plain_http_under_the_default_scheme` | `unix_socket` alone is valid; `https`, `both`, `tls_cert` and `tls_handshake_timeout_ms` beside it fail with their reasons |
| `config_input::tests::fake_gcs_server_variables_are_not_read` | `FAKE_GCS_SCHEME` is ignored and the default stays `auto` |
| `the_default_scheme_serves_http_and_https_on_one_port_and_links_follow_the_connection` | With no scheme setting, one port serves curl over HTTP and over HTTPS with the development CA; the ready event reports `scheme: "auto"`, `listeners[0].scheme: "http"` and an `http://` API base; a resumable session URL starts with the origin the initiation used; each upload completes; the other protocol reads the bytes; `selfLink` names the reader's origin |
| `detection_waits_no_longer_than_the_header_and_handshake_limits` | With `header_read_timeout_ms=4000` and `tls_handshake_timeout_ms=1500`, a silent connection is still open after 2.5 s and later closed; a TLS record sent 2 s after accept is closed within 1 s, because the handshake limit counts from accept; a connection closed before its first byte leaves the listener serving |
| `healthcheck_default_url_maps_wildcards_and_ignores_advertised_origin` | Without `--url`, the probe uses `http` under the default and `https` under `scheme=https` |
| `healthcheck_defaults_follow_http_https_both_and_changed_ports` | The `auto` cases, with loopback and wildcard listeners, pass the probe without `--url`, with an explicit URL and through environment precedence |
| `tls_rejects_untrusted_wrong_host_expired_and_plaintext` | With an explicit `scheme=https`, plain HTTP is still refused |
| `tls_embedded_default_startup_exports_only_ca_and_keeps_custom_trust_separate` | The default listener serves HTTPS with the embedded development certificate |
| `help_output_keeps_its_exact_bytes` | The help text names `auto` and its meaning |
| `a_signed_url_verifies_over_both_protocols_on_one_port` | In strict mode with the default scheme, URLs signed for the `https://` and the `http://` origin each return the object over both protocols; a changed Host gets 403 `SignatureDoesNotMatch` over both; a URL signed for a Host without the port gets 403 naming `signed_host_port=lenient` over both, and after a restart with `--signed-host-port lenient` returns the object over both |
| `silent_connections_fill_the_port_and_the_next_client_is_served_when_one_times_out` | With `--max-connections 2 --header-read-timeout-ms 2000`, two connections that send nothing hold both slots: the next plain HTTP client gets no response, and the next HTTPS client no completed handshake, within one second, and each is served once a silent connection times out; the server's stderr holds exactly one `connection_timeout` warning per silent connection and nothing for a connection closed before its first byte |
| `http_and_https_clients_share_one_port_concurrently` | Eight clients, alternating HTTP and HTTPS, upload ten objects each at the same time; each object is then read over the other protocol, also concurrently |

## Not established

- The pinned Go and Ruby SDK suites, a browser and the container image's `HEALTHCHECK` against a default server.
- Signed URLs on ports 80 and 443, where a client of the other protocol names the port in Host; the tests use ephemeral ports.
- Linux and Windows runs.
