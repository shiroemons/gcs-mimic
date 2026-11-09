# Use gcs-mimic with Rails Active Storage

<!-- markdownlint-configure-file
{"MD013": {"code_blocks": false, "tables": false}}
-->

[日本語](../ja/rails-active-storage.md)

Use gcs-mimic in development and test, and Google Cloud Storage in production.
Production configuration does not change.

Tested with Rails 8.1.4.1, google-cloud-storage 1.62.1, and googleauth 1.17.6
([verification](slices/sdk-active-storage-scenario.md#storageyml-emulator-recipe));
Rails 8.1.4 was also tested with googleauth 1.17.7. Other versions are untested.

Rails 8.1.4.1 direct uploads need gcs-mimic 0.10.0 or later. Rails
8.1.4.1 signs `x-goog-content-length-range` into every direct-upload URL;
0.10.0 enforces that range, while 0.9.0 rejects the header with
`400 InvalidArgument`. See
[XML upload size ranges](slices/xml-content-length-range.md).

## 1. Start the emulator with a bucket

`buckets.json` creates the bucket at startup. Its CORS rule lets the browser
send direct uploads from your app's origin. It is the
[Rails guide](https://guides.rubyonrails.org/active_storage_overview.html)'s
Google Cloud Storage rule plus `Cache-Control`, which the browser sends when
the service sets `cache_control`; without it, those uploads fail CORS. Rails
8.1.4.1 also sends `x-goog-content-length-range`, so the rule must allow it.

```json
{"version": 1, "project": "local-project", "buckets": [
  {"name": "local-bucket", "cors": [{
    "origin": ["http://localhost:3000"], "method": ["PUT"],
    "responseHeader": ["Content-Type", "Content-MD5", "Content-Disposition", "x-goog-content-length-range", "Cache-Control"],
    "maxAgeSeconds": 3600}]}
]}
```

```yaml
# compose.yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.15.0
    ports:
      - "127.0.0.1:4443:4443"
    environment:
      GCS_MIMIC_SCHEME: http
      GCS_MIMIC_BUCKETS_FILE: /buckets.json
    volumes:
      - ./buckets.json:/buckets.json:ro
```

## 2. Add an emulator entry to `config/storage.yml`

Keep your production entry and add `gcs_emulator` next to it:

```yaml
google: # production, unchanged
  service: GCS
  project: my-project
  bucket: my-bucket
  credentials: <%= Rails.root.join("path/to/keyfile.json") %>

gcs_emulator: # development and test
  service: GCS
  project: local-project
  bucket: <%= ENV.fetch("GCS_EMULATOR_BUCKET", "local-bucket") %>
  endpoint: <%= ENV.fetch("GCS_EMULATOR_ENDPOINT", "http://localhost:4443/") %>
  universe_domain: gcs-emulator.invalid
  credentials:
    type: service_account
    project_id: local-project
    client_email: local@example.invalid
    universe_domain: gcs-emulator.invalid
    private_key: <%= OpenSSL::PKey::RSA.generate(2048).to_pem.dump if Rails.env.local? %>
```

- **`universe_domain` keeps Rails away from Google.** It is a Google Cloud
  setting for clouds other than the public one. With any value other than
  `googleapis.com`, googleauth signs its own token instead of requesting one
  from Google OAuth. Without it, every upload fails trying to reach
  `oauth2.googleapis.com`. Set the same value in both places.
- **The key is a throwaway.** It is generated at boot in development and test
  only, and signs local URLs. It is not a Google credential. Strict mode
  rejects it because the emulator does not know it; [step 6](#6-production-like-checks-in-strict-mode)
  uses the built-in test signer instead.
- Keep the trailing `/` in `endpoint`.
- **`public: true` needs a workaround.** Its URLs never reach the emulator;
  see [public files](#public-files).

### Public files

For a service with `public: true`, Rails links each file at its public URL,
and google-cloud-storage 1.62.1 always builds that URL on
`https://storage.googleapis.com`, whatever `endpoint` says. A local app's public
images therefore never load from the emulator. Either workaround below points
those URLs at the emulator and leaves production untouched. In strict mode the
bucket also needs an `allUsers` `iamPolicy`, as in the
[strict local recipe](../../examples/strict-local/README.md).

1. **Recommended: the `EmulatorGcs` adapter.** Copy the adapter as described in
   [step 6](#6-production-like-checks-in-strict-mode). The supplied file
   already contains this `public_url` method, which names `public_endpoint`, the
   browser origin, instead of Google:

   ```ruby
   # In lib/active_storage/service/emulator_gcs_service.rb, which also requires "erb".
   private

     # The SDK names storage.googleapis.com in every public URL whatever the endpoint is; a public
     # bucket serves the same path on the final origin.
     def public_url(key, **)
       path = key.split("/", -1).map { |segment| ERB::Util.url_encode(segment) }.join("/")
       "#{@public_endpoint.to_s.delete_suffix('/')}/#{bucket.name}/#{path}"
     end
   ```

   Select it with `public: true` only in the local `storage.yml` entry.
   Production keeps `service: GCS` and its own URLs:

   ```yaml
   gcs_emulator_public:
     service: EmulatorGcs
     public: true
     public_endpoint: <%= ENV.fetch("GCS_EMULATOR_PUBLIC_ENDPOINT", "http://localhost:4443") %>
     # project, bucket, endpoint, universe_domain and credentials as in step 6
   ```

   It affects only the entries that select it, so no production entry can
   change, and `public_endpoint` also covers a browser origin that differs from
   the address Rails uses.

2. **Alternative: an initializer.** To keep the stock `service: GCS` locally
   too, add this file as `config/initializers/active_storage_gcs_emulator.rb`.
   It applies only when `Rails.env.local?` (development and test), and only to a
   service whose `storage.yml` entry sets `endpoint`, such as the emulator
   entry. Production entries set none and keep the stock URLs:

   ```ruby
   # frozen_string_literal: true

   # Active Storage builds the URLs of a public: true GCS service with the SDK, which always names
   # storage.googleapis.com. In development and test, a service whose storage.yml entry sets an
   # endpoint, such as the gcs-mimic entry, names that endpoint instead. Entries without an endpoint,
   # such as production's, keep the stock URL.
   if Rails.env.local?
     require "active_storage/service/gcs_service"
     require "erb"

     module GcsEmulatorPublicUrl
       private

       def public_url(key, **)
         endpoint = config[:endpoint]
         return super if endpoint.blank?

         path = key.split("/", -1).map { |segment| ERB::Util.url_encode(segment) }.join("/")
         "#{endpoint.delete_suffix('/')}/#{bucket.name}/#{path}"
       end
     end

     ActiveStorage::Service::GCSService.prepend(GcsEmulatorPublicUrl)
   end
   ```

   It uses the entry's `endpoint` as the browser address, which fits setups
   where Rails and the browser use one URL, such as `localhost:4443` or
   `gcs.localhost` in Compose. When the two addresses differ, use the adapter.

Both rely on the private `public_url(key, **)` hook of Rails 8.1.4.1, and the
strict suite runs both: the stock case still names `storage.googleapis.com`,
and each workaround gives the exact emulator URL, which an anonymous request
reads from an `allUsers` bucket. `resolve_model_to_route =
:rails_storage_proxy` is not a fix for this: `blob.url` still names Google.

## 3. Select the entry per environment

```ruby
# config/environments/development.rb and config/environments/test.rb
config.active_storage.service = :gcs_emulator

# config/environments/production.rb
config.active_storage.service = :google
```

## 4. When Rails runs in a container

Rails reaches the emulator by its Compose service name, but the browser on your
machine cannot resolve that name. Images then fail to load, because Rails
redirects the browser to the emulator, and direct uploads fail with
`Error storing ... Status: 0`.

Use the name `gcs.localhost` on both sides. Browsers send `*.localhost` to your
own machine, and a Compose network alias sends it to the emulator inside
Docker, so Rails and the browser use the same URL. The port must match on both
sides too, so the emulator listens inside the container on the port it
publishes. `GCS_MIMIC_PORT` picks that port, so each worktree or project can use
its own:

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.15.0
    ports:
      # The host port equals the container port, so one URL works on both sides.
      - "127.0.0.1:${GCS_MIMIC_PORT:-4443}:${GCS_MIMIC_PORT:-4443}"
    environment:
      GCS_MIMIC_SCHEME: http
      GCS_MIMIC_LISTEN: 0.0.0.0:${GCS_MIMIC_PORT:-4443}
      GCS_MIMIC_BUCKETS_FILE: /buckets.json
    volumes:
      - ./buckets.json:/buckets.json:ro
    networks:
      default:
        aliases:
          - gcs.localhost
  web: # your Rails service
    environment:
      GCS_EMULATOR_ENDPOINT: http://gcs.localhost:${GCS_MIMIC_PORT:-4443}/
```

Set `GCS_MIMIC_PORT`, for example in a worktree's `.env` file, when port 4443
is taken; Compose substitutes it in the file. Other services on the Compose
network then reach the emulator at `http://gcs:${GCS_MIMIC_PORT:-4443}` too.
When the Rails port also changes per worktree, the CORS origin in the bucket
document must follow it. With 0.14.0 or later, write the document
inline in `GCS_MIMIC_BUCKETS` instead of `GCS_MIMIC_BUCKETS_FILE`, so that
Compose substitutes the port there as well, as in
`"origin": ["http://localhost:${RAILS_PORT:-3000}"]` with a variable of your
own. An inline document also
applies such an edit to the existing bucket at the next start.

Images, direct uploads and Rails' own uploads then all work. If the host port
cannot equal the container port, serve files through Rails instead; direct
uploads still need a browser-reachable `endpoint`:

```ruby
# config/environments/development.rb
config.active_storage.resolve_model_to_route = :rails_storage_proxy
```

## 5. CI

Start the emulator as a service, wait until it is ready, then create the
bucket. Right after startup the port can answer with an empty reply, which
`--retry-connrefused` does not retry, so wait with `--retry-all-errors`:

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.15.0
    env:
      GCS_MIMIC_SCHEME: http
    ports:
      - 4443:4443
steps:
  - run: >
      curl -sS --fail --retry 30 --retry-all-errors --retry-delay 1
      -o /dev/null http://localhost:4443/_internal/ready
  - run: >
      curl -sS --fail -H 'Content-Type: application/json'
      -d '{"name":"local-bucket"}'
      'http://localhost:4443/storage/v1/b?project=local-project'
```

These commands were checked against the image with the port published on the
host, as on a GitHub-hosted runner; the workflow itself was not run on GitHub.

## 6. Production-like checks in strict mode

The entry in step 2 signs with a key generated at boot, which only the default
permissive profile accepts. Strict mode authorizes every request as Cloud
Storage does: a browser that requests a private object without a signed URL
gets 401, and Rails needs a key that the emulator knows. Give Rails the key of
the built-in test signer `test-signer@gcs-mimic.invalid`, which ships with
gcs-mimic, so nothing has to be generated or registered. This needs 0.14.0 or
later.

Add two settings to the `gcs` service of step 1 or step 4:

```yaml
    environment:
      GCS_MIMIC_AUTH_MODE: strict
      GCS_MIMIC_TEST_SIGNER: enabled
```

Strict mode needs no identity document here. The signer gets
`roles/storage.objectUser` on the projects of the bucket document's buckets and
on `GCS_MIMIC_SEED_PROJECT` (`local-project` by default). That role covers every
Rails operation below, but it cannot create buckets, so the app's buckets come
from the bucket document of step 1. Startup warns that the signer's private key
is published. For other roles, declare `test-signer@gcs-mimic.invalid` with its
`project_roles` in `GCS_MIMIC_IDENTITIES` or `GCS_MIMIC_IDENTITY_FILE`, which
also enables the signer without `GCS_MIMIC_TEST_SIGNER`.

Save the signer's key into the app, from the image that the service uses:

```sh
docker compose run --rm gcs test-signer credentials | jq -r .private_key > config/gcs-mimic-test-signer.pem
```

Then change the credentials of the `gcs_emulator` entry from step 2:

```yaml
gcs_emulator: # development and test
  service: GCS
  project: local-project
  bucket: <%= ENV.fetch("GCS_EMULATOR_BUCKET", "local-bucket") %>
  endpoint: <%= ENV.fetch("GCS_EMULATOR_ENDPOINT", "http://localhost:4443/") %>
  universe_domain: gcs-emulator.invalid
  credentials:
    type: service_account
    project_id: local-project
    client_email: test-signer@gcs-mimic.invalid
    universe_domain: gcs-emulator.invalid
    private_key: <%= File.read(Rails.root.join("config/gcs-mimic-test-signer.pem")).dump if Rails.env.local? %>
```

The `credentials` hash holds the fields that `test-signer credentials` prints,
including the non-Google `universe_domain: gcs-emulator.invalid`, so googleauth
signs its own token with the published key instead of asking Google OAuth. The
key is public, and anyone can sign as this identity: use it only for local
development and tests, never on a shared server and never as a Google
credential. Permissive mode ignores the token, so the same entry works there
too.

**Which listener.** Point `endpoint` at the main listener, the one the browser
reaches, even when the emulator also has a
[trusted listener](container-usage.md#keep-private-buckets-private-with-a-trusted-listener)
for your other servers. Rails always sends its own token, so it authenticates
as the signer on either listener, but `endpoint` is also the origin of the URLs
that Rails hands the browser: the redirect of `url` and the direct upload URL.
The trusted port is never published, and it refuses browser requests. When
Rails runs in a container, use `gcs.localhost` as in step 4.

**V2 or V4 URLs.** The stock `service: GCS` signs V2 URLs for downloads and
direct uploads. A V2 signature does not cover the host, so these URLs verify at
whatever address `endpoint` names, including `gcs.localhost` with a
per-worktree port. For V4 URLs, select the `EmulatorGcs` adapter: copy
[emulator_gcs_service.rb](../../sdk-tests/ruby/active_storage/lib/active_storage/service/emulator_gcs_service.rb)
into the app's `lib/active_storage/service/` directory, make `lib` available on
the load path, and set `public_endpoint` to the bare origin that the browser
uses:

```yaml
gcs_emulator:
  service: EmulatorGcs
  public_endpoint: <%= ENV.fetch("GCS_EMULATOR_PUBLIC_ENDPOINT", "http://localhost:4443") %>
  # project, bucket, endpoint, universe_domain and credentials as above
```

The adapter signs the host and port of `public_endpoint` before it emits the
URL, so the URL verifies without `signed_host_port=lenient`; never rewrite the
host afterwards. For direct uploads it also signs `x-goog-content-length-range`
and, when the service sets `cache_control`, `Cache-Control`. With Rails in a
container, `endpoint` and `public_endpoint` both name
`gcs.localhost:${GCS_MIMIC_PORT:-4443}`.

The strict Rails suite (`sdk-tests/ruby/active_storage/run-strict.sh`) runs
the same credentials against a strict server on which the signer has only
`roles/storage.objectUser`. It covers JWT upload and download, V2 signed GET,
direct uploads through stock V2 and through the `EmulatorGcs` V4 adapter,
attach, `purge` including the variants prefix, `delete_prefixed`, a
`blob.update!` of `content_type`, `Blob.compose` and a public bucket. For a
whole application with Go servers, a browser and Rails, start from the
[strict local recipe](../../examples/strict-local/README.md).

## What is verified

The repository's suite checks these through a `has_one_attached` model, under
a guard that blocks any request other than to the emulator:

| Works | How it was checked |
| --- | --- |
| Upload, download and `purge` | Bytes compared; the object is in the emulator |
| `url` and the default redirect | The signed URL returns the bytes; the redirect targets `endpoint` |
| Proxy mode | The file is served by Rails without a redirect |
| Direct upload, V2 and V4 (`cache_control`) | Rails' URL and headers, with the browser's CORS preflight and `PUT`; an unlisted origin is refused |
| No Google OAuth, ever | An upload 400 days after startup still stays local |
| Production is untouched | A production boot selects its own entry and generates no key |

A Rails app with Puma was also checked in Chrome, with Rails on the host and
with Rails in Compose using `gcs.localhost` (on other free ports, otherwise as
written here): images, direct uploads with Active Storage's JavaScript, and
uploads through Rails all worked. Other browsers were not tried.

The recipe above uses the default permissive profile. Since 0.8.0,
[strict local JWT authentication](slices/strict-service-account-jwt.md)
verifies a registered test service-account public key and applies existing IAM
permissions; 0.7.0 and earlier reject the token. Keep a stable local signing
key, register its public key and identity, and use the same non-Google universe
in both settings. A fresh unregistered key
generated at each boot cannot authenticate to that strict registry. Instead of
registering a key, you can use the
[built-in test signer](signed-urls.md#built-in-test-signer): start the emulator
with `GCS_MIMIC_TEST_SIGNER=enabled`, and the signer reads and writes objects in
the projects of the buckets document's buckets and `seed_project` without a
grant, or declare `test-signer@gcs-mimic.invalid` with project roles in the
identity document,
which also enables it (strict mode leaves it disabled otherwise and warns at
startup when it is enabled). A bucket in another project needs bucket IAM for
it. Set
`client_email` to `test-signer@gcs-mimic.invalid` and `private_key` to the
published key. Save the key from the emulator itself, which needs no
repository checkout:

```sh
docker run --rm ghcr.io/shiroemons/gcs-mimic:IMAGE_TAG test-signer credentials \
  | jq -r .private_key > config/gcs-mimic-test-signer.pem
```

Use 0.14.0 or later, because 0.13.0 lacks the command, and read the file as in
the [signer's `storage.yml` example](signed-urls.md#built-in-test-signer). Its
private key is public, so anyone can authenticate as it; use it only for local
tests. The same
credentials let permissive mode check Rails' signed URLs with
`GCS_MIMIC_SIGNED_URLS=verify`; a key generated at boot is unknown to the
emulator there, so its URLs fail with `InvalidAccessKeyId`.

Stock GCS service URLs use the scoped V2 flow. For final-origin V4 URLs, including
`cache_control` direct uploads and distinct Compose/browser origins, explicitly
select the supplied `EmulatorGcs` service adapter. It signs the final browser
host/port/path before emitting the URL; never rewrite the authority afterward.
`just check-active-storage-strict` runs two variants, and each passes its seven
cases: one registers a key, the other uses the built-in test signer with only
`roles/storage.objectUser` (see [step 6](#6-production-like-checks-in-strict-mode)),
with private unsigned reads and changed signed headers still rejected. This is
the Rails model/Net::HTTP fixture; an actual strict Rails browser UI remains
unverified. This local
identity simulation does not issue or validate Google OAuth credentials.

Limits of the recipe:

- The entry as written works only with the default permissive profile. Its key
  is generated at each boot and never registered, so with
  `GCS_MIMIC_AUTH_MODE=strict` uploads fail with `401 Invalid Credentials` and
  the emulator log shows one `auth_self_signed_jwt_rejected` warning. Strict
  mode accepts only the documented registered JWT subset described above, which
  includes the built-in test signer only when the identity document declares it
  or `GCS_MIMIC_TEST_SIGNER=enabled`. A JWT signed with the
  `test-signer credentials` key while the signer is disabled also gets the 401,
  and the warning then says that the test signer is disabled and that
  `GCS_MIMIC_TEST_SIGNER=enabled` turns it on.
- The recipe relies on googleauth signing its own token outside
  `googleapis.com`. Checked with googleauth 1.17.6 and 1.17.7; pin your
  versions and recheck after upgrades.
  [google-cloud-ruby#34023](https://github.com/googleapis/google-cloud-ruby/pull/34023)
  (unmerged on 2026-10-30) would add a simpler emulator option.

Most Rails apps use the Disk service in development and test instead. That is
simpler but never exercises Google Cloud Storage behavior such as signed URLs
and CORS.
