# Getting started: proposed end-to-end contract

<!-- markdownlint-configure-file {"MD013": false} -->

**Status: future end-to-end design specification.** The current JSON, strict IAM, XML GET/HEAD/PUT and V4 RSA query signing implementation has a separate [runnable guide](getting-started.md). The registry is JSON (`config/identities.json`, mounted by `compose.strict.yaml`), with bounded inline RSA public keys; the server never needs a signer private key. The [signed access journey](getting-started.md#signed-private-downloads-and-uploads) covers native/source-built Docker signing, private GET/PUT, method/object/host/expiry/header failures, conditions, restart and peer endpoints. Its remaining PAP case keeps UX-006 PARTIAL. TOML, broader fixtures/SDKs, proxy-derived origins and other signing formats below remain future acceptance targets, not current commands/settings. Fixed `api_base_url` is implemented. No published image is claimed. The project/CLI/image name is `gcs-mimic`.

This English document is canonical. Any Japanese companion is optional supplementary planning material outside the project's canonical documentation tree. Implementation prompts are Japanese; source code, identifiers, commits and project documentation remain English.

The current object walkthrough selects `name,size` from metadata and
`items(name),nextPageToken` from listing, captures the HTTP ETag header, requires
a bodyless 304 for matching If-None-Match and an unfiltered 412 error for stale
If-Match. Run those blocks natively and with Compose, then repeat the conditional
read after restart. Use the header rather than assuming equality with the legacy
bucket JSON etag. Policy GET and combined query conditions also support ETag
validators. Unsupported selector edges and write/list/date/If-Range validators
remain explicit in the slice contracts.

The current bucket IAM walkthrough must run continuously after strict object
setup, using the same endpoint, temporary directory and fixture tokens. It must:

- Read the default policy as admin, preserve its bindings and use its etag when
  granting `roles/storage.objectViewer` to
  `user:local-bucket-reader@example.invalid`, whose registry entry has no project
  roles.
- Verify exact object bytes and listing in the granted bucket, and 403 for another
  bucket, object creation and policy get/set by the reader.
- Show 412 `conditionNotMet` for a stale etag without changing the grant, then
  fetch the latest policy and revoke access; media and listing must return 403.
- Explain convenience members, effective project-plus-bucket grants, whole-policy
  replacement, persistence and overwrite without an etag.
- Exercise `BucketHandle.IAM().Policy` / `SetPolicy` with pinned Go SDK tests,
  including project-viewer denial and a permissive-mode round trip.

The bucket-only scope excludes `testIamPermissions`, object IAM, ACLs,
conditions, custom/basic roles and signed URLs. The public/private read journey
must continue with the same strict bucket and object, and:

- Construct host and peer URLs without granting access; anonymous reads fail
  with 401 before the policy changes.
- Preserve bindings and use the policy etag to grant `allUsers` the
  `roles/storage.objectViewer` role; host and peer GET return exact bytes and
  HEAD returns 200, the selected generation and `public, max-age=3600`.
- Stop/start with strict configuration and verify the persisted policy and
  anonymous read before revocation.
- Replace the public member with `allAuthenticatedUsers`; the registered
  bucket reader succeeds, anonymous and unregistered-token reads fail.
- Revoke the grant with a fresh etag, verify anonymous host/peer denial and
  registered-reader denial, and retain cleanup for subsequent guide sections.
- Explain published host ports, Compose service names, browser reachability,
  cache persistence after revocation and the deferred CORS/signing/UBLA/PAP scope.
- Cover pinned Go default/XML reads as well as explicit JSON reads.

The current bucket journey must create a bucket through HTTP with
`versioning.enabled=true` and `defaultEventBasedHold=true`, verify inherited hold
and a rejected delete, release the hold, overwrite, list both retained generations,
read the earlier bytes, then release/delete all generations and the empty bucket.
Run the same setting/effect contract in native and `legacy-emulator` compatibility
profiles through HTTP tests. The runnable guide's native journey is sufficient
for onboarding; the compatibility corpus additionally covers source-only names.
The implemented compatibility option is `--compat-profile`, environment
`GCS_MIMIC_COMPAT_PROFILE` or JSON `compat_profile`, default `native`, with
CLI > environment > file > default precedence. It never changes strict permissions.

The broader public and signing sequences in section 8 remain future acceptance
targets beyond the [implemented read subset](slices/public-object-reads.md).

The implemented bucket PATCH journey must merge a new label while preserving
an existing key, remove the new key with a null value, and change the supported
default `storageClass` to NEARLINE. Use curl with `ifMetagenerationMatch`, verify
one revision increment, then retry the stale condition and require 412 with no
mutation; compare the returned resource with GET. Run the updated steps against
the built native binary and Docker image. `labels:{}` preserves keys and
`labels:null` clears the map under the declared local contract; whole-map null,
scalar deletion/default and other provider edge parity remain explicit gaps.
The [runnable commands](getting-started.md#patch-bucket-labels-and-default-storage-class)
also describe boolean versioning/default-hold changes, strict update permission
and rejected advanced fields. The pinned Go `BucketHandle.Update` tests must
exercise `SetLabel`, `DeleteLabel`, class/settings changes, conditions and strict
denial without cloud credentials.

The implemented bucket PUT journey must replace the entire labels map and
default class, enable versioning/default holds, then send an empty resource and
verify omitted fields reset to the declared local defaults. Retry a stale
metageneration and require 412 without mutation; compare the response with GET
and the Docker resource before and after restart. Execute the shared
[create/PATCH/PUT blocks](getting-started.md#replace-bucket-labels-and-default-storage-class)
against the native binary and Docker image. Use the pinned generated Go API
client's `Buckets.Update` because high-level `BucketHandle.Update` sends PATCH.
Provider omission/null defaults remain open under SPEC-BUCKET-PUT.
The [final PUT evidence](evidence/bucket-metadata-put.md) records the completed
native/Docker blocks, restart equality and full pinned Go SDK verification.

The [README](../../README.md) and [runnable guide](getting-started.md) cover Docker/native builds, probes, configuration, persistent bucket/object media CRUD, multipart and bounded JSON resumable uploads, versioning/default holds, prefix/delimiter/paged listing, conditional metadata patch/replacement, single byte ranges, initial seed directories, Go SDK tests, stopping and isolated smoke checks. Ruby/Rails remain pending. Use those guides for working commands; broader sequences below remain future acceptance targets.

The delivered [local TLS journey](getting-started.md#local-https-with-an-explicit-development-ca)
uses HTTPS by default with a built-in development certificate/key and exported
client CA trust. A supplied PEM chain/key pair overrides the built-in identity.
Native, non-root container, pinned Go/Ruby and isolated browser checks cover
TLS 1.2/1.3, SAN/CA/expiry failures, bounded handshakes and HTTPS Locations.
HTTP examples in the runnable guide select `--scheme http` explicitly.
Automatic certificate issuance, dual listeners, system trust changes, HTTP/2,
HTTP/3 and gRPC remain outside that scope.

## 1. Prerequisites and source build

The [object rewrite journey](getting-started.md#rewrite-an-object-and-follow-continuation-tokens)
must execute a generation-selected one-call rewrite and a 3 MiB rewrite that
changes storage class with `maxBytesRewrittenPerCall=1048576`. Follow returned
tokens until done, verify string byte counts/resource presence, metadata and
download equality, then delete only the example objects. Execute these blocks
against native and Docker endpoints after source finalization. Rust/Go tests
separately cover binding, grants, conditions, replay, process-kill recovery,
corruption rejection, expiry and admission. The
[contract](slices/object-rewrite.md) names local rules and unsupported semantics.

Support Linux amd64, Linux arm64 and Docker Desktop on Apple Silicon macOS. The container workflow requires Git, Docker Engine/Desktop, Docker Compose v2 supporting both `up --wait` and `start --wait`, and curl. A native Rust toolchain is optional for Docker users; document the exact latest-stable Rust baseline separately for contributors according to [the dependency policy](dependency-policy.md). OpenSSL is required only for the strict signing walkthrough.

The first supported installation route is a source build:

```bash
git clone REPOSITORY_URL gcs-mimic
cd gcs-mimic
docker compose build
```

The repository supplies `compose.yaml`, `config/development.toml`, fixture samples and the Dockerfile. Image references in documentation initially use the locally built `gcs-mimic:dev`. Add registry pull commands only after an actual release exists, and test their exact tag.

## 2. Stable proposed CLI and configuration contract

These are emulator extensions, not Google API methods:

| Command | Required behavior |
| --- | --- |
| `gcs-mimic serve --config PATH` | Validate configuration, recover persistence, seed if appropriate, start listeners. |
| `gcs-mimic healthcheck --url URL` | Exit zero only when the health/readiness request succeeds. |
| `gcs-mimic url public --base-url URL --bucket B --object O` | Print an encoded object address without changing permissions. |
| `gcs-mimic url sign --base-url URL --bucket B --object O --method GET --expires 15m --signer ID --private-key PATH` | Print a locally generated V4 URL for the supplied final endpoint. Never call real Google IAM. |
| `gcs-mimic admin reset --endpoint URL --confirm gcs-mimic` | Reset only this local emulator instance when admin operations are explicitly enabled. Keep this command out of ordinary startup. |

Configuration precedence is CLI > `GCS_MIMIC_`-prefixed environment > config file > documented defaults. Invalid settings fail before a ready signal.

```toml
[server]
listen = "0.0.0.0:4443"
scheme = "http"
allowed_authorities = ["127.0.0.1:4443", "localhost:4443", "gcs:4443"]

[urls]
api_advertise_mode = "validated_request_origin"
public_base_url = "http://localhost:4443"
trust_forwarded_headers = false

[storage]
backend = "filesystem"
data_dir = "/var/lib/gcs-mimic"
seed_dir = "/fixtures"
seed_policy = "if_empty"
seed_project = "local-project"

[auth]
mode = "permissive"

[limits]
max_in_flight_requests = 32
stream_buffer_bytes = 262144
max_queued_work = 64
```

Values above are proposed defaults to benchmark, not measured optimal settings.

For generated API links such as resumable Location, `validated_request_origin` uses only the listener scheme and a request authority present in the explicit allowlist. It never accepts arbitrary forwarded headers. This allows host clients to receive host-reachable links and peer containers to receive container-reachable links. Offer an explicit fixed `api_base_url` override for reverse proxies or deployments needing one advertised origin; document the tradeoff. Public/signed links use the explicit public base URL.

## 3. Fixtures, persistence and Docker Compose

Use separate paths for read-only fixture input and persistent storage. Normal restarts must not overwrite mutations by replaying fixtures.

```bash
mkdir -p fixtures/local-bucket
printf 'Hello from the emulator.\n' > fixtures/local-bucket/hello.txt
```

Expected checked-in compose contract:

```yaml
services:
  gcs:
    build: .
    image: gcs-mimic:dev
    command: ["serve", "--config", "/etc/gcs-mimic/config.toml"]
    ports:
      - "127.0.0.1:4443:4443"
    read_only: true
    tmpfs:
      - /tmp:size=32m
    volumes:
      - ./config/development.toml:/etc/gcs-mimic/config.toml:ro
      - ./fixtures:/fixtures:ro
      - gcs-data:/var/lib/gcs-mimic
    healthcheck:
      test: ["CMD", "gcs-mimic", "healthcheck", "--url", "http://127.0.0.1:4443/_internal/health"]
      interval: 2s
      timeout: 2s
      start_period: 5s
      retries: 15
volumes:
  gcs-data:
```

The image runs as a non-root user and prepares ownership so the named data volume works without a manual chmod. Upload staging belongs on the data volume, not the bounded /tmp mount. An optional resource-limited compose override is validated separately.

```bash
docker compose up -d --wait
curl --fail http://127.0.0.1:4443/_internal/health
```

Ready means persistence recovery and initial seeding have finished. A running process alone is insufficient.

## 4. First bucket/object workflow

The runnable [compose journey](getting-started.md#compose-existing-objects) must
upload two sources, compose their ordered bytes with destination
ifGenerationMatch=0, verify componentCount/CRC32C and absent MD5, reject a stale
destination condition without mutation, then compose with deleteSourceObjects
and verify hard-deleted source generations. Run the block against the final
native binary and Docker image. The pinned Go ComposerFrom tests additionally
cover selected source generations, source conditions, destination metadata,
strict get/create permissions and a self-source append with atomic source
deletion. Restart and failed-mutation persistence belong in the wire tests.
The [slice contract](slices/object-compose.md) records advanced rejected fields
and local deletion safety rules; gRPC and XML compose are separate features.

These requests use the deliberately permissive local profile.

```bash
curl --fail "http://127.0.0.1:4443/storage/v1/b?project=local-project"
curl --fail "http://127.0.0.1:4443/storage/v1/b/local-bucket/o"
curl --fail "http://127.0.0.1:4443/storage/v1/b/local-bucket/o/hello.txt?alt=media"

curl --fail -X POST \
  -H "Content-Type: application/json" \
  --data '{"name":"scratch-bucket"}' \
  "http://127.0.0.1:4443/storage/v1/b?project=local-project"

printf 'Round-trip data.\n' > local-upload.txt
curl --fail -X POST -H "Content-Type: text/plain" \
  --data-binary @local-upload.txt \
  "http://127.0.0.1:4443/upload/storage/v1/b/scratch-bucket/o?uploadType=media&name=notes%2Fhello.txt"

curl --fail "http://127.0.0.1:4443/storage/v1/b/scratch-bucket/o/notes%2Fhello.txt"
curl --fail "http://127.0.0.1:4443/storage/v1/b/scratch-bucket/o/notes%2Fhello.txt?alt=media"
curl --fail -X POST -H "Content-Type: application/json" --data 'null' \
  "http://127.0.0.1:4443/storage/v1/b/scratch-bucket/o/notes%2Fhello.txt/copyTo/b/scratch-bucket/o/notes%2Fcopy.txt?ifGenerationMatch=0"
curl --fail "http://127.0.0.1:4443/storage/v1/b/scratch-bucket/o/notes%2Fcopy.txt?alt=media"
curl --fail -X DELETE "http://127.0.0.1:4443/storage/v1/b/scratch-bucket/o/notes%2Fcopy.txt"
curl --fail -X DELETE "http://127.0.0.1:4443/storage/v1/b/scratch-bucket/o/notes%2Fhello.txt"
curl --fail -X DELETE "http://127.0.0.1:4443/storage/v1/b/scratch-bucket"
```

Expected verification includes exact content, metadata fields, correct URL encoding and the correct missing-object error after deletion. Do not present these commands as passing until the corresponding implementation milestones exist.

The copy example uses the [declared local contract](slices/object-copy.md): pin an explicit source generation when exercising races, verify create-if-absent 412 and destination preservation, compare bytes/checksums and exercise restart. Null-body inheritance supports generated SDK clients; explicit-resource replacement/default edges remain source questions. The [shared final evidence](evidence/derived-objects.md) records native/Docker execution of the affected copy, one/multi-call rewrite and compose blocks, plus integrated restart equality. High-level Go `CopierFrom` passes actual rewrite workflows, while generated `Objects.Copy` verifies copy directly.

## 5. Host versus container connectivity

| Caller | API endpoint | Public/signed link consumed by host browser |
| --- | --- | --- |
| Host CLI or IDE application | `http://127.0.0.1:4443` | `http://localhost:4443/BUCKET/OBJECT` |
| Application in the same compose network | `http://gcs:4443` | Still generate `http://localhost:4443/...` if the browser is on the host |
| Host browser | `http://localhost:4443` | Same browser-reachable host |
| Peer container consuming a signed URL itself | `http://gcs:4443` | Sign for `http://gcs:4443` from the beginning |

`0.0.0.0` is a listener address, not an advertised URL. Inside an application container, localhost refers to that container. Do not replace a signed URL's host after signing. Ensure the allowlist covers exactly the documented development authorities.

## 6. Official Go SDK example

Pin and test a specific SDK version. Go documents STORAGE_EMULATOR_HOST and redirects NewClient calls to the emulator. The example below uses its documented host:port form.

```bash
export STORAGE_EMULATOR_HOST=localhost:4443
```

```go
ctx := context.Background()
client, err := storage.NewClient(ctx, storage.WithJSONReads())
if err != nil {
    return err
}
defer client.Close()

reader, err := client.Bucket("local-bucket").Object("hello.txt").NewReader(ctx)
if err != nil {
    return err
}
defer reader.Close()
_, err = io.Copy(os.Stdout, reader)
return err
```

For a compose application set the variable to `gcs:4443`. The repository must provide the complete compiling program and its go.mod. This P1 example explicitly selects JSON reads because the currently checked Go SDK defaults to XML; add default/XML read coverage in P2. Test the actually supported single-request/JSON multipart upload subset and listing first, then add resumable tests in P3. Do not claim large-upload SDK compatibility before that path exists. Record any deliberate single-request writer configuration in the example. Do not assume STORAGE_EMULATOR_HOST also determines the host used by signing helpers.

Official reference: [Go Storage SDK](https://docs.cloud.google.com/go/docs/reference/cloud.google.com/go/storage/latest).

## 7. Official Ruby SDK and Rails flow

Ruby exposes explicit endpoint configuration. The initial local permissive example uses an anonymous client to avoid fetching real OAuth tokens.

```ruby
require "google/cloud/storage"
require "stringio"

storage = Google::Cloud::Storage.anonymous(
  endpoint: ENV.fetch("GCS_ENDPOINT", "http://127.0.0.1:4443")
)
bucket = storage.bucket("local-bucket", skip_lookup: true)
bucket.create_file(StringIO.new("Ruby round trip\n"), "ruby.txt")
file = bucket.file("ruby.txt")
downloaded = file.download
downloaded.rewind
puts downloaded.read
```

Set GCS_ENDPOINT to `http://gcs:4443` for a compose application. Anonymous writes are intended here only because the local profile is permissive; they are not a production recommendation.

Ship a pinned Gemfile and execute the sample against the emulator. Add a dedicated Rails Active Storage example with a real minimal app, verified endpoint/signing configuration, direct browser upload, private download, public serving and delete. Do not invent Rails service configuration keys or claim a Ruby gem test proves Active Storage browser compatibility. Provide a tested strict-mode credential adapter separately.

References:

- [Ruby Storage SDK](https://docs.cloud.google.com/ruby/docs/reference/google-cloud-storage/latest/Google-Cloud-Storage)
- [Ruby File](https://docs.cloud.google.com/ruby/docs/reference/google-cloud-storage/latest/Google-Cloud-Storage-File)
- [Rails Active Storage](https://guides.rubyonrails.org/active_storage_overview.html)

## 8. Strict private/public/signed access walkthrough

The repository ships `compose.strict.yaml`, `config/strict.toml` and a clearly labelled local identity fixture. The strict TOML is a complete configuration sharing the same listener/storage settings as development, with `auth.mode = "strict"` and `auth.identity_file = "/etc/gcs-mimic/identities.toml"`. These are emulator fixtures, not real Google credentials.

Proposed fixture schema, to be implemented and frozen with this workflow:

```toml
[[identities]]
id = "local-admin@example.invalid"
tokens = ["local-admin-token"]
project_roles = { "local-project" = ["roles/storage.admin"] }

[[identities]]
id = "local-signer@example.invalid"
public_keys = ["/etc/gcs-mimic/signer.pub.pem"]
project_roles = { "local-project" = ["roles/storage.objectViewer"] }
```

The strict compose override replaces the config mount and adds read-only mounts for `config/identities.toml` and `.local/auth/signer.pub.pem`. It never mounts the private key into the running server. The test token is deliberately non-secret and valid only inside this local identity registry.

Generate a throwaway test signing key:

```bash
mkdir -p .local/auth
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out .local/auth/signer.pem
openssl pkey -in .local/auth/signer.pem -pubout -out .local/auth/signer.pub.pem
docker compose -f compose.yaml -f compose.strict.yaml up -d --wait
```

The strict override mounts the public key and identity fixture, not the private key. Ignore .local in git. Never load personal production credentials by default.

Expected sequence:

1. An admin-authenticated request creates a private bucket and uploads an object.
2. Anonymous GET of its plain object URL fails with the documented authorization error.
3. `url public` returns the address; anonymous GET still fails.
4. Admin grants allUsers the appropriate read access on a designated public test bucket; anonymous GET now succeeds there.
5. Remove the public grant; anonymous access fails again.
6. A V4 URL signed by a locally authorized signer allows GET of a private object.
7. Changing its method, object path, host or signed headers fails; expiry is tested with the clock fixture.
8. Enforcing public access prevention blocks anonymous public grants but does not block the valid signed request.

Complete admin setup pattern:

```bash
curl --fail -H "Authorization: Bearer local-admin-token" \
  -H "Content-Type: application/json" \
  --data '{"name":"private-bucket"}' \
  "http://127.0.0.1:4443/storage/v1/b?project=local-project"

curl --fail -H "Authorization: Bearer local-admin-token" \
  -H "Content-Type: application/json" \
  --data '{"name":"public-bucket"}' \
  "http://127.0.0.1:4443/storage/v1/b?project=local-project"

curl --fail -H "Authorization: Bearer local-admin-token" \
  -H "Content-Type: text/plain" --data-binary @fixtures/local-bucket/hello.txt \
  "http://127.0.0.1:4443/upload/storage/v1/b/private-bucket/o?uploadType=media&name=hello.txt"

curl --fail -H "Authorization: Bearer local-admin-token" \
  -H "Content-Type: text/plain" --data-binary @fixtures/local-bucket/hello.txt \
  "http://127.0.0.1:4443/upload/storage/v1/b/public-bucket/o?uploadType=media&name=hello.txt"

# Expected: authorization error; constructing the URL changes no permission.
curl -i "http://localhost:4443/private-bucket/hello.txt"

curl --fail -X PUT -H "Authorization: Bearer local-admin-token" \
  -H "Content-Type: application/json" \
  --data '{"version":1,"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}' \
  "http://127.0.0.1:4443/storage/v1/b/public-bucket/iam"

# Expected: object contents.
curl --fail "http://localhost:4443/public-bucket/hello.txt"

curl --fail -X PUT -H "Authorization: Bearer local-admin-token" \
  -H "Content-Type: application/json" --data '{"version":1,"bindings":[]}' \
  "http://127.0.0.1:4443/storage/v1/b/public-bucket/iam"

# Expected: authorization error again; project-level local admin grant remains.
curl -i "http://localhost:4443/public-bucket/hello.txt"
```

Example local signing command; implementation must supply a host binary or a documented Docker wrapper with identical behavior:

```bash
gcs-mimic url sign \
  --base-url http://localhost:4443 \
  --bucket private-bucket --object hello.txt \
  --method GET --expires 15m \
  --signer local-signer@example.invalid \
  --private-key .local/auth/signer.pem
```

A signer needs data permissions as well as a valid signature. Use XML object endpoints for signed requests. The policy/upload commands above are expected acceptance sequences; implementation must verify their output against the local fixture schema before publishing a runnable guide.

The source-built image also supplies the signing command, so installing a host binary is optional:

```bash
signed_url="$(docker run --rm --user "$(id -u):$(id -g)" \
  -v "$PWD/.local/auth/signer.pem:/run/signing-key.pem:ro" \
  gcs-mimic:dev url sign \
  --base-url http://localhost:4443 \
  --bucket private-bucket --object hello.txt \
  --method GET --expires 15m \
  --signer local-signer@example.invalid \
  --private-key /run/signing-key.pem)"
curl --fail "$signed_url"

curl --fail -X PATCH -H "Authorization: Bearer local-admin-token" \
  -H "Content-Type: application/json" \
  --data '{"iamConfiguration":{"publicAccessPrevention":"enforced"}}' \
  "http://127.0.0.1:4443/storage/v1/b/private-bucket"

# Expected: still succeeds because the signer has an effective read grant.
curl --fail "$signed_url"
# Expected: fails because the signed method was GET.
curl -i -X PUT --data 'unexpected' "$signed_url"
```

Expiry, signed-header and host mutation cases belong in an executable acceptance script using the controllable clock. Do not make a beginner wait 15 minutes to verify expiration. The fixture format and commands above are design commitments, not claims that their parser or binary already exists.

References:

- [Signed URLs](https://docs.cloud.google.com/storage/docs/access-control/signed-urls)
- [Canonical requests](https://docs.cloud.google.com/storage/docs/authentication/canonical-requests)
- [Public access prevention](https://docs.cloud.google.com/storage/docs/public-access-prevention)

## 9. Browser CORS and direct upload

The implemented [CORS journey](getting-started.md#browser-cors-and-mobile-application-origins)
and [actual HTTPS browser evidence](evidence/cors-browser.md) cover JSON transfers
and the existing XML reads. Bucket rules support custom serialized origins such
as `capacitor://localhost` and `ionic://localhost`, independently of HTTP/HTTPS
listener settings. Native app deep links, opaque origins, WebView networking and
real Google acceptance are distinguished in the [mobile research](slices/cors-mobile-origins.md).
Signed XML transfers and iOS device execution remain later work.

Provide a tested browser demo on `http://localhost:3000`. Its origin is different from the storage endpoint. Set the XML bucket CORS policy to that origin and the required methods/headers. Verify preflight and a real browser upload/download. A curl request can inspect headers, but cannot prove browser enforcement.

Test allowed and denied origins, required request headers, exposed response headers, and denied object access despite an allowed preflight. Keep JSON endpoint CORS defaults separate from XML bucket-configured behavior. Do not solve a browser test by allowing every origin globally.

Reference: [Cross-origin requests](https://docs.cloud.google.com/storage/docs/cross-origin).

## 10. Persistence, stopping and deliberate reset

Continue using the strict configuration selected in section 8. Upload an object after initial seeding, modify its metadata and permissions, then:

```bash
docker compose -f compose.yaml -f compose.strict.yaml restart gcs
docker compose -f compose.yaml -f compose.strict.yaml up -d --wait
```

The new object and metadata survive and the seed does not overwrite them. Also verify an interrupted upload follows the documented recovery behavior. Keep the same compose file set for the rest of this walkthrough; running base-only `up` could recreate the service in permissive mode. Confirm the effective mode in the startup/config summary after recreation.

```bash
docker compose -f compose.yaml -f compose.strict.yaml stop
docker compose -f compose.yaml -f compose.strict.yaml start --wait
docker compose -f compose.yaml -f compose.strict.yaml down
```

Stopping or `down` without `-v` preserves the named volume. Recreating the service must recover the same data. Before offering the reset command, the guide explicitly says it deletes this project's local emulator data:

```bash
docker compose -f compose.yaml -f compose.strict.yaml down -v
docker compose -f compose.yaml -f compose.strict.yaml up -d --wait
```

After deliberate reset, initial fixtures appear again. Do not use `down -v` as routine shutdown or CI cleanup for a shared developer project.

## 11. CI isolation and repeatability

Provide a checked-in CI runner that uses a unique compose project name and isolated named volume for each run, waits on health, executes examples/contracts, collects logs on failure, and removes only its own project and volume in a finally/trap path. Avoid fixed host ports in parallel runs; discover the assigned mapped port or run test clients inside the isolated compose network.

Tests use fixture identities and explicit local endpoints. Block unintended outbound cloud requests. Run the suite twice from clean state and once after restart; results must be deterministic. The runner must not reuse the developer's persistent volume.

## 12. Debugging and resource settings

| Symptom | Checks and required troubleshooting guidance |
| --- | --- |
| Connection refused | Health, port mapping, listener, host versus peer-container endpoint. |
| Resumable upload cannot reach Location | Validated request authority, fixed advertised override, proxy configuration. |
| Metadata works but public URL fails | XML path/virtual-host routing, host allowlist, permission profile. |
| Signature mismatch | Original signed host/port/method/path/headers and key mapping; do not rewrite URLs. |
| Browser fails but curl works | Actual origin, preflight headers, XML bucket CORS and endpoint type. |
| Unexpected permission success | Confirm strict mode; inspect local principal and effective IAM/ACL/UBLA/PAP. |
| Data disappears after restart | Named volume identity, accidental -v reset, seed policy, recovery logs. |
| OOM or high idle CPU | Buffer/concurrency limits, catalog cache, page-cache accounting, maintenance scheduling. |

Provide structured logs with request IDs while redacting signed query credentials and sensitive headers. Include a version/config-summary command or startup summary showing effective mode and advertised endpoints.

The initial acceptance target is streaming one 1 GiB object at concurrency 1 under a 128 MiB container memory limit without object-size-proportional buffering. This is a target, not a measured result or universal guarantee. Benchmarks specify host architecture, CPU limit, concurrency, backend, durability mode and cache state; report RSS and cgroup memory independently. Tune limits based on evidence instead of assuming Rust makes every workload cheap.

## Release gate

The final published getting-started guide is complete only after a clean checkout on supported macOS/Linux environments can execute every applicable section, all files referred to actually exist, command outputs are verified, and unsupported milestones are labelled clearly. The current document fixes the intended user journey; it does not declare that journey implemented.
