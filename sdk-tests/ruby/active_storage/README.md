# Rails Active Storage GCS scenario

<!-- markdownlint-configure-file {"MD013": false} -->

This independent suite pins Ruby 4.0.9, Bundler 4.0.23, Active Storage/railties
8.1.4, sqlite3 2.10.2 and google-cloud-storage 1.62.1. The Rails meta-gem, image
processors and external database services are unnecessary. Install the pinned
Ruby/Bundler, then install this suite's separate graph:

```sh
cd sdk-tests/ruby/active_storage
BUNDLE_FROZEN=true bundle install
cd ../../..
just check-active-storage
```

Rust, Go (the existing loopback free-port helper), jq and just are required.
The runner checks installed gems without installing them, builds the emulator,
starts an isolated HTTP permissive server with disk-backed emulator storage,
waits for its bound address and readiness probe, then stops and removes only its
own fixtures. Rails uses in-memory SQLite and the shipped Active Storage migration.
It never runs as part of `just check-sdk`.

The ordinary [storage.yml](config/storage.yml) uses `service: GCS`, `project`,
`bucket`, local-only `credentials` and `endpoint: http://127.0.0.1:4443/` (provided
by the runner). This is the SDK endpoint option passed through Rails, not an
emulator-specific Rails setting. The committed private key is a public test
fixture; never use it for real credentials.

AS-DEFAULT-AUTH first exercises the configured service without internal changes.
It fails when googleauth attempts oauth2.googleapis.com; the guard rejects that
attempt before network access. Only afterward the harness clears the private
generated client's OAuth authorization to isolate operation coverage. Those
successes are labeled PASS_WITH_WORKAROUND, not default-call PASS. Every Net::HTTP
connection must match the exact loopback HTTP origin; proxies and redirect
following are disabled. No request to Google or the metadata server is allowed.
The self-check exercises invalid hosts, paths, credentials, ports and schemes:

```sh
cd sdk-tests/ruby/active_storage
bundle exec ruby scenario.rb --self-check
```

Each of AS-DEFAULT-AUTH, AS-UPLOAD, AS-DOWNLOAD, AS-DELETE, AS-SIGNED-URL,
AS-DIRECT-UPLOAD, AS-EXIST-COMPOSE and AS-LOCAL-CREDENTIALS records its result independently. Any failure makes the command
exit 1 after remaining cases and cleanup. Independent SDK-seeded fixtures let
read, delete and signing cases execute even when Active Storage upload fails.
Seed setup is not evidence that Active Storage upload works. Image purge tests
variant-prefix deletion with seeded bytes; no variant processor is installed.

Signed URLs are used unchanged. A Google-bound signed URL fails the origin guard
before network access; it is not rewritten or counted as a passing local signed
request. The verified default V2 URLs reach the local origin unchanged and now
pass signed GET and direct PUT, including exact readback.
AS-LOCAL-CREDENTIALS separately constructs a new Rails GCS service using public
credential inputs: service-account credentials with a preset dummy access token
and a one-day expiry. It verifies upload/download/delete, missing-object lookup
and an unchanged V2 signed GET without clearing authorization. The result is one
separate recipe PASS, six PASS_WITH_WORKAROUND and one FAIL for default
authentication; exit status remains 1. It does not make ordinary storage.yml
authentication pass. Token expiry/renewal and strict Rails integration remain
unverified. The emulator has no token-issuing route.
See [results and next slices](../../../docs/en/slices/sdk-active-storage-scenario.md)
for the exact supported scope and known failures (DESIGN-084).

`attachments.rb` then verifies the documented `gcs_emulator` entry in
[storage.yml](config/storage.yml), which sets a non-Google `universe_domain` so
that googleauth signs its own token instead of requesting one from Google. The
runner starts the emulator with [buckets.json](buckets.json), which creates
`local-bucket` with the Rails guide's direct-upload CORS rule plus
`Cache-Control`. The script selects the entry as an application would and runs
ten AS-EMULATOR-* cases through a `has_one_attached` model, under the same
connection guard and without internal changes: attachments, `url`, the default
redirect target, proxy mode, V2 and V4 (`cache_control`, from the
`gcs_emulator_cache_control` entry) direct uploads with browser-equivalent CORS
requests, an upload 400 days after startup and `purge`. `attachments.rb --production` then checks that a
production boot generates no emulator key. The runner fails if any script
fails. See the [Rails guide page](../../../docs/en/rails-active-storage.md).

Strict local JWT authentication has a separate scenario and explicit final-host
V4 signing adapter. After building the final emulator, run:

```sh
cargo build --locked
bash sdk-tests/ruby/active_storage/run-strict.sh
```

The runner registers only the public test key, grants its local identity the
local-project admin role, starts an isolated strict server and cleans up its own
data. `strict.rb` verifies stock Rails V2 and the explicit `EmulatorGcs` V4
service, private access, PAP and browser-equivalent CORS with signatures kept
intact. It does not remove OAuth authorization or rewrite signed URLs.
The [strict JWT contract](../../../docs/en/slices/strict-service-account-jwt.md)
lists accepted claims, scope restrictions, key rotation and split API/browser
origins. Actual browser and container execution remains a separate integration
check; inspect the final evidence before claiming those environments pass.
