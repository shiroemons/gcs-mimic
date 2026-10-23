# Use the Docker image

The first public image is pending. The planned reference is
`ghcr.io/shiroemons/gcs-mimic:0.1.0`; the commands below require its publication
or an explicitly substituted local image. No checkout or build is required
once an image is available. Use an exact released version in CI.

The [README Compose example](../../README.md#quick-start) is the shortest start.
It selects HTTP and persists data in a named volume. HTTPS remains the server
default when no scheme is selected.

## Docker run

```sh
docker run -d --name gcs-mimic \
  -p 127.0.0.1:4443:4443 \
  -v gcs-mimic-data:/var/lib/gcs-mimic \
  -e GCS_MIMIC_SCHEME=http \
  -e GCS_MIMIC_API_BASE_URL=http://localhost:4443 \
  --no-healthcheck \
  ghcr.io/shiroemons/gcs-mimic:0.1.0
docker exec gcs-mimic gcs-mimic healthcheck \
  --url http://127.0.0.1:4443/_internal/ready
```

Once the readiness command succeeds, run the README's bucket and object commands.
This HTTP example disables the image's HTTPS probe and checks readiness directly;
the Compose example provides automatic health checks.
Stop with `docker stop gcs-mimic` and resume with `docker start gcs-mimic`.
The named volume keeps data when a stopped container is removed.

## Connect an application

| Caller | API endpoint |
| --- | --- |
| Host application or browser | `http://localhost:4443` |
| Application on the same Compose network | `http://gcs:4443` |

`localhost` inside a container refers to that container. If your application
follows API-generated URLs, set `GCS_MIMIC_API_BASE_URL` to the address reachable
by that application, such as `http://gcs:4443`. A single server advertises one API
base. Public and signed URLs must use the address reachable by their recipient.

## Load local files

Keep the bucket/object layout used by fake-gcs-server's `/data` mount:

```text
data/
  assets/
    logo.png
  uploads/
    example.txt
```

Before the first start, add these entries to the README's service, retaining its
other environment variables and persistent volume:

```yaml
    environment:
      GCS_MIMIC_SEED_DIR: /data
    volumes:
      - ./data:/data:ro
```

Merge the entries into the existing `environment` and `volumes` sections;
do not replace those sections. Files load before readiness only when the
persistent catalog is first initialized. Existing data is not re-seeded on
restart. The default owning project for seeded buckets is `local-project`;
use `GCS_MIMIC_SEED_PROJECT` to select another project.

If you already ran the quick start, stop it with `docker compose down`, then use
`docker compose -p gcs-seeded up -d --wait` for a separate, fresh data volume.
The original project's data is retained.

Native import currently uses `application/octet-stream` for file content.
It does not import fake-gcs-server's runtime xattrs/sidecars or preserve their
metadata. External edits after initialization are not synchronized, and API
writes do not appear under `/data`. See the [storage comparison](fake-gcs-server-storage.md)
before reusing a persistent source directory.

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

A file that initializes missing buckets and their settings, alongside later
API updates, is a planned capability. Existing settings will be preserved on
restart; removing an entry will not delete a bucket. The initializer and its
file syntax are not implemented. The current server JSON configuration and seed
directory do not provide this feature. See the
[configuration requirements](fake-gcs-server-migration.md#configuration-compatibility-and-extensions).

## Further setup

- [HTTPS and custom certificates](getting-started.md#local-https-with-an-explicit-development-ca)
- [SDK, authentication and browser examples](getting-started.md)
- [Source builds and verification](development.md)
- [Image publication and supported platforms](container-distribution.md)
