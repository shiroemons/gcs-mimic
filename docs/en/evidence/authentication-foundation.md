# Authentication and startup foundation verification

<!-- markdownlint-configure-file {"MD013": false} -->

The parent authentication, signing, ACL and readiness feature states remain unchanged. This evidence covers local regression behavior; it does not establish additional Google, SDK, container or platform fidelity.

## Verified behavior

- Signed-URL unit tests use the production XML preparation, catalog/key lookup and verification path. A live strict process accepts CLI-signed path and virtual-host GETs with the expected bytes and repeated signed headers; changed Host/port or required headers fail, and mixed V2/V4 credentials fail with 400.
- Signing rejects every V2, Google V4 and AWS V4 parameter name regardless of ASCII case, including encoded names in a supplied URL. Permissive V4 ignores credential shape; historical V2 query shape checks remain without signature or expiry verification.
- Per-server verifier budgets admit independent bounded blocking jobs. Streaming seed and chunk HMAC checks stay inline through a synchronous API. Public RSA keys are shared by reference-counted slices. Every JSON, XML, form, compressed, administrative and gRPC bearer path verifies JWT RSA through the same budget; anonymous and static-token resolution does not wait for a cryptographic permit.
- Compact project grants preserve explicit storage-admin authority, HMAC-key management permissions, membership from roles without object permissions, and the existing persisted upload-snapshot format. Authorization carries `Permission` through every layer; testIamPermissions parses names only at the input boundary. HMAC-key permissions remain excluded from bucket testIamPermissions; object testIamPermissions remains unsupported.
- JWT-shaped static tokens are rejected at registry load. TLS and registry diagnostics retain safe details without echoing token values. Explicit administrative principals require strict mode, enabled administration and a loaded registry ID.
- IAM and ACL user matching folds domain case only, preserves local-part/non-email case and stored spelling, and rejects ambiguous registry aliases. SPEC-PRINCIPAL-EMAIL-CASE retains provider questions.
- CORS accepts exactly 64 KiB of serialized JSON and rejects one byte more. Custom-time errors preserve pre-epoch dates. Typed stored-policy errors retain the JSON source through catalog conversion. Aborting a restarting task drops its active run.

## Executed checks

| Command or selection | Result |
| --- | --- |
| `CARGO_BUILD_JOBS=4 cargo check --workspace --all-targets` | PASS |
| `CARGO_BUILD_JOBS=4 cargo clippy --workspace --all-targets -- -D warnings` | PASS |
| `CARGO_BUILD_JOBS=4 cargo test --package xtask lint::` | PASS: 21 lint tests, including independent shared/target growth and stale-count rejection |
| Library filters `auth::`, `config::`, `task::`, `bucket::tests`, `notification::tests`, `csek::`, `bucket_init::`, `object::custom_time_tests` | PASS: 147 tests |
| `CARGO_BUILD_JOBS=4 cargo test --lib object::unix_nanos::` | PASS: 6 tests |
| `CARGO_BUILD_JOBS=4 cargo test --lib compact_grants_preserve_hmac_key_management_permissions` | PASS: 1 test |
| `CARGO_BUILD_JOBS=4 cargo test --lib permissive_v4_ignores_shape` | PASS: 1 test |
| `CARGO_BUILD_JOBS=4 cargo test --test it_xml_auth signing_cli_urls_verify_through_the_live_xml_route` | PASS: 1 live-process test |
| `CARGO_BUILD_JOBS=4 cargo test --test it_process healthcheck_defaults` | PASS: 3 tests |
| `CARGO_BUILD_JOBS=4 cargo test --bin gcs-mimic startup::` | PASS: 15 tests |
| `CARGO_BUILD_JOBS=4 cargo test --test it_sync sync_marker_failure_precedes_bind_and_ready_and_process_exports_after_ready` | PASS: 1 test |
| `CARGO_BUILD_JOBS=4 cargo test --lib auth::` after integration | PASS: 59 tests |
| Library filters `grpc_iam_validates_typed_policy_and_permission_values`, `legacy_reconciliation_preserves_existing_iam_bindings_and_permission_union`, `authentication_worker_failures_keep_their_source_and_unavailable_status`, `published_aws_streaming_seed_and_three_chunk_signatures_match_exactly` | PASS: 1 test each |
| `CARGO_BUILD_JOBS=4 cargo test --test it_json strict_soft_deleted_bucket_access_uses_project_permissions` | PASS: 1 test |
| `CARGO_BUILD_JOBS=4 cargo xtask lint` | PASS: 0 new violations; 352 native Clippy findings plus 6 shared text findings |
| `cargo xtask lint --target` with the C stub and `CARGO_TARGET_DIR=target/cross` | PASS: Windows MSVC 354; Linux GNU 355; Linux musl 355; 0 new violations on each |
| `env -u GCSM_CHECK_CROSS bash scripts/test-pre-push.sh`, `scripts/test-check.sh`, `scripts/test-trim-target.sh`, then `bash scripts/trim-target.sh` | PASS |
| `GCSM_CHECK_CROSS=0 CARGO_BUILD_JOBS=4 env -u RUSTUP_TOOLCHAIN bash scripts/with-test-tmpdir.sh bash scripts/check.sh` before the target-baseline extension | PASS: 16 test binaries and doctests; library 1402, XML 249, process 65; 1 existing ignored test; docs and offline shell self-tests passed |

## Final validation boundary

The temporary synchronous JWT verifier and string authorization APIs have been removed. Shared text rules use `xtask/lint-baseline.txt`; Clippy uses one file per compiler target. The native compiler triple is explicit even when local Cargo configuration selects a different default target. Every gate rejects both growth and stale counts, and loads only its shared or selected-target file. Baselines were fixed once from integrated sources: native macOS ARM64 352 Clippy findings, Windows MSVC 354, Linux GNU 355, Linux musl 355 and 6 shared static-string errors. No performance gain is claimed or measured.

The full native gate passed after replacing the old generic identity-file diagnostic expectation with the specific rejected reason. An earlier run also hit a resumable-cleanup 30-second hang guard and an XML cleanup observation race; those tests passed unchanged both individually and in the subsequent full native run. After extending the target-aware ratchet, the native gate and all three production target gates pass. The complete Windows all-target cross check still requires corrections to existing Unix-only fault-seam references and unused imports/variants; another complete cross and native run remains pending those corrections. The Linux inotify safety rationale was moved directly beside its unsafe ownership conversion; executable code did not change.

The sync thread-creation failure has no deterministic test seam; startup now calls it before maintenance and readiness. Existing sync startup and control tests cover the normal order. No durable write protocol changed. SDK suites, container builds, real Google requests and physical power-loss testing were not run for these changes.
