# Local credentials and empty soft-deleted listings

Scope: DESIGN-084 (PARTIAL), JSON-OBJECTS-011 (PARTIAL), JSON-BUCKETS-010
(NOT_STARTED). Statuses and HTTP responses remain unchanged.

## Acceptance

- local-auth-gap-recorded: record googleauth 1.17.6's ignored JSON token_uri,
  Rails storage.yml limitations and external-account V2 signing limitations.
- local-auth-recipe: verify application-supplied service-account credentials
  with a preset local token in the guarded Active Storage scenario; no token
  issuing endpoint exists in the emulator.
- soft-delete-policy-example: document seven-day Go v1.70.0 and Ruby 1.62.1
  policy setters in both languages, matching existing SDK tests.
- soft-delete-debug-log: one debug event explains an empty softDeleted=true
  listing without a policy and suggests retentionDurationSeconds=604800.
  Only debug requests with empty pages perform the extra bucket metadata read;
  diagnostic read failures never change the listing response.

## Verification

Checked 2026-10-27 against installed, lockfile-pinned googleauth 1.17.6,
Rails 8.1.4 and Storage 1.62.1 sources. googleauth's `service_account.rb` SHA-256:
`de86639f3efcbc1e98f1f74fbf99a9abac89ff37aaa50840d0a51d3a50c273ad`;
Storage's `file/signer_v2.rb`:
`30097e61257cc66c52c0ab780a63ad6218d16f57cc554bfabede5734193119fb`.
The service-account factory accepts a constructor token_credential_uri but does
not read JSON token_uri. Rails forwards options to the Storage constructor,
which has no token_credential_uri option. V2 needs an issuer and signing key;
external-account credentials do not supply that key. See
[googleauth release](https://rubygems.org/gems/googleauth/versions/1.17.6),
[Rails adapter](https://github.com/rails/rails/blob/v8.1.4/activestorage/lib/active_storage/service/gcs_service.rb)
and [Storage release](https://rubygems.org/gems/google-cloud-storage/versions/1.62.1).
The [official list reference](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/list)
documents the absent-policy 400; the owner-approved local empty 200 is unchanged.
SDK policy arguments match `sdk-tests/go/objects_soft_delete_test.go` and
`sdk-tests/ruby/https.rb`; the Go update form is also used in that test.

- `cargo xtask dependencies check`: PASS; all compiler/library/SDK/container
  inputs CURRENT (Rust 1.99.1); no dependency changes.
- `cargo check --workspace --all-targets --locked --offline`: PASS.
- `cargo test --lib --locked --offline empty_soft_deleted_listing_explains_missing_policy_only`:
  PASS, one test. It checks absent/present policy, ordinary listing, prefixes,
  continuation pages, repair text and unchanged empty JSON rendering.
- `cargo test --locked --offline --test log_level`: PASS, two tests.
  Existing `tests/log_level.rs` stderr capture checks exactly one diagnostic
  at debug, none at other levels, and no diagnostic for ordinary listings or
  empty listings after setting the policy; every listing stays empty 200.
  No new capture framework was added.
- `ruby -c sdk-tests/ruby/active_storage/scenario.rb`: PASS.
- `just check-active-storage`: one guarded HTTP execution, exit 1;
  **1 PASS, 6 PASS_WITH_WORKAROUND, 1 FAIL**. AS-LOCAL-CREDENTIALS passes
  upload/download/delete, absence and unchanged V2 signed GET with preset
  access_token/expires_at. AS-DEFAULT-AUTH retains the known
  `Google::Auth::AuthorizationError: request targets https://oauth2.googleapis.com:443`
  before socket creation. No external OAuth request was sent.
- Final `just check`: PASS with loopback socket access enabled; formatting,
  host/cross-target clippy, all Rust/xtask tests, generated compatibility checks
  and offline shell/publication checks pass.
- `npx --yes markdownlint-cli2` on the ten changed Markdown files: PASS,
  zero issues. The two soft-delete guides exclude code-block line length only
  to preserve the requested single-line SDK examples.

Full Go/Ruby SDK, container and guide end-to-end runs were not executed because
HTTP responses are unchanged; the single Active Storage recipe run was a scoped
exception. No benchmarks or cloud differential tests. Preset token renewal and
strict Rails integration remain unverified; ordinary storage.yml authentication
still fails. DESIGN-084/JSON-OBJECTS-011 stay PARTIAL; JSON-BUCKETS-010 stays
NOT_STARTED. Only their notes change; regenerating both matrices produces no
diff because notes are not rendered. The new
SPEC-ACTIVE-STORAGE-LOCAL-CREDENTIALS gap remains OPEN.
