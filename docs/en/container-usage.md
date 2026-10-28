# Use the Docker image

The image `ghcr.io/shiroemons/gcs-mimic:0.2.1` is published for `linux/amd64`
and `linux/arm64`; pulling it needs no registry login. Pin this exact version in
CI. To test unreleased source instead, build a local image from a checkout and
use its tag in the commands below:

```sh
docker build -t gcs-mimic:local .
```

The [Compose migration walkthrough](fake-gcs-server-migration.md#replace-a-fake-gcs-server-compose-service)
replaces a fake-gcs-server service with the published image and initializes
empty buckets. It covers UX-012, DESIGN-061 and UX-001.

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
  ghcr.io/shiroemons/gcs-mimic:0.2.1
docker exec gcs-mimic gcs-mimic healthcheck
```

Once the readiness command succeeds, run the README's bucket and object commands.
The image healthcheck follows `GCS_MIMIC_SCHEME`, `GCS_MIMIC_LISTEN` and the
JSON file selected by `GCS_MIMIC_CONFIG`; HTTP needs no probe override.
Wildcard listeners connect through loopback; `both` probes its HTTP listener.
HTTPS trusts the embedded development CA. Custom CAs require `--ca-cert PATH`.
Server CLI overrides are private to that process: use the same environment/file
settings for probes, or specify `healthcheck --url URL` explicitly.
The image supplies `GCS_MIMIC_LISTEN=0.0.0.0:4443` and
`GCS_MIMIC_DATA_DIR=/var/lib/gcs-mimic`; override these environment values to
change the bind port or persistent location. Environment values take precedence
over a JSON file, as they do in native startup.
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

`localhost` inside a container refers to that container. If your application
follows API-generated URLs, set `GCS_MIMIC_API_BASE_URL` to the address reachable
by that application, such as `http://gcs:4443`. A single server advertises one API
base. Public and signed URLs must use the address reachable by their recipient.
The migration Compose example explicitly advertises `http://gcs:4443` so
container SDKs can follow resumable Location. A host/browser needs its own
published origin, for example `http://localhost:14443`; the walkthrough explains
unsigned session translation, cancellation 499 and separate worktree ports.

Without an explicit API base, native startup derives it from the actual bound
listener, replacing wildcard IPs with loopback and port zero with the allocated
port. The ready event includes this effective `api_base_url`. The checked-in
Compose files instead advertise the published `GCS_MIMIC_PORT` (default 4443)
using their HTTP/HTTPS mode; an explicit `GCS_MIMIC_API_BASE_URL` wins.
Docker port-zero mappings require discovering and configuring the assigned host
port before following links. Neither a container listener nor request headers
can discover a host port mapping or a proxy's external address.

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
Bucket directories added after initialization do not create buckets on restart.
For empty-directory bucket setup, replace the seed mount with a
[buckets file](#initialize-bucket-settings); it creates missing buckets every start.

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

Entry `project` overrides file `project`, then `GCS_MIMIC_SEED_PROJECT`
(default `local-project`). Initializer v1 rejects IAM policies, ACLs, lifecycle,
UBLA and unknown fields, including disabled or empty unsupported settings.
Initialization is local administration in both auth modes and grants no API
permissions. CORS applies to XML; JSON uses its fixed defaults.
See the [schema, limits and ready counts](slices/bucket-initialization.md).

## Further setup

- [HTTPS and custom certificates](getting-started.md#local-https-with-an-explicit-development-ca)
- [SDK, authentication and browser examples](getting-started.md)
- [Source builds and verification](development.md)
- [Image publication and supported platforms](container-distribution.md)
