# fake-gcs-server migration and compatibility target

<!-- markdownlint-configure-file
{"MD013": {"tables": false, "code_blocks": false}}
-->

[日本語](../ja/fake-gcs-server-migration.md)

## Replace a fake-gcs-server Compose service

This walkthrough covers UX-012, DESIGN-061 and UX-001: HTTP, initialized empty
buckets, Go SDK and unsigned browser transfers in the default permissive/native
profile. It does not migrate existing backend data or establish full replacement.
The [scenario coverage and results](slices/migrate-guide-compose.md) include the
known differences in the pinned fake-gcs-server comparison.

For example, replace this source service (retain your existing application
services and their `STORAGE_EMULATOR_HOST=gcs:4443`):

```yaml
services:
  gcs:
    image: fsouza/fake-gcs-server:1.56.1
    command: ["-scheme", "http", "-host", "0.0.0.0", "-port", "4443",
              "-public-host", "gcs:4443", "-external-url", "http://gcs:4443",
              "-backend", "filesystem", "-filesystem-root", "/storage", "-data", "/data"]
    ports:
      - "127.0.0.1:${GCS_MIMIC_PORT:-4443}:4443"
    volumes:
      - ./data:/data:ro
      - source-data:/storage
volumes:
  source-data:
```

Five common option groups translate as follows. Old flags and `FAKE_GCS_*`
variables are diagnosed with replacement advice, never accepted as aliases.

| Before | After in this walkthrough | Why |
| --- | --- | --- |
| `-scheme http` | `GCS_MIMIC_SCHEME: http` | Otherwise the default is HTTPS, so existing HTTP clients fail. The image healthcheck follows this setting without an override. |
| `-host 0.0.0.0 -port 4443` | `GCS_MIMIC_LISTEN: 0.0.0.0:4443` | Peers need a wildcard listener; a loopback-only listener is unreachable from other containers. This repeats the image default. |
| `-public-host gcs:4443 -external-url http://gcs:4443` | `GCS_MIMIC_API_BASE_URL: http://gcs:4443` | Container SDKs must reach resumable Location. Native path-style reads need no public-host setting; this does not reproduce legacy virtual-host matching or grant permissions. |
| `-backend filesystem -filesystem-root /storage` | Named volume at `/var/lib/gcs-mimic` | The default persistent backend uses SQLite and opaque blobs. Keep the source volume separate; its format cannot be mounted as native storage. Memory backend has no equivalent here. |
| `-data /data` with empty bucket directories | `GCS_MIMIC_BUCKETS_FILE: /buckets.json` and a read-only file mount | Missing buckets must be created on each startup. Seed runs only on first catalog initialization; directories added afterward do not create buckets. |

The Ruby SDK's ordinary high-level calls now accept no-op wire defaults:

| Ruby call | gcs-mimic behavior |
| --- | --- |
| `create_file(StringIO, extensionless_name)` without content_type | Empty upload MIME is treated as absent; contentType defaults to application/octet-stream. Nonempty invalid MIME still returns 400. |
| `create_bucket(name)` without optional arguments | Disabled, empty or null Autoclass is treated as absent, with no Autoclass response property. Enabled or additional Autoclass settings return 400 badRequest; omit them or use enabled:false. |
| `compose(sources, name)` without optional arguments | Null deletion/destination uses ordinary non-destructive composition and destination defaults. Explicit true retains hard source deletion. |

These are the scoped [Ruby defaults](slices/sdk-ruby-defaults.md), not Autoclass
implementation or proof of provider null-input parity. No extra server setting is
required.

| Listing call | gcs-mimic behavior |
| --- | --- |
| Go `Query{MatchGlob: "**/*.txt"}` or Ruby `bucket.files(match_glob: "**/*.txt")` | JSON listing filters names and slash-delimited prefixes, including pagination and versions. Empty matchGlob is absent. Invalid syntax or an incompatible delimiter returns 400; use delimiter `/` or omit it. See the [glob contract](slices/sdk-list-glob.md). |

| Storage v2 gRPC object call | gcs-mimic behavior |
| --- | --- |
| Go `storage.NewGRPCClient(ctx)` Writer, NewReader, NewRangeReader, Attrs, Delete and Objects | Set `STORAGE_EMULATOR_HOST_GRPC` to the separately configured h2c listener, for example `127.0.0.1:4444`. Default Writer chunk size works for small and multi-MiB objects, and JSON/gRPC share data, generations, permissions, holds and retention. Prefix/delimiter/page tokens and resumable upload offsets/checksums are supported. Rapid append, cancellation, non-final non-resumable flush/state lookup and unsupported fields return UNIMPLEMENTED; see the [object I/O contract](slices/grpc-v2-object-io.md). This is Storage v2, not the reference emulator's legacy v1 service. |

| Object metadata call | gcs-mimic behavior |
| --- | --- |
| Go Writer/Update TemporaryHold; Ruby set_temporary_hold! / release_temporary_hold! | The flag persists across restart and blocks deletion and upload/copy/rewrite/compose replacement with 403 objectUnderActiveHold until released. Metadata remains editable. |
| Go Writer/Update CustomTime; Ruby create_file(custom_time:) / custom_time= | RFC 3339 timestamps persist; equal/later updates succeed. Clearing, backdating or omitting customTime from PUT after setting returns 400 invalidArgument; supply the current/later timestamp, or omit it from PATCH to preserve it. |

The [object holds/custom time contract](slices/sdk-object-holds-customtime.md)
records null defaults and local error details.

