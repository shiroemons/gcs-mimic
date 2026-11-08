# Built-in test signer verification

<!-- markdownlint-configure-file {"MD013": false} -->

This evidence covers local behavior only. It establishes no Google, SDK, container or platform fidelity. The commands ran on the pinned toolchain against this change; the full suite, SDK suites and container builds were not run.

## Verified behavior

- `test_signer` defaults to `enabled` in permissive mode and `disabled` in strict mode, and an explicit value wins in both; the ready event reports the effective value. Strict mode with `test_signer=enabled` emits one startup warning that the published key lets anyone authenticate as `test-signer@gcs-mimic.invalid` and reach `allAuthenticatedUsers` grants and anything granted to it.
- With strict `test_signer=enabled`, `test-signer@gcs-mimic.invalid` exists with the public key of `fixtures/signing/test-only-public.pem`, embedded from that single repository copy. Without an identity file entry it has no project roles: its V4 and V2 RSA signed URLs, including those of `gcs-mimic url sign` without `--signer` and `--private-key`, get 403 `AccessDenied` until a bucket grant names `user:test-signer@gcs-mimic.invalid`, then return the object; a changed path still fails with `SignatureDoesNotMatch`. Its self-signed JWT authenticates and follows bucket grants.
- A declared entry keeps its tokens, project roles and ACL entity and gains the built-in key only when the signer is enabled; the ready event's `identities` counts only file entries. An ACL alias or natural user entity naming the signer fails startup with a diagnostic naming `test_signer=disabled`.
- Strict mode by default and with `test_signer=disabled` has no built-in signer, declared or not: signatures with the published key fail with 403 `InvalidAccessKeyId` even with a bucket grant, its self-signed JWT gets 401, and a declared entry's own token still works.
- `url sign` without both credentials produces the same URL as an explicit signature with the published key; one credential alone fails with a diagnostic.
- The Docker build context contains `fixtures/signing/test-only-private.pem` and `fixtures/signing/test-only-public.pem` and still excludes other files of that directory, checked with throwaway `FROM scratch` builds; the release image itself was not built.

## Executed checks

| Command or selection | Result |
| --- | --- |
| `cargo check --workspace --all-targets --locked` | PASS |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS |
| `cargo xtask lint` | PASS: 0 new production violations |
| `cargo test --locked --lib test_signer` | PASS: 5 tests |
| `cargo test --locked --lib auth::` | PASS: 65 tests |
| `cargo test --locked --lib config::tests` | PASS: 37 tests |
| `cargo test --locked --bin gcs-mimic` | PASS: 32 tests |
| `cargo test --locked --test it_xml_auth signing::` | PASS: 43 tests |
| `cargo test --locked --test it_xml_auth auth_http` | PASS: 137 tests |
| `cargo test --locked --test it_xml_auth sign_url_cli` | PASS: 2 tests |
| `cargo test --locked --test it_process` | PASS: 76 tests |
