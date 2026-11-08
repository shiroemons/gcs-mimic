# Use the Docker image

The image `ghcr.io/shiroemons/gcs-mimic:0.13.0` is published for `linux/amd64` and `linux/arm64`; pulling it needs no registry login. Docker Hub serves the same image digest as `shiroemons/gcs-mimic:0.13.0`, so either name works in the commands below. Pin this exact version in CI. To test unreleased source instead, build a local image from a checkout and use its tag in the commands below:

```sh
docker build -t gcs-mimic:local .
```

The [Compose migration walkthrough](fake-gcs-server-migration.md#replace-a-fake-gcs-server-compose-service)
replaces a fake-gcs-server service with the published image and initializes
empty buckets. It covers UX-012, DESIGN-061 and UX-001.

The [README quick start](../../README.md#quick-start) is the shortest start: one `docker run` serves HTTPS with a built-in development certificate, and `-e GCS_MIMIC_SCHEME=http` selects plain HTTP. List every option with `docker run --rm ghcr.io/shiroemons/gcs-mimic:0.13.0 --help`.

## Docker run

```sh
docker run -d --name gcs-mimic \
  -p 127.0.0.1:4443:4443 \
  -v gcs-mimic-data:/var/lib/gcs-mimic \
  -e GCS_MIMIC_SCHEME=http \
  ghcr.io/shiroemons/gcs-mimic:0.13.0
docker exec gcs-mimic gcs-mimic healthcheck
```

Once the readiness command succeeds, run the README's bucket and object commands.
The advertised API base is derived as `http://127.0.0.1:4443` because the host
and container ports are both 4443; see [Connect an application](#connect-an-application)
when they differ.
The image healthcheck follows `GCS_MIMIC_SCHEME`, `GCS_MIMIC_LISTEN` and the
JSON file selected by `GCS_MIMIC_CONFIG`; HTTP needs no probe override.
Wildcard listeners connect through loopback; `both` probes its HTTP listener.
HTTPS trusts the embedded development CA. Custom CAs require `--ca-cert PATH`.
When the server runs as PID 1, as in the image, and the probe derived from the
environment fails, the healthcheck retries with the server's command-line
options, including a Compose `command`. Elsewhere, use the same
environment/file settings for probes, or specify `healthcheck --url URL`.
The image supplies `GCS_MIMIC_LISTEN=0.0.0.0:4443` and
`GCS_MIMIC_DATA_DIR=/var/lib/gcs-mimic`; override these environment values to
change the bind port or persistent location. Environment values take precedence
over a JSON file, as they do in native startup.
It also sets `SQLITE_TMPDIR=/var/lib/gcs-mimic`, where SQLite spills the journal of a
large statement, because the root may be read-only. If you move the data directory
on a read-only root, set `SQLITE_TMPDIR` to the same place, or such a request answers
503 `disk I/O error`.
For example, `-e GCS_MIMIC_LISTEN=0.0.0.0:8080 -p 127.0.0.1:8080:8080`
changes the listener and published port. Use
`-e GCS_MIMIC_DATA_DIR=/custom-data -v gcs-mimic-data:/custom-data` to change
the persistent location; setting only `listen` or `data_dir` in JSON does not
override the image environment.
Stop with `docker stop gcs-mimic` and resume with `docker start gcs-mimic`.
The named volume keeps data when a stopped container is removed.

## Connect an application

| Caller | API endpoint |
| --- | --- |
| Host application or browser | `http://localhost:4443` |
| Application on the same Compose network | `http://gcs:4443` |

`localhost` inside a container refers to that container. Links that the API
returns, such as `Location`, `mediaLink` and `selfLink`, use the Host of each
request and the scheme of its connection, so a caller gets back the address it
connected to and the host and other containers can share one server, whichever
host port is published. You do not need `GCS_MIMIC_API_BASE_URL` for this.
Public and signed URLs must still use the address reachable by their recipient.
Go storage drops a non-default port when it signs a URL, so its signed URLs for
`localhost:4443` fail with 403 `SignatureDoesNotMatch`. If a default port is not
an option, for example because each worktree publishes its own port, set
`GCS_MIMIC_SIGNED_HOST_PORT=lenient` on a server that verifies signatures
(strict mode, or `GCS_MIMIC_SIGNED_URLS=verify`); see [signed URLs](signed-urls.md).

Set `GCS_MIMIC_API_BASE_URL` to fix one address for every caller. A reverse proxy
that rewrites Host needs it, because `Forwarded` and `X-Forwarded-*` are ignored.
The XML virtual host setup does not need it: a request to a virtual bucket host
such as `bucket.storage.example:4443` gets links on the configured domain with the
same port and the scheme of its connection (`storage.example:4443`), never with the
bucket label. Set it only if your DNS resolves the bucket names and not that domain.
An invalid or missing Host falls back to the actual
bound listener, with wildcard IPs replaced by loopback and port zero by the
allocated port, which is `http://127.0.0.1:4443` (or `https://`) in the image, and
the ready event reports it as `api_base_url`. The checked-in Compose files pass
`GCS_MIMIC_API_BASE_URL` through only when it is set in your environment.

## Load local files

Keep the bucket/object layout used by fake-gcs-server's `/data` mount:

```text
data/
  assets/
    logo.png
  uploads/
    example.txt
```

Mount it read-only at `/data`:

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  -e GCS_MIMIC_SCHEME=http \
  -v "$PWD/data:/data:ro" ghcr.io/shiroemons/gcs-mimic:0.13.0
curl 'http://localhost:4443/storage/v1/b?project=local-project'
```

In Compose, add `- ./data:/data:ro` to the service's existing `volumes` list.
No environment variable is needed.

### When the seed directory is used

The image sets `GCS_MIMIC_DEFAULT_SEED_DIR=/data` (`--default-seed-dir`, JSON
`default_seed_dir`; native default none). It applies only when `seed_dir` is
unset, the directory exists, and it does not overlap the data or sync directory;
otherwise it is silently ignored. An explicit `GCS_MIMIC_SEED_DIR`
(`--seed-dir`, JSON `seed_dir`) always wins, for example to read another path.

### What each startup does

Seeding runs on every startup, before readiness. Each top-level directory is a
bucket: it is created if missing, and an existing bucket keeps its settings.
Buckets belong to the seed project, `local-project` by default
(`GCS_MIMIC_SEED_PROJECT`); an existing bucket owned by another project stops
startup. Empty directories create empty buckets. Regular files directly under
the seed root, such as a `README.md` or `.gitkeep`, are skipped with a
`seed_skipped` warning, as fake-gcs-server ignores them; symbolic links and
invalid bucket names still stop startup.

Each file below a bucket directory is an object named by its relative path:

| Live object | Result |
| --- | --- |
| Absent | Created |
| Exactly what seeding would write: same bytes and seed Content-Type, with no Content-Encoding, Content-Disposition, Content-Language, Cache-Control, custom time or custom metadata | Left unchanged, keeping its generation; counted as `unchanged` |
| Any other difference, including metadata edited through the API | Replaced as a new live generation with seed default metadata; a versioned bucket keeps the previous one as noncurrent, and a soft delete policy keeps it soft-deleted |
| Different, but under an object hold or retention | The whole replay is rejected: no seeded change is applied and the server does not become ready |

Holds and storage class are not compared, so an identical held object counts as
`unchanged`. Since 0.8.0 seed replay is atomic; 0.7.0 and earlier skipped a
protected object and counted it as `blocked`. Release the hold or retention, or
align the fixture, before restarting.

Objects without a seed file are untouched, and removing a seed file never
deletes an object. An object deleted through the API is recreated on the next
restart, and edits to seed files apply on the next restart. Seed input is not
watched. Restart the container, or enable local administration
(`GCS_MIMIC_ADMIN_ENABLED=true`, on an isolated listener) and use the opt-in
[reseed endpoint](slices/atomic-seed-reseed.md). A reseed of a tree that breaks a
configured limit or holds names it cannot import answers 400 and names the problem;
only work a later attempt can finish answers 503 with `Retry-After: 1`.

Content-Type comes from the filename extension through a pinned table, for
example `.png` becomes `image/png`. Set `GCS_MIMIC_SEED_CONTENT_TYPE=octet-stream`
to store every file as `application/octet-stream`, the previous default. The
same setting applies to files ingested from a
[sync directory](#mirror-objects-to-a-host-directory). macOS metadata files such
as `.DS_Store` and `._*` are skipped.

The ready event reports the result in its `seed` field, for example
`{"outcome":"imported","buckets":1,"objects":2,"replaced":0,"unchanged":3,"blocked":0,"skipped_os_metadata":0}`,
where `buckets` and `objects` count newly created entries. Without a seed
directory it is `{"outcome":"disabled"}`.

### Differences from fake-gcs-server

Like fake-gcs-server, a restart replaces seeded objects that differ, including
API metadata edits. The intended differences: fake-gcs-server writes a new
generation even for an exactly identical object, which gcs-mimic keeps, and
gcs-mimic's `/_internal/reseed` route is off until local administration is
enabled. Seeding does not import fake-gcs-server's
runtime xattrs/sidecars or preserve their metadata. The seed directory stays
read-only input: API writes do not appear under `/data`. To browse API-written
objects as files, use a separate [sync directory](#mirror-objects-to-a-host-directory).
To create buckets with CORS or versioning settings, use a
[buckets file](#initialize-bucket-settings); it runs before seed. See the
[storage comparison](fake-gcs-server-storage.md) before reusing a persistent
source directory.

## Mirror objects to a host directory

`GCS_MIMIC_SYNC_DIR` (`--sync-dir`, JSON `sync_dir`) writes every bucket and live
object to `BUCKET/OBJECT` files under an existing writable directory. Add these
entries to the README's service, retaining its other environment variables and
persistent volume:

```yaml
    environment:
      GCS_MIMIC_SYNC_DIR: /sync
    volumes:
      - ./sync:/sync
```

Create `./sync` before starting, then use the API as usual:

```sh
mkdir -p sync
docker compose up -d --wait
curl --fail -H 'Content-Type: application/json' -d '{"name":"demo-bucket"}' \
  'http://localhost:4443/storage/v1/b?project=demo-project'
curl --fail --data-binary 'hello' \
  'http://localhost:4443/upload/storage/v1/b/demo-bucket/o?uploadType=media&name=docs%2Fhello.txt'
cat sync/demo-bucket/docs/hello.txt
```

After readiness, the server exports existing buckets and objects, including empty
buckets. API create, overwrite, copy, compose and delete then update the files
shortly after each response; poll rather than expecting the file at response time.
The SQLite catalog and internal blobs remain the stored state, so the directory
is a mirror. Data persistence still requires the `/var/lib/gcs-mimic` volume.

Since 0.8.0 the server also watches the directory
([continuous observation](slices/continuous-directory-observation.md)), applies
file deletions behind a guard, and offers live status, findings, wait and
approval controls through `gcs-mimic sync`; 0.7.0 imported files only at
startup and never deleted objects. Use the
[synchronization contract](slices/directory-synchronization.md) for the exact
supported platform/auth/recovery subset. Ready precedes initial reconciliation
and is not a synchronization-complete signal; `gcs-mimic sync wait` waits for it.

Files created or edited outside the server are imported as new object
generations, with hashes, shortly after the change; no restart is needed.
Deleting a file deletes its live object, subject to holds and retention. When at
least 100 objects would go and they are more than half of the tree, the deletion
guard (`GCS_MIMIC_SYNC_DELETE_GUARD`, default `true`) stops until the exact
set is approved with `gcs-mimic sync approve`. Recent automatic deletions also
count toward this threshold across split scans. Deleting a bucket directory
keeps the bucket, and deleting a bucket through the API keeps its directory.
When both the API and a file changed the same object, neither overwrites the
other; the conflict is retained and listed by `gcs-mimic sync conflicts` for
explicit resolution. Object names containing `\` or `:`, such as
`logs/2026-10-28T12:00:00.log`, and other names that cannot be mapped to host
file names are skipped without stopping the mirror; they stay available through
the API.

The directory holds `.gcs-mimic-sync.json` and `.gcs-mimic-sync-work`, which tie
it to one data directory; add both and `.gcs-mimic-sync.json.staging` to
`.gitignore`. Marker creation writes and syncs the staging file before atomic
publication. The staging marker component is reserved at every object depth;
existing API objects using it remain stored but cannot be mapped to the tree.
Restart can recover a complete staged marker belonging to the same
catalog/root; foreign or malformed published markers still fail closed. After
`docker compose down --volumes` or another new data volume, startup rejects the
old directory with a `ForeignMarker` diagnostic. Move it aside and create a fresh
empty directory instead. The directory must not overlap the data or seed
directory. In strict mode, sync controls require project-owner authority through
the local identity registry. Sync runs on Linux and macOS; on other platforms the
directory cannot be watched, so the server reports the failure and does not
become ready.

Directory metadata, ownership and extended-attribute changes revalidate read and
search access. An inaccessible subtree keeps synchronization readiness down until
access is repaired and observation recovers.

Exported files and directories are private to the server user (modes `0600` and
`0700`). Docker Desktop maps ownership on bind mounts, so the files are readable
on macOS hosts. On a Linux host, the image's UID `10001` would own them; run the
container as your user and keep the data directory on a bind mount owned by the
same user:

```sh
mkdir -p data sync
docker run -d --name gcs-mimic --user "$(id -u):$(id -g)" \
  -p 127.0.0.1:4443:4443 \
  -v "$PWD/data:/data" -v "$PWD/sync:/sync" \
  -e GCS_MIMIC_SCHEME=http -e GCS_MIMIC_DATA_DIR=/data \
  -e GCS_MIMIC_SYNC_DIR=/sync \
  ghcr.io/shiroemons/gcs-mimic:0.13.0
```

See the [synchronization contract](slices/directory-synchronization.md) for the
verified subset, recovery behavior and pending work.

## Configure the server and buckets

Server options control the listener, protocol, persistent data location and
advertised API endpoint. Use `GCS_MIMIC_*` environment variables in Compose or
`serve --help` to inspect native options. See the
[fake-gcs-server option mapping](fake-gcs-server-migration.md#startup-options-and-current-equivalents)
for `-public-host`, `-external-url`, `-data` and related settings.
The [environment reference](fake-gcs-server-configuration.md) covers all 19
source variables, including Docker entrypoint precedence and missing equivalents.

Bucket settings belong to each bucket. For example, create a bucket with its
own CORS rules using the JSON API:

```sh
curl --fail -H 'Content-Type: application/json' \
  -d '{"name":"web-assets","cors":[{
    "origin":["http://localhost:3000"],"method":["GET","HEAD"],
    "responseHeader":["Content-Type"],"maxAgeSeconds":3600
  }]}' \
  'http://localhost:4443/storage/v1/b?project=demo-project'
