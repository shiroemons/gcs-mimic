# Move from fake-gcs-server

[日本語](../ja/migrate-from-fake-gcs-server.md)

gcs-mimic takes its everyday ergonomics from
[fake-gcs-server](https://github.com/fsouza/fake-gcs-server), so most container
setups move with a few changed lines. This page starts with what you give up,
because that decides whether moving is worth it. It is written for gcs-mimic
0.14.0 and fake-gcs-server 1.56.1.

gcs-mimic is not a drop-in replacement for every setup.

## What stops working

Roughly in order of how many setups it affects.

| You do this today | After the move | Do this instead |
| --- | --- | --- |
| Keep objects in a `/storage` volume | The format differs, so the volume cannot be reused and `-filesystem-root` is ignored. gcs-mimic starts empty. | [Copy the object bytes](#bring-existing-objects) or reseed from your fixtures. |
| Rely on the generations, custom metadata or content types of existing objects | They cannot be carried over today. `gcs-mimic import fake-gcs-server` exists, but in 0.14.0 it stops on every object uploaded through fake-gcs-server's API, because those objects carry a generated `projectOwner-test-project` ACL it does not accept. | Re-create the objects your tests depend on through the API, or keep that data on fake-gcs-server. |
| Browse or edit objects as files in the storage directory | Objects are stored as opaque files. `/data` is read-only input, and API writes never appear there. | Add a [sync directory](container-usage.md#mirror-objects-to-a-host-directory). It mirrors objects as files; its conflict and recovery cases are still partial. |
| Use `docker exec … sh`, shell healthchecks or shell entrypoints | The image has no shell and no package manager. | Remove your `healthcheck:` override, since the image has its own (`gcs-mimic healthcheck`). Run scripts from another container. |
| Mount directories only root can read or write | The server runs as UID/GID 10001, not root. | Use a named volume for `/var/lib/gcs-mimic`, and keep files under `/data` readable by every user. |
| Embed `fakestorage` in a Go test process | There is no in-process server. The [Go package](../../go/README.md) starts the Rust binary as a subprocess and covers a subset of the API; `Backend()` is not supported. | Use that package where its subset is enough. |
| Use a Testcontainers module or wrapper that hard-codes `fake-gcs-server` | Changing the image is not enough, because the wrapper expects the old executable and readiness log. | Use a [generic container adapter](slices/sdk-client-adapters.md). |
| Run `-backend memory` with a lot of data | RAM storage has a budget of 64 MiB of object bytes by default. Past it, writes fail with 503. | Raise `memory_max_bytes` ([RAM contract](slices/memory-backend.md)) or use the disk backend. |
| Put `.metadata` sidecar files in a seed directory | They are seeded as ordinary objects, not read as metadata. | Set metadata through the API. |
| Call `/_internal/reseed` or `PUT /_internal/config` | These routes are off by default. | Set `GCS_MIMIC_ADMIN_ENABLED=true`, on an isolated listener. |
| List buckets without `?project=` | Returns 400, as Cloud Storage does. SDKs send the project already. | Add `?project=local-project`, or set `GCS_MIMIC_COMPAT_PROFILE=legacy-emulator`. |
| Use `-event.*` without a Pub/Sub emulator | Startup fails, so tests that wait for notifications do not hang. | Point `PUBSUB_EMULATOR_HOST` at a local emulator. |
| Depend on `-public-host` matching the Host header | The flag is accepted, but the matching is off. | Set `GCS_MIMIC_COMPAT_PROFILE=legacy-emulator`. |
| Send requests Cloud Storage would reject | gcs-mimic validates input as Cloud Storage does, so loose requests can now return 4xx. | Fix the request. |
| Assert on the status of a cancelled resumable upload | Returns 499, which Google documents as a successful cancel. | Accept 499. |

## When to stay on fake-gcs-server

Moving is probably not worth it yet if one of these is true:

- Your Go tests embed `fakestorage` and depend on `Backend()` or on running
  in-process.
- You need the generations or custom metadata of objects already in a
  `/storage` volume.
- Your workflow depends on editing object files in place.

## Swap the container

```diff
 gcs:
-  image: fsouza/fake-gcs-server:1.56.1
+  image: ghcr.io/shiroemons/gcs-mimic:0.14.0
   command: ["-scheme", "http", "-external-url", "http://gcs:4443"]
   volumes:
     - ./data:/data
-    - gcs-storage:/storage
+    - gcs-data:/var/lib/gcs-mimic
```

Most `command:` flags, `FAKE_GCS_*` variables and the `./data:/data` mount keep
working. At startup each old setting logs a warning with its `GCS_MIMIC_*` name,
so you can rename them whenever it suits you. Application code that talks to
the emulator over HTTP does not change, including `STORAGE_EMULATOR_HOST`.

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

## Move to native settings when you are ready

| fake-gcs-server | gcs-mimic |
| --- | --- |
| `-scheme http` | `GCS_MIMIC_SCHEME: http` |
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
- [Every flag and `FAKE_GCS_*` variable](fake-gcs-server-configuration.md)
- [How the storage formats differ](fake-gcs-server-storage.md)