| Bucket metadata call | gcs-mimic behavior |
| --- | --- |
| Ruby `bucket.update { \|b\| b.public_access_prevention = "enforced" }` / JSON bucket PUT | Only supplied settings change; omitted labels, versioning and other modeled settings survive. Supplied labels replace the map; labels:null clears and versioning:{} disables. This is a local SDK compatibility choice, without real-GCS verification; see the [PUT contract](slices/bucket-metadata-put.md). |

| Soft-delete call | gcs-mimic behavior |
| --- | --- |
| Go `BucketAttrs.SoftDeletePolicy` / Ruby `bucket.soft_delete_policy=` | JSON insert/PATCH/PUT accept 0 or 604800–7776000 retention seconds. Omission at creation disables soft delete locally, unlike GCS's seven-day new-bucket default; omission on PATCH/PUT preserves the current policy. |
| Go `Query.SoftDeleted`, `Generation(g).SoftDeleted().Attrs`, `Restore` / Ruby `files(soft_deleted: true)`, `file(name, generation:, soft_deleted: true)`, `restore_file` | Deleted/overwritten generations retain bytes until hardDeleteTime; ordinary reads hide them. Omitted policy and explicit zero both allow listing (200, empty unless retained generations remain); missing generations return 404 on get/restore. This differs from Google's absent-policy errors. Restore creates a new live generation and keeps the original retained generation. Holds and retention still protect deletion/replacement. |
| Delete a bucket containing only soft-deleted generations | Permanently removes the bucket and retained generations. Bucket soft deletion/restore and initialization-file softDeletePolicy remain unsupported; see the [soft-delete contract](slices/sdk-soft-delete-restore.md). |

For an existing `bucket`, enable seven days before deleting objects. These
setters match the pinned Go Storage v1.70.0 and Ruby Storage 1.62.1 SDK tests
(`storage`/`time` imports and the existing Go context `ctx` are assumed):

```go
_, err := bucket.Update(ctx, storage.BucketAttrsToUpdate{SoftDeletePolicy: &storage.SoftDeletePolicy{RetentionDuration: 7 * 24 * time.Hour}})
```

```ruby
bucket.soft_delete_policy = { retention_duration_seconds: 604800 }
```

Check Go's returned `err` before deleting. An unset policy returns an empty 200
for `softDeleted=true` (Google returns 400): this is an owner-approved local
choice. Previously retained generations remain listable after disabling the
policy. With `--log-level debug`, an empty policy-free listing explains the
reason and gives `softDeletePolicy.retentionDurationSeconds=604800` as a repair.

| Bucket retention call | gcs-mimic behavior |
| --- | --- |
| Go `BucketAttrs.RetentionPolicy` / Ruby `bucket.retention_period = 3600` | JSON insert/PATCH/PUT persist a period from 1 to 3,155,759,999 seconds and expose effectiveTime/isLocked. Unlocked policies can be changed or removed; PATCH null clears the policy. Objects expose creation time plus the current period as retentionExpirationTime. |
| Go `BucketHandle.If(...).LockRetentionPolicy` / Ruby `bucket.lock_retention_policy!` | Requires the current metageneration and storage.buckets.update in strict mode. A locked policy cannot be removed or shortened: 400 badRequestException explains the lock and gives a valid extension example. IAM denial remains 403 forbidden. Test locks only on emulator buckets. |
| Delete or replace a retained object, including XML PUT, move source and compose source deletion | Returns 403 retentionPolicyNotMet until expiration; holds independently keep blocking deletion/replacement. No new setting is required. See the [retention contract](slices/sdk-retention-policy.md) for unsupported interactions. |

For `data/assets/` and `data/exports/`, write `buckets.json` before startup:

```json
{"version":1,"project":"local-project","buckets":[{"name":"assets"},{"name":"exports"}]}
```

