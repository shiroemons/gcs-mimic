# Trusted listener verification

<!-- markdownlint-configure-file {"MD013": false} -->

Verified on **2026-11-08** for DESIGN-101 and the
[trusted listener section](../container-usage.md#keep-private-buckets-private-with-a-trusted-listener)
with Rust 1.99.1 on macOS arm64. No container, Compose, SDK, browser or
real-cloud run was performed: the documented Compose example and an unmodified
Go storage client against the trusted listener remain unverified, which keeps
the row PARTIAL. gRPC has no trusted listener.

## Commands

```sh
cargo check --workspace --all-targets --locked
cargo fmt --all
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo xtask lint
cargo test --locked --lib auth::
cargo test --locked --lib config::
cargo test --locked --lib server::
cargo test --locked --lib xml::
cargo test --locked --lib grpc::
cargo test --locked --bin gcs-mimic
cargo test --locked --test it_xml_auth trusted_listener
cargo test --locked --test it_xml_auth auth_http
cargo test --locked --test it_xml_auth signed_browser
cargo test --locked --test it_process foundation
cargo test --locked --test it_process unix_transport
cargo test --locked --test it_process grpc_foundation
cargo test --locked --test it_process healthcheck
cargo test --locked --test it_protocol http_batch
cargo test --locked --test it_protocol request_host
cargo xtask compatibility check
```

## Results

| Check | Result |
| --- | --- |
| Compile, formatting, Clippy with warnings denied | PASS |
| Production lint gate | PASS: 0 new production violations, 0 ratcheted findings |
| `--lib auth::` | PASS: 74 passed, 0 failed |
| `--lib config::` | PASS: 62 passed, 0 failed |
| `--lib server::` | PASS: 129 passed, 0 failed |
| `--lib xml::` | PASS: 38 passed, 0 failed |
| `--lib grpc::` | PASS: 156 passed, 0 failed |
| `--bin gcs-mimic` | PASS: 32 passed, 0 failed |
| `--test it_xml_auth trusted_listener` | PASS: 5 passed, 0 failed |
| `--test it_xml_auth auth_http` | PASS: 141 passed, 0 failed |
| `--test it_xml_auth signed_browser` | PASS: 9 passed, 0 failed |
| `--test it_process foundation` | PASS: 41 passed, 0 failed |
| `--test it_process unix_transport` | PASS: 3 passed, 0 failed |
| `--test it_process grpc_foundation` | PASS: 2 passed, 0 failed |
| `--test it_process healthcheck` | PASS: 7 passed, 0 failed |
| `--test it_protocol http_batch` | PASS: 7 passed, 0 failed |
| `--test it_protocol request_host` | PASS: 13 passed, 0 failed |
| Ledger and generated matrices | PASS |

## Cases

| Test | Behavior |
| --- | --- |
| `config::tests::a_trusted_listener_needs_strict_mode_its_principal_and_its_own_address` | A strict pair parses; each setting alone, permissive mode (also with `signed_urls=verify`), a host name, an invalid principal id and an address equal to `listen`, the `scheme=both` HTTP listener or `grpc_listen` fail on the named setting; equal port-0 addresses and a principal the file may lack pass configuration |
| `config::unix_tests::unix_socket_configuration_has_no_implicit_tcp_or_tls_fallback` | A valid strict pair with `unix_socket` fails on `unix_socket`, naming `trusted_listen` |
| `auth::tests::the_trusted_listener_changes_only_requests_without_credentials` | Without a principal the trusted listener stays anonymous; an unknown id is refused; after trusting, only the trusted listener resolves a credential-free request to the principal; another token resolves to its own identity; unknown, empty, Basic, duplicate and query credentials stay 401 on the trusted listener; the enabled test signer can be trusted; permissive mode refuses |
| `startup_rejects_a_trusted_principal_missing_from_the_registry` | A principal outside the identity file stops startup before the data directory is created, with a diagnostic naming it |
| `requests_without_credentials_act_as_the_trusted_principal_only_on_the_trusted_listener` | Ready reports `trusted_listen` and `trusted_principal` and keeps one entry in `listeners`; without credentials the main listener answers 401 for JSON bucket creation, media upload, JSON and XML reads, XML PUT and resumable initiation, while the trusted listener succeeds; the anonymous upload records the same owner and ACL entities as an upload presenting the principal's token; the resumable Location and the final `mediaLink` use the trusted Host `gcs:4444` |
| `presented_credentials_and_public_grants_decide_alike_on_both_listeners` | On the trusted listener a viewer token reads but cannot create a bucket, a role-less token gets 403, unknown bearer, Basic and `access_token` credentials get 401, and an unverifiable V2 signed URL gets 403; a bucket granting `allUsers` `roles/storage.objectViewer` is readable anonymously through JSON and XML on the main listener while the private bucket stays 401 |
| `browser_initiated_requests_are_refused_on_the_trusted_listener_only` | `Origin` (including `null`) and `Sec-Fetch-Site` `cross-site`/`same-origin` get 403 naming the trusted listener on JSON, XML and probe routes; the same requests to the main listener read the public object; `Sec-Fetch-Site: none` passes |
| `server::browser::tests::only_the_trusted_listener_refuses_browser_initiated_requests` | The shared check refuses every browser header combination only for the trusted role |
| `server::admin::guard::tests::management_guards_run_before_every_route_specific_gate` | Management keeps refusing the same browser headers through the shared check |
| `batch_parts_and_management_follow_the_listener_of_their_request` | A batch part runs as the principal on the trusted listener (200) and anonymously on the main one (401); `PUT /_internal/config` without a token succeeds on the trusted listener only, because the principal is the administrator |
| `server::admin::tests::config_uses_the_instance_principal_instead_of_project_roles` | Management without a token is 401 on the public role and 200 on the trusted role whose principal is the administrator; a presented non-administrator token is 403 on both |
| `process_shutdown_reports_stopped_and_releases_catalog_for_restart` | The default ready key set is unchanged |
| `help_output_keeps_its_exact_bytes` | The help digest pins the two new option lines |

The listener shares `max_connections` and every timeout because it is served by
the same accept loop and connection code as the other HTTP listeners; no test
measures that separately.

## Limits of the browser refusal

The refusal relies on headers that browsers send for page requests. The
[Fetch Metadata](https://w3c.github.io/webappsec-fetch-metadata/) working draft
of 21 September 2026 sends `Sec-Fetch-*` only to potentially trustworthy URLs
(loopback or HTTPS) and uses `none` for navigations the user started, such as a
typed URL. The [Fetch standard](https://fetch.spec.whatwg.org/) (last updated
19 October 2026) appends `Origin` to CORS-mode requests and to requests whose
method is neither GET nor HEAD, so a plain link or image GET carries none. A
page on a plain HTTP address other than loopback can therefore still reach the
listener with such a request; keeping the port unpublished is the protection.
