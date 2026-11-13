# fake-gcs-server migration and compatibility target

[日本語](../ja/fake-gcs-server-migration.md)

This is the full reference. To decide whether to move and to make the swap, start with [Move from fake-gcs-server](migrate-from-fake-gcs-server.md).

Published container examples below use **0.20.0**. Since 0.8.0 the release includes separately scoped migration adapters and protocols; 0.7.0 and earlier lack them. The [integration acceptance map](evidence/migration-integration.md) distinguishes native wire proof, prepared client harnesses and unexecuted final SDK/container/browser gates. Full replacement remains an unmet product gate.

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
      - "127.0.0.1:${GCSM_PORT:-4443}:4443"
    volumes:
      - ./data:/data:ro
      - source-data:/storage
volumes:
  source-data:
```

Five common option groups translate as follows. Since 0.18.0, source flags
are accepted when their translations preserve working requests;
inexact flags stop startup with a native alternative. Since 0.15.0,
`FAKE_GCS_*` variables are not read. This walkthrough uses native names. See
[option aliases](slices/option-aliases.md).

| Before | After in this walkthrough | Why |
| --- | --- | --- |
| `-scheme http` | Nothing | The default `auto` serves HTTP and HTTPS on the same port, so existing HTTP clients connect, and the image healthcheck probes HTTP without an override. Up to 0.18.0 the default was HTTPS-only and needed `GCS_MIMIC_SCHEME: http`. |
| `-host 0.0.0.0 -port 4443` | Nothing | The image already listens on `0.0.0.0:4443`, which peers need; a loopback-only listener is unreachable from other containers. |
| `-public-host gcs:4443 -external-url http://gcs:4443` | Nothing | Links that the server returns, including resumable Location, follow the Host of each request, so container SDKs get `gcs:4443` and the host gets its published port. Native path-style reads need no public-host setting; this does not reproduce legacy virtual-host matching or grant permissions. |
| `-backend filesystem -filesystem-root /storage` | Named volume at `/var/lib/gcs-mimic` | `filesystem` selects native `disk`; `filesystem-root` is refused. Keep the source volume separate and import a stopped snapshot into a new destination. Native `--backend memory` is also available. |
| `-data /data` with empty bucket directories | `GCS_MIMIC_BUCKETS_FILE: /buckets.json` and a read-only file mount, or the same document inline in `GCS_MIMIC_BUCKETS` ([below](#bucket-file-inside-composeyaml)) | A `./data:/data:ro` mount alone replays seed on every startup, including empty directories; the bucket document is the way to give buckets CORS, versioning or IAM policy settings. |

The Ruby SDK's ordinary high-level calls now accept no-op wire defaults:

| Ruby call | gcs-mimic behavior |
| --- | --- |
| `create_file(StringIO, extensionless_name)` without content_type | Empty upload MIME is treated as absent; contentType defaults to application/octet-stream. Nonempty invalid MIME still returns 400. |
| `create_bucket(name)` without optional arguments | Disabled, empty or null Autoclass is treated as absent, with no Autoclass response property. `autoclass_enabled: true` creates an [Autoclass bucket](slices/bucket-autoclass-storage-class.md); malformed Autoclass settings return 400 invalidArgument. |
| `compose(sources, name)` without optional arguments | Null deletion/destination uses ordinary non-destructive composition and destination defaults. Explicit true retains hard source deletion. |

These are the scoped [Ruby defaults](slices/sdk-ruby-defaults.md), not proof
of provider null-input parity. No extra server setting is
required.

| Listing call | gcs-mimic behavior |
| --- | --- |
| Go `Query{MatchGlob: "**/*.txt"}` or Ruby `bucket.files(match_glob: "**/*.txt")` | JSON listing filters names and slash-delimited prefixes, including pagination and versions. Empty matchGlob is absent. Invalid syntax or an incompatible delimiter returns 400; use delimiter `/` or omit it. See the [glob contract](slices/sdk-list-glob.md). |

| Storage v2 gRPC object call | gcs-mimic behavior |
| --- | --- |
| Go `storage.NewGRPCClient(ctx)` Writer, NewReader, NewRangeReader, Attrs, Delete and Objects | Set `STORAGE_EMULATOR_HOST_GRPC` to the separately configured h2c listener, for example `127.0.0.1:4444`. Default Writer chunk size works for small and multi-MiB objects, and JSON/gRPC share data, generations, permissions, holds and retention. Prefix/delimiter/page tokens and resumable upload offsets/checksums are supported, and CancelResumableWrite discards a session. `Writer.Append`, `NewWriterFromAppendableObject` and `FinalizeOnClose` work with documented limits; see the [Rapid append contract](slices/grpc-v2-rapid-append.md). Non-final non-resumable flush/state lookup and unsupported fields return UNIMPLEMENTED; see the [object I/O contract](slices/grpc-v2-object-io.md). This is Storage v2, not the reference emulator's legacy v1 service. |

| Object metadata call | gcs-mimic behavior |
| --- | --- |
| Go Writer/Update TemporaryHold; Ruby set_temporary_hold! / release_temporary_hold! | The flag persists across restart and blocks deletion and upload/copy/rewrite/compose replacement with 403 `forbidden` (XML `ObjectUnderActiveHold`) until released. A released hold reads `false`; a hold that was never set is omitted. Metadata remains editable. |
| Go Writer/Update CustomTime; Ruby create_file(custom_time:) / custom_time= | RFC 3339 timestamps persist; equal/later updates succeed. Null or omission (PATCH or PUT) keeps the current value; an earlier time returns 400 `invalid` ("Custom time cannot be decreased…"). |

The [object holds/custom time contract](slices/sdk-object-holds-customtime.md)
records null defaults and local error details.

| Bucket metadata call | gcs-mimic behavior |
| --- | --- |
| Ruby `bucket.update { \|b\| b.public_access_prevention = "enforced" }` / JSON bucket PUT | In the native profile PUT replaces the resource, as recorded from Cloud Storage: omitted labels, CORS, lifecycle, versioning, retention policy (a locked one stays) and defaultEventBasedHold are cleared; the storage class, soft delete policy and access settings are kept. Ruby update blocks send PUT, so they lose omitted settings; use direct setters (PATCH) instead. `--compat-profile fake-gcs-server` keeps the earlier omission-preserving behavior. See the [PUT contract](slices/bucket-metadata-put.md). |

| Soft-delete call | gcs-mimic behavior |
| --- | --- |
| Go `BucketAttrs.SoftDeletePolicy` / Ruby `bucket.soft_delete_policy=` | JSON insert/PATCH/PUT accept 0 or 1–3155760000 retention seconds. Soft delete is off by default on every backend, as in fake-gcs-server: omission at creation stores no policy, and **an overwrite or delete frees its bytes at once**. To keep them, send `softDeletePolicy.retentionDurationSeconds`, or start the server with `--default-soft-delete-retention-seconds N` (or `GCS_MIMIC_DEFAULT_SOFT_DELETE_RETENTION_SECONDS=N`). This default and range are local: Cloud Storage defaults to seven days and accepts 0 or 7 to 90 days, so a test of its rejection below seven days cannot run here. Buckets that store a policy keep it, including the seven days that 0.11.0 and earlier gave new buckets; omission on PATCH/PUT preserves the current policy. `--compat-profile fake-gcs-server` still applies no default. |
| Go `Query.SoftDeleted`, `Generation(g).SoftDeleted().Attrs`, `Restore` / Ruby `files(soft_deleted: true)`, `file(name, generation:, soft_deleted: true)`, `restore_file` | Deleted/overwritten generations retain bytes until hardDeleteTime; ordinary reads hide them. A bucket with no stored policy and one with an explicit zero both allow listing (200, empty unless retained generations remain); missing generations return 404 on get/restore. This differs from Google's absent-policy errors. Restore creates a new live generation and keeps the original retained generation. Holds and retention still protect deletion/replacement. |
| Delete a bucket containing only soft-deleted generations | With a positive policy the bucket becomes soft-deleted with its retained generations and can be restored by generation (`buckets.restore`); its objects stay soft-deleted until restored separately. Without a policy it is removed permanently. Buckets created from a seed directory or buckets file get the server default, and a buckets file entry can set its own `softDeletePolicy`; see the [object](slices/sdk-soft-delete-restore.md) and [bucket](slices/bucket-soft-delete-restore.md) soft-delete contracts. |

To restore objects from an existing `bucket`, enable a policy before deleting
them; a retention of seven days matches Cloud Storage's default. These
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

Keeping the `./data:/data:ro` mount alone also works: the image seeds it on every
startup, so empty directories, including ones added later, become buckets. This
walkthrough removes that mount and uses the file instead, which can also carry
bucket settings such as CORS, versioning and an IAM policy. Restart creates
only missing entries, preserves existing objects and API settings, and does not
delete buckets removed from the file. Add future bucket names to this file and
restart. Object fixtures use a separate [seed directory](container-usage.md#load-local-files).

Replace the `gcs` service with the published image and keep your application
services unchanged. Place `buckets.json` beside the Compose file; the mount
source is relative to it. The example file is `compose.migration.yaml`; in an
existing app, edit the service in your own Compose file instead.

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.20.0
    ports:
      - "127.0.0.1:${GCSM_PORT:-4443}:4443"
    environment:
      GCS_MIMIC_BUCKETS_FILE: /buckets.json
    volumes:
      - migration-data:/var/lib/gcs-mimic
      - ./buckets.json:/buckets.json:ro
volumes:
  migration-data:
```

Connection limits, object size, timeouts and soft delete need no settings for this. Links
follow the Host of each request and the scheme of its connection, so container SDKs get
`gcs:4443` while the host/browser gets `localhost` and the published port, from one server.
Set `GCS_MIMIC_API_BASE_URL` only for one fixed origin: a reverse proxy that rewrites
Host needs it, because forwarded headers are ignored. With 0.11.0 and earlier, whose
links are always fixed, add `GCS_MIMIC_API_BASE_URL: http://gcs:4443` here. The image
healthcheck needs no override. App
services can wait with `depends_on: {gcs: {condition: service_healthy}}`.

Use a distinct Compose project and host port per worktree; the internal port
stays 4443. Run these commands beside the files:

```sh
export COMPOSE_FILE=compose.migration.yaml
export COMPOSE_PROJECT_NAME=assets-migration
export GCSM_PORT=14443
docker compose up -d --wait
docker compose exec -T gcs gcs-mimic healthcheck
api="http://localhost:${GCSM_PORT}"
curl --fail "$api/storage/v1/b/assets"
curl --fail "$api/storage/v1/b/exports"
curl --fail -H 'Content-Type: application/json' -d '{"name":"scratch-assets"}' \
  "$api/storage/v1/b?project=local-project"
curl --fail --data-binary 'hello' \
  "$api/upload/storage/v1/b/assets/o?uploadType=media&name=hello.txt"
test "$(curl --fail "$api/storage/v1/b/assets/o/hello.txt?alt=media")" = hello
```

For a second worktree use, for example, project `assets-migration-two` and
`GCSM_PORT=24443`; project-scoped volumes keep state separate. Stop with
`docker compose down` using the same file/project. The named volume retains data.

### Bucket file inside compose.yaml

The bucket document can live in the Compose file itself, so no separate
`buckets.json` is needed. Write it into the service's environment as
`GCS_MIMIC_BUCKETS`, in place of `GCS_MIMIC_BUCKETS_FILE` and the `buckets.json`
mount; the two settings cannot be combined. This is the recommended form, and
it needs 0.14.0 or later:

```yaml
services:
  gcs:
    environment:
      GCS_MIMIC_BUCKETS: |
        {"version": 1, "buckets": [
          {"name": "app-images", "cors": [{"origin": ["http://localhost:${FRONTEND_PORT:-8081}"],
            "method": ["GET", "HEAD", "PUT"], "responseHeader": ["Content-Type"], "maxAgeSeconds": 3600}],
           "iamPolicy": {"bindings": [{"role": "roles/storage.objectViewer", "members": ["allUsers"]}]}},
          {"name": "app-exports"}
        ]}
```

Compose, not gcs-mimic, expands `${FRONTEND_PORT:-8081}`, following its
[interpolation rules](https://docs.docker.com/reference/compose-file/interpolation/);
gcs-mimic reads the result as plain JSON and expands nothing. Write a literal `$`
as `$$`. `iamPolicy` gives a new bucket its policy, replacing the default one as
`setIamPolicy` does; releases up to 0.13.0 reject it as unsupported.

The named volume outlives `docker compose down`, so its buckets already exist on
the next start. The document reconciles them: `GCS_MIMIC_BUCKETS_MODE` defaults
to `reconcile`, so each start writes the fields an entry sets whose value differs,
fields it omits keep their API changes, and ready counts the changed buckets in
`buckets.reconciled`. Set the mode to `create` to keep
existing buckets as they are. The value is part of the service's environment, so
after an edit a plain `docker compose up -d` recreates the container and the next
start applies it; `docker compose restart` keeps the old value. Deleting a field
never resets it: removing the `iamPolicy` above leaves `app-images` public, with
a one-time warning that `iamPolicy` is no longer written, until the entry writes
the policy you want.

Alternatively, a top-level `configs:` entry with inline `content:` can hold the
file that `GCS_MIMIC_BUCKETS_FILE` names; the service's `configs:` mounts it at
`target`. Inline `content` needs Docker Compose 2.23.1 or later, according to the
[Compose configs reference](https://docs.docker.com/reference/compose-file/configs/),
and a service without `read_only: true`: Compose copies the content into the
container and refuses a read-only one. A bucket file reconciles existing buckets
the same way as the inline document:

```yaml
services:
  gcs:
    environment:
      GCS_MIMIC_BUCKETS_FILE: /etc/gcs-mimic/buckets.json
    configs:
      - source: gcs_buckets
        target: /etc/gcs-mimic/buckets.json
configs:
  gcs_buckets:
    content: |
      {"version": 1, "buckets": [{"name": "app-images"}, {"name": "app-exports"}]}
```

The server reads the edited `content` only in a recreated container: with Docker
Compose 5.6.1 a plain `docker compose up -d` keeps the running container and
`docker compose restart` keeps the old content, so run
`docker compose up -d --force-recreate gcs`.

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
returns bytes in this permissive profile; XML cross-origin fetch follows the
bucket's CORS rules, or the fallback below for a bucket without them. CORS never
grants access in strict mode. See
[Google's CORS distinction](https://docs.cloud.google.com/storage/docs/cross-origin).

fake-gcs-server answers every route with one wildcard CORS policy, so a page
that fetches path-style public URLs such as `$api/assets/browser.txt` worked
without configuration. In the default permissive profile it still does:
`GCS_MIMIC_CORS_FALLBACK` defaults to `allow-all`, which allows any origin on the
XML and public routes of buckets without stored rules. The first such
cross-origin request for each bucket logs one `cors_fallback_default` event at
info level, so you can see which buckets would need rules. Buckets with rules
always follow them, and the fallback never changes authorization. Strict mode defaults to `none`, as
Cloud Storage behaves: a browser fetch fails until the bucket has `cors` rules,
which the [strict local recipe](../../examples/strict-local/README.md) writes for
each bucket. See [CORS fallback](slices/cors-fallback.md).

A resumable session started from the browser address gets a Location on that address.
A session URI also works through any other address of the server, so a server that
starts a session through `gcs:4443` can hand the browser the same path and query on the
browser-reachable authority. The scenario verifies changed-Host PUT with Content-Range and a preflight.
Cancellation returns a **499** `clientClosedRequest` JSON envelope, and later
session access repeats it, as confirmed by the 2026-11-02 live recording of the
[JSON resumable protocol](https://docs.cloud.google.com/storage/docs/performing-resumable-uploads).
Do not treat 499 here as an infrastructure failure or retry cancellation as an
upload. This example checks the status explicitly instead of using curl `--fail`:

```sh
location=$(curl --fail -sS -D - -o /dev/null \
  -H 'Content-Type: application/json' -H 'X-Upload-Content-Type: text/plain' \
  -d '{}' "$api/upload/storage/v1/b/assets/o?uploadType=resumable&name=cancelled.txt" \
  | awk 'tolower($1)=="location:" {sub("\r$", "", $2); print $2}')
cancellation_status=$(curl -sS -o /dev/null -w '%{http_code}' -X DELETE "$location")
test "$cancellation_status" = 499
```

Authority rewriting is scoped to unsigned permissive sessions. Never
rewrite a strict signed URL after signing; sign the final recipient-visible URL.

### Which setup do I need?

| Goal | Setup | Settings |
| --- | --- | --- |
| Behave like fake-gcs-server: every request is allowed, no signature is checked and browsers read every bucket | The default permissive profile | `GCS_MIMIC_SCHEME=http` |
| Production-like access: private buckets stay private, public ones open through `allUsers`, browsers use signed URLs, application servers keep their own Cloud Storage calls | Strict mode with a trusted listener: the [strict local recipe](../../examples/strict-local/README.md) | `GCS_MIMIC_SCHEME=http`, `GCS_MIMIC_AUTH_MODE=strict`, `GCS_MIMIC_TRUSTED_LISTEN=0.0.0.0:4444` (never published) and `GCS_MIMIC_BUCKETS` with each bucket's policy and CORS rules, plus `GCS_MIMIC_TEST_SIGNER=enabled` when the application signs URLs or, like Rails, uses the `test-signer credentials` key |
| Only while strict mode is not possible yet: check signed URLs, allow everything else | Permissive with signature verification | `GCS_MIMIC_SCHEME=http` and `GCS_MIMIC_SIGNED_URLS=verify` |

Start with the default, which this walkthrough uses. For production-like checks,
use strict mode with the trusted listener, as the strict local recipe sets it
up: the container smoke test runs that exact block with the image, and its README
explains each line and the defaults it relies on. Permissive `verify` is only a
step for setups that cannot use strict mode yet: it rejects a bad signature, but
it grants every verified and every unsigned request, so it cannot show that a
private bucket stays private. The last two rows need 0.14.0 or later.
[Container usage](container-usage.md#how-the-setups-differ-from-fake-gcs-server)
compares the two profiles with fake-gcs-server behavior by behavior.

### Strict mode without changing application servers

fake-gcs-server checks no permissions, so application servers that move to
strict mode would otherwise need credentials in every client: anonymous requests
then reach only `allUsers` grants. The
[strict local recipe](../../examples/strict-local/README.md) avoids that change;
paste its `gcs` service in place of the one above. Its application services use
`STORAGE_EMULATOR_HOST=gcs:4444`, a trusted listener on the Compose network where
requests without credentials act as the built-in `trusted-app@gcs-mimic.invalid`,
with `roles/storage.admin` on the buckets' project, while browsers use the
published port: public buckets through `allUsers`, private ones only through
signed URLs. It needs no identity document, certificate or generated key. The
[full application example](../../examples/full-stack/README.md) shows a Go API,
Rails and a frontend wired to it. Never publish the trusted
port. The [trusted listener section](container-usage.md#keep-private-buckets-private-with-a-trusted-listener)
explains its checks: requests with credentials authenticate as on the main
listener, browser-initiated requests get 403, and a Host other than an IP
literal, a `localhost` name, a single-label name such as `gcs` or a name in
`GCS_MIMIC_TRUSTED_ALLOWED_HOSTS` gets 421.

This covers only the requests the servers send themselves. fake-gcs-server checks
no signature, so an application may leave the URLs it hands to browsers unsigned
against the emulator; in strict mode those requests get 401, and permissive
`GCS_MIMIC_SIGNED_URLS=verify` allows them because they carry no signature to
check. The recipe's
[list of what the trusted listener does not cover](../../examples/strict-local/README.md#what-the-trusted-listener-does-not-cover)
names the changes these applications need.

### Rails Active Storage

The data `endpoint` alone does not redirect OAuth: googleauth 1.17.6 requests
a token from Google for service-account credentials in the default universe,
and ignores `token_uri`. Add a `gcs_emulator` entry with a non-Google
`universe_domain` to `config/storage.yml` instead; googleauth then signs its
own token and never contacts Google. Select it in development and test, and
keep the production entry for production. See the
[Rails recipe](rails-active-storage.md) and its
[verification](slices/sdk-active-storage-scenario.md#storageyml-emulator-recipe).

An earlier recipe built the service in application code with a preset token
and a one-day expiry. It is replaced: after the expiry, googleauth tried to
refresh the token at Google. The entry, with its key generated at boot, works
only with the permissive profile. A trusted listener does not change that: the
entry always sends its own token, which authenticates as on the main listener,
so strict mode needs a signer the server knows. The strict local recipe enables
the built-in test signer for this, and its README shows the entry that uses it.

### Migration troubleshooting

| Symptom | Cause | Repair |
| --- | --- | --- |
| A `FAKE_GCS_*` variable has no effect | Since 0.15.0 they are not read | Set the matching flag or `GCS_MIMIC_*` variable. |
| Startup rejects `-scheme` | A [withdrawn release](release-policy.md#withdrawn-releases) (0.6.0 or earlier) does not translate source names | Upgrade to the current release, or remove the old command/key and set `GCS_MIMIC_SCHEME: http`. |
| Startup rejects `-event.pubsub-topic` or another `-event.*` flag | 0.7.0 and earlier lack delivery; since 0.8.0 event flags map to native settings and require complete local publisher configuration | Upgrade to 0.8.0 or later and supply project/topic and an explicit local Pub/Sub emulator endpoint; provision its topic/subscriptions separately. See the [notification contract](slices/pubsub-notification-delivery.md). |
| SDK initiation succeeds but its next request cannot connect | Location contains an address fixed by `GCS_MIMIC_API_BASE_URL`, or a proxy rewrote Host | Remove the fixed address so links follow each request, or set one every client can reach, and recreate the service. |
| Browser cannot resolve `gcs` | Container DNS is not host DNS | Use `http://localhost:14443` for the example browser origin; preserve session path/query when a server hands an unsigned session URI to a browser. |
| A newly added empty directory creates no bucket | A [withdrawn release](release-policy.md#withdrawn-releases) (0.2.1 or earlier) imported seed files once | Upgrade to the current release with an empty data volume; it replays seed on every startup. Data from withdrawn releases is unsupported. |
| HTTP connection fails or an inherited probe is unhealthy | Scheme/listener differs from the client or probe environment | Leave `GCS_MIMIC_SCHEME` unset, so the default `auto` serves HTTP, or set it to `http`, and leave the image's `GCS_MIMIC_LISTEN` default of `0.0.0.0:4443`; let the image's probe inherit them. Up to 0.18.0 the default served only HTTPS. |
| Bucket listing returns 400 `required` ("Required parameter: project") | Native requires an explicit project | Request `/storage/v1/b?project=local-project`. The fake-gcs-server profile defaults omitted project to seed_project. |
| Docker reports the host port is in use | Another worktree publishes the same port | Set `GCSM_PORT=24443` and a different Compose project; update browser URLs. Peers keep using `http://gcs:4443`. |
| `curl --fail` reports 499 on session DELETE | Successful JSON resumable cancellation uses 499 | Inspect `%{http_code}` as above and accept 499 for cancellation only. |
| A request never finishes, a client's connections stay in use, and the server logs nothing | The client announced a request body (`Content-Length` or chunked) and stopped sending it. With the default `body_inactivity_timeout_ms` of 0 no limit expires, so the server keeps waiting and has nothing to log | To find such clients, set `GCS_MIMIC_BODY_INACTIVITY_TIMEOUT_MS`, for example to `30000`. A body silent that long then fails with JSON 408 `uploadBrokenConnection` or XML 400 `RequestTimeout`, and the server logs a `connection_timeout` warning. The default stays 0 so a slow client is never cut off. |

## Goal and current state

The migration goal is to preserve existing application workflows while adding
stricter Google-contract validation, broader capabilities and resource controls.
CLI spelling, configuration structure and internal storage need not be identical
to fake-gcs-server. Document equivalent uses and explicit differences; the broader
full-superset gate below tracks remaining functional coverage.
This is a target, not a current compatibility claim. Filesystem import and
quiesced memory capture now have separate contracts. Go integration uses a
Rust subprocess facade with a supported exported subset; literal in-process
embedding and the complete source Go API remain unsupported.
The existing `fake-gcs-server` profile covers a limited bucket contract; it does
not implement the complete migration profile described here.

gcs-mimic serves path-style and opt-in virtual-host XML GET/HEAD/PUT, strict
V4 RSA/HMAC signed transfers, V2 RSA/HMAC GET/PUT and scoped public access
prevention, with
[recorded evidence](evidence/signed-urls.md). URL construction and authorization
remain separate. Since 0.8.0, XML listing, copying and multipart are served
with explicit field/resource bounds. Broader XML fields, ACL/UBLA interactions,
provider error parity and complete endpoint/configuration equivalence remain
gaps; see the [acceptance map](evidence/migration-integration.md).

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

API uploads have no separate concurrency quota. `max_connections` (default 1024)
bounds active connections; excess connections wait. Catalog operations also wait for
admission instead of failing, and a client that disconnects gives up its place. Active
resumable sessions, unfinished rewrites, XML multipart uploads and derived transfers have
no count limit by default. Different part numbers of an XML multipart upload write at the
same time. Requests that arrive during a reset, reseed or seed commit wait and then run against
the state it left. Bodies retain 64 KiB chunks
and a depth-two channel, without collecting the whole object in memory.

A 503 remains possible in these cases:

- A request that outlasts `catalog_wait_timeout_ms` (default 120000), including one waiting
  for a busy session such as a resent XML multipart part number or a completion, or a handler
  that outlasts `request_timeout_ms` (default 180000). A pause while the client sends a request
  body never counts against `request_timeout_ms`; only `body_inactivity_timeout_ms`, disabled
  by default, bounds it, and answers JSON 408 `uploadBrokenConnection` or XML 400
  `RequestTimeout`.
- `max_resumable_sessions` or `max_rewrite_sessions` that you set.
- I/O failures and shutdown.

Shutdown waits for active uploads and cleanup within `shutdown_timeout_ms`.
Disconnecting before EOF aborts publication and cleans staging. New uploads
after catalog shutdown starts fail with a `catalog is closed` cause.
JSON object 503 messages also identify the catalog wait timeout, a busy session,
I/O failure or corruption when safely known; private backend details are hidden.
For wait timeouts, retry with generation preconditions or raise
`catalog_wait_timeout_ms`. A completed-body upload can still commit after
the response times out: read state before retrying an unconditional write.

Retry and notification records are windows, so a full one drops its oldest entry and the
write succeeds. A single-request upload or copy retried after its record is gone publishes
again, and an unreachable Pub/Sub loses its oldest undelivered events; see the
[replay contract](slices/sdk-upload-replay.md) and the
[notification contract](slices/pubsub-notification-delivery.md).

The [upload admission contract](slices/migrate-upload-admission.md) records
12/40-upload tests at 32 connections, 64-upload tests at 64 connections,
resumable initiation/final 128 KiB PUT at 40/64 uploads, concurrent GET/list checks
and shutdown/disconnect cleanup. This is scoped
emulator evidence, not a Google rate-limit or full migration parity claim.

## Bucket core baseline before further bucket extensions

The create-by-name, basic-resource GET and empty-bucket DELETE baseline is
delivered on **2026-10-22** with source-supported versioning/default hold
configuration and real object effects. Select `compat_profile=fake-gcs-server`
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
| Uniform bucket-level access / predefined ACL | Go/Ruby default UBLA creation, inspection and disabling work; aliases and the 90-day lock persist, including creation with enforced PAP | Under UBLA, remove predefinedAcl/predefinedDefaultObjectAcl/destinationPredefinedAcl parameters: they return 400. Without UBLA, upload predefinedAcl applies its canned object ACL ([evidence](evidence/upload-predefined-acl.md)); in the native profile bucket PATCH/PUT apply predefinedAcl/predefinedDefaultObjectAcl and rewrite applies destinationPredefinedAcl, while bucket insert still rejects them. ACL enforcement and initialization-file UBLA remain pending; [contract and evidence](slices/sdk-ubla-predefined-acl.md) |
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
| Bucket listing without project | Source allows `GET /storage/v1/b` without project | `fake-gcs-server` defaults to `seed_project`, as creation does; native returns 400 with `?project=local-project`. Explicit project validation, filtering and authorization remain active. |
| Lifecycle configuration | The pinned source ignores lifecycle settings at bucket creation and does not execute lifecycle rules | JSON insert/get/list/PATCH/PUT validate and persist the scoped rules, including ordinary Go/Ruby lifecycle calls. gcs-mimic stores configuration but never executes actions; XML lifecycle and initialization-file lifecycle remain unsupported. See the [lifecycle contract](slices/sdk-lifecycle-config.md). |
| Endpoints | Public/virtual-host and API routes, external upload URLs and mutable internal configuration | Preserve client-visible host/path/query behavior; map advertised addresses explicitly |
| HTTP semantics | JSON/XML, uploads, ranges, conditions, metadata, ACLs, batching and error shapes | Run the same client scenarios and negative cases against both implementations |
| Permissive signed URLs | Signature/expiry are not validated; callers can rewrite hosts | Permissive V2/V4 ignores signatures and expiry; V2 credential structure and numeric Expires are validated. Strict path-style V2 GET/PUT verifies registered RSA-SHA256 keys, expiry and signed headers with shared IAM permissions; URLs are used unchanged. V2 does not bind Host or response overrides. V2 HMAC query/header signatures and virtual-object subsets are accepted ([V2 HMAC contract](slices/xml-v2-hmac-signatures.md)); V2 HEAD/generation selection, virtual bucket queries, broad POST acceptance and resumable signing remain pending. See the [V2 contract](slices/sdk-active-storage-v2-signing.md). |
| Fixture preload | Docker mounts `/data`; the entrypoint passes `-data /data` | Import the seed format and reproduce the selected seed/reseed lifecycle |
| Durable data | Filesystem root with object files and xattr/sidecar metadata | Read-only, verified conversion into a separate destination |
| Memory backend | In-process bytes and active/archived versions | Optional ephemeral behavior and export before the source process stops |
| Go embedding | Constructors, server lifecycle, clients/handlers and direct object methods | A separately tested Go-compatible adapter; a Rust executable alone is insufficient |
| gRPC | v1.56.1 imports Storage **v1** protobufs | gcs-mimic optionally opens separate plaintext `grpc_listen` (for example `127.0.0.1:4444`); point Go `STORAGE_EMULATOR_HOST_GRPC` there. Storage v2 bucket CRUD, object reads/writes/deletes/listing and resumable uploads share JSON state, metadata validation and strict authorization. Scoped bucket/object updates, bucket IAM, retention locking, rewrite, compose, same-bucket move and soft-delete restore use that same shared state. BidiReadObject supports Go `NewMultiRangeDownloader` with `storage.WithGRPCBidiReads()`, CRC32C and generation-pinned read handles; handles retain the latest 64 initial selections within the process and require current authorization on reconnect. CancelResumableWrite discards a resumable session through the same catalog operation as the JSON session DELETE. Appendable objects support creation, flush, gRPC reads while appending, takeover and finalization ([Rapid append contract](slices/grpc-v2-rapid-append.md)). Unsupported fields and unknown RPCs return non-retryable `UNIMPLEMENTED` with ErrorInfo and JSON API guidance. The same h2c endpoint independently serves the reference's twelve legacy v1 bucket/object RPCs through shared state and strict permissions, with bounded multi-message uploads and atomic conditions. Legacy v1 bucket listing without a project spans every project, as in the reference (strict callers see only projects they may list), and objects support a documented metadata subset; it does not claim Google GA/EXACT compatibility. Other v1 methods, media reads and resumable uploads return the same explicit UNIMPLEMENTED. See the [legacy contract](slices/grpc-v1-legacy.md), [v2 bucket contract](slices/grpc-v2-buckets.md), [object I/O contract](slices/grpc-v2-object-io.md), [update/copy contract](slices/grpc-v2-update-copy.md), [bidi read contract](slices/grpc-v2-bidi-read.md) and [foundation contract](slices/grpc-v2-foundation.md). |
| Helpers/events | Internal config/reseed routes and notification/event settings | Inventory and test local behavior plus explicit external integration boundaries |

Ordinary endpoint-based SDK workflows can retain application calls within their
tested subset. Renaming settings, changing images and converting data are
deployment steps. Go facade adoption and wrappers that hard-code executable
names may require an explicit module or client adapter. This is not a guarantee
of unchanged application source or literal in-process Go library replacement.

## Observed GitHub uses and migration boundaries

These source files were inspected at the fixed revisions linked below. Their
applications were **not executed** against this checkout. They show concrete
uses to cover, rather than proving compatibility or every use on GitHub.
The migration reference remains upstream fake-gcs-server v1.56.1; a consumer's
older dependency or fork is not that reference's universal contract.

| Observed use | Migration obstacle | Current adaptation and boundary |
| --- | --- | --- |
| [gcsfuse fixtures](https://github.com/GoogleCloudPlatform/gcsfuse/blob/4fefe6330742a00f4706cf48cae01a15b4766e86/internal/storage/fake_storage_util.go): explicit generations, metadata, gzip bytes, Client/Stop | A seed directory alone loses fixture identity and metadata | Go constructor fixtures use dedicated bounded administration; explicit generations and content encoding are preserved within the declared fixture subset. |
| [Apache Beam Go filesystem tests](https://github.com/apache/beam/blob/e843314a31dad586cdb585a7f96a67499a448e97/sdks/go/pkg/beam/io/filesystem/gcs/gcs_test.go): NewServer/Client/Stop and filesystem operations | HTTP endpoints alone do not provide the exported Go constructors/lifecycle | Use the [subprocess facade](slices/go-subprocess-facade.md); its own integration harness covers selected SDK operations, not the full Beam suite. |
| [go-feature-flag helper](https://github.com/thomaspoignant/go-feature-flag/blob/d6ef445437b5c644e3acb4540e0d4d8646289fb4/testutils/gcs_mock.go): CreateObject with a supplied MD5 | Direct fixtures need validated body/hash publication | The facade forwards fixtures through shared integrity/condition/publication paths; unsupported identities or metadata fail explicitly. |
| [TiDB NoListener tests](https://github.com/pingcap/tidb/blob/49ad420278001b896de85fd7985b35ff49be877f/pkg/objstore/gcs_test.go): CreateBucketWithOpts, injected HTTPClient, reads/ranges and 1003-file walks | In-process transport and bucket helper methods differ from a standalone Rust binary | `NoListener` uses private Unix HTTP and no TCP listener. Bucket helpers forward all three source fields, preserving duplicate versioning/hold rules. This remains a subprocess and does not reproduce every backend API. |
| [Node Testcontainers module](https://github.com/testcontainers/testcontainers-node/blob/b99bb381be1c2470691a533937c23efd25389953/packages/modules/gcloud/src/cloudstorage-emulator-container.ts): hard-coded executable, readiness log and endpoint update | Changing only the image leaves the old entrypoint/readiness assumptions | Use the [generic-container adapters](slices/sdk-client-adapters.md): normal entrypoint, healthcheck, random mapped port and explicit opt-in externalUrl/publicHost update before resumable initiation. |
| [.NET FakeGcsServerBuilder](https://github.com/testcontainers/testcontainers-dotnet/blob/a6ce741af4dde9e8eb5f8c49bf2b90040b981187/src/Testcontainers.FakeGcsServer/FakeGcsServerBuilder.cs): shell script and fake-gcs-server binary | The sealed source-specific builder cannot use the distroless Rust image unchanged | The separate generic .NET adapter selects the native entrypoint and bounded RAM/HTTP configuration; it is not an image-only substitution. |
| [Aiven Java wrapper](https://github.com/Aiven-Open/testcontainers-fake-gcs-server/blob/ec7c605b593d648527f29b3df65cdfacda93985a/src/main/java/io/aiven/testcontainers/fakegcsserver/FakeGcsServerContainer.java): fixed binary and externalUrl update | Source-specific startup and management assumptions | Use the generic Java adapter and explicitly enabled management endpoint; this source is an Aiven wrapper, not a built-in Java Testcontainers module. |
| [Arrow object_store CI](https://github.com/apache/arrow-rs-object-store/blob/a0dc669ecf50aee37ec4f8e19dbd759f7028613f/.github/workflows/ci.yml): HTTP/memory/public-host against a fork | Fork behavior and XML/condition limitations differ from upstream and Google | The independent Arrow harness selects the local endpoint and tests XML operations. It remains separate from the official Rust SDK and from the fork's own excluded cases. |

Rails/Active Storage is an existing project verification priority. The linked
consumer inspection did not establish a representative Rails consumer source;
the project's Rails harness and official client sources supply that separate
evidence. A passing SDK scorecard is not proof that these consumer applications
or their source-specific wrappers pass unchanged.

## Startup options and current equivalents

The [complete environment reference](fake-gcs-server-configuration.md) lists all
19 source variables, defaults, CLI precedence and current native mappings.

`serve` accepts translations that preserve working requests and shows native equivalents
in one info diagnostic with a canonical `native_args` JSON array. A shell line
is included only for values without quotes, backslashes or control characters;
otherwise decode the array and pass each element as one argument.
It refuses inexact flags with one error per original key,
reports them together and exits with status 1 before startup.
`FAKE_GCS_*` variables are not read. See the
[complete classification](slices/option-aliases.md#behavior); use native names
for new configurations.

| fake-gcs-server option | Current gcs-mimic setting | Difference |
| --- | --- | --- |
| `-scheme http` / `https` / `both` | `--scheme`, `GCS_MIMIC_SCHEME` | Both uses primary HTTPS and a second HTTP listener with one advertised API origin |
| `-host`, `-port` | `--host`, `--port`, `--listen IP:PORT`, `GCS_MIMIC_LISTEN` | `--host` and `--port` keep the other part of the address; container port publication remains separate |
| `-port-http` | Native `--port-http`, `--listen-http IP:PORT`, `GCS_MIMIC_LISTEN_HTTP` | Requires `scheme=both`; default primary IP:8000. Other schemes fail instead of discarding the port |
| `-external-url` | Refused; choose `--api-base-url`, `GCS_MIMIC_API_BASE_URL` | Source fixes selfLink/mediaLink only; native fixes resumable URLs too. Choose an address every client can reach, or omit the fixed origin |
| `-public-host` | Accepted as `--public-base-url` with the effective scheme | Its subdomains select virtual-host buckets; JSON API callers must use another host or set `--api-base-url` explicitly. Existing explicit API-origin conflicts still fail |
| `-data` | `--seed-dir`, `GCS_MIMIC_SEED_DIR`; the image defaults to `/data` through `GCS_MIMIC_DEFAULT_SEED_DIR` | Applied before readiness on every startup. Exactly identical objects keep their generation. Since 0.8.0, seeding publishes the complete tree atomically, fails the entire replay on a differing protected object, and adds opt-in reseed; live synchronization uses a separate `--sync-dir`. 0.7.0 and earlier lack those additions. |
| `-filesystem-root` | Refused; choose `--data-dir` or `GCS_MIMIC_DATA_DIR` | Image path `/var/lib/gcs-mimic`; convert a stopped source snapshot with `gcs-mimic import fake-gcs-server` instead of reusing its format |
| `-cert-location`, `-private-key-location` | `--tls-cert`, `--tls-key` | Both PEM paths are required for custom TLS |
| `-log-level` | Native `--log-level`, `GCS_MIMIC_LOG_LEVEL`, JSON `log_level` | Minimum runtime stderr and informational diagnostic severity; validation warnings/errors and stdout lifecycle remain unfiltered |
| `-cors-headers` | Refused; remove it or explicitly choose `--cors-fallback allow-all` | The source's global header allowlist cannot be preserved. Permissive mode already allows every origin without bucket rules; strict mode follows bucket rules. Explicit fallback applies only to XML/public buckets without rules; stored rules and JSON behavior stay unchanged |
| `-location` | Refused; choose `--default-location`, `GCS_MIMIC_DEFAULT_LOCATION` or JSON `default_location` | The source controls a server-wide response location; native defaults affect only new buckets without an explicit location. Existing buckets and explicit locations win; the fake-gcs-server profile rejects the default |
| `-event.pubsub-project-id`, `-event.pubsub-topic`, `-event.bucket`, `-event.object-prefix`, `-event.list` | Native `--event-*` spellings | Accepted only with a configured local Pub/Sub emulator host. gcs-mimic never publishes to Google. Use native `--event-list none` to disable the global rule |
| `-backend memory` | `--backend memory`, `GCS_MIMIC_BACKEND=memory` since 0.8.0 | Process-local SQLite and leased chunks, with explicit aggregate storage budgets and empty restart; see the [RAM contract](slices/memory-backend.md). Disk remains default. |

The source's `-public-host` controls public URL construction and Host matching
on relevant public routes; it does not select the listener. A configured port
participates in its matching. `-external-url` controls the source's selfLink and
mediaLink bases; resumable `Location` follows the request address. Native
`api_base_url` also fixes resumable URLs, so `external-url` is refused.
Native virtual-host domains are DNS suffixes without a port; the actual
request authority still includes the port for strict signature checks.
Neither product's public-host/URL setting grants bucket permissions.
Unlike fake-gcs-server, public-host subdomains select virtual-host buckets
before JSON routing. Use a host outside those subdomains for JSON requests or
set `--api-base-url` explicitly; the accepted flag's info includes this advice.
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

Keep CLI > environment > server JSON > default precedence. On one command line,
a translated flag wins over a native option of the same setting and warns that
it overrides it. See the
[migration diagnostics](fake-gcs-server-configuration.md#migration-diagnostics).

`GCSM_PORT` in the repository's source-build Compose file controls host
publication only. It is not a native server option. Seed input, durable storage,
listener addresses and advertised URLs must retain their distinct meanings.

## Configuration compatibility and extensions

Keep server-wide settings in CLI options, environment variables or the existing
server JSON file. Store CORS, IAM, labels and supported versioning settings per
bucket through the shared bucket API and catalog. Do not create per-bucket
environment-variable suffixes or overload a global public-host setting with
bucket permissions or CORS.

The `--buckets-file PATH` extension reads a version-1 JSON initialization file
listing multiple buckets and their individual settings. On startup, create
missing buckets and apply each entry's written fields to existing ones; fields an
entry omits and objects survive unchanged, and removing an entry does not delete
its bucket. `buckets_mode=create` instead keeps existing buckets as they are, so
restart never undoes an API change, and warns on stderr about an edited entry it
did not apply. Select the file with `GCS_MIMIC_BUCKETS_FILE` or server JSON
`buckets_file`, and the mode with `GCS_MIMIC_BUCKETS_MODE` or `buckets_mode`; CLI
overrides environment, then config. See the
[runnable example](container-usage.md#initialize-bucket-settings).

The initializer always uses native field validation and local administrative
authority, including in strict mode. It grants no subsequent API permissions
beyond the `iamPolicy` an entry writes. It validates the whole file before one
atomic transaction, rejects duplicate names, policy fields other than
`bindings`, lifecycle and unknown fields, and retries without overwriting
committed buckets. Existing project mismatches warn without moving
ownership. This differs from API insertion and legacy response rendering;
the [contract](slices/bucket-initialization.md) specifies limits and supported fields.

## Profiles and conflicting behavior

Keep Google-contract/strict behavior and legacy emulator compatibility distinct.
The accepted option `--compat-profile fake-gcs-server` implements the limited
bucket baseline above. The broader `fake-gcs-server compatibility profile`
remains a design target; freeze additional behavior with each affected slice.
The existing `auth_mode=permissive` is not already a complete fake-gcs-server profile.

- A compatibility corpus reproduces the pinned emulator's observable contract,
  including its tested permissive URL, metadata and error behavior.
- A strict corpus checks local identities, effective permissions and valid
  signatures against the Google contract. Legacy signature acceptance must not
  leak into strict requests.
- Native seeding replays on every startup: an object
  that differs from its seed file in bytes or metadata is replaced as a new
  generation, so seed files win over API edits at restart; only an exactly
  identical object keeps its generation. Since 0.8.0, opt-in atomic reseed is
  available (disk merge, RAM replacement); a changed protected fixture rejects
  the complete replay. Durable migration must never silently replay fixtures over
  imported changes; do not mount seed files that overlap imported objects unless
  the seed file should win.
- Resource limits and unsupported settings remain visible. A recognized setting
  must not be accepted with no effect merely to pass a startup test.

Record contract conflicts and their profile-specific expectations. Passing a
legacy case is not Google `EXACT` fidelity. New strict restrictions cannot be
silently imposed on applications selecting the legacy contract. An unresolved
required conflict blocks the full compatibility claim.

## Editable directory requirement

Replacement also requires a visible bucket/object directory with bidirectional
synchronization: external additions, changes and deletions reach the API, and API
mutations appear in that directory. Since 0.8.0, gcs-mimic provides bounded
observation, shared create/edit ingestion, API write-back and revision-bound
deletion controls. Complete conflict/recovery/strict/platform acceptance is
still being integrated. Seed/import alone does not satisfy this requirement.

SQLite may remain the metadata/index catalog. A synchronized working tree can
feed the existing bounded publication engine while preserving immutable internal
blobs. Conflict behavior, synchronization delay and unusual-name representation
remain explicit in the [sync contract](slices/directory-synchronization.md).
Use the [live CLI](slices/sync-cli.md) for supported status/findings/deletion,
rescan, approval and fixed-fence waits without opening another catalog process.
The planning contract is
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
   gcs-mimic seed input derives MIME from a pinned extension table by default.
   Do not claim the existing
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

This gate, together with the fixed named additions, is a goal rather than a product version condition.
Product **1.0.0** freezes the public contract and states the supported migration scope as defined in the [release policy](release-policy.md#the-100-gate).
No release, before or after it, claims a full superset until this gate passes.

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
