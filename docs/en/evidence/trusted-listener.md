# Trusted listener verification

<!-- markdownlint-configure-file {"MD013": false} -->

Verified on **2026-11-08** for DESIGN-101 and the
[trusted listener section](../container-usage.md#keep-private-buckets-private-with-a-trusted-listener)
with Rust 1.99.1 on macOS arm64, including the Host rule and response headers
that keep DNS rebinding pages and cross-origin embeds out. The container smoke
test also ran the [strict local recipe](../../../examples/strict-local/README.md)
with the image built from the checkout; see [Container recipe](#container-recipe).
No SDK, browser or real-cloud run was performed: an unmodified Go storage client
against the trusted listener and a browser remain unverified, which keeps the row
PARTIAL. gRPC has no trusted listener.

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
cargo test --locked --lib storage::resumable_http_tests
cargo test --locked --bin gcs-mimic
cargo test --locked --test it_xml_auth trusted_listener
cargo test --locked --test it_xml_auth auth_http
cargo test --locked --test it_xml_auth signed_browser
cargo test --locked --test it_admin admin_http
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
| `--lib config::` | PASS: 63 passed, 0 failed |
| `--lib server::` | PASS: 133 passed, 0 failed |
| `--lib xml::` | PASS: 38 passed, 0 failed |
| `--lib grpc::` | PASS: 156 passed, 0 failed |
| `--lib storage::resumable_http_tests` | PASS: 106 passed, 0 failed |
| `--bin gcs-mimic` | PASS: 32 passed, 0 failed |
| `--test it_xml_auth trusted_listener` | PASS: 7 passed, 0 failed |
| `--test it_xml_auth auth_http` | PASS: 141 passed, 0 failed |
| `--test it_xml_auth signed_browser` | PASS: 9 passed, 0 failed |
| `--test it_admin admin_http` | PASS: 35 passed, 0 failed |
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
| `config::tests::trusted_allowed_hosts_need_the_listener_and_name_dns_hosts` | Entries are lowercased, lose a trailing dot and are deduplicated; the setting without `trusted_listen` fails on it; schemes, ports, wildcards, empty entries, IP addresses, spaces and a 33rd name are rejected |
| `config::unix_tests::unix_socket_configuration_has_no_implicit_tcp_or_tls_fallback` | A valid strict pair with `unix_socket` fails on `unix_socket`, naming `trusted_listen` |
| `auth::tests::the_trusted_listener_changes_only_requests_without_credentials` | Without a principal the trusted listener stays anonymous; an unknown id is refused; after trusting, only the trusted listener resolves a credential-free request to the principal; another token resolves to its own identity; unknown, empty, Basic, duplicate and query credentials stay 401 on the trusted listener; the enabled test signer can be trusted; permissive mode refuses |
| `startup_rejects_a_trusted_principal_missing_from_the_registry` | A principal outside the identity file stops startup before the data directory is created, with a diagnostic naming it |
| `requests_without_credentials_act_as_the_trusted_principal_only_on_the_trusted_listener` | Ready reports `trusted_listen` and `trusted_principal` and keeps one entry in `listeners`; without credentials the main listener answers 401 for JSON bucket creation, media upload, JSON and XML reads, XML PUT and resumable initiation, while the trusted listener succeeds; the anonymous upload records the same owner and ACL entities as an upload presenting the principal's token; the resumable Location and the final `mediaLink` use the trusted Host `gcs:4444` |
| `presented_credentials_and_public_grants_decide_alike_on_both_listeners` | On the trusted listener a viewer token reads but cannot create a bucket, a role-less token gets 403, unknown bearer, Basic and `access_token` credentials get 401, and an unverifiable V2 signed URL gets 403; a bucket granting `allUsers` `roles/storage.objectViewer` is readable anonymously through JSON and XML on the main listener while the private bucket stays 401 |
| `browser_initiated_requests_are_refused_on_the_trusted_listener_only` | `Origin` (including `null`) and `Sec-Fetch-Site` `cross-site`/`same-origin` get 403 naming the trusted listener on JSON, XML and probe routes; the same requests to the main listener read the public object; `Sec-Fetch-Site: none` passes |
| `foreign_host_names_are_misdirected_on_the_trusted_listener_even_with_credentials` | A private object reads through Hosts `127.0.0.1:PORT`, `[::1]:PORT`, `localhost:PORT`, `app.localhost:PORT`, `gcs` and the listed alias; `evil.example:PORT` gets 421 `misdirected` naming `trusted_allowed_hosts` without credentials, with the principal's token and with another token; a request without Host gets 421; the main listener still answers that foreign Host |
| `trusted_responses_refuse_cross_origin_embedding_and_public_ones_are_unchanged` | Trusted-listener 200, probe, 404, 401, 403 (role and browser) and 421 responses carry `Cross-Origin-Resource-Policy: same-origin` and `X-Content-Type-Options: nosniff`; main-listener 200, 401 and probe responses carry neither |
| `server::browser::tests::only_the_trusted_listener_refuses_browser_initiated_requests` | The shared check refuses every browser header combination only for the trusted role |
| `server::browser::tests::the_trusted_listener_admits_only_hosts_a_rebinding_page_cannot_name` | IP literals, `localhost` and `.localhost` names, single labels (also uppercase with a trailing dot) and a listed name pass; dotted foreign names, a parent domain of a listed name, a listed name when nothing is configured and a missing Host get 421; duplicate Host fields get 400; a bearer token does not bypass the rule; the public role admits every Host |
| `server::browser::tests::trusted_responses_refuse_cross_origin_embedding_and_sniffing` | The response headers replace an existing `X-Content-Type-Options` value instead of adding a second one |
| `server::host::tests::the_request_target_wins_and_malformed_hosts_name_nothing` | The absolute-form authority wins over Host; names are lowercased without port or trailing dot; duplicate fields, user information, a non-numeric or empty port, an empty host and a path fail |
| `server::host::tests::literals_and_single_labels_are_told_apart_from_dotted_names` | IPv4 and bracketed IPv6 literals and single labels are told apart from dotted names |
| `server::admin::guard::tests::management_guards_run_before_every_route_specific_gate` | Management keeps refusing the same browser headers through the shared check |
| `batch_parts_and_management_follow_the_listener_of_their_request` | A batch part runs as the principal on the trusted listener (200) and anonymously on the main one (401); `PUT /_internal/config` without a token succeeds on the trusted listener only, because the principal is the administrator |
| `server::admin::tests::config_uses_the_instance_principal_instead_of_project_roles` | Management without a token is 401 on the public role and 200 on the trusted role whose principal is the administrator; a presented non-administrator token is 403 on both |
| `process_shutdown_reports_stopped_and_releases_catalog_for_restart` | The default ready key set is unchanged |
| `help_output_keeps_its_exact_bytes` | The help digest pins the trusted listener option lines |

The listener shares `max_connections` and every timeout because it is served by
the same accept loop and connection code as the other HTTP listeners; no test
measures that separately. The browser and Host checks run once per request the
listener receives, before routing, so batch parts inherit their envelope's
admission. Responses that hyper writes itself for a malformed request head do
not pass through the service and carry no added headers.

## Why the Host rule keeps pages out

The browser refusal relies on headers that browsers send for page requests. The
[Fetch Metadata](https://w3c.github.io/webappsec-fetch-metadata/) working draft
of 21 September 2026 sends `Sec-Fetch-*` only to potentially trustworthy URLs
(loopback or HTTPS) and uses `none` for navigations the user started, such as a
typed URL. The [Fetch standard](https://fetch.spec.whatwg.org/) (last updated
19 October 2026) appends `Origin` to CORS-mode requests and to requests whose
method is neither GET nor HEAD, so a plain link, image or same-origin GET
carries none. A page that rebinds its own DNS name to the listener therefore
sends GETs that carry neither header.

Its Host still names the page's own origin. Rebinding needs a name that the
attacker's DNS answers, which is a public name with a dot. A page cannot make a
browser send an IP literal (no DNS is involved), a `localhost` name (browsers
resolve those to loopback, and no public DNS delegates the reserved name) or a
single-label name for an origin its attacker controls, so the listener accepts
only those and the names an operator lists in `trusted_allowed_hosts`.
`Cross-Origin-Resource-Policy: same-origin` and `X-Content-Type-Options:
nosniff` additionally stop a page on another origin from using a response as an
opaque embed. None of this stops a program that is not a browser: anything that
reaches the port with an accepted Host, such as its IP address, acts as
`trusted_principal`, so keeping the port unpublished remains the protection.

## Container recipe

Run on **2026-11-08** on macOS arm64 with Docker Engine 29.9.2 and Docker
Compose 5.6.1. The script built the image from the checkout for linux/arm64;
linux/amd64 was not run.

```sh
bash scripts/smoke-containers.sh linux/arm64
```

| Scenario | Result |
| --- | --- |
| The five earlier scenarios of the script | PASS |
| `PASS: linux/arm64 strict local recipe: ready with test signer and trusted listener, credential-free trusted reads and writes, anonymous main listener with allUsers and CORS, portless and exact signed GET, tampered query, signed PUT, browser and Host refusals, test signer credentials, bucket file reconcile` | PASS |

The scenario starts `examples/strict-local/compose.yaml`, which
`scripts/check-docs.sh` keeps identical to the block its README shows, with a
fresh project and a free published port, and checks:

| Step | Behavior |
| --- | --- |
| Ready | The test signer warning before readiness; ready reports strict mode, the enabled test signer, `signed_urls` `verify`, `signed_host_port` `lenient`, one entry in `listeners`, `trusted_listen` `0.0.0.0:4444`, `trusted_principal` `app@example.invalid` and two created buckets; only port 4443 is published |
| Trusted listener | From the pinned curl image on the Compose network, Host `gcs:4444` and no credentials: the bucket list holds both buckets, `app-public` grants `allUsers` `roles/storage.objectViewer` and `app-private` grants `allUsers` nothing; uploads to both buckets succeed and the private object reads back |
| Main listener | From the host without credentials: the private object answers 401 through JSON and XML; the public object answers 200 through both, and with `Origin: http://localhost:3000` carries that `Access-Control-Allow-Origin` |
| Signed URLs | `gcs-mimic url sign` inside the container with the built-in signer: a URL for `http://localhost` without the port, as Go storage signs, reads the private object at `localhost:PORT` and logs `signature_host_port_omitted`; a URL for `http://localhost:PORT` reads it; a changed `X-Goog-Expires` gets 403 `SignatureDoesNotMatch`; a signed PUT passes its preflight for `content-type`, answers 200 with `Access-Control-Allow-Origin` and stores the bytes |
| Trusted refusals | From the peer, `Origin: http://evil.example` gets 403 and `Host: evil.example:4444` gets 421 |
| Credentials | `docker compose run --rm gcs test-signer credentials` prints JSON whose `client_email` is `test-signer@gcs-mimic.invalid` and whose `private_key` starts with `-----BEGIN PRIVATE KEY-----` |
| Reconcile | `up --force-recreate` with `FRONTEND_PORT=3001` reports two reconciled buckets; the public object then allows `http://localhost:3001` and no longer `http://localhost:3000` |

All requests are sent with curl, so no browser enforces CORS or meets the
browser checks, and no Go client runs. While writing the recipe, these were
observed once by hand with the same Compose version and are not part of the
script: Compose refuses inline configs in a service with `read_only: true`
(`cannot create config ... in read-only service gcs`); a plain `up -d` after
changing `FRONTEND_PORT` kept the running container, and `restart` kept the old
config content, so only a recreated container reconciled; an application
compose.yaml that lists the recipe under `include:` started it and reached
`gcs:4444` from another service; and the 0.13.0 image stops at startup with
`Unknown setting` for the test signer, signed host port and trusted listener
settings.
