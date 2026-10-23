# Official Ruby SDK HTTP and HTTPS test

<!-- markdownlint-configure-file {"MD013": false} -->

This scoped local HTTP/HTTPS journey pins Ruby 4.0.9, Bundler 4.0.23 and
`google-cloud-storage` 1.62.1, verified against official release metadata on
2026-10-23. The SDK uses Apache-2.0; Bundler uses MIT. `Gemfile.lock` pins the
complete dependency graph and RubyGems checksums. The SDK's generated storage
client and Faraday transport are locked transitively; no additional direct gem
is needed. The historical runner name remains `https.rb`. This covers the basic
API milestone and local transport, rather than full Ruby or Rails compatibility.

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

For HTTP, start a separate isolated emulator from the repository root:

```sh
cargo run --locked -- serve --scheme http --api-base-url http://127.0.0.1:4443
```

Then run the same suite without a CA input:

```sh
cd sdk-tests/ruby
GCS_MIMIC_ENDPOINT=http://127.0.0.1:4443 bundle exec ruby https.rb
```

Both journeys default to an isolated permissive emulator. To exercise the
existing strict fixture, add `--auth-mode strict --identity-file config/identities.json`
to the server command and set `GCS_MIMIC_TOKEN=local-admin-token` for the runner.
The suite uses the fixture project `local-project`. It constructs an anonymous
client, then attaches this optional static token without credential discovery.

A custom development certificate whose SAN
includes the endpoint hostname/IP is also supported: point `GCS_MIMIC_CA_CERT` at
its CA. The [local HTTPS guide](../../docs/en/getting-started.md)
documents listener startup and custom certificate inputs. The runner requires an
explicit loopback HTTP or HTTPS origin; HTTPS also requires a readable CA file.
It does not discover credentials or contact the metadata server. CA and hostname
verification stay enabled for HTTPS. Proxies, redirects
and requests outside that exact emulator origin are disabled. `localhost` dialing
is pinned to `127.0.0.1`. Use the server's advertised API origin so resumable
session URLs retain the same origin. The self-check
verifies HTTP/HTTPS and IPv6 loopback acceptance plus external-host, metadata
server, scheme-change and port-change rejection without a server.

The journey executes six bucket methods (`insert`, `get`, `list`, `patch`,
`update`, `delete`) and nine object methods (`insert`, `get`, `list`, `patch`,
`update`, `delete`, `copy`, `rewrite`, `compose`). It verifies bucket PATCH and PUT
replacement, object metadata PATCH and PUT, and the bytes of all derived objects.
It writes a bounded disk fixture, streams a 576 KiB object through three 256 KiB
resumable chunks, downloads each result to disk with the SDK's CRC32C verification
and compares SHA-256. Composition concatenates two sources and verifies the
result using bounded 64 KiB reads. The advertised session URL must retain the
exact configured origin. Objects and bucket are deleted afterward.
Every request has a ten-second timeout and automatic retries
are disabled. Test fixtures are removed on success and failure.

Bucket creation and bucket/object PUT use the official generated
`Google::Apis::StorageV1` client;
the high-level `Project#create_bucket` always sends an Autoclass setting, which
is outside the emulator's supported bucket subset. `StorageService#copy_object`
also exercises the actual copy API, since high-level `File#copy` delegates to
rewrite. `File#rewrite` and `Bucket#compose` exercise their named operations;
composition explicitly sets `delete_source_objects: false`, since the pinned SDK
otherwise sends `deleteSourceObjects: null`, rejected by the documented local
compose validation contract. A destination block also supplies `content_type` so
the SDK sends an object rather than unsupported `destination: null`.
The remaining methods use the high-level SDK. Every generated and high-level
call shares the same guarded connection and optional static fixture token.

Official sources:

- [Ruby stable downloads](https://www.ruby-lang.org/en/downloads/)
- [Bundler release metadata](https://rubygems.org/api/v1/gems/bundler.json)
- [Storage SDK release metadata](https://rubygems.org/api/v1/gems/google-cloud-storage.json)
- [Storage Ruby client API](https://cloud.google.com/ruby/docs/reference/google-cloud-storage/latest)
