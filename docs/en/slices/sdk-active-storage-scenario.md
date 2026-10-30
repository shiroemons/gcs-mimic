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
| AS-LOCAL-CREDENTIALS | A separate application-created GCS service with preset local access_token/expires_at uploads, downloads, deletes and signs unchanged V2 GET without internal authorization removal. This does not change AS-DEFAULT-AUTH. |

Cases use a standard `service: GCS` storage.yml with project, bucket, explicit
local credentials and one `endpoint` value, including the Ruby SDK's required
trailing slash (for example `http://127.0.0.1:4443/`). In-memory SQLite stores Rails blob
records; the normal Active Storage migration creates its schema. No web server,
image processor, external database or product dependency is added. Variant
deletion exercises the purge path using seeded variant bytes, not image rendering.
Only an isolated permissive emulator is in scope; strict signature validation,
public serving and actual image processing remain unverified. Browser CORS and
the strict-profile failure are covered by the
[storage.yml emulator recipe](#storageyml-emulator-recipe).
AS-DEFAULT-AUTH first uses the configured service without internal changes.
The connection guard rejects its Google OAuth attempt before socket creation.
Only afterward, the harness clears the private generated client's authorization
to isolate operation coverage, retaining local credentials for signing. Success
under that workaround is `PASS_WITH_WORKAROUND`, never a default-call PASS.
Independent fixture seeding and cleanup use the public anonymous SDK constructor.
The guard rejects other origins, disables proxies/redirects and pins localhost
dialing to loopback. No Google credential discovery or local OAuth success is claimed.

The later [local credential recipe](sdk-local-auth-softdelete-notes.md) adds
AS-LOCAL-CREDENTIALS: PASS with public application credential inputs. Its one
guarded run reports **1 PASS, 6 PASS_WITH_WORKAROUND, 1 FAIL** and exits 1 because
AS-DEFAULT-AUTH still fails. That recipe avoided token exchange with a preset
token and a one-day expiry; the [storage.yml emulator recipe](#storageyml-emulator-recipe)
below replaces it, because the expiry later triggered a refresh at Google.
SPEC-ACTIVE-STORAGE-LOCAL-CREDENTIALS records the googleauth and external-account
limitations. DESIGN-084 stays PARTIAL.

## storage.yml emulator recipe

Applications use the emulator in development and test and Google Cloud Storage
in production through one `gcs_emulator` entry in `config/storage.yml`. The
user-facing steps are in [Rails Active Storage](../rails-active-storage.md);
the suite uses the same entry (`config/storage.yml`) and bucket file
(`buckets.json`).

- **`universe_domain` avoids OAuth.** In googleauth 1.17.6,
  `ServiceAccountCredentials#enable_self_signed_jwt?` is true outside the
  `googleapis.com` universe, because OAuth is unavailable there. The client then
  signs its own JWT bearer for every request and never requests a token, so no
  expiry or refresh can reach Google. The service and the credentials must name
  the same universe; without the service-level value every call fails with
  `Universe domain is googleapis.com but credentials are in gcs-emulator.invalid`.
- **The throwaway key** is generated at boot in development and test only. It
  signs the JWT and V2 URLs; the permissive profile verifies neither.
- **Limits.** Strict mode accepts only tokens from its identity file. A
  one-off probe against a strict emulator got `401 Invalid Credentials` on the
  first upload, so this entry is permissive-only.

| Case | Required behavior |
| --- | --- |
| AS-EMULATOR-BUCKETS-FILE | `buckets.json` creates `local-bucket` with the Rails guide's direct-upload CORS rule. |
| AS-EMULATOR-ATTACH | With `service: :gcs_emulator` selected, `has_one_attached` uploads and downloads exact bytes, and the object is in the emulator bucket. |
| AS-EMULATOR-URL | The unchanged `attachment.url` targets the emulator and returns the bytes. |
| AS-EMULATOR-REDIRECT-TARGET | The default `rails_blob_path` route answers 302 to the configured `endpoint` origin, which is why a browser cannot follow it when that origin is a Compose service name. |
| AS-EMULATOR-PROXY | With `resolve_model_to_route = :rails_storage_proxy`, the proxy route returns the bytes from Rails with no redirect. |
| AS-EMULATOR-DIRECT-UPLOAD | An unchanged direct-upload URL and headers PUT, then attaching the blob by `signed_id`, preserve the bytes. |
| AS-EMULATOR-DIRECT-UPLOAD-CORS | A preflight from `http://localhost:3000` with Rails' direct-upload headers is allowed, an unlisted origin is not, and the `PUT` with `Origin` carries the CORS response header and stores the bytes. |
| AS-EMULATOR-V4-DIRECT-UPLOAD | With `cache_control` set, Rails signs the direct upload with V4 and adds `Cache-Control`; the browser-equivalent preflight is allowed by the `Cache-Control` response header in `buckets.json`, and the `PUT` stores the bytes and the cache control. |
| AS-EMULATOR-NO-OAUTH-LATER | 400 days after startup (`travel`), an attachment still uploads and downloads without any request leaving the emulator origin. |
| AS-EMULATOR-PURGE | `purge` removes the object from the emulator. |
| AS-EMULATOR-PRODUCTION-NO-KEY | A production boot selects its own service and evaluates the `gcs_emulator` entry without generating a key. |

`sdk-tests/ruby/active_storage/attachments.rb` runs these cases under the same
connection guard, with no internal authorization change; the routes are
exercised in process through `Rack::MockRequest`. Results on 2026-10-30 with
Rails 8.1.4, google-cloud-storage 1.62.1 and googleauth 1.17.6: **10 PASS, 0
FAIL**, and the production case **PASS**.

A separate Rails app with Puma 8.0.2 and googleauth 1.17.7, outside the
repository, was checked in Chrome on 2026-10-30. The ports were changed to
13000 and 14443 because 3000 and 4443 were in use; everything else followed
[Rails Active Storage](../rails-active-storage.md):

| Setup | Result in Chrome |
| --- | --- |
| Rails and the emulator on the host | Images load through the redirect and through proxy mode (no browser request to the emulator); a direct upload with Active Storage's JavaScript stores the same bytes, crossing origins under CORS |
| Rails in Compose, `endpoint` `http://gcs:4443/` | The redirect goes to `http://gcs:…`, so images do not load, and a direct upload fails with `Error storing "direct.png". Status: 0` |
| Rails in Compose, `gcs.localhost` alias and equal ports | Images load through the redirect to `http://gcs.localhost:…`, a direct upload stores the same bytes, and an upload through Rails works |

Negative checks confirmed that the cases detect each failure mode:

- Removing `universe_domain` failed every case at `oauth2.googleapis.com`.
- Removing only the service-level value failed every case with the
  universe-mismatch error above.
- Removing the CORS rule failed AS-EMULATOR-BUCKETS-FILE and
  AS-EMULATOR-DIRECT-UPLOAD-CORS.
- Generating the key unconditionally failed AS-EMULATOR-PRODUCTION-NO-KEY.
- Without `Cache-Control` in the CORS response headers, the V4 preflight was
  refused, as the Rails guide's rule alone would be.
- An application-built service with a one-day `expires_at`, the earlier
  migration recipe, failed the 400-day case when googleauth tried to refresh at
  `www.googleapis.com`. This recipe replaces it.

Other ways that were checked do not avoid OAuth with these versions:

- googleauth reads no `token_uri` from a key file; upstream
  [google-auth-library-ruby#251](https://github.com/googleapis/google-auth-library-ruby/pull/251)
  was closed without merging.
- google-cloud-storage 1.62.1 has no emulator option. Upstream
  [google-cloud-ruby#34023](https://github.com/googleapis/google-cloud-ruby/pull/34023)
  adds `STORAGE_EMULATOR_HOST` support with anonymous access. It was open and
  unmerged on 2026-10-30; after a release it may replace this recipe.
- Most Rails applications use the Disk service in development and test, as the
  [Rails guide](https://guides.rubyonrails.org/active_storage_overview.html)
  generates. That does not exercise GCS behavior such as signed URLs.

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

Original seven-case HTTP execution on 2026-10-27 (Ruby 4.0.9, Bundler 4.0.23,
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
were not run. No performance claim is made. The later Chrome and strict checks
are recorded under the [storage.yml emulator recipe](#storageyml-emulator-recipe).

## Failures and next slices

| Case | Symptom | Cause | Proposed next slice |
| --- | --- | --- | --- |
| AS-DEFAULT-AUTH | First unmodified Rails upload raises Google::Auth::AuthorizationError before HTTP reaches the emulator. | Service-account credentials in the default universe attempt OAuth at oauth2.googleapis.com; an SDK data endpoint alone does not configure token exchange. The guard blocks the attempt before network access. | Use the [storage.yml emulator recipe](#storageyml-emulator-recipe), which sets `universe_domain`. A default-universe entry stays unsupported. |
| AS-SIGNED-URL | PASS_WITH_WORKAROUND; unchanged blob.url GET returns exact bytes. | V2 RSA query support is implemented; default authentication still needs the separate fix. | V2 signing complete; `sdk-active-storage-local-auth` remains. |
| AS-DIRECT-UPLOAD | PASS_WITH_WORKAROUND; unchanged URL/headers PUT and exact readback succeed. | Default GCS direct uploads select V2 when cache_control is absent; that subset is supported. | V2 signing complete; `sdk-active-storage-local-auth` remains. |
| Strict profile | The storage.yml emulator entry gets `401 Invalid Credentials`; the log shows one `auth_self_signed_jwt_rejected` warning (`strict_jwt_bearer_fails_unchanged_and_warns_once_about_self_signed_tokens`). | Strict mode matches only identity-file tokens, while googleauth sends a self-signed JWT. Google accepts such JWTs from service accounts. | Deferred until a user needs strict mode with this recipe: verify RS256 self-signed JWTs against identity-file `public_keys`, with the issuer, audience or scope and expiry rules confirmed from official sources. |

AS-UPLOAD, AS-DOWNLOAD, AS-DELETE, AS-SIGNED-URL, AS-DIRECT-UPLOAD and AS-EXIST-COMPOSE are conditional successes
only after private-client authorization removal. They remain pending as normal
storage.yml workflows; the separate application credential recipe does not
resolve default-call authentication.

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
