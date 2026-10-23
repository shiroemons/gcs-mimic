# fake-gcs-server configuration reference

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

[日本語](../ja/fake-gcs-server-configuration.md)

Checked on **2026-10-24** against fake-gcs-server **v1.56.1**, commit
`896ece51e49eed0946089f3089f6343855c0d106`. The executable reads the following
**19** environment variables. They are explicit settings, not a rule that turns
any flag into an environment variable. The Go library's constructors do not
automatically use this executable configuration layer.

## Source settings

Defaults below are the executable defaults. The official image additionally
supplies `-data /data` through its entrypoint.

| Environment variable | CLI option | Default |
| --- | --- | --- |
| `FAKE_GCS_PORT` | `-port` | `4443` |
| `FAKE_GCS_PORT_HTTP` | `-port-http` | `8000`, second listener with `-scheme both` |
| `FAKE_GCS_BACKEND` | `-backend` | `filesystem` |
| `FAKE_GCS_FILESYSTEM_ROOT` | `-filesystem-root` | `/storage` |
| `FAKE_GCS_PUBLIC_HOST` | `-public-host` | `storage.googleapis.com` |
| `FAKE_GCS_EXTERNAL_URL` | `-external-url` | Computed from scheme, host and port |
| `FAKE_GCS_SCHEME` | `-scheme` | `https` |
| `FAKE_GCS_HOST` | `-host` | `0.0.0.0` |
| `FAKE_GCS_DATA` | `-data` | Empty; image supplies `/data` by CLI |
| `FAKE_GCS_CORS_HEADERS` | `-cors-headers` | Empty additional header list |
| `FAKE_GCS_EVENT_PUBSUB_PROJECT_ID` | `-event.pubsub-project-id` | Empty |
| `FAKE_GCS_EVENT_PUBSUB_TOPIC` | `-event.pubsub-topic` | Empty |
| `FAKE_GCS_EVENT_BUCKET` | `-event.bucket` | Empty: no bucket filter |
| `FAKE_GCS_EVENT_OBJECT_PREFIX` | `-event.object-prefix` | Empty: no prefix filter |
| `FAKE_GCS_EVENT_LIST` | `-event.list` | `finalize` |
| `FAKE_GCS_LOCATION` | `-location` | `US-CENTRAL1` |
| `FAKE_GCS_CERT_LOCATION` | `-cert-location` | Empty: built-in certificate |
| `FAKE_GCS_PRIVATE_KEY_LOCATION` | `-private-key-location` | Empty: built-in key |
| `FAKE_GCS_LOG_LEVEL` | `-log-level` | `info` |

`-scheme http` uses the normal `-port` value, **4443** by default. Port 8000
does not become the HTTP-only default. `-scheme both` adds the second HTTP
listener; its computed external URL still selects HTTPS.

`-public-host` supplies the public host authority and participates in Host
matching on relevant object routes. It neither changes the listening address
nor grants object access. `-external-url` controls the advertised endpoint,
including resumable upload URLs. Its trailing slashes are removed when server
options are constructed.

`-cors-headers` adds globally allowed **request** headers. It is not a
per-bucket CORS policy or a list of exposed response headers. `-location` is a
server-wide bucket location default. Event settings configure one publisher
and optional bucket/object-prefix filters, not independent bucket policies.

## Precedence and validation

An unset or empty source environment variable uses its default. Valid CLI
values override environment values. In the official image, the entrypoint's
`-data /data` therefore overrides `FAKE_GCS_DATA`; append an explicit
`-data /another-directory` to change it. Repeated CLI flags use the last value.

Both port environment variables are parsed before CLI arguments. A nonnumeric
value fails startup even when a valid CLI override exists, or when the second
HTTP listener is unused. A parseable out-of-range value can be replaced by a
valid CLI value before final validation. Unknown `FAKE_GCS_*` names are ignored.

The implemented log levels are `debug`, `info`, `warning`, `warn` and `error`.
Despite the help text, `trace`, `fatal` and `panic` are rejected. Enabled event
notifications require both project and topic. Their supported event list is
`finalize`, `delete`, `metadataUpdate` and `archive`; `archive` is omitted from
the help text. Event-list validation occurs only when notifications are enabled.

## Current gcs-mimic equivalents

Use native names by role. **The current binary ignores `FAKE_GCS_*` variables**;
they are not aliases. The planned migration diagnostic is not implemented.
Unknown `GCS_MIMIC_*` names fail validation, and an empty native value does not
generally mean “use the default.” See the
[native configuration reference](getting-started.md#native-binary-and-configuration).

| Source role | Current native setting | Remaining difference |
| --- | --- | --- |
| Scheme | `GCS_MIMIC_SCHEME` | `http` or `https`; no dual listener |
| Bind host and port | `GCS_MIMIC_LISTEN` | One `IP:port` value |
| Advertised API endpoint | `GCS_MIMIC_API_BASE_URL` | API origin, separate from public URL construction |
| Seed input | `GCS_MIMIC_SEED_DIR`, `GCS_MIMIC_SEED_PROJECT` | One-time import; MIME and metadata differ |
| Persistent destination | `GCS_MIMIC_DATA_DIR` | SQLite plus opaque files; cannot mount legacy `/storage` directly |
| Supplied certificate/key | `GCS_MIMIC_TLS_CERT`, `GCS_MIMIC_TLS_KEY` | Supply a matching pair |
| Public-host behavior | URL helper plus `GCS_MIMIC_XML_VIRTUAL_HOST_DOMAINS` | No single equivalent setting or identical Host filtering |
| Bucket CORS | JSON bucket API | Per-bucket XML rules; separate fixed JSON defaults |
| Bucket location | JSON bucket API | Native `US`, `EU`, `ASIA`; no global location setting |
| Memory backend, event publisher, log-level selection | No current equivalent | Missing capabilities remain on the roadmap |

The legacy profile's fixed `US-CENTRAL1` response does not implement configurable
regional placement. A public URL helper does not establish `-public-host` parity.
See [option examples](fake-gcs-server-migration.md#startup-options-and-current-equivalents).

`GCS_MIMIC_PORT` in the repository's source-build Compose file is a **Compose
host-port variable**, not a server environment setting. Passing it to the binary
does not configure the listener. Do not mechanically rename every `FAKE_GCS_`
suffix to `GCS_MIMIC_`.

## Evidence and sources

The pinned configuration package's upstream tests and temporary focused probes
passed with Go **1.27.3**, using `go test -mod=readonly -count=1 -v ./internal/config`.
The probes checked all 19 values and 14 precedence/validation cases, including
the image entrypoint's argument sequence. See the
[configuration evidence](evidence/fake-gcs-server-configuration.md).
This was not a full upstream test run, real Pub/Sub delivery test or certificate
loading test; certificate paths were checked as configuration values.

Primary sources:

- [Configuration parser and defaults](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/internal/config/config.go)
- [Official image entrypoint](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/Dockerfile)
- [Server routing and CORS](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/server.go)

Implementation status remains in the [feature ledger](../../compatibility/features.json).
