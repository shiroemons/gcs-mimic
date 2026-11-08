# Use the Docker image

The image `ghcr.io/shiroemons/gcs-mimic:0.14.0` is published for `linux/amd64` and `linux/arm64`; pulling it needs no registry login. Docker Hub serves the same image digest as `shiroemons/gcs-mimic:0.14.0`, so either name works in the commands below. Pin this exact version in CI. To test unreleased source instead, build a local image from a checkout and use its tag in the commands below:

```sh
docker build -t gcs-mimic:local .
```

The [Compose migration walkthrough](fake-gcs-server-migration.md#replace-a-fake-gcs-server-compose-service)
replaces a fake-gcs-server service with the published image and initializes
empty buckets. It covers UX-012, DESIGN-061 and UX-001.

The [README quick start](../../README.md#quick-start) is the shortest start: one `docker run` serves HTTPS with a built-in development certificate, and `-e GCS_MIMIC_SCHEME=http` selects plain HTTP. List every option with `docker run --rm ghcr.io/shiroemons/gcs-mimic:0.14.0 --help`.

## Docker run

```sh
docker run -d --name gcs-mimic \
  -p 127.0.0.1:4443:4443 \
  -v gcs-mimic-data:/var/lib/gcs-mimic \
  -e GCS_MIMIC_SCHEME=http \
  ghcr.io/shiroemons/gcs-mimic:0.14.0
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
Go storage drops a non-default port when it signs a URL. The default permissive
server does not check signatures, so such URLs for `localhost:4443` work there.
A server that verifies signatures (strict mode, or `GCS_MIMIC_SIGNED_URLS=verify`)
accepts them from the built-in test signer while it is enabled, because
`GCS_MIMIC_SIGNED_HOST_PORT` then defaults to `lenient` for that signer's own
signatures. Every other key, and every signer without the test signer, gets
`exact`, which rejects such URLs with 403 `SignatureDoesNotMatch`; set `lenient`,
which covers every signer, when another key signs such URLs, for example because
each worktree publishes its own port and a default port is not an option; see
[signed URLs](signed-urls.md).

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

## Which setup do I need?

| Goal | Setup | Settings |
| --- | --- | --- |
| Behave like fake-gcs-server: every request is allowed, no signature is checked and browsers read every bucket | The default permissive profile | `GCS_MIMIC_SCHEME=http` |
| Production-like access: private buckets stay private, public ones open through `allUsers`, browsers use signed URLs, application servers keep their own Cloud Storage calls | Strict mode with a trusted listener: the [strict local recipe](../../examples/strict-local/README.md) | `GCS_MIMIC_SCHEME=http`, `GCS_MIMIC_AUTH_MODE=strict`, `GCS_MIMIC_TRUSTED_LISTEN=0.0.0.0:4444` (never published) and `GCS_MIMIC_BUCKETS` with each bucket's policy and CORS rules, plus `GCS_MIMIC_TEST_SIGNER=enabled` when the application signs URLs or, like Rails, uses the `test-signer credentials` key |
| Only while strict mode is not possible yet: check signed URLs, allow everything else | Permissive with signature verification | `GCS_MIMIC_SCHEME=http` and `GCS_MIMIC_SIGNED_URLS=verify` |

Start with the default. For production-like checks, use strict mode with the
trusted listener, as the strict local recipe sets it up: the container smoke test
runs that exact block with the image, and its README explains each line and the
defaults it relies on. The [full application example](../../examples/full-stack/README.md)
shows how a Go API, a Rails app and a frontend connect to it. Permissive `verify`
is only a step for setups that cannot use strict mode yet: it rejects a bad
signature, but it grants every verified and every unsigned request, so it cannot
show that a private bucket stays private. The last two rows need 0.14.0 or
later.

### How the setups differ from fake-gcs-server

This table compares behavior with fake-gcs-server 1.56.1, using the
[migration guide](fake-gcs-server-migration.md), the
[detailed comparison](feature-comparison.md), the
[configuration reference](fake-gcs-server-configuration.md) and the release
notes. "Same as strict" marks behavior that the profile does not change.

| Behavior | fake-gcs-server 1.56.1 | gcs-mimic permissive (default) | gcs-mimic strict |
| --- | --- | --- | --- |
| Authentication | None; every request is allowed | None; credentials are ignored and every request is allowed | Local identities: bearer tokens, self-signed JWTs, signed URLs and HMAC signatures. A request without credentials is anonymous |
| IAM and ACLs | ACL metadata operations without enforcement; no bucket IAM routes | Stored and returned, never enforced | Predefined roles in project roles and bucket IAM policies are enforced; object ACL `READER` and `OWNER` grants authorize reads |
| Public and private buckets | No split: every object is served to anyone | No split: every object is served to anyone | Requests without credentials get only `allUsers` grants and otherwise 401; `allAuthenticatedUsers` needs a registered identity |
| Signed URL verification | Signatures and expiry are not checked | Not checked by default; `GCS_MIMIC_SIGNED_URLS=verify` checks them and still allows every request | Always checked: host, path, query, method, signed headers and expiry. A host without its port, as Go storage signs, passes in the enabled test signer's signatures, or in every signer's with `signed_host_port` `lenient` |
| CORS defaults | One wildcard policy on every route; `-cors-headers` adds allowed request headers | JSON API: fixed defaults. XML and public routes: every origin for buckets without rules (`cors_fallback` `allow-all`); bucket rules apply exactly | JSON API: fixed defaults. XML and public routes: only bucket `cors` rules (`cors_fallback` `none`) |
| Bucket setup | A seed directory, `-data`, whose top-level directories become buckets | Same as strict | A seed directory at `/data`, as in fake-gcs-server, and a bucket document with CORS, versioning, IAM policy and soft delete per bucket: `GCS_MIMIC_BUCKETS` inline, whose edits reach existing buckets, or `GCS_MIMIC_BUCKETS_FILE` |
| Soft delete | None: a delete or overwrite frees the bytes | Same as strict | Off by default, as in fake-gcs-server; a bucket policy or `GCS_MIMIC_DEFAULT_SOFT_DELETE_RETENTION_SECONDS` keeps the bytes for restore |
| Timeouts | No setting among its 19 variables | Same as strict | 30 s to send a request head and 180 s for a request; no limit on a silent body or on object transfers; idle connections close after 120 s. Each is a setting |
| Connection limit | No setting among its 19 variables | Same as strict | 1024 connections across listeners; further clients wait for a slot |
| Object size and memory | `CreateObject` buffers the whole object; the memory backend keeps every byte in the process | Same as strict | Bodies stream in 64 KiB chunks, so memory does not grow with object size; objects up to 5 TiB (`max_object_bytes`) |
| Resumable sessions across restarts | Not compared in this repository | Same as strict | Sessions and their acknowledged bytes persist in the data volume, resume after a restart and expire after seven days; the memory backend loses them |
| Returned URLs | `-external-url` fixes one address for every client | Same as strict | Links follow each request's Host and connection scheme, so containers and the host each get their own address; `GCS_MIMIC_API_BASE_URL` fixes one |
| Conditional requests | Generation conditions on uploads only; `If-Match` and `If-None-Match` are ignored on reads | Same as strict | Generation and metageneration conditions on object and bucket mutations, enforced with the write; ETag validators on reads |
| Bucket list without `project` | Allowed | Same as strict | 400 `required`; `GCS_MIMIC_COMPAT_PROFILE=legacy-emulator` uses `seed_project` |
| Trusted listener | None | Not available: startup fails, since every request is already allowed | `GCS_MIMIC_TRUSTED_LISTEN`: requests without credentials act as `trusted-app@gcs-mimic.invalid` or `GCS_MIMIC_TRUSTED_PRINCIPAL`; see [below](#keep-private-buckets-private-with-a-trusted-listener) |

No row compares speed or measured memory use.

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
  -v "$PWD/data:/data:ro" ghcr.io/shiroemons/gcs-mimic:0.14.0
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
configured limit, holds names it cannot import, or holds files the container user
(uid 10001) cannot read answers 400 and names the problem; only work a later
attempt can finish answers 503 with `Retry-After: 1`.

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
  ghcr.io/shiroemons/gcs-mimic:0.14.0
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

A client that announces a request body and stops sending it holds its connection
silently: the default `GCS_MIMIC_BODY_INACTIVITY_TIMEOUT_MS` of 0 sets no limit, so
nothing expires and nothing is logged. To find such a client, set it, for example
to `30000`; that silent request then fails with JSON 408 `uploadBrokenConnection`
or XML 400 `RequestTimeout`, and the server logs a `connection_timeout` warning.

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
For buckets without rules, `GCS_MIMIC_CORS_FALLBACK` defaults to `allow-all` in
permissive mode, so any origin reads their XML and public URLs, as with
fake-gcs-server; the first such request for each bucket logs one
`cors_fallback_default` event at info level. In strict mode it defaults to
`none`, so a browser needs the bucket's `cors` rules, as with Cloud Storage. Buckets with rules always follow them; see
[CORS fallback](slices/cors-fallback.md).

By default the server accepts signed URLs without checking their signatures.
To reject changed, expired or unknown-key signed requests while keeping
permissive access, add `GCS_MIMIC_SIGNED_URLS=verify`; see
[verifying signed requests in permissive mode](signed-urls.md#verifying-signed-requests-in-permissive-mode).
It also covers `Authorization` header signatures and POST policies, so S3-style
clients that sign with made-up HMAC keys are rejected once it is on.
Such a server then accepts the built-in test signer, whose published key the
image prints as a service account key file; the image holds no copy of the
repository:

```sh
docker run --rm "${GCS_MIMIC_IMAGE:-gcs-mimic:dev}" test-signer credentials > test-signer.json
```

Its private key is public, so use it only for local tests; see the
[built-in test signer](signed-urls.md#built-in-test-signer).

## Initialize bucket settings

For a source checkout, the supplied `config/buckets.json` creates two buckets
with separate CORS, versioning and project settings. Start with its read-only mount:

```sh
docker compose -f compose.yaml -f compose.http.yaml -f compose.buckets.yaml \
  up -d --build --wait
curl --fail http://localhost:4443/storage/v1/b/initialized-assets
```

For another Compose file, write the document into the service's environment as
`GCS_MIMIC_BUCKETS`, as the [strict local recipe](../../examples/strict-local/README.md)
does, or add `GCS_MIMIC_BUCKETS_FILE: /buckets.json` to its environment and
`./buckets.json:/buckets.json:ro` to its volumes; the two settings cannot be
combined. Compose expands `${VAR}` in the inline value, so write a literal `$` as
`$$`. A mounted file must exist before startup. The JSON shape is:

```json
{"version":1,"project":"local-project","buckets":[
  {"name":"web-assets","cors":[{"origin":["http://localhost:3000"],"method":["GET"]}]},
  {"name":"private-uploads","versioning":{"enabled":true}}
]}
```

Each startup validates the entire document, then creates only missing buckets
before seed. With a bucket file, existing settings, IAM policies, timestamps and
objects remain unchanged; edit them through the API. Removing an entry never
deletes a bucket. A deleted bucket can be recreated at the next startup. A
different existing project emits
a warning and increments ready's `project_mismatches`, preserving its owner.

An entry can give a new bucket its IAM policy with
`"iamPolicy":{"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}`.
The policy replaces the default one, as `setIamPolicy` does, so write the legacy
bindings too if you need them. Startup fails before any change when public access
prevention refuses a public member.

When you edit an entry of a bucket file after its bucket exists, the next startup
keeps the bucket, warns on stderr which fields still differ and increments ready's
`unapplied`. To apply the written fields to existing buckets, for example on a
volume that outlives Compose restarts, set `GCS_MIMIC_BUCKETS_FILE_MODE: reconcile`.
Each startup then writes only the fields an entry sets whose value differs and
counts the changed buckets in ready's `reconciled`; fields an entry omits keep
their API changes, and a bucket's location never changes. The mode also reads
`--buckets-file-mode` or server JSON `buckets_file_mode`. It defaults to
`reconcile` for the inline `GCS_MIMIC_BUCKETS`, whose edits Compose delivers by
recreating the container on a plain `docker compose up -d`, and to `create` for a
bucket file; it fails startup when neither is selected. The ready event reports
both forms under `buckets_file`.

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

## Keep private buckets private with a trusted listener

In strict mode a request without credentials is anonymous, so only `allUsers`
grants authorize it. That is what a browser should get: a page reads public
buckets and reaches private ones only through signed URLs. The application's
own servers usually send no credentials to an emulator either (Go storage with
`STORAGE_EMULATOR_HOST`, or an HTTP client that starts resumable uploads), so
they would be anonymous too. A trusted listener gives those servers a port of
their own, and those calls stay unchanged. URLs the servers hand to browsers are
still checked, so an application that leaves them unsigned against the emulator
[needs a change](../../examples/strict-local/README.md#what-the-trusted-listener-does-not-cover).

The listener works like this:

- A request without credentials on the trusted listener acts as one identity,
  the trusted principal: the built-in `trusted-app@gcs-mimic.invalid` unless
  `trusted_principal` names another. It then passes the same IAM, ACL and public
  access prevention checks as a request presenting that identity's credentials,
  and the objects it writes record that identity as owner.
- A request that carries credentials (a bearer token, a self-signed JWT, signed
  URL parameters, a signature `Authorization` header or a POST policy)
  authenticates exactly as on the main listener. Invalid credentials fail and
  never fall back to the principal. The
  [Rails Active Storage recipe](rails-active-storage.md) always sends its own
  token, so the server must still know its signer: the built-in test signer
  with `GCS_MIMIC_TEST_SIGNER=enabled`, or a key the identity document registers.
- A request with any `Origin` header, or a `Sec-Fetch-Site` other than `none`,
  gets 403 before routing. Server-side clients send neither header.
- The request's Host must be an IP literal (`127.0.0.1:4444`, `[::1]:4444`),
  `localhost` or a name ending in `.localhost`, a single-label name such as the
  Compose service name `gcs`, or a name listed in `trusted_allowed_hosts`. Any
  other Host, and a request without one, gets 421 `misdirected`, whether or not
  it carries credentials. This stops DNS rebinding: a page on
  `http://evil.example` can point its own name at this listener, and its
  same-origin GETs then carry neither `Origin` nor, over plain HTTP to an address
  other than loopback, `Sec-Fetch-Site`, but its Host still says `evil.example`.
  Rebinding needs a name that the attacker's DNS answers, which is a public name
  with a dot; a page cannot make a browser send an IP literal, a `localhost` name
  (browsers resolve those themselves) or a single-label name for an origin its
  attacker controls.
- Every response of the trusted listener carries
  `Cross-Origin-Resource-Policy: same-origin` and
  `X-Content-Type-Options: nosniff`, so a page on another origin cannot use it
  even as an image or script embed. Clients that are not browsers ignore both.
- It serves every route of the main listener: JSON, XML, uploads, batch requests
  and `/_internal`. Management still requires `admin_principal`, so it works there
  without a token only when `trusted_principal` names that identity. gRPC has no
  trusted listener.
- It speaks plain HTTP whatever `scheme` selects, because it is meant for a
  private container network, and it shares `max_connections` and every timeout
  with the other listeners. Unless `GCS_MIMIC_API_BASE_URL` fixes one address,
  links that the API returns follow the Host of each request, so a resumable
  session started through `gcs:4444` continues there.

Set `GCS_MIMIC_TRUSTED_LISTEN` (`--trusted-listen`, JSON `trusted_listen`) to
open it. Without `GCS_MIMIC_TRUSTED_PRINCIPAL` (`--trusted-principal`, JSON
`trusted_principal`), the principal is the built-in `trusted-app@gcs-mimic.invalid`.
It holds `roles/storage.admin` on the project of every bucket the bucket document
(`GCS_MIMIC_BUCKETS` or `GCS_MIMIC_BUCKETS_FILE`) creates and on the effective
`seed_project`, so the application's servers can do what a project owner's
service account does, and strict mode then starts without an identity document.
These roles are implicit, so they are not the explicit `roles/storage.admin`
grant that directory synchronization controls require: those answer 403 on the
trusted listener until the identity document grants it.
An entry for `trusted-app@gcs-mimic.invalid` in the identity document
(`GCS_MIMIC_IDENTITIES` or `GCS_MIMIC_IDENTITY_FILE`) replaces those roles. Set
`GCS_MIMIC_TRUSTED_PRINCIPAL` to act as another identity of the identity document,
or as the built-in test signer when it is enabled. Startup fails when the
principal appears without the listener, in permissive mode (where every request
is already allowed), with `unix_socket`, when the address equals another
listener's, and when a named principal is neither an identity of the identity
document nor the enabled built-in test signer. The ready event reports the bound
`trusted_listen` and the effective `trusted_principal`.

When the application reaches the emulator through a dotted name, such as a
Compose network alias `storage.compose.internal` or a Kubernetes service name,
list it in `GCS_MIMIC_TRUSTED_ALLOWED_HOSTS` (`--trusted-allowed-hosts`, JSON
`trusted_allowed_hosts`): up to 32 comma-separated DNS names without ports or
schemes, compared case-insensitively and without a trailing dot. It requires
`trusted_listen`. List only names that public DNS cannot answer for someone
else, because a listed name is admitted as any other accepted Host.

> [!WARNING]
> Never publish the trusted port or bind it on a network that other people or
> machines share. Anything that reaches it with an accepted Host, such as its IP
> address, acts as `trusted_principal` without credentials. The browser and Host
> checks keep web pages out; they do not stop other programs.

The server does not warn about a wildcard trusted address: inside a container
the listener must bind `0.0.0.0` for other containers to reach it, so a warning
would fire on every start. Keeping the port off `ports:` is the protection.

The [strict local recipe](../../examples/strict-local/README.md) is the complete
example: a `gcs` service to paste into your compose.yaml, with the trusted listener
and its built-in identity for the application containers, the published main
listener for the browser, the buckets written inline in its environment, and the
built-in test signer for signed URLs. The
[full application example](../../examples/full-stack/README.md) connects a Go
API, a Rails app and a frontend to it. The container smoke test runs both with
the image, so start from them rather than assembling these settings yourself.

`gcs` is a single-label name, so the recipe needs no `trusted_allowed_hosts`, and
neither does `gcs.localhost`, the alias its Rails variant adds, because it is a
`.localhost` name. To use a dotted alias instead, give the service a network
alias and list it:

```yaml
services:
  gcs:
    environment:
      GCS_MIMIC_TRUSTED_ALLOWED_HOSTS: storage.compose.internal
    networks:
      default:
        aliases:
          - storage.compose.internal
  app:
    environment:
      STORAGE_EMULATOR_HOST: storage.compose.internal:4444
```

The built-in identity has no token, because the application never presents one.
To give it only the roles the application needs, declare
`trusted-app@gcs-mimic.invalid` in the identity document with those roles, as the
recipe's README shows. Pages then use
`http://localhost:${GCS_MIMIC_PORT:-4443}`: a bucket whose policy grants `allUsers`
`roles/storage.objectViewer` is readable there anonymously, and a private object
answers 401 unless the page has a signed URL. The application signs such URLs
for that browser-facing address, never for `gcs:4444`, with a key the identity
document registers or with the built-in test signer; see [signed URLs](signed-urls.md).
The image healthcheck probes the main listener and needs no change. DESIGN-101
records this local extension and stays PARTIAL; see the
[evidence](evidence/trusted-listener.md).

## Further setup

- [HTTPS and custom certificates](local-https.md#local-https-with-an-explicit-development-ca)
- [SDK, authentication and browser examples](getting-started.md)
- [Source builds and verification](development.md)
- [Image publication and supported platforms](container-distribution.md)
