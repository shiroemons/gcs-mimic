# Single-port HTTP/HTTPS verification

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: DESIGN-102, following the [design](../slices/single-port-scheme.md). Verified on 2026-11-11 on macOS (Apple Silicon) with the pinned Rust toolchain, at the source that adds `scheme=auto`. The row stays `PARTIAL`: signed URLs over both protocols, concurrent use of both protocols, the connection-slot case, SDK suites, a browser and the container image have not run against this change.

## Commands and results

| Command | Result |
| --- | --- |
| `cargo test --lib -- config:: config_input:: tls:: server::handshake` | PASS, 120 tests |
| `cargo test --bin gcs-mimic -- help_output_keeps_its_exact_bytes` | PASS |
| `bash scripts/with-test-tmpdir.sh cargo test --locked --offline --test it_process` | PASS, 102 tests |
| `bash scripts/with-test-tmpdir.sh bash scripts/test-rust.sh` | Every binary except `it_process` passed (library: 2058 tests). `it_process` then failed 12 tests that asserted the former `https` default; after updating them it passed as above. |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS |
| `cargo xtask lint` | PASS: no new production violation; test wall-clock calls within the baseline |

## What the tests establish

| Test | Behavior |
| --- | --- |
| `server::handshake::tests::a_tls_record_selects_tls_and_a_request_line_selects_plain_http` | A first byte of `0x16` selects TLS; an HTTP/1 request line selects plain HTTP |
| `server::handshake::tests::detection_leaves_the_first_byte_for_the_protocol` | Detection peeks: the HTTP/2 preface is still read in full afterwards |
| `server::handshake::tests::a_client_that_closes_first_is_closed_and_a_silent_one_times_out` | A connection closed before its first byte is `Closed`; one that sends nothing until the deadline is `Silent` |
| `server::handshake::tests::shutdown_ends_the_wait_for_the_first_byte` | The stop signal ends the wait |
| `server::handshake::tests::fixed_modes_never_wait_and_a_unix_socket_serves_plain_http_under_detection` | `Plain` and `Tls` listeners select at once even with a past deadline; a Unix socket under detection serves plain HTTP |
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

## Not established

- A V4 signed URL verified over both protocols on one port, including `signed_host_port`.
- Concurrent HTTP and HTTPS clients on one port, and a full `max_connections` of silent connections releasing a slot when one times out.
- The pinned Go and Ruby SDK suites, a browser and the container image's `HEALTHCHECK` against a default server.
- That a connection closed before its first byte emits no diagnostic: the code path returns without one, but no test reads stderr for it.
- Linux and Windows runs.
