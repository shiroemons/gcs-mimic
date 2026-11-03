# Authentication and startup foundation verification

<!-- markdownlint-configure-file {"MD013": false} -->

The parent authentication, signing, ACL and readiness feature states remain unchanged. This evidence covers local regression behavior; it does not establish additional Google, SDK, container or platform fidelity.

## Verified behavior

- Signed-URL unit tests use the production XML preparation, catalog/key lookup and verification path. A live strict process accepts CLI-signed path and virtual-host GETs with the expected bytes and repeated signed headers; changed Host/port or required headers fail, and mixed V2/V4 credentials fail with 400.
- Signing rejects every V2, Google V4 and AWS V4 parameter name regardless of ASCII case, including encoded names in a supplied URL. Permissive V4 ignores credential shape; historical V2 query shape checks remain without signature or expiry verification.
- Per-server verifier budgets admit independent bounded blocking jobs. Streaming seed and chunk HMAC checks stay inline. Public RSA keys are shared by reference-counted slices. The new JWT request API verifies RSA through the same budget; anonymous and static-token resolution does not wait for a cryptographic permit.
- Compact project grants preserve explicit storage-admin authority, membership from roles without object permissions, and the existing persisted upload-snapshot format.
- JWT-shaped static tokens are rejected at registry load. TLS and registry diagnostics retain safe details without echoing token values. Explicit administrative principals require strict mode, enabled administration and a loaded registry ID.
- IAM and ACL user matching folds domain case only, preserves local-part/non-email case and stored spelling, and rejects ambiguous registry aliases. SPEC-PRINCIPAL-EMAIL-CASE retains provider questions.
- CORS accepts exactly 64 KiB of serialized JSON and rejects one byte more. Custom-time errors preserve pre-epoch dates. Typed stored-policy errors retain the JSON source through catalog conversion. Aborting a restarting task drops its active run.

## Executed checks

| Command or selection | Result |
| --- | --- |
| `CARGO_BUILD_JOBS=4 cargo check --workspace --all-targets` | PASS |
| `CARGO_BUILD_JOBS=4 cargo clippy --workspace --all-targets -- -D warnings` | PASS |
| `CARGO_BUILD_JOBS=4 cargo test --package xtask lint::` | PASS: 20 lint tests |
| Library filters `auth::`, `config::`, `task::`, `bucket::tests`, `notification::tests`, `csek::`, `bucket_init::`, `object::custom_time_tests` | PASS: 147 tests |
| `CARGO_BUILD_JOBS=4 cargo test --lib object::unix_nanos::` | PASS: 6 tests |
| `CARGO_BUILD_JOBS=4 cargo test --lib permissive_v4_ignores_shape` | PASS: 1 test |
| `CARGO_BUILD_JOBS=4 cargo test --test it_xml_auth signing_cli_urls_verify_through_the_live_xml_route` | PASS: 1 live-process test |
| `CARGO_BUILD_JOBS=4 cargo test --test it_process healthcheck_defaults` | PASS: 3 tests |
| `CARGO_BUILD_JOBS=4 cargo test --bin gcs-mimic startup::` | PASS: 15 tests |
| `CARGO_BUILD_JOBS=4 cargo test --test it_sync sync_marker_failure_precedes_bind_and_ready_and_process_exports_after_ready` | PASS: 1 test |

## Integration boundary

Request adapters still use the temporary synchronous authentication entry point until their coordinated migration. Permission arguments still use the existing string API while the internal compact representation is in place. The lint implementation and its tests are complete, but the baseline is deliberately generated only after integrating parallel changes; without that file, `cargo xtask lint` fails visibly. The complete `GCSM_CHECK_CROSS=0 just check` gate is pending that integration. No performance gain is claimed or measured.

The sync thread-creation failure has no deterministic test seam; startup now calls it before maintenance and readiness. Existing sync startup and control tests cover the normal order. No durable write protocol changed. SDK suites, container builds, real Google requests and physical power-loss testing were not run for these changes.