Remove the empty-directory seed mount and add this file instead. Restart creates
only missing entries, preserves existing objects and API settings, and does not
delete buckets removed from the file. Add future bucket names to this file and
restart. Object fixtures are a separate [one-time seed](container-usage.md#load-local-files).

Replace the `gcs` service with the published image and keep your application
services unchanged. Place `buckets.json` beside the Compose file; the mount
source is relative to it. The example file is `compose.migration.yaml`; in an
existing app, edit the service in your own Compose file instead.

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.1.3
    ports:
      - "127.0.0.1:${GCS_MIMIC_PORT:-4443}:4443"
    environment:
      GCS_MIMIC_SCHEME: http
      GCS_MIMIC_LISTEN: 0.0.0.0:4443
      GCS_MIMIC_API_BASE_URL: http://gcs:4443
      GCS_MIMIC_BUCKETS_FILE: /buckets.json
    volumes:
      - migration-data:/var/lib/gcs-mimic
      - ./buckets.json:/buckets.json:ro
volumes:
  migration-data:
```

The API base is fixed, not derived from request Host. Without the explicit
container origin, bind-derived loopback Location points another container back
to itself. One server advertises one origin: container SDKs use `gcs:4443`, while
the host/browser uses `localhost` and the published port. The server cannot
discover Docker port mappings. The image healthcheck needs no override. App
services can wait with `depends_on: {gcs: {condition: service_healthy}}`.

Use a distinct Compose project and host port per worktree; the internal port
and advertised container URL stay 4443. Run these commands beside the files:

```sh
export COMPOSE_FILE=compose.migration.yaml
export COMPOSE_PROJECT_NAME=assets-migration
export GCS_MIMIC_PORT=14443
docker compose up -d --wait
docker compose exec -T gcs gcs-mimic healthcheck
api="http://localhost:${GCS_MIMIC_PORT}"
curl --fail "$api/storage/v1/b/assets"
curl --fail "$api/storage/v1/b/exports"
curl --fail -H 'Content-Type: application/json' -d '{"name":"scratch-assets"}' \
  "$api/storage/v1/b?project=local-project"
curl --fail --data-binary 'hello' \
  "$api/upload/storage/v1/b/assets/o?uploadType=media&name=hello.txt"
test "$(curl --fail "$api/storage/v1/b/assets/o/hello.txt?alt=media")" = hello
```

For a second worktree use, for example, project `assets-migration-two` and
`GCS_MIMIC_PORT=24443`; project-scoped volumes keep state separate. Stop with
`docker compose down` using the same file/project. The named volume retains data.

### Cross-origin upload and reading

These raw HTTP requests model a browser page at `http://localhost:3000` using
the JSON API without credentials; no CORS setting is required for this scope.
Check the echoed `Access-Control-Allow-Origin` in both responses:

```sh
curl --fail -i -X OPTIONS -H 'Origin: http://localhost:3000' \
  -H 'Access-Control-Request-Method: POST' \
  -H 'Access-Control-Request-Headers: content-type' \
  "$api/upload/storage/v1/b/assets/o?uploadType=media&name=browser.txt"
curl --fail -i -H 'Origin: http://localhost:3000' -H 'Content-Type: text/plain' \
  --data-binary 'browser bytes' \
  "$api/upload/storage/v1/b/assets/o?uploadType=media&name=browser.txt"
curl --fail -i -H 'Origin: http://localhost:3000' \
  "$api/storage/v1/b/assets/o/browser.txt?alt=media"
curl --fail "$api/assets/browser.txt"
```

The scenario also verifies twelve concurrent browser-role uploads and exact
reads. Raw HTTP does not verify browser enforcement. Path-style anonymous GET
returns bytes in this permissive profile; XML cross-origin fetch uses separate
bucket CORS rules. CORS never grants access in strict mode. See
[Google's CORS distinction](https://docs.cloud.google.com/storage/docs/cross-origin).

For an unsigned resumable session created by the server, preserve path/query
when replacing the internal authority with the browser-reachable authority.
The scenario verifies changed-Host PUT with Content-Range and a preflight.
Cancellation deliberately returns **499**, and later session access returns
410, following the [JSON resumable protocol](https://docs.cloud.google.com/storage/docs/performing-resumable-uploads).
Do not treat 499 here as an infrastructure failure or retry cancellation as an
upload. This example checks the status explicitly instead of using curl `--fail`:

```sh
location=$(curl --fail -sS -D - -o /dev/null \
  -H 'Content-Type: application/json' -H 'X-Upload-Content-Type: text/plain' \
  -d '{}' "$api/upload/storage/v1/b/assets/o?uploadType=resumable&name=cancelled.txt" \
  | awk 'tolower($1)=="location:" {sub("\r$", "", $2); print $2}')
host_session="$api${location#http://gcs:4443}"
cancellation_status=$(curl -sS -o /dev/null -w '%{http_code}' -X DELETE "$host_session")
test "$cancellation_status" = 499
```

Authority rewriting is scoped to these unsigned permissive sessions. Never
rewrite a strict signed URL after signing; sign the final recipient-visible URL.

### Rails Active Storage with application-supplied local credentials

The data `endpoint` does not redirect OAuth. With googleauth 1.17.6,
service-account JSON `token_uri` is ignored; Rails 8.1.4 `storage.yml` cannot
pass the credential constructor's `token_credential_uri`. `external_account`
credentials lack the private key needed for default V2 signing. The emulator
has no token-issuing route, so there is no supported local token exchange path.

For an isolated **permissive** emulator, construct the service in application
code with service-account credentials carrying a preset dummy access token.
The JSON file below is an example location for a locally generated test key,
with an example project and email; do not use real Google credentials.

```ruby
require "googleauth"
require "active_storage/service/gcs_service"

credentials = File.open("config/credentials/example-local-service-account.json") do |key|
  Google::Auth::ServiceAccountCredentials.make_creds(
    json_key_io: key,
    scope: "https://www.googleapis.com/auth/devstorage.full_control"
  )
end
credentials.access_token = "example-local-token"
credentials.expires_at = Time.now + 86_400
service = ActiveStorage::Service::GCSService.new(
  project: "example-project", bucket: "example-bucket", credentials: credentials,
  endpoint: "http://127.0.0.1:4443/"
)
```

Use this application-created service for local operations. The example JSON
needs `type: service_account`, `project_id: example-project`,
`client_email: example-signer@example.invalid` and a locally generated RSA
`private_key`. Create `example-bucket` first. A fresh preset token avoids OAuth
while retaining the key for V2 signing; renew the token and expiry before the
client needs a refresh or googleauth will attempt its normal OAuth endpoint.
Permissive mode accepts the dummy bearer token without validating it.

The guarded Rails 8.1.4 / Storage 1.62.1 scenario verifies upload, download,
delete and an unchanged V2 signed GET with these credential semantics, without
clearing internal authorization. Only direct service calls were verified; wiring
this service into `ActiveStorage::Blob` or attachments (including replacing
`Blob.service` or registering a named service in `storage.yml`) is unverified.
This is a separate application recipe; ordinary
`storage.yml` authentication still fails. Strict Rails integration and automatic
token renewal are unverified. See [verification and limits](slices/sdk-local-auth-softdelete-notes.md)
and SPEC-ACTIVE-STORAGE-LOCAL-CREDENTIALS; DESIGN-084 remains PARTIAL.

### Migration troubleshooting

| Symptom | Cause | Repair |
| --- | --- | --- |
| Startup rejects `-scheme` or `FAKE_GCS_SCHEME` | Source names are not aliases | Remove the old command/key and set `GCS_MIMIC_SCHEME: http`. |
| SDK initiation succeeds but its next request cannot connect | Location contains loopback or a host-only address | Set `GCS_MIMIC_API_BASE_URL: http://gcs:4443` for container clients and recreate the service. |
| Browser cannot resolve `gcs` | Container DNS is not host DNS | Use `http://localhost:14443` for the example browser origin; preserve session path/query when translating unsigned URLs. |
| A newly added empty directory creates no bucket | Seed completed during the first catalog initialization | Add `{"name":"exports"}` to the version-1 buckets file and restart; do not erase the data volume. |
| HTTP connection fails or an inherited probe is unhealthy | Scheme/listener differs from the client or probe environment | Use `GCS_MIMIC_SCHEME: http` and `GCS_MIMIC_LISTEN: 0.0.0.0:4443`; let the image's probe inherit them. |
| Bucket listing returns 400 with a project example | Native requires an explicit project | Request `/storage/v1/b?project=local-project`. The existing legacy profile defaults omitted project to seed_project. |
| Docker reports the host port is in use | Another worktree publishes the same port | Set `GCS_MIMIC_PORT=24443` and a different Compose project; update browser URLs, leaving `http://gcs:4443` unchanged. |
| `curl --fail` reports 499 on session DELETE | Successful JSON resumable cancellation uses 499 | Inspect `%{http_code}` as above and accept 499 for cancellation only. |

## Goal and current state

The migration goal is to preserve existing application workflows while adding
stricter Google-contract validation, broader capabilities and resource controls.
CLI spelling, configuration structure and internal storage need not be identical
to fake-gcs-server. Document equivalent uses and explicit differences; the broader
full-superset gate below tracks remaining functional coverage.
This is a target, not a current compatibility claim. Migration importers,
Go embedding and source-only startup settings remain unavailable.
The existing `legacy-emulator` profile covers a limited bucket contract; it does
not implement the complete migration profile described here.

gcs-mimic serves path-style and opt-in virtual-host XML GET/HEAD/PUT, strict
V4 RSA signed transfers, path-style V2 RSA GET/PUT and scoped public access
prevention, with
[recorded evidence](evidence/signed-urls.md). URL construction and authorization
remain separate. Broader XML/listing, ACLs, UBLA transitions, permissive legacy
signing and complete endpoint/configuration equivalence remain gaps.

The first reference is the fake-gcs-server release in the
[detailed comparison](feature-comparison.md).
Record its complete surface before implementation and add later releases as
explicit baselines. Do not present one release's passing tests as proof about
all versions. No baseline
capability is excluded from the full-superset target merely because it is absent
from the current GCS inventory or the early-release queue.

The ledger's `delivery_plan.migration` owns the planning scope and acceptance
milestones. Existing feature statuses and evidence remain authoritative; this
broader target adds no passing full-migration result.

## Concurrent uploads and storage errors

API uploads have no separate concurrency quota. `max_connections` (default 32)
bounds active connections; excess connections wait. Raising it to 64 permits
64 simultaneous uploads without an upload-admission 503. Upload prechecks and
resumable session creation/read/checkpoint wait for catalog admission within
the existing catalog deadline; other catalog operations can still return 503
when their 32-operation admission limit is saturated. Bodies retain 64 KiB chunks
and a depth-two channel, without collecting the whole object in memory.

Shutdown waits for active uploads and cleanup within `shutdown_timeout_ms`.
Disconnecting before EOF aborts publication and cleans staging. New uploads
after catalog shutdown starts fail with a `catalog is closed` cause.
JSON object 503 messages also identify catalog saturation, catalog wait timeout,
I/O failure or corruption when safely known; private backend details are hidden.
For wait timeouts, retry with generation preconditions or consider
`catalog_wait_timeout_ms=60000`. A completed-body upload can still commit after
the response times out: read state before retrying an unconditional write.

The [upload admission contract](slices/migrate-upload-admission.md) records
12/40-upload tests at 32 connections, 64-upload tests at 64 connections,
resumable initiation/final 128 KiB PUT at 40/64 uploads, concurrent GET/list checks
and shutdown/disconnect cleanup. This is scoped
emulator evidence, not a Google rate-limit or full migration parity claim.

## Bucket core baseline before further bucket extensions

The create-by-name, basic-resource GET and empty-bucket DELETE baseline is
delivered on **2026-10-22** with source-supported versioning/default hold
configuration and real object effects. Select `compat_profile=legacy-emulator`
for source-specific naming/defaults/input/deletion behavior; native and strict
authorization retain their independent contracts. This is a named capability
baseline, not complete migration or full Google-method compatibility.

The [final integrated evidence](evidence/bucket-core-parity.md) records Rust,
pinned Go, both Linux architectures and the full runnable guide.
The [executed comparison](evidence/bucket-core-comparison.md) and
[raw records](evidence/bucket-core-comparison.txt) contain 92 requests each
against filesystem and memory reference backends plus both local profiles,
with no skipped profile. The reference is release `v1.56.1`, commit
`896ece51e49eed0946089f3089f6343855c0d106`.

| Operation | Executed result | Remaining difference |
| --- | --- | --- |
| Create by name | Source-valid names, omitted project, duplicates and failed-state preservation pass; both settings persist and affect HTTP object mutations | Native requires project/Google-oriented names. Profile retains strict project ownership, input bounds and 400 badRequest for unsupported protection; fs reference rejects versioning true |
| Basic resource | Create/GET match in every target; profile emits both booleans and source US-CENTRAL1/project/ETag defaults; list items match profile GET; restart/migration and strict permissions pass | Millisecond timestamps, local parser messages, native list scoping/paging, 666-byte profile name cap and unsupported trailing collection slash |
| Empty deletion | Profile returns exact 200 JSON null/newline and nonempty 412; native returns 204/409. Repeat/missing, recreate, protected histories and preserved object bytes pass | Atomic/history-aware delete and enforced holds intentionally exceed source live-only/no-hold behavior |
| Bucket permission test | Go `IAM().TestPermissions` and Ruby `bucket.test_permissions` use the default calls; strict returns effective project/bucket grants and permissive returns all supported requested permissions | No permission is needed to call it. Project-only create/list names return 400; other unsupported permissions and billing return 400 badRequest. Provider error parity and expanded ACL/UBLA/PAP controls remain pending; [contract and evidence](slices/sdk-test-permissions.md) |
| Uniform bucket-level access / predefined ACL | Go/Ruby default UBLA creation, inspection and disabling work; aliases and the 90-day lock persist, including creation with enforced PAP | Under UBLA, remove predefinedAcl/predefinedDefaultObjectAcl/destinationPredefinedAcl parameters: they return 400. Without UBLA they remain explicitly unsupported. ACL CRUD/effects and initialization-file UBLA remain pending; [contract and evidence](slices/sdk-ubla-predefined-acl.md) |
| Versioning/default holds | HTTP API creation enables retained versions and inherited holds in both profiles; selected history read/list/delete and hold release pass, with pinned Go workflows | Propagation/provider edge parity, retention and soft delete remain pending; temporary holds and custom time are covered by the object SDK slice |
| Atomic object move | Default Go `ObjectHandle.Move` and Ruby `bucket.move_file` rename live objects within one flat-namespace bucket; bytes/metadata persist, all eight conditions and holds are enforced, and source deletion plus destination publication use one catalog transaction without copying the blob | HNS/folders/cross-bucket moves, full projection, move-only authorization and provider eligibility/history/error parity remain pending. Source live generation is removed even with versioning; replaced destination history is retained. See [contract and evidence](slices/sdk-object-move.md) |

The table above retains earlier reference-comparison evidence. Its current
400 badRequest rejection expectations were updated by the
[fail-fast slice](slices/sdk-unsupported-fail-fast.md); the reference comparison
script was not rerun for that slice.

Source identities inspected at that revision are unchanged:

| Source | Git blob |
| --- | --- |
| `fakestorage/bucket.go` | `b1cfdbb9e6100a917888e309660cadf11a2de657` |
| `fakestorage/response.go` | `1a19a7fafa44bf2f80899c9b1eb70bdc6a75f2ed` |
| `fakestorage/json_response.go` | `7a10f7160d86fc154d904dc3ca3bd286eafcdaab` |
| `fakestorage/bucket_test.go` | `517271a4cebf115af54a94211ddedb08b38fef23` |

Every observed difference is classified as intentional, strict or unsupported
in the [scope contract](slices/bucket-core-parity.md#state-and-remaining-boundaries)
and comparison report, including object-resource framing/ACL/identity differences
on preservation probes. Those probes verify exact retained bytes, not complete
source object wire rendering. Official Google documentation, Discovery and
googleapis definitions remain the contract authority.

The three broad JSON bucket rows remain PARTIAL with their original policy,
ACL/owner and advanced cases. Versioning/default-hold behavior rows also remain
PARTIAL for unresolved provider interactions. Supported setting updates are now
delivered through [PATCH](evidence/bucket-metadata-patch.md) and
[PUT](evidence/bucket-metadata-put.md); this does not complete the broad methods.
All other source surfaces, configuration flags, helpers/events, Go embedding,
legacy gRPC and lossless import/export remain required by the full-superset
gate below; this delivery does not remove them.

## Compatibility surfaces

The observations below come from fake-gcs-server's README, configuration,
server implementation and gRPC adapter.
Inventory every flag, exported Go API and registered route before freezing the
executable contract; these examples are not the entire inventory.

| Surface | Observed source behavior | Required migration result |
| --- | --- | --- |
| Startup | Flags and `FAKE_GCS_*` environment variables; flags take precedence | Provide documented equivalent uses with native `GCS_MIMIC_*` settings; identical option names are not required |
| Listener/TLS | HTTPS by default; HTTP and dual-listener modes, certificate settings | Existing scheme/ports/trust configuration continue to work |
| Container readiness | Existing HTTP deployments need a probe matching their listener | `gcs-mimic healthcheck` and the image probe derive readiness from `GCS_MIMIC_*` and the selected JSON file; wildcard addresses use loopback and both prefers HTTP. HTTPS trusts the development CA; custom CAs use `--ca-cert`. CLI-only server overrides need matching environment/file settings or an explicit probe `--url`. |
| Image listener and data defaults | Source supplies startup defaults through its entrypoint | The image supplies `GCS_MIMIC_LISTEN=0.0.0.0:4443` and `GCS_MIMIC_DATA_DIR=/var/lib/gcs-mimic` as environment defaults, which override JSON file values (file < environment < CLI). Use, for example, `GCS_MIMIC_LISTEN: 0.0.0.0:8080` and `GCS_MIMIC_DATA_DIR: /custom-data` in Compose, and adjust the published port and volume mount accordingly. |
| Bucket listing without project | Source allows `GET /storage/v1/b` without project | `legacy-emulator` defaults to `seed_project`, as creation does; native returns 400 with `?project=local-project`. Explicit project validation, filtering and authorization remain active. |
| Lifecycle configuration | The pinned source ignores lifecycle settings at bucket creation and does not execute lifecycle rules | JSON insert/get/list/PATCH/PUT validate and persist the scoped rules, including ordinary Go/Ruby lifecycle calls. gcs-mimic stores configuration but never executes actions; XML lifecycle and initialization-file lifecycle remain unsupported. See the [lifecycle contract](slices/sdk-lifecycle-config.md). |
| Endpoints | Public/virtual-host and API routes, external upload URLs and mutable internal configuration | Preserve client-visible host/path/query behavior; map advertised addresses explicitly |
| HTTP semantics | JSON/XML, uploads, ranges, conditions, metadata, ACLs, batching and error shapes | Run the same client scenarios and negative cases against both implementations |
| Permissive signed URLs | Signature/expiry are not validated; callers can rewrite hosts | Permissive V2/V4 ignores signatures and expiry; V2 credential structure and numeric Expires are validated. Strict path-style V2 GET/PUT verifies registered RSA-SHA256 keys, expiry and signed headers with shared IAM permissions; URLs are used unchanged. V2 does not bind Host or response overrides. HMAC, V2 HEAD/generation selection, virtual hosts, POST and resumable signing remain unsupported. See the [V2 contract](slices/sdk-active-storage-v2-signing.md). |
| Fixture preload | Docker mounts `/data`; the entrypoint passes `-data /data` | Import the seed format and reproduce the selected seed/reseed lifecycle |
| Durable data | Filesystem root with object files and xattr/sidecar metadata | Read-only, verified conversion into a separate destination |
| Memory backend | In-process bytes and active/archived versions | Optional ephemeral behavior and export before the source process stops |
| Go embedding | Constructors, server lifecycle, clients/handlers and direct object methods | A separately tested Go-compatible adapter; a Rust executable alone is insufficient |
| gRPC | v1.56.1 imports Storage **v1** protobufs | gcs-mimic optionally opens separate plaintext `grpc_listen` (for example `127.0.0.1:4444`); point Go `STORAGE_EMULATOR_HOST_GRPC` there. Storage v2 bucket CRUD, object reads/writes/deletes/listing and resumable uploads share JSON state, metadata validation and strict authorization. Scoped bucket/object updates, bucket IAM, retention locking, rewrite, compose, same-bucket move and soft-delete restore use that same shared state. BidiReadObject supports Go `NewMultiRangeDownloader` with `storage.WithGRPCBidiReads()`, CRC32C and generation-pinned read handles; handles retain the latest 64 initial selections within the process and require current authorization on reconnect. CancelResumableWrite, Rapid append, unsupported fields and unknown RPCs return non-retryable `UNIMPLEMENTED` with ErrorInfo and JSON API guidance. Legacy v1 is not served and is a separate compatibility slice in progress; v2 cannot substitute for v1. See the [bucket contract](slices/grpc-v2-buckets.md), [object I/O contract](slices/grpc-v2-object-io.md), [update/copy contract](slices/grpc-v2-update-copy.md), [bidi read contract](slices/grpc-v2-bidi-read.md) and [foundation contract](slices/grpc-v2-foundation.md). |
| Helpers/events | Internal config/reseed routes and notification/event settings | Inventory and test local behavior plus explicit external integration boundaries |

Application source should not need modification. Renaming environment keys,
changing an image/binary or a Go module replacement and performing an explicit
data conversion are deployment steps, which must be documented and tested.
Do not claim literal in-process Go library replacement merely because equivalent
HTTP operations exist.

## Startup options and current equivalents

The [complete environment reference](fake-gcs-server-configuration.md) lists all
19 source variables, defaults, CLI precedence and current native mappings.
Source variables are not accepted aliases in the current gcs-mimic binary.

These are role mappings, not accepted aliases. gcs-mimic uses `serve --name VALUE`
and native `GCS_MIMIC_*` environment variables. Do not copy a source command and
assume that changing the image name is sufficient.

| fake-gcs-server option | Current gcs-mimic setting | Difference |
| --- | --- | --- |
| `-scheme http` / `https` / `both` | `--scheme`, `GCS_MIMIC_SCHEME` | Both uses primary HTTPS and a second HTTP listener with one advertised API origin |
| `-host`, `-port` | `--listen IP:PORT`, `GCS_MIMIC_LISTEN` | Combined listener setting; container port publication remains separate |
| `-port-http` | `--listen-http IP:PORT`, `GCS_MIMIC_LISTEN_HTTP` | Requires `scheme=both`; default primary IP:8000 |
| `-external-url` | `--api-base-url`, `GCS_MIMIC_API_BASE_URL` | Fixed origin for API-generated links; not a public-host allowlist |
| `-public-host` | `--public-base-url`, `GCS_MIMIC_PUBLIC_BASE_URL` | HTTP(S) origin; adds a DNS virtual domain. Legacy Host matching requires `--compat-profile legacy-emulator`; native path-style accepts any Host. |
| `-data` | `--seed-dir`, `GCS_MIMIC_SEED_DIR` | Initial import into a fresh catalog, not startup replay or live synchronization |
| `-filesystem-root` | `--data-dir`, `GCS_MIMIC_DATA_DIR` | Persistent destination with a different format; cannot reuse the source root directly |
| `-cert-location`, `-private-key-location` | `--tls-cert`, `--tls-key` | Both PEM paths are required for custom TLS |
| `-log-level` | `--log-level`, `GCS_MIMIC_LOG_LEVEL`, JSON `log_level` | Minimum runtime stderr severity; default info; debug/info/warn/error and warning alias; diagnostics and stdout lifecycle are unfiltered |
| `-cors-headers` | Bucket `cors` through the JSON API | Source setting extends globally allowed request headers; gcs-mimic XML rules are per bucket, JSON defaults are fixed |
| `-location` | `--default-location`, `GCS_MIMIC_DEFAULT_LOCATION`, JSON `default_location` | Native accepts official regional/predefined dual-region and `US`, `EU`, `ASIA` names, case-insensitively; API/initializer nonempty location wins. Legacy profile retains fixed `US-CENTRAL1` and rejects the global default. No physical placement or configurable dual-region simulation; see [location contract](slices/sdk-regional-locations.md). |
| `-backend memory` | None | Current backend is persistent SQLite metadata plus files |

The source's `-public-host` controls public URL construction and Host matching
on relevant public routes; it does not select the listener. A configured port
participates in its matching. `-external-url` separately controls API-generated
URLs. Native virtual-host domains are DNS suffixes without a port; the actual
request authority still includes the port for strict signature checks.
Neither product's public-host/URL setting grants bucket permissions.
Public-origin normalization removes HTTP 80/HTTPS 443 and lowercases DNS;
non-default ports remain significant. IPs support path-style only. With legacy
matching, use equal API/public authorities for SDK XML reads: a different API
authority falls back to a hostname bucket. URL helpers use the file's
`public_base_url` only when `--config PATH` is explicit;
`--base-url` overrides it.

For example, native `url public --base-url http://localhost:4443 --bucket assets
--object hello.txt` generates a path-style URL. For `assets.storage.test`, enable
`--xml-virtual-host-domains storage.test` and arrange client DNS. HTTPS also needs
a matching certificate. Neither action changes IAM or makes a private object
public. Sign the final recipient-visible URL; strict signatures cannot survive
arbitrary host rewrites.

Source: pinned [configuration](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/internal/config/config.go)
and [routes and URL helpers](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/server.go).
The table is not a complete flag inventory; event and
other source-only settings still need individually scoped decisions.

Use native names where they already express the intended role. Retaining every
source suffix or adding aliases is not required. Keep CLI > environment > server
JSON > default precedence. If aliases are added later, reject contradictory
values at the same precedence rather than depending on iteration order.
Legacy `FAKE_GCS_*` keys do not select compatibility mode. The development
binary rejects the 19 known keys with migration advice and warns on unknown
names; these are diagnostics, not aliases. See the
[migration diagnostics](fake-gcs-server-configuration.md#migration-diagnostics).

`GCS_MIMIC_PORT` in the repository's source-build Compose file controls host
publication only. It is not a native server option. Seed input, durable storage,
listener addresses and advertised URLs must retain their distinct meanings.

## Configuration compatibility and extensions

Keep server-wide settings in CLI options, environment variables or the existing
server JSON file. Store CORS, IAM, labels and supported versioning settings per
bucket through the shared bucket API and catalog. Do not create per-bucket
environment-variable suffixes or overload a global public-host setting with
bucket permissions or CORS.

The `--buckets-file PATH` extension reads a version-1 JSON initialization file
listing multiple buckets and their individual settings. On startup, create only
missing buckets. Existing bucket settings and objects survive unchanged, even
when the file differs; removing an entry does not delete its bucket. This keeps
restart from undoing API changes. Changing an existing bucket remains an API
operation. Select it with `GCS_MIMIC_BUCKETS_FILE` or server JSON `buckets_file`;
CLI overrides environment, then config. See the [runnable example](container-usage.md#initialize-bucket-settings).

The initializer always uses native field validation and local administrative
authority, including in strict mode. It grants no subsequent API permissions.
It validates the whole file before one atomic create-missing batch, rejects
duplicate names, IAM policies, lifecycle and unknown fields, and retries without
overwriting committed buckets. Existing project mismatches warn without moving
ownership. This differs from API insertion and legacy response rendering;
the [contract](slices/bucket-initialization.md) specifies limits and supported fields.

## Profiles and conflicting behavior

Keep Google-contract/strict behavior and legacy emulator compatibility distinct.
The accepted option `--compat-profile legacy-emulator` implements the limited
bucket baseline above. The broader `fake-gcs-server compatibility profile`
remains a design target; freeze additional behavior with each affected slice.
The existing `auth_mode=permissive` is not already a complete legacy profile.

- A compatibility corpus reproduces the pinned emulator's observable contract,
  including its tested permissive URL, metadata and error behavior.
- A strict corpus checks local identities, effective permissions and valid
  signatures against the Google contract. Legacy signature acceptance must not
  leak into strict requests.
- Existing native seeding remains one-time and does not replay over mutations.
  Source-compatible seed/reseed behavior must be selected explicitly; durable
  migration must never silently replay fixtures over imported changes.
- Resource limits and unsupported settings remain visible. A recognized setting
  must not be accepted with no effect merely to pass a startup test.

Record contract conflicts and their profile-specific expectations. Passing a
legacy case is not Google `EXACT` fidelity. New strict restrictions cannot be
silently imposed on applications selecting the legacy contract. An unresolved
required conflict blocks the full compatibility claim.

## Editable directory requirement

Replacement also requires a visible bucket/object directory with bidirectional
synchronization: external additions, changes and deletions reach the API, and API
mutations appear in that directory. This is not implemented. Native seed import
and the planned one-time source converter do not satisfy this requirement.

SQLite may remain the metadata/index catalog. A synchronized working tree can
feed the existing bounded publication engine while preserving immutable internal
blobs. Conflict behavior, synchronization delay and unusual-name representation
must be settled before implementation. The planning contract is
`delivery_plan.migration.filesystem_sync_contract` in the feature ledger.
See the [source investigation and executed file-edit cases](fake-gcs-server-storage.md).
Safe reconciliation must update hashes and generations; copying the source's
stale-metadata behavior is not a sufficient editing contract.

## Data movement without coupling internal formats

Support source readers that produce typed bucket/object/version records and
bounded body streams for the shared catalog/blob import path. Keep source
parsing outside JSON/XML handlers and catalog internals. Do not add a generic
storage-engine abstraction just to read a foreign format.

Three source formats must remain distinct:

1. **Seed directory:** the source loader treats regular files below bucket
   directories as object content and derives MIME types from extensions. Native
   gcs-mimic seed input currently uses octet-stream. Do not claim the existing
   seed implementation already preserves fake-gcs-server metadata/defaults.
2. **Persistent filesystem:** decode the source's bucket metadata, object
   attributes and xattr/sidecar selection from the selected source format.
   A sidecar is metadata here; do not reuse the seed loader's regular-file rules.
3. **Live memory/API export:** quiesce source writes while it stays alive, export
   live and archived generations using supported APIs or the Go adapter, then
   import the bounded manifest/body stream. Stopping first loses memory state.

The destination is always a different data directory. Mount offline source
snapshots read-only, preserve xattrs in backups when they are used, and never let
old and new servers write the same storage root. A copied directory without its
xattrs is not automatically a complete export. Internal filesystem layout
compatibility is not required to preserve application behavior.

### Preflight and preservation requirements

- Record source version/backend, configuration, selected profile, stable export
  identity and explicit project/identity mappings. Do not invent missing project
  ownership or silently make a private resource public.
- Dry-run the full manifest and report unsupported metadata, malformed records,
  missing attributes, name/path collisions and corrupt/missing bodies. Do not
  skip failures or report partial copy as successful migration.
- Preserve raw logical names, bytes, MIME/content headers, custom metadata,
  bucket settings and ACL/retention/version state that the source exposes.
  Preserve source-visible generation, timestamp and ETag semantics: assigning
  fresh values must not silently invalidate stored generation URLs or conditions.
- Store imported identity separately from opaque blob paths, and advance future
  generation allocation so imported identities cannot be reused. If the current
  representation cannot retain a value or its precision, block and report it.
- Stream and independently verify lengths and checksums. Report inconsistent
  source hashes instead of silently replacing metadata or bypassing validation.
- Persist an import journal with bounded cursors and source fingerprints. Retry
  only the same export; never overwrite unrelated destination objects. Changed
  inputs, conflicts, disk-full and interruption require explicit outcomes.
- Publish the imported dataset only after validation and readiness checks.
  Reuse the destination's existing blob/catalog recovery rules; import itself
  does not make a cross-file transaction atomic.

Unsupported advanced attributes or archived versions stay blocking findings
until their semantics exist. An early live-object-only importer can have its
own declared scope, but it cannot be called lossless full migration.

### Cutover and rollback

Quiesce mutations, complete or cancel uploads and export a stable source. Do not
assume old resumable session URLs or page tokens will survive replacement.
Either preserve and test a selected token contract or require a documented drain
and restart boundary before cutover. Count resources/generations and compare
bytes, hashes, metadata, permissions and old generation-qualified URLs.

Switch the endpoint only after verification. Retain the original snapshot and
configuration, and test reopening/restoring them. Switching back to an old
snapshot after new writes would lose those writes; rollback must export/replay
the changes or remain inside an explicitly write-frozen verification window.
Do not automatically delete the source or claim bidirectional live migration.

## Delivery and full-superset gate

The migration track complements the capability queue; it does not mark an entire
phase complete or bundle every missing feature into one implementation turn.

| Milestone | Completion evidence |
| --- | --- |
| Pinned compatibility baseline | Versioned route/flag/Go/backend inventories and reproducible positive/negative corpus with every known gap visible |
| Seed compatibility | Same fixtures produce the required keys/bytes/defaults and selected lifecycle in both profiles |
| Persistent import | xattr and sidecar fixtures, identity/metadata preservation, dry-run, restart/resume and nondestructive rollback cases |
| Go and live-state migration | Export before shutdown, archived-version preservation and unchanged representative Go tests through the adapter |
| Full replacement validation | Every baseline surface passes, including TLS, legacy gRPC, helpers/events and memory behavior; remaining exclusions prevent the full claim |

Normalize only documented nondeterministic fields, such as a chosen ephemeral
port or an unconstrained newly generated request identifier. Do not normalize
away missing fields, errors, permissions, imported generations or wrong bytes.
Run both source backends, relevant official SDKs and representative existing
application tests. Review applicable upstream tests with their licenses intact.

A partial developer release remains possible and must identify its supported
migration scope. The phrase "fully backward-compatible superset" becomes a
release claim only after the complete pinned baseline and preserved-data checks
pass. Additional APIs, strict authorization and resource/performance results are
reported separately with their own evidence. New upstream releases expand the
baseline explicitly rather than silently changing the denominator.

Product **1.0.0** additionally requires the fixed extension scopes and stable
public contract in the [release policy](release-policy.md#the-100-gate).
Earlier useful increments ship as `0.x` releases without a full-superset claim.

## Sources consulted

Reviewed on **2026-10-22**. Source inspection does not mean the source tests,
importer or compatibility corpus ran.

| Source | Purpose |
| --- | --- |
| fake-gcs-server README and Dockerfile | Startup, URL behavior and fixture entrypoint |
| `internal/config/config.go` | Flags and environment settings |
| `fakestorage/server.go` | Seed loading, routes, constructors and lifecycle |
| `fakestorage/config.go` | Mutable internal configuration |
| `fakestorage/object.go` | Direct Go object APIs |
| `internal/backend/object.go` | Persisted attributes and identities |
| `internal/grpc/server.go` | Legacy Storage v1 contract |
| `docs/en/storage-backends.md` | Persistence alternatives and recovery tradeoffs |
