# Rails Active Storage scenario

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: DESIGN-084. An isolated, HTTP-only Rails GCS scenario with scoped V2 support
in [sdk-active-storage-v2-signing](sdk-active-storage-v2-signing.md). Record each case independently and return a failing exit status if
any case fails. Workarounds never count as default-call passes.

## Contract and acceptance

| Case | Required behavior |
| --- | --- |
| AS-DEFAULT-AUTH | The unmodified configured Rails GCS service uploads without internal authorization changes; reject outbound OAuth before network access and record that as FAIL. |
| AS-UPLOAD | `ActiveStorage::Blob.create_and_upload!` with omitted content_type identifies text and preserves bytes; an explicit binary type also works. |
| AS-DOWNLOAD | Independently seeded blobs support exact `blob.download` and checksum-verified `blob.open`. |
| AS-DELETE | `blob.purge` removes its database record and object, including the image variant prefix; `delete_prefixed` preserves a neighboring prefix. |
| AS-SIGNED-URL | Fetch the unchanged `blob.url` and verify bytes; report any provider-bound endpoint instead of rewriting it. |
| AS-DIRECT-UPLOAD | Send the unchanged `url_for_direct_upload` URL and `headers_for_direct_upload` in an HTTP PUT, then verify bytes. |
| AS-EXIST-COMPOSE | `exist?` distinguishes present/missing keys and the Rails GCS service composes ordered sources without deleting them. |

Cases use a standard `service: GCS` storage.yml with project, bucket, explicit
local credentials and one `endpoint` value, including the Ruby SDK's required
trailing slash (for example `http://127.0.0.1:4443/`). In-memory SQLite stores Rails blob
records; the normal Active Storage migration creates its schema. No web server,
image processor, external database or product dependency is added. Variant
deletion exercises the purge path using seeded variant bytes, not image rendering.
Only an isolated permissive emulator is in scope; strict signature validation,
browser CORS, public serving and actual image processing remain unverified.
AS-DEFAULT-AUTH first uses the configured service without internal changes.
The connection guard rejects its Google OAuth attempt before socket creation.
Only afterward, the harness clears the private generated client's authorization
to isolate operation coverage, retaining local credentials for signing. Success
under that workaround is `PASS_WITH_WORKAROUND`, never a default-call PASS.
Independent fixture seeding and cleanup use the public anonymous SDK constructor.
The guard rejects other origins, disables proxies/redirects and pins localhost
dialing to loopback. No Google credential discovery or local OAuth success is claimed.

## Sources and dependency selection

