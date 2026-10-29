# Official Ruby SDK HTTP and HTTPS test

The soft-delete journey uses ordinary soft_delete_policy=, files(soft_deleted:
true), file(name, generation:, soft_deleted: true), restore_file and download.
It passes in permissive and strict profiles; the generated client's form POST
body must match its URL query exactly. See the
[soft-delete contract](../../docs/en/slices/sdk-soft-delete-restore.md).

The retention journey uses ordinary `retention_period=` and
`lock_retention_policy!`, verifies retained-delete denial, removes the unlocked
policy for cleanup, then locks an empty emulator bucket. The generated client
repeats metageneration as form data on POST; the adapter accepts only a matching
value. Expiry uses injected-clock wire tests, without sleeping. See the
[retention contract](../../docs/en/slices/sdk-retention-policy.md).

<!-- markdownlint-configure-file {"MD013": false} -->

This scoped local HTTP/HTTPS journey pins Ruby 4.0.9, Bundler 4.0.23 and
`google-cloud-storage` 1.62.1, verified against official release metadata on
2026-10-27. The SDK uses Apache-2.0; Bundler uses MIT. `Gemfile.lock` pins the
complete dependency graph and RubyGems checksums. The SDK's generated storage
client and Faraday transport are locked transitively; no additional direct gem
is needed. The historical runner name remains `https.rb`. This covers the basic
API milestone and local transport, rather than full Ruby or Rails compatibility.

The 2026-10-27 refresh resolves Faraday 2.15.1, generated Storage client 0.70.0
and PStore 0.3.0 within the existing SDK requirements. No direct gem or harness
change was required. The [dependency refresh evidence](../../docs/en/evidence/dependency-refresh.md)
records release review and the full Go/Ruby check.

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
Every request has a ten-second timeout. Transfers disable automatic retries;
the unsupported-call section restores the SDK's default three retries and
requires 400 badRequest within three seconds, distinguishable from not
found. Test fixtures are removed on success and failure.

Bucket creation uses high-level `Project#create_bucket(name)` without optional
arguments. The disabled Autoclass setting sent by the SDK is treated as absent.
`create_bucket(name, autoclass_enabled: true)` reads back enabled Autoclass with
a toggle time and the default NEARLINE terminal class, and a COLDLINE upload to
that bucket is stored as STANDARD.
An extensionless `StringIO` upload uses `create_file` without `content_type` and
verifies application/octet-stream and exact bytes. The migration scenario also
omits `content_type` for its named text file.
`Bucket#update` blocks use PUT with only changed attributes; direct bucket
setters use PATCH. The PUT regression changes only PAP and verifies that
versioning and labels survive reload. Generated bucket/object PUT calls also
exercise `Google::Apis::StorageV1`. `StorageService#copy_object`
also exercises the actual copy API, since high-level `File#copy` delegates to
rewrite. `File#rewrite` and `Bucket#compose` exercise their named operations;
composition omits all optional arguments, so the SDK's `deleteSourceObjects: null`
and `destination: null` use the existing non-destructive/default-metadata behavior.
The [defaults contract](../../docs/en/slices/sdk-ruby-defaults.md) records these
local normalization choices and their provider-verification limits.
The remaining methods use the high-level SDK. Every generated and high-level
call shares the same guarded connection and optional static fixture token.

The repository-root `just check-sdk` now runs both pinned Go and Ruby suites
against isolated permissive/strict servers. Install the pinned Ruby/Bundler/gems
above first; the command does not substitute the host Ruby or install dependencies
silently. `GCS_MIMIC_SDK_SCHEME=https just check-sdk` uses the exported built-in CA
with verification enabled. A targeted `GCS_MIMIC_GO_TEST_RUN` retains Go-only
iteration; the unfiltered recipe executes Ruby as well.

The strict run adds official `File#signed_url(version: :v4, method: "GET"/"PUT",
issuer:, signing_key:)` checks with the clearly public fixture key and a temporary
registry granting its admin signer permissions. It verifies private exact bytes,
signed Content-Type, missing/changed headers, changed query/object/Host/method and
anonymous denial. No bearer header is attached to signed requests.

The same run verifies V2 GET/PUT with both omitted version (the SDK default) and
explicit `version: :v2`, using the normal file and endpoint without the V4
bucket-bound workaround. URLs remain unchanged; changed Content-Type, path and
method fail strict verification. See the [V2 contract](../../docs/en/slices/sdk-active-storage-v2-signing.md).

Storage 1.62.1's `file/signer_v4.rb` uses the service endpoint for the emitted URL,
but its default `host_name` is `storage.googleapis.com`. The test asserts that
this normal custom-endpoint URL retains the actual port yet fails strict
verification. The public `bucket_bound_hostname:` option instead binds the supplied
host:port and suppresses the normal bucket prefix. A lookup-free File with name
`"#{bucket.name}/signed.txt"` supplies the full path before signing, producing
`/bucket/signed.txt` on the final endpoint. The test asserts both authority and path,
then uses the resulting URL unchanged. This is an explicit SDK endpoint adaptation,
not virtual-host support. See SPEC-RUBY-SIGNER-ENDPOINT and the
[combined evidence](../../docs/en/evidence/signed-urls.md). Prefer `gcs-mimic url sign`
for the ordinary local path-style workflow.

Official sources:

- [Ruby stable downloads](https://www.ruby-lang.org/en/downloads/)
- [Bundler release metadata](https://rubygems.org/api/v1/gems/bundler.json)
- [Storage SDK release metadata](https://rubygems.org/api/v1/gems/google-cloud-storage.json)
- [Storage Ruby client API](https://cloud.google.com/ruby/docs/reference/google-cloud-storage/latest)
