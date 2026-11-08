# Built-in test signer and permissive signature verification

<!-- markdownlint-configure-file {"MD013": false} -->

This evidence covers local behavior only. It establishes no Google, SDK, container or platform fidelity. The commands ran on the pinned toolchain against this change; the full suite script, SDK suites and container builds were not run.

## Built-in test signer

- `test_signer` defaults to `enabled` only in permissive mode with `signed_urls=verify` and to `disabled` otherwise; an explicit value wins, except that an explicit `enabled` while signatures are ignored fails startup with "test_signer=enabled has no effect while signatures are ignored". An explicit `disabled` is accepted everywhere. The ready event reports the effective value. Strict mode with `test_signer=enabled` emits one startup warning that the published key lets anyone authenticate as `test-signer@gcs-mimic.invalid` and reach `allAuthenticatedUsers` grants and anything granted to it.
- With strict `test_signer=enabled`, `test-signer@gcs-mimic.invalid` exists with the public key of `fixtures/signing/test-only-public.pem`, embedded from that single repository copy. Without an identity file entry it has no project roles: its V4 and V2 RSA signed URLs, including those of `gcs-mimic url sign` without `--signer` and `--private-key`, get 403 `AccessDenied` until a bucket grant names `user:test-signer@gcs-mimic.invalid`, then return the object; a changed path still fails with `SignatureDoesNotMatch`. Its self-signed JWT authenticates and follows bucket grants.
- A declared entry keeps its tokens, project roles and ACL entity and gains the built-in key only when the signer is enabled; the ready event's `identities` counts only file entries. An ACL alias or natural user entity naming the signer fails startup with a diagnostic naming `test_signer=disabled`.
- Strict mode by default and with `test_signer=disabled` has no built-in signer, declared or not: signatures with the published key fail with 403 `InvalidAccessKeyId` whose message says the built-in test signer is disabled and names `test_signer=enabled`, even with a bucket grant; its self-signed JWT gets 401, and a declared entry's own token still works. An unknown signer keeps the strict message about an unknown key or registered identity.
- `url sign` without both credentials produces the same URL as an explicit signature with the published key; one credential alone fails with a diagnostic.
- `gcs-mimic test-signer credentials` prints JSON with `type`, `project_id`, `client_email`, `private_key` and `universe_domain`. Its private key signs what the embedded public key verifies; a URL signed with it returns the object from a permissive `signed_urls=verify` server, and the same URL with a changed signature gets 403. `test-signer` without a subcommand, with another subcommand or with extra arguments fails without output; `test-signer --help` prints usage.
- The Docker build context contains `fixtures/signing/test-only-private.pem` and `fixtures/signing/test-only-public.pem` and still excludes other files of that directory, checked with throwaway `FROM scratch` builds; the release image itself was not built, so `docker run IMAGE test-signer credentials` was not executed.

## Permissive verification

- `signed_urls` defaults to `ignore` in permissive mode and `verify` in strict mode; the ready event reports the effective value. Strict mode rejects an explicit `ignore` at startup.
- Configuration validation alone selects the authentication profile that startup loads. Disagreeing settings (strict without an identity file, strict with `signed_urls=ignore`, a permissive identity file, `signed_host_port=lenient` or an explicit `test_signer=enabled` while signatures are ignored, or an invalid value of any of these settings) leave no profile, and loading a profile reports only identity file contents. `signed_host_port` comes from the selected profile, so no construction path keeps `exact` by omission.
- With `signed_urls=verify`, a permissive server verifies V4 query and header HMAC (`GOOG4-HMAC-SHA256` and `AWS4-HMAC-SHA256`), V4 RSA, V2 RSA and HMAC queries, signed `aws-chunked` PUTs and V2/V4 POST policies. Verified requests succeed without any grant; the HMAC key's service account has no identity entry. Unsigned XML and JSON requests, including one with an unregistered bearer token, still succeed.
- Changed paths and headers get 403 `SignatureDoesNotMatch`, unknown HMAC keys and RSA signers 403 `InvalidAccessKeyId` whose message names identity file `public_keys`, HMAC keys created through the JSON API and the built-in test signer instead of a registered identity, expired V4 and V2 signatures 400 `ExpiredToken`, a wrong chunk signature 403, and a POST policy signed more than 15 minutes ahead or 8 days earlier 400 `InvalidPolicyDocument`; none of them creates an object. A library test checks the same V2/V4 boundaries through the shared verifier and that a verified permissive signature returns no caller.
- Identity file public keys verify in permissive mode; with `test_signer=disabled` the built-in key gets `InvalidAccessKeyId` naming `test_signer=enabled`. The identity file may declare only `id`, `public_keys` and an empty or absent `tokens`: `project_roles`, `acl_entity`, non-empty `tokens`, `public_access_prevention_projects` and an identity without `public_keys` fail startup with diagnostics. `identity_file` without `signed_urls=verify` keeps failing, now naming `signed_urls=verify`; `local_creator_email` stays valid with a verifying identity file.
- The default `ignore` still accepts expired, unknown-key and unverifiable signatures and policies outside the date window. A signed `aws-chunked` upload there gets 403 `InvalidAccessKeyId` saying that signatures are ignored and naming `signed_urls=verify`.
- `X-Goog-Expires=0` and `X-Amz-Expires=0` add that 0 usually means the signer's expiry was less than one second away; other out-of-range values keep the range message.

## Executed checks

| Command or selection | Result |
| --- | --- |
| `cargo check --workspace --all-targets --locked` | PASS |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS |
| `cargo xtask lint` | PASS: 0 new production violations |
| `cargo test --locked --lib` | PASS: 1779 tests |
| `cargo test --locked --lib -- auth:: xml:: config::` | PASS: 175 tests |
| `cargo test --locked --lib -- signed_urls signing_keys permissive_verification config::tests` | PASS: 44 tests |
| `cargo test --locked --lib test_signer` | PASS: 6 tests |
| `cargo test --locked --bin gcs-mimic` | PASS: 32 tests |
| `cargo test --locked --test it_xml_auth` | PASS: 272 tests |
| `cargo test --locked --test it_xml_auth signing::` | PASS: 47 tests |
| `cargo test --locked --test it_process` | PASS: 79 tests |
