# Use gcs-mimic with Rails Active Storage

<!-- markdownlint-configure-file
{"MD013": {"code_blocks": false, "tables": false}}
-->

[日本語](../ja/rails-active-storage.md)

Use gcs-mimic in development and test, and Google Cloud Storage in production.
Production configuration does not change.

Tested with Rails 8.1.4, google-cloud-storage 1.62.1, and googleauth 1.17.6
and 1.17.7 ([verification](slices/sdk-active-storage-scenario.md#storageyml-emulator-recipe)).
Other versions are untested.

## 1. Start the emulator with a bucket

`buckets.json` creates the bucket at startup. Its CORS rule lets the browser
send direct uploads from your app's origin. It is the
[Rails guide](https://guides.rubyonrails.org/active_storage_overview.html)'s
Google Cloud Storage rule plus `Cache-Control`, which the browser sends when
the service sets `cache_control`; without it, those uploads fail CORS.

```json
{"version": 1, "project": "local-project", "buckets": [
  {"name": "local-bucket", "cors": [{
    "origin": ["http://localhost:3000"], "method": ["PUT"],
    "responseHeader": ["Content-Type", "Content-MD5", "Content-Disposition", "Cache-Control"],
    "maxAgeSeconds": 3600}]}
]}
```

```yaml
# compose.yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.7.0
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
  only, and signs local URLs. It is not a Google credential.
- Keep the trailing `/` in `endpoint`.

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
Docker, so Rails and the browser use the same URL:

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.7.0
    ports:
      - "127.0.0.1:4443:4443" # the host port must equal the container port
    environment:
      GCS_MIMIC_SCHEME: http
      GCS_MIMIC_API_BASE_URL: http://gcs.localhost:4443
      GCS_MIMIC_BUCKETS_FILE: /buckets.json
    volumes:
      - ./buckets.json:/buckets.json:ro
    networks:
      default:
        aliases:
          - gcs.localhost
  web: # your Rails service
    environment:
      GCS_EMULATOR_ENDPOINT: http://gcs.localhost:4443/
```

Images, direct uploads and Rails' own uploads then all work. If you cannot
publish the same port, serve files through Rails instead; direct uploads still
need a browser-reachable `endpoint`:

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
    image: ghcr.io/shiroemons/gcs-mimic:0.7.0
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

The recipe above describes the published permissive profile. For the
**unreleased source checkout**, [strict local JWT authentication](slices/strict-service-account-jwt.md)
verifies a registered test service-account public key and applies existing IAM
permissions. Keep a stable local signing key, register its public key and identity,
and use the same non-Google universe in both settings. A fresh unregistered key
generated at each boot cannot authenticate to that strict registry.

Stock GCS service URLs use the scoped V2 flow. For final-origin V4 URLs, including
`cache_control` direct uploads and distinct Compose/browser origins, explicitly
select the supplied `EmulatorGcs` service adapter. It signs the final browser
host/port/path before emitting the URL; never rewrite the authority afterward.
`just check-active-storage-strict` passes three integrated native scenarios:
JWT upload/download and V2/V4 direct-upload CORS, with private unsigned reads
and changed signed headers still rejected. This is the Rails model/Net::HTTP
fixture; an actual strict Rails browser UI remains unverified. This local
identity simulation does not issue or validate Google OAuth credentials.

Limits of the published recipe:

- The entry works only with the default permissive profile. With
  `GCS_MIMIC_AUTH_MODE=strict`, uploads fail with `401 Invalid Credentials`,
  because the published version does not verify the self-signed JWT. The
  emulator log then shows one `auth_self_signed_jwt_rejected` warning. Source
  builds accept only the explicitly documented registered JWT subset.
- The recipe relies on googleauth signing its own token outside
  `googleapis.com`. Checked with googleauth 1.17.6 and 1.17.7; pin your
  versions and recheck after upgrades.
  [google-cloud-ruby#34023](https://github.com/googleapis/google-cloud-ruby/pull/34023)
  (unmerged on 2026-10-30) would add a simpler emulator option.

Most Rails apps use the Disk service in development and test instead. That is
simpler but never exercises Google Cloud Storage behavior such as signed URLs
and CORS.