```

Create another bucket with different rules, or change existing rules with
`PATCH /storage/v1/b/BUCKET`. Bucket CORS governs XML object endpoints;
JSON endpoints use fixed CORS defaults. CORS does not grant object permissions.
To let any origin read XML/public URLs of buckets without rules, as
fake-gcs-server did, add `GCS_MIMIC_CORS_FALLBACK=allow-all`; see
[CORS fallback](slices/cors-fallback.md).

By default the server accepts signed URLs without checking their signatures.
To reject changed, expired or unknown-key signed requests while keeping
permissive access, add `GCS_MIMIC_SIGNED_URLS=verify`; see
[verifying signed requests in permissive mode](signed-urls.md#verifying-signed-requests-in-permissive-mode).

## Initialize bucket settings

For a source checkout, the supplied `config/buckets.json` creates two buckets
with separate CORS, versioning and project settings. Start with its read-only mount:

```sh
docker compose -f compose.yaml -f compose.http.yaml -f compose.buckets.yaml \
  up -d --build --wait
curl --fail http://localhost:4443/storage/v1/b/initialized-assets
```

For another Compose file, add `GCS_MIMIC_BUCKETS_FILE: /buckets.json` to its
environment and `./buckets.json:/buckets.json:ro` to its volumes. The file must
exist before startup. Its JSON shape is:

```json
{"version":1,"project":"local-project","buckets":[
  {"name":"web-assets","cors":[{"origin":["http://localhost:3000"],"method":["GET"]}]},
  {"name":"private-uploads","versioning":{"enabled":true}}
]}
```

Each startup validates the entire file, then creates only missing buckets before
seed. Existing settings, IAM policies, timestamps and objects remain unchanged;
edit them through the API. Removing an entry never deletes a bucket. A deleted
bucket can be recreated at the next startup. A different existing project emits
a warning and increments ready's `project_mismatches`, preserving its owner.

An entry can give a new bucket its IAM policy with
`"iamPolicy":{"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}`.
The policy replaces the default one, as `setIamPolicy` does, so write the legacy
bindings too if you need them. Startup fails before any change when public access
prevention refuses a public member.

When you edit an entry after its bucket exists, the next startup keeps the
bucket, warns on stderr which fields still differ and increments ready's
`unapplied`. To apply the written fields to existing buckets, for example on a
volume that outlives Compose restarts, set `GCS_MIMIC_BUCKETS_FILE_MODE: reconcile`.
Each startup then writes only the fields an entry sets whose value differs and
counts the changed buckets in ready's `reconciled`; fields an entry omits keep
their API changes, and a bucket's location never changes. The mode also reads
`--buckets-file-mode` or server JSON `buckets_file_mode`, defaults to `create`,
and fails startup when no buckets file is selected.

Removing a field from an entry never resets the bucket, in either mode: after you
drop an `allUsers` `iamPolicy`, the bucket stays public, and the next startup
warns once that `iamPolicy` is no longer written. Write the value you want
instead, such as `"iamPolicy":{"bindings":[]}`. The first startup after an
upgrade from 0.13.0 also warns once about each existing bucket that differs from
its entry.

Entry `project` overrides file `project`, then `GCS_MIMIC_SEED_PROJECT`
(default `local-project`). Initializer v1 rejects ACLs, lifecycle, UBLA, `iam`,
policy fields other than `bindings` and unknown fields, including disabled or
empty unsupported settings. Initialization is local administration in both auth
modes and grants no API permissions beyond the policy an entry writes. CORS
applies to XML; JSON uses its fixed defaults.
See the [schema, limits and ready counts](slices/bucket-initialization.md).

## Further setup

- [HTTPS and custom certificates](local-https.md#local-https-with-an-explicit-development-ca)
- [SDK, authentication and browser examples](getting-started.md)
- [Source builds and verification](development.md)
- [Image publication and supported platforms](container-distribution.md)
