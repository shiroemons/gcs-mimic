# Official Ruby SDK HTTPS test

<!-- markdownlint-configure-file {"MD013": false} -->

This narrow local HTTPS journey pins Ruby 4.0.9, Bundler 4.0.23 and
`google-cloud-storage` 1.62.1, verified against official release metadata on
2026-10-23. The SDK uses Apache-2.0; Bundler uses MIT. `Gemfile.lock` pins the
complete dependency graph and RubyGems checksums. The SDK's generated storage
client and Faraday transport are locked transitively; no additional direct gem
is needed. This is local TLS coverage, not full Ruby or Rails compatibility.

Install the pinned Ruby and Bundler using your preferred version manager, then:

```sh
cd sdk-tests/ruby
gem install bundler -v 4.0.23 --no-document
BUNDLE_FROZEN=true bundle install
bundle exec ruby https.rb --self-check
```

Start the emulator with its built-in local development certificate in another
terminal (`cargo run --locked -- serve` from the repository root), then export
that certificate's CA. No certificate generation or system trust-store change
is needed:

```sh
cd ../..
cert_dir=$(mktemp -d)
cargo run --locked -- tls ca > "$cert_dir/ca.pem"
cd sdk-tests/ruby
GCS_MIMIC_ENDPOINT=https://127.0.0.1:4443 \
  GCS_MIMIC_CA_CERT="$cert_dir/ca.pem" bundle exec ruby https.rb
rm -r "$cert_dir"
```

Use an isolated permissive emulator. A custom development certificate whose SAN
includes the endpoint hostname/IP is also supported: point `GCS_MIMIC_CA_CERT` at
its CA. The [local HTTPS guide](../../docs/en/getting-started.md)
documents listener startup and custom certificate inputs. The runner requires an
explicit loopback HTTPS origin and CA file. It uses the SDK's anonymous
client and a fixture project, so it does not discover credentials or contact the
metadata server. CA and hostname verification stay enabled. Proxies, redirects
and requests outside that exact emulator origin are disabled. `localhost` dialing
is pinned to `127.0.0.1`. Use the server's advertised API origin so resumable
session URLs retain the same origin. The self-check
verifies origin validation and the outbound request guard without a server.

The journey creates a random bucket, gets/lists/updates it, writes a bounded
disk fixture, streams a 576 KiB object through three 256 KiB resumable chunks,
gets/lists metadata, downloads to disk with the SDK's CRC32C verification and
compares SHA-256, verifies the advertised HTTPS session origin, then deletes the
object and bucket. Every request has a ten-second timeout and automatic retries
are disabled. Test fixtures are removed on success and failure.

Bucket creation uses the official generated `Google::Apis::StorageV1` client;
the high-level `Project#create_bucket` always sends an Autoclass setting, which
is outside the emulator's supported bucket subset. Other operations use the
high-level SDK. The generated client shares the same guarded TLS connection.

Official sources:

- [Ruby stable downloads](https://www.ruby-lang.org/en/downloads/)
- [Bundler release metadata](https://rubygems.org/api/v1/gems/bundler.json)
- [Storage SDK release metadata](https://rubygems.org/api/v1/gems/google-cloud-storage.json)
- [Storage Ruby client API](https://cloud.google.com/ruby/docs/reference/google-cloud-storage/latest)