Checked 2026-10-27: [Rails guide](https://guides.rubyonrails.org/active_storage_overview.html),
[GCS adapter at v8.1.4](https://github.com/rails/rails/blob/v8.1.4/activestorage/lib/active_storage/service/gcs_service.rb),
tag object `6326130711fd92ac630f308e15399d0dace2570e`.
RubyGems active-version lists and version-specific metadata establish stable,
non-yanked activestorage/railties 8.1.4 and sqlite3 2.10.2. Ruby 4.0.9,
Bundler 4.0.23 and google-cloud-storage 1.62.1 match the parent suite.
Rails GCS requires google-apis-iamcredentials_v1 0.28.0 even with local signing;
no IAM call is authorized by this scenario.
Exact release metadata and checksum sources are the
[Active Storage](https://rubygems.org/api/v2/rubygems/activestorage/versions/8.1.4.json),
[railties](https://rubygems.org/api/v2/rubygems/railties/versions/8.1.4.json),
[SQLite](https://rubygems.org/api/v2/rubygems/sqlite3/versions/2.10.2.json) and
[IAM client](https://rubygems.org/api/v1/versions/google-apis-iamcredentials_v1.json)
RubyGems APIs. The lockfile commits package SHA-256 checksums and Bundler version.

Development-only dependency review: maintained Rails activestorage/railties
8.1.4 are MIT (73,728/205,312 compressed gem bytes), sqlite3 2.10.2 is BSD-3-Clause
(3,410,944 bytes for the source gem); the existing Google SDK and required IAM
client use Apache-2.0. The Rails meta-gem is omitted; lockfiles record transitives.
The existing daily freshness workflow runs the same xtask checker; its declared
SDK scopes now include this Gemfile. Native gem platform suffixes are validated
against the exact pinned numeric version, including mismatched-platform tests.

## Verification

HTTP execution on 2026-10-27 (Ruby 4.0.9, Bundler 4.0.23,
Rails 8.1.4, SDK 1.62.1). Case summary, with the authentication error abbreviated:

```text
AS-DEFAULT-AUTH: FAIL Google::Auth::AuthorizationError (outbound OAuth blocked)
AS-UPLOAD: PASS_WITH_WORKAROUND (OAuth authorization removed)
AS-DOWNLOAD: PASS_WITH_WORKAROUND (OAuth authorization removed)
AS-DELETE: PASS_WITH_WORKAROUND (OAuth authorization removed)
AS-SIGNED-URL: PASS_WITH_WORKAROUND (OAuth authorization removed)
AS-DIRECT-UPLOAD: PASS_WITH_WORKAROUND (OAuth authorization removed)
AS-EXIST-COMPOSE: PASS_WITH_WORKAROUND (OAuth authorization removed)
Summary: 0 PASS, 6 PASS_WITH_WORKAROUND, 1 FAIL
```

`just check-active-storage` exits 1 after all seven independent cases and successful
cleanup. Both unchanged signed requests now pass with the existing authentication workaround.
The default authentication case raises `Google::Auth::AuthorizationError` when
googleauth attempts `https://oauth2.googleapis.com:443`; the connection guard
refuses it before any external connection. No external OAuth request was sent.
Upload/download/delete/signing/existence/compose coverage succeeds only with internal
authorization removed; the ledger keeps those default acceptance cases PENDING.
Content type identification itself is not bypassed.
Direct-upload readback uses the anonymous fixture SDK client, avoiding an
authorization-removal workaround in that assertion. That readback now executes and
verifies the exact direct-upload bytes.

- `cargo check --workspace --all-targets --locked --offline`: PASS.
- `cargo test -p xtask --locked --offline`: PASS (26 unit, 11 compatibility and 2 dependency integration tests).
- `cargo xtask dependencies check`: PASS; all declared inputs, including the five new direct gem checks, Ruby and Bundler, are CURRENT.
- Ruby syntax checks, shell syntax and the offline origin self-check: PASS.
- `just check` (one execution): PASS, including formatting, host/cross-target clippy, the complete Rust and xtask suites, generated compatibility views and offline script checks.
- `npx --yes markdownlint-cli2 docs/en/slices/sdk-active-storage-scenario.md docs/ja/sdk-active-storage-scenario.md sdk-tests/ruby/active_storage/README.md docs/ja/README.md`: PASS (0 issues).

The original scenario added no product HTTP behavior. The later V2 slice reruns
the complete parent Go/Ruby SDK suite over HTTPS and records its checks separately.
Real-cloud differential tests, Rails strict integration, browser execution and benchmarks
were not run. No performance claim is made.

## Failures and next slices

| Case | Symptom | Cause | Proposed next slice |
| --- | --- | --- | --- |
| AS-DEFAULT-AUTH | First unmodified Rails upload raises Google::Auth::AuthorizationError before HTTP reaches the emulator. | Service-account credentials attempt OAuth at oauth2.googleapis.com; an SDK data endpoint alone does not configure token exchange. The guard blocks the attempt before network access. | `sdk-active-storage-local-auth`: verify a supported local credential configuration or separately scope a local token endpoint. |
| AS-SIGNED-URL | PASS_WITH_WORKAROUND; unchanged blob.url GET returns exact bytes. | V2 RSA query support is implemented; default authentication still needs the separate fix. | V2 signing complete; `sdk-active-storage-local-auth` remains. |
| AS-DIRECT-UPLOAD | PASS_WITH_WORKAROUND; unchanged URL/headers PUT and exact readback succeed. | Default GCS direct uploads select V2 when cache_control is absent; that subset is supported. | V2 signing complete; `sdk-active-storage-local-auth` remains. |

AS-UPLOAD, AS-DOWNLOAD, AS-DELETE, AS-SIGNED-URL, AS-DIRECT-UPLOAD and AS-EXIST-COMPOSE are conditional successes
only after private-client authorization removal. They remain pending as normal
storage.yml workflows until `sdk-active-storage-local-auth` resolves the first failure.

The [official V2 signing contract](https://docs.cloud.google.com/storage/docs/access-control/signed-urls-v2)
requires XML access, expiration and signed header checks; independent strict Go/Ruby
V2 tests now verify this subset. DESIGN-084 remains PARTIAL for default authentication.

The endpoint issue referenced by [sdk-ruby-defaults](sdk-ruby-defaults.md) is the
Ruby SDK's **V4 canonical signing host**, whose default is `storage.googleapis.com`
even when the emitted URL uses the configured service endpoint. In this scenario,
Rails' default **V2** URLs already target the configured local origin and arrive
unchanged; the previous unsupported V2 parameter failure is resolved. V4
(including Rails' cache_control-triggered V4 direct upload) and Rails strict
verification remain untested here. No host/path rewrite or `bucket_bound_hostname`
adaptation is applied. SPEC-ACTIVE-STORAGE-V2 is resolved for this scoped subset.
