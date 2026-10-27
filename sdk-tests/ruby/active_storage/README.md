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
AS-DIRECT-UPLOAD and AS-EXIST-COMPOSE records its result independently. Any failure makes the command
exit 1 after remaining cases and cleanup. Independent SDK-seeded fixtures let
read, delete and signing cases execute even when Active Storage upload fails.
Seed setup is not evidence that Active Storage upload works. Image purge tests
variant-prefix deletion with seeded bytes; no variant processor is installed.

Signed URLs are used unchanged. A Google-bound signed URL fails the origin guard
before network access; it is not rewritten or counted as a passing local signed
request. The verified default V2 URLs reach the local origin, then fail with XML
400 for unsupported `Expires`: the final result is zero default PASS, four
PASS_WITH_WORKAROUND and three FAIL, including default authentication.
See [results and next slices](../../../docs/en/slices/sdk-active-storage-scenario.md)
for the exact supported scope and known failures (DESIGN-084).
