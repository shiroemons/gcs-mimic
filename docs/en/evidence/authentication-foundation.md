# Authentication and startup foundation verification

<!-- markdownlint-configure-file {"MD013": false} -->

The parent authentication, signing, ACL and readiness feature states remain unchanged. This evidence covers local regression behavior; it does not establish additional Google, SDK, container or platform fidelity.

## Verified behavior

- Signed-URL unit tests use the production XML preparation, catalog/key lookup and verification path. A live strict process accepts CLI-signed path and virtual-host GETs with the expected bytes and repeated signed headers, including a configuration-file base with uppercase `LOCALHOST` and a trailing slash normalized to its final authority; changed Host/port or required headers fail, and mixed V2/V4 credentials fail with 400.
- Signing rejects every V2, Google V4 and AWS V4 parameter name regardless of ASCII case, including encoded names in a supplied URL. Permissive V4 ignores credential shape; historical V2 query shape checks remain without signature or expiry verification.
- Per-server verifier budgets admit independent bounded blocking jobs. Streaming seed and chunk HMAC checks stay inline through a synchronous API. Public RSA keys are shared by reference-counted slices. Every JSON, XML, form, compressed, administrative and gRPC bearer path verifies JWT RSA through the same budget; anonymous and static-token resolution does not wait for a cryptographic permit. The gRPC v1 and v2 Unauthenticated message is intentionally shared; verifier failures retain their source and return Unavailable.
- Compact project grants preserve explicit storage-admin authority, HMAC-key management permissions, membership from roles without object permissions, and the existing persisted upload-snapshot format. Authorization carries `Permission` through every layer; testIamPermissions parses names only at the input boundary. HMAC-key permissions remain excluded from bucket testIamPermissions; object testIamPermissions remains unsupported.
- JWT-shaped static tokens are rejected at registry load. Registry diagnostics retain safe details without echoing token values. TLS errors distinguish certificate, private-key and combined-identity failures, preserve the I/O cause and identify the affected setting with bounded PEM repair instructions. Explicit administrative principals require strict mode, enabled administration and a loaded registry ID.
- IAM and ACL user matching folds domain case only, preserves local-part/non-email case and stored spelling, and rejects ambiguous registry aliases. SPEC-PRINCIPAL-EMAIL-CASE retains provider questions.
- CORS accepts exactly 64 KiB of serialized JSON and rejects one byte more. Custom-time errors preserve pre-epoch dates. Typed stored-policy errors retain the JSON source through catalog conversion. Aborting a restarting task drops its active run.

## Executed checks

| Command or selection | Result |
| --- | --- |
| `CARGO_BUILD_JOBS=4 cargo check --workspace --all-targets` | PASS |
| `CARGO_BUILD_JOBS=4 cargo clippy --workspace --all-targets -- -D warnings` | PASS |
| `CARGO_BUILD_JOBS=4 cargo fmt --all --check` | PASS |
| `CARGO_BUILD_JOBS=4 cargo test --package xtask lint::` | PASS: 24 tests, including pinned/stable decrease policy, unconditional growth rejection, missing-target empty baselines, native command construction and no-op detection |
| `CARGO_BUILD_JOBS=4 cargo test --test it_process startup_` | PASS: 5 tests |
| `CARGO_BUILD_JOBS=4 cargo test --lib server_identity_errors` | PASS: 1 test |
| `CARGO_BUILD_JOBS=4 cargo test --lib server::admin::sync::tests::` after the final pagination-fixture adjustment | PASS: 23 tests |
| `CARGO_BUILD_JOBS=4 cargo test --test it_xml_auth signing_cli_urls_verify_through_the_live_xml_route` | PASS: 1 live-process test |
| `CARGO_BUILD_JOBS=4 cargo xtask lint` | PASS: 0 new violations; 352 native Clippy findings plus 6 shared text findings |
| `CARGO_BUILD_JOBS=4 env -u RUSTUP_TOOLCHAIN bash scripts/check-cross.sh` | PASS: Windows MSVC, Linux GNU and Linux musl; workspace/all-target Clippy and production lint; 354/355/355 ratcheted findings, 0 new violations |
| `env -u GCSM_CHECK_CROSS bash scripts/test-pre-push.sh`, `scripts/test-check.sh`, `scripts/test-trim-target.sh`, then `bash scripts/trim-target.sh` | PASS |
| `GCSM_CHECK_CROSS=0 CARGO_BUILD_JOBS=4 env -u RUSTUP_TOOLCHAIN bash scripts/with-test-tmpdir.sh bash scripts/check.sh` | PASS: 16 test binaries and doctests; library 1410, process 66, XML 249, xtask 183; 1 existing ignored test; docs and offline shell self-tests passed |

## Final validation boundary

The temporary synchronous JWT verifier and string authorization APIs have been removed. Shared text rules use `xtask/lint-baseline.txt`; Clippy uses one file per compiler target. Native Clippy reuses the default native build without `--target`; cross checks select their target explicitly. Every gate rejects growth. Only the pinned compiler rejects stale Clippy counts; current-stable decreases warn and retain the pin's counts. Shared text decreases always fail. A missing target file acts as an empty baseline and never borrows another target's counts. Committed counts are native macOS ARM64 352 Clippy findings, Windows MSVC 354, Linux GNU 355, Linux musl 355 and 6 shared static-string errors. No performance gain is claimed or measured.

The integrated source includes explicit cleanup-ownership observation, platform-specific fault-seam guards and directory ownership-event access checks. The Linux inotify safety rationale sits directly beside its unsafe ownership conversion. The full cross and native gates passed on the final production sources without changing any baseline count. A final sync pagination-fixture adjustment changes test code only; workspace/all-target Clippy, its sync tests, formatting and the production lint gate validate that adjustment separately.

The sync thread-creation failure has no deterministic test seam; startup now calls it before maintenance and readiness. Existing sync startup and control tests cover the normal order. No durable write protocol changed. SDK suites, container builds, real Google requests and physical power-loss testing were not run for these changes.
