# Move from fake-gcs-server

[日本語](../ja/migrate-from-fake-gcs-server.md)

gcs-mimic takes its everyday ergonomics from
[fake-gcs-server](https://github.com/fsouza/fake-gcs-server), so most container
setups move with a few changed lines. This page starts with what you give up,
because that decides whether moving is worth it. It is written for gcs-mimic
0.27.0 and fake-gcs-server 1.56.1.

gcs-mimic is not a drop-in replacement for every setup.

## What stops working

Roughly in order of how many setups it affects.

| You do this today | After the move | Do this instead |
| --- | --- | --- |
| Keep objects in a `/storage` volume | The format differs, so the volume cannot be reused. Since 0.18.0, `-filesystem-root` stops startup. | Remove the flag; [copy the object bytes](#bring-existing-objects) or reseed from your fixtures. |
| Set `FAKE_GCS_*` variables | Since 0.15.0 they are not read, and nothing warns. With `FAKE_GCS_PORT=9000` left in place the server still listens on 4443, so clients sent to port 9000 fail to connect. | Use the matching flag or `GCS_MIMIC_*` variable from [the table below](#setting-equivalents). |
| Rely on the generations, custom metadata or content types of existing objects | They cannot be carried over today. `gcs-mimic import fake-gcs-server` exists, but in 0.14.0 it stops on every object uploaded through fake-gcs-server's API, because those objects carry a generated `projectOwner-test-project` ACL it does not accept. | Re-create the objects your tests depend on through the API, or keep that data on fake-gcs-server. |
| Browse or edit objects as files in the storage directory | Objects are stored as opaque files. `/data` is read-only input, and API writes never appear there. | Add a [sync directory](#keep-objects-as-files). Objects and files stay in step both ways; its conflict and recovery cases are still partial. |
| Use `docker exec … sh`, shell healthchecks or shell entrypoints | The image has no shell and no package manager. | Remove your `healthcheck:` override, since the image has its own (`gcs-mimic healthcheck`), whose start period also covers the first pass of a [sync directory](container-usage.md#mirror-objects-to-a-host-directory). Run scripts from another container. |
| Mount directories only root can read or write | The server runs as UID/GID 10001, not root. | Use a named volume for `/var/lib/gcs-mimic`, and keep files under `/data` readable by every user. |
| Embed `fakestorage` in a Go test process | There is no in-process server. The [`fakestorage` package](../../go/README.md#fakestorage) starts the Rust binary as a subprocess and covers a subset of the API; `Backend()` is not supported. | Change the import path to `github.com/shiroemons/gcs-mimic/go/fakestorage` where its subset is enough. Write new tests with [`gcsmimic`](../../go/README.md#gcsmimic), which adds per-test buckets, fixture loading and a reset to the startup state. |
| Use a Testcontainers module or wrapper that hard-codes `fake-gcs-server` | Changing the image is not enough, because the wrapper expects the old executable and readiness log. | Use a [generic container adapter](slices/sdk-client-adapters.md). |
| Run `-backend memory` with a lot of data | RAM storage has a budget of object bytes, by default about half of the available memory, from 64 MiB to 1 GiB. Past it, writes fail with 503. | Raise `memory_max_bytes` ([RAM contract](slices/memory-backend.md)) or use the disk backend. |
| Put `.metadata` sidecar files in a seed directory | They are seeded as ordinary objects, not read as metadata. | Set metadata through the API. |
| Call `/_internal/reseed` or `PUT /_internal/config` | These routes are off by default. | Set `GCS_MIMIC_ADMIN_ENABLED=true`, on an isolated listener. |
| List buckets without `?project=` | Returns 400, as Cloud Storage does. SDKs send the project already. | Add `?project=local-project`, or set `GCS_MIMIC_COMPAT_PROFILE=fake-gcs-server`. |
| Use `-event.*` without a Pub/Sub emulator | Startup fails, so tests that wait for notifications do not hang. | Point `PUBSUB_EMULATOR_HOST` at a local emulator. |
| Depend on `-public-host` matching the Host header | Accepted as `public_base_url`; subdomains select virtual-host buckets. | Use another host for JSON or set `--api-base-url`; review the [routing differences](fake-gcs-server-configuration.md). |
| Send requests Cloud Storage would reject | gcs-mimic validates input as Cloud Storage does, so loose requests can now return 4xx. | Fix the request. |
| Assert on the status of a cancelled resumable upload | Returns 499, which Google documents as a successful cancel. | Accept 499. |

## When to stay on fake-gcs-server

Moving is probably not worth it yet if one of these is true:

- Your Go tests embed `fakestorage` and depend on `Backend()` or on running
  in-process.
- You need the generations or custom metadata of objects already in a
  `/storage` volume.
- Your tests read an object's file the moment the API answers and cannot wait.
  A [sync directory](#keep-objects-as-files) writes the file just after the
  response, so those tests need `gcs-mimic sync wait` first.

## Swap the container

```diff
 gcs:
-  image: fsouza/fake-gcs-server:1.56.1
+  image: ghcr.io/shiroemons/gcs-mimic:0.27.0
-  command: ["-scheme", "http", "-external-url", "http://gcs:4443"]
+  command: ["--api-base-url", "http://gcs:4443"]
   volumes:
     - ./data:/data
-    - gcs-storage:/storage
+    - gcs-data:/var/lib/gcs-mimic
```

Most `command:` flags and the `./data:/data` mount keep working. Since 0.18.0,
a flag that changes meaning stops startup with native-setting advice. `-scheme http`
can be dropped: the default serves HTTP and HTTPS on the same port.
Application code that talks to the emulator over HTTP does not change,
including `STORAGE_EMULATOR_HOST`.

Soft delete stays off by default, as in fake-gcs-server: deleting or overwriting
an object frees its bytes at once. Set
`GCS_MIMIC_DEFAULT_SOFT_DELETE_RETENTION_SECONDS` to keep them.

## Bring existing objects

The objects in a `/storage` volume are plain files, so their names and bytes can
become a seed directory. Stop fake-gcs-server first, then copy them out:

```sh
docker run --rm -v OLD_VOLUME:/storage:ro -v "$PWD/data:/out" busybox:1 \
  sh -c 'cp -R /storage/. /out/ && rm -f /out/*.bucketMetadata && chmod -R a+rX /out'
```

Mount `./data:/data:ro` and start gcs-mimic. Buckets, including empty ones,
object names and bytes come across. Content types are derived again from file
extensions. Generations, custom metadata and ACLs do not come across.

Seeding runs on every startup and replaces API changes to the same names. Once
the objects are in the data volume, remove the mount if you do not want that.

## Keep objects as files

fake-gcs-server's filesystem backend stores each object as the file
`<root>/<bucket>/<object>`. A sync directory gives gcs-mimic the same view in
both directions: every bucket and live object appears as `BUCKET/OBJECT` under a
host directory, files you add or edit there become objects, and deleting a file
deletes the object.

Three things to settle before the first start:

- **Create the directory first.** The server refuses to start without it, and
  Docker does not create a missing bind source on the host.
- **On a Linux host, run the server as yourself.** It writes files readable only
  by its own user (modes `0600`, directories `0700`), and it can only write a
  directory its user owns. As UID 10001 it would write files you cannot read
  without `sudo`, so give it your UID and put the data on a host directory you
  own too (the default named volume belongs to UID 10001). Docker Desktop on
  macOS maps ownership, so macOS needs none of this.
- **Ignore the directory in git.** It holds the server's marker files as well as
  your objects: add `/sync/`, or the directory you chose, to `.gitignore`.

Add these entries to your own service, keeping what it already has. The
variables default to the macOS case, so the same lines serve both hosts:

```yaml
services:
  gcs:
    image: ghcr.io/shiroemons/gcs-mimic:0.27.0
    user: "${GCSM_UID:-10001}"
    environment:
      GCS_MIMIC_SYNC_DIR: /sync
    volumes:
      - ${GCSM_DATA_DIR:-gcs-data}:/var/lib/gcs-mimic
      - ./sync:/sync
```

Then start it, write an object and read the file. On Linux, run the `export`
line; on macOS leave it out:

```sh
mkdir -p sync data
export GCSM_UID=$(id -u) GCSM_DATA_DIR=./data
docker compose up --wait
curl --fail -H 'Content-Type: application/json' -d '{"name":"demo-bucket"}' \
  'http://localhost:4443/storage/v1/b?project=local-project'
curl --fail --data-binary 'hello' \
  'http://localhost:4443/upload/storage/v1/b/demo-bucket/o?uploadType=media&name=docs%2Fhello.txt'
docker compose exec gcs gcs-mimic sync wait --endpoint http://127.0.0.1:4443
cat sync/demo-bucket/docs/hello.txt
```

If you start from a checkout of this repository instead,
[`compose.sync.yaml`](../../compose.sync.yaml) holds the same entries for
`compose.yaml`. Use a published image, as everywhere else in these docs:

```sh
export GCSM_IMAGE=ghcr.io/shiroemons/gcs-mimic:0.27.0
export COMPOSE_FILE=compose.yaml:compose.sync.yaml
docker compose up --wait --no-build
```

`docker compose up --wait` returns only after the first pass has taken in the
files already in the directory and written every existing object, so no flag or
healthcheck override is needed. A service that lists the server under
`depends_on` with `condition: service_healthy` starts after that pass too.
On Linux, a server that stops with `sync_dir must be an existing writable
directory` is running as a user that does not own `./sync`: set `GCSM_UID`
first.

What differs from fake-gcs-server:

- **Files are written after the response.** fake-gcs-server writes the file
  before it answers; gcs-mimic answers first, so the file can be missing for a
  moment. Read it after `gcs-mimic sync wait --endpoint …`, which returns once
  the changes it can apply have been applied. The same command, run after you
  add or edit a file by hand, waits until the server has taken the change in.
- **Files you add or edit become objects.** No restart is needed. In
  fake-gcs-server's macOS probe, a plain file with no stored metadata answered
  404 ([comparison](fake-gcs-server-storage.md#external-edits-do-not-reconcile-metadata)).
- **A name with `:` is kept.** `logs/2026-10-28T12:00:00.log` becomes a file of
  that name where the filesystem stores colons, which the server checks at
  startup. macOS Finder shows the `:` as `/`; the name on disk keeps the colon.
  A name containing `\` is not written as a file; the API still serves it.
- **Reset, reseed and purge are refused** for synchronized buckets while
  `sync_dir` is set: external files are never reset or purged. To start over,
  remove the data and the directory together, dot files included, so that the
  markers go too:

  ```sh
  docker compose down --volumes
  rm -r sync data
  mkdir sync data
  ```

  Removing the data but keeping the directory, or emptying it with `sync/*`,
  which does not match dot files, leaves the old markers, and the server then
  refuses the directory (`ForeignMarker`) because they belong to the old data.
- **Seed files win at every start.** If you also mount `/data` as a seed
  directory, a seeded name is replaced at each start, so an edit to that object
  made in the sync directory or through the API does not outlive a restart.
- **Lifecycle rules do not run by default** with a sync directory, because they
  would delete host files. `GCS_MIMIC_LIFECYCLE_EXECUTION=enabled` turns them on.
- **Deletions are applied without asking, up to a guard.** Deleting a file
  deletes its object, and so does a file deleted while the server was stopped,
  at the next start. Only when at least 100 tracked files, and more than half of
  all tracked files, would go at once does the server wait for your
  approval. With several buckets, deleting one bucket's directory usually stays
  under that half and is applied at once. When the guard does stop a deletion,
  the API keeps answering. Take the revision from `pending_deletion_revision`,
  look at the list, then approve it (use your own value for
  `DELETION_REVISION`):

  ```sh
  docker compose exec gcs gcs-mimic sync status --endpoint http://127.0.0.1:4443
  docker compose exec gcs gcs-mimic sync status --view deletions \
    --endpoint http://127.0.0.1:4443 --revision DELETION_REVISION
  docker compose exec gcs gcs-mimic sync approve \
    --endpoint http://127.0.0.1:4443 --deletion-set DELETION_REVISION
  ```

  [Sync CLI usage](slices/sync-cli.md) lists the other options.
- **The directory must match the data.** The server refuses a directory that
  belonged to other data (`ForeignMarker`), as after `docker compose down
  --volumes`. Start over as above.
- **An empty directory switches without losing data.** To keep the data and
  synchronize another host directory, mount an empty one (`.DS_Store` alone is
  fine) at the same path. The server writes every object into it, deletes none
  and leaves the previous directory as it was. A directory that holds anything
  else is refused (`RootChanged`).

The [reference](container-usage.md#mirror-objects-to-a-host-directory) covers
conflicts, recovery and the remaining limits.

## Setting equivalents

Since 0.18.0, source flags that change meaning are refused before startup,
with all refusals reported together. Accepted translations show one native
option block at info level. Use the
[complete flag table](slices/option-aliases.md#behavior) when keeping source flags.

| fake-gcs-server | gcs-mimic |
| --- | --- |
| `-scheme http`, `FAKE_GCS_SCHEME` | `--scheme http`, `GCS_MIMIC_SCHEME: http` |
| `-port 9000`, `FAKE_GCS_PORT` | `--port 9000`, `GCS_MIMIC_LISTEN: 0.0.0.0:9000` |
| `-port-http 8000`, `FAKE_GCS_PORT_HTTP` | Requires `--scheme both`; use `--port-http 8000` or `GCS_MIMIC_LISTEN_HTTP: 0.0.0.0:8000` |
| `-host 0.0.0.0 -port 4443` | Nothing; the image already listens there |
| `-external-url`, `-public-host` | Nothing; returned links follow each request's address |
| `-backend filesystem -filesystem-root /storage` | A named volume at `/var/lib/gcs-mimic` |
| `-backend memory` | `GCS_MIMIC_BACKEND: memory` |
| `-data /data` | Nothing; keep the mount |
| `-cors-headers` | Nothing; set `cors` on the buckets that need rules |

## Go deeper

- [Common settings](container-usage.md#common-settings)
- [Full migration reference](fake-gcs-server-migration.md), including
  [troubleshooting](fake-gcs-server-migration.md#migration-troubleshooting) and
  [observed GitHub uses](fake-gcs-server-migration.md#observed-github-uses-and-migration-boundaries)
- [Configuration reference](fake-gcs-server-configuration.md)
- [How the storage formats differ](fake-gcs-server-storage.md)
